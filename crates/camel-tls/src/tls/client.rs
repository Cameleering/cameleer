//! Client handshake (TLS 1.3 with TLS 1.2 fallback), with client certificates and
//! session resumption (TLS 1.3 PSK tickets, TLS 1.2 session tickets and IDs).

use super::codec::{self, find_ext, handshake_msg, parse_extensions, parse_u16_list, put_u16, put_vec16, put_vec8, Reader};
use super::config::TlsClientConfig;
use super::conn::{Conn, TicketSink};
use super::keyschedule::{cipher13, ciphers12, finished12, master_secret12, message_hash, psk_binder, KeySchedule13};
use super::session::{now_ms, ClientSession};
use super::{
    alert, choose_scheme, ext, hs, sign_message, sigscheme, verify_message, AuthKind, CipherSuite, KeyShare, NamedGroup, TlsError,
    TlsResult, TlsVersion,
};
use crate::crypto::hash::{Digest, Sha256};
use crate::crypto::rng;
use crate::keys::{PrivateKey, PublicKey};
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
    /// TLS 1.3 resumption PSK (ticket) to offer.
    psk: Option<&'a ClientSession>,
    /// TLS 1.2: `Some(ticket)` sends the session_ticket extension (empty = support only).
    ticket12: Option<&'a [u8]>,
}

/// Builds a ClientHello. With a PSK the binder is left zeroed; see [`fill_binder`].
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
    if cfg.ocsp != crate::tls::config::OcspMode::Off {
        // status_type ocsp, empty responder_id_list and request_extensions
        exts.push((ext::STATUS_REQUEST, vec![1, 0, 0, 0, 0]));
    }
    if !cfg.alpn.is_empty() {
        exts.push((ext::ALPN, codec::alpn_list(&cfg.alpn)));
    }
    if p.offer12 {
        exts.push((ext::EXTENDED_MASTER_SECRET, Vec::new()));
        exts.push((ext::RENEGOTIATION_INFO, vec![0]));
        if let Some(t) = p.ticket12 {
            exts.push((ext::SESSION_TICKET, t.to_vec()));
        }
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
        if let Some(s) = p.psk {
            exts.push((ext::PSK_KEY_EXCHANGE_MODES, vec![1, 1])); // psk_dhe_ke
            let age = (now_ms().saturating_sub(s.received_ms) as u32).wrapping_add(s.age_add);
            let mut ident = Vec::new();
            put_vec16(&mut ident, &s.ticket);
            ident.extend_from_slice(&age.to_be_bytes());
            let mut d = Vec::new();
            put_vec16(&mut d, &ident);
            let hl = s.suite.hash().output_len();
            let mut binder = Vec::new();
            put_vec8(&mut binder, &vec![0u8; hl]);
            put_vec16(&mut d, &binder);
            exts.push((ext::PRE_SHARED_KEY, d)); // must be the last extension
        }
    }
    body.extend(codec::extensions(&exts));
    handshake_msg(hs::CLIENT_HELLO, &body)
}

/// Computes the PSK binder of `msg` (a ClientHello built with a zeroed binder) over
/// `prefix` (the transcript before it) and writes it in place.
fn fill_binder(msg: &mut [u8], prefix: &[u8], s: &ClientSession) {
    let h = s.suite.hash();
    let hl = h.output_len();
    let cut = msg.len() - (2 + 1 + hl);
    let mut t = prefix.to_vec();
    t.extend_from_slice(&msg[..cut]);
    let binder = psk_binder(h, &s.secret, &h.hash(&t));
    let n = msg.len();
    msg[n - hl..].copy_from_slice(&binder);
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

/// TLS 1.2 `ClientCertificateType` for our key.
fn cert_type_for(key: &PrivateKey) -> u8 {
    match key {
        PrivateKey::Rsa(_) => 1,
        _ => 64,
    }
}

impl<S: Read + Write> Conn<S> {
    pub(crate) fn client_handshake(&mut self, cfg: &TlsClientConfig, server_name: &str, cache_key: Option<&str>) -> TlsResult<()> {
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
        // resumption
        let cache_key = cache_key.unwrap_or(host).to_string();
        let cached = cfg
            .session_cache
            .as_ref()
            .and_then(|c| c.get(&cache_key))
            .filter(|s| s.server_name == host && suites.contains(&s.suite));
        let psk13 = cached.clone().filter(|s| s.version == TlsVersion::Tls13);
        let sess12 = cached.filter(|s| s.version == TlsVersion::Tls12);
        let ticket12: Option<Vec<u8>> = if offer12 && cfg.session_cache.is_some() {
            Some(sess12.as_ref().map(|s| s.ticket.clone()).unwrap_or_default())
        } else {
            None
        };

        let random = rng::random_bytes(32);
        let session_id = match &sess12 {
            Some(s) if s.ticket.is_empty() => s.session_id.clone(),
            Some(_) => rng::random_bytes(32),
            None if offer13 => rng::random_bytes(32),
            None => Vec::new(),
        };
        let mut share = KeyShare::generate(cfg.groups[0]);
        let mut ch1 = client_hello(
            cfg,
            &HelloParams {
                random: &random,
                session_id: &session_id,
                suites: &suites,
                share: &share,
                sni,
                cookie: None,
                offer13,
                offer12,
                psk: psk13.as_ref(),
                ticket12: ticket12.as_deref(),
            },
        );
        if let Some(p) = &psk13 {
            fill_binder(&mut ch1, &[], p);
        }
        self.send_hs(&ch1)?;
        self.flush()?;

        let mut msg = self.expect(hs::SERVER_HELLO)?;
        let mut sh = ServerHello::parse(&msg[4..])?;
        let mut sent_ccs = false;
        let mut hrr_suite = None;
        let mut offered_psk = psk13.clone();
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
            // the PSK can only be kept if its hash matches the selected suite
            offered_psk = offered_psk.filter(|p| p.suite.hash() == h);
            let mut ch2 = client_hello(
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
                    psk: offered_psk.as_ref(),
                    ticket12: ticket12.as_deref(),
                },
            );
            if let Some(p) = &offered_psk {
                let prefix = self.transcript.clone();
                fill_binder(&mut ch2, &prefix, p);
            }
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
            let p = Client13 { host, cache_key: &cache_key, share: &share, session_id: &session_id, suites: &suites, hrr_suite, sent_ccs };
            self.client13(cfg, &sh, &p, offered_psk.as_ref())
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
            if find_ext(&sh.exts, ext::PRE_SHARED_KEY).is_some() || find_ext(&sh.exts, ext::KEY_SHARE).is_some() {
                return Err(TlsError::proto(alert::UNSUPPORTED_EXTENSION, "TLS 1.3 extension in a TLS 1.2 ServerHello"));
            }
            self.client12(cfg, host, &cache_key, &sh, &random, &suites, sess12.as_ref(), &session_id)
        }
    }

    fn check_server_certs(&mut self, cfg: &TlsClientConfig, host: &str, certs: Vec<Vec<u8>>) -> TlsResult<PublicKey> {
        if certs.is_empty() {
            return Err(TlsError::proto(alert::DECODE_ERROR, "server sent an empty certificate list"));
        }
        if cfg.verify {
            let name = if host.is_empty() || !cfg.verify_hostname { None } else { Some(host) };
            x509::verify_chain_with_crls(&certs, name, &cfg.roots, x509::now_unix(), x509::OID_EKU_SERVER_AUTH, &cfg.crls)?;
        }
        let leaf = Certificate::from_der(&certs[0]).map_err(|e| TlsError::proto(alert::BAD_CERTIFICATE, e.to_string()))?;
        self.peer_certs = certs;
        leaf.public_key.ok_or_else(|| TlsError::proto(alert::UNSUPPORTED_CERTIFICATE, "unsupported server key type"))
    }

    /// Validates a stapled OCSP response (CertificateStatus body) for the verified chain.
    fn check_ocsp(&mut self, cfg: &TlsClientConfig, status: Option<&[u8]>) -> TlsResult<()> {
        use crate::tls::config::OcspMode;
        let bad = |m: String| TlsError::proto(113, m); // bad_certificate_status_response
        let resp = match status {
            Some(s) => {
                let mut r = Reader::new(s, "CertificateStatus");
                if r.u8()? != 1 {
                    return Err(bad("unsupported certificate status type".into()));
                }
                let d = r.vec24()?.to_vec();
                r.finish()?;
                Some(d)
            }
            None => None,
        };
        match (cfg.ocsp, resp) {
            (OcspMode::Off, _) => Ok(()),
            (OcspMode::Require, None) => Err(bad("the server did not staple an OCSP response".into())),
            (_, None) => Ok(()),
            (_, Some(d)) => {
                if cfg.verify {
                    crate::ocsp::check_stapled(&d, &self.peer_certs, &cfg.roots, x509::now_unix()).map_err(bad)?;
                }
                self.peer_ocsp = Some(d);
                Ok(())
            }
        }
    }

    fn client13(&mut self, cfg: &TlsClientConfig, sh: &ServerHello, p: &Client13, psk: Option<&ClientSession>) -> TlsResult<()> {
        let illegal = |m: &str| TlsError::proto(alert::ILLEGAL_PARAMETER, m.to_string());
        let suite =
            CipherSuite::from_id(sh.suite).filter(|s| s.is_tls13() && p.suites.contains(s)).ok_or_else(|| illegal("bad cipher suite"))?;
        if p.hrr_suite.is_some_and(|s| s != suite) {
            return Err(illegal("cipher suite changed after HelloRetryRequest"));
        }
        if sh.session_id != p.session_id || sh.compression != 0 {
            return Err(illegal("bad session id echo or compression"));
        }
        let ks = find_ext(&sh.exts, ext::KEY_SHARE).ok_or_else(|| TlsError::proto(alert::MISSING_EXTENSION, "missing key_share"))?;
        let mut r = Reader::new(ks, "key_share");
        let gid = r.u16()?;
        let peer = r.vec16()?;
        r.finish()?;
        if gid != p.share.group.id() {
            return Err(illegal("key_share group mismatch"));
        }
        // PSK accepted?
        let resumed_psk = match find_ext(&sh.exts, ext::PRE_SHARED_KEY) {
            Some(d) => {
                let s = psk.ok_or_else(|| TlsError::proto(alert::UNSUPPORTED_EXTENSION, "pre_shared_key without an offered PSK"))?;
                if d != [0, 0] {
                    return Err(illegal("server selected an unknown PSK identity"));
                }
                if s.suite.hash() != suite.hash() {
                    return Err(illegal("PSK hash does not match the selected cipher suite"));
                }
                Some(s)
            }
            None => None,
        };
        let shared = p.share.agree(peer)?;
        let h = suite.hash();
        let sched = KeySchedule13::new_psk(suite, resumed_psk.map(|s| s.secret.as_slice()), &shared, &h.hash(&self.transcript));
        self.version = Some(TlsVersion::Tls13);
        self.suite = Some(suite);
        self.group = Some(p.share.group);
        self.set_read_cipher(cipher13(suite, &sched.server_hs))?;
        self.set_write_cipher(cipher13(suite, &sched.client_hs));

        // EncryptedExtensions
        let ee = self.expect(hs::ENCRYPTED_EXTENSIONS)?;
        let mut r = Reader::new(&ee[4..], "EncryptedExtensions");
        let exts = parse_extensions(r.vec16()?)?;
        r.finish()?;
        self.alpn = check_alpn(cfg, &exts)?;
        self.transcript.extend_from_slice(&ee);

        let mut cert_request: Option<(Vec<u8>, Vec<u16>)> = None;
        if let Some(s) = resumed_psk {
            self.resumed = true;
            self.peer_certs = s.peer_certs.clone();
        } else {
            let mut msg = self.read_hs()?;
            if msg[0] == hs::CERTIFICATE_REQUEST {
                let mut r = Reader::new(&msg[4..], "CertificateRequest");
                let ctx = r.vec8()?.to_vec();
                let exts = parse_extensions(r.vec16()?)?;
                r.finish()?;
                let algs = find_ext(&exts, ext::SIGNATURE_ALGORITHMS)
                    .ok_or_else(|| TlsError::proto(alert::MISSING_EXTENSION, "CertificateRequest without signature_algorithms"))?;
                cert_request = Some((ctx, parse_u16_list(algs, "signature_algorithms")?));
                self.transcript.extend_from_slice(&msg);
                msg = self.read_hs()?;
            }
            if msg[0] != hs::CERTIFICATE {
                return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "expected Certificate"));
            }
            let (certs, leaf_exts) = parse_certificate13_exts(&msg)?;
            self.transcript.extend_from_slice(&msg);
            let leaf_key = self.check_server_certs(cfg, p.host, certs)?;
            let status = find_ext(&leaf_exts, ext::STATUS_REQUEST).map(|s| s.to_vec());
            self.check_ocsp(cfg, status.as_deref())?;

            let cv = self.expect(hs::CERTIFICATE_VERIFY)?;
            let mut r = Reader::new(&cv[4..], "CertificateVerify");
            let scheme = r.u16()?;
            let sig = r.vec16()?;
            r.finish()?;
            let signed = cv_content(true, &h.hash(&self.transcript));
            verify_message(&leaf_key, scheme, &signed, sig, true)?;
            self.transcript.extend_from_slice(&cv);
        }

        let fin = self.expect(hs::FINISHED)?;
        let expected = sched.finished_mac(&sched.server_hs, &h.hash(&self.transcript));
        if !ct_eq(&fin[4..], &expected) {
            return Err(TlsError::proto(alert::DECRYPT_ERROR, "server Finished verification failed"));
        }
        self.transcript.extend_from_slice(&fin);
        let (c_ap, s_ap) = sched.app_secrets(&h.hash(&self.transcript));

        if !p.sent_ccs {
            self.send_ccs();
        }
        if let Some((ctx, algs)) = cert_request {
            let chosen = cfg.client_cert.as_ref().and_then(|ck| choose_scheme(&ck.key, &algs, true).map(|s| (ck.clone(), s)));
            let mut body = Vec::new();
            put_vec8(&mut body, &ctx);
            let mut list = Vec::new();
            if let Some((ck, _)) = &chosen {
                for c in &ck.chain {
                    codec::put_vec24(&mut list, c);
                    put_u16(&mut list, 0);
                }
            }
            codec::put_vec24(&mut body, &list);
            self.send_hs(&handshake_msg(hs::CERTIFICATE, &body))?;
            if let Some((ck, scheme)) = chosen {
                let signed = cv_content(false, &h.hash(&self.transcript));
                let sig = sign_message(&ck.key, scheme, &signed)?;
                let mut body = Vec::new();
                put_u16(&mut body, scheme);
                put_vec16(&mut body, &sig);
                self.send_hs(&handshake_msg(hs::CERTIFICATE_VERIFY, &body))?;
            }
        }
        let cfin = sched.finished_mac(&sched.client_hs, &h.hash(&self.transcript));
        self.send_hs(&handshake_msg(hs::FINISHED, &cfin))?;
        if let Some(cache) = &cfg.session_cache {
            self.ticket_sink = Some(TicketSink {
                cache: cache.clone(),
                key: p.cache_key.to_string(),
                resumption_master: sched.resumption_master(&h.hash(&self.transcript)),
                suite,
                server_name: p.host.to_string(),
                peer_certs: self.peer_certs.clone(),
            });
        }
        self.set_write_cipher(cipher13(suite, &c_ap));
        self.set_read_cipher(cipher13(suite, &s_ap))?;
        self.read_secret = s_ap;
        self.write_secret = c_ap;
        self.handshaking = false;
        self.flush()
    }

    #[allow(clippy::too_many_arguments)]
    fn client12(
        &mut self,
        cfg: &TlsClientConfig,
        host: &str,
        cache_key: &str,
        sh: &ServerHello,
        client_random: &[u8],
        suites: &[CipherSuite],
        sess: Option<&ClientSession>,
        offered_session_id: &[u8],
    ) -> TlsResult<()> {
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
        let ticket_expected = match find_ext(&sh.exts, ext::SESSION_TICKET) {
            Some(d) if d.is_empty() && cfg.session_cache.is_some() => true,
            Some(_) => return Err(TlsError::proto(alert::UNSUPPORTED_EXTENSION, "unexpected session_ticket extension")),
            None => false,
        };
        self.alpn = check_alpn(cfg, &sh.exts)?;
        self.version = Some(TlsVersion::Tls12);
        self.suite = Some(suite);
        let h = suite.hash();
        let server_random = sh.random.clone();

        // abbreviated handshake?
        if let Some(s) = sess.filter(|_| !sh.session_id.is_empty() && sh.session_id == offered_session_id) {
            if s.suite != suite {
                return Err(illegal("resumed session with a different cipher suite"));
            }
            if s.ems != ems {
                return Err(TlsError::proto(alert::HANDSHAKE_FAILURE, "extended master secret mismatch on resumption"));
            }
            let mut new_ticket = None;
            if ticket_expected {
                new_ticket = Some(self.read_ticket12()?);
            }
            let (cw, sw) = ciphers12(suite, &s.secret, client_random, &server_random);
            self.pending_read12 = Some(sw);
            let fin = self.expect(hs::FINISHED)?;
            if self.pending_read12.is_some() {
                return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "Finished before ChangeCipherSpec"));
            }
            let expected = finished12(h, &s.secret, false, &h.hash(&self.transcript));
            if !ct_eq(&fin[4..], &expected) {
                return Err(TlsError::proto(alert::DECRYPT_ERROR, "server Finished verification failed"));
            }
            self.transcript.extend_from_slice(&fin);
            self.send_ccs();
            self.set_write_cipher(cw);
            let verify = finished12(h, &s.secret, true, &h.hash(&self.transcript));
            self.send_hs(&handshake_msg(hs::FINISHED, &verify))?;
            self.flush()?;
            self.resumed = true;
            self.peer_certs = s.peer_certs.clone();
            self.group = None;
            if let (Some(cache), Some((lifetime, ticket))) = (&cfg.session_cache, new_ticket) {
                let mut ns = s.clone();
                ns.ticket = ticket;
                ns.received_ms = now_ms();
                ns.lifetime_secs = lifetime;
                cache.put(cache_key, ns);
            }
            self.handshaking = false;
            return Ok(());
        }

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
            (Some(AuthKind::Ecdsa), PublicKey::Ec { .. } | PublicKey::Ed25519(_)) | (Some(AuthKind::Rsa), PublicKey::Rsa(_))
        );
        if !key_ok {
            return Err(TlsError::proto(alert::UNSUPPORTED_CERTIFICATE, "certificate key does not match cipher suite"));
        }

        let mut ske = self.read_hs()?;
        if ske[0] == hs::CERTIFICATE_STATUS {
            self.transcript.extend_from_slice(&ske);
            let body = ske[4..].to_vec();
            self.check_ocsp(cfg, Some(&body))?;
            ske = self.read_hs()?;
        } else {
            self.check_ocsp(cfg, None)?;
        }
        if ske[0] != hs::SERVER_KEY_EXCHANGE {
            return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "expected ServerKeyExchange"));
        }
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
        let mut cert_request: Option<(Vec<u8>, Vec<u16>)> = None;
        if msg[0] == hs::CERTIFICATE_REQUEST {
            let mut r = Reader::new(&msg[4..], "CertificateRequest");
            let types = r.vec8()?.to_vec();
            let algs_raw = r.vec16()?;
            let _cas = r.vec16()?;
            r.finish()?;
            if algs_raw.len() % 2 != 0 {
                return Err(TlsError::decode("CertificateRequest"));
            }
            let algs = algs_raw.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            cert_request = Some((types, algs));
            self.transcript.extend_from_slice(&msg);
            msg = self.read_hs()?;
        }
        if msg[0] != hs::SERVER_HELLO_DONE || msg.len() != 4 {
            return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "expected ServerHelloDone"));
        }
        self.transcript.extend_from_slice(&msg);

        let mut client_cv = None;
        if let Some((types, algs)) = &cert_request {
            let chosen = cfg.client_cert.as_ref().and_then(|ck| {
                if !types.contains(&cert_type_for(&ck.key)) {
                    return None;
                }
                choose_scheme(&ck.key, algs, false).map(|s| (ck.clone(), s))
            });
            let mut list = Vec::new();
            if let Some((ck, _)) = &chosen {
                for c in &ck.chain {
                    codec::put_vec24(&mut list, c);
                }
            }
            let mut body = Vec::new();
            codec::put_vec24(&mut body, &list);
            self.send_hs(&handshake_msg(hs::CERTIFICATE, &body))?;
            client_cv = chosen;
        }
        let share = KeyShare::generate(group);
        let pms = share.agree(point)?;
        self.group = Some(group);
        let mut cke = Vec::new();
        put_vec8(&mut cke, &share.public);
        self.send_hs(&handshake_msg(hs::CLIENT_KEY_EXCHANGE, &cke))?;
        let session_hash = h.hash(&self.transcript);
        if let Some((ck, scheme)) = client_cv {
            let sig = sign_message(&ck.key, scheme, &self.transcript)?;
            let mut body = Vec::new();
            put_u16(&mut body, scheme);
            put_vec16(&mut body, &sig);
            self.send_hs(&handshake_msg(hs::CERTIFICATE_VERIFY, &body))?;
        }
        let master = master_secret12(h, &pms, client_random, &server_random, if ems { Some(&session_hash) } else { None });
        let (cw, sw) = ciphers12(suite, &master, client_random, &server_random);
        self.send_ccs();
        self.set_write_cipher(cw);
        let verify = finished12(h, &master, true, &h.hash(&self.transcript));
        self.send_hs(&handshake_msg(hs::FINISHED, &verify))?;
        self.flush()?;

        let mut new_ticket = None;
        if ticket_expected {
            new_ticket = Some(self.read_ticket12()?);
        }
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
        if let Some(cache) = &cfg.session_cache {
            let (lifetime, ticket) = new_ticket.unwrap_or((super::config::DEFAULT_SESSION_LIFETIME, Vec::new()));
            if !ticket.is_empty() || !sh.session_id.is_empty() {
                cache.put(
                    cache_key,
                    ClientSession {
                        version: TlsVersion::Tls12,
                        suite,
                        secret: master,
                        ticket,
                        session_id: sh.session_id.clone(),
                        ems,
                        age_add: 0,
                        received_ms: now_ms(),
                        lifetime_secs: lifetime,
                        server_name: host.to_string(),
                        peer_certs: self.peer_certs.clone(),
                    },
                );
            }
        }
        self.handshaking = false;
        Ok(())
    }

    /// TLS 1.2 NewSessionTicket (RFC 5077): (lifetime, ticket).
    fn read_ticket12(&mut self) -> TlsResult<(u32, Vec<u8>)> {
        let m = self.expect(hs::NEW_SESSION_TICKET)?;
        let mut r = Reader::new(&m[4..], "NewSessionTicket");
        let hint = u32::from_be_bytes(r.take(4)?.try_into().unwrap());
        let ticket = r.vec16()?.to_vec();
        r.finish()?;
        self.transcript.extend_from_slice(&m);
        let lifetime = if hint == 0 { super::config::DEFAULT_SESSION_LIFETIME } else { hint.min(604_800) };
        Ok((lifetime, ticket))
    }
}

struct Client13<'a> {
    host: &'a str,
    cache_key: &'a str,
    share: &'a KeyShare,
    session_id: &'a [u8],
    suites: &'a [CipherSuite],
    hrr_suite: Option<CipherSuite>,
    sent_ccs: bool,
}

/// The content signed in a TLS 1.3 CertificateVerify.
pub(crate) fn cv_content(server: bool, transcript_hash: &[u8]) -> Vec<u8> {
    let mut signed = vec![0x20u8; 64];
    signed.extend_from_slice(if server { b"TLS 1.3, server CertificateVerify\0" } else { b"TLS 1.3, client CertificateVerify\0" });
    signed.extend_from_slice(transcript_hash);
    signed
}

/// Parses a TLS 1.3 Certificate message into the DER chain and the leaf's extensions.
pub(crate) fn parse_certificate13_exts(msg: &[u8]) -> TlsResult<(Vec<Vec<u8>>, Vec<(u16, Vec<u8>)>)> {
    let mut r = Reader::new(&msg[4..], "Certificate");
    let _ctx = r.vec8()?;
    let list = r.vec24()?;
    r.finish()?;
    let mut lr = Reader::new(list, "Certificate");
    let mut certs = Vec::new();
    let mut leaf_exts = Vec::new();
    while !lr.is_empty() {
        certs.push(lr.vec24()?.to_vec());
        let e = lr.vec16()?;
        if certs.len() == 1 {
            leaf_exts = parse_extensions(e)?;
        }
    }
    Ok((certs, leaf_exts))
}

/// Parses a TLS 1.3 Certificate message into the DER chain.
pub(crate) fn parse_certificate13(msg: &[u8]) -> TlsResult<Vec<Vec<u8>>> {
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
    Ok(certs)
}
