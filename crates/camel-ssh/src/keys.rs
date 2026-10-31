//! SSH public and private keys: wire blobs (RFC 4253 6.6, RFC 5656, RFC 8709),
//! signatures (`ssh-ed25519`, `ecdsa-sha2-nistp256/384`, `rsa-sha2-256/512`),
//! `authorized_keys` style lines, SHA-256 fingerprints and private keys in the
//! OpenSSH format (`-----BEGIN OPENSSH PRIVATE KEY-----`, unencrypted) and PEM
//! (PKCS#1 `RSA PRIVATE KEY`, SEC1 `EC PRIVATE KEY`, unencrypted PKCS#8).

use camel_tls::asn1::Der;
use camel_tls::crypto::ec::EcCurve;
use camel_tls::crypto::hash::{Digest, HashAlg, Sha256};
use camel_tls::crypto::rsa::{RsaPrivateKey, RsaPublicKey};
use camel_tls::pem::{base64_decode, base64_encode, parse_pem};

use crate::crypto::ed25519;
use crate::error::{SshError, SshResult};
use crate::wire::{Reader, Writer};

/// Minimum accepted RSA modulus size.
pub const MIN_RSA_BITS: usize = 1024;

fn key_err(m: impl Into<String>) -> SshError {
    SshError::Key(m.into())
}

fn curve_name(c: EcCurve) -> &'static str {
    match c {
        EcCurve::P256 => "nistp256",
        EcCurve::P384 => "nistp384",
    }
}

fn curve_from_name(n: &str) -> Option<EcCurve> {
    match n {
        "nistp256" => Some(EcCurve::P256),
        "nistp384" => Some(EcCurve::P384),
        _ => None,
    }
}

fn pad_left(v: &[u8], len: usize) -> Option<Vec<u8>> {
    if v.len() > len {
        return None;
    }
    let mut out = vec![0u8; len - v.len()];
    out.extend_from_slice(v);
    Some(out)
}

/// The hash used by an RSA signature algorithm name.
fn rsa_hash(alg: &str) -> Option<HashAlg> {
    match alg {
        "rsa-sha2-256" => Some(HashAlg::Sha256),
        "rsa-sha2-512" => Some(HashAlg::Sha512),
        _ => None,
    }
}

/// An SSH public key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SshPublicKey {
    Ed25519([u8; 32]),
    Ecdsa { curve: EcCurve, point: Vec<u8> },
    /// RSA public exponent and modulus (big-endian, no leading zeros).
    Rsa { e: Vec<u8>, n: Vec<u8> },
}

impl SshPublicKey {
    /// The key type name (`ssh-ed25519`, `ecdsa-sha2-nistp256`, `ssh-rsa`...).
    pub fn key_type(&self) -> &'static str {
        match self {
            SshPublicKey::Ed25519(_) => "ssh-ed25519",
            SshPublicKey::Ecdsa { curve: EcCurve::P256, .. } => "ecdsa-sha2-nistp256",
            SshPublicKey::Ecdsa { curve: EcCurve::P384, .. } => "ecdsa-sha2-nistp384",
            SshPublicKey::Rsa { .. } => "ssh-rsa",
        }
    }

    /// Signature algorithms usable with this key, most preferred first.
    pub fn signature_algorithms(&self) -> &'static [&'static str] {
        match self {
            SshPublicKey::Rsa { .. } => &["rsa-sha2-512", "rsa-sha2-256"],
            other => match other.key_type() {
                "ssh-ed25519" => &["ssh-ed25519"],
                "ecdsa-sha2-nistp256" => &["ecdsa-sha2-nistp256"],
                _ => &["ecdsa-sha2-nistp384"],
            },
        }
    }

    /// Whether `alg` is a signature algorithm for this key type.
    pub fn supports_algorithm(&self, alg: &str) -> bool {
        self.signature_algorithms().contains(&alg)
    }

    /// The public key blob (`string type || key data`).
    pub fn to_blob(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.str(self.key_type());
        match self {
            SshPublicKey::Ed25519(k) => {
                w.string(k);
            }
            SshPublicKey::Ecdsa { curve, point } => {
                w.str(curve_name(*curve)).string(point);
            }
            SshPublicKey::Rsa { e, n } => {
                w.mpint(e).mpint(n);
            }
        }
        w.finish()
    }

    /// Parses a public key blob.
    pub fn from_blob(blob: &[u8]) -> SshResult<SshPublicKey> {
        let mut r = Reader::new(blob);
        let k = Self::read_body(&r.str()?, &mut r)?;
        if !r.is_empty() {
            return Err(key_err("trailing data after public key"));
        }
        Ok(k)
    }

    fn read_body(t: &str, r: &mut Reader) -> SshResult<SshPublicKey> {
        match t {
            "ssh-ed25519" => {
                let k = r.string()?;
                let k: [u8; 32] = k.try_into().map_err(|_| key_err("bad ed25519 key length"))?;
                Ok(SshPublicKey::Ed25519(k))
            }
            "ecdsa-sha2-nistp256" | "ecdsa-sha2-nistp384" => {
                let curve = curve_from_name(&r.str()?).ok_or_else(|| key_err("unsupported ECDSA curve"))?;
                if t != format!("ecdsa-sha2-{}", curve_name(curve)) {
                    return Err(key_err("ECDSA curve mismatch"));
                }
                let point = r.string()?.to_vec();
                if !curve.validate_point(&point) {
                    return Err(key_err("invalid ECDSA public point"));
                }
                Ok(SshPublicKey::Ecdsa { curve, point })
            }
            "ssh-rsa" => {
                let e = r.mpint()?.to_vec();
                let n = r.mpint()?.to_vec();
                let pk = RsaPublicKey::new(&n, &e).ok_or_else(|| key_err("invalid RSA public key"))?;
                if pk.modulus_bits() < MIN_RSA_BITS {
                    return Err(key_err(format!("RSA key too small ({} bits)", pk.modulus_bits())));
                }
                Ok(SshPublicKey::Rsa { e, n })
            }
            other => Err(key_err(format!("unsupported key type {}", other))),
        }
    }

    /// Verifies a signature blob (`string alg || string sig`) made with algorithm `alg` over `data`.
    pub fn verify(&self, alg: &str, data: &[u8], sig_blob: &[u8]) -> bool {
        let mut r = Reader::new(sig_blob);
        let (name, sig) = match (r.str(), r.string()) {
            (Ok(n), Ok(s)) => (n, s),
            _ => return false,
        };
        if name != alg || !r.is_empty() || !self.supports_algorithm(alg) {
            return false;
        }
        match self {
            SshPublicKey::Ed25519(k) => ed25519::verify(k, data, sig),
            SshPublicKey::Ecdsa { curve, point } => {
                let mut sr = Reader::new(sig);
                let (rr, ss) = match (sr.mpint(), sr.mpint()) {
                    (Ok(a), Ok(b)) => (a, b),
                    _ => return false,
                };
                if !sr.is_empty() {
                    return false;
                }
                let len = curve.byte_len();
                let (rr, ss) = match (pad_left(rr, len), pad_left(ss, len)) {
                    (Some(a), Some(b)) => (a, b),
                    _ => return false,
                };
                curve.verify(point, &curve.hash().hash(data), &rr, &ss)
            }
            SshPublicKey::Rsa { e, n } => {
                let (hash, pk) = match (rsa_hash(alg), RsaPublicKey::new(n, e)) {
                    (Some(h), Some(pk)) => (h, pk),
                    _ => return false,
                };
                // some implementations strip leading zero bytes of the signature
                let sig = match pad_left(sig, pk.size()) {
                    Some(s) => s,
                    None => return false,
                };
                pk.verify_pkcs1(hash, &hash.hash(data), &sig)
            }
        }
    }

    /// `SHA256:<base64>` fingerprint as printed by `ssh-keygen -l`.
    pub fn fingerprint(&self) -> String {
        format!("SHA256:{}", base64_encode(&Sha256::digest(&self.to_blob())).trim_end_matches('='))
    }

    /// One-line OpenSSH representation (`type base64 [comment]`).
    pub fn to_openssh(&self, comment: &str) -> String {
        let s = format!("{} {}", self.key_type(), base64_encode(&self.to_blob()));
        if comment.is_empty() {
            s
        } else {
            format!("{} {}", s, comment)
        }
    }

    /// Parses `type base64 [comment]` (an `authorized_keys` / `.pub` line without options).
    pub fn from_openssh(line: &str) -> SshResult<SshPublicKey> {
        let mut it = line.split_whitespace();
        let t = it.next().ok_or_else(|| key_err("empty public key line"))?;
        let b = it.next().ok_or_else(|| key_err("missing base64 key data"))?;
        let blob = base64_decode(b).ok_or_else(|| key_err("bad base64 in public key"))?;
        let k = SshPublicKey::from_blob(&blob)?;
        if k.key_type() != t {
            return Err(key_err("key type mismatch"));
        }
        Ok(k)
    }
}

/// Parses an `authorized_keys` file (options before the key type are skipped).
pub fn parse_authorized_keys(text: &str) -> Vec<SshPublicKey> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        for i in 0..words.len().saturating_sub(1) {
            if let Ok(k) = SshPublicKey::from_openssh(&words[i..].join(" ")) {
                out.push(k);
                break;
            }
        }
    }
    out
}

/// An SSH private key.
#[derive(Clone)]
pub enum SshPrivateKey {
    Ed25519 { seed: [u8; 32], public: [u8; 32] },
    Ecdsa { curve: EcCurve, d: Vec<u8>, point: Vec<u8> },
    Rsa { key: RsaPrivateKey, e: Vec<u8>, n: Vec<u8>, extra: Option<RsaExtra> },
}

/// Optional CRT values of an RSA key (kept to rewrite OpenSSH files).
#[derive(Clone)]
pub struct RsaExtra {
    pub d: Vec<u8>,
    pub iqmp: Vec<u8>,
    pub p: Vec<u8>,
    pub q: Vec<u8>,
}

impl std::fmt::Debug for SshPrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SshPrivateKey({})", self.public_key().key_type())
    }
}

fn strip_zeros(v: &[u8]) -> Vec<u8> {
    let i = v.iter().position(|&b| b != 0).unwrap_or(v.len());
    v[i..].to_vec()
}

impl SshPrivateKey {
    /// Generates a new Ed25519 key.
    pub fn generate_ed25519() -> SshPrivateKey {
        let seed = ed25519::generate_seed();
        SshPrivateKey::Ed25519 { seed, public: ed25519::public_key(&seed) }
    }

    /// Generates a new ECDSA key on P-256 or P-384.
    pub fn generate_ecdsa(curve: EcCurve) -> SshPrivateKey {
        let d = curve.generate_private();
        let point = curve.public_key(&d).expect("valid scalar");
        SshPrivateKey::Ecdsa { curve, d, point }
    }

    /// Builds an RSA key from big-endian n, e, d.
    pub fn rsa(n: &[u8], e: &[u8], d: &[u8]) -> SshResult<SshPrivateKey> {
        let key = RsaPrivateKey::new(n, e, d).ok_or_else(|| key_err("invalid RSA private key"))?;
        if key.public.modulus_bits() < MIN_RSA_BITS {
            return Err(key_err("RSA key too small"));
        }
        Ok(SshPrivateKey::Rsa { key, e: strip_zeros(e), n: strip_zeros(n), extra: None })
    }

    /// The matching public key.
    pub fn public_key(&self) -> SshPublicKey {
        match self {
            SshPrivateKey::Ed25519 { public, .. } => SshPublicKey::Ed25519(*public),
            SshPrivateKey::Ecdsa { curve, point, .. } => SshPublicKey::Ecdsa { curve: *curve, point: point.clone() },
            SshPrivateKey::Rsa { e, n, .. } => SshPublicKey::Rsa { e: e.clone(), n: n.clone() },
        }
    }

    /// Signs `data` with signature algorithm `alg`; returns the signature blob.
    pub fn sign(&self, alg: &str, data: &[u8]) -> SshResult<Vec<u8>> {
        if !self.public_key().supports_algorithm(alg) {
            return Err(key_err(format!("algorithm {} does not match key type {}", alg, self.public_key().key_type())));
        }
        let sig = match self {
            SshPrivateKey::Ed25519 { seed, .. } => ed25519::sign(seed, data).to_vec(),
            SshPrivateKey::Ecdsa { curve, d, .. } => {
                let h = curve.hash();
                let (r, s) = curve.sign(d, &h.hash(data), h).ok_or_else(|| key_err("ECDSA signing failed"))?;
                Writer::new().mpint(&r).mpint(&s).finish()
            }
            SshPrivateKey::Rsa { key, .. } => {
                let h = rsa_hash(alg).ok_or_else(|| key_err("unsupported RSA signature algorithm"))?;
                key.sign_pkcs1(h, &h.hash(data)).ok_or_else(|| key_err("RSA signing failed"))?
            }
        };
        Ok(Writer::new().str(alg).string(&sig).finish())
    }

    /// Parses a private key file: OpenSSH format or PEM (PKCS#1, SEC1, PKCS#8).
    pub fn from_text(text: &str) -> SshResult<SshPrivateKey> {
        let blocks = parse_pem(text.as_bytes()).map_err(key_err)?;
        for b in blocks {
            match b.label.as_str() {
                "OPENSSH PRIVATE KEY" => return SshPrivateKey::from_openssh_bytes(&b.der),
                "RSA PRIVATE KEY" => return Self::from_pkcs1_rsa(&b.der),
                "PRIVATE KEY" => {
                    if let Some(k) = Self::try_pkcs8_rsa(&b.der) {
                        return k;
                    }
                    return Self::from_camel_tls(text);
                }
                "EC PRIVATE KEY" => return Self::from_camel_tls(text),
                "ENCRYPTED PRIVATE KEY" => return Err(key_err("encrypted private keys are not supported")),
                _ => continue,
            }
        }
        Err(key_err("no private key found (expected OPENSSH PRIVATE KEY or PEM)"))
    }

    fn from_camel_tls(text: &str) -> SshResult<SshPrivateKey> {
        if text.contains("Proc-Type: 4,ENCRYPTED") {
            return Err(key_err("encrypted PEM private keys are not supported"));
        }
        match camel_tls::PrivateKey::from_pem(text.as_bytes()).map_err(key_err)? {
            camel_tls::PrivateKey::Ec { curve, d, public } => Ok(SshPrivateKey::Ecdsa { curve, d, point: public }),
            camel_tls::PrivateKey::Ed25519 { seed, public } => Ok(SshPrivateKey::Ed25519 { seed, public }),
            camel_tls::PrivateKey::Rsa(_) => Err(key_err("unexpected RSA key")),
        }
    }

    fn from_pkcs1_rsa(der: &[u8]) -> SshResult<SshPrivateKey> {
        let e = |x: camel_tls::asn1::DerError| key_err(x.to_string());
        let mut s = Der::new(der).sequence().map_err(e)?;
        if s.small_uint().map_err(e)? != 0 {
            return Err(key_err("multi-prime RSA keys are not supported"));
        }
        let n = s.uint().map_err(e)?.to_vec();
        let pe = s.uint().map_err(e)?.to_vec();
        let d = s.uint().map_err(e)?.to_vec();
        let p = s.uint().map_err(e)?.to_vec();
        let q = s.uint().map_err(e)?.to_vec();
        let _dp = s.uint().map_err(e)?;
        let _dq = s.uint().map_err(e)?;
        let iqmp = s.uint().map_err(e)?.to_vec();
        let mut k = SshPrivateKey::rsa(&n, &pe, &d)?;
        if let SshPrivateKey::Rsa { extra, .. } = &mut k {
            *extra = Some(RsaExtra { d, iqmp, p, q });
        }
        Ok(k)
    }

    fn try_pkcs8_rsa(der: &[u8]) -> Option<SshResult<SshPrivateKey>> {
        let mut s = Der::new(der).sequence().ok()?;
        s.small_uint().ok()?;
        let mut alg = s.sequence().ok()?;
        if alg.oid().ok()? != camel_tls::keys::OID_RSA_ENCRYPTION {
            return None;
        }
        let inner = s.octet_string().ok()?;
        Some(Self::from_pkcs1_rsa(inner))
    }

    /// Parses the binary `openssh-key-v1` structure (unencrypted keys only).
    pub fn from_openssh_bytes(data: &[u8]) -> SshResult<SshPrivateKey> {
        const MAGIC: &[u8] = b"openssh-key-v1\0";
        if !data.starts_with(MAGIC) {
            return Err(key_err("not an openssh-key-v1 key"));
        }
        let mut r = Reader::new(&data[MAGIC.len()..]);
        let cipher = r.str()?;
        let kdf = r.str()?;
        let _kdf_opts = r.string()?;
        if cipher != "none" || kdf != "none" {
            return Err(key_err(format!(
                "passphrase protected OpenSSH keys ({} / {}) are not supported; remove the passphrase with ssh-keygen -p",
                cipher, kdf
            )));
        }
        if r.u32()? != 1 {
            return Err(key_err("only single-key OpenSSH files are supported"));
        }
        let pub_blob = r.string()?.to_vec();
        let priv_list = r.string()?;
        let mut p = Reader::new(priv_list);
        if p.u32()? != p.u32()? {
            return Err(key_err("corrupt OpenSSH key (check bytes differ)"));
        }
        let t = p.str()?;
        let key = match t.as_str() {
            "ssh-ed25519" => {
                let pk = p.string()?;
                let sk = p.string()?;
                if pk.len() != 32 || sk.len() != 64 || &sk[32..] != pk {
                    return Err(key_err("corrupt ed25519 key"));
                }
                let seed: [u8; 32] = sk[..32].try_into().unwrap();
                let public = ed25519::public_key(&seed);
                if public[..] != pk[..] {
                    return Err(key_err("ed25519 public key does not match private key"));
                }
                SshPrivateKey::Ed25519 { seed, public }
            }
            "ecdsa-sha2-nistp256" | "ecdsa-sha2-nistp384" => {
                let curve = curve_from_name(&p.str()?).ok_or_else(|| key_err("unsupported curve"))?;
                let point = p.string()?.to_vec();
                let d = pad_left(p.mpint()?, curve.byte_len()).ok_or_else(|| key_err("bad ECDSA scalar"))?;
                if curve.public_key(&d).as_deref() != Some(&point[..]) {
                    return Err(key_err("ECDSA public key does not match private key"));
                }
                SshPrivateKey::Ecdsa { curve, d, point }
            }
            "ssh-rsa" => {
                let n = p.mpint()?.to_vec();
                let e = p.mpint()?.to_vec();
                let d = p.mpint()?.to_vec();
                let iqmp = p.mpint()?.to_vec();
                let pp = p.mpint()?.to_vec();
                let q = p.mpint()?.to_vec();
                let mut k = SshPrivateKey::rsa(&n, &e, &d)?;
                if let SshPrivateKey::Rsa { extra, .. } = &mut k {
                    *extra = Some(RsaExtra { d, iqmp, p: pp, q });
                }
                k
            }
            other => return Err(key_err(format!("unsupported private key type {}", other))),
        };
        if key.public_key().to_blob() != pub_blob {
            return Err(key_err("public key section does not match private key"));
        }
        Ok(key)
    }

    /// Serialises as an unencrypted `-----BEGIN OPENSSH PRIVATE KEY-----` file.
    pub fn to_openssh_pem(&self, comment: &str) -> SshResult<String> {
        let mut pk = Writer::new();
        let check = u32::from_be_bytes(camel_tls::crypto::rng::random_bytes(4).try_into().unwrap());
        pk.u32(check).u32(check);
        pk.str(self.public_key().key_type());
        match self {
            SshPrivateKey::Ed25519 { seed, public } => {
                let mut sk = seed.to_vec();
                sk.extend_from_slice(public);
                pk.string(public).string(&sk);
            }
            SshPrivateKey::Ecdsa { curve, d, point } => {
                pk.str(curve_name(*curve)).string(point).mpint(d);
            }
            SshPrivateKey::Rsa { e, n, extra, .. } => {
                let x = extra.as_ref().ok_or_else(|| key_err("RSA key without CRT parameters cannot be exported"))?;
                pk.mpint(n).mpint(e).mpint(&x.d).mpint(&x.iqmp).mpint(&x.p).mpint(&x.q);
            }
        }
        pk.str(comment);
        let mut i = 1u8;
        while !pk.buf.len().is_multiple_of(8) {
            pk.u8(i);
            i += 1;
        }
        let mut w = Writer::new();
        w.raw(b"openssh-key-v1\0").str("none").str("none").string(b"").u32(1);
        w.string(&self.public_key().to_blob()).string(&pk.buf);
        let b64 = base64_encode(&w.buf);
        let mut out = String::from("-----BEGIN OPENSSH PRIVATE KEY-----\n");
        for chunk in b64.as_bytes().chunks(70) {
            out.push_str(std::str::from_utf8(chunk).unwrap());
            out.push('\n');
        }
        out.push_str("-----END OPENSSH PRIVATE KEY-----\n");
        Ok(out)
    }
}

/// Host key algorithm names a private key can serve.
pub fn host_key_algorithms(k: &SshPrivateKey) -> Vec<&'static str> {
    k.public_key().signature_algorithms().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ed25519_and_ecdsa_roundtrip() {
        for k in [SshPrivateKey::generate_ed25519(), SshPrivateKey::generate_ecdsa(EcCurve::P256), SshPrivateKey::generate_ecdsa(EcCurve::P384)] {
            let pubk = k.public_key();
            assert_eq!(SshPublicKey::from_blob(&pubk.to_blob()).unwrap(), pubk);
            let alg = pubk.signature_algorithms()[0];
            let sig = k.sign(alg, b"data").unwrap();
            assert!(pubk.verify(alg, b"data", &sig));
            assert!(!pubk.verify(alg, b"datb", &sig));
            assert!(!pubk.verify("rsa-sha2-256", b"data", &sig));
            let pem = k.to_openssh_pem("me@host").unwrap();
            let back = SshPrivateKey::from_text(&pem).unwrap();
            assert_eq!(back.public_key(), pubk);
            let line = pubk.to_openssh("c");
            assert_eq!(SshPublicKey::from_openssh(&line).unwrap(), pubk);
            assert_eq!(parse_authorized_keys(&format!("# x\nfrom=\"1.2.3.4\",no-pty {}\n", line)), vec![pubk.clone()]);
            assert!(pubk.fingerprint().starts_with("SHA256:"));
        }
    }

    #[test]
    fn rejects_encrypted_and_garbage() {
        assert!(SshPrivateKey::from_text("nope").is_err());
        let mut w = Writer::new();
        w.raw(b"openssh-key-v1\0").str("aes256-ctr").str("bcrypt").string(b"x").u32(1);
        assert!(SshPrivateKey::from_openssh_bytes(&w.buf).unwrap_err().to_string().contains("passphrase"));
    }
}
