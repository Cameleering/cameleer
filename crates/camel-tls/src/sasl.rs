//! SASL mechanisms used by messaging/database clients on top of TLS: `PLAIN`
//! (RFC 4616) and `SCRAM-SHA-256` / `SCRAM-SHA-512` (RFC 5802 / RFC 7677), built on the
//! hashes of [`crate::crypto::hash`]. [`ScramServer`] verifies clients (for embedded
//! test brokers).
//!
//! ```
//! use camel_tls::sasl::{ScramClient, ScramMechanism, ScramServer, ScramCredentials};
//! let creds = ScramCredentials::derive(ScramMechanism::Sha256, "pencil", b"salt-1234", 4096);
//! let mut client = ScramClient::new(ScramMechanism::Sha256, "user", "pencil");
//! let first = client.client_first();
//! let mut server = ScramServer::new(ScramMechanism::Sha256, |_user| Some(creds.clone()));
//! let server_first = server.handle_client_first(&first).unwrap();
//! let client_final = client.handle_server_first(&server_first).unwrap();
//! let server_final = server.handle_client_final(&client_final).unwrap();
//! client.verify_server_final(&server_final).unwrap();
//! ```

use crate::crypto::hash::HashAlg;
use crate::crypto::rng;
use crate::pem::{base64_decode, base64_encode};

/// PBKDF2 (RFC 8018) with HMAC-`alg`.
pub fn pbkdf2_hmac(alg: HashAlg, password: &[u8], salt: &[u8], iterations: u32, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut block = 1u32;
    while out.len() < len {
        let mut u = alg.hmac_parts(password, &[salt, &block.to_be_bytes()]);
        let mut t = u.clone();
        for _ in 1..iterations.max(1) {
            u = alg.hmac(password, &u);
            for (a, b) in t.iter_mut().zip(&u) {
                *a ^= b;
            }
        }
        let take = (len - out.len()).min(t.len());
        out.extend_from_slice(&t[..take]);
        block += 1;
    }
    out
}

/// The `PLAIN` initial response: `authzid NUL authcid NUL passwd`.
pub fn plain_initial_response(authzid: &str, user: &str, password: &str) -> Vec<u8> {
    format!("{}\0{}\0{}", authzid, user, password).into_bytes()
}

/// Parses a `PLAIN` response into `(authzid, user, password)`.
pub fn parse_plain(data: &[u8]) -> Option<(String, String, String)> {
    let s = std::str::from_utf8(data).ok()?;
    let mut it = s.split('\0');
    let (a, u, p) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() {
        return None;
    }
    Some((a.to_string(), u.to_string(), p.to_string()))
}

/// SCRAM hash variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScramMechanism {
    Sha256,
    Sha512,
}

impl ScramMechanism {
    /// `SCRAM-SHA-256` / `SCRAM-SHA-512` (case-insensitive).
    pub fn from_name(name: &str) -> Option<ScramMechanism> {
        match name.trim().to_ascii_uppercase().as_str() {
            "SCRAM-SHA-256" => Some(ScramMechanism::Sha256),
            "SCRAM-SHA-512" => Some(ScramMechanism::Sha512),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ScramMechanism::Sha256 => "SCRAM-SHA-256",
            ScramMechanism::Sha512 => "SCRAM-SHA-512",
        }
    }

    fn alg(self) -> HashAlg {
        match self {
            ScramMechanism::Sha256 => HashAlg::Sha256,
            ScramMechanism::Sha512 => HashAlg::Sha512,
        }
    }
}

/// SCRAM error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScramError(pub String);

impl std::fmt::Display for ScramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SCRAM authentication failed: {}", self.0)
    }
}

impl std::error::Error for ScramError {}

fn err<T>(m: impl Into<String>) -> Result<T, ScramError> {
    Err(ScramError(m.into()))
}

fn nonce() -> String {
    base64_encode(&rng::random_bytes(18)).replace(['+', '/', '='], "x")
}

/// `n` / `=2C` / `=3D` escaping of SCRAM user names.
fn saslname(s: &str) -> String {
    s.replace('=', "=3D").replace(',', "=2C")
}

fn attr<'a>(msg: &'a str, key: char) -> Option<&'a str> {
    msg.split(',').find_map(|p| p.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

fn xor(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b).map(|(x, y)| x ^ y).collect()
}

/// Derived SCRAM credentials (what a server stores).
#[derive(Clone, Debug)]
pub struct ScramCredentials {
    pub salt: Vec<u8>,
    pub iterations: u32,
    pub stored_key: Vec<u8>,
    pub server_key: Vec<u8>,
}

impl ScramCredentials {
    pub fn derive(mech: ScramMechanism, password: &str, salt: &[u8], iterations: u32) -> ScramCredentials {
        let alg = mech.alg();
        let salted = pbkdf2_hmac(alg, password.as_bytes(), salt, iterations, alg.output_len());
        let client_key = alg.hmac(&salted, b"Client Key");
        ScramCredentials { salt: salt.to_vec(), iterations, stored_key: alg.hash(&client_key), server_key: alg.hmac(&salted, b"Server Key") }
    }
}

/// Client side of a SCRAM exchange (no channel binding: `n,,`).
pub struct ScramClient {
    mech: ScramMechanism,
    user: String,
    password: String,
    nonce: String,
    client_first_bare: String,
    server_signature: Option<Vec<u8>>,
}

impl ScramClient {
    pub fn new(mech: ScramMechanism, user: &str, password: &str) -> ScramClient {
        ScramClient { mech, user: user.to_string(), password: password.to_string(), nonce: nonce(), client_first_bare: String::new(), server_signature: None }
    }

    /// The client-first message (`n,,n=user,r=nonce`).
    pub fn client_first(&mut self) -> String {
        self.client_first_bare = format!("n={},r={}", saslname(&self.user), self.nonce);
        format!("n,,{}", self.client_first_bare)
    }

    /// Processes server-first (`r=..,s=..,i=..`) and returns client-final.
    pub fn handle_server_first(&mut self, server_first: &str) -> Result<String, ScramError> {
        if let Some(e) = attr(server_first, 'e') {
            return err(format!("server error: {e}"));
        }
        let r = attr(server_first, 'r').ok_or_else(|| ScramError("missing nonce".into()))?;
        if !r.starts_with(&self.nonce) || r.len() <= self.nonce.len() {
            return err("server nonce does not extend the client nonce");
        }
        let salt = attr(server_first, 's').and_then(base64_decode).ok_or_else(|| ScramError("missing or invalid salt".into()))?;
        let iterations: u32 = attr(server_first, 'i').and_then(|i| i.parse().ok()).ok_or_else(|| ScramError("missing iteration count".into()))?;
        if iterations == 0 {
            return err("invalid iteration count");
        }
        let alg = self.mech.alg();
        let salted = pbkdf2_hmac(alg, self.password.as_bytes(), &salt, iterations, alg.output_len());
        let client_key = alg.hmac(&salted, b"Client Key");
        let stored_key = alg.hash(&client_key);
        let without_proof = format!("c=biws,r={}", r);
        let auth_message = format!("{},{},{}", self.client_first_bare, server_first, without_proof);
        let client_signature = alg.hmac(&stored_key, auth_message.as_bytes());
        let proof = xor(&client_key, &client_signature);
        let server_key = alg.hmac(&salted, b"Server Key");
        self.server_signature = Some(alg.hmac(&server_key, auth_message.as_bytes()));
        Ok(format!("{},p={}", without_proof, base64_encode(&proof)))
    }

    /// Verifies server-final (`v=signature`).
    pub fn verify_server_final(&self, server_final: &str) -> Result<(), ScramError> {
        if let Some(e) = attr(server_final, 'e') {
            return err(format!("server error: {e}"));
        }
        let v = attr(server_final, 'v').and_then(base64_decode).ok_or_else(|| ScramError("missing server signature".into()))?;
        match &self.server_signature {
            Some(s) if crate::util::ct_eq(s, &v) => Ok(()),
            _ => err("server signature mismatch"),
        }
    }
}

/// Server side of a SCRAM exchange (for embedded test servers).
pub struct ScramServer<F: Fn(&str) -> Option<ScramCredentials>> {
    mech: ScramMechanism,
    lookup: F,
    user: Option<String>,
    creds: Option<ScramCredentials>,
    client_first_bare: String,
    server_first: String,
    nonce: String,
}

impl<F: Fn(&str) -> Option<ScramCredentials>> ScramServer<F> {
    pub fn new(mech: ScramMechanism, lookup: F) -> Self {
        ScramServer { mech, lookup, user: None, creds: None, client_first_bare: String::new(), server_first: String::new(), nonce: String::new() }
    }

    /// The authenticated user after a successful exchange.
    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    pub fn handle_client_first(&mut self, client_first: &str) -> Result<String, ScramError> {
        let bare = client_first.strip_prefix("n,,").or_else(|| client_first.strip_prefix("y,,")).ok_or_else(|| ScramError("unsupported channel binding".into()))?;
        let user = attr(bare, 'n').ok_or_else(|| ScramError("missing user".into()))?.replace("=2C", ",").replace("=3D", "=");
        let cnonce = attr(bare, 'r').ok_or_else(|| ScramError("missing nonce".into()))?;
        let creds = (self.lookup)(&user).ok_or_else(|| ScramError(format!("unknown user {user}")))?;
        self.nonce = format!("{}{}", cnonce, nonce());
        self.server_first = format!("r={},s={},i={}", self.nonce, base64_encode(&creds.salt), creds.iterations);
        self.client_first_bare = bare.to_string();
        self.user = Some(user);
        self.creds = Some(creds);
        Ok(self.server_first.clone())
    }

    pub fn handle_client_final(&mut self, client_final: &str) -> Result<String, ScramError> {
        let creds = self.creds.clone().ok_or_else(|| ScramError("unexpected client-final".into()))?;
        let (without_proof, proof) = client_final.rsplit_once(",p=").ok_or_else(|| ScramError("missing proof".into()))?;
        if attr(without_proof, 'r') != Some(self.nonce.as_str()) {
            return err("nonce mismatch");
        }
        let proof = base64_decode(proof).ok_or_else(|| ScramError("invalid proof".into()))?;
        let alg = self.mech.alg();
        let auth_message = format!("{},{},{}", self.client_first_bare, self.server_first, without_proof);
        let client_signature = alg.hmac(&creds.stored_key, auth_message.as_bytes());
        let client_key = xor(&proof, &client_signature);
        if proof.len() != client_signature.len() || !crate::util::ct_eq(&alg.hash(&client_key), &creds.stored_key) {
            self.user = None;
            return err("invalid credentials");
        }
        Ok(format!("v={}", base64_encode(&alg.hmac(&creds.server_key, auth_message.as_bytes()))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbkdf2_rfc7914_vector() {
        // RFC 7914 section 11: PBKDF2-HMAC-SHA256, "passwd"/"salt", c=1, dkLen=64
        let dk = pbkdf2_hmac(HashAlg::Sha256, b"passwd", b"salt", 1, 64);
        assert_eq!(crate::util::hex(&dk[..16]), "55ac046e56e3089fec1691c22544b605");
    }

    #[test]
    fn rfc7677_example() {
        // RFC 7677 section 3 test vector (client side, fixed nonce)
        let mut c = ScramClient::new(ScramMechanism::Sha256, "user", "pencil");
        c.nonce = "rOprNGfwEbeRWgbNEkqO".into();
        assert_eq!(c.client_first(), "n,,n=user,r=rOprNGfwEbeRWgbNEkqO");
        let fin = c.handle_server_first("r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096").unwrap();
        assert_eq!(fin, "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=");
        c.verify_server_final("v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=").unwrap();
        assert!(c.verify_server_final("v=AAAA").is_err());
    }

    #[test]
    fn round_trip_sha512_and_wrong_password() {
        let creds = ScramCredentials::derive(ScramMechanism::Sha512, "s3cret", b"NaCl", 1000);
        let run = |pw: &str| {
            let mut c = ScramClient::new(ScramMechanism::Sha512, "alice", pw);
            let mut s = ScramServer::new(ScramMechanism::Sha512, |u| (u == "alice").then(|| creds.clone()));
            let sf = s.handle_client_first(&c.client_first())?;
            let cf = c.handle_server_first(&sf)?;
            let fin = s.handle_client_final(&cf)?;
            c.verify_server_final(&fin)
        };
        run("s3cret").unwrap();
        assert!(run("wrong").is_err());
        assert_eq!(parse_plain(&plain_initial_response("", "u", "p")), Some(("".into(), "u".into(), "p".into())));
    }
}
