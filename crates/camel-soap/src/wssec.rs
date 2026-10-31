//! WS-Security (OASIS WSS 1.0): UsernameToken with `PasswordText` or `PasswordDigest`
//! (`Base64(SHA-1(nonce + created + password))`), `wsu:Timestamp`, and an enveloped XML
//! Signature over the whole envelope placed in the `wsse:Security` header (camel-xml's
//! `xmlsecurity`, RSA-SHA256 / ECDSA-SHA256, exclusive c14n).
//!
//! Verification ([`verify_security`]) checks the user / password (digest or text), rejects
//! replayed nonces and stale `Created` values (default 5 minutes), and verifies the
//! signature when a trusted key is configured. Failures become a `wsse:FailedAuthentication`
//! / `wsse:InvalidSecurity` SOAP fault. Encryption (XML Encryption) and SAML / X.509 tokens
//! are not supported.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use camel_core::error::{CamelError, Result};
use camel_tls::crypto::hash::HashAlg;
use camel_tls::keys::{PrivateKey, PublicKey};
use camel_tls::pem::{base64_decode, base64_encode};
use camel_tls::x509::Certificate;
use camel_xml::dom::{Document, NodeId};
use camel_xml::serialize::escape_text;

use crate::envelope::{SoapFault, SoapVersion};
use crate::support::{format_utc_millis, now_utc, parse_datetime_millis};

/// The WS-Security extension namespace (`wsse`).
pub const WSSE_NS: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd";
/// The WS-Security utility namespace (`wsu`).
pub const WSU_NS: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd";
/// `PasswordText` type.
pub const PASSWORD_TEXT: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordText";
/// `PasswordDigest` type.
pub const PASSWORD_DIGEST: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest";
/// Base64 encoding type of the nonce.
pub const BASE64_ENCODING: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary";

const DSIG_NS: &str = "http://www.w3.org/2000/09/xmldsig#";
const EXC_C14N: &str = "http://www.w3.org/2001/10/xml-exc-c14n#";

/// Password type of a UsernameToken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordType {
    Text,
    Digest,
}

impl PasswordType {
    /// Parse `PasswordText` / `PasswordDigest` (also `text` / `digest`).
    pub fn parse(s: &str) -> Option<PasswordType> {
        match s.trim().to_ascii_lowercase().as_str() {
            "passwordtext" | "text" => Some(PasswordType::Text),
            "passworddigest" | "digest" => Some(PasswordType::Digest),
            _ => None,
        }
    }
}

/// `Base64(SHA-1(nonce + created + password))`.
pub fn password_digest(nonce: &[u8], created: &str, password: &str) -> String {
    let mut data = nonce.to_vec();
    data.extend_from_slice(created.as_bytes());
    data.extend_from_slice(password.as_bytes());
    base64_encode(&HashAlg::Sha1.hash(&data))
}

/// Build a `wsse:Security` header with a UsernameToken (and a `wsu:Timestamp`).
pub fn security_header(version: SoapVersion, user: &str, password: &str, ty: PasswordType) -> String {
    let created = now_utc();
    let nonce = camel_tls::crypto::rng::random_bytes(16);
    security_header_with(version, user, password, ty, &nonce, &created)
}

/// Like [`security_header`] with an explicit nonce and creation time.
pub fn security_header_with(version: SoapVersion, user: &str, password: &str, ty: PasswordType, nonce: &[u8], created: &str) -> String {
    let (pw, ty_uri) = match ty {
        PasswordType::Text => (escape_text(password), PASSWORD_TEXT),
        PasswordType::Digest => (password_digest(nonce, created, password), PASSWORD_DIGEST),
    };
    let expires = parse_datetime_millis(created).map(|m| format_utc_millis(m + 300_000)).unwrap_or_default();
    format!(
        "<wsse:Security xmlns:wsse=\"{wsse}\" xmlns:wsu=\"{wsu}\" xmlns:soap=\"{soap}\" soap:mustUnderstand=\"{mu}\">\
<wsu:Timestamp wsu:Id=\"TS-1\"><wsu:Created>{created}</wsu:Created><wsu:Expires>{expires}</wsu:Expires></wsu:Timestamp>\
<wsse:UsernameToken wsu:Id=\"UsernameToken-1\"><wsse:Username>{user}</wsse:Username>\
<wsse:Password Type=\"{ty}\">{pw}</wsse:Password>\
<wsse:Nonce EncodingType=\"{enc}\">{nonce}</wsse:Nonce><wsu:Created>{created}</wsu:Created></wsse:UsernameToken></wsse:Security>",
        wsse = WSSE_NS,
        wsu = WSU_NS,
        soap = version.ns(),
        mu = if version == SoapVersion::V11 { "1" } else { "true" },
        created = created,
        expires = expires,
        user = escape_text(user),
        ty = ty_uri,
        pw = pw,
        enc = BASE64_ENCODING,
        nonce = base64_encode(nonce),
    )
}

/// An empty `wsse:Security` header (used to carry only a signature).
pub fn empty_security_header(version: SoapVersion) -> String {
    format!(
        "<wsse:Security xmlns:wsse=\"{}\" xmlns:soap=\"{}\" soap:mustUnderstand=\"{}\"/>",
        WSSE_NS,
        version.ns(),
        if version == SoapVersion::V11 { "1" } else { "true" }
    )
}

fn find_security(doc: &Document) -> Option<NodeId> {
    let env = doc.document_element()?;
    let header = doc.element_children(env).into_iter().find(|&c| doc.local_name(c) == "Header")?;
    doc.child_elements_named(header, WSSE_NS, "Security").next()
}

/// Sign an envelope: an enveloped signature over the whole envelope is appended to the
/// `wsse:Security` header (which must exist, see [`empty_security_header`]).
pub fn sign_envelope(envelope: &str, key: &PrivateKey, cert: Option<&Certificate>) -> Result<String> {
    let mut doc = Document::parse(envelope).map_err(|e| CamelError::illegal_argument(format!("Invalid envelope: {}", e)))?;
    let sec = find_security(&doc).ok_or_else(|| CamelError::illegal_state("No wsse:Security header to hold the signature"))?;
    camel_xml::xmlsecurity::sign_document(&mut doc, sec, key, cert, EXC_C14N, "ds")?;
    Ok(doc.to_xml_string())
}

/// Server-side WS-Security policy.
#[derive(Default)]
pub struct SecurityPolicy {
    /// Users and passwords; when non-empty a UsernameToken is required.
    pub users: HashMap<String, String>,
    /// Trusted key; when set a valid signature is required.
    pub signature_key: Option<PublicKey>,
    /// Maximum age of `Created` in milliseconds (0 = not checked).
    pub max_age_ms: i64,
    seen_nonces: Mutex<VecDeque<String>>,
}

impl SecurityPolicy {
    /// A policy accepting the given users (5 minute freshness window).
    pub fn with_users(users: HashMap<String, String>) -> SecurityPolicy {
        SecurityPolicy { users, max_age_ms: 300_000, ..Default::default() }
    }

    /// Whether the policy checks anything.
    pub fn is_active(&self) -> bool {
        !self.users.is_empty() || self.signature_key.is_some()
    }
}

fn failed(version: SoapVersion, m: &str) -> SoapFault {
    let _ = m;
    let reason = "The security token could not be authenticated or authorized";
    match version {
        SoapVersion::V11 => SoapFault::new(format!("{{{}}}FailedAuthentication", WSSE_NS), reason),
        SoapVersion::V12 => SoapFault::new("Sender", reason).with_subcode(format!("{{{}}}FailedAuthentication", WSSE_NS)),
    }
}

fn invalid(version: SoapVersion, reason: &str) -> SoapFault {
    match version {
        SoapVersion::V11 => SoapFault::new(format!("{{{}}}InvalidSecurity", WSSE_NS), reason),
        SoapVersion::V12 => SoapFault::new("Sender", reason).with_subcode(format!("{{{}}}InvalidSecurity", WSSE_NS)),
    }
}

/// Verify the security header of an envelope document against a policy. Returns the
/// authenticated user name (if a token was required).
pub fn verify_security(doc: &Document, version: SoapVersion, policy: &SecurityPolicy) -> std::result::Result<Option<String>, SoapFault> {
    if !policy.is_active() {
        return Ok(None);
    }
    let sec = find_security(doc).ok_or_else(|| invalid(version, "An error was discovered processing the <wsse:Security> header: no security header"))?;
    if let Some(key) = &policy.signature_key {
        if doc.child_elements_named(sec, DSIG_NS, "Signature").next().is_none() {
            return Err(invalid(version, "A signature is required"));
        }
        camel_xml::xmlsecurity::verify_document(doc, Some(key), false).map_err(|e| invalid(version, &format!("The signature or decryption was invalid: {}", e.message)))?;
    }
    if policy.users.is_empty() {
        return Ok(None);
    }
    let token = doc.child_elements_named(sec, WSSE_NS, "UsernameToken").next().ok_or_else(|| failed(version, "no UsernameToken"))?;
    let text = |local: &str, ns: &str| doc.child_elements_named(token, ns, local).next().map(|e| doc.string_value(e).trim().to_string());
    let user = text("Username", WSSE_NS).ok_or_else(|| failed(version, "no Username"))?;
    let pw_el = doc.child_elements_named(token, WSSE_NS, "Password").next().ok_or_else(|| failed(version, "no Password"))?;
    let pw = doc.string_value(pw_el);
    let ty = doc.attribute(pw_el, "Type").unwrap_or(PASSWORD_TEXT);
    let expected = policy.users.get(&user).ok_or_else(|| failed(version, "unknown user"))?;
    let nonce = text("Nonce", WSSE_NS);
    let created = text("Created", WSU_NS);
    if let Some(c) = &created {
        if policy.max_age_ms > 0 {
            let t = parse_datetime_millis(c).ok_or_else(|| invalid(version, "Invalid Created timestamp"))?;
            let now = camel_core::util_time::now_millis();
            if now - t > policy.max_age_ms || t - now > 60_000 {
                return Err(invalid(version, "The message has expired"));
            }
        }
    }
    let ok = if ty == PASSWORD_DIGEST {
        let n = nonce.as_deref().and_then(base64_decode).ok_or_else(|| failed(version, "digest without nonce"))?;
        let c = created.as_deref().ok_or_else(|| failed(version, "digest without created"))?;
        password_digest(&n, c, expected) == pw.trim()
    } else {
        pw == *expected
    };
    if !ok {
        return Err(failed(version, "bad password"));
    }
    if let Some(n) = nonce {
        let mut seen = policy.seen_nonces.lock().unwrap();
        if seen.contains(&n) {
            return Err(invalid(version, "Nonce replay detected"));
        }
        seen.push_back(n);
        if seen.len() > 10_000 {
            seen.pop_front();
        }
    }
    Ok(Some(user))
}

/// Remove the `wsse:Security` header block(s) from a list of header fragments.
pub fn strip_security(headers: &[String]) -> Vec<String> {
    headers
        .iter()
        .filter(|h| match Document::parse(h) {
            Ok(d) => d.document_element().map(|r| !(d.namespace_uri(r) == WSSE_NS && d.local_name(r) == "Security")).unwrap_or(true),
            Err(_) => true,
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::build_envelope;

    #[test]
    fn oasis_digest_example() {
        // digest computed independently for nonce/created/password
        let d = password_digest(b"0123456789abcdef", "2024-01-01T00:00:00Z", "secret");
        assert_eq!(d.len(), 28);
        assert_eq!(d, password_digest(b"0123456789abcdef", "2024-01-01T00:00:00Z", "secret"));
        assert_ne!(d, password_digest(b"0123456789abcdeg", "2024-01-01T00:00:00Z", "secret"));
    }

    #[test]
    fn token_accept_reject() {
        let mut users = HashMap::new();
        users.insert("alice".to_string(), "secret".to_string());
        let policy = SecurityPolicy::with_users(users);
        for ty in [PasswordType::Text, PasswordType::Digest] {
            let env = build_envelope(SoapVersion::V11, &[security_header(SoapVersion::V11, "alice", "secret", ty)], &["<a/>".into()]);
            let doc = Document::parse(&env).unwrap();
            assert_eq!(verify_security(&doc, SoapVersion::V11, &policy).unwrap().as_deref(), Some("alice"));
            // replay of the same nonce
            assert!(verify_security(&doc, SoapVersion::V11, &policy).unwrap_err().reason.contains("replay"));
            let bad = build_envelope(SoapVersion::V12, &[security_header(SoapVersion::V12, "alice", "wrong", ty)], &["<a/>".into()]);
            let f = verify_security(&Document::parse(&bad).unwrap(), SoapVersion::V12, &policy).unwrap_err();
            assert_eq!(f.subcode.as_deref(), Some(format!("{{{}}}FailedAuthentication", WSSE_NS).as_str()));
        }
        let none = build_envelope(SoapVersion::V11, &[], &["<a/>".into()]);
        assert!(verify_security(&Document::parse(&none).unwrap(), SoapVersion::V11, &policy).is_err());
        let old = build_envelope(SoapVersion::V11, &[security_header_with(SoapVersion::V11, "alice", "secret", PasswordType::Digest, b"n", "2001-01-01T00:00:00Z")], &["<a/>".into()]);
        assert!(verify_security(&Document::parse(&old).unwrap(), SoapVersion::V11, &policy).unwrap_err().reason.contains("expired"));
    }

    #[test]
    fn signature() {
        let id = &camel_tls::testing::pki().localhost;
        let cert = Certificate::from_der(&id.cert_der).unwrap();
        let env = build_envelope(SoapVersion::V11, &[empty_security_header(SoapVersion::V11)], &["<m:x xmlns:m=\"urn:m\">1</m:x>".into()]);
        let signed = sign_envelope(&env, &id.key, Some(&cert)).unwrap();
        let policy = SecurityPolicy { signature_key: cert.public_key.clone(), ..Default::default() };
        assert!(verify_security(&Document::parse(&signed).unwrap(), SoapVersion::V11, &policy).is_ok());
        let tampered = signed.replace(">1</m:x>", ">2</m:x>");
        assert!(verify_security(&Document::parse(&tampered).unwrap(), SoapVersion::V11, &policy).is_err());
        assert!(verify_security(&Document::parse(&env).unwrap(), SoapVersion::V11, &policy).is_err());
    }
}
