//! AWS Signature Version 4 (header and presigned query-string signing) and the
//! matching server-side verification used by the mock servers.
//!
//! Canonical URI: S3 uses the (normalised) wire path as is; every other service
//! URI-encodes the wire path once more, as the AWS SDKs do.

use crate::http::{Request, ServerRequest, Url};
use crate::util::{amz_date, ct_eq, hex, hmac_sha256, percent_decode, sha256_hex, uri_encode};

/// Hash of an empty payload.
pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// Payload hash for presigned URLs / unsigned streaming.
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// AWS credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").field("access_key", &self.access_key).field("secret_key", &"***").finish()
    }
}

impl Credentials {
    pub fn new(access_key: impl Into<String>, secret_key: impl Into<String>) -> Credentials {
        Credentials { access_key: access_key.into(), secret_key: secret_key.into(), session_token: None }
    }
}

/// Canonical URI for a wire path.
pub fn canonical_uri(wire_path: &str, service: &str) -> String {
    let normalized: String = wire_path.split('/').map(|seg| uri_encode(&percent_decode(seg, false), false)).collect::<Vec<_>>().join("/");
    let p = if normalized.is_empty() { "/".to_string() } else { normalized };
    if service == "s3" {
        p
    } else {
        uri_encode(&p, true)
    }
}

/// Canonical query string for a raw query (parameters decoded, re-encoded and sorted).
pub fn canonical_query(raw_query: &str) -> String {
    let mut pairs: Vec<(String, String)> = raw_query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (uri_encode(&percent_decode(k, false), false), uri_encode(&percent_decode(v, false), false))
        })
        .collect();
    pairs.sort();
    pairs.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join("&")
}

fn collapse(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Canonical headers block (`name:value\n` for each signed header, in order).
pub fn canonical_headers(headers: &[(String, String)], signed: &[String]) -> String {
    let mut out = String::new();
    for name in signed {
        let values: Vec<String> = headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| collapse(v)).collect();
        out.push_str(&format!("{}:{}\n", name, values.join(",")));
    }
    out
}

/// The canonical request.
pub fn canonical_request(method: &str, wire_path: &str, raw_query: &str, headers: &[(String, String)], signed: &[String], payload_hash: &str, service: &str) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method,
        canonical_uri(wire_path, service),
        canonical_query(raw_query),
        canonical_headers(headers, signed),
        signed.join(";"),
        payload_hash
    )
}

/// `date/region/service/aws4_request`.
pub fn credential_scope(date: &str, region: &str, service: &str) -> String {
    format!("{}/{}/{}/aws4_request", date, region, service)
}

/// The string to sign.
pub fn string_to_sign(amz_date: &str, scope: &str, canonical_request: &str) -> String {
    format!("{}\n{}\n{}\n{}", ALGORITHM, amz_date, scope, sha256_hex(canonical_request.as_bytes()))
}

/// Derived signing key.
pub fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{}", secret).as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// Hex signature of a string to sign.
pub fn signature(secret: &str, date: &str, region: &str, service: &str, string_to_sign: &str) -> String {
    hex(&hmac_sha256(&signing_key(secret, date, region, service), string_to_sign.as_bytes()))
}

fn excluded(name: &str) -> bool {
    matches!(name, "authorization" | "user-agent" | "content-length" | "connection" | "expect" | "x-amzn-trace-id")
}

/// Sign `req` in place (adds `Host`, `X-Amz-Date`, `X-Amz-Security-Token`, for S3
/// `x-amz-content-sha256`, and `Authorization`). `secs` is the signing time.
pub fn sign_request(req: &mut Request, creds: &Credentials, region: &str, service: &str, secs: i64) {
    let date_time = amz_date(secs);
    let date = &date_time[..8];
    if req.get_header("Host").is_none() {
        let h = req.url.host_header();
        req.set_header("Host", h);
    }
    req.set_header("X-Amz-Date", date_time.clone());
    if let Some(t) = &creds.session_token {
        req.set_header("X-Amz-Security-Token", t.clone());
    }
    let payload_hash = match req.get_header("x-amz-content-sha256") {
        Some(h) => h.to_string(),
        None => sha256_hex(&req.body),
    };
    if service == "s3" && req.get_header("x-amz-content-sha256").is_none() {
        req.set_header("x-amz-content-sha256", payload_hash.clone());
    }
    let mut signed: Vec<String> = req.headers.iter().map(|(k, _)| k.to_ascii_lowercase()).filter(|k| !excluded(k)).collect();
    signed.sort();
    signed.dedup();
    let headers = req.headers.clone();
    let cr = canonical_request(&req.method, &req.url.path, req.url.query.as_deref().unwrap_or(""), &headers, &signed, &payload_hash, service);
    let scope = credential_scope(date, region, service);
    let sts = string_to_sign(&date_time, &scope, &cr);
    let sig = signature(&creds.secret_key, date, region, service, &sts);
    req.set_header(
        "Authorization",
        format!("{} Credential={}/{}, SignedHeaders={}, Signature={}", ALGORITHM, creds.access_key, scope, signed.join(";"), sig),
    );
}

/// Presign a GET (or other method) URL with query-string authentication.
pub fn presign_url(method: &str, url: &Url, creds: &Credentials, region: &str, service: &str, secs: i64, expires_secs: u64) -> Url {
    let date_time = amz_date(secs);
    let date = &date_time[..8];
    let scope = credential_scope(date, region, service);
    let mut query: Vec<String> = url.query.iter().filter(|q| !q.is_empty()).cloned().collect();
    query.push(format!("X-Amz-Algorithm={}", ALGORITHM));
    query.push(format!("X-Amz-Credential={}", uri_encode(&format!("{}/{}", creds.access_key, scope), false)));
    query.push(format!("X-Amz-Date={}", date_time));
    query.push(format!("X-Amz-Expires={}", expires_secs));
    if let Some(t) = &creds.session_token {
        query.push(format!("X-Amz-Security-Token={}", uri_encode(t, false)));
    }
    query.push("X-Amz-SignedHeaders=host".to_string());
    let raw_query = query.join("&");
    let headers = vec![("host".to_string(), url.host_header())];
    let cr = canonical_request(method, &url.path, &raw_query, &headers, &["host".to_string()], UNSIGNED_PAYLOAD, service);
    let sig = signature(&creds.secret_key, date, region, service, &string_to_sign(&date_time, &scope, &cr));
    let mut out = url.clone();
    out.query = Some(format!("{}&X-Amz-Signature={}", raw_query, sig));
    out
}

/// Parsed `Authorization: AWS4-HMAC-SHA256 ...` header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedAuthorization {
    pub access_key: String,
    pub date: String,
    pub region: String,
    pub service: String,
    pub signed_headers: Vec<String>,
    pub signature: String,
}

fn parse_credential(cred: &str) -> Option<(String, String, String, String)> {
    let parts: Vec<&str> = cred.split('/').collect();
    if parts.len() != 5 || parts[4] != "aws4_request" {
        return None;
    }
    Some((parts[0].into(), parts[1].into(), parts[2].into(), parts[3].into()))
}

/// Parse an Authorization header value.
pub fn parse_authorization(value: &str) -> Option<ParsedAuthorization> {
    let rest = value.strip_prefix(ALGORITHM)?.trim();
    let (mut cred, mut signed, mut sig) = (None, None, None);
    for part in rest.split(',') {
        let (k, v) = part.trim().split_once('=')?;
        match k {
            "Credential" => cred = Some(v),
            "SignedHeaders" => signed = Some(v),
            "Signature" => sig = Some(v),
            _ => {}
        }
    }
    let (access_key, date, region, service) = parse_credential(cred?)?;
    Some(ParsedAuthorization {
        access_key,
        date,
        region,
        service,
        signed_headers: signed?.split(';').map(|s| s.to_string()).collect(),
        signature: sig?.to_string(),
    })
}

/// Result of a successful verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRequest {
    pub access_key: String,
    pub region: String,
    pub service: String,
}

/// A verification failure: AWS error code and message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthFailure {
    pub code: &'static str,
    pub message: String,
    /// Service from the credential scope, when it could be parsed.
    pub service: Option<String>,
}

fn fail(code: &'static str, message: impl Into<String>, service: Option<&str>) -> AuthFailure {
    AuthFailure { code, message: message.into(), service: service.map(|s| s.to_string()) }
}

/// Verify a request signed with SigV4 (header or presigned query). `secret_for` maps
/// an access key to its secret; `now` is the server clock (epoch seconds).
pub fn verify_request(req: &ServerRequest, secret_for: &dyn Fn(&str) -> Option<String>, now: i64) -> Result<VerifiedRequest, AuthFailure> {
    let params = crate::util::parse_form(&req.query);
    if let Some(auth) = req.header("Authorization") {
        let a = parse_authorization(auth).ok_or_else(|| fail("IncompleteSignature", "Authorization header is malformed", None))?;
        let svc = Some(a.service.as_str());
        let secret = secret_for(&a.access_key)
            .ok_or_else(|| fail("InvalidAccessKeyId", "The AWS Access Key Id you provided does not exist in our records.", svc))?;
        let date_time = req.header("X-Amz-Date").ok_or_else(|| fail("MissingAuthenticationToken", "Missing X-Amz-Date", svc))?;
        if !date_time.starts_with(&a.date) {
            return Err(fail("SignatureDoesNotMatch", "Credential date does not match X-Amz-Date", svc));
        }
        let t = crate::util::parse_iso8601(date_time).unwrap_or(0);
        if (now - t).abs() > 15 * 60 {
            return Err(fail("RequestTimeTooSkewed", "The difference between the request time and the current time is too large.", svc));
        }
        if !a.signed_headers.iter().any(|h| h == "host") {
            return Err(fail("SignatureDoesNotMatch", "The host header must be signed", svc));
        }
        let body_hash = sha256_hex(&req.body);
        let payload_hash = match req.header("x-amz-content-sha256") {
            Some(h) if h == UNSIGNED_PAYLOAD => h.to_string(),
            Some(h) => {
                if h != body_hash {
                    return Err(fail("XAmzContentSHA256Mismatch", "The provided 'x-amz-content-sha256' header does not match what was computed.", svc));
                }
                h.to_string()
            }
            None => body_hash,
        };
        let cr = canonical_request(&req.method, &req.path, &req.query, &req.headers, &a.signed_headers, &payload_hash, &a.service);
        let sts = string_to_sign(date_time, &credential_scope(&a.date, &a.region, &a.service), &cr);
        let expected = signature(&secret, &a.date, &a.region, &a.service, &sts);
        if !ct_eq(expected.as_bytes(), a.signature.as_bytes()) {
            return Err(fail(
                "SignatureDoesNotMatch",
                "The request signature we calculated does not match the signature you provided. Check your key and signing method.",
                svc,
            ));
        }
        return Ok(VerifiedRequest { access_key: a.access_key, region: a.region, service: a.service });
    }
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    if let Some(sig) = get("X-Amz-Signature") {
        let cred = get("X-Amz-Credential").unwrap_or_default();
        let (ak, date, region, service) = parse_credential(&cred).ok_or_else(|| fail("AuthorizationQueryParametersError", "Bad X-Amz-Credential", None))?;
        let svc = Some(service.as_str());
        let secret = secret_for(&ak).ok_or_else(|| fail("InvalidAccessKeyId", "The AWS Access Key Id you provided does not exist in our records.", svc))?;
        let date_time = get("X-Amz-Date").unwrap_or_default();
        let expires: i64 = get("X-Amz-Expires").and_then(|e| e.parse().ok()).unwrap_or(0);
        let t = crate::util::parse_iso8601(&date_time).unwrap_or(0);
        if now > t + expires {
            return Err(fail("AccessDenied", "Request has expired", svc));
        }
        let signed: Vec<String> = get("X-Amz-SignedHeaders").unwrap_or_default().split(';').map(|s| s.to_string()).collect();
        let raw_query: String = req.query.split('&').filter(|p| !p.starts_with("X-Amz-Signature=")).collect::<Vec<_>>().join("&");
        let cr = canonical_request(&req.method, &req.path, &raw_query, &req.headers, &signed, UNSIGNED_PAYLOAD, &service);
        let expected = signature(&secret, &date, &region, &service, &string_to_sign(&date_time, &credential_scope(&date, &region, &service), &cr));
        if !ct_eq(expected.as_bytes(), sig.as_bytes()) {
            return Err(fail("SignatureDoesNotMatch", "The request signature we calculated does not match the signature you provided.", svc));
        }
        return Ok(VerifiedRequest { access_key: ak, region, service });
    }
    Err(fail("MissingAuthenticationToken", "Request is missing Authentication Token", None))
}

#[cfg(test)]
mod tests {
    //! Known-answer tests from the AWS SigV4 test suite and the AWS documentation.
    use super::*;
    use cameleer_core::util_time::epoch_from_civil;

    const SUITE_AK: &str = "AKIDEXAMPLE";
    const SUITE_SK: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
    const S3_AK: &str = "AKIAIOSFODNN7EXAMPLE";
    const S3_SK: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    fn suite_time() -> i64 {
        epoch_from_civil(2015, 8, 30, 12, 36, 0)
    }

    fn suite_request(method: &str, target: &str) -> Request {
        let url = Url::parse(&format!("https://example.amazonaws.com{}", target)).unwrap();
        Request::new(method, url).header("Host", "example.amazonaws.com")
    }

    fn sig_of(auth: &str) -> &str {
        auth.rsplit("Signature=").next().unwrap()
    }

    #[test]
    fn signing_key_derivation_doc_example() {
        // "Derive a signing key" example of the SigV4 documentation (IAM, 20150830, us-east-1)
        let k = signing_key(SUITE_SK, "20150830", "us-east-1", "iam");
        assert_eq!(hex(&k), "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9");
    }

    #[test]
    fn suite_get_vanilla() {
        let mut req = suite_request("GET", "/");
        let creds = Credentials::new(SUITE_AK, SUITE_SK);
        sign_request(&mut req, &creds, "us-east-1", "service", suite_time());
        let cr = canonical_request(&req.method, "/", "", &req.headers, &["host".into(), "x-amz-date".into()], EMPTY_SHA256, "service");
        assert_eq!(
            cr,
            "GET\n/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(sha256_hex(cr.as_bytes()), "bb579772317eb040ac9ed261061d46c1f17a8133879d6129b6e1c25292927e63");
        let auth = req.get_header("Authorization").unwrap();
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn suite_get_vanilla_query_order_key_case() {
        let mut req = suite_request("GET", "/?Param2=value2&Param1=value1");
        sign_request(&mut req, &Credentials::new(SUITE_AK, SUITE_SK), "us-east-1", "service", suite_time());
        assert_eq!(canonical_query("Param2=value2&Param1=value1"), "Param1=value1&Param2=value2");
        assert_eq!(sig_of(req.get_header("Authorization").unwrap()), "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500");
    }

    #[test]
    fn suite_post_vanilla() {
        let mut req = suite_request("POST", "/");
        sign_request(&mut req, &Credentials::new(SUITE_AK, SUITE_SK), "us-east-1", "service", suite_time());
        assert_eq!(sig_of(req.get_header("Authorization").unwrap()), "5da7c1a2acd57cee7505fc6676e4e544621c30862966e37dddb68e92efbe5d6b");
    }

    fn s3_time() -> i64 {
        epoch_from_civil(2013, 5, 24, 0, 0, 0)
    }

    #[test]
    fn s3_doc_get_object() {
        let url = Url::parse("https://examplebucket.s3.amazonaws.com/test.txt").unwrap();
        let mut req = Request::new("GET", url).header("Host", "examplebucket.s3.amazonaws.com").header("Range", "bytes=0-9");
        sign_request(&mut req, &Credentials::new(S3_AK, S3_SK), "us-east-1", "s3", s3_time());
        assert_eq!(
            req.get_header("Authorization").unwrap(),
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn s3_doc_put_object() {
        let url = Url::parse("https://examplebucket.s3.amazonaws.com/test%24file.text").unwrap();
        let mut req = Request::new("PUT", url)
            .header("Host", "examplebucket.s3.amazonaws.com")
            .header("Date", "Fri, 24 May 2013 00:00:00 GMT")
            .header("x-amz-storage-class", "REDUCED_REDUNDANCY")
            .body(&b"Welcome to Amazon S3."[..]);
        sign_request(&mut req, &Credentials::new(S3_AK, S3_SK), "us-east-1", "s3", s3_time());
        assert_eq!(req.get_header("x-amz-content-sha256"), Some("44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"));
        assert_eq!(sig_of(req.get_header("Authorization").unwrap()), "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd");
    }

    #[test]
    fn s3_doc_get_bucket_lifecycle_and_list_objects() {
        let creds = Credentials::new(S3_AK, S3_SK);
        let mut req = Request::new("GET", Url::parse("https://examplebucket.s3.amazonaws.com/?lifecycle").unwrap()).header("Host", "examplebucket.s3.amazonaws.com");
        sign_request(&mut req, &creds, "us-east-1", "s3", s3_time());
        assert_eq!(sig_of(req.get_header("Authorization").unwrap()), "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543");
        let mut req = Request::new("GET", Url::parse("https://examplebucket.s3.amazonaws.com/?max-keys=2&prefix=J").unwrap()).header("Host", "examplebucket.s3.amazonaws.com");
        sign_request(&mut req, &creds, "us-east-1", "s3", s3_time());
        assert_eq!(sig_of(req.get_header("Authorization").unwrap()), "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7");
    }

    #[test]
    fn s3_doc_presigned_url() {
        // "Authenticating Requests: Using Query Parameters" example (86400 s expiry)
        let url = Url::parse("https://examplebucket.s3.amazonaws.com/test.txt").unwrap();
        let p = presign_url("GET", &url, &Credentials::new(S3_AK, S3_SK), "us-east-1", "s3", s3_time(), 86400);
        assert!(p.query.as_deref().unwrap().ends_with("X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"), "{}", p);
    }

    #[test]
    fn verification_roundtrip() {
        let creds = Credentials::new("AK", "SECRET");
        let now = crate::util::now_secs();
        let url = Url::parse("http://127.0.0.1:9000/bucket/a%20key?list-type=2&prefix=x%2Fy").unwrap();
        let mut req = Request::new("PUT", url.clone()).header("Content-Type", "text/plain").body(&b"data"[..]);
        sign_request(&mut req, &creds, "eu-west-1", "s3", now);
        let server = ServerRequest {
            method: "PUT".into(),
            path: url.path.clone(),
            query: url.query.clone().unwrap(),
            headers: req.headers.clone(),
            body: req.body.clone(),
        };
        let lookup = |k: &str| if k == "AK" { Some("SECRET".to_string()) } else { None };
        let v = verify_request(&server, &lookup, now).unwrap();
        assert_eq!(v.service, "s3");
        assert_eq!(v.region, "eu-west-1");
        let wrong = |k: &str| if k == "AK" { Some("OTHER".to_string()) } else { None };
        assert_eq!(verify_request(&server, &wrong, now).unwrap_err().code, "SignatureDoesNotMatch");
        let mut tampered = server.clone();
        tampered.body = b"evil".to_vec();
        assert_eq!(verify_request(&tampered, &lookup, now).unwrap_err().code, "XAmzContentSHA256Mismatch");
        assert_eq!(verify_request(&server, &lookup, now + 3600).unwrap_err().code, "RequestTimeTooSkewed");
        // presigned
        let p = presign_url("GET", &Url::parse("http://127.0.0.1:9000/bucket/k").unwrap(), &creds, "eu-west-1", "s3", now, 60);
        let server = ServerRequest {
            method: "GET".into(),
            path: p.path.clone(),
            query: p.query.clone().unwrap(),
            headers: vec![("Host".into(), p.host_header())],
            body: vec![],
        };
        assert!(verify_request(&server, &lookup, now).is_ok());
        assert_eq!(verify_request(&server, &lookup, now + 120).unwrap_err().code, "AccessDenied");
    }

    #[test]
    fn parse_authorization_header() {
        let a = parse_authorization("AWS4-HMAC-SHA256 Credential=AK/20150830/us-east-1/sqs/aws4_request, SignedHeaders=host;x-amz-date, Signature=abc").unwrap();
        assert_eq!(a.service, "sqs");
        assert_eq!(a.signed_headers, vec!["host", "x-amz-date"]);
        assert!(parse_authorization("Bearer x").is_none());
    }
}
