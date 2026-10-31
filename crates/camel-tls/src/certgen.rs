//! Runtime generation of self-signed ECDSA P-256 certificates (DER encoder),
//! for tests and development servers.

use crate::asn1;
use crate::crypto::ec::EcCurve;
use crate::crypto::hash::{Digest, Sha256};
use crate::crypto::rng;
use crate::keys::{self, PrivateKey};
use crate::pem;
use crate::x509;
use std::net::IpAddr;

/// A generated certificate and its private key, in DER and PEM forms.
#[derive(Clone, Debug)]
pub struct SelfSigned {
    pub cert_der: Vec<u8>,
    /// PKCS#8 DER.
    pub key_der: Vec<u8>,
    pub cert_pem: String,
    pub key_pem: String,
    pub key: PrivateKey,
}

/// Self-signed P-256 certificate for `name` (also used as CN), valid from one
/// day ago for 365 days. `localhost` additionally gets the 127.0.0.1 and ::1 IP SANs.
pub fn self_signed(name: &str) -> SelfSigned {
    let mut names = vec![name.to_string()];
    if name.eq_ignore_ascii_case("localhost") {
        names.push("127.0.0.1".into());
        names.push("::1".into());
    }
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    self_signed_with_sans(&refs, 365)
}

/// Self-signed P-256 certificate with the given SAN entries (DNS names or IP
/// addresses); the first entry becomes the subject CN.
pub fn self_signed_with_sans(names: &[&str], days_valid: i64) -> SelfSigned {
    assert!(!names.is_empty(), "at least one name required");
    let curve = EcCurve::P256;
    let d = curve.generate_private();
    let public = curve.public_key(&d).expect("valid scalar");
    let key = PrivateKey::Ec { curve, d: d.clone(), public: public.clone() };

    let now = x509::now_unix();
    let mut serial = rng::random_bytes(16);
    serial[0] &= 0x7f;
    serial[0] |= 0x01;
    let sig_alg = asn1::seq(&[asn1::oid("1.2.840.10045.4.3.2")]);
    let name = asn1::seq(&[asn1::set(&[asn1::seq(&[asn1::oid("2.5.4.3"), asn1::tlv(asn1::TAG_UTF8_STRING, names[0].as_bytes())])])]);
    let validity = asn1::seq(&[asn1::time(now - 86400), asn1::time(now + days_valid * 86400)]);
    let spki = asn1::seq(&[asn1::seq(&[asn1::oid(keys::OID_EC_PUBLIC_KEY), asn1::oid(keys::OID_P256)]), asn1::bit_string(&public)]);

    let mut general_names = Vec::new();
    for n in names {
        match n.parse::<IpAddr>() {
            Ok(IpAddr::V4(v4)) => general_names.push(asn1::tlv(0x87, &v4.octets())),
            Ok(IpAddr::V6(v6)) => general_names.push(asn1::tlv(0x87, &v6.octets())),
            Err(_) => general_names.push(asn1::tlv(0x82, n.as_bytes())),
        }
    }
    let ext = |oid: &str, critical: bool, value: Vec<u8>| -> Vec<u8> {
        let mut parts = vec![asn1::oid(oid)];
        if critical {
            parts.push(asn1::tlv(asn1::TAG_BOOLEAN, &[0xff]));
        }
        parts.push(asn1::octet_string(&value));
        asn1::seq(&parts)
    };
    let ski = Sha256::digest(&public)[..20].to_vec();
    let extensions = asn1::seq(&[
        ext("2.5.29.17", false, asn1::seq(&general_names)),
        // CA:TRUE so the certificate can act as its own trust anchor in other TLS stacks
        ext("2.5.29.19", true, asn1::seq(&[asn1::tlv(asn1::TAG_BOOLEAN, &[0xff])])),
        // digitalSignature | keyCertSign
        ext("2.5.29.15", true, asn1::tlv(asn1::TAG_BIT_STRING, &[0x02, 0x84])),
        ext("2.5.29.37", false, asn1::seq(&[asn1::oid(x509::OID_EKU_SERVER_AUTH), asn1::oid(x509::OID_EKU_CLIENT_AUTH)])),
        ext("2.5.29.14", false, asn1::octet_string(&ski)),
    ]);
    let tbs = asn1::seq(&[
        asn1::explicit(0, &asn1::uint(&[2])),
        asn1::uint(&serial),
        sig_alg.clone(),
        name.clone(),
        validity,
        name,
        spki,
        asn1::explicit(3, &extensions),
    ]);
    let (r, s) = curve.sign(&d, &Sha256::digest(&tbs), crate::crypto::hash::HashAlg::Sha256).expect("sign");
    let sig = keys::ecdsa_sig_to_der(&r, &s);
    let cert_der = asn1::seq(&[tbs, sig_alg, asn1::bit_string(&sig)]);
    let key_der = key.to_pkcs8_der().expect("EC key");
    SelfSigned {
        cert_pem: pem::encode_pem("CERTIFICATE", &cert_der),
        key_pem: pem::encode_pem("PRIVATE KEY", &key_der),
        cert_der,
        key_der,
        key,
    }
}

/// Parameters of a certificate issued by [`issue`].
#[derive(Clone, Debug)]
pub struct CertParams {
    /// Subject common name.
    pub common_name: String,
    /// SAN entries (DNS names or IP addresses).
    pub sans: Vec<String>,
    pub is_ca: bool,
    /// Extended key usage OIDs (empty = no extension).
    pub ext_key_usage: Vec<String>,
    pub days_valid: i64,
}

impl CertParams {
    /// A TLS server certificate for the given names (first = CN).
    pub fn server(names: &[&str]) -> CertParams {
        CertParams {
            common_name: names[0].to_string(),
            sans: names.iter().map(|s| s.to_string()).collect(),
            is_ca: false,
            ext_key_usage: vec![x509::OID_EKU_SERVER_AUTH.to_string()],
            days_valid: 365,
        }
    }

    /// A TLS client certificate with subject CN `cn`.
    pub fn client(cn: &str) -> CertParams {
        CertParams { common_name: cn.to_string(), sans: Vec::new(), is_ca: false, ext_key_usage: vec![x509::OID_EKU_CLIENT_AUTH.to_string()], days_valid: 365 }
    }

    /// A CA certificate.
    pub fn ca(cn: &str) -> CertParams {
        CertParams { common_name: cn.to_string(), sans: Vec::new(), is_ca: true, ext_key_usage: Vec::new(), days_valid: 3650 }
    }
}

fn sig_alg_for(key: &PrivateKey) -> Vec<u8> {
    match key {
        PrivateKey::Ec { curve: crate::crypto::ec::EcCurve::P384, .. } => asn1::seq(&[asn1::oid("1.2.840.10045.4.3.3")]),
        PrivateKey::Ec { .. } => asn1::seq(&[asn1::oid("1.2.840.10045.4.3.2")]),
        PrivateKey::Ed25519 { .. } => asn1::seq(&[asn1::oid(keys::OID_ED25519)]),
        PrivateKey::Rsa(_) => asn1::seq(&[asn1::oid("1.2.840.113549.1.1.11"), asn1::tlv(asn1::TAG_NULL, &[])]),
    }
}

fn sign_with(key: &PrivateKey, tbs: &[u8]) -> Vec<u8> {
    use crate::crypto::hash::HashAlg;
    match key {
        PrivateKey::Ec { curve, d, .. } => {
            let h = curve.hash();
            let (r, s) = curve.sign(d, &h.hash(tbs), h).expect("sign");
            keys::ecdsa_sig_to_der(&r, &s)
        }
        PrivateKey::Ed25519 { seed, .. } => crate::crypto::ed25519::sign(seed, tbs).to_vec(),
        PrivateKey::Rsa(k) => k.sign_pkcs1(HashAlg::Sha256, &HashAlg::Sha256.hash(tbs)).expect("sign"),
    }
}

fn name_der(cn: &str) -> Vec<u8> {
    asn1::seq(&[asn1::set(&[asn1::seq(&[asn1::oid("2.5.4.3"), asn1::tlv(asn1::TAG_UTF8_STRING, cn.as_bytes())])])])
}

/// Issues a certificate for `key` signed by `issuer` (self-signed when `None`).
pub fn issue(params: &CertParams, key: PrivateKey, issuer: Option<&SelfSigned>) -> SelfSigned {
    let now = x509::now_unix();
    let mut serial = rng::random_bytes(16);
    serial[0] &= 0x7f;
    serial[0] |= 0x01;
    let (issuer_name, signer) = match issuer {
        Some(i) => (x509::Certificate::from_der(&i.cert_der).expect("issuer certificate").subject_raw, &i.key),
        None => (name_der(&params.common_name), &key),
    };
    let sig_alg = sig_alg_for(signer);
    let spki = key.public_key().to_spki();
    let ext = |oid: &str, critical: bool, value: Vec<u8>| -> Vec<u8> {
        let mut parts = vec![asn1::oid(oid)];
        if critical {
            parts.push(asn1::tlv(asn1::TAG_BOOLEAN, &[0xff]));
        }
        parts.push(asn1::octet_string(&value));
        asn1::seq(&parts)
    };
    let mut exts = Vec::new();
    if !params.sans.is_empty() {
        let mut general_names = Vec::new();
        for n in &params.sans {
            match n.parse::<IpAddr>() {
                Ok(IpAddr::V4(v4)) => general_names.push(asn1::tlv(0x87, &v4.octets())),
                Ok(IpAddr::V6(v6)) => general_names.push(asn1::tlv(0x87, &v6.octets())),
                Err(_) => general_names.push(asn1::tlv(0x82, n.as_bytes())),
            }
        }
        exts.push(ext("2.5.29.17", false, asn1::seq(&general_names)));
    }
    if params.is_ca {
        exts.push(ext("2.5.29.19", true, asn1::seq(&[asn1::tlv(asn1::TAG_BOOLEAN, &[0xff])])));
        // digitalSignature | keyCertSign | cRLSign
        exts.push(ext("2.5.29.15", true, asn1::tlv(asn1::TAG_BIT_STRING, &[0x01, 0x86])));
    } else {
        exts.push(ext("2.5.29.19", true, asn1::seq(&[])));
        exts.push(ext("2.5.29.15", true, asn1::tlv(asn1::TAG_BIT_STRING, &[0x07, 0x80])));
    }
    if !params.ext_key_usage.is_empty() {
        exts.push(ext("2.5.29.37", false, asn1::seq(&params.ext_key_usage.iter().map(|o| asn1::oid(o)).collect::<Vec<_>>())));
    }
    let tbs = asn1::seq(&[
        asn1::explicit(0, &asn1::uint(&[2])),
        asn1::uint(&serial),
        sig_alg.clone(),
        issuer_name,
        asn1::seq(&[asn1::time(now - 86400), asn1::time(now + params.days_valid * 86400)]),
        name_der(&params.common_name),
        spki,
        asn1::explicit(3, &asn1::seq(&exts)),
    ]);
    let sig = sign_with(signer, &tbs);
    let cert_der = asn1::seq(&[tbs, sig_alg, asn1::bit_string(&sig)]);
    let key_der = key.to_pkcs8();
    SelfSigned { cert_pem: pem::encode_pem("CERTIFICATE", &cert_der), key_pem: pem::encode_pem("PRIVATE KEY", &key_der), cert_der, key_der, key }
}

/// Issues a CRL signed by `issuer` revoking the certificates with the given DER
/// encodings (their serial numbers), valid for `days`.
pub fn issue_crl(issuer: &SelfSigned, revoked: &[&[u8]], days: i64) -> Vec<u8> {
    let now = x509::now_unix();
    let issuer_cert = x509::Certificate::from_der(&issuer.cert_der).expect("issuer certificate");
    let sig_alg = sig_alg_for(&issuer.key);
    let mut entries = Vec::new();
    for der in revoked {
        let c = x509::Certificate::from_der(der).expect("revoked certificate");
        entries.push(asn1::seq(&[asn1::tlv(asn1::TAG_INTEGER, &c.serial), asn1::time(now - 60)]));
    }
    let mut parts = vec![asn1::uint(&[1]), sig_alg.clone(), issuer_cert.subject_raw, asn1::time(now - 60), asn1::time(now + days * 86400)];
    if !entries.is_empty() {
        parts.push(asn1::seq(&entries));
    }
    let tbs = asn1::seq(&parts);
    let sig = sign_with(&issuer.key, &tbs);
    asn1::seq(&[tbs, sig_alg, asn1::bit_string(&sig)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x509::{verify_chain, Certificate, RootStore, OID_EKU_SERVER_AUTH};

    #[test]
    fn generated_certificate_parses_and_verifies() {
        let ss = self_signed("localhost");
        let cert = Certificate::from_der(&ss.cert_der).unwrap();
        assert_eq!(cert.subject.common_name(), Some("localhost"));
        assert!(cert.matches_hostname("localhost"));
        assert!(cert.matches_hostname("127.0.0.1"));
        assert!(cert.matches_hostname("::1"));
        assert!(!cert.matches_hostname("example.com"));
        assert!(cert.verify_signed_by(cert.public_key.as_ref().unwrap()));
        let mut roots = RootStore::empty();
        roots.add_pem(ss.cert_pem.as_bytes()).unwrap();
        verify_chain(std::slice::from_ref(&ss.cert_der), Some("localhost"), &roots, x509::now_unix(), OID_EKU_SERVER_AUTH).unwrap();
        let k = PrivateKey::from_pem(ss.key_pem.as_bytes()).unwrap();
        assert!(matches!(k, PrivateKey::Ec { .. }));
    }

    #[test]
    fn issued_chain_and_crl() {
        let ca = issue(&CertParams::ca("Test CA"), PrivateKey::generate_ed25519(), None);
        let srv = issue(&CertParams::server(&["localhost"]), PrivateKey::generate_ec(crate::crypto::ec::EcCurve::P256), Some(&ca));
        let cli = issue(&CertParams::client("alice"), PrivateKey::generate_ed25519(), Some(&ca));
        let mut roots = RootStore::empty();
        roots.add_der(&ca.cert_der).unwrap();
        let now = x509::now_unix();
        verify_chain(std::slice::from_ref(&srv.cert_der), Some("localhost"), &roots, now, OID_EKU_SERVER_AUTH).unwrap();
        verify_chain(std::slice::from_ref(&cli.cert_der), None, &roots, now, x509::OID_EKU_CLIENT_AUTH).unwrap();
        assert!(verify_chain(std::slice::from_ref(&cli.cert_der), None, &roots, now, OID_EKU_SERVER_AUTH).is_err());
        let crl = x509::Crl::from_der(&issue_crl(&ca, &[&cli.cert_der], 7)).unwrap();
        assert_eq!(crl.revoked.len(), 1);
        assert!(crl.verify_signed_by(&ca.key.public_key()));
        let r = x509::verify_chain_with_crls(std::slice::from_ref(&cli.cert_der), None, &roots, now, x509::OID_EKU_CLIENT_AUTH, std::slice::from_ref(&crl));
        assert!(matches!(r, Err(x509::CertError::Revoked { .. })));
        x509::verify_chain_with_crls(std::slice::from_ref(&srv.cert_der), None, &roots, now, OID_EKU_SERVER_AUTH, &[crl]).unwrap();
    }
}
