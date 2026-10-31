//! OpenPGP keys: v4 public/secret key packets, transferable keys (keyrings), S2K secret key
//! protection, v4 signatures (create/verify), public key encryption of session keys (RSA,
//! ECDH on Curve25519 / NIST P-256 / P-384) and key generation (Ed25519 + Cv25519).

use camel_core::error::Result;
use camel_tls::crypto::bigint::{from_be_bytes, lt, to_be_bytes, MontCtx};
use camel_tls::crypto::ec::EcCurve;
use camel_tls::crypto::ed25519;
use camel_tls::crypto::hash::{Digest, HashAlg, Sha1};
use camel_tls::crypto::rng;
use camel_tls::crypto::rsa::{RsaPrivateKey, RsaPublicKey};
use camel_tls::crypto::x25519;

use super::packet::*;
use super::pgp_err;

pub const ALG_RSA: u8 = 1;
pub const ALG_RSA_E: u8 = 2;
pub const ALG_RSA_S: u8 = 3;
pub const ALG_ECDH: u8 = 18;
pub const ALG_ECDSA: u8 = 19;
pub const ALG_EDDSA: u8 = 22;

pub const HASH_SHA1: u8 = 2;
pub const HASH_SHA256: u8 = 8;
pub const HASH_SHA384: u8 = 9;
pub const HASH_SHA512: u8 = 10;

pub fn hash_alg(id: u8) -> Result<HashAlg> {
    Ok(match id {
        HASH_SHA1 => HashAlg::Sha1,
        HASH_SHA256 => HashAlg::Sha256,
        HASH_SHA384 => HashAlg::Sha384,
        HASH_SHA512 => HashAlg::Sha512,
        other => return Err(pgp_err(format!("Unsupported hash algorithm {}", other))),
    })
}

/// Incremental hash selected at runtime.
#[derive(Clone)]
pub enum Hasher {
    Sha1(Sha1),
    Sha256(camel_tls::crypto::hash::Sha256),
    Sha384(camel_tls::crypto::hash::Sha384),
    Sha512(camel_tls::crypto::hash::Sha512),
}

impl Hasher {
    pub fn new(h: HashAlg) -> Hasher {
        match h {
            HashAlg::Sha1 => Hasher::Sha1(Sha1::new()),
            HashAlg::Sha256 => Hasher::Sha256(Digest::new()),
            HashAlg::Sha384 => Hasher::Sha384(Digest::new()),
            HashAlg::Sha512 => Hasher::Sha512(Digest::new()),
        }
    }
    pub fn update(&mut self, d: &[u8]) {
        match self {
            Hasher::Sha1(h) => h.update(d),
            Hasher::Sha256(h) => h.update(d),
            Hasher::Sha384(h) => h.update(d),
            Hasher::Sha512(h) => h.update(d),
        }
    }
    pub fn finish(self) -> Vec<u8> {
        match self {
            Hasher::Sha1(h) => h.finish(),
            Hasher::Sha256(h) => h.finish(),
            Hasher::Sha384(h) => h.finish(),
            Hasher::Sha512(h) => h.finish(),
        }
    }
}

/// Elliptic curves known to the implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    Ed25519,
    Cv25519,
    P256,
    P384,
}

const OID_ED25519: &[u8] = &[0x2B, 0x06, 0x01, 0x04, 0x01, 0xDA, 0x47, 0x0F, 0x01];
const OID_CV25519: &[u8] = &[0x2B, 0x06, 0x01, 0x04, 0x01, 0x97, 0x55, 0x01, 0x05, 0x01];
const OID_P256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
const OID_P384: &[u8] = &[0x2B, 0x81, 0x04, 0x00, 0x22];

impl Curve {
    pub fn oid(self) -> &'static [u8] {
        match self {
            Curve::Ed25519 => OID_ED25519,
            Curve::Cv25519 => OID_CV25519,
            Curve::P256 => OID_P256,
            Curve::P384 => OID_P384,
        }
    }
    fn from_oid(o: &[u8]) -> Option<Curve> {
        [Curve::Ed25519, Curve::Cv25519, Curve::P256, Curve::P384].into_iter().find(|c| c.oid() == o)
    }
    fn nist(self) -> Option<EcCurve> {
        match self {
            Curve::P256 => Some(EcCurve::P256),
            Curve::P384 => Some(EcCurve::P384),
            _ => None,
        }
    }
}

/// Public key material.
#[derive(Clone, Debug, PartialEq)]
pub enum PublicParams {
    Rsa { n: Vec<u8>, e: Vec<u8> },
    Ecdsa { curve: Curve, point: Vec<u8> },
    EdDsa { point: Vec<u8> },
    Ecdh { curve: Curve, point: Vec<u8>, hash: u8, sym: u8 },
    Unsupported,
}

/// Secret key material (decrypted).
#[derive(Clone, Debug)]
pub enum SecretParams {
    Rsa { d: Vec<u8>, p: Vec<u8>, q: Vec<u8>, u: Vec<u8> },
    /// ECDSA/EdDSA/ECDH secret MPI.
    Scalar(Vec<u8>),
}

#[derive(Clone, Debug)]
enum SecretState {
    Plain(SecretParams),
    /// Encrypted secret part (everything after the public key fields).
    Protected(Vec<u8>),
    /// GNU dummy / stub key.
    Unavailable,
}

/// A v4 (sub)key.
#[derive(Clone, Debug)]
pub struct Key {
    pub created: u32,
    pub algorithm: u8,
    pub public: PublicParams,
    /// The public key packet body (fingerprint input).
    pub public_body: Vec<u8>,
    pub fingerprint: Vec<u8>,
    pub key_id: [u8; 8],
    /// Key flags from the self-signature (subpacket 27).
    pub flags: Option<u8>,
    secret: Option<SecretState>,
}

impl Key {
    /// Parse a public key packet body (v4 only; returns the key and the number of bytes used).
    pub fn parse_public(body: &[u8]) -> Result<(Key, usize)> {
        if body.first() != Some(&4) {
            return Err(pgp_err(format!("Unsupported key version {}", body.first().copied().unwrap_or(0))));
        }
        if body.len() < 6 {
            return Err(pgp_err("Truncated key packet"));
        }
        let created = u32::from_be_bytes(body[1..5].try_into().unwrap());
        let algorithm = body[5];
        let mut p = 6;
        let read_oid = |p: &mut usize| -> Result<Option<Curve>> {
            let l = *body.get(*p).ok_or_else(|| pgp_err("Truncated OID"))? as usize;
            let o = body.get(*p + 1..*p + 1 + l).ok_or_else(|| pgp_err("Truncated OID"))?;
            *p += 1 + l;
            Ok(Curve::from_oid(o))
        };
        let public = match algorithm {
            ALG_RSA | ALG_RSA_E | ALG_RSA_S => {
                let n = read_mpi(body, &mut p)?.to_vec();
                let e = read_mpi(body, &mut p)?.to_vec();
                PublicParams::Rsa { n, e }
            }
            ALG_ECDSA | ALG_EDDSA => {
                let c = read_oid(&mut p)?;
                let point = read_mpi(body, &mut p)?.to_vec();
                match (algorithm, c) {
                    (ALG_EDDSA, Some(Curve::Ed25519)) => PublicParams::EdDsa { point },
                    (ALG_ECDSA, Some(c)) if c.nist().is_some() => PublicParams::Ecdsa { curve: c, point },
                    _ => PublicParams::Unsupported,
                }
            }
            ALG_ECDH => {
                let c = read_oid(&mut p)?;
                let point = read_mpi(body, &mut p)?.to_vec();
                let l = *body.get(p).ok_or_else(|| pgp_err("Truncated KDF params"))? as usize;
                let kdf = body.get(p + 1..p + 1 + l).ok_or_else(|| pgp_err("Truncated KDF params"))?;
                p += 1 + l;
                match c {
                    Some(curve) if curve != Curve::Ed25519 && kdf.len() >= 3 => PublicParams::Ecdh { curve, point, hash: kdf[1], sym: kdf[2] },
                    _ => PublicParams::Unsupported,
                }
            }
            _ => {
                // unknown algorithm (DSA, ElGamal, ...): we cannot know the field layout
                p = body.len();
                PublicParams::Unsupported
            }
        };
        let public_body = body[..p].to_vec();
        let mut h = Sha1::new();
        h.update(&[0x99]);
        h.update(&(public_body.len() as u16).to_be_bytes());
        h.update(&public_body);
        let fingerprint = h.finish();
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&fingerprint[12..20]);
        Ok((Key { created, algorithm, public, public_body, fingerprint, key_id, flags: None, secret: None }, p))
    }

    /// Parse a secret key packet body.
    pub fn parse_secret(body: &[u8]) -> Result<Key> {
        let (mut k, p) = Key::parse_public(body)?;
        let rest = &body[p..];
        let usage = *rest.first().ok_or_else(|| pgp_err("Truncated secret key"))?;
        k.secret = Some(match usage {
            0 => {
                let (sp, _) = parse_secret_mpis(k.algorithm, &rest[1..])?;
                SecretState::Plain(sp)
            }
            254 | 255 => {
                if rest.len() > 3 && rest[2] == 101 {
                    SecretState::Unavailable // gnu-dummy S2K
                } else {
                    SecretState::Protected(rest.to_vec())
                }
            }
            _ => SecretState::Protected(rest.to_vec()),
        });
        Ok(k)
    }

    pub fn has_secret(&self) -> bool {
        matches!(self.secret, Some(SecretState::Plain(_)) | Some(SecretState::Protected(_)))
    }

    pub fn is_protected(&self) -> bool {
        matches!(self.secret, Some(SecretState::Protected(_)))
    }

    pub fn key_id_hex(&self) -> String {
        self.key_id.iter().map(|b| format!("{:02X}", b)).collect()
    }

    pub fn fingerprint_hex(&self) -> String {
        self.fingerprint.iter().map(|b| format!("{:02X}", b)).collect()
    }

    pub fn can_encrypt(&self) -> bool {
        let alg_ok = matches!(self.public, PublicParams::Rsa { .. } | PublicParams::Ecdh { .. }) && self.algorithm != ALG_RSA_S;
        alg_ok && self.flags.map(|f| f & 0x0c != 0).unwrap_or(true)
    }

    pub fn can_sign(&self) -> bool {
        let alg_ok = matches!(self.public, PublicParams::Rsa { .. } | PublicParams::EdDsa { .. } | PublicParams::Ecdsa { .. }) && self.algorithm != ALG_RSA_E;
        alg_ok && self.flags.map(|f| f & 0x02 != 0).unwrap_or(true)
    }

    /// Decrypt the secret key material with a passphrase.
    pub fn unlock(&self, password: Option<&str>) -> Result<SecretParams> {
        match &self.secret {
            None => Err(pgp_err(format!("Key {} has no secret part", self.key_id_hex()))),
            Some(SecretState::Unavailable) => Err(pgp_err(format!("Secret key {} is not available (stub)", self.key_id_hex()))),
            Some(SecretState::Plain(s)) => Ok(s.clone()),
            Some(SecretState::Protected(rest)) => {
                let usage = rest[0];
                if usage != 254 && usage != 255 {
                    return Err(pgp_err(format!("Unsupported secret key protection {}", usage)));
                }
                let sym = *rest.get(1).ok_or_else(|| pgp_err("Truncated secret key"))?;
                let mut p = 2;
                let s2k = S2k::parse(rest, &mut p)?;
                let bs = sym_block_len(sym);
                let iv = rest.get(p..p + bs).ok_or_else(|| pgp_err("Truncated secret key IV"))?.to_vec();
                p += bs;
                let pw = password.ok_or_else(|| pgp_err(format!("A password is required for the secret key {}", self.key_id_hex())))?;
                let key = s2k.derive(pw.as_bytes(), sym_key_len(sym)?)?;
                let mut data = rest[p..].to_vec();
                Cipher::new(sym, &key)?.cfb_decrypt(&iv, &mut data);
                let ok = if usage == 254 {
                    data.len() >= 20 && Sha1::digest(&data[..data.len() - 20]) == data[data.len() - 20..]
                } else {
                    data.len() >= 2 && {
                        let sum: u32 = data[..data.len() - 2].iter().map(|b| *b as u32).sum();
                        (sum & 0xffff) as u16 == u16::from_be_bytes(data[data.len() - 2..].try_into().unwrap())
                    }
                };
                if !ok {
                    return Err(pgp_err("Cannot decrypt the secret key: wrong password"));
                }
                let (sp, _) = parse_secret_mpis(self.algorithm, &data)?;
                Ok(sp)
            }
        }
    }

    /// The secret key packet body (unprotected, or protected with AES-256 + iterated
    /// salted SHA-256 S2K when a password is given).
    pub fn secret_body(&self, secret: &SecretParams, password: Option<&str>) -> Vec<u8> {
        let mut out = self.public_body.clone();
        let mut mpis = Vec::new();
        match secret {
            SecretParams::Rsa { d, p, q, u } => {
                for m in [d, p, q, u] {
                    write_mpi(&mut mpis, m);
                }
            }
            SecretParams::Scalar(s) => write_mpi(&mut mpis, s),
        }
        match password {
            None => {
                out.push(0);
                let sum: u32 = mpis.iter().map(|b| *b as u32).sum();
                out.extend_from_slice(&mpis);
                out.extend_from_slice(&((sum & 0xffff) as u16).to_be_bytes());
            }
            Some(pw) => {
                let s2k = S2k { kind: 3, hash: HASH_SHA256, salt: rng::random_bytes(8), count: 0xC0 };
                let key = s2k.derive(pw.as_bytes(), 32).expect("supported S2K");
                let iv = rng::random_bytes(16);
                let mut data = mpis.clone();
                data.extend(Sha1::digest(&mpis));
                Cipher::new(SYM_AES256, &key).expect("aes").cfb_encrypt(&iv, &mut data);
                out.push(254);
                out.push(SYM_AES256);
                s2k.write(&mut out);
                out.extend_from_slice(&iv);
                out.extend_from_slice(&data);
            }
        }
        out
    }
}

fn parse_secret_mpis(alg: u8, data: &[u8]) -> Result<(SecretParams, usize)> {
    let mut p = 0;
    let sp = match alg {
        ALG_RSA | ALG_RSA_E | ALG_RSA_S => {
            let d = read_mpi(data, &mut p)?.to_vec();
            let pp = read_mpi(data, &mut p)?.to_vec();
            let q = read_mpi(data, &mut p)?.to_vec();
            let u = read_mpi(data, &mut p)?.to_vec();
            SecretParams::Rsa { d, p: pp, q, u }
        }
        ALG_ECDH | ALG_ECDSA | ALG_EDDSA => SecretParams::Scalar(read_mpi(data, &mut p)?.to_vec()),
        other => return Err(pgp_err(format!("Unsupported public key algorithm {}", other))),
    };
    Ok((sp, p))
}

/// String-to-key specifier.
#[derive(Clone, Debug)]
pub struct S2k {
    pub kind: u8,
    pub hash: u8,
    pub salt: Vec<u8>,
    pub count: u8,
}

impl S2k {
    pub fn parse(d: &[u8], p: &mut usize) -> Result<S2k> {
        let kind = *d.get(*p).ok_or_else(|| pgp_err("Truncated S2K"))?;
        let hash = *d.get(*p + 1).ok_or_else(|| pgp_err("Truncated S2K"))?;
        *p += 2;
        let mut s = S2k { kind, hash, salt: Vec::new(), count: 0 };
        match kind {
            0 => {}
            1 | 3 => {
                s.salt = d.get(*p..*p + 8).ok_or_else(|| pgp_err("Truncated S2K salt"))?.to_vec();
                *p += 8;
                if kind == 3 {
                    s.count = *d.get(*p).ok_or_else(|| pgp_err("Truncated S2K count"))?;
                    *p += 1;
                }
            }
            other => return Err(pgp_err(format!("Unsupported S2K type {}", other))),
        }
        Ok(s)
    }
    pub fn write(&self, out: &mut Vec<u8>) {
        out.push(self.kind);
        out.push(self.hash);
        if self.kind == 1 || self.kind == 3 {
            out.extend_from_slice(&self.salt);
        }
        if self.kind == 3 {
            out.push(self.count);
        }
    }
    pub fn derive(&self, pw: &[u8], len: usize) -> Result<Vec<u8>> {
        let h = hash_alg(self.hash)?;
        let mut out = Vec::new();
        let mut preload = 0;
        while out.len() < len {
            let mut hs = Hasher::new(h);
            hs.update(&vec![0u8; preload]);
            match self.kind {
                0 => hs.update(pw),
                1 => {
                    hs.update(&self.salt);
                    hs.update(pw);
                }
                _ => {
                    let count = ((16 + (self.count as usize & 15)) << ((self.count as usize >> 4) + 6)).max(self.salt.len() + pw.len());
                    let unit: Vec<u8> = [self.salt.as_slice(), pw].concat();
                    let mut left = count;
                    while left > 0 {
                        let n = left.min(unit.len());
                        hs.update(&unit[..n]);
                        left -= n;
                    }
                }
            }
            out.extend(hs.finish());
            preload += 1;
        }
        out.truncate(len);
        Ok(out)
    }
}

/// A transferable key: primary key, user IDs and subkeys.
#[derive(Clone, Debug)]
pub struct Cert {
    pub primary: Key,
    pub user_ids: Vec<String>,
    pub subkeys: Vec<Key>,
}

impl Cert {
    /// All keys (primary first).
    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        std::iter::once(&self.primary).chain(self.subkeys.iter())
    }
    /// Whether a user ID contains `id` (Camel matches user ID parts).
    pub fn matches_user_id(&self, id: &str) -> bool {
        self.user_ids.iter().any(|u| u.contains(id))
    }
}

/// Parse a keyring (armored or binary; public and/or secret keys).
pub fn parse_keyring(data: &[u8]) -> Result<Vec<Cert>> {
    let bin = to_binary(data)?;
    let packets = parse_packets(&bin)?;
    let mut certs: Vec<Cert> = Vec::new();
    // which key a signature applies to: None = primary
    let mut current_sub: Option<usize> = None;
    for pk in packets {
        match pk.tag {
            TAG_PUBLIC_KEY | TAG_SECRET_KEY => {
                let k = if pk.tag == TAG_SECRET_KEY { Key::parse_secret(&pk.body)? } else { Key::parse_public(&pk.body)?.0 };
                certs.push(Cert { primary: k, user_ids: Vec::new(), subkeys: Vec::new() });
                current_sub = None;
            }
            TAG_PUBLIC_SUBKEY | TAG_SECRET_SUBKEY => {
                let c = certs.last_mut().ok_or_else(|| pgp_err("Subkey without primary key"))?;
                let k = if pk.tag == TAG_SECRET_SUBKEY { Key::parse_secret(&pk.body) } else { Key::parse_public(&pk.body).map(|x| x.0) };
                match k {
                    Ok(k) => {
                        c.subkeys.push(k);
                        current_sub = Some(c.subkeys.len() - 1);
                    }
                    Err(_) => current_sub = None,
                }
            }
            TAG_USER_ID => {
                if let Some(c) = certs.last_mut() {
                    c.user_ids.push(String::from_utf8_lossy(&pk.body).to_string());
                }
                current_sub = None;
            }
            TAG_SIGNATURE => {
                if let (Some(c), Ok(sig)) = (certs.last_mut(), Signature::parse(&pk.body)) {
                    if let Some(f) = sig.key_flags {
                        match (sig.sig_type, current_sub) {
                            (0x18, Some(i)) => c.subkeys[i].flags = Some(f),
                            (0x10..=0x13 | 0x1f, None) => c.primary.flags = Some(c.primary.flags.unwrap_or(0) | f),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(certs)
}

// ------------------------------------------------------------------------- signatures

/// A parsed v4 signature.
#[derive(Clone, Debug)]
pub struct Signature {
    pub sig_type: u8,
    pub pk_alg: u8,
    pub hash_alg: u8,
    /// version .. end of hashed subpackets (hashed with the trailer).
    pub hashed_part: Vec<u8>,
    pub left16: [u8; 2],
    pub mpis: Vec<Vec<u8>>,
    pub issuer: Option<[u8; 8]>,
    pub issuer_fpr: Option<Vec<u8>>,
    pub created: Option<u32>,
    pub key_flags: Option<u8>,
}

fn parse_subpackets(d: &[u8], sig: &mut Signature) {
    let mut i = 0;
    while i < d.len() {
        let b0 = d[i] as usize;
        let (len, hl) = if b0 < 192 {
            (b0, 1)
        } else if b0 < 255 {
            match d.get(i + 1) {
                Some(b1) => (((b0 - 192) << 8) + *b1 as usize + 192, 2),
                None => return,
            }
        } else {
            match d.get(i + 1..i + 5) {
                Some(l) => (u32::from_be_bytes(l.try_into().unwrap()) as usize, 5),
                None => return,
            }
        };
        let body = match d.get(i + hl..i + hl + len) {
            Some(b) if !b.is_empty() => b,
            _ => return,
        };
        let t = body[0] & 0x7f;
        let v = &body[1..];
        match t {
            2 if v.len() == 4 => sig.created = Some(u32::from_be_bytes(v.try_into().unwrap())),
            16 if v.len() == 8 => sig.issuer = Some(v.try_into().unwrap()),
            27 if !v.is_empty() => sig.key_flags = Some(v[0]),
            33 if v.len() >= 21 => {
                sig.issuer_fpr = Some(v[1..].to_vec());
                if v[0] == 4 && v.len() == 21 && sig.issuer.is_none() {
                    sig.issuer = Some(v[13..21].try_into().unwrap());
                }
            }
            _ => {}
        }
        i += hl + len;
    }
}

impl Signature {
    pub fn parse(b: &[u8]) -> Result<Signature> {
        if b.first() != Some(&4) {
            return Err(pgp_err(format!("Unsupported signature version {}", b.first().copied().unwrap_or(0))));
        }
        if b.len() < 6 {
            return Err(pgp_err("Truncated signature"));
        }
        let hl = u16::from_be_bytes(b[4..6].try_into().unwrap()) as usize;
        let hashed = b.get(6..6 + hl).ok_or_else(|| pgp_err("Truncated signature"))?;
        let mut p = 6 + hl;
        let ul = u16::from_be_bytes(b.get(p..p + 2).ok_or_else(|| pgp_err("Truncated signature"))?.try_into().unwrap()) as usize;
        let unhashed = b.get(p + 2..p + 2 + ul).ok_or_else(|| pgp_err("Truncated signature"))?;
        p += 2 + ul;
        let left16: [u8; 2] = b.get(p..p + 2).ok_or_else(|| pgp_err("Truncated signature"))?.try_into().unwrap();
        p += 2;
        let mut mpis = Vec::new();
        while p < b.len() {
            mpis.push(read_mpi(b, &mut p)?.to_vec());
        }
        let mut sig = Signature {
            sig_type: b[1],
            pk_alg: b[2],
            hash_alg: b[3],
            hashed_part: b[..6 + hl].to_vec(),
            left16,
            mpis,
            issuer: None,
            issuer_fpr: None,
            created: None,
            key_flags: None,
        };
        parse_subpackets(hashed, &mut sig);
        parse_subpackets(unhashed, &mut sig);
        Ok(sig)
    }

    /// Finish the hash over the signed data with the signature trailer.
    pub fn digest(&self, mut h: Hasher) -> Vec<u8> {
        h.update(&self.hashed_part);
        h.update(&[4, 0xff]);
        h.update(&(self.hashed_part.len() as u32).to_be_bytes());
        h.finish()
    }

    /// Verify the signature for a finished digest with a public key.
    pub fn verify_digest(&self, key: &Key, digest: &[u8]) -> Result<bool> {
        if digest[..2] != self.left16 {
            return Ok(false);
        }
        let h = hash_alg(self.hash_alg)?;
        Ok(match &key.public {
            PublicParams::Rsa { n, e } => {
                let pk = RsaPublicKey::new(n, e).ok_or_else(|| pgp_err("Invalid RSA key"))?;
                let s = pad_left(self.mpis.first().ok_or_else(|| pgp_err("Missing signature MPI"))?, pk.size());
                pk.verify_pkcs1(h, digest, &s)
            }
            PublicParams::EdDsa { point } => {
                if self.mpis.len() != 2 || point.len() != 33 || point[0] != 0x40 {
                    return Ok(false);
                }
                let mut sig = pad_left(&self.mpis[0], 32);
                sig.extend(pad_left(&self.mpis[1], 32));
                ed25519::verify(&point[1..], digest, &sig)
            }
            PublicParams::Ecdsa { curve, point } => {
                let c = curve.nist().ok_or_else(|| pgp_err("Unsupported curve"))?;
                if self.mpis.len() != 2 {
                    return Ok(false);
                }
                let n = c.byte_len();
                c.verify(point, digest, &pad_left(&self.mpis[0], n), &pad_left(&self.mpis[1], n))
            }
            _ => return Err(pgp_err("Unsupported signature key algorithm")),
        })
    }
}

/// Build the hashed part of a new v4 signature.
pub fn signature_hashed_part(sig_type: u8, key: &Key, hash: u8, extra: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut sub = Vec::new();
    let mut add = |t: u8, v: &[u8]| {
        sub.push((v.len() + 1) as u8);
        sub.push(t);
        sub.extend_from_slice(v);
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as u32).unwrap_or(0);
    add(2, &now.max(key.created).to_be_bytes());
    let mut fpr = vec![4u8];
    fpr.extend_from_slice(&key.fingerprint);
    add(33, &fpr);
    for (t, v) in extra {
        add(*t, v);
    }
    let mut h = vec![4, sig_type, key.algorithm, hash];
    h.extend_from_slice(&(sub.len() as u16).to_be_bytes());
    h.extend(sub);
    h
}

/// Sign a digest computed over data + hashed part + trailer; returns the signature packet body.
pub fn make_signature(key: &Key, secret: &SecretParams, hash: u8, hashed_part: Vec<u8>, data_hasher: Hasher) -> Result<Vec<u8>> {
    let mut h = data_hasher;
    h.update(&hashed_part);
    h.update(&[4, 0xff]);
    h.update(&(hashed_part.len() as u32).to_be_bytes());
    let digest = h.finish();
    let ha = hash_alg(hash)?;
    let mut mpis = Vec::new();
    match (&key.public, secret) {
        (PublicParams::Rsa { n, e }, SecretParams::Rsa { d, .. }) => {
            let sk = RsaPrivateKey::new(n, e, d).ok_or_else(|| pgp_err("Invalid RSA secret key"))?;
            let s = sk.sign_pkcs1(ha, &digest).ok_or_else(|| pgp_err("RSA signing failed"))?;
            write_mpi(&mut mpis, &s);
        }
        (PublicParams::EdDsa { .. }, SecretParams::Scalar(seed)) => {
            let seed: [u8; 32] = pad_left(seed, 32).try_into().unwrap();
            let sig = ed25519::sign(&seed, &digest);
            write_mpi(&mut mpis, &sig[..32]);
            write_mpi(&mut mpis, &sig[32..]);
        }
        (PublicParams::Ecdsa { curve, .. }, SecretParams::Scalar(s)) => {
            let c = curve.nist().ok_or_else(|| pgp_err("Unsupported curve"))?;
            let (r, s) = c.sign(&pad_left(s, c.byte_len()), &digest, ha).ok_or_else(|| pgp_err("ECDSA signing failed"))?;
            write_mpi(&mut mpis, &r);
            write_mpi(&mut mpis, &s);
        }
        _ => return Err(pgp_err("Key cannot sign")),
    }
    let mut body = hashed_part;
    // unhashed: issuer key ID
    body.extend_from_slice(&10u16.to_be_bytes());
    body.push(9);
    body.push(16);
    body.extend_from_slice(&key.key_id);
    body.extend_from_slice(&digest[..2]);
    body.extend(mpis);
    Ok(body)
}

// ------------------------------------------------------------------------- RSA raw

fn rsa_raw(n: &[u8], exp: &[u8], input: &[u8], secret: bool) -> Result<Vec<u8>> {
    let nn = pad_left(n, n.len());
    let k = nn.len();
    let limbs = k.div_ceil(8);
    let m = from_be_bytes(&nn, limbs).ok_or_else(|| pgp_err("Invalid RSA modulus"))?;
    if m[0] & 1 == 0 {
        return Err(pgp_err("Invalid RSA modulus"));
    }
    let x = from_be_bytes(input, limbs).ok_or_else(|| pgp_err("RSA input too large"))?;
    if !lt(&x, &m) {
        return Err(pgp_err("RSA input out of range"));
    }
    let e = from_be_bytes(exp, limbs).ok_or_else(|| pgp_err("Invalid RSA exponent"))?;
    let ctx = MontCtx::new(m);
    Ok(to_be_bytes(&ctx.pow(&x, &e, secret), k))
}

// ------------------------------------------------------------------------- ECDH (RFC 6637)

fn aes_key_wrap(kek: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use camel_tls::crypto::aes::Aes;
    let aes = Aes::new(kek);
    let n = data.len() / 8;
    let mut a = [0xA6u8; 8];
    let mut r: Vec<[u8; 8]> = data.chunks(8).map(|c| c.try_into().unwrap()).collect();
    for j in 0..6 {
        for i in 0..n {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(&r[i]);
            aes.encrypt_block(&mut b);
            let t = (n * j + i + 1) as u64;
            a.copy_from_slice(&b[..8]);
            for (x, y) in a.iter_mut().zip(t.to_be_bytes()) {
                *x ^= y;
            }
            r[i].copy_from_slice(&b[8..]);
        }
    }
    let mut out = a.to_vec();
    for x in r {
        out.extend_from_slice(&x);
    }
    Ok(out)
}

fn aes_key_unwrap(kek: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use camel_tls::crypto::aes::Aes;
    if data.len() % 8 != 0 || data.len() < 24 {
        return Err(pgp_err("Invalid wrapped key length"));
    }
    let aes = Aes::new(kek);
    let n = data.len() / 8 - 1;
    let mut a: [u8; 8] = data[..8].try_into().unwrap();
    let mut r: Vec<[u8; 8]> = data[8..].chunks(8).map(|c| c.try_into().unwrap()).collect();
    for j in (0..6).rev() {
        for i in (0..n).rev() {
            let t = (n * j + i + 1) as u64;
            let mut b = [0u8; 16];
            for (k, y) in t.to_be_bytes().iter().enumerate() {
                b[k] = a[k] ^ y;
            }
            b[8..].copy_from_slice(&r[i]);
            aes.decrypt_block(&mut b);
            a.copy_from_slice(&b[..8]);
            r[i].copy_from_slice(&b[8..]);
        }
    }
    if a != [0xA6u8; 8] {
        return Err(pgp_err("Session key unwrap failed (wrong key?)"));
    }
    Ok(r.concat())
}

fn ecdh_kdf(curve: Curve, hash: u8, sym: u8, fingerprint: &[u8], z: &[u8]) -> Result<Vec<u8>> {
    let mut param = Vec::new();
    param.push(curve.oid().len() as u8);
    param.extend_from_slice(curve.oid());
    param.push(ALG_ECDH);
    param.extend_from_slice(&[3, 1, hash, sym]);
    param.extend_from_slice(b"Anonymous Sender    ");
    param.extend_from_slice(fingerprint);
    let mut h = Hasher::new(hash_alg(hash)?);
    h.update(&[0, 0, 0, 1]);
    h.update(z);
    h.update(&param);
    let mut k = h.finish();
    k.truncate(sym_key_len(sym)?);
    Ok(k)
}

/// Encrypt a session key for a public key; returns the PKESK packet body.
pub fn encrypt_session_key(key: &Key, sym: u8, session_key: &[u8]) -> Result<Vec<u8>> {
    let mut m = vec![sym];
    m.extend_from_slice(session_key);
    let sum: u32 = session_key.iter().map(|b| *b as u32).sum();
    m.extend_from_slice(&((sum & 0xffff) as u16).to_be_bytes());
    let mut body = vec![3];
    body.extend_from_slice(&key.key_id);
    body.push(key.algorithm);
    match &key.public {
        PublicParams::Rsa { n, e } => {
            let k = n.len();
            if m.len() + 11 > k {
                return Err(pgp_err("RSA key too small"));
            }
            let mut em = vec![0u8, 2];
            let ps_len = k - 3 - m.len();
            while em.len() < 2 + ps_len {
                let b = rng::random_bytes(1)[0];
                if b != 0 {
                    em.push(b);
                }
            }
            em.push(0);
            em.extend_from_slice(&m);
            let c = rsa_raw(n, e, &em, false)?;
            write_mpi(&mut body, &c);
        }
        PublicParams::Ecdh { curve, point, hash, sym: kek_alg } => {
            let (eph_pub, z) = match curve {
                Curve::Cv25519 => {
                    if point.len() != 33 || point[0] != 0x40 {
                        return Err(pgp_err("Invalid Cv25519 public key"));
                    }
                    let mut sk = [0u8; 32];
                    rng::fill(&mut sk);
                    let eph = x25519::x25519_base(&sk);
                    let peer: [u8; 32] = point[1..].try_into().unwrap();
                    let z = x25519::x25519(&sk, &peer);
                    let mut ep = vec![0x40];
                    ep.extend_from_slice(&eph);
                    (ep, z.to_vec())
                }
                c => {
                    let ec = c.nist().ok_or_else(|| pgp_err("Unsupported ECDH curve"))?;
                    let sk = ec.generate_private();
                    let ep = ec.public_key(&sk).ok_or_else(|| pgp_err("ECDH failed"))?;
                    let z = ec.ecdh(&sk, point).ok_or_else(|| pgp_err("Invalid ECDH public key"))?;
                    (ep, z)
                }
            };
            let kek = ecdh_kdf(*curve, *hash, *kek_alg, &key.fingerprint, &z)?;
            let pad = 8 - m.len() % 8;
            m.extend(std::iter::repeat_n(pad as u8, pad));
            let wrapped = aes_key_wrap(&kek, &m)?;
            write_mpi(&mut body, &eph_pub);
            body.push(wrapped.len() as u8);
            body.extend(wrapped);
        }
        _ => return Err(pgp_err("Key cannot encrypt")),
    }
    Ok(body)
}

/// Decrypt a PKESK body with a secret key; returns (symmetric algorithm, session key).
pub fn decrypt_session_key(key: &Key, secret: &SecretParams, pkesk: &[u8]) -> Result<(u8, Vec<u8>)> {
    let data = &pkesk[10..];
    let m = match (&key.public, secret) {
        (PublicParams::Rsa { n, .. }, SecretParams::Rsa { d, .. }) => {
            let mut p = 0;
            let c = read_mpi(data, &mut p)?;
            let em = rsa_raw(n, d, &pad_left(c, n.len()), true)?;
            if em[0] != 0 || em[1] != 2 {
                return Err(pgp_err("RSA session key decryption failed"));
            }
            let z = em[2..].iter().position(|b| *b == 0).ok_or_else(|| pgp_err("RSA session key decryption failed"))?;
            em[3 + z..].to_vec()
        }
        (PublicParams::Ecdh { curve, hash, sym, .. }, SecretParams::Scalar(s)) => {
            let mut p = 0;
            let eph = read_mpi(data, &mut p)?.to_vec();
            let l = *data.get(p).ok_or_else(|| pgp_err("Truncated ECDH session key"))? as usize;
            let wrapped = data.get(p + 1..p + 1 + l).ok_or_else(|| pgp_err("Truncated ECDH session key"))?;
            let z = match curve {
                Curve::Cv25519 => {
                    if eph.len() != 33 || eph[0] != 0x40 {
                        return Err(pgp_err("Invalid ephemeral key"));
                    }
                    // the secret is stored big-endian (MPI): reverse to the native little-endian scalar
                    let mut sk: Vec<u8> = pad_left(s, 32);
                    sk.reverse();
                    let sk: [u8; 32] = sk.try_into().unwrap();
                    x25519::x25519(&sk, &eph[1..].try_into().unwrap()).to_vec()
                }
                c => {
                    let ec = c.nist().ok_or_else(|| pgp_err("Unsupported ECDH curve"))?;
                    ec.ecdh(&pad_left(s, ec.byte_len()), &eph).ok_or_else(|| pgp_err("Invalid ephemeral key"))?
                }
            };
            let kek = ecdh_kdf(*curve, *hash, *sym, &key.fingerprint, &z)?;
            let mut m = aes_key_unwrap(&kek, wrapped)?;
            let pad = *m.last().ok_or_else(|| pgp_err("Empty session key"))? as usize;
            if pad == 0 || pad > 8 || pad > m.len() {
                return Err(pgp_err("Invalid session key padding"));
            }
            m.truncate(m.len() - pad);
            m
        }
        _ => return Err(pgp_err("Key cannot decrypt")),
    };
    if m.len() < 3 {
        return Err(pgp_err("Invalid session key"));
    }
    let sym = m[0];
    let sk = m[1..m.len() - 2].to_vec();
    let sum: u32 = sk.iter().map(|b| *b as u32).sum();
    if (sum & 0xffff) as u16 != u16::from_be_bytes(m[m.len() - 2..].try_into().unwrap()) {
        return Err(pgp_err("Session key checksum mismatch"));
    }
    if sk.len() != sym_key_len(sym)? {
        return Err(pgp_err("Session key length mismatch"));
    }
    Ok((sym, sk))
}

// ------------------------------------------------------------------------- generation

/// A generated key pair: armored public and secret keyrings.
pub struct GeneratedKey {
    pub public_keyring: String,
    pub secret_keyring: String,
    pub fingerprint: String,
}

/// Generate an Ed25519 (sign/certify) primary key with a Cv25519 (encryption) subkey,
/// self-signed for `user_id`; the secret keys are protected with `password` when given.
pub fn generate_ed25519(user_id: &str, password: Option<&str>) -> Result<GeneratedKey> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as u32).unwrap_or(0) - 60;
    // primary
    let seed = ed25519::generate_seed();
    let pubk = ed25519::public_key(&seed);
    let mut body = vec![4];
    body.extend_from_slice(&now.to_be_bytes());
    body.push(ALG_EDDSA);
    body.push(OID_ED25519.len() as u8);
    body.extend_from_slice(OID_ED25519);
    let mut pt = vec![0x40];
    pt.extend_from_slice(&pubk);
    write_mpi(&mut body, &pt);
    let (primary, _) = Key::parse_public(&body)?;
    let primary_secret = SecretParams::Scalar(seed.to_vec());
    // subkey
    let mut sk = [0u8; 32];
    rng::fill(&mut sk);
    sk[0] &= 248;
    sk[31] &= 127;
    sk[31] |= 64;
    let xpub = x25519::x25519_base(&sk);
    let mut sbody = vec![4];
    sbody.extend_from_slice(&now.to_be_bytes());
    sbody.push(ALG_ECDH);
    sbody.push(OID_CV25519.len() as u8);
    sbody.extend_from_slice(OID_CV25519);
    let mut pt = vec![0x40];
    pt.extend_from_slice(&xpub);
    write_mpi(&mut sbody, &pt);
    sbody.extend_from_slice(&[3, 1, HASH_SHA256, SYM_AES128]);
    let (sub, _) = Key::parse_public(&sbody)?;
    let mut rev = sk.to_vec();
    rev.reverse();
    let sub_secret = SecretParams::Scalar(rev);

    let key_hash = |k: &Key, h: &mut Hasher| {
        h.update(&[0x99]);
        h.update(&(k.public_body.len() as u16).to_be_bytes());
        h.update(&k.public_body);
    };
    // user id certification
    let prefs: Vec<(u8, Vec<u8>)> = vec![(27, vec![0x03]), (11, vec![SYM_AES256, SYM_AES192, SYM_AES128]), (21, vec![HASH_SHA512, HASH_SHA256]), (22, vec![2, 1, 0]), (30, vec![1])];
    let hp = signature_hashed_part(0x13, &primary, HASH_SHA256, &prefs);
    let mut h = Hasher::new(HashAlg::Sha256);
    key_hash(&primary, &mut h);
    h.update(&[0xb4]);
    h.update(&(user_id.len() as u32).to_be_bytes());
    h.update(user_id.as_bytes());
    let uid_sig = make_signature(&primary, &primary_secret, HASH_SHA256, hp, h)?;
    // subkey binding
    let hp = signature_hashed_part(0x18, &primary, HASH_SHA256, &[(27, vec![0x0c])]);
    let mut h = Hasher::new(HashAlg::Sha256);
    key_hash(&primary, &mut h);
    key_hash(&sub, &mut h);
    let sub_sig = make_signature(&primary, &primary_secret, HASH_SHA256, hp, h)?;

    let mut public = Vec::new();
    write_packet(&mut public, TAG_PUBLIC_KEY, &primary.public_body);
    write_packet(&mut public, TAG_USER_ID, user_id.as_bytes());
    write_packet(&mut public, TAG_SIGNATURE, &uid_sig);
    write_packet(&mut public, TAG_PUBLIC_SUBKEY, &sub.public_body);
    write_packet(&mut public, TAG_SIGNATURE, &sub_sig);
    let mut secret = Vec::new();
    write_packet(&mut secret, TAG_SECRET_KEY, &primary.secret_body(&primary_secret, password));
    write_packet(&mut secret, TAG_USER_ID, user_id.as_bytes());
    write_packet(&mut secret, TAG_SIGNATURE, &uid_sig);
    write_packet(&mut secret, TAG_SECRET_SUBKEY, &sub.secret_body(&sub_secret, password));
    write_packet(&mut secret, TAG_SIGNATURE, &sub_sig);
    Ok(GeneratedKey { public_keyring: armor("PUBLIC KEY BLOCK", &public), secret_keyring: armor("PRIVATE KEY BLOCK", &secret), fingerprint: primary.fingerprint_hex() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_wrap_rfc3394() {
        // RFC 3394 4.1: wrap 128 bits of key data with a 128-bit KEK
        let kek: Vec<u8> = (0u8..16).collect();
        let data = camel_tls::util::unhex("00112233445566778899AABBCCDDEEFF");
        let w = aes_key_wrap(&kek, &data).unwrap();
        assert_eq!(camel_tls::util::hex(&w).to_uppercase(), "1FA68B0A8112B447AEF34BD8FB5A7B829D3E862371D2CFE5");
        assert_eq!(aes_key_unwrap(&kek, &w).unwrap(), data);
    }

    #[test]
    fn s2k_iterated_count() {
        let s = S2k { kind: 3, hash: HASH_SHA1, salt: vec![1; 8], count: 0x60 };
        // count 0x60 => 65536 bytes
        assert_eq!(s.derive(b"pw", 16).unwrap().len(), 16);
        let s0 = S2k { kind: 0, hash: HASH_SHA1, salt: vec![], count: 0 };
        assert_eq!(s0.derive(b"abc", 20).unwrap(), Sha1::digest(b"abc"));
        // a 32 byte key from SHA-1 uses a second, zero-preloaded hash
        let long = s0.derive(b"abc", 32).unwrap();
        let mut second = vec![0u8];
        second.extend_from_slice(b"abc");
        assert_eq!(long[20..], Sha1::digest(&second)[..12]);
    }

    #[test]
    fn generate_parse_unlock() {
        let g = generate_ed25519("Alice <alice@example.com>", Some("secret")).unwrap();
        let certs = parse_keyring(g.secret_keyring.as_bytes()).unwrap();
        assert_eq!(certs.len(), 1);
        let c = &certs[0];
        assert_eq!(c.user_ids, vec!["Alice <alice@example.com>".to_string()]);
        assert!(c.primary.can_sign() && !c.primary.can_encrypt());
        assert!(c.subkeys[0].can_encrypt() && !c.subkeys[0].can_sign());
        assert!(c.primary.unlock(Some("wrong")).is_err());
        assert!(c.primary.unlock(Some("secret")).is_ok());
        assert_eq!(c.primary.fingerprint_hex(), g.fingerprint);
        let p = parse_keyring(g.public_keyring.as_bytes()).unwrap();
        assert!(!p[0].primary.has_secret());
    }
}
