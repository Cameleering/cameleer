//! SSH client: user authentication (RFC 4252 `publickey`, `password`, RFC 4256
//! `keyboard-interactive`) and the connection protocol (RFC 4254) with
//! multiplexed session channels (`exec`, `shell`, `subsystem`, `pty-req`, `env`).
//!
//! The client is synchronous: every channel operation drives the single
//! connection, buffering data that arrives for other channels.

use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::error::{SshError, SshResult};
use crate::keys::{SshPrivateKey, SshPublicKey};
use crate::known_hosts::{HostKeyStatus, KnownHosts};
use crate::transport::{msg, reason, HostKeyVerifier, Transport, TransportConfig};
use crate::wire::{Reader, Writer};

/// How the server host key is verified.
#[derive(Clone, Debug)]
pub enum HostKeyCheck {
    /// Accept any host key. Insecure: vulnerable to man-in-the-middle attacks.
    AcceptAny,
    /// Check against `known_hosts` entries. Unknown hosts are accepted only when
    /// `strict` is false; changed or revoked keys are always rejected.
    KnownHosts { known: KnownHosts, strict: bool },
    /// Accept only keys with one of these `SHA256:...` fingerprints.
    Fingerprints(Vec<String>),
}

/// Client connection settings.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub private_keys: Vec<SshPrivateKey>,
    /// OpenSSH user certificates for keys in `private_keys` (tried before the plain key).
    pub certificates: Vec<crate::cert::SshCertificate>,
    /// SSH agent socket whose identities are tried after `private_keys`.
    pub agent: Option<std::path::PathBuf>,
    pub host_key_check: HostKeyCheck,
    pub transport: TransportConfig,
    pub connect_timeout: Duration,
    /// Socket read timeout (None = block forever).
    pub timeout: Option<Duration>,
    /// Authentication methods in preference order.
    pub preferred_auth: Vec<String>,
    /// Initial receive window per channel.
    pub window_size: u32,
    /// Maximum channel data packet accepted.
    pub max_packet: u32,
}

impl ClientConfig {
    /// Settings for `user@host:port` with password-less, host-key-unchecked defaults.
    pub fn new(host: &str, port: u16, username: &str) -> ClientConfig {
        ClientConfig {
            host: host.to_string(),
            port,
            username: username.to_string(),
            password: None,
            private_keys: Vec::new(),
            certificates: Vec::new(),
            agent: None,
            host_key_check: HostKeyCheck::AcceptAny,
            transport: TransportConfig::default(),
            connect_timeout: Duration::from_secs(10),
            timeout: Some(Duration::from_secs(60)),
            preferred_auth: vec!["publickey".into(), "keyboard-interactive".into(), "password".into()],
            window_size: 2 * 1024 * 1024,
            max_packet: 32768,
        }
    }
    pub fn password(mut self, p: &str) -> Self {
        self.password = Some(p.to_string());
        self
    }
    pub fn private_key(mut self, k: SshPrivateKey) -> Self {
        self.private_keys.push(k);
        self
    }
    pub fn host_key_check(mut self, c: HostKeyCheck) -> Self {
        self.host_key_check = c;
        self
    }
}

/// Output of a finished command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_status: Option<u32>,
    pub exit_signal: Option<String>,
}

#[derive(Default)]
struct ChannelState {
    remote_id: u32,
    remote_window: u64,
    remote_max: u32,
    /// Receive window still granted to the peer.
    local_window: u32,
    consumed: u32,
    stdout: VecDeque<u8>,
    stderr: Vec<u8>,
    eof: bool,
    closed: bool,
    close_sent: bool,
    eof_sent: bool,
    exit_status: Option<u32>,
    exit_signal: Option<String>,
    replies: VecDeque<bool>,
    open: Option<Result<(), String>>,
}

/// An authenticated SSH client connection.
pub struct SshClient {
    t: Transport<TcpStream>,
    channels: HashMap<u32, ChannelState>,
    next_id: u32,
    window: u32,
    max_packet: u32,
    banner: Option<String>,
    host_key: SshPublicKey,
    auth_method: String,
}

fn connect_tcp(host: &str, port: u16, timeout: Duration) -> SshResult<TcpStream> {
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs()?.collect();
    let mut last = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map(SshError::Io).unwrap_or_else(|| SshError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, format!("unknown host {}", host)))))
}

fn verifier(check: HostKeyCheck, host: String, port: u16) -> HostKeyVerifier {
    Box::new(move |key: &SshPublicKey, cert: Option<&crate::cert::SshCertificate>| match &check {
        HostKeyCheck::KnownHosts { known, strict } if cert.is_some() => match known.check_certificate(&host, port, cert.unwrap()) {
            HostKeyStatus::Trusted => Ok(()),
            HostKeyStatus::Revoked => Err(SshError::HostKey(format!("the host certificate of {} is revoked", host))),
            _ if !strict && !known.has_cert_authority(&host, port) => Ok(()),
            _ => Err(SshError::HostKey(format!("the host certificate of {} is not signed by a trusted @cert-authority (or invalid for this host / time)", host))),
        },
        HostKeyCheck::AcceptAny => Ok(()),
        HostKeyCheck::Fingerprints(list) => {
            let fp = key.fingerprint();
            if list.iter().any(|f| f == &fp || f.trim_start_matches("SHA256:") == fp.trim_start_matches("SHA256:")) {
                Ok(())
            } else {
                Err(SshError::HostKey(format!("{} key {} of {} is not among the accepted fingerprints", key.key_type(), fp, host)))
            }
        }
        HostKeyCheck::KnownHosts { known, strict } => match known.check(&host, port, key) {
            HostKeyStatus::Trusted => Ok(()),
            HostKeyStatus::Unknown if !strict => Ok(()),
            HostKeyStatus::Unknown => Err(SshError::HostKey(format!(
                "no {} host key is known for {} (strict host key checking; server key fingerprint {})",
                key.key_type(),
                crate::known_hosts::host_name(&host, port),
                key.fingerprint()
            ))),
            HostKeyStatus::Changed => Err(SshError::HostKey(format!(
                "REMOTE HOST IDENTIFICATION HAS CHANGED for {}: the {} key {} does not match known_hosts (possible man-in-the-middle attack)",
                crate::known_hosts::host_name(&host, port),
                key.key_type(),
                key.fingerprint()
            ))),
            HostKeyStatus::Revoked => Err(SshError::HostKey(format!("the {} key {} of {} is revoked", key.key_type(), key.fingerprint(), host))),
        },
    })
}

impl SshClient {
    /// Connects, verifies the host key and authenticates.
    pub fn connect(cfg: &ClientConfig) -> SshResult<SshClient> {
        let tcp = connect_tcp(&cfg.host, cfg.port, cfg.connect_timeout)?;
        tcp.set_read_timeout(cfg.timeout)?;
        tcp.set_write_timeout(cfg.timeout)?;
        let _ = tcp.set_nodelay(true);
        let mut tcfg = cfg.transport.clone();
        // with @cert-authority entries for the host, prefer host certificates (like OpenSSH)
        if let HostKeyCheck::KnownHosts { known, .. } = &cfg.host_key_check {
            if known.has_cert_authority(&cfg.host, cfg.port) && !tcfg.algorithms.host_key.iter().any(|a| crate::cert::is_cert_algorithm(a)) {
                let mut algs: Vec<String> = crate::cert::CERT_ALGORITHMS.iter().map(|s| s.to_string()).collect();
                algs.extend(tcfg.algorithms.host_key.iter().cloned());
                tcfg.algorithms.host_key = algs;
            }
        }
        let t = Transport::client(tcp, tcfg, verifier(cfg.host_key_check.clone(), cfg.host.clone(), cfg.port))?;
        let blob = t.host_key_blob().unwrap_or_default();
        let host_key = match crate::cert::SshCertificate::from_blob(blob) {
            Ok(c) => c.key,
            Err(_) => SshPublicKey::from_blob(blob)?,
        };
        let mut c = SshClient {
            t,
            channels: HashMap::new(),
            next_id: 0,
            window: cfg.window_size,
            max_packet: cfg.max_packet,
            banner: None,
            host_key,
            auth_method: String::new(),
        };
        if let Err(e) = c.authenticate(cfg) {
            let code = if matches!(e, SshError::Auth(_)) { reason::NO_MORE_AUTH_METHODS } else { reason::BY_APPLICATION };
            c.t.disconnect(code, "authentication failed");
            return Err(e);
        }
        Ok(c)
    }

    /// The transport (negotiated algorithms, session id...).
    pub fn transport(&self) -> &Transport<TcpStream> {
        &self.t
    }

    /// Mutable transport access (e.g. to force a rekey).
    pub fn transport_mut(&mut self) -> &mut Transport<TcpStream> {
        &mut self.t
    }

    /// The verified server host key.
    pub fn host_key(&self) -> &SshPublicKey {
        &self.host_key
    }

    /// The authentication banner sent by the server, if any.
    pub fn banner(&self) -> Option<&str> {
        self.banner.as_deref()
    }

    /// The method that authenticated the user (`publickey`, `password`, ...).
    pub fn auth_method(&self) -> &str {
        &self.auth_method
    }

    // ------------------------------------------------------------ authentication

    fn recv_auth(&mut self) -> SshResult<Vec<u8>> {
        loop {
            let p = self.t.recv()?;
            if p[0] == msg::USERAUTH_BANNER {
                let text = Reader::new(&p[1..]).str().unwrap_or_default();
                self.banner.get_or_insert_with(String::new).push_str(&text);
                continue;
            }
            if p[0] == msg::GLOBAL_REQUEST {
                self.handle_global_request(&p)?;
                continue;
            }
            return Ok(p);
        }
    }

    fn auth_request(&self, user: &str, method: &str) -> Writer {
        let mut w = Writer::msg(msg::USERAUTH_REQUEST);
        w.str(user).str("ssh-connection").str(method);
        w
    }

    /// Returns Ok(true) on success, Ok(false) on failure (updating the allowed methods).
    fn auth_outcome(p: &[u8], allowed: &mut Vec<String>) -> SshResult<bool> {
        match p[0] {
            msg::USERAUTH_SUCCESS => Ok(true),
            msg::USERAUTH_FAILURE => {
                let mut r = Reader::new(&p[1..]);
                *allowed = r.name_list()?;
                Ok(false)
            }
            t => Err(SshError::protocol(format!("unexpected message {} during authentication", t))),
        }
    }

    fn authenticate(&mut self, cfg: &ClientConfig) -> SshResult<()> {
        self.t.send(&Writer::msg(msg::SERVICE_REQUEST).str("ssh-userauth").finish())?;
        let p = self.recv_auth()?;
        if p[0] != msg::SERVICE_ACCEPT {
            return Err(SshError::protocol(format!("expected SERVICE_ACCEPT, got message {}", p[0])));
        }
        let user = cfg.username.clone();
        self.t.send(&self.auth_request(&user, "none").finish())?;
        let p = self.recv_auth()?;
        let mut allowed = Vec::new();
        if Self::auth_outcome(&p, &mut allowed)? {
            self.auth_method = "none".into();
            return Ok(());
        }
        let mut tried = Vec::new();
        for method in &cfg.preferred_auth {
            if !allowed.contains(method) {
                continue;
            }
            let ok = match method.as_str() {
                "publickey" if !cfg.private_keys.is_empty() || cfg.agent.is_some() => {
                    tried.push("publickey");
                    let mut ok = false;
                    for key in &cfg.private_keys {
                        let pk = key.public_key();
                        if let Some(c) = cfg.certificates.iter().find(|c| c.key == pk) {
                            if self.auth_publickey_with(&user, key, Some(c), &mut allowed)? {
                                ok = true;
                                break;
                            }
                        }
                        if self.auth_publickey(&user, key, &mut allowed)? {
                            ok = true;
                            break;
                        }
                        if !allowed.iter().any(|m| m == "publickey") {
                            break;
                        }
                    }
                    if !ok && allowed.iter().any(|m| m == "publickey") {
                        if let Some(path) = &cfg.agent {
                            let mut agent = crate::agent::AgentClient::connect(path)?;
                            for id in agent.identities()? {
                                if self.auth_publickey_agent(&user, &mut agent, &id, &mut allowed)? {
                                    ok = true;
                                    break;
                                }
                                if !allowed.iter().any(|m| m == "publickey") {
                                    break;
                                }
                            }
                        }
                    }
                    ok
                }
                "password" if cfg.password.is_some() => {
                    tried.push("password");
                    let mut w = self.auth_request(&user, "password");
                    w.bool(false).str(cfg.password.as_deref().unwrap_or(""));
                    self.t.send(&w.finish())?;
                    let p = self.recv_auth()?;
                    if p[0] == msg::USERAUTH_60 {
                        return Err(SshError::Auth("the server requires a password change".into()));
                    }
                    Self::auth_outcome(&p, &mut allowed)?
                }
                "keyboard-interactive" if cfg.password.is_some() => {
                    tried.push("keyboard-interactive");
                    self.auth_keyboard_interactive(&user, cfg.password.as_deref().unwrap_or(""), &mut allowed)?
                }
                _ => continue,
            };
            if ok {
                self.auth_method = method.clone();
                return Ok(());
            }
        }
        Err(SshError::Auth(format!(
            "user '{}' could not authenticate (tried: {}; server allows: {})",
            user,
            if tried.is_empty() { "nothing".to_string() } else { tried.join(",") },
            allowed.join(",")
        )))
    }

    fn auth_publickey(&mut self, user: &str, key: &SshPrivateKey, allowed: &mut Vec<String>) -> SshResult<bool> {
        self.auth_publickey_with(user, key, None, allowed)
    }

    /// Public key authentication with the plain key or, given `cert`, with the certificate
    /// (algorithm `*-cert-v01@openssh.com`, signed by the certified key).
    fn auth_publickey_with(&mut self, user: &str, key: &SshPrivateKey, cert: Option<&crate::cert::SshCertificate>, allowed: &mut Vec<String>) -> SshResult<bool> {
        let public = key.public_key();
        if let Some(c) = cert {
            let plain = match &public {
                SshPublicKey::Rsa { .. } => match self.t.server_sig_algs() {
                    Some(algs) if algs.iter().any(|a| a == "rsa-sha2-512") => "rsa-sha2-512",
                    _ => "rsa-sha2-256",
                },
                other => other.signature_algorithms()[0],
            };
            let alg = format!("{}-cert-v01@openssh.com", plain);
            let mut data = Writer::new();
            data.string(self.t.session_id());
            data.u8(msg::USERAUTH_REQUEST).str(user).str("ssh-connection").str("publickey").bool(true).str(&alg).string(&c.blob);
            let sig = key.sign(plain, &data.buf)?;
            let mut w = self.auth_request(user, "publickey");
            w.bool(true).str(&alg).string(&c.blob).string(&sig);
            self.t.send(&w.finish())?;
            let p = self.recv_auth()?;
            return Self::auth_outcome(&p, allowed);
        }
        let alg = match &public {
            SshPublicKey::Rsa { .. } => match self.t.server_sig_algs() {
                Some(algs) if algs.iter().any(|a| a == "rsa-sha2-512") => "rsa-sha2-512",
                _ => "rsa-sha2-256",
            },
            other => other.signature_algorithms()[0],
        };
        let blob = public.to_blob();
        let mut data = Writer::new();
        data.string(self.t.session_id());
        data.u8(msg::USERAUTH_REQUEST).str(user).str("ssh-connection").str("publickey").bool(true).str(alg).string(&blob);
        let sig = key.sign(alg, &data.buf)?;
        let mut w = self.auth_request(user, "publickey");
        w.bool(true).str(alg).string(&blob).string(&sig);
        self.t.send(&w.finish())?;
        let p = self.recv_auth()?;
        Self::auth_outcome(&p, allowed)
    }

    fn auth_publickey_agent(&mut self, user: &str, agent: &mut crate::agent::AgentClient, id: &crate::agent::AgentIdentity, allowed: &mut Vec<String>) -> SshResult<bool> {
        let Ok(public) = id.public_key() else { return Ok(false) };
        let flags = match &public {
            SshPublicKey::Rsa { .. } => match self.t.server_sig_algs() {
                Some(algs) if algs.iter().any(|a| a == "rsa-sha2-512") => crate::agent::RSA_SHA2_512,
                _ => crate::agent::RSA_SHA2_256,
            },
            _ => 0,
        };
        let alg = crate::agent::algorithm_for(&public, flags);
        let mut data = Writer::new();
        data.string(self.t.session_id());
        data.u8(msg::USERAUTH_REQUEST).str(user).str("ssh-connection").str("publickey").bool(true).str(alg).string(&id.blob);
        let sig = agent.sign(&id.blob, &data.buf, flags)?;
        let mut w = self.auth_request(user, "publickey");
        w.bool(true).str(alg).string(&id.blob).string(&sig);
        self.t.send(&w.finish())?;
        let p = self.recv_auth()?;
        Self::auth_outcome(&p, allowed)
    }

    fn auth_keyboard_interactive(&mut self, user: &str, password: &str, allowed: &mut Vec<String>) -> SshResult<bool> {
        let mut w = self.auth_request(user, "keyboard-interactive");
        w.str("").str("");
        self.t.send(&w.finish())?;
        let mut answered_password = false;
        for _ in 0..16 {
            let p = self.recv_auth()?;
            if p[0] != msg::USERAUTH_60 {
                return Self::auth_outcome(&p, allowed);
            }
            let mut r = Reader::new(&p[1..]);
            let _name = r.str()?;
            let _instruction = r.str()?;
            let _lang = r.str()?;
            let n = r.u32()?;
            if n > 64 {
                return Err(SshError::protocol("too many keyboard-interactive prompts"));
            }
            let mut resp = Writer::msg(msg::USERAUTH_INFO_RESPONSE);
            resp.u32(n);
            for _ in 0..n {
                let _prompt = r.str()?;
                let _echo = r.bool()?;
                resp.str(password);
            }
            if n > 0 {
                if answered_password {
                    // the server asks again: the password was wrong; do not loop forever
                    resp = Writer::msg(msg::USERAUTH_INFO_RESPONSE);
                    resp.u32(n);
                    for _ in 0..n {
                        resp.str("");
                    }
                }
                answered_password = true;
            }
            self.t.send(&resp.finish())?;
        }
        Err(SshError::Auth("keyboard-interactive exchange did not terminate".into()))
    }

    // ------------------------------------------------------------ connection protocol

    fn handle_global_request(&mut self, p: &[u8]) -> SshResult<()> {
        let mut r = Reader::new(&p[1..]);
        let _name = r.str()?;
        if r.bool()? {
            self.t.send(&[msg::REQUEST_FAILURE])?;
        }
        Ok(())
    }

    fn chan(&mut self, id: u32) -> SshResult<&mut ChannelState> {
        self.channels.get_mut(&id).ok_or_else(|| SshError::Channel(format!("unknown channel {}", id)))
    }

    /// Reads and dispatches one message.
    fn pump(&mut self) -> SshResult<()> {
        let p = self.t.recv()?;
        let mut r = Reader::new(&p[1..]);
        match p[0] {
            msg::GLOBAL_REQUEST => self.handle_global_request(&p)?,
            msg::REQUEST_SUCCESS | msg::REQUEST_FAILURE | msg::USERAUTH_BANNER => {}
            msg::CHANNEL_OPEN => {
                let _type = r.str()?;
                let sender = r.u32()?;
                let w = Writer::msg(msg::CHANNEL_OPEN_FAILURE).u32(sender).u32(1).str("not supported").str("").finish();
                self.t.send(&w)?;
            }
            t @ msg::CHANNEL_OPEN_CONFIRMATION..=msg::CHANNEL_FAILURE => {
                let id = r.u32()?;
                let window = self.window;
                // messages for channels that are already gone are ignored
                let st = match self.channels.get_mut(&id) {
                    Some(st) => st,
                    None => return Ok(()),
                };
                match t {
                    msg::CHANNEL_OPEN_CONFIRMATION => {
                        st.remote_id = r.u32()?;
                        st.remote_window = r.u32()? as u64;
                        st.remote_max = r.u32()?.max(1);
                        st.open = Some(Ok(()));
                    }
                    msg::CHANNEL_OPEN_FAILURE => {
                        let code = r.u32()?;
                        let desc = r.str()?;
                        st.open = Some(Err(format!("{} (code {})", desc, code)));
                    }
                    msg::CHANNEL_WINDOW_ADJUST => {
                        st.remote_window = (st.remote_window + r.u32()? as u64).min(u32::MAX as u64);
                    }
                    msg::CHANNEL_DATA | msg::CHANNEL_EXTENDED_DATA => {
                        let ext = if t == msg::CHANNEL_EXTENDED_DATA { Some(r.u32()?) } else { None };
                        let data = r.string()?;
                        if data.len() > st.local_window as usize {
                            return Err(SshError::protocol("the server sent more channel data than the window allows"));
                        }
                        st.local_window -= data.len() as u32;
                        match ext {
                            None => st.stdout.extend(data.iter()),
                            Some(_) => st.stderr.extend_from_slice(data),
                        }
                        st.consumed = st.consumed.saturating_add(data.len() as u32);
                        if st.consumed >= window / 2 && !st.closed {
                            let w = Writer::msg(msg::CHANNEL_WINDOW_ADJUST).u32(st.remote_id).u32(st.consumed).finish();
                            st.local_window += st.consumed;
                            st.consumed = 0;
                            self.t.send(&w)?;
                        }
                    }
                    msg::CHANNEL_EOF => st.eof = true,
                    msg::CHANNEL_CLOSE => {
                        st.closed = true;
                        st.eof = true;
                        if st.open.is_none() {
                            st.open = Some(Err("channel closed".into()));
                        }
                        if !st.close_sent {
                            st.close_sent = true;
                            let w = Writer::msg(msg::CHANNEL_CLOSE).u32(st.remote_id).finish();
                            self.t.send(&w)?;
                        }
                    }
                    msg::CHANNEL_REQUEST => {
                        let kind = r.str()?;
                        let want_reply = r.bool()?;
                        match kind.as_str() {
                            "exit-status" => st.exit_status = Some(r.u32()?),
                            "exit-signal" => st.exit_signal = Some(r.str()?),
                            _ => {}
                        }
                        if want_reply {
                            let w = Writer::msg(msg::CHANNEL_FAILURE).u32(st.remote_id).finish();
                            self.t.send(&w)?;
                        }
                    }
                    _ => st.replies.push_back(t == msg::CHANNEL_SUCCESS),
                }
            }
            _ => self.t.send_unimplemented()?,
        }
        Ok(())
    }

    /// Opens a `session` channel; returns its local id.
    pub fn open_session(&mut self) -> SshResult<u32> {
        let id = self.next_id;
        self.next_id += 1;
        self.channels.insert(id, ChannelState { local_window: self.window, ..ChannelState::default() });
        let w = Writer::msg(msg::CHANNEL_OPEN).str("session").u32(id).u32(self.window).u32(self.max_packet).finish();
        self.t.send(&w)?;
        loop {
            match &self.chan(id)?.open {
                Some(Ok(())) => return Ok(id),
                Some(Err(e)) => {
                    let e = e.clone();
                    self.channels.remove(&id);
                    return Err(SshError::Channel(format!("cannot open session channel: {}", e)));
                }
                None => self.pump()?,
            }
        }
    }

    /// Sends a channel request; with `want_reply`, waits for the answer.
    pub fn channel_request(&mut self, id: u32, kind: &str, want_reply: bool, data: &[u8]) -> SshResult<bool> {
        let remote = self.chan(id)?.remote_id;
        let w = Writer::msg(msg::CHANNEL_REQUEST).u32(remote).str(kind).bool(want_reply).raw(data).finish();
        self.t.send(&w)?;
        if !want_reply {
            return Ok(true);
        }
        loop {
            let st = self.chan(id)?;
            if let Some(ok) = st.replies.pop_front() {
                return Ok(ok);
            }
            if st.closed {
                return Err(SshError::Channel(format!("channel closed while waiting for '{}' reply", kind)));
            }
            self.pump()?;
        }
    }

    fn require(&mut self, id: u32, kind: &str, data: &[u8]) -> SshResult<()> {
        if self.channel_request(id, kind, true, data)? {
            Ok(())
        } else {
            Err(SshError::Channel(format!("the server refused the '{}' request", kind)))
        }
    }

    /// Starts `command` on a new session channel.
    pub fn exec(&mut self, command: &str) -> SshResult<u32> {
        let id = self.open_session()?;
        self.require(id, "exec", &Writer::new().str(command).finish())?;
        Ok(id)
    }

    /// Starts a subsystem (e.g. `sftp`) on a new session channel.
    pub fn subsystem(&mut self, name: &str) -> SshResult<u32> {
        let id = self.open_session()?;
        self.require(id, "subsystem", &Writer::new().str(name).finish())?;
        Ok(id)
    }

    /// Requests a pseudo terminal on a channel.
    pub fn request_pty(&mut self, id: u32, term: &str, cols: u32, rows: u32) -> SshResult<()> {
        let d = Writer::new().str(term).u32(cols).u32(rows).u32(0).u32(0).string(&[0]).finish();
        self.require(id, "pty-req", &d)
    }

    /// Sets an environment variable (servers often refuse).
    pub fn set_env(&mut self, id: u32, name: &str, value: &str) -> SshResult<bool> {
        self.channel_request(id, "env", true, &Writer::new().str(name).str(value).finish())
    }

    /// Starts an interactive shell on a new session channel.
    pub fn shell(&mut self) -> SshResult<u32> {
        let id = self.open_session()?;
        self.require(id, "shell", &[])?;
        Ok(id)
    }

    /// Writes data to a channel, honouring the remote window and packet size.
    pub fn write(&mut self, id: u32, data: &[u8]) -> SshResult<()> {
        let mut off = 0;
        while off < data.len() {
            // drain what the peer sent so that it never blocks on a full socket
            while self.t.data_available() {
                self.pump()?;
            }
            let st = self.chan(id)?;
            if st.closed || st.eof_sent {
                return Err(SshError::Channel("channel is closed for writing".into()));
            }
            if st.remote_window == 0 {
                self.pump()?;
                continue;
            }
            let n = (data.len() - off).min(st.remote_window as usize).min(st.remote_max as usize).min(32768);
            st.remote_window -= n as u64;
            let w = Writer::msg(msg::CHANNEL_DATA).u32(st.remote_id).string(&data[off..off + n]).finish();
            self.t.send(&w)?;
            off += n;
        }
        Ok(())
    }

    /// Signals end of input on a channel.
    pub fn send_eof(&mut self, id: u32) -> SshResult<()> {
        let st = self.chan(id)?;
        if st.eof_sent || st.closed {
            return Ok(());
        }
        st.eof_sent = true;
        let w = Writer::msg(msg::CHANNEL_EOF).u32(st.remote_id).finish();
        self.t.send(&w)
    }

    /// Returns buffered stdout data, blocking until some arrives; empty at end of stream.
    pub fn read_some(&mut self, id: u32) -> SshResult<Vec<u8>> {
        loop {
            let st = self.chan(id)?;
            if !st.stdout.is_empty() {
                return Ok(st.stdout.drain(..).collect());
            }
            if st.eof || st.closed {
                return Ok(Vec::new());
            }
            self.pump()?;
        }
    }

    /// Takes the stderr data received so far.
    pub fn take_stderr(&mut self, id: u32) -> SshResult<Vec<u8>> {
        Ok(std::mem::take(&mut self.chan(id)?.stderr))
    }

    /// Waits until the channel is closed and returns everything not read yet.
    pub fn wait_close(&mut self, id: u32) -> SshResult<ExecResult> {
        while !self.chan(id)?.closed {
            self.pump()?;
        }
        let st = self.channels.remove(&id).expect("channel present");
        Ok(ExecResult { stdout: st.stdout.into_iter().collect(), stderr: st.stderr, exit_status: st.exit_status, exit_signal: st.exit_signal })
    }

    /// Closes a channel (sends CLOSE and waits for the peer's CLOSE).
    pub fn close_channel(&mut self, id: u32) -> SshResult<()> {
        let st = self.chan(id)?;
        if !st.close_sent {
            st.close_sent = true;
            let w = Writer::msg(msg::CHANNEL_CLOSE).u32(st.remote_id).finish();
            self.t.send(&w)?;
        }
        self.wait_close(id).map(|_| ())
    }

    /// Runs a command, feeding `stdin` (then EOF), and collects its output.
    pub fn execute(&mut self, command: &str, stdin: Option<&[u8]>) -> SshResult<ExecResult> {
        let id = self.exec(command)?;
        if let Some(input) = stdin {
            self.write(id, input)?;
        }
        self.send_eof(id)?;
        self.wait_close(id)
    }

    /// Sends a keep-alive global request and waits for any reply.
    pub fn keepalive(&mut self) -> SshResult<()> {
        self.t.send(&Writer::msg(msg::GLOBAL_REQUEST).str("keepalive@openssh.com").bool(true).finish())?;
        loop {
            let p = self.t.recv()?;
            if p[0] == msg::REQUEST_SUCCESS || p[0] == msg::REQUEST_FAILURE {
                return Ok(());
            }
            // dispatch anything else through the normal path
            self.t.push_front(p);
            self.pump()?;
        }
    }

    /// Whether the connection still works (sends a keep-alive).
    pub fn is_alive(&mut self) -> bool {
        self.keepalive().is_ok()
    }

    /// Sends `SSH_MSG_DISCONNECT` and closes the socket.
    pub fn disconnect(mut self) {
        self.t.disconnect(reason::BY_APPLICATION, "bye");
        let _ = self.t.stream().shutdown(std::net::Shutdown::Both);
    }
}
