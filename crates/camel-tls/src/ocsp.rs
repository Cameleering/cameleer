//! OCSP (RFC 6960) responses: parsing, validation against a certificate and its issuer
//! (signature by the issuer or by a delegated responder carrying the OCSPSigning EKU,
//! CertID match with SHA-1 or SHA-256 hashes, thisUpdate / nextUpdate window), and building
//! responses (embedded responders, tests). Used for TLS OCSP stapling (`status_request`).

use crate::asn1::{self, Der, DerResult, Tlv};
use crate::crypto::hash::HashAlg;
use crate::keys::{PrivateKey, SignatureAlgorithm};
use crate::x509::Certificate;

/// id-pkix-ocsp-basic.
pub const OID_OCSP_BASIC: &str = "1.3.6.1.5.5.7.48.1.1";
/// id-kp-OCSPSigning.
pub const OID_EKU_OCSP_SIGNING: &str = "1.3.6.1.5.5.7.3.9";
/// Allowed clock skew when checking thisUpdate / nextUpdate.
pub const CLOCK_SKEW_SECS: i64 = 300;

/// The status of one certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CertStatus {
    Good,
    Revoked { time: i64, reason: Option<u8> },
    Unknown,
}

/// One SingleResponse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SingleResponse {
    pub hash: HashAlg,
    pub issuer_name_hash: Vec<u8>,
    pub issuer_key_hash: Vec<u8>,
    pub serial: Vec<u8>,
    pub status: CertStatus,
    pub this_update: i64,
    pub next_update: Option<i64>,
}

/// A parsed (successful, basic) OCSP response.
#[derive(Clone, Debug)]
pub struct OcspResponse {
    pub der: Vec<u8>,
    pub produced_at: i64,
    pub responses: Vec<SingleResponse>,
    tbs: Vec<u8>,
    sig_alg: SignatureAlgorithm,
    signature: Vec<u8>,
    /// Certificates embedded by the responder (delegated responder chain).
    pub certs: Vec<Vec<u8>>,
}

fn err(m: impl Into<String>) -> String {
    m.into()
}

fn hash_from_oid(oid: &str) -> Option<HashAlg> {
    match oid {
        "1.3.14.3.2.26" => Some(HashAlg::Sha1),
        "2.16.840.1.101.3.4.2.1" => Some(HashAlg::Sha256),
        "2.16.840.1.101.3.4.2.2" => Some(HashAlg::Sha384),
        "2.16.840.1.101.3.4.2.3" => Some(HashAlg::Sha512),
        _ => None,
    }
}

fn hash_oid(h: HashAlg) -> &'static str {
    match h {
        HashAlg::Sha1 => "1.3.14.3.2.26",
        HashAlg::Sha256 => "2.16.840.1.101.3.4.2.1",
        HashAlg::Sha384 => "2.16.840.1.101.3.4.2.2",
        HashAlg::Sha512 => "2.16.840.1.101.3.4.2.3",
    }
}

/// SHA hash of the issuer's public key (the BIT STRING contents of its SPKI).
fn key_hash(h: HashAlg, issuer: &Certificate) -> Vec<u8> {
    let key_bits = (|| -> DerResult<Vec<u8>> {
        let mut spki = Der::new(&issuer.spki).sequence()?;
        spki.expect(asn1::TAG_SEQUENCE)?;
        Ok(spki.bit_string()?.to_vec())
    })()
    .unwrap_or_default();
    h.hash(&key_bits)
}

fn parse_single(t: Der) -> DerResult<SingleResponse> {
    let mut s = t;
    let mut cid = s.sequence()?;
    let mut alg = cid.sequence()?;
    let hash = hash_from_oid(&alg.oid()?).ok_or_else(|| asn1::DerError("unsupported CertID hash".into()))?;
    let issuer_name_hash = cid.octet_string()?.to_vec();
    let issuer_key_hash = cid.octet_string()?.to_vec();
    let serial = cid.uint()?.to_vec();
    let st = s.read()?;
    let status = match st.tag {
        0x80 => CertStatus::Good,
        0xa1 => {
            let mut r = st.reader();
            let time = asn1::parse_time(&r.read()?)?;
            let reason = match r.optional(0xa0)? {
                Some(t) => t.reader().read()?.value.first().copied(),
                None => None,
            };
            CertStatus::Revoked { time, reason }
        }
        0x82 => CertStatus::Unknown,
        _ => return Err(asn1::DerError("bad certStatus".into())),
    };
    let this_update = asn1::parse_time(&s.read()?)?;
    let next_update = match s.optional(0xa0)? {
        Some(t) => Some(asn1::parse_time(&t.reader().read()?)?),
        None => None,
    };
    Ok(SingleResponse { hash, issuer_name_hash, issuer_key_hash, serial, status, this_update, next_update })
}

impl OcspResponse {
    /// Parses a DER OCSPResponse; non-successful statuses are errors.
    pub fn parse(der: &[u8]) -> Result<OcspResponse, String> {
        let r = (|| -> DerResult<Result<OcspResponse, String>> {
            let mut top = Der::new(der).sequence()?;
            let status = top.read()?;
            if status.tag != 0x0a {
                return Err(asn1::DerError("bad responseStatus".into()));
            }
            let code = status.value.first().copied().unwrap_or(255);
            if code != 0 {
                let name = match code {
                    1 => "malformedRequest",
                    2 => "internalError",
                    3 => "tryLater",
                    5 => "sigRequired",
                    6 => "unauthorized",
                    _ => "unknown",
                };
                return Ok(Err(err(format!("OCSP responder status {} ({})", code, name))));
            }
            let rb = top.expect(0xa0)?;
            let mut rb = rb.reader().sequence()?;
            if rb.oid()? != OID_OCSP_BASIC {
                return Ok(Err(err("unsupported OCSP response type")));
            }
            let basic = rb.octet_string()?;
            let mut b = Der::new(basic).sequence()?;
            let tbs_tlv: Tlv = b.read()?;
            let mut sa = b.sequence()?;
            let sig_alg = SignatureAlgorithm::parse(&mut sa)?;
            let signature = b.bit_string()?.to_vec();
            let mut certs = Vec::new();
            if let Some(c) = b.optional(0xa0)? {
                let mut list = c.reader().sequence()?;
                while !list.is_empty() {
                    certs.push(list.read()?.raw.to_vec());
                }
            }
            let mut tbs = tbs_tlv.reader();
            if tbs.peek_tag() == Some(0xa0) {
                tbs.read()?; // version
            }
            tbs.read()?; // responderID
            let produced_at = asn1::parse_time(&tbs.read()?)?;
            let mut list = tbs.sequence()?;
            let mut responses = Vec::new();
            while !list.is_empty() {
                responses.push(parse_single(list.sequence()?)?);
            }
            Ok(Ok(OcspResponse { der: der.to_vec(), produced_at, responses, tbs: tbs_tlv.raw.to_vec(), sig_alg, signature, certs }))
        })();
        match r {
            Ok(x) => x,
            Err(e) => Err(format!("malformed OCSP response: {}", e.0)),
        }
    }

    /// The response for `cert` issued by `issuer` (CertID match).
    pub fn find(&self, cert: &Certificate, issuer: &Certificate) -> Option<&SingleResponse> {
        self.responses.iter().find(|s| s.serial == cert.serial && s.issuer_name_hash == s.hash.hash(&issuer.subject_raw) && s.issuer_key_hash == key_hash(s.hash, issuer))
    }

    /// Validates the response for `cert` (issued by `issuer`) at `now`: responder signature
    /// (issuer, or a delegated responder certificate signed by the issuer with the
    /// OCSPSigning EKU), CertID, validity window. Returns the certificate status.
    pub fn validate(&self, cert: &Certificate, issuer: &Certificate, now: i64) -> Result<CertStatus, String> {
        let issuer_key = issuer.public_key.as_ref().ok_or("unsupported issuer key")?;
        let signed_by_issuer = self.sig_alg.verify(issuer_key, &self.tbs, &self.signature);
        if !signed_by_issuer {
            let delegated = self.certs.iter().filter_map(|d| Certificate::from_der(d).ok()).find(|r| {
                r.issuer_raw == issuer.subject_raw
                    && r.verify_signed_by(issuer_key)
                    && r.ext_key_usage.as_ref().is_some_and(|e| e.iter().any(|o| o == OID_EKU_OCSP_SIGNING))
                    && now >= r.not_before - CLOCK_SKEW_SECS
                    && now <= r.not_after + CLOCK_SKEW_SECS
                    && r.public_key.as_ref().is_some_and(|k| self.sig_alg.verify(k, &self.tbs, &self.signature))
            });
            if delegated.is_none() {
                return Err("OCSP response signature is not from the issuer or an authorized responder".into());
            }
        }
        let single = self.find(cert, issuer).ok_or("the OCSP response does not cover the certificate")?;
        if single.this_update > now + CLOCK_SKEW_SECS {
            return Err("OCSP response is not yet valid (thisUpdate in the future)".into());
        }
        if let Some(n) = single.next_update {
            if n < now - CLOCK_SKEW_SECS {
                return Err("OCSP response expired (nextUpdate in the past)".into());
            }
        } else if single.this_update < now - 7 * 86400 {
            return Err("OCSP response without nextUpdate is older than a week".into());
        }
        Ok(single.status.clone())
    }
}

/// GeneralizedTime (OCSP requires it for every time field).
fn gtime(unix: i64) -> Vec<u8> {
    let days = unix.div_euclid(86400);
    let secs = unix.rem_euclid(86400);
    let (y, m, d) = asn1::civil_from_days(days);
    let s = format!("{:04}{:02}{:02}{:02}{:02}{:02}Z", y, m, d, secs / 3600, secs / 60 % 60, secs % 60);
    asn1::tlv(asn1::TAG_GENERALIZED_TIME, s.as_bytes())
}

/// Builds a basic OCSP response for `cert` signed by `issuer_key` (the issuer itself).
pub fn build_response(cert: &Certificate, issuer: &Certificate, issuer_key: &PrivateKey, status: &CertStatus, this_update: i64, next_update: Option<i64>) -> Vec<u8> {
    let h = HashAlg::Sha1;
    let cert_id = asn1::seq(&[asn1::seq(&[asn1::oid(hash_oid(h)), asn1::tlv(asn1::TAG_NULL, &[])]), asn1::octet_string(&h.hash(&issuer.subject_raw)), asn1::octet_string(&key_hash(h, issuer)), asn1::uint(&cert.serial)]);
    let status_der = match status {
        CertStatus::Good => vec![0x80, 0],
        CertStatus::Revoked { time, reason } => {
            let mut inner = gtime(*time);
            if let Some(r) = reason {
                inner.extend(asn1::explicit(0, &asn1::tlv(0x0a, &[*r])));
            }
            asn1::tlv(0xa1, &inner)
        }
        CertStatus::Unknown => vec![0x82, 0],
    };
    let mut single = vec![cert_id, status_der, gtime(this_update)];
    if let Some(n) = next_update {
        single.push(asn1::explicit(0, &gtime(n)));
    }
    let responder_id = asn1::explicit(2, &asn1::octet_string(&key_hash(HashAlg::Sha1, issuer)));
    let tbs = asn1::seq(&[responder_id, gtime(this_update), asn1::seq(&[asn1::seq(&single)])]);
    let sig = crate::certgen::sign_with(issuer_key, &tbs);
    let basic = asn1::seq(&[tbs, crate::certgen::sig_alg_for(issuer_key), asn1::bit_string(&sig)]);
    asn1::seq(&[asn1::tlv(0x0a, &[0]), asn1::explicit(0, &asn1::seq(&[asn1::oid(OID_OCSP_BASIC), asn1::octet_string(&basic)]))])
}

/// Validates a stapled response for a verified chain (`chain[0]` = leaf; the issuer is
/// `chain[1]` or a matching trust anchor). Revoked / unknown statuses are errors.
pub fn check_stapled(der: &[u8], chain: &[Vec<u8>], roots: &crate::x509::RootStore, now: i64) -> Result<(), String> {
    let resp = OcspResponse::parse(der)?;
    let leaf = Certificate::from_der(chain.first().ok_or("empty chain")?).map_err(|e| e.to_string())?;
    let issuer = match chain.get(1).and_then(|d| Certificate::from_der(d).ok()) {
        Some(i) if i.subject_raw == leaf.issuer_raw => i,
        _ => roots.certificates().find(|r| r.subject_raw == leaf.issuer_raw).cloned().ok_or("issuer of the leaf certificate not found")?,
    };
    match resp.validate(&leaf, &issuer, now)? {
        CertStatus::Good => Ok(()),
        CertStatus::Revoked { .. } => Err("the server certificate is revoked (stapled OCSP response)".into()),
        CertStatus::Unknown => Err("the stapled OCSP response reports an unknown certificate status".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certgen::{issue, CertParams};
    use crate::crypto::ec::EcCurve;

    #[test]
    fn build_parse_validate() {
        let ca = issue(&CertParams::ca("OCSP CA"), PrivateKey::generate_ec(EcCurve::P256), None);
        let leaf = issue(&CertParams::server(&["localhost"]), PrivateKey::generate_ec(EcCurve::P256), Some(&ca));
        let (cac, leafc) = (Certificate::from_der(&ca.cert_der).unwrap(), Certificate::from_der(&leaf.cert_der).unwrap());
        let now = crate::x509::now_unix();
        let good = build_response(&leafc, &cac, &ca.key, &CertStatus::Good, now - 60, Some(now + 3600));
        let r = OcspResponse::parse(&good).unwrap();
        assert_eq!(r.validate(&leafc, &cac, now), Ok(CertStatus::Good));
        assert!(r.validate(&leafc, &cac, now + 7200).is_err(), "expired");
        // another certificate is not covered
        let other = issue(&CertParams::server(&["other"]), PrivateKey::generate_ec(EcCurve::P256), Some(&ca));
        assert!(r.validate(&Certificate::from_der(&other.cert_der).unwrap(), &cac, now).is_err());
        // signed by a stranger
        let stranger = issue(&CertParams::ca("Stranger"), PrivateKey::generate_ec(EcCurve::P256), None);
        let forged = build_response(&leafc, &cac, &stranger.key, &CertStatus::Good, now - 60, Some(now + 3600));
        assert!(OcspResponse::parse(&forged).unwrap().validate(&leafc, &cac, now).is_err());
        let revoked = build_response(&leafc, &cac, &ca.key, &CertStatus::Revoked { time: now - 10, reason: Some(1) }, now - 60, Some(now + 3600));
        assert_eq!(OcspResponse::parse(&revoked).unwrap().validate(&leafc, &cac, now), Ok(CertStatus::Revoked { time: now - 10, reason: Some(1) }));
        let mut roots = crate::x509::RootStore::empty();
        roots.add_der(&ca.cert_der).unwrap();
        assert!(check_stapled(&good, &[leaf.cert_der.clone()], &roots, now).is_ok());
        assert!(check_stapled(&revoked, &[leaf.cert_der.clone()], &roots, now).is_err());
        // a non-successful responder status
        assert!(OcspResponse::parse(&asn1::seq(&[asn1::tlv(0x0a, &[3])])).unwrap_err().contains("tryLater"));
    }
}
