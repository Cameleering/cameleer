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
        verify_chain(&[ss.cert_der.clone()], Some("localhost"), &roots, x509::now_unix(), OID_EKU_SERVER_AUTH).unwrap();
        let k = PrivateKey::from_pem(ss.key_pem.as_bytes()).unwrap();
        assert!(matches!(k, PrivateKey::Ec { .. }));
    }
}
