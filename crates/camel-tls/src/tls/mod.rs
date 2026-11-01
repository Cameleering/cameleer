//! TLS 1.3 / TLS 1.2 protocol implementation.

mod client;
mod codec;
pub mod config;
mod conn;
mod keyschedule;
mod record;
mod server;
pub mod session;
pub mod stream;

use crate::crypto::ec::EcCurve;
use crate::crypto::hash::HashAlg;
use crate::crypto::rng;
use crate::crypto::x25519;
use crate::keys::{ecdsa_sig_from_der, ecdsa_sig_to_der, PrivateKey, PublicKey};
use crate::x509::CertError;
use std::fmt;
use std::io;

pub const TLS12: u16 = 0x0303;
pub const TLS13: u16 = 0x0304;

pub(crate) mod content {
    pub const CHANGE_CIPHER_SPEC: u8 = 20;
    pub const ALERT: u8 = 21;
    pub const HANDSHAKE: u8 = 22;
    pub const APPLICATION_DATA: u8 = 23;
}

pub(crate) mod hs {
    pub const HELLO_REQUEST: u8 = 0;
    pub const CLIENT_HELLO: u8 = 1;
    pub const SERVER_HELLO: u8 = 2;
    pub const NEW_SESSION_TICKET: u8 = 4;
    pub const END_OF_EARLY_DATA: u8 = 5;
    pub const ENCRYPTED_EXTENSIONS: u8 = 8;
    pub const CERTIFICATE_STATUS: u8 = 22;
    pub const CERTIFICATE: u8 = 11;
    pub const SERVER_KEY_EXCHANGE: u8 = 12;
    pub const CERTIFICATE_REQUEST: u8 = 13;
    pub const SERVER_HELLO_DONE: u8 = 14;
    pub const CERTIFICATE_VERIFY: u8 = 15;
    pub const CLIENT_KEY_EXCHANGE: u8 = 16;
    pub const FINISHED: u8 = 20;
    pub const KEY_UPDATE: u8 = 24;
    pub const MESSAGE_HASH: u8 = 254;
}

pub(crate) mod ext {
    pub const SERVER_NAME: u16 = 0;
    pub const STATUS_REQUEST: u16 = 5;
    pub const EARLY_DATA: u16 = 42;
    pub const SUPPORTED_GROUPS: u16 = 10;
    pub const EC_POINT_FORMATS: u16 = 11;
    pub const SIGNATURE_ALGORITHMS: u16 = 13;
    pub const ALPN: u16 = 16;
    pub const EXTENDED_MASTER_SECRET: u16 = 23;
    pub const SESSION_TICKET: u16 = 35;
    pub const PRE_SHARED_KEY: u16 = 41;
    pub const SUPPORTED_VERSIONS: u16 = 43;
    pub const COOKIE: u16 = 44;
    pub const PSK_KEY_EXCHANGE_MODES: u16 = 45;
    pub const CERTIFICATE_AUTHORITIES: u16 = 47;
    pub const KEY_SHARE: u16 = 51;
    pub const RENEGOTIATION_INFO: u16 = 0xff01;
}

/// TLS alert descriptions (RFC 8446 section 6).
pub mod alert {
    pub const CLOSE_NOTIFY: u8 = 0;
    pub const UNEXPECTED_MESSAGE: u8 = 10;
    pub const BAD_RECORD_MAC: u8 = 20;
    pub const RECORD_OVERFLOW: u8 = 22;
    pub const HANDSHAKE_FAILURE: u8 = 40;
    pub const BAD_CERTIFICATE: u8 = 42;
    pub const UNSUPPORTED_CERTIFICATE: u8 = 43;
    pub const CERTIFICATE_REVOKED: u8 = 44;
    pub const CERTIFICATE_EXPIRED: u8 = 45;
    pub const CERTIFICATE_UNKNOWN: u8 = 46;
    pub const ILLEGAL_PARAMETER: u8 = 47;
    pub const UNKNOWN_CA: u8 = 48;
    pub const DECODE_ERROR: u8 = 50;
    pub const DECRYPT_ERROR: u8 = 51;
    pub const PROTOCOL_VERSION: u8 = 70;
    pub const INSUFFICIENT_SECURITY: u8 = 71;
    pub const INTERNAL_ERROR: u8 = 80;
    pub const USER_CANCELED: u8 = 90;
    pub const NO_RENEGOTIATION: u8 = 100;
    pub const MISSING_EXTENSION: u8 = 109;
    pub const UNSUPPORTED_EXTENSION: u8 = 110;
    pub const CERTIFICATE_REQUIRED: u8 = 116;
    pub const UNRECOGNIZED_NAME: u8 = 112;
    pub const NO_APPLICATION_PROTOCOL: u8 = 120;

    pub fn name(code: u8) -> &'static str {
        match code {
            0 => "close_notify",
            10 => "unexpected_message",
            20 => "bad_record_mac",
            22 => "record_overflow",
            40 => "handshake_failure",
            42 => "bad_certificate",
            43 => "unsupported_certificate",
            44 => "certificate_revoked",
            45 => "certificate_expired",
            46 => "certificate_unknown",
            47 => "illegal_parameter",
            48 => "unknown_ca",
            49 => "access_denied",
            50 => "decode_error",
            51 => "decrypt_error",
            70 => "protocol_version",
            71 => "insufficient_security",
            80 => "internal_error",
            86 => "inappropriate_fallback",
            90 => "user_canceled",
            100 => "no_renegotiation",
            109 => "missing_extension",
            110 => "unsupported_extension",
            112 => "unrecognized_name",
            113 => "bad_certificate_status_response",
            115 => "unknown_psk_identity",
            116 => "certificate_required",
            120 => "no_application_protocol",
            _ => "unknown_alert",
        }
    }
}

/// Errors produced by the TLS layer.
#[derive(Debug)]
pub enum TlsError {
    Io(io::Error),
    /// The peer sent a fatal alert.
    AlertReceived(u8),
    /// A protocol violation detected locally; the alert is sent to the peer.
    Protocol { alert: u8, message: String },
    /// Certificate validation failed.
    Certificate(CertError),
    /// Invalid configuration (certificates, keys, options).
    Config(String),
    /// The connection was closed (close_notify sent or received).
    Closed,
}

impl TlsError {
    pub(crate) fn proto(alert: u8, message: impl Into<String>) -> TlsError {
        TlsError::Protocol { alert, message: message.into() }
    }
    pub(crate) fn decode(what: &str) -> TlsError {
        TlsError::proto(alert::DECODE_ERROR, format!("malformed {what}"))
    }
    /// The alert to send for this error, if any.
    pub fn alert_to_send(&self) -> Option<u8> {
        match self {
            TlsError::Protocol { alert, .. } => Some(*alert),
            TlsError::Certificate(c) => Some(match c {
                CertError::Expired { .. } | CertError::NotYetValid { .. } => alert::CERTIFICATE_EXPIRED,
                CertError::UnknownIssuer { .. } => alert::UNKNOWN_CA,
                CertError::Parse(_) => alert::BAD_CERTIFICATE,
                CertError::BadSignature { .. } => alert::BAD_CERTIFICATE,
                CertError::Revoked { .. } => alert::CERTIFICATE_REVOKED,
                _ => alert::CERTIFICATE_UNKNOWN,
            }),
            _ => None,
        }
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsError::Io(e) => write!(f, "TLS I/O error: {e}"),
            TlsError::AlertReceived(a) => write!(f, "TLS alert received from peer: {} ({a})", alert::name(*a)),
            TlsError::Protocol { alert: a, message } => write!(f, "TLS protocol error: {message} (alert {})", alert::name(*a)),
            TlsError::Certificate(c) => write!(f, "TLS certificate error: {c}"),
            TlsError::Config(m) => write!(f, "TLS configuration error: {m}"),
            TlsError::Closed => write!(f, "TLS connection closed"),
        }
    }
}

impl std::error::Error for TlsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TlsError::Io(e) => Some(e),
            TlsError::Certificate(c) => Some(c),
            _ => None,
        }
    }
}

impl From<io::Error> for TlsError {
    fn from(e: io::Error) -> TlsError {
        TlsError::Io(e)
    }
}

impl From<CertError> for TlsError {
    fn from(e: CertError) -> TlsError {
        TlsError::Certificate(e)
    }
}

impl From<TlsError> for io::Error {
    fn from(e: TlsError) -> io::Error {
        match e {
            TlsError::Io(e) => e,
            TlsError::Closed => io::Error::new(io::ErrorKind::BrokenPipe, "TLS connection closed"),
            other => io::Error::new(io::ErrorKind::InvalidData, other),
        }
    }
}

pub type TlsResult<T> = Result<T, TlsError>;

/// Protocol version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TlsVersion {
    Tls12,
    Tls13,
}

impl TlsVersion {
    pub fn wire(self) -> u16 {
        match self {
            TlsVersion::Tls12 => TLS12,
            TlsVersion::Tls13 => TLS13,
        }
    }
}

/// Supported cipher suites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CipherSuite {
    Tls13Aes128GcmSha256,
    Tls13Aes256GcmSha384,
    Tls13Chacha20Poly1305Sha256,
    EcdheEcdsaAes128GcmSha256,
    EcdheEcdsaAes256GcmSha384,
    EcdheEcdsaChacha20Poly1305,
    EcdheRsaAes128GcmSha256,
    EcdheRsaAes256GcmSha384,
    EcdheRsaChacha20Poly1305,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AeadAlg {
    Aes128Gcm,
    Aes256Gcm,
    Chacha20Poly1305,
}

impl AeadAlg {
    pub(crate) fn key_len(self) -> usize {
        match self {
            AeadAlg::Aes128Gcm => 16,
            _ => 32,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthKind {
    Ecdsa,
    Rsa,
}

impl CipherSuite {
    pub const ALL: [CipherSuite; 9] = [
        CipherSuite::Tls13Aes128GcmSha256,
        CipherSuite::Tls13Aes256GcmSha384,
        CipherSuite::Tls13Chacha20Poly1305Sha256,
        CipherSuite::EcdheEcdsaAes128GcmSha256,
        CipherSuite::EcdheRsaAes128GcmSha256,
        CipherSuite::EcdheEcdsaAes256GcmSha384,
        CipherSuite::EcdheRsaAes256GcmSha384,
        CipherSuite::EcdheEcdsaChacha20Poly1305,
        CipherSuite::EcdheRsaChacha20Poly1305,
    ];

    pub fn id(self) -> u16 {
        match self {
            CipherSuite::Tls13Aes128GcmSha256 => 0x1301,
            CipherSuite::Tls13Aes256GcmSha384 => 0x1302,
            CipherSuite::Tls13Chacha20Poly1305Sha256 => 0x1303,
            CipherSuite::EcdheEcdsaAes128GcmSha256 => 0xc02b,
            CipherSuite::EcdheEcdsaAes256GcmSha384 => 0xc02c,
            CipherSuite::EcdheRsaAes128GcmSha256 => 0xc02f,
            CipherSuite::EcdheRsaAes256GcmSha384 => 0xc030,
            CipherSuite::EcdheRsaChacha20Poly1305 => 0xcca8,
            CipherSuite::EcdheEcdsaChacha20Poly1305 => 0xcca9,
        }
    }

    pub fn from_id(id: u16) -> Option<CipherSuite> {
        CipherSuite::ALL.iter().copied().find(|s| s.id() == id)
    }

    /// IANA name, e.g. `TLS_AES_128_GCM_SHA256`.
    pub fn name(self) -> &'static str {
        match self {
            CipherSuite::Tls13Aes128GcmSha256 => "TLS_AES_128_GCM_SHA256",
            CipherSuite::Tls13Aes256GcmSha384 => "TLS_AES_256_GCM_SHA384",
            CipherSuite::Tls13Chacha20Poly1305Sha256 => "TLS_CHACHA20_POLY1305_SHA256",
            CipherSuite::EcdheEcdsaAes128GcmSha256 => "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256",
            CipherSuite::EcdheEcdsaAes256GcmSha384 => "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384",
            CipherSuite::EcdheEcdsaChacha20Poly1305 => "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256",
            CipherSuite::EcdheRsaAes128GcmSha256 => "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256",
            CipherSuite::EcdheRsaAes256GcmSha384 => "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
            CipherSuite::EcdheRsaChacha20Poly1305 => "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256",
        }
    }

    pub fn is_tls13(self) -> bool {
        matches!(self, CipherSuite::Tls13Aes128GcmSha256 | CipherSuite::Tls13Aes256GcmSha384 | CipherSuite::Tls13Chacha20Poly1305Sha256)
    }

    pub fn hash(self) -> HashAlg {
        match self {
            CipherSuite::Tls13Aes256GcmSha384 | CipherSuite::EcdheEcdsaAes256GcmSha384 | CipherSuite::EcdheRsaAes256GcmSha384 => {
                HashAlg::Sha384
            }
            _ => HashAlg::Sha256,
        }
    }

    pub(crate) fn aead(self) -> AeadAlg {
        match self {
            CipherSuite::Tls13Aes128GcmSha256 | CipherSuite::EcdheEcdsaAes128GcmSha256 | CipherSuite::EcdheRsaAes128GcmSha256 => {
                AeadAlg::Aes128Gcm
            }
            CipherSuite::Tls13Aes256GcmSha384 | CipherSuite::EcdheEcdsaAes256GcmSha384 | CipherSuite::EcdheRsaAes256GcmSha384 => {
                AeadAlg::Aes256Gcm
            }
            _ => AeadAlg::Chacha20Poly1305,
        }
    }

    pub(crate) fn auth(self) -> Option<AuthKind> {
        match self {
            CipherSuite::EcdheEcdsaAes128GcmSha256 | CipherSuite::EcdheEcdsaAes256GcmSha384 | CipherSuite::EcdheEcdsaChacha20Poly1305 => {
                Some(AuthKind::Ecdsa)
            }
            CipherSuite::EcdheRsaAes128GcmSha256 | CipherSuite::EcdheRsaAes256GcmSha384 | CipherSuite::EcdheRsaChacha20Poly1305 => {
                Some(AuthKind::Rsa)
            }
            _ => None,
        }
    }
}

/// Supported key-exchange groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedGroup {
    X25519,
    Secp256r1,
    Secp384r1,
}

impl NamedGroup {
    pub const ALL: [NamedGroup; 3] = [NamedGroup::X25519, NamedGroup::Secp256r1, NamedGroup::Secp384r1];

    pub fn id(self) -> u16 {
        match self {
            NamedGroup::X25519 => 0x001d,
            NamedGroup::Secp256r1 => 0x0017,
            NamedGroup::Secp384r1 => 0x0018,
        }
    }

    pub fn from_id(id: u16) -> Option<NamedGroup> {
        NamedGroup::ALL.iter().copied().find(|g| g.id() == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            NamedGroup::X25519 => "x25519",
            NamedGroup::Secp256r1 => "secp256r1",
            NamedGroup::Secp384r1 => "secp384r1",
        }
    }
}

/// An ephemeral key-exchange key pair.
pub(crate) struct KeyShare {
    pub group: NamedGroup,
    private: Vec<u8>,
    pub public: Vec<u8>,
}

impl KeyShare {
    pub fn generate(group: NamedGroup) -> KeyShare {
        match group {
            NamedGroup::X25519 => {
                let mut sk = [0u8; 32];
                rng::fill(&mut sk);
                let pk = x25519::x25519_base(&sk);
                KeyShare { group, private: sk.to_vec(), public: pk.to_vec() }
            }
            NamedGroup::Secp256r1 | NamedGroup::Secp384r1 => {
                let curve = if group == NamedGroup::Secp256r1 { EcCurve::P256 } else { EcCurve::P384 };
                let sk = curve.generate_private();
                let pk = curve.public_key(&sk).expect("valid scalar");
                KeyShare { group, private: sk, public: pk }
            }
        }
    }

    /// Computes the shared secret; fails for invalid peer keys.
    pub fn agree(&self, peer: &[u8]) -> TlsResult<Vec<u8>> {
        let bad = || TlsError::proto(alert::ILLEGAL_PARAMETER, "invalid key share from peer");
        match self.group {
            NamedGroup::X25519 => {
                let p: [u8; 32] = peer.try_into().map_err(|_| bad())?;
                let s: [u8; 32] = self.private.as_slice().try_into().unwrap();
                let out = x25519::x25519(&s, &p);
                if out.iter().all(|b| *b == 0) {
                    return Err(bad());
                }
                Ok(out.to_vec())
            }
            NamedGroup::Secp256r1 => EcCurve::P256.ecdh(&self.private, peer).ok_or_else(bad),
            NamedGroup::Secp384r1 => EcCurve::P384.ecdh(&self.private, peer).ok_or_else(bad),
        }
    }
}

/// Signature schemes (RFC 8446 section 4.2.3).
pub mod sigscheme {
    pub const RSA_PKCS1_SHA256: u16 = 0x0401;
    pub const RSA_PKCS1_SHA384: u16 = 0x0501;
    pub const RSA_PKCS1_SHA512: u16 = 0x0601;
    pub const ECDSA_SECP256R1_SHA256: u16 = 0x0403;
    pub const ECDSA_SECP384R1_SHA384: u16 = 0x0503;
    pub const RSA_PSS_RSAE_SHA256: u16 = 0x0804;
    pub const RSA_PSS_RSAE_SHA384: u16 = 0x0805;
    pub const RSA_PSS_RSAE_SHA512: u16 = 0x0806;
    pub const ED25519: u16 = 0x0807;

    /// What we offer / accept, in preference order.
    pub const SUPPORTED: [u16; 9] = [
        ED25519,
        ECDSA_SECP256R1_SHA256,
        ECDSA_SECP384R1_SHA384,
        RSA_PSS_RSAE_SHA256,
        RSA_PSS_RSAE_SHA384,
        RSA_PSS_RSAE_SHA512,
        RSA_PKCS1_SHA256,
        RSA_PKCS1_SHA384,
        RSA_PKCS1_SHA512,
    ];
}

fn scheme_hash(scheme: u16) -> Option<HashAlg> {
    use sigscheme::*;
    match scheme {
        RSA_PKCS1_SHA256 | ECDSA_SECP256R1_SHA256 | RSA_PSS_RSAE_SHA256 => Some(HashAlg::Sha256),
        RSA_PKCS1_SHA384 | ECDSA_SECP384R1_SHA384 | RSA_PSS_RSAE_SHA384 => Some(HashAlg::Sha384),
        RSA_PKCS1_SHA512 | RSA_PSS_RSAE_SHA512 => Some(HashAlg::Sha512),
        _ => None,
    }
}

/// Picks the signature scheme for our key given the peer's offered list.
pub(crate) fn choose_scheme(key: &PrivateKey, offered: &[u16], tls13: bool) -> Option<u16> {
    use sigscheme::*;
    let candidates: Vec<u16> = match key {
        PrivateKey::Ec { curve: EcCurve::P256, .. } => vec![ECDSA_SECP256R1_SHA256],
        PrivateKey::Ec { curve: EcCurve::P384, .. } => vec![ECDSA_SECP384R1_SHA384],
        PrivateKey::Ed25519 { .. } => vec![ED25519],
        PrivateKey::Rsa(_) if tls13 => vec![RSA_PSS_RSAE_SHA256, RSA_PSS_RSAE_SHA384, RSA_PSS_RSAE_SHA512],
        PrivateKey::Rsa(_) => vec![
            RSA_PSS_RSAE_SHA256,
            RSA_PSS_RSAE_SHA384,
            RSA_PSS_RSAE_SHA512,
            RSA_PKCS1_SHA256,
            RSA_PKCS1_SHA384,
            RSA_PKCS1_SHA512,
        ],
    };
    candidates.into_iter().find(|c| offered.contains(c))
}

/// Signs `message` (hashing it with the scheme's hash).
pub(crate) fn sign_message(key: &PrivateKey, scheme: u16, message: &[u8]) -> TlsResult<Vec<u8>> {
    if let PrivateKey::Ed25519 { seed, .. } = key {
        if scheme != sigscheme::ED25519 {
            return Err(TlsError::proto(alert::INTERNAL_ERROR, "unsupported signature scheme"));
        }
        return Ok(crate::crypto::ed25519::sign(seed, message).to_vec());
    }
    let h = scheme_hash(scheme).ok_or_else(|| TlsError::proto(alert::INTERNAL_ERROR, "unsupported signature scheme"))?;
    let digest = h.hash(message);
    let sig = match key {
        PrivateKey::Ec { curve, d, .. } => curve.sign(d, &digest, h).map(|(r, s)| ecdsa_sig_to_der(&r, &s)),
        PrivateKey::Rsa(k) => match scheme {
            sigscheme::RSA_PSS_RSAE_SHA256 | sigscheme::RSA_PSS_RSAE_SHA384 | sigscheme::RSA_PSS_RSAE_SHA512 => k.sign_pss(h, &digest),
            _ => k.sign_pkcs1(h, &digest),
        },
        PrivateKey::Ed25519 { .. } => None,
    };
    sig.ok_or_else(|| TlsError::proto(alert::INTERNAL_ERROR, "signing failed"))
}

/// Verifies a handshake signature. In TLS 1.3 ECDSA schemes are bound to their
/// curve and PKCS#1 v1.5 is not allowed.
pub(crate) fn verify_message(key: &PublicKey, scheme: u16, message: &[u8], sig: &[u8], tls13: bool) -> TlsResult<()> {
    use sigscheme::*;
    let fail = || TlsError::proto(alert::DECRYPT_ERROR, "handshake signature verification failed");
    if !SUPPORTED.contains(&scheme) {
        return Err(TlsError::proto(alert::ILLEGAL_PARAMETER, format!("unsupported signature scheme {scheme:#06x}")));
    }
    if scheme == ED25519 {
        return match key {
            PublicKey::Ed25519(k) if crate::crypto::ed25519::verify(k, message, sig) => Ok(()),
            PublicKey::Ed25519(_) => Err(fail()),
            _ => Err(TlsError::proto(alert::ILLEGAL_PARAMETER, "signature scheme does not match key type")),
        };
    }
    let h = scheme_hash(scheme).unwrap();
    let digest = h.hash(message);
    let ok = match (key, scheme) {
        (PublicKey::Ec { curve, point }, ECDSA_SECP256R1_SHA256 | ECDSA_SECP384R1_SHA384) => {
            if tls13 && ((scheme == ECDSA_SECP256R1_SHA256) != (*curve == EcCurve::P256)) {
                return Err(TlsError::proto(alert::ILLEGAL_PARAMETER, "ECDSA scheme does not match certificate curve"));
            }
            match ecdsa_sig_from_der(sig) {
                Some((r, s)) => curve.verify(point, &digest, &r, &s),
                None => false,
            }
        }
        (PublicKey::Rsa(k), RSA_PSS_RSAE_SHA256 | RSA_PSS_RSAE_SHA384 | RSA_PSS_RSAE_SHA512) => {
            k.verify_pss(h, &digest, sig, Some(h.output_len()))
        }
        (PublicKey::Rsa(k), RSA_PKCS1_SHA256 | RSA_PKCS1_SHA384 | RSA_PKCS1_SHA512) if !tls13 => k.verify_pkcs1(h, &digest, sig),
        _ => return Err(TlsError::proto(alert::ILLEGAL_PARAMETER, "signature scheme does not match key type")),
    };
    if ok {
        Ok(())
    } else {
        Err(fail())
    }
}
