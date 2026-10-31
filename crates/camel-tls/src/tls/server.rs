//! Server handshake (TLS 1.3 and TLS 1.2).

use super::client::{hrr_random, DOWNGRADE_TLS12};
use super::codec::{self, find_ext, handshake_msg, parse_extensions, parse_u16_list, put_u16, put_vec16, put_vec24, put_vec8, Reader};
use super::config::TlsServerConfig;
use super::conn::Conn;
use super::keyschedule::{cipher13, ciphers12, finished12, master_secret12, message_hash, KeySchedule13};
use super::{alert, choose_scheme, ext, hs, sign_message, AuthKind, KeyShare, NamedGroup, TlsError, TlsResult, TlsVersion};
use crate::crypto::rng;
use crate::keys::PrivateKey;
use crate::util::ct_eq;
use std::io::{Read, Write};

struct ClientHello {
    legacy_version: u16,
    random: Vec<u8>,
    session_id: Vec<u8>,
    suites: Vec<u16>,
    exts: Vec<(u16, Vec<u8>)>,
}

impl ClientHello {
    fn parse(body: &[u8]) -> TlsResult<ClientHello> {
        let mut r = Reader::new(body, "ClientHello");
        let legacy_version = r.u16()?;
        let random = r.take(32)?.to_vec();
        let session_id = r.vec8()?.to_vec();
        if session_id.len() > 32 {
            return Err(TlsError::decode("ClientHello session id"));
        }
        let suites_raw = r.vec16()?;
        if suites_raw.len() % 2 != 0 || suites_raw.is_empty() {
            return Err(TlsError::decode("ClientHello cipher suites"));
        }
        let suites = suites_raw.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        let compression = r.vec8()?;
        if !compression.contains(&0) {
            return Err(TlsError::proto(alert::ILLEGAL_PARAMETER, "client does not offer null compression"));
        }
        let exts = if r.is_empty() { Vec::new() } else { parse_extensions(r.vec16()?)? };
        r.finish()?;
        Ok(ClientHello { legacy_version, random, session_id, suites, exts })
    }

    fn versions(&self) -> TlsResult<Option<Vec<u16>>> {
        match find_ext(&self.exts, ext::SUPPORTED_VERSIONS) {
            None => Ok(None),
            Some(d) => {
                let mut r = Reader::new(d, "supported_versions");
                let list = r.vec8()?;
                r.finish()?;
                if list.len() % 2 != 0 {
                    return Err(TlsError::decode("supported_versions"));
                }
                Ok(Some(list.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()))
            }
        }
    }

    fn sni(&self) -> TlsResult<Option<String>> {
        let d = match find_ext(&self.exts, ext::SERVER_NAME) {
            Some(d) => d,
            None => return Ok(None),
        };
        let mut r = Reader::new(d, "server_name");
        let list = r.vec16()?;
        r.finish()?;
        let mut lr = Reader::new(list, "server_name");
        while !lr.is_empty() {
            let typ = lr.u8()?;
            let name = lr.vec16()?;
            if typ == 0 {
                return Ok(Some(String::from_utf8_lossy(name).to_ascii_lowercase()));
            }
        }
        Ok(None)
    }

    fn key_shares(&self) -> TlsResult<Vec<(u16, Vec<u8>)>> {
        let d = match find_ext(&self.exts, ext::KEY_SHARE) {
            Some(d) => d,
            None => return Ok(Vec::new()),
        };
        let mut r = Reader::new(d, "key_share");
        let list = r.vec16()?;
        r.finish()?;
        let mut lr = Reader::new(list, "key_share");
        let mut out = Vec::new();
        while !lr.is_empty() {
            let g = lr.u16()?;
            let k = lr.vec16()?.to_vec();
            out.push((g, k));
        }
        Ok(out)
    }

    fn sig_algs(&self) -> TlsResult<Option<Vec<u16>>> {
        find_ext(&self.exts, ext::SIGNATURE_ALGORITHMS).map(|d| parse_u16_list(d, "signature_algorithms")).transpose()
    }

    fn groups(&self) -> TlsResult<Option<Vec<u16>>> {
        find_ext(&self.exts, ext::SUPPORTED_GROUPS).map(|d| parse_u16_list(d, "supported_groups")).transpose()
    }
}

fn select_alpn(cfg: &TlsServerConfig, ch: &ClientHello) -> TlsResult<Option<Vec<u8>>> {
    let offered = match find_ext(&ch.exts, ext::ALPN) {
        Some(d) => codec::parse_alpn_list(d)?,
        None => return Ok(None),
    };
    if cfg.alpn.is_empty() {
        return Ok(None);
    }
    match cfg.alpn.iter().find(|p| offered.contains(p)) {
        Some(p) => Ok(Some(p.clone())),
        None => Err(TlsError::proto(alert::NO_APPLICATION_PROTOCOL, "no common ALPN protocol")),
    }
}

fn alpn_ext(proto: &[u8]) -> Vec<u8> {
    codec::alpn_list(&[proto.to_vec()])
}

impl<S: Read + Write> Conn<S> {
    pub(crate) fn server_handshake(&mut self, cfg: &TlsServerConfig) -> TlsResult<()> {
        let msg = self.expect(hs::CLIENT_HELLO)?;
        let ch = ClientHello::parse(&msg[4..])?;
        let version = match ch.versions()? {
            Some(list) => {
                if list.contains(&0x0304) && cfg.max_version >= TlsVersion::Tls13 {
                    TlsVersion::Tls13
                } else if list.contains(&0x0303) && cfg.min_version <= TlsVersion::Tls12 {
                    TlsVersion::Tls12
                } else {
                    return Err(TlsError::proto(alert::PROTOCOL_VERSION, "no mutually supported TLS version"));
                }
            }
            None => {
                if ch.legacy_version >= 0x0303 && cfg.min_version <= TlsVersion::Tls12 {
                    TlsVersion::Tls12
                } else {
                    return Err(TlsError::proto(alert::PROTOCOL_VERSION, "client does not support TLS 1.2 or later"));
                }
            }
        };
        self.sni = ch.sni()?;
        self.transcript.extend_from_slice(&msg);
        match version {
            TlsVersion::Tls13 => self.server13(cfg, ch, msg),
            TlsVersion::Tls12 => self.server12(cfg, ch),
        }
    }

    fn server13(&mut self, cfg: &TlsServerConfig, mut ch: ClientHello, ch1_msg: Vec<u8>) -> TlsResult<()> {
        let suite = cfg
            .suites
            .iter()
            .copied()
            .find(|s| s.is_tls13() && ch.suites.contains(&s.id()))
            .ok_or_else(|| TlsError::proto(alert::HANDSHAKE_FAILURE, "no common TLS 1.3 cipher suite"))?;
        let h = suite.hash();
        let mut shares = ch.key_shares()?;
        let client_groups = ch.groups()?.ok_or_else(|| TlsError::proto(alert::MISSING_EXTENSION, "missing supported_groups"))?;
        let mut chosen = cfg.groups.iter().copied().find_map(|g| shares.iter().find(|(id, _)| *id == g.id()).map(|(_, k)| (g, k.clone())));
        let mut sent_ccs = false;
        if chosen.is_none() {
            let group = cfg
                .groups
                .iter()
                .copied()
                .find(|g| client_groups.contains(&g.id()))
                .ok_or_else(|| TlsError::proto(alert::HANDSHAKE_FAILURE, "no common key exchange group"))?;
            // HelloRetryRequest
            let mut body = Vec::new();
            put_u16(&mut body, 0x0303);
            body.extend(hrr_random());
            put_vec8(&mut body, &ch.session_id);
            put_u16(&mut body, suite.id());
            body.push(0);
            body.extend(codec::extensions(&[(ext::SUPPORTED_VERSIONS, vec![0x03, 0x04]), (ext::KEY_SHARE, group.id().to_be_bytes().to_vec())]));
            let hrr = handshake_msg(hs::SERVER_HELLO, &body);
            self.transcript = message_hash(h, &ch1_msg);
            self.send_hs(&hrr)?;
            if !ch.session_id.is_empty() {
                self.send_ccs();
                sent_ccs = true;
            }
            self.flush()?;
            let msg2 = self.expect(hs::CLIENT_HELLO)?;
            let ch2 = ClientHello::parse(&msg2[4..])?;
            if ch2.random != ch.random || ch2.session_id != ch.session_id || !ch2.suites.contains(&suite.id()) {
                return Err(TlsError::proto(alert::ILLEGAL_PARAMETER, "second ClientHello inconsistent with the first"));
            }
            if ch2.versions()?.is_none_or(|v| !v.contains(&0x0304)) {
                return Err(TlsError::proto(alert::ILLEGAL_PARAMETER, "second ClientHello dropped TLS 1.3"));
            }
            shares = ch2.key_shares()?;
            if shares.len() != 1 || shares[0].0 != group.id() {
                return Err(TlsError::proto(alert::ILLEGAL_PARAMETER, "second ClientHello has wrong key_share"));
            }
            chosen = Some((group, shares[0].1.clone()));
            self.transcript.extend_from_slice(&msg2);
            ch = ch2;
        }
        let (group, client_key) = chosen.expect("group chosen");
        let sig_algs = ch.sig_algs()?.ok_or_else(|| TlsError::proto(alert::MISSING_EXTENSION, "missing signature_algorithms"))?;
        let scheme = choose_scheme(&cfg.key, &sig_algs, true)
            .ok_or_else(|| TlsError::proto(alert::HANDSHAKE_FAILURE, "no common signature scheme"))?;
        let alpn = select_alpn(cfg, &ch)?;

        let share = KeyShare::generate(group);
        let shared = share.agree(&client_key)?;

        let mut body = Vec::new();
        put_u16(&mut body, 0x0303);
        body.extend(rng::random_bytes(32));
        put_vec8(&mut body, &ch.session_id);
        put_u16(&mut body, suite.id());
        body.push(0);
        let mut ks = Vec::new();
        put_u16(&mut ks, group.id());
        put_vec16(&mut ks, &share.public);
        body.extend(codec::extensions(&[(ext::SUPPORTED_VERSIONS, vec![0x03, 0x04]), (ext::KEY_SHARE, ks)]));
        self.send_hs(&handshake_msg(hs::SERVER_HELLO, &body))?;
        let sched = KeySchedule13::new(suite, &shared, &h.hash(&self.transcript));
        self.version = Some(TlsVersion::Tls13);
        self.suite = Some(suite);
        self.group = Some(group);
        self.alpn = alpn.clone();
        if !sent_ccs && !ch.session_id.is_empty() {
            self.send_ccs();
        }
        self.set_write_cipher(cipher13(suite, &sched.server_hs));

        // EncryptedExtensions
        let mut exts = Vec::new();
        if let Some(p) = &alpn {
            exts.push((ext::ALPN, alpn_ext(p)));
        }
        if self.sni.is_some() {
            exts.push((ext::SERVER_NAME, Vec::new()));
        }
        self.send_hs(&handshake_msg(hs::ENCRYPTED_EXTENSIONS, &codec::extensions(&exts)))?;

        // Certificate
        let mut list = Vec::new();
        for c in &cfg.cert_chain {
            put_vec24(&mut list, c);
            put_u16(&mut list, 0);
        }
        let mut body = vec![0u8];
        put_vec24(&mut body, &list);
        self.send_hs(&handshake_msg(hs::CERTIFICATE, &body))?;

        // CertificateVerify
        let mut signed = vec![0x20u8; 64];
        signed.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
        signed.extend(h.hash(&self.transcript));
        let sig = sign_message(&cfg.key, scheme, &signed)?;
        let mut body = Vec::new();
        put_u16(&mut body, scheme);
        put_vec16(&mut body, &sig);
        self.send_hs(&handshake_msg(hs::CERTIFICATE_VERIFY, &body))?;

        // Finished
        let fin = sched.finished_mac(&sched.server_hs, &h.hash(&self.transcript));
        self.send_hs(&handshake_msg(hs::FINISHED, &fin))?;
        let (c_ap, s_ap) = sched.app_secrets(&h.hash(&self.transcript));
        self.set_write_cipher(cipher13(suite, &s_ap));
        self.flush()?;

        self.set_read_cipher(cipher13(suite, &sched.client_hs))?;
        let cfin = self.expect(hs::FINISHED)?;
        let expected = sched.finished_mac(&sched.client_hs, &h.hash(&self.transcript));
        if !ct_eq(&cfin[4..], &expected) {
            return Err(TlsError::proto(alert::DECRYPT_ERROR, "client Finished verification failed"));
        }
        self.transcript.extend_from_slice(&cfin);
        self.set_read_cipher(cipher13(suite, &c_ap))?;
        self.read_secret = c_ap;
        self.write_secret = s_ap;
        self.handshaking = false;
        Ok(())
    }

    fn server12(&mut self, cfg: &TlsServerConfig, ch: ClientHello) -> TlsResult<()> {
        let kind = match cfg.key {
            PrivateKey::Ec { .. } => AuthKind::Ecdsa,
            PrivateKey::Rsa(_) => AuthKind::Rsa,
        };
        let suite = cfg
            .suites
            .iter()
            .copied()
            .find(|s| !s.is_tls13() && s.auth() == Some(kind) && ch.suites.contains(&s.id()))
            .ok_or_else(|| TlsError::proto(alert::HANDSHAKE_FAILURE, "no common TLS 1.2 cipher suite"))?;
        let sig_algs = ch
            .sig_algs()?
            .ok_or_else(|| TlsError::proto(alert::HANDSHAKE_FAILURE, "client did not send signature_algorithms (SHA-1 only)"))?;
        let scheme = choose_scheme(&cfg.key, &sig_algs, false)
            .ok_or_else(|| TlsError::proto(alert::HANDSHAKE_FAILURE, "no common signature scheme"))?;
        let client_groups = ch.groups()?.unwrap_or_else(|| vec![NamedGroup::Secp256r1.id()]);
        // RFC 8422 section 5.1: an ECDSA certificate's curve must be one the client supports
        if let PrivateKey::Ec { curve, .. } = &cfg.key {
            let needed = match curve {
                crate::crypto::ec::EcCurve::P256 => NamedGroup::Secp256r1,
                crate::crypto::ec::EcCurve::P384 => NamedGroup::Secp384r1,
            };
            if !client_groups.contains(&needed.id()) {
                return Err(TlsError::proto(alert::HANDSHAKE_FAILURE, "client does not support the certificate's curve"));
            }
        }
        let group = cfg
            .groups
            .iter()
            .copied()
            .find(|g| client_groups.contains(&g.id()))
            .ok_or_else(|| TlsError::proto(alert::HANDSHAKE_FAILURE, "no common curve"))?;
        if let Some(pf) = find_ext(&ch.exts, ext::EC_POINT_FORMATS) {
            let mut r = Reader::new(pf, "ec_point_formats");
            if !r.vec8()?.contains(&0) {
                return Err(TlsError::proto(alert::ILLEGAL_PARAMETER, "uncompressed points not supported by client"));
            }
        }
        let ems = find_ext(&ch.exts, ext::EXTENDED_MASTER_SECRET).is_some();
        let reneg = match find_ext(&ch.exts, ext::RENEGOTIATION_INFO) {
            Some(d) if d != [0] => return Err(TlsError::proto(alert::HANDSHAKE_FAILURE, "bad renegotiation_info")),
            Some(_) => true,
            None => ch.suites.contains(&0x00ff),
        };
        let alpn = select_alpn(cfg, &ch)?;
        self.version = Some(TlsVersion::Tls12);
        self.suite = Some(suite);
        self.group = Some(group);
        self.alpn = alpn.clone();
        let h = suite.hash();

        let mut server_random = rng::random_bytes(32);
        if cfg.max_version >= TlsVersion::Tls13 {
            server_random[24..].copy_from_slice(&DOWNGRADE_TLS12);
        }
        let mut body = Vec::new();
        put_u16(&mut body, 0x0303);
        body.extend_from_slice(&server_random);
        put_vec8(&mut body, &[]);
        put_u16(&mut body, suite.id());
        body.push(0);
        let mut exts: Vec<(u16, Vec<u8>)> = Vec::new();
        if ems {
            exts.push((ext::EXTENDED_MASTER_SECRET, Vec::new()));
        }
        if reneg {
            exts.push((ext::RENEGOTIATION_INFO, vec![0]));
        }
        if let Some(p) = &alpn {
            exts.push((ext::ALPN, alpn_ext(p)));
        }
        exts.push((ext::EC_POINT_FORMATS, vec![1, 0]));
        body.extend(codec::extensions(&exts));
        self.send_hs(&handshake_msg(hs::SERVER_HELLO, &body))?;

        let mut list = Vec::new();
        for c in &cfg.cert_chain {
            put_vec24(&mut list, c);
        }
        let mut body = Vec::new();
        put_vec24(&mut body, &list);
        self.send_hs(&handshake_msg(hs::CERTIFICATE, &body))?;

        let share = KeyShare::generate(group);
        let mut params = vec![3u8];
        put_u16(&mut params, group.id());
        put_vec8(&mut params, &share.public);
        let mut signed = ch.random.clone();
        signed.extend_from_slice(&server_random);
        signed.extend_from_slice(&params);
        let sig = sign_message(&cfg.key, scheme, &signed)?;
        let mut body = params;
        put_u16(&mut body, scheme);
        put_vec16(&mut body, &sig);
        self.send_hs(&handshake_msg(hs::SERVER_KEY_EXCHANGE, &body))?;
        self.send_hs(&handshake_msg(hs::SERVER_HELLO_DONE, &[]))?;
        self.flush()?;

        let cke = self.expect(hs::CLIENT_KEY_EXCHANGE)?;
        let mut r = Reader::new(&cke[4..], "ClientKeyExchange");
        let point = r.vec8()?.to_vec();
        r.finish()?;
        let pms = share.agree(&point)?;
        self.transcript.extend_from_slice(&cke);
        let session_hash = h.hash(&self.transcript);
        let master = master_secret12(h, &pms, &ch.random, &server_random, if ems { Some(&session_hash) } else { None });
        let (cw, sw) = ciphers12(suite, &master, &ch.random, &server_random);
        self.pending_read12 = Some(cw);
        let fin = self.expect(hs::FINISHED)?;
        if self.pending_read12.is_some() {
            return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "Finished before ChangeCipherSpec"));
        }
        let expected = finished12(h, &master, true, &h.hash(&self.transcript));
        if !ct_eq(&fin[4..], &expected) {
            return Err(TlsError::proto(alert::DECRYPT_ERROR, "client Finished verification failed"));
        }
        self.transcript.extend_from_slice(&fin);
        self.send_ccs();
        self.set_write_cipher(sw);
        let verify = finished12(h, &master, false, &h.hash(&self.transcript));
        self.send_hs(&handshake_msg(hs::FINISHED, &verify))?;
        self.handshaking = false;
        self.flush()
    }
}
