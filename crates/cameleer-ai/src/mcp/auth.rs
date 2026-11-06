//! Who may use the MCP server over HTTP.
//!
//! | `camel.server.mcp-auth` | how a request authenticates |
//! |---|---|
//! | `none` (default) | it doesn't; the principal is `anonymous` |
//! | `api-key` | `Authorization: Bearer <key>` or `X-API-Key: <key>`, one of `camel.server.mcp-api-keys` (`name=key,...`) |
//! | `jwt` | a bearer JWT verified against `camel.server.mcp-jwt-jwks-url` (RS256/384/512, PS256/384/512, ES256/384) or `camel.server.mcp-jwt-secret` (HS256/384/512) |
//! | `introspection` | a bearer token checked with RFC 7662 at `camel.server.mcp-introspection-url` |
//!
//! With `jwt` and `introspection` the token's audience must name this server
//! (`camel.server.mcp-resource-url`, RFC 8707), and the server publishes RFC 9728
//! protected-resource metadata at `/.well-known/oauth-protected-resource`; a request without a
//! valid token gets `401` with `WWW-Authenticate: Bearer resource_metadata="..."`. A client's
//! token is never passed to the routes or downstream; routes see only the principal.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::{ObjectMap, Value};
use cameleer_tls::crypto::hash::HashAlg;

use super::{base64url_decode, csv, prop, McpConfig, Principal};
use crate::tool::sha256_hex;

/// Authentication settings.
#[derive(Clone, Debug, Default)]
pub struct AuthConfig {
    /// `none`, `api-key`, `jwt` or `introspection`.
    pub mode: String,
    /// name -> SHA-256 of the key
    pub api_keys: Vec<(String, String)>,
    pub jwks_url: Option<String>,
    pub jwt_secret: Option<Vec<u8>>,
    pub issuer: Option<String>,
    /// Accepted audiences (`camel.server.mcp-jwt-audience`, default the resource URL).
    pub audiences: Vec<String>,
    pub introspection_url: Option<String>,
    pub introspection_client_id: Option<String>,
    pub introspection_client_secret: Option<String>,
    /// The MCP endpoint's public URL (RFC 8707 resource indicator).
    pub resource_url: Option<String>,
    pub authorization_servers: Vec<String>,
    pub required_scopes: Vec<String>,
    /// Claim used as the principal name (default `sub`).
    pub principal_claim: String,
}

impl AuthConfig {
    pub fn from_properties(props: &BTreeMap<String, String>, _c: &McpConfig) -> Result<AuthConfig> {
        let get = |k: &str| prop(props, k);
        let mode = get("camel.server.mcp-auth").map(|m| m.trim().to_ascii_lowercase()).unwrap_or_else(|| "none".into());
        let mut a = AuthConfig { mode: mode.clone(), principal_claim: get("camel.server.mcp-principal-claim").unwrap_or_else(|| "sub".into()), ..Default::default() };
        a.resource_url = get("camel.server.mcp-resource-url");
        a.authorization_servers = get("camel.server.mcp-authorization-servers").map(|s| csv(&s)).unwrap_or_default();
        a.required_scopes = get("camel.server.mcp-required-scopes").map(|s| s.split([' ', ',']).filter(|x| !x.is_empty()).map(str::to_string).collect()).unwrap_or_default();
        a.issuer = get("camel.server.mcp-jwt-issuer");
        a.audiences = get("camel.server.mcp-jwt-audience").map(|s| csv(&s)).unwrap_or_default();
        if a.audiences.is_empty() {
            if let Some(r) = &a.resource_url {
                a.audiences.push(r.clone());
            }
        }
        match mode.as_str() {
            "none" => {}
            "api-key" => {
                let raw = get("camel.server.mcp-api-keys").ok_or_else(|| CamelError::illegal_argument("camel.server.mcp-auth=api-key needs camel.server.mcp-api-keys (name=key,...)"))?;
                for entry in csv(&raw) {
                    let (name, key) = entry.split_once('=').ok_or_else(|| CamelError::illegal_argument("camel.server.mcp-api-keys entries must be name=key"))?;
                    if key.trim().len() < 16 {
                        return Err(CamelError::illegal_argument(format!("The API key of {} is shorter than 16 characters", name.trim())));
                    }
                    a.api_keys.push((name.trim().to_string(), sha256_hex(key.trim().as_bytes())));
                }
            }
            "jwt" => {
                a.jwks_url = get("camel.server.mcp-jwt-jwks-url");
                a.jwt_secret = get("camel.server.mcp-jwt-secret").map(|s| s.into_bytes());
                if a.jwks_url.is_none() && a.jwt_secret.is_none() {
                    return Err(CamelError::illegal_argument("camel.server.mcp-auth=jwt needs camel.server.mcp-jwt-jwks-url or camel.server.mcp-jwt-secret"));
                }
                if a.audiences.is_empty() {
                    return Err(CamelError::illegal_argument("camel.server.mcp-auth=jwt needs camel.server.mcp-resource-url (or camel.server.mcp-jwt-audience): tokens must be issued for this server"));
                }
            }
            "introspection" => {
                a.introspection_url = get("camel.server.mcp-introspection-url");
                a.introspection_client_id = get("camel.server.mcp-introspection-client-id");
                a.introspection_client_secret = get("camel.server.mcp-introspection-client-secret");
                if a.introspection_url.is_none() {
                    return Err(CamelError::illegal_argument("camel.server.mcp-auth=introspection needs camel.server.mcp-introspection-url"));
                }
                if a.audiences.is_empty() {
                    return Err(CamelError::illegal_argument("camel.server.mcp-auth=introspection needs camel.server.mcp-resource-url (or camel.server.mcp-jwt-audience)"));
                }
            }
            other => return Err(CamelError::illegal_argument(format!("camel.server.mcp-auth must be none, api-key, jwt or introspection, got {}", other))),
        }
        Ok(a)
    }

    pub fn is_enabled(&self) -> bool {
        self.mode != "none" && !self.mode.is_empty()
    }

    /// Whether the server publishes protected-resource metadata (OAuth modes).
    pub fn is_oauth(&self) -> bool {
        self.mode == "jwt" || self.mode == "introspection"
    }

    /// RFC 9728 metadata.
    pub fn resource_metadata(&self) -> Value {
        let mut m = ObjectMap::new();
        m.insert("resource".into(), Value::from(self.resource_url.clone().unwrap_or_default()));
        m.insert("authorization_servers".into(), Value::Array(self.authorization_servers.iter().map(|s| Value::from(s.as_str())).collect()));
        if !self.required_scopes.is_empty() {
            m.insert("scopes_supported".into(), Value::Array(self.required_scopes.iter().map(|s| Value::from(s.as_str())).collect()));
        }
        m.insert("bearer_methods_supported".into(), Value::Array(vec![Value::from("header")]));
        Value::Object(m)
    }
}

/// Why a request was refused.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthError {
    /// 401: no credentials or invalid ones.
    Unauthorized { error: Option<&'static str>, description: String },
    /// 403: valid credentials without the required scopes.
    Forbidden { description: String },
}

/// Verifies requests; caches JWKS and introspection results.
pub struct Authenticator {
    config: AuthConfig,
    jwks: Mutex<Option<(Instant, Vec<Jwk>)>>,
    last_jwks_fetch: Mutex<Option<Instant>>,
    introspected: Mutex<HashMap<String, (Instant, std::result::Result<Principal, AuthError>)>>,
}

#[derive(Clone, Debug)]
struct Jwk {
    kid: Option<String>,
    key: JwkKey,
}

#[derive(Clone, Debug)]
enum JwkKey {
    Rsa(cameleer_tls::crypto::rsa::RsaPublicKey),
    Ec(cameleer_tls::crypto::ec::EcCurve, Vec<u8>),
}

impl Authenticator {
    pub fn new(config: AuthConfig) -> Authenticator {
        Authenticator { config, jwks: Mutex::new(None), last_jwks_fetch: Mutex::new(None), introspected: Mutex::default() }
    }

    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// Authenticate a request from its `Authorization` and `X-API-Key` headers.
    pub fn authenticate(&self, authorization: Option<&str>, api_key: Option<&str>) -> std::result::Result<Principal, AuthError> {
        let bearer = authorization.and_then(|a| {
            let (scheme, rest) = a.trim().split_once(' ')?;
            if scheme.eq_ignore_ascii_case("bearer") {
                Some(rest.trim().to_string())
            } else {
                None
            }
        });
        let principal = match self.config.mode.as_str() {
            "none" | "" => return Ok(Principal::anonymous()),
            "api-key" => {
                let presented = api_key.map(|k| k.trim().to_string()).or(bearer).ok_or_else(|| missing("an API key"))?;
                let digest = sha256_hex(presented.as_bytes());
                let mut found = None;
                for (name, d) in &self.config.api_keys {
                    if constant_time_eq(d.as_bytes(), digest.as_bytes()) {
                        found = Some(name.clone());
                    }
                }
                let name = found.ok_or(AuthError::Unauthorized { error: Some("invalid_token"), description: "unknown API key".into() })?;
                Principal { name, scopes: Vec::new(), method: "api-key".into() }
            }
            "jwt" => self.verify_jwt(&bearer.ok_or_else(|| missing("a bearer token"))?)?,
            "introspection" => self.introspect(&bearer.ok_or_else(|| missing("a bearer token"))?)?,
            _ => return Err(AuthError::Unauthorized { error: None, description: "authentication is misconfigured".into() }),
        };
        let missing_scopes: Vec<&String> = self.config.required_scopes.iter().filter(|s| !principal.scopes.contains(s)).collect();
        if !missing_scopes.is_empty() && self.config.mode != "api-key" {
            return Err(AuthError::Forbidden { description: format!("missing scopes: {}", missing_scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")) });
        }
        Ok(principal)
    }

    /// The `WWW-Authenticate` value for a refusal.
    pub fn challenge(&self, err: &AuthError, metadata_url: Option<&str>) -> String {
        let mut parts = vec!["Bearer realm=\"mcp\"".to_string()];
        if let Some(url) = metadata_url.filter(|_| self.config.is_oauth()) {
            parts.push(format!("resource_metadata=\"{}\"", url));
        }
        match err {
            AuthError::Unauthorized { error: Some(e), description } => {
                parts.push(format!("error=\"{}\"", e));
                parts.push(format!("error_description=\"{}\"", description.replace('"', "'")));
            }
            AuthError::Unauthorized { error: None, .. } => {}
            AuthError::Forbidden { description } => {
                parts.push("error=\"insufficient_scope\"".into());
                parts.push(format!("scope=\"{}\"", self.config.required_scopes.join(" ")));
                parts.push(format!("error_description=\"{}\"", description.replace('"', "'")));
            }
        }
        parts.join(", ")
    }

    // --- JWT ---

    fn verify_jwt(&self, token: &str) -> std::result::Result<Principal, AuthError> {
        let invalid = |d: &str| AuthError::Unauthorized { error: Some("invalid_token"), description: d.to_string() };
        let mut parts = token.split('.');
        let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            return Err(invalid("not a JWT"));
        };
        let header = decode_json(h).ok_or_else(|| invalid("bad JWT header"))?;
        let claims = decode_json(p).ok_or_else(|| invalid("bad JWT payload"))?;
        let sig = base64url_decode(s).ok_or_else(|| invalid("bad JWT signature"))?;
        let alg = header.get("alg").and_then(|a| a.as_str()).unwrap_or("");
        let kid = header.get("kid").and_then(|k| k.as_str()).map(str::to_string);
        let signing_input = format!("{}.{}", h, p);
        let ok = match alg {
            "HS256" | "HS384" | "HS512" => {
                let secret = self.config.jwt_secret.as_ref().ok_or_else(|| invalid("HMAC tokens are not accepted"))?;
                let hash = hash_for(alg);
                constant_time_eq(&hash.hmac(secret, signing_input.as_bytes()), &sig)
            }
            "RS256" | "RS384" | "RS512" | "PS256" | "PS384" | "PS512" | "ES256" | "ES384" => {
                let keys = self.keys(kid.as_deref()).map_err(|e| invalid(&e))?;
                let hash = hash_for(alg);
                let digest = hash.hash(signing_input.as_bytes());
                keys.iter().filter(|k| kid.is_none() || k.kid.is_none() || k.kid == kid).any(|k| match (&k.key, &alg[..2]) {
                    (JwkKey::Rsa(pk), "RS") => pk.verify_pkcs1(hash, &digest, &sig),
                    (JwkKey::Rsa(pk), "PS") => pk.verify_pss(hash, &digest, &sig, Some(hash.output_len())),
                    (JwkKey::Ec(curve, point), "ES") => {
                        let half = sig.len() / 2;
                        sig.len() % 2 == 0 && curve.verify(point, &digest, &sig[..half], &sig[half..])
                    }
                    _ => false,
                })
            }
            _ => return Err(invalid("unsupported JWT algorithm")),
        };
        if !ok {
            return Err(invalid("signature does not verify"));
        }
        self.check_claims(&claims)
    }

    fn check_claims(&self, claims: &ObjectMap) -> std::result::Result<Principal, AuthError> {
        let invalid = |d: &str| AuthError::Unauthorized { error: Some("invalid_token"), description: d.to_string() };
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let exp = claims.get("exp").and_then(|e| e.as_i64()).ok_or_else(|| invalid("the token has no exp"))?;
        if exp + 30 < now {
            return Err(invalid("the token expired"));
        }
        if let Some(nbf) = claims.get("nbf").and_then(|e| e.as_i64()) {
            if nbf > now + 30 {
                return Err(invalid("the token is not valid yet"));
            }
        }
        if let Some(iss) = &self.config.issuer {
            if claims.get("iss").and_then(|i| i.as_str()) != Some(iss.as_str()) {
                return Err(invalid("the token has another issuer"));
            }
        }
        let auds: Vec<String> = match claims.get("aud") {
            Some(Value::String(a)) => vec![a.clone()],
            Some(Value::Array(a)) => a.iter().map(|x| x.to_string()).collect(),
            _ => Vec::new(),
        };
        if !auds.iter().any(|a| self.config.audiences.contains(a)) {
            return Err(invalid("the token was not issued for this server (audience)"));
        }
        let scopes: Vec<String> = match (claims.get("scope"), claims.get("scp")) {
            (Some(Value::String(s)), _) => s.split(' ').filter(|x| !x.is_empty()).map(str::to_string).collect(),
            (_, Some(Value::Array(a))) => a.iter().map(|x| x.to_string()).collect(),
            (_, Some(Value::String(s))) => s.split(' ').filter(|x| !x.is_empty()).map(str::to_string).collect(),
            _ => Vec::new(),
        };
        let name = claims.get(&self.config.principal_claim).map(|v| v.to_string()).filter(|s| !s.is_empty()).ok_or_else(|| invalid("the token has no subject"))?;
        Ok(Principal { name, scopes, method: self.config.mode.clone() })
    }

    fn keys(&self, kid: Option<&str>) -> std::result::Result<Vec<Jwk>, String> {
        let url = self.config.jwks_url.clone().ok_or("no JWKS URL is configured")?;
        let cached = self.jwks.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some((at, keys)) = &cached {
            let known = kid.map(|k| keys.iter().any(|j| j.kid.as_deref() == Some(k))).unwrap_or(true);
            if at.elapsed() < Duration::from_secs(600) && known {
                return Ok(keys.clone());
            }
            // an unknown kid may be a rotated key: refetch, at most once a minute
            let recent = self.last_jwks_fetch.lock().unwrap_or_else(|p| p.into_inner()).map(|t| t.elapsed() < Duration::from_secs(60)).unwrap_or(false);
            if recent {
                return Ok(keys.clone());
            }
        }
        *self.last_jwks_fetch.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
        let resp = cameleer_http::client::HttpClient::new().request("GET", &url, &[("Accept", "application/json")], None, Some(Duration::from_secs(10))).map_err(|e| format!("cannot fetch the JWKS: {}", e.message))?;
        if resp.status != 200 {
            return Err(format!("the JWKS endpoint answered {}", resp.status));
        }
        let keys = parse_jwks(&resp.body_text()).ok_or("invalid JWKS")?;
        *self.jwks.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), keys.clone()));
        Ok(keys)
    }

    // --- introspection ---

    fn introspect(&self, token: &str) -> std::result::Result<Principal, AuthError> {
        let key = sha256_hex(token.as_bytes());
        if let Some((at, r)) = self.introspected.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
            if at.elapsed() < Duration::from_secs(60) {
                return r.clone();
            }
        }
        let r = self.introspect_now(token);
        let mut cache = self.introspected.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() > 10_000 {
            cache.clear();
        }
        cache.insert(key, (Instant::now(), r.clone()));
        r
    }

    fn introspect_now(&self, token: &str) -> std::result::Result<Principal, AuthError> {
        let invalid = |d: &str| AuthError::Unauthorized { error: Some("invalid_token"), description: d.to_string() };
        let url = self.config.introspection_url.clone().unwrap_or_default();
        let body = format!("token={}&token_type_hint=access_token", cameleer_core::util::url_encode(token));
        let mut headers: Vec<(String, String)> = vec![("Content-Type".into(), "application/x-www-form-urlencoded".into()), ("Accept".into(), "application/json".into())];
        if let (Some(id), Some(secret)) = (&self.config.introspection_client_id, &self.config.introspection_client_secret) {
            headers.push(("Authorization".into(), format!("Basic {}", cameleer_tls::pem::base64_encode(format!("{}:{}", id, secret).as_bytes()))));
        }
        let hdrs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let resp = cameleer_http::client::HttpClient::new()
            .request("POST", &url, &hdrs, Some(body.as_bytes()), Some(Duration::from_secs(10)))
            .map_err(|_| AuthError::Unauthorized { error: None, description: "the authorization server is unreachable".into() })?;
        if resp.status != 200 {
            return Err(AuthError::Unauthorized { error: None, description: format!("introspection answered {}", resp.status) });
        }
        let v = cameleer_core::json::parse(&resp.body_text()).map_err(|_| invalid("invalid introspection response"))?;
        let m = v.as_object().cloned().ok_or_else(|| invalid("invalid introspection response"))?;
        if !m.get("active").map(|a| a.is_truthy()).unwrap_or(false) {
            return Err(invalid("the token is not active"));
        }
        let mut claims = m.clone();
        if !claims.contains_key("exp") {
            claims.insert("exp".into(), Value::Int(i64::MAX / 2));
        }
        self.check_claims(&claims)
    }
}

fn missing(what: &str) -> AuthError {
    AuthError::Unauthorized { error: None, description: format!("this server needs {}", what) }
}

fn hash_for(alg: &str) -> HashAlg {
    match &alg[2..] {
        "384" => HashAlg::Sha384,
        "512" => HashAlg::Sha512,
        _ => HashAlg::Sha256,
    }
}

fn decode_json(part: &str) -> Option<ObjectMap> {
    let raw = base64url_decode(part)?;
    cameleer_core::json::parse(&String::from_utf8(raw).ok()?).ok()?.as_object().cloned()
}

fn parse_jwks(text: &str) -> Option<Vec<Jwk>> {
    let v = cameleer_core::json::parse(text).ok()?;
    let keys = v.as_object()?.get("keys")?.as_array()?;
    let mut out = Vec::new();
    for k in keys {
        let Some(m) = k.as_object() else { continue };
        let s = |n: &str| m.get(n).and_then(|x| x.as_str()).map(str::to_string);
        if s("use").map(|u| u != "sig").unwrap_or(false) {
            continue;
        }
        let key = match s("kty").as_deref() {
            Some("RSA") => {
                let n = base64url_decode(&s("n")?)?;
                let e = base64url_decode(&s("e")?)?;
                JwkKey::Rsa(cameleer_tls::crypto::rsa::RsaPublicKey::new(&n, &e)?)
            }
            Some("EC") => {
                let curve = match s("crv").as_deref() {
                    Some("P-256") => cameleer_tls::crypto::ec::EcCurve::P256,
                    Some("P-384") => cameleer_tls::crypto::ec::EcCurve::P384,
                    _ => continue,
                };
                let mut point = vec![0x04];
                point.extend(base64url_decode(&s("x")?)?);
                point.extend(base64url_decode(&s("y")?)?);
                JwkKey::Ec(curve, point)
            }
            _ => continue,
        };
        out.push(Jwk { kid: s("kid"), key });
    }
    Some(out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::base64url;

    fn hs_token(secret: &[u8], claims: &str) -> String {
        let h = base64url(br#"{"alg":"HS256","typ":"JWT"}"#);
        let p = base64url(claims.as_bytes());
        let sig = HashAlg::Sha256.hmac(secret, format!("{}.{}", h, p).as_bytes());
        format!("{}.{}.{}", h, p, base64url(&sig))
    }

    fn jwt_auth() -> Authenticator {
        Authenticator::new(AuthConfig {
            mode: "jwt".into(),
            jwt_secret: Some(b"0123456789abcdef0123456789abcdef".to_vec()),
            audiences: vec!["https://mcp.example.com/mcp".into()],
            issuer: Some("https://issuer.example.com".into()),
            required_scopes: vec!["tools".into()],
            principal_claim: "sub".into(),
            ..Default::default()
        })
    }

    #[test]
    fn jwt_claims_are_checked() {
        let a = jwt_auth();
        let secret = b"0123456789abcdef0123456789abcdef";
        let exp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + 600;
        let good = hs_token(secret, &format!(r#"{{"sub":"ann","aud":"https://mcp.example.com/mcp","iss":"https://issuer.example.com","exp":{},"scope":"tools read"}}"#, exp));
        let p = a.authenticate(Some(&format!("Bearer {}", good)), None).unwrap();
        assert_eq!((p.name.as_str(), p.scopes.len()), ("ann", 2));
        let wrong_aud = hs_token(secret, &format!(r#"{{"sub":"ann","aud":"https://other","iss":"https://issuer.example.com","exp":{},"scope":"tools"}}"#, exp));
        assert!(matches!(a.authenticate(Some(&format!("Bearer {}", wrong_aud)), None), Err(AuthError::Unauthorized { .. })));
        let expired = hs_token(secret, r#"{"sub":"ann","aud":"https://mcp.example.com/mcp","iss":"https://issuer.example.com","exp":1000,"scope":"tools"}"#);
        assert!(a.authenticate(Some(&format!("Bearer {}", expired)), None).is_err());
        let no_scope = hs_token(secret, &format!(r#"{{"sub":"ann","aud":"https://mcp.example.com/mcp","iss":"https://issuer.example.com","exp":{}}}"#, exp));
        assert!(matches!(a.authenticate(Some(&format!("Bearer {}", no_scope)), None), Err(AuthError::Forbidden { .. })));
        let forged = hs_token(b"another-secret-another-secret-xx", &format!(r#"{{"sub":"ann","aud":"https://mcp.example.com/mcp","iss":"https://issuer.example.com","exp":{},"scope":"tools"}}"#, exp));
        assert!(a.authenticate(Some(&format!("Bearer {}", forged)), None).is_err());
        let none_alg = format!("{}.{}.", base64url(br#"{"alg":"none"}"#), base64url(br#"{"sub":"x"}"#));
        assert!(a.authenticate(Some(&format!("Bearer {}", none_alg)), None).is_err());
        assert!(matches!(a.authenticate(None, None), Err(AuthError::Unauthorized { error: None, .. })));
    }

    #[test]
    fn api_keys() {
        let a = Authenticator::new(AuthConfig { mode: "api-key".into(), api_keys: vec![("ci".into(), sha256_hex(b"k-0123456789abcdef"))], ..Default::default() });
        assert_eq!(a.authenticate(None, Some("k-0123456789abcdef")).unwrap().name, "ci");
        assert_eq!(a.authenticate(Some("Bearer k-0123456789abcdef"), None).unwrap().name, "ci");
        assert!(a.authenticate(Some("Bearer nope"), None).is_err());
    }

    #[test]
    fn challenges() {
        let a = jwt_auth();
        let c = a.challenge(&AuthError::Unauthorized { error: Some("invalid_token"), description: "expired".into() }, Some("https://mcp.example.com/.well-known/oauth-protected-resource/mcp"));
        assert_eq!(c, "Bearer realm=\"mcp\", resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource/mcp\", error=\"invalid_token\", error_description=\"expired\"");
    }
}
