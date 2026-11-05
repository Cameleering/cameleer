//! OpenSSH certificates (`PROTOCOL.certkeys`): `*-cert-v01@openssh.com` user and host
//! certificates — parsing, signature / validity / principal checks, and signing (for tests
//! and embedded CAs).

use cameleer_tls::pem::{base64_decode, base64_encode};

use crate::error::{SshError, SshResult};
use crate::keys::{SshPrivateKey, SshPublicKey};
use crate::wire::{Reader, Writer};

/// Certificate type: user.
pub const USER_CERT: u32 = 1;
/// Certificate type: host.
pub const HOST_CERT: u32 = 2;

const SUFFIX: &str = "-cert-v01@openssh.com";

/// Certificate signature / host key algorithm names.
pub const CERT_ALGORITHMS: &[&str] = &[
    "ssh-ed25519-cert-v01@openssh.com",
    "ecdsa-sha2-nistp256-cert-v01@openssh.com",
    "ecdsa-sha2-nistp384-cert-v01@openssh.com",
    "rsa-sha2-512-cert-v01@openssh.com",
    "rsa-sha2-256-cert-v01@openssh.com",
];

/// The plain signature algorithm of a certificate algorithm (`rsa-sha2-256-cert-v01@...`
/// -> `rsa-sha2-256`), or None for non-certificate names.
pub fn plain_algorithm(alg: &str) -> Option<&str> {
    alg.strip_suffix(SUFFIX)
}

/// Whether `alg` is a certificate algorithm.
pub fn is_cert_algorithm(alg: &str) -> bool {
    alg.ends_with(SUFFIX)
}

fn err(m: impl Into<String>) -> SshError {
    SshError::Key(m.into())
}

/// A parsed OpenSSH certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshCertificate {
    /// The certified public key.
    pub key: SshPublicKey,
    pub serial: u64,
    /// [`USER_CERT`] or [`HOST_CERT`].
    pub cert_type: u32,
    pub key_id: String,
    pub principals: Vec<String>,
    pub valid_after: u64,
    pub valid_before: u64,
    pub critical_options: Vec<(String, Vec<u8>)>,
    pub extensions: Vec<(String, Vec<u8>)>,
    /// The CA key.
    pub signature_key: SshPublicKey,
    /// The full certificate blob.
    pub blob: Vec<u8>,
    signed_len: usize,
    signature: Vec<u8>,
}

fn options(data: &[u8]) -> SshResult<Vec<(String, Vec<u8>)>> {
    let mut r = Reader::new(data);
    let mut out = Vec::new();
    while !r.is_empty() {
        out.push((r.str()?, r.string()?.to_vec()));
    }
    Ok(out)
}

impl SshCertificate {
    /// The certificate type name (`ssh-ed25519-cert-v01@openssh.com`...).
    pub fn cert_type_name(&self) -> String {
        format!("{}{}", self.key.key_type(), SUFFIX)
    }

    /// Certificate algorithms usable with this certificate, preferred first.
    pub fn algorithms(&self) -> Vec<String> {
        self.key.signature_algorithms().iter().map(|a| format!("{}{}", a, SUFFIX)).collect()
    }

    /// Parses a certificate blob.
    pub fn from_blob(blob: &[u8]) -> SshResult<SshCertificate> {
        let mut r = Reader::new(blob);
        let t = r.str()?;
        let key_type = t.strip_suffix(SUFFIX).ok_or_else(|| err(format!("not a certificate: {}", t)))?;
        let _nonce = r.string()?;
        // the key fields follow the nonce: rebuild a plain public key blob
        let mut kb = Writer::new();
        kb.str(key_type);
        match key_type {
            "ssh-ed25519" => {
                kb.string(r.string()?);
            }
            "ecdsa-sha2-nistp256" | "ecdsa-sha2-nistp384" => {
                let curve = r.string()?;
                let q = r.string()?;
                kb.string(curve).string(q);
            }
            "ssh-rsa" => {
                let e = r.mpint()?;
                let n = r.mpint()?;
                kb.mpint(e).mpint(n);
            }
            other => return Err(err(format!("unsupported certificate key type {}", other))),
        }
        let key = SshPublicKey::from_blob(&kb.finish())?;
        let serial = r.u64()?;
        let cert_type = r.u32()?;
        let key_id = r.str()?;
        let mut pr = Reader::new(r.string()?);
        let mut principals = Vec::new();
        while !pr.is_empty() {
            principals.push(pr.str()?);
        }
        let valid_after = r.u64()?;
        let valid_before = r.u64()?;
        let critical_options = options(r.string()?)?;
        let extensions = options(r.string()?)?;
        let _reserved = r.string()?;
        let signature_key = SshPublicKey::from_blob(r.string()?)?;
        let signed_len = blob.len() - r.remaining().len();
        let signature = r.string()?.to_vec();
        if !r.is_empty() {
            return Err(err("trailing data after certificate"));
        }
        Ok(SshCertificate { key, serial, cert_type, key_id, principals, valid_after, valid_before, critical_options, extensions, signature_key, blob: blob.to_vec(), signed_len, signature })
    }

    /// Parses a `-cert.pub` line (`type base64 [comment]`).
    pub fn from_openssh(line: &str) -> SshResult<SshCertificate> {
        let mut it = line.split_whitespace();
        let t = it.next().ok_or_else(|| err("empty certificate line"))?;
        let blob = it.next().and_then(base64_decode).ok_or_else(|| err("bad base64 in certificate"))?;
        let c = SshCertificate::from_blob(&blob)?;
        if c.cert_type_name() != t {
            return Err(err("certificate type mismatch"));
        }
        Ok(c)
    }

    /// One-line OpenSSH representation.
    pub fn to_openssh(&self, comment: &str) -> String {
        let s = format!("{} {}", self.cert_type_name(), base64_encode(&self.blob));
        if comment.is_empty() {
            s
        } else {
            format!("{} {}", s, comment)
        }
    }

    /// Whether the CA signature over the certificate is valid.
    pub fn signature_valid(&self) -> bool {
        let alg = match Reader::new(&self.signature).str() {
            Ok(a) => a,
            Err(_) => return false,
        };
        self.signature_key.verify(&alg, &self.blob[..self.signed_len], &self.signature)
    }

    /// Checks type, CA signature, validity period (`now` in seconds since the epoch) and that
    /// `principal` is listed (an empty principal list matches any principal, as OpenSSH does
    /// for host certificates only).
    pub fn validate(&self, cert_type: u32, principal: &str, now: u64) -> Result<(), String> {
        if self.cert_type != cert_type {
            return Err(format!("certificate is a {} certificate", if self.cert_type == USER_CERT { "user" } else { "host" }));
        }
        if !self.signature_valid() {
            return Err("invalid certificate signature".into());
        }
        if now < self.valid_after || now >= self.valid_before {
            return Err("certificate is not valid at this time".into());
        }
        let any = self.principals.is_empty() && cert_type == HOST_CERT;
        if !any && !self.principals.iter().any(|p| p == principal) {
            return Err(format!("principal {} is not listed in the certificate", principal));
        }
        if let Some((name, _)) = self.critical_options.iter().find(|(n, _)| n != "force-command" && n != "source-address") {
            return Err(format!("unsupported critical option {}", name));
        }
        Ok(())
    }

    /// Issues a certificate for `key` signed by `ca` (like `ssh-keygen -s`).
    #[allow(clippy::too_many_arguments)]
    pub fn sign(key: &SshPublicKey, ca: &SshPrivateKey, cert_type: u32, key_id: &str, principals: &[&str], valid_after: u64, valid_before: u64, serial: u64) -> SshResult<SshCertificate> {
        let mut w = Writer::new();
        w.str(&format!("{}{}", key.key_type(), SUFFIX));
        w.string(&cameleer_tls::crypto::rng::random_bytes(32));
        // key fields = public key blob without its type string
        let kb = key.to_blob();
        let mut kr = Reader::new(&kb);
        kr.str()?;
        w.raw(kr.remaining());
        w.u64(serial).u32(cert_type).str(key_id);
        let mut p = Writer::new();
        for x in principals {
            p.str(x);
        }
        w.string(&p.finish());
        w.u64(valid_after).u64(valid_before);
        w.string(&[]);
        let ext = if cert_type == USER_CERT {
            let mut e = Writer::new();
            for name in ["permit-X11-forwarding", "permit-agent-forwarding", "permit-port-forwarding", "permit-pty", "permit-user-rc"] {
                e.str(name).string(&[]);
            }
            e.finish()
        } else {
            Vec::new()
        };
        w.string(&ext);
        w.string(&[]);
        w.string(&ca.public_key().to_blob());
        let tbs = w.finish();
        let alg = ca.public_key().signature_algorithms()[0];
        let sig = ca.sign(alg, &tbs)?;
        let mut full = tbs;
        let mut sw = Writer::new();
        sw.string(&sig);
        full.extend(sw.finish());
        SshCertificate::from_blob(&full)
    }
}

/// Seconds since the epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_parse_validate() {
        let ca = SshPrivateKey::generate_ed25519();
        let user = SshPrivateKey::generate_ecdsa(cameleer_tls::crypto::ec::EcCurve::P256);
        let now = now_secs();
        let c = SshCertificate::sign(&user.public_key(), &ca, USER_CERT, "id-1", &["alice", "bob"], now - 10, now + 3600, 7).unwrap();
        assert_eq!(c.cert_type_name(), "ecdsa-sha2-nistp256-cert-v01@openssh.com");
        assert_eq!(c.key, user.public_key());
        assert_eq!((c.serial, c.key_id.as_str()), (7, "id-1"));
        let line = c.to_openssh("comment");
        let back = SshCertificate::from_openssh(&line).unwrap();
        assert_eq!(back, c);
        assert!(c.validate(USER_CERT, "alice", now).is_ok());
        assert!(c.validate(USER_CERT, "carol", now).is_err());
        assert!(c.validate(HOST_CERT, "alice", now).is_err());
        assert!(c.validate(USER_CERT, "alice", now + 7200).is_err());
        let mut tampered = c.blob.clone();
        let i = tampered.len() / 2;
        tampered[i] ^= 1;
        assert!(SshCertificate::from_blob(&tampered).map(|t| !t.signature_valid()).unwrap_or(true));
        assert_eq!(plain_algorithm("rsa-sha2-512-cert-v01@openssh.com"), Some("rsa-sha2-512"));
        assert!(!is_cert_algorithm("ssh-ed25519"));
    }
}
