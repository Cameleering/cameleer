//! gRPC call authentication (Camel `authenticationType`): HMAC-signed JWTs (`JWT`, with
//! `jwtAlgorithm` / `jwtSecret` / `jwtIssuer` / `jwtSubject`, verified by consumers) and
//! Google service-account JWT access tokens (`GOOGLE`, `serviceAccountResource`): a
//! self-signed RS256 JWT whose audience is `https://<host>/<package.Service>`, used directly
//! as the bearer token (no OAuth token exchange), as Google APIs accept for gRPC.

use std::sync::Mutex;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::Value;
use cameleer_tls::crypto::hash::HashAlg;
use cameleer_tls::keys::PrivateKey;

use crate::component::{GrpcClientInterceptor, GrpcServerInterceptor};
use crate::grpc::{Code, Metadata, Status};

/// `jwtAlgorithm`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JwtAlgorithm {
    Hmac256,
    Hmac384,
    Hmac512,
}

impl JwtAlgorithm {
    pub fn parse(s: &str) -> Option<JwtAlgorithm> {
        Some(match s.to_ascii_uppercase().as_str() {
            "HMAC256" | "HS256" => JwtAlgorithm::Hmac256,
            "HMAC384" | "HS384" => JwtAlgorithm::Hmac384,
            "HMAC512" | "HS512" => JwtAlgorithm::Hmac512,
            _ => return None,
        })
    }
    fn jose(self) -> &'static str {
        match self {
            JwtAlgorithm::Hmac256 => "HS256",
            JwtAlgorithm::Hmac384 => "HS384",
            JwtAlgorithm::Hmac512 => "HS512",
        }
    }
    fn hash(self) -> HashAlg {
        match self {
            JwtAlgorithm::Hmac256 => HashAlg::Sha256,
            JwtAlgorithm::Hmac384 => HashAlg::Sha384,
            JwtAlgorithm::Hmac512 => HashAlg::Sha512,
        }
    }
}

/// base64url without padding.
pub fn b64url(b: &[u8]) -> String {
    cameleer_tls::pem::base64_encode(b).trim_end_matches('=').replace('+', "-").replace('/', "_")
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut t = s.replace('-', "+").replace('_', "/");
    while t.len() % 4 != 0 {
        t.push('=');
    }
    cameleer_tls::pem::base64_decode(&t)
}

fn json_str(s: &str) -> String {
    Value::String(s.to_string()).to_json()
}

fn now_secs() -> i64 {
    cameleer_core::util_time::now_millis() / 1000
}

/// An HMAC-signed JWT with optional `iss` / `sub` claims (like Camel's `JwtHelper`).
pub fn create_hmac_jwt(alg: JwtAlgorithm, secret: &str, issuer: Option<&str>, subject: Option<&str>) -> String {
    let header = format!("{{\"typ\":\"JWT\",\"alg\":\"{}\"}}", alg.jose());
    let mut claims = Vec::new();
    if let Some(i) = issuer {
        claims.push(format!("\"iss\":{}", json_str(i)));
    }
    if let Some(s) = subject {
        claims.push(format!("\"sub\":{}", json_str(s)));
    }
    let payload = format!("{{{}}}", claims.join(","));
    let input = format!("{}.{}", b64url(header.as_bytes()), b64url(payload.as_bytes()));
    let sig = alg.hash().hmac(secret.as_bytes(), input.as_bytes());
    format!("{}.{}", input, b64url(&sig))
}

/// Verifies an HMAC JWT: algorithm, signature (constant time), `exp` / `nbf` when present and
/// the expected issuer / subject. Returns the claims.
pub fn verify_hmac_jwt(token: &str, alg: JwtAlgorithm, secret: &str, issuer: Option<&str>, subject: Option<&str>) -> std::result::Result<Value, String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("malformed JWT".into());
    }
    let header = b64url_decode(parts[0]).and_then(|b| String::from_utf8(b).ok()).ok_or("invalid JWT header")?;
    let header = cameleer_core::json::parse(&header).map_err(|_| "invalid JWT header JSON")?;
    if header.get("alg").and_then(|a| a.as_str()) != Some(alg.jose()) {
        return Err(format!("unexpected JWT algorithm (expected {})", alg.jose()));
    }
    let expected = alg.hash().hmac(secret.as_bytes(), format!("{}.{}", parts[0], parts[1]).as_bytes());
    let sig = b64url_decode(parts[2]).ok_or("invalid JWT signature encoding")?;
    if sig.len() != expected.len() || sig.iter().zip(&expected).fold(0u8, |a, (x, y)| a | (x ^ y)) != 0 {
        return Err("invalid JWT signature".into());
    }
    let claims = b64url_decode(parts[1]).and_then(|b| String::from_utf8(b).ok()).ok_or("invalid JWT payload")?;
    let claims = cameleer_core::json::parse(&claims).map_err(|_| "invalid JWT payload JSON")?;
    let now = now_secs();
    if let Some(exp) = claims.get("exp").and_then(|v| v.as_i64()) {
        if now >= exp {
            return Err("JWT expired".into());
        }
    }
    if let Some(nbf) = claims.get("nbf").and_then(|v| v.as_i64()) {
        if now < nbf {
            return Err("JWT not valid yet".into());
        }
    }
    for (claim, want) in [("iss", issuer), ("sub", subject)] {
        if let Some(w) = want {
            if claims.get(claim).and_then(|v| v.as_str()) != Some(w) {
                return Err(format!("JWT claim {} does not match", claim));
            }
        }
    }
    Ok(claims)
}

/// A Google service account key (JSON key file).
#[derive(Clone, Debug)]
pub struct ServiceAccount {
    pub client_email: String,
    pub private_key_id: Option<String>,
    pub key: PrivateKey,
}

impl ServiceAccount {
    pub fn from_json(text: &str) -> Result<ServiceAccount> {
        let v = cameleer_core::json::parse(text)?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
        let email = s("client_email").ok_or_else(|| CamelError::illegal_argument("service account JSON without client_email"))?;
        let pem = s("private_key").ok_or_else(|| CamelError::illegal_argument("service account JSON without private_key"))?;
        let key = PrivateKey::from_pem(pem.as_bytes()).map_err(|e| CamelError::illegal_argument(format!("invalid service account private key: {}", e)))?;
        if !matches!(key, PrivateKey::Rsa(_)) {
            return Err(CamelError::illegal_argument("service account private keys must be RSA"));
        }
        Ok(ServiceAccount { client_email: email, private_key_id: s("private_key_id"), key })
    }

    /// A self-signed JWT access token (RS256) for `audience`, valid for an hour from `iat`.
    pub fn jwt_access_token(&self, audience: &str, iat: i64) -> Result<String> {
        let header = match &self.private_key_id {
            Some(kid) => format!("{{\"alg\":\"RS256\",\"typ\":\"JWT\",\"kid\":{}}}", json_str(kid)),
            None => "{\"alg\":\"RS256\",\"typ\":\"JWT\"}".to_string(),
        };
        let payload = format!(
            "{{\"iss\":{e},\"sub\":{e},\"aud\":{a},\"iat\":{i},\"exp\":{x}}}",
            e = json_str(&self.client_email),
            a = json_str(audience),
            i = iat,
            x = iat + 3600
        );
        let input = format!("{}.{}", b64url(header.as_bytes()), b64url(payload.as_bytes()));
        let PrivateKey::Rsa(rsa) = &self.key else { unreachable!() };
        let digest = HashAlg::Sha256.hash(input.as_bytes());
        let sig = rsa.sign_pkcs1(HashAlg::Sha256, &digest).ok_or_else(|| CamelError::new("GeneralSecurityException", "RSA signing failed"))?;
        Ok(format!("{}.{}", input, b64url(&sig)))
    }
}

/// Producer side `authenticationType=JWT`: adds `authorization: Bearer <jwt>`.
pub struct JwtClientInterceptor {
    token: String,
}

impl JwtClientInterceptor {
    pub fn new(alg: JwtAlgorithm, secret: &str, issuer: Option<&str>, subject: Option<&str>) -> Self {
        JwtClientInterceptor { token: create_hmac_jwt(alg, secret, issuer, subject) }
    }
}

impl GrpcClientInterceptor for JwtClientInterceptor {
    fn intercept_call(&self, _method_path: &str, metadata: &mut Metadata) -> Result<()> {
        metadata.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
        metadata.push(("authorization".to_string(), format!("Bearer {}", self.token)));
        Ok(())
    }
}

/// Producer side `authenticationType=GOOGLE`: service-account JWT access tokens, re-signed
/// when less than five minutes of validity remain.
pub struct GoogleClientInterceptor {
    account: ServiceAccount,
    audience: String,
    cached: Mutex<Option<(String, i64)>>,
}

impl GoogleClientInterceptor {
    /// `audience`: `https://<host>/<package.Service>`.
    pub fn new(account: ServiceAccount, audience: String) -> Self {
        GoogleClientInterceptor { account, audience, cached: Mutex::new(None) }
    }

    /// The current token.
    pub fn token(&self) -> Result<String> {
        let now = now_secs();
        let mut g = self.cached.lock().unwrap();
        if let Some((t, exp)) = g.as_ref() {
            if *exp - now > 300 {
                return Ok(t.clone());
            }
        }
        let t = self.account.jwt_access_token(&self.audience, now)?;
        *g = Some((t.clone(), now + 3600));
        Ok(t)
    }
}

impl GrpcClientInterceptor for GoogleClientInterceptor {
    fn intercept_call(&self, _method_path: &str, metadata: &mut Metadata) -> Result<()> {
        let t = self.token()?;
        metadata.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
        metadata.push(("authorization".to_string(), format!("Bearer {}", t)));
        Ok(())
    }
}

/// Consumer side `authenticationType=JWT`: calls without a valid bearer JWT fail with
/// `UNAUTHENTICATED`.
pub struct JwtServerInterceptor {
    pub algorithm: JwtAlgorithm,
    pub secret: String,
    pub issuer: Option<String>,
    pub subject: Option<String>,
}

impl GrpcServerInterceptor for JwtServerInterceptor {
    fn intercept_call(&self, _method_path: &str, metadata: &Metadata) -> std::result::Result<(), Status> {
        let auth = metadata.iter().find(|(k, _)| k.eq_ignore_ascii_case("authorization")).map(|(_, v)| v.as_str());
        let token = auth.and_then(|a| a.strip_prefix("Bearer ").or_else(|| a.strip_prefix("bearer "))).ok_or_else(|| Status::new(Code::Unauthenticated, "Authorization token is missing"))?;
        verify_hmac_jwt(token.trim(), self.algorithm, &self.secret, self.issuer.as_deref(), self.subject.as_deref()).map(|_| ()).map_err(|e| Status::new(Code::Unauthenticated, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_jwt_round_trip_and_rejections() {
        let t = create_hmac_jwt(JwtAlgorithm::Hmac256, "secret", Some("camel"), Some("tester"));
        // RFC 7515 style: three base64url parts
        assert_eq!(t.split('.').count(), 3);
        let claims = verify_hmac_jwt(&t, JwtAlgorithm::Hmac256, "secret", Some("camel"), Some("tester")).unwrap();
        assert_eq!(claims.get("iss"), Some(&Value::from("camel")));
        assert!(verify_hmac_jwt(&t, JwtAlgorithm::Hmac256, "wrong", None, None).is_err());
        assert!(verify_hmac_jwt(&t, JwtAlgorithm::Hmac512, "secret", None, None).is_err());
        assert!(verify_hmac_jwt(&t, JwtAlgorithm::Hmac256, "secret", Some("other"), None).is_err());
        assert!(verify_hmac_jwt("a.b", JwtAlgorithm::Hmac256, "secret", None, None).is_err());
        // a known HS256 token (jwt.io's example, secret "your-256-bit-secret")
        let jwtio = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let c = verify_hmac_jwt(jwtio, JwtAlgorithm::Hmac256, "your-256-bit-secret", None, Some("1234567890")).unwrap();
        assert_eq!(c.get("name"), Some(&Value::from("John Doe")));
        for a in [JwtAlgorithm::Hmac384, JwtAlgorithm::Hmac512] {
            let t = create_hmac_jwt(a, "k", None, None);
            assert!(verify_hmac_jwt(&t, a, "k", None, None).is_ok());
        }
    }
}
