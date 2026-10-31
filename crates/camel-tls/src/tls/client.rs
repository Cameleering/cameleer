//! Client handshake (TLS 1.3 with TLS 1.2 fallback).

use super::codec::{self, find_ext, handshake_msg, parse_extensions, put_u16, put_vec16, put_vec8, Reader};
use super::config::TlsClientConfig;
use super::conn::Conn;
use super::keyschedule::{cipher13, ciphers12, finished12, master_secret12, message_hash, KeySchedule13};
use super::{
    alert, ext, hs, sigscheme, verify_message, AuthKind, CipherSuite, KeyShare, NamedGroup, TlsError, TlsResult, TlsVersion,
};
use crate::crypto::hash::{Digest, Sha256};
use crate::crypto::rng;
use crate::keys::PublicKey;
use crate::util::ct_eq;
use crate::x509::{self, Certificate};
use std::io::{Read, Write};
use std::net::IpAddr;

pub(crate) const DOWNGRADE_TLS12: [u8; 8] = *b"DOWNGRD\x01";
pub(crate) const DOWNGRADE_TLS11: [u8; 8] = *b"DOWNGRD\x00";

pub(crate) fn hrr_random() -> Vec<u8> {
    Sha256::digest(b"HelloRetryRequest")
}

pub(crate) struct ServerHello {
    pub legacy_version: u16,
    pub random: Vec<u8>,
    pub session_id: Vec<u8>,
    pub suite: u16,
    pub compression: u8,
    pub exts: Vec<(u16, Vec<u8>)>,
}

impl ServerHello {
    fn parse(body: &[u8]) -> TlsResult<ServerHello> {
        let mut r = Reader::new(body, "ServerHello");
        let legacy_version = r.u16()?;
        let random = r.take(32)?.to_vec();
        let session_id = r.vec8()?.to_vec();
        let suite = r.u16()?;
        let compression = r.u8()?;
        let exts = if r.is_empty() { Vec::new() } else { parse_extensions(r.vec16()?)? };
        r.finish()?;
        Ok(ServerHello { legacy_version, random, session_id, suite, compression, exts })
    }
}

struct HelloParams<'a> {
    random: &'a [u8],
    session_id: &'a [u8],
    suites: &'a [CipherSuite],
    share: &'a KeyShare,
    sni: Option<&'a str>,
    cookie: Option<&'a [u8]>,
    offer13: bool,
    offer12: bool,
}

fn client_hello(cfg: &TlsClientConfig, p: &HelloParams) -> Vec<u8> {
    let mut body = Vec::new();
    put_u16(&mut body, 0x0303);
    body.extend_from_slice(p.random);
    put_vec8(&mut body, p.session_id);
    let ids: Vec<u16> = p.suites.iter().map(|s| s.id()).collect();
    body.extend(codec::u16_list(&ids));
    put_vec8(&mut body, &[0]);
    let mut exts: Vec<(u16, Vec<u8>)> = Vec::new();
    if let Some(name) = p.sni {
        let mut entry = vec![0u8];
        put_vec16(&mut entry, name.as_bytes());
        let mut d = Vec::new();
        put_vec16(&mut d, &entry);
        exts.push((ext::SERVER_NAME, d));
    }
    let groups: Vec<u16> = cfg.groups.iter().map(|g| g.id()).collect();
    exts.push((ext::SUPPORTED_GROUPS, codec::u16_list(&groups)));
    if p.offer12 {
        exts.push((ext::EC_POINT_FORMATS, vec![1, 0]));
    }
    exts.push((ext::SIGNATURE_ALGORITHMS, codec::u16_list(&sigscheme::SUPPORTED)));
    if !cfg.alpn.is_empty() {
        exts.push((ext::ALPN, codec::alpn_list(&cfg.alpn)));
    }
    if p.offer12 {
        exts.push((ext::EXTENDED_MASTER_SECRET, Vec::new()));
        exts.push((ext::RENEGOTIATION_INFO, vec![0]));
    }
    if p.offer13 {
        let mut versions = vec![0x03, 0x04];
        if p.offer12 {
            versions.extend_from_slice(&[0x03, 0x03]);
        }
        let mut d = Vec::new();
        put_vec8(&mut d, &versions);
        exts.push((ext::SUPPORTED_VERSIONS, d));
        if let Some(c) = p.cookie {
            let mut d = Vec::new();
            put_vec16(&mut d, c);
            exts.push((ext::COOKIE, d));
        }
        let mut entry = Vec::new();
        put_u16(&mut entry, p.share.group.id());
        put_vec16(&mut entry, &p.share.public);
        let mut d = Vec::new();
        put_vec16(&mut d, &entry);
        exts.push((ext::KEY_SHARE, d));
    }
    body.extend(codec::extensions(&exts));
    handshake_msg(hs::CLIENT_HELLO, &body)
}

fn check_alpn(cfg: &TlsClientConfig, exts: &[(u16, Vec<u8>)]) -> TlsResult<Option<Vec<u8>>> {
    match find_ext(exts, ext::ALPN) {
        None => Ok(None),
        Some(d) => {
            let list = codec::parse_alpn_list(d)?;
            if list.len() != 1 || !cfg.alpn.contains(&list[0]) {
                return Err(TlsError::proto(alert::ILLEGAL_PARAMETER, "server selected an ALPN protocol we did not offer"));
            }
            Ok(Some(list[0].clone()))
        }
    }
}

impl<S: Read + Write> Conn<S> {
    pub(crate) fn client_handshake(&mut self, cfg: &TlsClientConfig, server_name: &str) -> TlsResult<()> {
        let offer13 = cfg.max_version >= TlsVersion::Tls13;
        let offer12 = cfg.min_version <= TlsVersion::Tls12;
        let suites: Vec<CipherSuite> =
            cfg.suites.iter().copied().filter(|s| if s.is_tls13() { offer13 } else { offer12 }).collect();
        if suites.is_empty() || cfg.groups.is_empty() {
            return Err(TlsError::Config("no usable cipher suites or groups for the configured versions".into()));
        }
        let host = server_name.trim_end_matches('.');
        let sni = if host.is_empty() || host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>().is_ok() {
            None
        } else {
            Some(host)
        };
        let random = rng::random_bytes(32);
        let session_id = if offer13 { rng::random_bytes(32) } else { Vec::new() };
        let mut share = KeyShare::generate(cfg.groups[0]);
        let ch1 = client_hello(
            cfg,
            &HelloParams { random: &random, session_id: &session_id, suites: &suites, share: &share, sni, cookie: None, offer13, offer12 },
        );
        self.send_hs(&ch1)?;
        self.flush()?;

        let mut msg = self.expect(hs::SERVER_HELLO)?;
        let mut sh = ServerHello::parse(&msg[4..])?;
        let mut sent_ccs = false;
        let mut hrr_suite = None;
        if sh.random == hrr_random() {
            // HelloRetryRequest
            let bad = |m: &str| TlsError::proto(alert::ILLEGAL_PARAMETER, m.to_string());
            if !offer13 || find_ext(&sh.exts, ext::SUPPORTED_VERSIONS) != Some(&[0x03, 0x04][..]) {
                return Err(bad("HelloRetryRequest without TLS 1.3"));
            }
            let suite = CipherSuite::from_id(sh.suite).filter(|s| s.is_tls13() && suites.contains(s)).ok_or_else(|| bad("bad HRR cipher suite"))?;
            if sh.session_id != session_id {
                return Err(bad("HRR session id mismatch"));
            }
            let ks = find_ext(&sh.exts, ext::KEY_SHARE).ok_or_else(|| bad("HRR without key_share"))?;
            if ks.len() != 2 {
                return Err(TlsError::decode("HRR key_share"));
            }
            let gid = u16::from_be_bytes([ks[0], ks[1]]);
            let group = NamedGroup::from_id(gid).filter(|g| cfg.groups.contains(g) && *g != share.group).ok_or_else(|| bad("bad HRR group"))?;
            let cookie = match find_ext(&sh.exts, ext::COOKIE) {
                Some(c) => {
                    let mut r = Reader::new(c, "cookie");
                    let v = r.vec16()?.to_vec();
                    r.finish()?;
                    Some(v)
                }
                None => None,
            };
            let h = suite.hash();
            self.transcript = message_hash(h, &ch1);
            self.transcript.extend_from_slice(&msg);
            share = KeyShare::generate(group);
            let ch2 = client_hello(
                cfg,
                &HelloParams {
                    random: &random,
                    session_id: &session_id,
                    suites: &suites,
                    share: &share,
                    sni,
                    cookie: cookie.as_deref(),
                    offer13,
                    offer12,
                },
            );
            self.send_ccs();
            sent_ccs = true;
            self.send_hs(&ch2)?;
            self.flush()?;
            msg = self.expect(hs::SERVER_HELLO)?;
            sh = ServerHello::parse(&msg[4..])?;
            if sh.random == hrr_random() {
                return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "second HelloRetryRequest"));
            }
            hrr_suite = Some(suite);
        }
        self.transcript.extend_from_slice(&msg);

        if let Some(sv) = find_ext(&sh.exts, ext::SUPPORTED_VERSIONS) {
            if sv != [0x03, 0x04] || !offer13 {
                return Err(TlsError::proto(alert::ILLEGAL_PARAMETER, "server selected an unsupported version"));
            }
            self.client13(cfg, host, &sh, &share, &session_id, &suites, hrr_suite, sent_ccs)
        } else {
            if hrr_suite.is_some() {
                return Err(TlsError::proto(alert::ILLEGAL_PARAMETER, "TLS 1.2 ServerHello after HelloRetryRequest"));
            }
            if sh.legacy_version != 0x0303 || !offer12 {
                return Err(TlsError::proto(alert::PROTOCOL_VERSION, format!("server offered unsupported version {:#06x}", sh.legacy_version)));
            }
            if offer13 && (sh.random[24..] == DOWNGRADE_TLS12 || sh.random[24..] == DOWNGRADE_TLS11) {
                return Err(TlsError::proto(alert::ILLEGAL_PARAMETER, "TLS downgrade detected"));
            }
            self.client12(cfg, host, &sh, &random, &suites)
        }
    }

    fn check_server_certs(&mut self, cfg: &TlsClientConfig, host: &str, certs: Vec<Vec<u8>>) -> TlsResult<PublicKey> {
        if certs.is_empty() {
            return Err(TlsError::proto(alert::DECODE_ERROR, "server sent an empty certificate list"));
        }
        if cfg.verify {
            let name = if host.is_empty() || !cfg.verify_hostname { None } else { Some(host) };
            x509::verify_chain(&certs, name, &cfg.roots, x509::now_unix(), x509::OID_EKU_SERVER_AUTH)?;
        }
        let leaf = Certificate::from_der(&certs[0]).map_err(|e| TlsError::proto(alert::BAD_CERTIFICATE, e.to_string()))?;
        self.peer_certs = certs;
        leaf.public_key.ok_or_else(|| TlsError::proto(alert::UNSUPPORTED_CERTIFICATE, "unsupported server key type"))
    }

    #[allow(clippy::too_many_arguments)]
    fn client13(
        &mut self,
        cfg: &TlsClientConfig,
        host: &str,
        sh: &ServerHello,
        share: &KeyShare,
        session_id: &[u8],
        suites: &[CipherSuite],
        hrr_suite: Option<CipherSuite>,
        sent_ccs: bool,
    ) -> TlsResult<()> {
        let illegal = |m: &str| TlsError::proto(alert::ILLEGAL_PARAMETER, m.to_string());
        let suite = CipherSuite::from_id(sh.suite).filter(|s| s.is_tls13() && suites.contains(s)).ok_or_else(|| illegal("bad cipher suite"))?;
        if hrr_suite.is_some_and(|s| s != suite) {
            return Err(illegal("cipher suite changed after HelloRetryRequest"));
        }
        if sh.session_id != session_id || sh.compression != 0 {
            return Err(illegal("bad session id echo or compression"));
        }
        let ks = find_ext(&sh.exts, ext::KEY_SHARE).ok_or_else(|| TlsError::proto(alert::MISSING_EXTENSION, "missing key_share"))?;
        let mut r = Reader::new(ks, "key_share");
        let gid = r.u16()?;
        let peer = r.vec16()?;
        r.finish()?;
        if gid != share.group.id() {
            return Err(illegal("key_share group mismatch"));
        }
        let shared = share.agree(peer)?;
        let h = suite.hash();
        let sched = KeySchedule13::new(suite, &shared, &h.hash(&self.transcript));
        self.version = Some(TlsVersion::Tls13);
        self.suite = Some(suite);
        self.group = Some(share.group);
        self.set_read_cipher(cipher13(suite, &sched.server_hs))?;
        self.set_write_cipher(cipher13(suite, &sched.client_hs));

        // EncryptedExtensions
        let ee = self.expect(hs::ENCRYPTED_EXTENSIONS)?;
        let mut r = Reader::new(&ee[4..], "EncryptedExtensions");
        let exts = parse_extensions(r.vec16()?)?;
        r.finish()?;
        self.alpn = check_alpn(cfg, &exts)?;
        self.transcript.extend_from_slice(&ee);

        let mut msg = self.read_hs()?;
        let mut cert_request: Option<Vec<u8>> = None;
        if msg[0] == hs::CERTIFICATE_REQUEST {
            let mut r = Reader::new(&msg[4..], "CertificateRequest");
            cert_request = Some(r.vec8()?.to_vec());
            self.transcript.extend_from_slice(&msg);
            msg = self.read_hs()?;
        }
        if msg[0] != hs::CERTIFICATE {
            return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "expected Certificate"));
        }
        let mut r = Reader::new(&msg[4..], "Certificate");
        let _ctx = r.vec8()?;
        let list = r.vec24()?;
        r.finish()?;
        let mut lr = Reader::new(list, "Certificate");
        let mut certs = Vec::new();
        while !lr.is_empty() {
            certs.push(lr.vec24()?.to_vec());
            let _exts = lr.vec16()?;
        }
        self.transcript.extend_from_slice(&msg);
        let leaf_key = self.check_server_certs(cfg, host, certs)?;

        let cv = self.expect(hs::CERTIFICATE_VERIFY)?;
        let mut r = Reader::new(&cv[4..], "CertificateVerify");
        let scheme = r.u16()?;
        let sig = r.vec16()?;
        r.finish()?;
        let mut signed = vec![0x20u8; 64];
        signed.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
        signed.extend(h.hash(&self.transcript));
        verify_message(&leaf_key, scheme, &signed, sig, true)?;
        self.transcript.extend_from_slice(&cv);

        let fin = self.expect(hs::FINISHED)?;
        let expected = sched.finished_mac(&sched.server_hs, &h.hash(&self.transcript));
        if !ct_eq(&fin[4..], &expected) {
            return Err(TlsError::proto(alert::DECRYPT_ERROR, "server Finished verification failed"));
        }
        self.transcript.extend_from_slice(&fin);
        let (c_ap, s_ap) = sched.app_secrets(&h.hash(&self.transcript));

        if !sent_ccs {
            self.send_ccs();
        }
        if let Some(ctx) = cert_request {
            let mut body = Vec::new();
            put_vec8(&mut body, &ctx);
            codec::put_vec24(&mut body, &[]);
            self.send_hs(&handshake_msg(hs::CERTIFICATE, &body))?;
        }
        let cfin = sched.finished_mac(&sched.client_hs, &h.hash(&self.transcript));
        self.send_hs(&handshake_msg(hs::FINISHED, &cfin))?;
        self.set_write_cipher(cipher13(suite, &c_ap));
        self.set_read_cipher(cipher13(suite, &s_ap))?;
        self.read_secret = s_ap;
        self.write_secret = c_ap;
        self.handshaking = false;
        self.flush()
    }

    fn client12(&mut self, cfg: &TlsClientConfig, host: &str, sh: &ServerHello, client_random: &[u8], suites: &[CipherSuite]) -> TlsResult<()> {
        let illegal = |m: &str| TlsError::proto(alert::ILLEGAL_PARAMETER, m.to_string());
        let suite = CipherSuite::from_id(sh.suite).filter(|s| !s.is_tls13() && suites.contains(s)).ok_or_else(|| illegal("bad cipher suite"))?;
        if sh.compression != 0 {
            return Err(illegal("compression not supported"));
        }
        let ems = find_ext(&sh.exts, ext::EXTENDED_MASTER_SECRET).is_some();
        if let Some(ri) = find_ext(&sh.exts, ext::RENEGOTIATION_INFO) {
            if ri != [0] {
                return Err(TlsError::proto(alert::HANDSHAKE_FAILURE, "bad renegotiation_info"));
            }
        }
        self.alpn = check_alpn(cfg, &sh.exts)?;
        self.version = Some(TlsVersion::Tls12);
        self.suite = Some(suite);
        let h = suite.hash();
        let server_random = sh.random.clone();

        let cm = self.expect(hs::CERTIFICATE)?;
        let mut r = Reader::new(&cm[4..], "Certificate");
        let list = r.vec24()?;
        r.finish()?;
        let mut lr = Reader::new(list, "Certificate");
        let mut certs = Vec::new();
        while !lr.is_empty() {
            certs.push(lr.vec24()?.to_vec());
        }
        self.transcript.extend_from_slice(&cm);
        let leaf_key = self.check_server_certs(cfg, host, certs)?;
        let key_ok = matches!(
            (suite.auth(), &leaf_key),
            (Some(AuthKind::Ecdsa), PublicKey::Ec { .. }) | (Some(AuthKind::Rsa), PublicKey::Rsa(_))
        );
        if !key_ok {
            return Err(TlsError::proto(alert::UNSUPPORTED_CERTIFICATE, "certificate key does not match cipher suite"));
        }

        let ske = self.expect(hs::SERVER_KEY_EXCHANGE)?;
        let body = &ske[4..];
        let mut r = Reader::new(body, "ServerKeyExchange");
        if r.u8()? != 3 {
            return Err(illegal("only named curves are supported"));
        }
        let gid = r.u16()?;
        let point = r.vec8()?;
        let params_len = 4 + point.len();
        let scheme = r.u16()?;
        let sig = r.vec16()?;
        r.finish()?;
        let group = NamedGroup::from_id(gid).filter(|g| cfg.groups.contains(g)).ok_or_else(|| illegal("server chose an unsupported curve"))?;
        let mut signed = client_random.to_vec();
        signed.extend_from_slice(&server_random);
        signed.extend_from_slice(&body[..params_len]);
        verify_message(&leaf_key, scheme, &signed, sig, false)?;
        self.transcript.extend_from_slice(&ske);

        let mut msg = self.read_hs()?;
        let mut cert_requested = false;
        if msg[0] == hs::CERTIFICATE_REQUEST {
            cert_requested = true;
            self.transcript.extend_from_slice(&msg);
            msg = self.read_hs()?;
        }
        if msg[0] != hs::SERVER_HELLO_DONE || msg.len() != 4 {
            return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "expected ServerHelloDone"));
        }
        self.transcript.extend_from_slice(&msg);

        if cert_requested {
            self.send_hs(&handshake_msg(hs::CERTIFICATE, &[0, 0, 0]))?;
        }
        let share = KeyShare::generate(group);
        let pms = share.agree(point)?;
        self.group = Some(group);
        let mut cke = Vec::new();
        put_vec8(&mut cke, &share.public);
        self.send_hs(&handshake_msg(hs::CLIENT_KEY_EXCHANGE, &cke))?;
        let session_hash = h.hash(&self.transcript);
        let master = master_secret12(h, &pms, client_random, &server_random, if ems { Some(&session_hash) } else { None });
        let (cw, sw) = ciphers12(suite, &master, client_random, &server_random);
        self.send_ccs();
        self.set_write_cipher(cw);
        let verify = finished12(h, &master, true, &h.hash(&self.transcript));
        self.send_hs(&handshake_msg(hs::FINISHED, &verify))?;
        self.flush()?;

        self.pending_read12 = Some(sw);
        let fin = self.expect(hs::FINISHED)?;
        if self.pending_read12.is_some() {
            return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "Finished before ChangeCipherSpec"));
        }
        let expected = finished12(h, &master, false, &h.hash(&self.transcript));
        if !ct_eq(&fin[4..], &expected) {
            return Err(TlsError::proto(alert::DECRYPT_ERROR, "server Finished verification failed"));
        }
        self.transcript.extend_from_slice(&fin);
        self.handshaking = false;
        Ok(())
    }
}
