//! SSH transport layer protocol (RFC 4253) for both roles: version exchange,
//! binary packet protocol, algorithm negotiation, ECDH key exchange
//! (`curve25519-sha256` RFC 8731, `ecdh-sha2-nistp256` RFC 5656), key
//! derivation, rekeying and OpenSSH "strict KEX" (Terrapin countermeasure,
//! `kex-strict-*-v00@openssh.com`). Extension negotiation (RFC 8308) is
//! supported for `server-sig-algs`.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpStream;

use cameleer_tls::crypto::aes::AesGcm;
use cameleer_tls::crypto::ec::EcCurve;
use cameleer_tls::crypto::hash::{Digest, Sha256};
use cameleer_tls::crypto::rng;
use cameleer_tls::crypto::x25519::{x25519, x25519_base};
use cameleer_tls::util::ct_eq;

use crate::crypto::cipher::{AesCtr, ChaChaPolyOpenSsh, MacAlg};
use crate::error::{SshError, SshResult};
use crate::keys::{SshPrivateKey, SshPublicKey};
use crate::wire::{mpint_bytes, Reader, Writer};

/// Message numbers (RFC 4250 section 4.1).
pub mod msg {
    pub const DISCONNECT: u8 = 1;
    pub const IGNORE: u8 = 2;
    pub const UNIMPLEMENTED: u8 = 3;
    pub const DEBUG: u8 = 4;
    pub const SERVICE_REQUEST: u8 = 5;
    pub const SERVICE_ACCEPT: u8 = 6;
    pub const EXT_INFO: u8 = 7;
    pub const KEXINIT: u8 = 20;
    pub const NEWKEYS: u8 = 21;
    pub const KEX_ECDH_INIT: u8 = 30;
    pub const KEX_ECDH_REPLY: u8 = 31;
    pub const USERAUTH_REQUEST: u8 = 50;
    pub const USERAUTH_FAILURE: u8 = 51;
    pub const USERAUTH_SUCCESS: u8 = 52;
    pub const USERAUTH_BANNER: u8 = 53;
    /// `SSH_MSG_USERAUTH_PK_OK`, `SSH_MSG_USERAUTH_PASSWD_CHANGEREQ` and `SSH_MSG_USERAUTH_INFO_REQUEST`.
    pub const USERAUTH_60: u8 = 60;
    pub const USERAUTH_INFO_RESPONSE: u8 = 61;
    pub const GLOBAL_REQUEST: u8 = 80;
    pub const REQUEST_SUCCESS: u8 = 81;
    pub const REQUEST_FAILURE: u8 = 82;
    pub const CHANNEL_OPEN: u8 = 90;
    pub const CHANNEL_OPEN_CONFIRMATION: u8 = 91;
    pub const CHANNEL_OPEN_FAILURE: u8 = 92;
    pub const CHANNEL_WINDOW_ADJUST: u8 = 93;
    pub const CHANNEL_DATA: u8 = 94;
    pub const CHANNEL_EXTENDED_DATA: u8 = 95;
    pub const CHANNEL_EOF: u8 = 96;
    pub const CHANNEL_CLOSE: u8 = 97;
    pub const CHANNEL_REQUEST: u8 = 98;
    pub const CHANNEL_SUCCESS: u8 = 99;
    pub const CHANNEL_FAILURE: u8 = 100;
}

/// Disconnect reason codes used by this implementation.
pub mod reason {
    pub const PROTOCOL_ERROR: u32 = 2;
    pub const KEY_EXCHANGE_FAILED: u32 = 3;
    pub const MAC_ERROR: u32 = 5;
    pub const SERVICE_NOT_AVAILABLE: u32 = 7;
    pub const HOST_KEY_NOT_VERIFIABLE: u32 = 9;
    pub const BY_APPLICATION: u32 = 11;
    pub const NO_MORE_AUTH_METHODS: u32 = 14;
}

/// Supported key exchange methods, in default preference order.
pub const SUPPORTED_KEX: &[&str] = &["curve25519-sha256", "curve25519-sha256@libssh.org", "ecdh-sha2-nistp256"];
/// Supported host key (signature) algorithms, in default preference order.
pub const SUPPORTED_HOST_KEY: &[&str] = &["ssh-ed25519", "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp384", "rsa-sha2-512", "rsa-sha2-256"];
/// Supported ciphers, in default preference order.
pub const SUPPORTED_CIPHERS: &[&str] = &[
    "chacha20-poly1305@openssh.com",
    "aes128-gcm@openssh.com",
    "aes256-gcm@openssh.com",
    "aes128-ctr",
    "aes192-ctr",
    "aes256-ctr",
];
/// Supported MACs (used with the CTR ciphers), in default preference order.
pub const SUPPORTED_MACS: &[&str] = &["hmac-sha2-256-etm@openssh.com", "hmac-sha2-512-etm@openssh.com", "hmac-sha2-256", "hmac-sha2-512"];

const STRICT_C: &str = "kex-strict-c-v00@openssh.com";
const STRICT_S: &str = "kex-strict-s-v00@openssh.com";
const MAX_PACKET: usize = 256 * 1024 + 1024;

/// Algorithm preference lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Algorithms {
    pub kex: Vec<String>,
    pub host_key: Vec<String>,
    pub ciphers: Vec<String>,
    pub macs: Vec<String>,
}

impl Default for Algorithms {
    fn default() -> Self {
        let v = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
        Algorithms { kex: v(SUPPORTED_KEX), host_key: v(SUPPORTED_HOST_KEY), ciphers: v(SUPPORTED_CIPHERS), macs: v(SUPPORTED_MACS) }
    }
}

/// Parses a comma separated algorithm list, rejecting unsupported names.
pub fn parse_algorithm_list(what: &str, list: &str, supported: &[&str]) -> SshResult<Vec<String>> {
    let mut out = Vec::new();
    for name in list.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if !supported.contains(&name) {
            return Err(SshError::Negotiation(format!("unsupported {} algorithm '{}' (supported: {})", what, name, supported.join(","))));
        }
        out.push(name.to_string());
    }
    if out.is_empty() {
        return Err(SshError::Negotiation(format!("empty {} algorithm list", what)));
    }
    Ok(out)
}

/// Supported compression algorithms (`zlib@openssh.com` starts after user authentication).
pub const SUPPORTED_COMPRESSION: &[&str] = &["none", "zlib@openssh.com", "zlib"];

/// The algorithms chosen by the last key exchange.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Negotiated {
    pub comp_c2s: String,
    pub comp_s2c: String,
    pub kex: String,
    pub host_key: String,
    pub cipher_c2s: String,
    pub cipher_s2c: String,
    /// Empty for AEAD ciphers.
    pub mac_c2s: String,
    pub mac_s2c: String,
}

/// A byte stream carrying an SSH connection.
pub trait Stream: Read + Write + Send {
    /// Whether some data can be read without blocking (best effort, default false).
    fn data_available(&mut self) -> bool {
        false
    }
}

impl Stream for TcpStream {
    fn data_available(&mut self) -> bool {
        if self.set_nonblocking(true).is_err() {
            return false;
        }
        let mut b = [0u8; 1];
        let r = self.peek(&mut b);
        let _ = self.set_nonblocking(false);
        r.is_ok()
    }
}

/// Callback deciding whether a server host key is acceptable (client role); the second
/// argument is the host certificate when the server presented one (its CA signature is not
/// checked yet: that is the verifier's job, e.g. against `@cert-authority` entries).
pub type HostKeyVerifier = Box<dyn FnMut(&SshPublicKey, Option<&crate::cert::SshCertificate>) -> SshResult<()> + Send>;

/// Transport configuration.
#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub algorithms: Algorithms,
    /// Rekey after this many bytes in either direction (default 1 GiB).
    pub rekey_limit: u64,
    /// Software version advertised in the identification string.
    pub software_version: String,
    /// Compression preference (`none`, `zlib@openssh.com`, `zlib`), default `none` only.
    pub compression: Vec<String>,
    /// Server role: host certificates (offered as `*-cert-v01@openssh.com` host key
    /// algorithms for the host keys they certify).
    pub host_certificates: Vec<crate::cert::SshCertificate>,
}

impl Default for TransportConfig {
    fn default() -> Self {
        TransportConfig { algorithms: Algorithms::default(), rekey_limit: 1 << 30, software_version: "CamelSSH_1.0".into(), compression: vec!["none".into()], host_certificates: Vec::new() }
    }
}

enum Role {
    Client(HostKeyVerifier),
    Server(Vec<SshPrivateKey>),
}

enum Cipher {
    None,
    Ctr { ctr: AesCtr, mac: MacAlg, key: Vec<u8> },
    Gcm { gcm: AesGcm, iv: [u8; 12] },
    ChaCha(ChaChaPolyOpenSsh),
}

struct Dir {
    seq: u32,
    cipher: Cipher,
    bytes: u64,
    packets: u64,
    /// Negotiated compression of this direction ("none", "zlib", "zlib@openssh.com").
    comp: String,
    deflater: Option<crate::zlib::Deflater>,
    inflater: Option<crate::zlib::Inflater>,
}

impl Dir {
    fn new() -> Dir {
        Dir { seq: 0, cipher: Cipher::None, bytes: 0, packets: 0, comp: "none".into(), deflater: None, inflater: None }
    }
    fn compressing(&self) -> bool {
        self.deflater.is_some() || self.inflater.is_some()
    }
    /// Switches compression on (outgoing: deflater, incoming: inflater).
    fn start_compression(&mut self, outgoing: bool) {
        if self.comp != "none" && !self.compressing() {
            if outgoing {
                self.deflater = Some(crate::zlib::Deflater::new());
            } else {
                self.inflater = Some(crate::zlib::Inflater::new());
            }
        }
    }
}

fn incr_iv(iv: &mut [u8; 12]) {
    let mut c = [0u8; 8];
    c.copy_from_slice(&iv[4..]);
    iv[4..].copy_from_slice(&u64::from_be_bytes(c).wrapping_add(1).to_be_bytes());
}

/// Key length, IV length and AEAD flag of a cipher.
fn cipher_params(name: &str) -> (usize, usize, bool) {
    match name {
        "chacha20-poly1305@openssh.com" => (64, 0, true),
        "aes128-gcm@openssh.com" => (16, 12, true),
        "aes256-gcm@openssh.com" => (32, 12, true),
        "aes128-ctr" => (16, 16, false),
        "aes192-ctr" => (24, 16, false),
        _ => (32, 16, false),
    }
}

fn make_cipher(name: &str, mac: &str, iv: &[u8], key: &[u8], mac_key: Vec<u8>) -> Cipher {
    match name {
        "chacha20-poly1305@openssh.com" => Cipher::ChaCha(ChaChaPolyOpenSsh::new(key)),
        "aes128-gcm@openssh.com" | "aes256-gcm@openssh.com" => {
            let mut n = [0u8; 12];
            n.copy_from_slice(&iv[..12]);
            Cipher::Gcm { gcm: AesGcm::new(key), iv: n }
        }
        _ => Cipher::Ctr { ctr: AesCtr::new(key, iv), mac: MacAlg::from_name(mac).expect("negotiated MAC"), key: mac_key },
    }
}

#[derive(Debug)]
struct KexInit {
    kex: Vec<String>,
    host_key: Vec<String>,
    enc: [Vec<String>; 2],
    mac: [Vec<String>; 2],
    comp: [Vec<String>; 2],
    first_follows: bool,
}

fn parse_kexinit(p: &[u8]) -> SshResult<KexInit> {
    let mut r = Reader::new(p);
    if r.u8()? != msg::KEXINIT {
        return Err(SshError::protocol("expected KEXINIT"));
    }
    r.take(16)?;
    let kex = r.name_list()?;
    let host_key = r.name_list()?;
    let enc = [r.name_list()?, r.name_list()?];
    let mac = [r.name_list()?, r.name_list()?];
    let comp = [r.name_list()?, r.name_list()?];
    r.name_list()?;
    r.name_list()?;
    let first_follows = r.bool()?;
    r.u32()?;
    Ok(KexInit { kex, host_key, enc, mac, comp, first_follows })
}

fn choose(what: &str, client: &[String], server: &[String]) -> SshResult<String> {
    client
        .iter()
        .find(|c| server.contains(c))
        .cloned()
        .ok_or_else(|| SshError::Negotiation(format!("no common {} algorithm (client: {}; server: {})", what, client.join(","), server.join(","))))
}

fn parse_disconnect(p: &[u8]) -> SshError {
    let mut r = Reader::new(&p[1..]);
    let reason = r.u32().unwrap_or(0);
    let message = r.str().unwrap_or_default();
    SshError::Disconnected { reason, message }
}

/// An established SSH transport.
pub struct Transport<S: Stream> {
    stream: S,
    rbuf: Vec<u8>,
    rpos: usize,
    role: Role,
    cfg: TransportConfig,
    v_local: Vec<u8>,
    v_remote: Vec<u8>,
    session_id: Vec<u8>,
    out: Dir,
    inp: Dir,
    strict: bool,
    /// Strict mode during the initial key exchange: only KEX messages allowed.
    strict_initial: bool,
    host_key_blob: Option<Vec<u8>>,
    pending: VecDeque<Vec<u8>>,
    negotiated: Negotiated,
    peer_ext_info: bool,
    server_sig_algs: Option<Vec<String>>,
    kex_count: u32,
    last_recv_seq: u32,
    /// User authentication succeeded (delayed `zlib@openssh.com` compression is active).
    authenticated: bool,
}

impl<S: Stream> Transport<S> {
    fn new(stream: S, role: Role, cfg: TransportConfig) -> Self {
        let v_local = format!("SSH-2.0-{}", cfg.software_version).into_bytes();
        Transport {
            stream,
            rbuf: Vec::new(),
            rpos: 0,
            role,
            cfg,
            v_local,
            v_remote: Vec::new(),
            session_id: Vec::new(),
            out: Dir::new(),
            inp: Dir::new(),
            strict: false,
            strict_initial: false,
            host_key_blob: None,
            pending: VecDeque::new(),
            negotiated: Negotiated::default(),
            peer_ext_info: false,
            server_sig_algs: None,
            kex_count: 0,
            last_recv_seq: 0,
            authenticated: false,
        }
    }

    /// Performs the client side handshake (version exchange and first key exchange).
    pub fn client(stream: S, cfg: TransportConfig, verifier: HostKeyVerifier) -> SshResult<Self> {
        let mut t = Transport::new(stream, Role::Client(verifier), cfg);
        t.version_exchange()?;
        t.kex(None)?;
        Ok(t)
    }

    /// Performs the server side handshake with the given host keys.
    pub fn server(stream: S, cfg: TransportConfig, host_keys: Vec<SshPrivateKey>) -> SshResult<Self> {
        if host_keys.is_empty() {
            return Err(SshError::Key("server needs at least one host key".into()));
        }
        let mut t = Transport::new(stream, Role::Server(host_keys), cfg);
        t.version_exchange()?;
        t.kex(None)?;
        if t.peer_ext_info {
            let algs: Vec<&str> = SUPPORTED_HOST_KEY.to_vec();
            let p = Writer::msg(msg::EXT_INFO).u32(1).str("server-sig-algs").name_list(&algs).finish();
            t.write_packet(&p)?;
        }
        Ok(t)
    }

    fn is_client(&self) -> bool {
        matches!(self.role, Role::Client(_))
    }

    /// The session identifier (exchange hash of the first key exchange).
    pub fn session_id(&self) -> &[u8] {
        &self.session_id
    }

    /// The algorithms of the current keys.
    pub fn negotiated(&self) -> &Negotiated {
        &self.negotiated
    }

    /// The peer identification string.
    pub fn remote_version(&self) -> String {
        String::from_utf8_lossy(&self.v_remote).into_owned()
    }

    /// Number of completed key exchanges (1 after the handshake).
    pub fn kex_count(&self) -> u32 {
        self.kex_count
    }

    /// Whether strict KEX is in effect.
    pub fn strict_kex(&self) -> bool {
        self.strict
    }

    /// `server-sig-algs` from the server's `SSH_MSG_EXT_INFO` (client role).
    pub fn server_sig_algs(&self) -> Option<&[String]> {
        self.server_sig_algs.as_deref()
    }

    /// The server host key blob (client role).
    pub fn host_key_blob(&self) -> Option<&[u8]> {
        self.host_key_blob.as_deref()
    }

    /// The underlying stream (to adjust socket options).
    pub fn stream(&self) -> &S {
        &self.stream
    }

    /// Whether data can be read without blocking.
    pub fn data_available(&mut self) -> bool {
        !self.pending.is_empty() || self.rpos < self.rbuf.len() || self.stream.data_available()
    }

    // ------------------------------------------------------------ raw I/O

    fn fill(&mut self, n: usize) -> SshResult<()> {
        while self.rbuf.len() - self.rpos < n {
            if self.rpos > 0 && self.rpos == self.rbuf.len() {
                self.rbuf.clear();
                self.rpos = 0;
            } else if self.rpos > 65536 {
                self.rbuf.drain(..self.rpos);
                self.rpos = 0;
            }
            let mut tmp = [0u8; 32768];
            let k = self.stream.read(&mut tmp)?;
            if k == 0 {
                return Err(SshError::Disconnected { reason: 0, message: "connection closed by peer".into() });
            }
            self.rbuf.extend_from_slice(&tmp[..k]);
        }
        Ok(())
    }

    fn take(&mut self, n: usize) -> SshResult<Vec<u8>> {
        self.fill(n)?;
        let v = self.rbuf[self.rpos..self.rpos + n].to_vec();
        self.rpos += n;
        Ok(v)
    }

    fn version_exchange(&mut self) -> SshResult<()> {
        let mut line = self.v_local.clone();
        line.extend_from_slice(b"\r\n");
        self.stream.write_all(&line)?;
        self.stream.flush()?;
        for _ in 0..64 {
            let mut l = Vec::new();
            loop {
                self.fill(1)?;
                let b = self.rbuf[self.rpos];
                self.rpos += 1;
                if b == b'\n' {
                    break;
                }
                l.push(b);
                if l.len() > 1024 {
                    return Err(SshError::protocol("identification line too long"));
                }
            }
            if l.last() == Some(&b'\r') {
                l.pop();
            }
            if l.starts_with(b"SSH-") {
                if !(l.starts_with(b"SSH-2.0-") || l.starts_with(b"SSH-1.99-")) {
                    return Err(SshError::protocol(format!("unsupported protocol version: {}", String::from_utf8_lossy(&l))));
                }
                self.v_remote = l;
                return Ok(());
            }
        }
        Err(SshError::protocol("no SSH identification string received"))
    }

    // ------------------------------------------------------------ packets

    fn write_packet(&mut self, payload: &[u8]) -> SshResult<()> {
        let compressed;
        let payload = match self.out.deflater.as_mut() {
            Some(d) => {
                compressed = d.compress(payload);
                &compressed[..]
            }
            None => payload,
        };
        let r = self.write_packet_raw(payload);
        // server: delayed compression starts after USERAUTH_SUCCESS was sent
        if !self.is_client() && !self.authenticated && payload.first() == Some(&msg::USERAUTH_SUCCESS) && r.is_ok() {
            self.authenticated = true;
            if self.out.comp == "zlib@openssh.com" {
                self.out.start_compression(true);
            }
            if self.inp.comp == "zlib@openssh.com" {
                self.inp.start_compression(false);
            }
        }
        r
    }

    fn write_packet_raw(&mut self, payload: &[u8]) -> SshResult<()> {
        let (bs, len_excluded) = match &self.out.cipher {
            Cipher::None => (8, false),
            Cipher::Ctr { mac, .. } => (16, mac.etm),
            Cipher::Gcm { .. } => (16, true),
            Cipher::ChaCha(_) => (8, true),
        };
        let base = 1 + payload.len() + if len_excluded { 0 } else { 4 };
        let mut pad = bs - base % bs;
        if pad < 4 {
            pad += bs;
        }
        let plen = 1 + payload.len() + pad;
        let mut pkt = Vec::with_capacity(4 + plen + 64);
        pkt.extend_from_slice(&(plen as u32).to_be_bytes());
        pkt.push(pad as u8);
        pkt.extend_from_slice(payload);
        pkt.extend_from_slice(&rng::random_bytes(pad));
        let seq = self.out.seq;
        let wire = match &mut self.out.cipher {
            Cipher::None => pkt,
            Cipher::Ctr { ctr, mac, key } => {
                if mac.etm {
                    let mut body = pkt[4..].to_vec();
                    ctr.apply(&mut body);
                    let tag = mac.compute(key, seq, &[&pkt[..4], &body]);
                    let mut w = pkt[..4].to_vec();
                    w.extend_from_slice(&body);
                    w.extend_from_slice(&tag);
                    w
                } else {
                    let tag = mac.compute(key, seq, &[&pkt]);
                    ctr.apply(&mut pkt);
                    pkt.extend_from_slice(&tag);
                    pkt
                }
            }
            Cipher::Gcm { gcm, iv } => {
                let ct = gcm.seal(iv, &pkt[..4], &pkt[4..]);
                incr_iv(iv);
                let mut w = pkt[..4].to_vec();
                w.extend_from_slice(&ct);
                w
            }
            Cipher::ChaCha(c) => c.seal(seq, &pkt),
        };
        self.stream.write_all(&wire)?;
        self.stream.flush()?;
        self.out.seq = seq.wrapping_add(1);
        self.out.bytes += wire.len() as u64;
        self.out.packets += 1;
        Ok(())
    }

    fn read_packet(&mut self) -> SshResult<Vec<u8>> {
        let mut cipher = std::mem::replace(&mut self.inp.cipher, Cipher::None);
        let r = self.read_packet_with(&mut cipher);
        self.inp.cipher = cipher;
        r
    }

    fn read_packet_with(&mut self, cipher: &mut Cipher) -> SshResult<Vec<u8>> {
        let seq = self.inp.seq;
        let mac_err = || SshError::Protocol("message authentication failed (corrupt packet or wrong keys)".into());
        let bad_len = |l: usize| SshError::Protocol(format!("invalid packet length {}", l));
        let mut wire_len = 0usize;
        let body = match cipher {
            Cipher::None => {
                let l = self.take(4)?;
                let len = u32::from_be_bytes([l[0], l[1], l[2], l[3]]) as usize;
                if !(5..=MAX_PACKET).contains(&len) {
                    return Err(bad_len(len));
                }
                wire_len += 4 + len;
                self.take(len)?
            }
            Cipher::Ctr { ctr, mac, key } if !mac.etm => {
                let mut full = self.take(16)?;
                ctr.apply(&mut full);
                let len = u32::from_be_bytes([full[0], full[1], full[2], full[3]]) as usize;
                if len + 4 < 16 || !(len + 4).is_multiple_of(16) || len > MAX_PACKET {
                    return Err(bad_len(len));
                }
                let mut rest = self.take(len + 4 - 16)?;
                ctr.apply(&mut rest);
                full.extend_from_slice(&rest);
                let tag = self.take(mac.tag_len())?;
                if !ct_eq(&mac.compute(key, seq, &[&full]), &tag) {
                    return Err(mac_err());
                }
                wire_len += full.len() + tag.len();
                full.split_off(4)
            }
            Cipher::Ctr { ctr, mac, key } => {
                let lb = self.take(4)?;
                let len = u32::from_be_bytes([lb[0], lb[1], lb[2], lb[3]]) as usize;
                if len < 16 || !len.is_multiple_of(16) || len > MAX_PACKET {
                    return Err(bad_len(len));
                }
                let mut enc = self.take(len)?;
                let tag = self.take(mac.tag_len())?;
                if !ct_eq(&mac.compute(key, seq, &[&lb, &enc]), &tag) {
                    return Err(mac_err());
                }
                wire_len += 4 + len + tag.len();
                ctr.apply(&mut enc);
                enc
            }
            Cipher::Gcm { gcm, iv } => {
                let lb = self.take(4)?;
                let len = u32::from_be_bytes([lb[0], lb[1], lb[2], lb[3]]) as usize;
                if len < 16 || !len.is_multiple_of(16) || len > MAX_PACKET {
                    return Err(bad_len(len));
                }
                let data = self.take(len + 16)?;
                let pt = gcm.open(iv, &lb, &data).ok_or_else(mac_err)?;
                incr_iv(iv);
                wire_len += 4 + len + 16;
                pt
            }
            Cipher::ChaCha(c) => {
                let mut data = self.take(4)?;
                let len = c.decrypt_length(seq, &data) as usize;
                if len < 8 || !len.is_multiple_of(8) || len > MAX_PACKET {
                    return Err(bad_len(len));
                }
                data.extend_from_slice(&self.take(len + 16)?);
                wire_len += data.len();
                c.open(seq, &data).ok_or_else(mac_err)?
            }
        };
        let pad = body[0] as usize;
        if pad < 4 || pad + 1 >= body.len() {
            return Err(SshError::protocol(format!("invalid padding length {}", pad)));
        }
        let mut payload = body[1..body.len() - pad].to_vec();
        if let Some(inf) = self.inp.inflater.as_mut() {
            payload = inf.decompress(&payload).map_err(|e| SshError::protocol(format!("decompression failed: {}", e)))?;
            if payload.is_empty() {
                return Err(SshError::protocol("empty packet after decompression"));
            }
        }
        // client: delayed compression starts after USERAUTH_SUCCESS was received
        if self.is_client() && !self.authenticated && payload.first() == Some(&msg::USERAUTH_SUCCESS) {
            self.authenticated = true;
            if self.out.comp == "zlib@openssh.com" {
                self.out.start_compression(true);
            }
            if self.inp.comp == "zlib@openssh.com" {
                self.inp.start_compression(false);
            }
        }
        self.last_recv_seq = seq;
        self.inp.seq = seq.wrapping_add(1);
        self.inp.bytes += wire_len as u64;
        self.inp.packets += 1;
        Ok(payload)
    }

    // ------------------------------------------------------------ public packet API

    fn needs_rekey(&self) -> bool {
        let lim = self.cfg.rekey_limit;
        !self.session_id.is_empty() && (self.out.bytes > lim || self.inp.bytes > lim || self.out.packets > 1 << 28 || self.inp.packets > 1 << 28)
    }

    /// Sends a payload (rekeying first when the data limit was reached).
    pub fn send(&mut self, payload: &[u8]) -> SshResult<()> {
        if self.needs_rekey() {
            self.kex(None)?;
        }
        self.write_packet(payload)
    }

    /// Receives the next payload above the transport layer. Transport messages
    /// (IGNORE, DEBUG, UNIMPLEMENTED, EXT_INFO, key re-exchange) are handled internally;
    /// DISCONNECT becomes [`SshError::Disconnected`].
    pub fn recv(&mut self) -> SshResult<Vec<u8>> {
        loop {
            if let Some(p) = self.pending.pop_front() {
                return Ok(p);
            }
            if self.needs_rekey() {
                self.kex(None)?;
                continue;
            }
            let p = self.read_packet()?;
            match p[0] {
                msg::DISCONNECT => return Err(parse_disconnect(&p)),
                msg::IGNORE | msg::DEBUG | msg::UNIMPLEMENTED => continue,
                msg::EXT_INFO if self.is_client() => {
                    self.parse_ext_info(&p);
                    continue;
                }
                msg::KEXINIT => {
                    self.kex(Some(p))?;
                    continue;
                }
                21..=49 => return Err(SshError::protocol(format!("unexpected key exchange message {}", p[0]))),
                _ => return Ok(p),
            }
        }
    }

    fn parse_ext_info(&mut self, p: &[u8]) {
        let mut r = Reader::new(&p[1..]);
        let n = r.u32().unwrap_or(0);
        for _ in 0..n {
            let (name, value) = match (r.str(), r.str()) {
                (Ok(a), Ok(b)) => (a, b),
                _ => return,
            };
            if name == "server-sig-algs" {
                self.server_sig_algs = Some(value.split(',').map(|s| s.to_string()).collect());
            }
        }
    }

    /// Puts a payload back so that the next [`recv`](Self::recv) returns it.
    pub fn push_front(&mut self, payload: Vec<u8>) {
        self.pending.push_front(payload);
    }

    /// Answers the last received packet with `SSH_MSG_UNIMPLEMENTED`.
    pub fn send_unimplemented(&mut self) -> SshResult<()> {
        let p = Writer::msg(msg::UNIMPLEMENTED).u32(self.last_recv_seq).finish();
        self.write_packet(&p)
    }

    /// Initiates a key re-exchange now.
    pub fn rekey(&mut self) -> SshResult<()> {
        self.kex(None)
    }

    /// Sends `SSH_MSG_DISCONNECT` (errors ignored).
    pub fn disconnect(&mut self, reason: u32, message: &str) {
        let p = Writer::msg(msg::DISCONNECT).u32(reason).str(message).str("").finish();
        let _ = self.write_packet(&p);
    }

    // ------------------------------------------------------------ key exchange

    fn build_kexinit(&self, first: bool) -> Vec<u8> {
        let a = &self.cfg.algorithms;
        let mut w = Writer::msg(msg::KEXINIT);
        w.raw(&rng::random_bytes(16));
        let mut kex = a.kex.clone();
        let host_keys: Vec<String> = match &self.role {
            Role::Client(_) => {
                if first {
                    kex.push("ext-info-c".into());
                    kex.push(STRICT_C.into());
                }
                a.host_key.clone()
            }
            Role::Server(keys) => {
                if first {
                    kex.push(STRICT_S.into());
                }
                let mut algs: Vec<String> = Vec::new();
                for c in &self.cfg.host_certificates {
                    if keys.iter().any(|k| k.public_key() == c.key) {
                        for alg in c.algorithms() {
                            if !algs.contains(&alg) {
                                algs.push(alg);
                            }
                        }
                    }
                }
                algs.extend(a.host_key.iter().filter(|alg| keys.iter().any(|k| k.public_key().supports_algorithm(alg))).cloned());
                algs
            }
        };
        w.name_list(&kex).name_list(&host_keys);
        w.name_list(&a.ciphers).name_list(&a.ciphers);
        w.name_list(&a.macs).name_list(&a.macs);
        let comp = if self.cfg.compression.is_empty() { vec!["none".to_string()] } else { self.cfg.compression.clone() };
        w.name_list(&comp).name_list(&comp);
        w.str("").str("");
        w.bool(false).u32(0);
        w.finish()
    }

    /// Reads the next key exchange message; earlier non-KEX packets are queued
    /// (except during a strict initial exchange).
    fn next_kex_msg(&mut self, may_queue: bool) -> SshResult<Vec<u8>> {
        loop {
            let p = self.read_packet()?;
            match p[0] {
                msg::DISCONNECT => return Err(parse_disconnect(&p)),
                msg::IGNORE | msg::DEBUG | msg::UNIMPLEMENTED if !self.strict_initial => continue,
                20..=49 => return Ok(p),
                t if may_queue && !self.session_id.is_empty() && t >= 50 => self.pending.push_back(p),
                t => {
                    self.disconnect(reason::PROTOCOL_ERROR, "unexpected message during key exchange");
                    return Err(SshError::protocol(format!("unexpected message {} during key exchange", t)));
                }
            }
        }
    }

    fn kex(&mut self, peer_kexinit: Option<Vec<u8>>) -> SshResult<()> {
        let r = self.kex_inner(peer_kexinit);
        if let Err(e) = &r {
            if !matches!(e, SshError::Disconnected { .. } | SshError::Io(_)) {
                let code = if matches!(e, SshError::HostKey(_)) { reason::HOST_KEY_NOT_VERIFIABLE } else { reason::KEY_EXCHANGE_FAILED };
                self.disconnect(code, &e.to_string());
            }
        }
        r
    }

    fn kex_inner(&mut self, peer_kexinit: Option<Vec<u8>>) -> SshResult<()> {
        let first = self.session_id.is_empty();
        let ours = self.build_kexinit(first);
        self.write_packet(&ours)?;
        let packets_before = self.inp.packets;
        let theirs = match peer_kexinit {
            Some(p) => p,
            None => {
                let p = self.next_kex_msg(true)?;
                if p[0] != msg::KEXINIT {
                    return Err(SshError::protocol(format!("expected KEXINIT, got message {}", p[0])));
                }
                p
            }
        };
        let client = self.is_client();
        let (i_c, i_s) = if client { (ours, theirs) } else { (theirs, ours) };
        let pc = parse_kexinit(&i_c)?;
        let ps = parse_kexinit(&i_s)?;
        if first {
            self.strict = pc.kex.iter().any(|k| k == STRICT_C) && ps.kex.iter().any(|k| k == STRICT_S);
            if self.strict && (packets_before > 0 || self.inp.packets != 1) {
                return Err(SshError::protocol("strict KEX: KEXINIT was not the first packet"));
            }
            self.strict_initial = self.strict;
            self.peer_ext_info = !client && pc.kex.iter().any(|k| k == "ext-info-c");
        }
        let kex = choose("key exchange", &pc.kex, &ps.kex)?;
        if !SUPPORTED_KEX.contains(&kex.as_str()) {
            return Err(SshError::Negotiation(format!("unsupported key exchange {}", kex)));
        }
        let host_key = choose("host key", &pc.host_key, &ps.host_key)?;
        let cipher_c2s = choose("cipher (client to server)", &pc.enc[0], &ps.enc[0])?;
        let cipher_s2c = choose("cipher (server to client)", &pc.enc[1], &ps.enc[1])?;
        let mac_c2s = if cipher_params(&cipher_c2s).2 { String::new() } else { choose("MAC (client to server)", &pc.mac[0], &ps.mac[0])? };
        let mac_s2c = if cipher_params(&cipher_s2c).2 { String::new() } else { choose("MAC (server to client)", &pc.mac[1], &ps.mac[1])? };
        let comp_c2s = choose("compression", &pc.comp[0], &ps.comp[0])?;
        let comp_s2c = choose("compression", &pc.comp[1], &ps.comp[1])?;
        let n = Negotiated { kex, host_key, cipher_c2s, cipher_s2c, mac_c2s, mac_s2c, comp_c2s, comp_s2c };
        // a wrongly guessed first KEX packet from the peer must be ignored
        let peer = if client { &ps } else { &pc };
        let ignore_next = peer.first_follows && (peer.kex.first() != Some(&n.kex) || peer.host_key.first() != Some(&n.host_key));

        let (k_mp, h) = if client { self.kex_client(&n, &i_c, &i_s, first)? } else { self.kex_server(&n, &i_c, &i_s, ignore_next)? };
        if first {
            self.session_id = h.clone();
        }
        let derive = |letter: u8, len: usize| -> Vec<u8> {
            let mut hasher = Sha256::new();
            hasher.update(&k_mp);
            hasher.update(&h);
            hasher.update(&[letter]);
            hasher.update(&self.session_id);
            let mut out = hasher.finish();
            while out.len() < len {
                let mut hasher = Sha256::new();
                hasher.update(&k_mp);
                hasher.update(&h);
                hasher.update(&out);
                out.extend(hasher.finish());
            }
            out.truncate(len);
            out
        };
        let build = |cipher: &str, mac: &str, iv_l: u8, key_l: u8, mac_l: u8| -> Cipher {
            let (kl, il, aead) = cipher_params(cipher);
            let mac_len = if aead { 0 } else { MacAlg::from_name(mac).map(|m| m.tag_len()).unwrap_or(0) };
            make_cipher(cipher, mac, &derive(iv_l, il), &derive(key_l, kl), derive(mac_l, mac_len))
        };
        let c2s = build(&n.cipher_c2s, &n.mac_c2s, b'A', b'C', b'E');
        let s2c = build(&n.cipher_s2c, &n.mac_s2c, b'B', b'D', b'F');
        let (new_out, new_in) = if client { (c2s, s2c) } else { (s2c, c2s) };

        let (comp_out, comp_in) = if client { (n.comp_c2s.clone(), n.comp_s2c.clone()) } else { (n.comp_s2c.clone(), n.comp_c2s.clone()) };
        self.write_packet(&[msg::NEWKEYS])?;
        self.out.cipher = new_out;
        if self.out.comp != comp_out {
            self.out.comp = comp_out;
            self.out.deflater = None;
            if self.out.comp == "zlib" || (self.out.comp == "zlib@openssh.com" && self.authenticated) {
                self.out.start_compression(true);
            }
        }
        self.out.bytes = 0;
        self.out.packets = 0;
        if self.strict {
            self.out.seq = 0;
        }
        let p = self.next_kex_msg(false)?;
        if p[0] != msg::NEWKEYS {
            return Err(SshError::protocol(format!("expected NEWKEYS, got message {}", p[0])));
        }
        self.inp.cipher = new_in;
        if self.inp.comp != comp_in {
            self.inp.comp = comp_in;
            self.inp.inflater = None;
            if self.inp.comp == "zlib" || (self.inp.comp == "zlib@openssh.com" && self.authenticated) {
                self.inp.start_compression(false);
            }
        }
        self.inp.bytes = 0;
        self.inp.packets = 0;
        if self.strict {
            self.inp.seq = 0;
        }
        self.strict_initial = false;
        self.negotiated = n;
        self.kex_count += 1;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn exchange_hash(v_c: &[u8], v_s: &[u8], i_c: &[u8], i_s: &[u8], k_s: &[u8], q_c: &[u8], q_s: &[u8], k_mp: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.string(v_c).string(v_s).string(i_c).string(i_s).string(k_s).string(q_c).string(q_s).raw(k_mp);
        Sha256::digest(&w.buf)
    }

    fn kex_client(&mut self, n: &Negotiated, i_c: &[u8], i_s: &[u8], first: bool) -> SshResult<(Vec<u8>, Vec<u8>)> {
        let (secret, q_c) = ephemeral(&n.kex);
        self.write_packet(&Writer::msg(msg::KEX_ECDH_INIT).string(&q_c).finish())?;
        let p = self.next_kex_msg(false)?;
        if p[0] != msg::KEX_ECDH_REPLY {
            return Err(SshError::protocol(format!("expected KEX_ECDH_REPLY, got message {}", p[0])));
        }
        let mut r = Reader::new(&p[1..]);
        let k_s = r.string()?.to_vec();
        let q_s = r.string()?.to_vec();
        let sig = r.string()?.to_vec();
        let shared = shared_secret(&n.kex, &secret, &q_s)?;
        let k_mp = mpint_bytes(&shared);
        let h = Self::exchange_hash(&self.v_local, &self.v_remote, i_c, i_s, &k_s, &q_c, &q_s, &k_mp);
        let (key, cert, alg) = match crate::cert::plain_algorithm(&n.host_key) {
            Some(plain) => {
                let c = crate::cert::SshCertificate::from_blob(&k_s).map_err(|e| SshError::HostKey(e.to_string()))?;
                (c.key.clone(), Some(c), plain.to_string())
            }
            None => (SshPublicKey::from_blob(&k_s).map_err(|e| SshError::HostKey(e.to_string()))?, None, n.host_key.clone()),
        };
        if !key.supports_algorithm(&alg) {
            return Err(SshError::HostKey(format!("server sent a {} key for algorithm {}", key.key_type(), n.host_key)));
        }
        if !key.verify(&alg, &h, &sig) {
            return Err(SshError::HostKey("invalid host key signature over the exchange hash".into()));
        }
        if first {
            if let Role::Client(verifier) = &mut self.role {
                verifier(&key, cert.as_ref())?;
            }
            self.host_key_blob = Some(k_s);
        } else if self.host_key_blob.as_deref() != Some(&k_s[..]) {
            return Err(SshError::HostKey("server host key changed during key re-exchange".into()));
        }
        Ok((k_mp, h))
    }

    fn kex_server(&mut self, n: &Negotiated, i_c: &[u8], i_s: &[u8], ignore_next: bool) -> SshResult<(Vec<u8>, Vec<u8>)> {
        let mut p = self.next_kex_msg(false)?;
        if ignore_next {
            p = self.next_kex_msg(false)?;
        }
        if p[0] != msg::KEX_ECDH_INIT {
            return Err(SshError::protocol(format!("expected KEX_ECDH_INIT, got message {}", p[0])));
        }
        let q_c = Reader::new(&p[1..]).string()?.to_vec();
        let (secret, q_s) = ephemeral(&n.kex);
        let shared = shared_secret(&n.kex, &secret, &q_c)?;
        let k_mp = mpint_bytes(&shared);
        let plain = crate::cert::plain_algorithm(&n.host_key).unwrap_or(&n.host_key).to_string();
        let host_key = match &self.role {
            Role::Server(keys) => keys.iter().find(|k| k.public_key().supports_algorithm(&plain)).cloned(),
            Role::Client(_) => None,
        }
        .ok_or_else(|| SshError::Negotiation("no host key for the negotiated algorithm".into()))?;
        let k_s = if plain != n.host_key {
            let pk = host_key.public_key();
            self.cfg.host_certificates.iter().find(|c| c.key == pk).map(|c| c.blob.clone()).ok_or_else(|| SshError::Negotiation("no host certificate for the negotiated algorithm".into()))?
        } else {
            host_key.public_key().to_blob()
        };
        let h = Self::exchange_hash(&self.v_remote, &self.v_local, i_c, i_s, &k_s, &q_c, &q_s, &k_mp);
        let sig = host_key.sign(&plain, &h)?;
        self.write_packet(&Writer::msg(msg::KEX_ECDH_REPLY).string(&k_s).string(&q_s).string(&sig).finish())?;
        Ok((k_mp, h))
    }
}

/// Generates an ephemeral key pair for the key exchange method: (secret, public).
fn ephemeral(kex: &str) -> (Vec<u8>, Vec<u8>) {
    if kex.starts_with("curve25519") {
        let mut s = [0u8; 32];
        rng::fill(&mut s);
        let p = x25519_base(&s);
        (s.to_vec(), p.to_vec())
    } else {
        let s = EcCurve::P256.generate_private();
        let p = EcCurve::P256.public_key(&s).expect("valid scalar");
        (s, p)
    }
}

/// Computes the shared secret (raw bytes, interpreted as an unsigned big-endian integer).
fn shared_secret(kex: &str, secret: &[u8], peer: &[u8]) -> SshResult<Vec<u8>> {
    if kex.starts_with("curve25519") {
        let peer: [u8; 32] = peer.try_into().map_err(|_| SshError::protocol("bad curve25519 public key length"))?;
        let s: [u8; 32] = secret.try_into().expect("32-byte secret");
        let k = x25519(&s, &peer);
        if k.iter().all(|&b| b == 0) {
            return Err(SshError::protocol("curve25519 shared secret is zero (low order point)"));
        }
        Ok(k.to_vec())
    } else {
        EcCurve::P256.ecdh(secret, peer).ok_or_else(|| SshError::protocol("invalid ECDH public point"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn exchange_hash_is_consistent_between_peers() {
        // both sides compute the same curve25519 secret and therefore the same H
        let (sa, pa) = ephemeral("curve25519-sha256");
        let (sb, pb) = ephemeral("curve25519-sha256");
        let ka = shared_secret("curve25519-sha256", &sa, &pb).unwrap();
        let kb = shared_secret("curve25519-sha256", &sb, &pa).unwrap();
        assert_eq!(ka, kb);
        let h1 = Transport::<TcpStream>::exchange_hash(b"SSH-2.0-a", b"SSH-2.0-b", b"ic", b"is", b"ks", &pa, &pb, &mpint_bytes(&ka));
        let h2 = Transport::<TcpStream>::exchange_hash(b"SSH-2.0-a", b"SSH-2.0-b", b"ic", b"is", b"ks", &pa, &pb, &mpint_bytes(&kb));
        assert_eq!(h1, h2);
        assert!(shared_secret("curve25519-sha256", &sa, &[0u8; 32]).is_err());
        let (sa, pa) = ephemeral("ecdh-sha2-nistp256");
        let (sb, pb) = ephemeral("ecdh-sha2-nistp256");
        assert_eq!(shared_secret("ecdh-sha2-nistp256", &sa, &pb).unwrap(), shared_secret("ecdh-sha2-nistp256", &sb, &pa).unwrap());
        assert!(shared_secret("ecdh-sha2-nistp256", &sa, &pb[1..]).is_err());
    }

    #[test]
    fn curve25519_sha256_known_exchange_hash() {
        // RFC 7748 6.1 Alice/Bob keys; H recomputed from the encoded fields
        let a = cameleer_tls::util::unhex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let b_pub = cameleer_tls::util::unhex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
        let k = shared_secret("curve25519-sha256", &a, &b_pub).unwrap();
        assert_eq!(cameleer_tls::util::hex(&k), "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
        let k_mp = mpint_bytes(&k);
        assert_eq!(&k_mp[..4], &[0, 0, 0, 32]);
        let h = Transport::<TcpStream>::exchange_hash(b"V_C", b"V_S", b"I_C", b"I_S", b"K_S", b"Q_C", b"Q_S", &k_mp);
        let mut manual = Vec::new();
        for f in [&b"V_C"[..], b"V_S", b"I_C", b"I_S", b"K_S", b"Q_C", b"Q_S"] {
            manual.extend_from_slice(&(f.len() as u32).to_be_bytes());
            manual.extend_from_slice(f);
        }
        manual.extend_from_slice(&k_mp);
        assert_eq!(h, Sha256::digest(&manual));
    }

    fn pair(cfg_c: TransportConfig, cfg_s: TransportConfig, keys: Vec<SshPrivateKey>) -> (Transport<TcpStream>, Transport<TcpStream>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let srv = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
            Transport::server(s, cfg_s, keys).unwrap()
        });
        let cs = TcpStream::connect(addr).unwrap();
        cs.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        let c = Transport::client(cs, cfg_c, Box::new(|_, _| Ok(()))).unwrap();
        (c, srv.join().unwrap())
    }

    #[test]
    fn handshake_all_ciphers_and_rekey() {
        for cipher in SUPPORTED_CIPHERS {
            for mac in ["hmac-sha2-256", "hmac-sha2-512-etm@openssh.com"] {
                let mut cc = TransportConfig::default();
                cc.algorithms.ciphers = vec![cipher.to_string()];
                cc.algorithms.macs = vec![mac.to_string()];
                cc.rekey_limit = 4096;
                let (mut c, mut s) = pair(cc, TransportConfig::default(), vec![SshPrivateKey::generate_ed25519()]);
                assert!(c.strict_kex() && s.strict_kex());
                assert_eq!(c.session_id(), s.session_id());
                assert_eq!(&c.negotiated().cipher_c2s, cipher);
                let t = std::thread::spawn(move || {
                    while let Ok(p) = s.recv() {
                        s.send(&p).unwrap();
                    }
                    s.kex_count()
                });
                for i in 0..20u8 {
                    let p = vec![200 + (i % 50), i, 1, 2, 3].into_iter().chain(std::iter::repeat_n(i, 1000)).collect::<Vec<u8>>();
                    c.send(&p).unwrap();
                    assert_eq!(c.recv().unwrap(), p);
                }
                let kc = c.kex_count();
                c.disconnect(reason::BY_APPLICATION, "done");
                drop(c);
                let ks = t.join().unwrap();
                assert!(kc > 2 && ks > 2, "rekeyed {} / {} times", kc, ks);
            }
        }
    }
}
