//! Google Cloud components (`google-storage`, `google-pubsub`, `google-bigquery`,
//! `google-bigquery-sql`, `google-secret-manager`, `google-functions`, `google-firestore`,
//! `google-sheets`, `google-mail`, `google-calendar`, `google-drive`, `google-vertexai`,
//! `google-speech-to-text`, `google-text-to-speech`) and the shared OAuth2 service-account
//! authentication (with domain-wide delegation: `delegate` / `subject` = the impersonated user,
//! sent as the JWT `sub` claim).
//!
//! Authentication: a service account JSON key (`serviceAccountKey` option: `file:path`,
//! `classpath:path` (resolved as a file path), a plain path or the inline JSON; otherwise the
//! `GOOGLE_APPLICATION_CREDENTIALS` environment variable) is turned into a JWT signed with
//! RS256 (RSA PKCS#1 v1.5 + SHA-256, from `camel-tls`), exchanged at the key's `token_uri`
//! (`urn:ietf:params:oauth:grant-type:jwt-bearer`) for an access token, which is cached
//! process-wide until shortly before it expires. `authenticate=false` sends no credentials
//! (emulators).
//!
//! Endpoint override: `uriEndpointOverride` + `overrideEndpoint=true`, or `endpoint`
//! (`host:port` of an emulator, or a URL).

pub mod ai;
pub mod bigquery;
pub mod firestore;
pub mod functions;
pub mod pubsub;
pub mod pubsub_stream;
pub mod secretmanager;
pub mod sheets;
pub mod storage;
pub mod workspace;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use camel_core::error::{CamelError, Result};
use camel_core::value::Value;
use camel_tls::crypto::hash::HashAlg;
use camel_tls::PrivateKey;

use crate::http::{HttpClient, Proxy, Request, Response, Url};
use crate::util::{base64url_encode, cloud_error, encode_form, kinds, now_secs, parse_json, sha256, Opts};

/// Default OAuth2 scope.
pub const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// A parsed service account key file.
#[derive(Clone)]
pub struct ServiceAccountKey {
    pub project_id: String,
    pub private_key_id: String,
    pub client_email: String,
    pub token_uri: String,
    key: PrivateKey,
}

impl std::fmt::Debug for ServiceAccountKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAccountKey").field("client_email", &self.client_email).field("project_id", &self.project_id).finish()
    }
}

impl ServiceAccountKey {
    /// Parse the JSON key.
    pub fn parse(json: &str) -> Result<ServiceAccountKey> {
        let v = camel_core::json::parse(json)?;
        let s = |k: &str| v.get(k).map(|x| x.to_string()).unwrap_or_default();
        if !s("type").is_empty() && s("type") != "service_account" {
            return Err(CamelError::illegal_argument(format!("Unsupported Google credentials type '{}' (only service_account keys are supported)", s("type"))));
        }
        let pem = s("private_key");
        let key = PrivateKey::from_pem(pem.as_bytes()).map_err(|e| CamelError::illegal_argument(format!("Invalid service account private key: {}", e)))?;
        if !matches!(key, PrivateKey::Rsa(_)) {
            return Err(CamelError::illegal_argument("Service account private key must be an RSA key"));
        }
        let token_uri = s("token_uri");
        Ok(ServiceAccountKey {
            project_id: s("project_id"),
            private_key_id: s("private_key_id"),
            client_email: s("client_email"),
            token_uri: if token_uri.is_empty() { "https://oauth2.googleapis.com/token".into() } else { token_uri },
            key,
        })
    }

    /// Load from `file:`, `classpath:`, a path, or inline JSON.
    pub fn load(location: &str) -> Result<ServiceAccountKey> {
        let t = location.trim();
        if t.starts_with('{') {
            return Self::parse(t);
        }
        let path = t.strip_prefix("file:").or_else(|| t.strip_prefix("classpath:")).unwrap_or(t);
        let text = std::fs::read_to_string(path).map_err(|e| CamelError::io(format!("Cannot read service account key {}: {}", path, e)))?;
        Self::parse(&text)
    }

    /// RS256 signature of `data`.
    pub fn sign(&self, data: &[u8]) -> Result<Vec<u8>> {
        match &self.key {
            PrivateKey::Rsa(r) => r.sign_pkcs1(HashAlg::Sha256, &sha256(data)).ok_or_else(|| CamelError::illegal_state("RSA signing failed")),
            _ => Err(CamelError::illegal_state("Not an RSA key")),
        }
    }

    /// The public key (for verifying JWTs in tests / mocks).
    pub fn public_key(&self) -> camel_tls::PublicKey {
        self.key.public_key()
    }

    /// A signed JWT assertion for the token endpoint.
    pub fn jwt(&self, scope: &str, now: i64) -> Result<String> {
        self.jwt_for(scope, None, now)
    }

    /// A signed JWT assertion impersonating `subject` (domain-wide delegation, `sub` claim).
    pub fn jwt_for(&self, scope: &str, subject: Option<&str>, now: i64) -> Result<String> {
        let header = crate::util::obj([("alg", Value::from("RS256")), ("typ", Value::from("JWT")), ("kid", Value::from(self.private_key_id.clone()))]);
        let mut claims = crate::util::obj([
            ("iss", Value::from(self.client_email.clone())),
            ("scope", Value::from(scope)),
            ("aud", Value::from(self.token_uri.clone())),
            ("iat", Value::Int(now)),
            ("exp", Value::Int(now + 3600)),
        ]);
        if let (Some(s), Value::Object(m)) = (subject, &mut claims) {
            m.insert("sub".into(), Value::from(s));
        }
        let input = format!("{}.{}", base64url_encode(header.to_json().as_bytes()), base64url_encode(claims.to_json().as_bytes()));
        let sig = self.sign(input.as_bytes())?;
        Ok(format!("{}.{}", input, base64url_encode(&sig)))
    }
}

/// Verify an RS256 JWT with an RSA public key; returns the claims.
pub fn verify_jwt(jwt: &str, key: &camel_tls::PublicKey) -> std::result::Result<Value, String> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return Err("malformed JWT".into());
    }
    let dec = |s: &str| crate::util::base64_decode(s).ok_or_else(|| "bad base64url".to_string());
    let header = parse_json(&dec(parts[0])?).map_err(|e| e.to_string())?;
    if header.get("alg").map(|a| a.to_string()).as_deref() != Some("RS256") {
        return Err("unsupported JWT alg".into());
    }
    let sig = dec(parts[2])?;
    let digest = sha256(format!("{}.{}", parts[0], parts[1]).as_bytes());
    match key {
        camel_tls::PublicKey::Rsa(r) if r.verify_pkcs1(HashAlg::Sha256, &digest, &sig) => {}
        _ => return Err("invalid JWT signature".into()),
    }
    parse_json(&dec(parts[1])?).map_err(|e| e.to_string())
}

type TokenKey = (String, String, String, String);

fn token_cache() -> &'static Mutex<HashMap<TokenKey, (String, i64)>> {
    static CACHE: OnceLock<Mutex<HashMap<TokenKey, (String, i64)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(key: &ServiceAccountKey, scope: &str, subject: Option<&str>) -> TokenKey {
    (key.client_email.clone(), key.token_uri.clone(), scope.to_string(), subject.unwrap_or("").to_string())
}

/// Get an access token for `scope` (cached until 60 s before expiry).
pub fn access_token(key: &ServiceAccountKey, scope: &str, http: &HttpClient) -> Result<String> {
    access_token_as(key, scope, None, http)
}

/// Get an access token for `scope` impersonating `subject` (domain-wide delegation) when given.
pub fn access_token_as(key: &ServiceAccountKey, scope: &str, subject: Option<&str>, http: &HttpClient) -> Result<String> {
    let ck = cache_key(key, scope, subject);
    let now = now_secs();
    if let Some((tok, exp)) = token_cache().lock().unwrap().get(&ck) {
        if *exp > now + 60 {
            return Ok(tok.clone());
        }
    }
    let assertion = key.jwt_for(scope, subject, now)?;
    let body = encode_form(&[("grant_type".into(), "urn:ietf:params:oauth:grant-type:jwt-bearer".into()), ("assertion".into(), assertion)]);
    let req = Request::new("POST", Url::parse(&key.token_uri)?).header("Content-Type", "application/x-www-form-urlencoded").body(body.into_bytes());
    let resp = http.send(&req)?;
    let v = parse_json(&resp.body).unwrap_or_default();
    if !resp.is_success() {
        let msg = v.get("error_description").or_else(|| v.get("error")).map(|e| e.to_string()).unwrap_or_else(|| resp.text());
        return Err(cloud_error(kinds::GOOGLE_AUTH, format!("Error getting access token for service account {}: {} (status {})", key.client_email, msg, resp.status)));
    }
    let tok = v.get("access_token").map(|t| t.to_string()).filter(|t| !t.is_empty()).ok_or_else(|| cloud_error(kinds::GOOGLE_AUTH, "Token response without access_token"))?;
    let exp = now + v.get("expires_in").and_then(|e| e.as_i64().or_else(|| e.to_string().parse().ok())).unwrap_or(3600);
    token_cache().lock().unwrap().insert(ck, (tok.clone(), exp));
    Ok(tok)
}

/// Forget a cached token (after a 401).
pub fn invalidate_token(key: &ServiceAccountKey, scope: &str) {
    invalidate_token_as(key, scope, None);
}

/// Forget a cached delegated token.
pub fn invalidate_token_as(key: &ServiceAccountKey, scope: &str, subject: Option<&str>) {
    token_cache().lock().unwrap().remove(&cache_key(key, scope, subject));
}

/// Client configuration shared by the Google endpoints.
#[derive(Clone, Debug)]
pub struct GcpConfig {
    pub key: Option<Arc<ServiceAccountKey>>,
    pub authenticate: bool,
    pub endpoint: Option<String>,
    pub proxy: Proxy,
    pub trust_all_certificates: bool,
    /// User impersonated with domain-wide delegation (`delegate` / `subject` options).
    pub subject: Option<String>,
}

impl GcpConfig {
    /// Read the common options.
    pub fn from_opts(o: &mut Opts) -> Result<GcpConfig> {
        let authenticate = o.bool("authenticate", true)?;
        let key = match o.get("serviceAccountKey").or_else(|| std::env::var("GOOGLE_APPLICATION_CREDENTIALS").ok().filter(|s| !s.is_empty())) {
            Some(loc) if authenticate => Some(Arc::new(ServiceAccountKey::load(&loc)?)),
            _ => None,
        };
        let over = o.get("uriEndpointOverride");
        let override_flag = o.bool("overrideEndpoint", false)?;
        let endpoint = match (over, override_flag, o.get("endpoint")) {
            (Some(u), true, _) => Some(u),
            (_, _, Some(e)) => Some(if e.contains("://") { e } else { format!("http://{}", e) }),
            _ => None,
        };
        let proxy = match o.get("proxyHost") {
            Some(h) => Proxy::Explicit(format!("http://{}:{}", h, o.string("proxyPort", "3128"))),
            None => {
                o.ignore(&["proxyPort"]);
                Proxy::Environment
            }
        };
        o.ignore(&["storageClient", "pubsubEndpoint", "serializer", "loggerId"]);
        let subject = o.get("delegate").or_else(|| o.get("subject"));
        Ok(GcpConfig { key, authenticate, endpoint, proxy, trust_all_certificates: o.bool("trustAllCertificates", false)?, subject })
    }

    pub fn http(&self) -> HttpClient {
        HttpClient::new().with_proxy(self.proxy.clone()).trust_all(self.trust_all_certificates)
    }

    /// POST/GET/... a JSON request and parse the JSON response.
    pub fn json(&self, method: &str, url: &str, body: Option<&Value>, kind: &str) -> Result<Value> {
        let mut req = Request::new(method, Url::parse(url)?);
        if let Some(b) = body {
            req = req.header("Content-Type", "application/json").body(b.to_json().into_bytes());
        }
        let resp = self.send(req, kind)?;
        parse_json(&resp.body)
    }

    /// Base URL (`default` unless overridden), without trailing slash.
    pub fn base(&self, default: &str) -> String {
        self.endpoint.clone().unwrap_or_else(|| default.to_string()).trim_end_matches('/').to_string()
    }

    /// The project id of the key.
    pub fn project_id(&self) -> Option<String> {
        self.key.as_ref().map(|k| k.project_id.clone()).filter(|p| !p.is_empty())
    }

    /// A bearer token for `scope` (None when authentication is disabled).
    pub fn bearer(&self, scope: &str) -> Result<Option<String>> {
        if !self.authenticate {
            return Ok(None);
        }
        let key = self.key.as_ref().ok_or_else(|| cloud_error(kinds::GOOGLE_AUTH, "No Google credentials: set serviceAccountKey or GOOGLE_APPLICATION_CREDENTIALS (or authenticate=false for emulators)"))?;
        access_token_as(key, scope, self.subject.as_deref(), &self.http()).map(Some)
    }

    /// Forget the cached token for `scope` (after an authentication failure).
    pub fn invalidate(&self, scope: &str) {
        if let Some(k) = &self.key {
            invalidate_token_as(k, scope, self.subject.as_deref());
        }
    }

    /// Send with a bearer token (retrying once after a 401); non-2xx responses become errors of `kind`.
    pub fn send(&self, req: Request, kind: &str) -> Result<Response> {
        self.send_scoped(req, kind, CLOUD_PLATFORM_SCOPE)
    }

    /// [`GcpConfig::send`] with a token for another OAuth scope (e.g. Sheets).
    pub fn send_scoped(&self, req: Request, kind: &str, scope: &str) -> Result<Response> {
        let http = self.http();
        for attempt in 0..2 {
            let mut r = req.clone();
            if self.authenticate {
                let key = self.key.as_ref().ok_or_else(|| {
                    cloud_error(kinds::GOOGLE_AUTH, "No Google credentials: set serviceAccountKey or GOOGLE_APPLICATION_CREDENTIALS (or authenticate=false for emulators)")
                })?;
                r.set_header("Authorization", format!("Bearer {}", access_token_as(key, scope, self.subject.as_deref(), &http)?));
            }
            let resp = http.send(&r)?;
            if resp.status == 401 && attempt == 0 && self.authenticate {
                if let Some(k) = &self.key {
                    invalidate_token_as(k, scope, self.subject.as_deref());
                }
                continue;
            }
            if resp.is_success() {
                return Ok(resp);
            }
            return Err(google_error(kind, &resp));
        }
        unreachable!("loop returns")
    }
}

/// Build the exception for a Google API error (`{"error": {"code", "message", "status"}}`).
pub fn google_error(kind: &str, resp: &Response) -> CamelError {
    let v = parse_json(&resp.body).unwrap_or_default();
    let e = v.get("error").cloned().unwrap_or_default();
    let msg = e.get("message").map(|m| m.to_string()).unwrap_or_else(|| resp.text());
    let status = e.get("status").map(|m| m.to_string()).unwrap_or_default();
    cloud_error(kind, format!("{} {} (code: {}, status: {})", resp.status, msg, resp.status, status))
}

/// HTTP status from a Google exception message.
pub fn error_status(e: &CamelError) -> Option<u16> {
    e.message.split("(code: ").nth(1).and_then(|r| r.split(',').next()).and_then(|s| s.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key_json(token_uri: &str) -> String {
        crate::testing::gcp::service_account_json(token_uri, "camel@test-project.iam.gserviceaccount.com")
    }

    #[test]
    fn jwt_sign_and_verify() {
        let key = ServiceAccountKey::parse(&test_key_json("http://127.0.0.1:1/token")).unwrap();
        let jwt = key.jwt(CLOUD_PLATFORM_SCOPE, 1_700_000_000).unwrap();
        let claims = verify_jwt(&jwt, &key.public_key()).unwrap();
        assert_eq!(claims.get("iss").unwrap(), &Value::from("camel@test-project.iam.gserviceaccount.com"));
        assert_eq!(claims.get("exp").unwrap(), &Value::Int(1_700_003_600));
        let mut tampered = jwt.clone();
        tampered.insert(jwt.find('.').unwrap() + 3, 'x');
        assert!(verify_jwt(&tampered, &key.public_key()).is_err());
        assert!(ServiceAccountKey::parse(r#"{"type":"authorized_user"}"#).is_err());
    }
}
