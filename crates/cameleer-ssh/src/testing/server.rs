//! Embedded SSH-2 server for tests: Ed25519 host key generated at runtime (more
//! host keys can be added), `password`, `publickey` and `keyboard-interactive`
//! authentication, `exec`/`shell` running a tiny built-in command set (never
//! system commands) and an `sftp` subsystem serving a directory.

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::error::{SshError, SshResult};
use crate::keys::{SshPrivateKey, SshPublicKey};
use crate::known_hosts::KnownHosts;
use crate::sftp::server::SftpServer;
use crate::transport::{msg, reason, Transport, TransportConfig};
use crate::wire::{Reader, Writer};

/// Output of a command: (stdout, stderr, exit status).
pub type CommandOutput = (Vec<u8>, Vec<u8>, u32);

/// Custom command handler: (command line, stdin) -> output.
pub type CommandHandler = Arc<dyn Fn(&str, &[u8]) -> CommandOutput + Send + Sync>;

/// Configuration of the embedded server.
#[derive(Clone)]
pub struct SshServerConfig {
    /// Users and their passwords.
    pub users: HashMap<String, String>,
    /// Users and their authorized public keys.
    pub authorized_keys: HashMap<String, Vec<SshPublicKey>>,
    /// Host keys (an Ed25519 key is generated when empty).
    pub host_keys: Vec<SshPrivateKey>,
    /// Directory served by the `sftp` subsystem and seen by `ls`/`cat` (no SFTP when None).
    pub root: Option<PathBuf>,
    pub transport: TransportConfig,
    pub banner: Option<String>,
    /// Offer `keyboard-interactive` (one password prompt).
    pub keyboard_interactive: bool,
    /// Replaces the built-in commands.
    pub command_handler: Option<CommandHandler>,
    /// CA keys whose user certificates are accepted (`TrustedUserCAKeys`); the user name must
    /// be a principal of the certificate.
    pub trusted_user_ca_keys: Vec<SshPublicKey>,
    /// Accept `direct-tcpip` channels and `tcpip-forward` requests (default true, like sshd's
    /// `AllowTcpForwarding yes`).
    pub allow_tcp_forwarding: bool,
}

impl Default for SshServerConfig {
    fn default() -> Self {
        SshServerConfig {
            users: HashMap::new(),
            authorized_keys: HashMap::new(),
            host_keys: Vec::new(),
            root: None,
            transport: TransportConfig::default(),
            banner: None,
            keyboard_interactive: true,
            command_handler: None,
            trusted_user_ca_keys: Vec::new(),
            allow_tcp_forwarding: true,
        }
    }
}

impl SshServerConfig {
    pub fn user(mut self, name: &str, password: &str) -> Self {
        self.users.insert(name.to_string(), password.to_string());
        self
    }
    pub fn authorized_key(mut self, user: &str, key: SshPublicKey) -> Self {
        self.authorized_keys.entry(user.to_string()).or_default().push(key);
        self
    }
    pub fn host_key(mut self, key: SshPrivateKey) -> Self {
        self.host_keys.push(key);
        self
    }
    pub fn root(mut self, dir: &Path) -> Self {
        self.root = Some(dir.to_path_buf());
        self
    }
}

struct Shared {
    cfg: SshServerConfig,
    commands: Mutex<Vec<String>>,
    auth_log: Mutex<Vec<String>>,
}

/// A running embedded server.
pub struct SshServer {
    local: SocketAddr,
    shared: Arc<Shared>,
    running: Arc<AtomicBool>,
    accept: Mutex<Option<JoinHandle<()>>>,
    conns: Arc<Mutex<Vec<TcpStream>>>,
}

impl SshServer {
    /// Starts a server on an ephemeral port of 127.0.0.1.
    pub fn start(cfg: SshServerConfig) -> std::io::Result<SshServer> {
        SshServer::bind("127.0.0.1:0", cfg)
    }

    /// Starts a server on `addr`.
    pub fn bind(addr: &str, mut cfg: SshServerConfig) -> std::io::Result<SshServer> {
        if cfg.host_keys.is_empty() {
            cfg.host_keys.push(SshPrivateKey::generate_ed25519());
        }
        let listener = TcpListener::bind(addr)?;
        let local = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared { cfg, commands: Mutex::new(Vec::new()), auth_log: Mutex::new(Vec::new()) });
        let running = Arc::new(AtomicBool::new(true));
        let conns: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let (sh, run, cs) = (shared.clone(), running.clone(), conns.clone());
        let accept = std::thread::Builder::new().name(format!("ssh-test-server-{}", local.port())).spawn(move || {
            while run.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((s, _)) => {
                        let _ = s.set_nonblocking(false);
                        if let Ok(c) = s.try_clone() {
                            cs.lock().unwrap().push(c);
                        }
                        let sh = sh.clone();
                        let _ = std::thread::Builder::new().name("ssh-test-conn".into()).spawn(move || {
                            let _ = serve(s, &sh);
                        });
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        })?;
        Ok(SshServer { local, shared, running, accept: Mutex::new(Some(accept)), conns })
    }

    /// The listening port.
    pub fn port(&self) -> u16 {
        self.local.port()
    }

    /// The public host keys.
    pub fn host_keys(&self) -> Vec<SshPublicKey> {
        self.shared.cfg.host_keys.iter().map(|k| k.public_key()).collect()
    }

    /// The first public host key.
    pub fn host_key(&self) -> SshPublicKey {
        self.shared.cfg.host_keys[0].public_key()
    }

    /// `known_hosts` text trusting this server's host keys for `host`.
    pub fn known_hosts_text(&self, host: &str) -> String {
        self.host_keys().iter().map(|k| KnownHosts::entry_line(host, self.port(), k, false) + "\n").collect()
    }

    /// Commands executed so far (exec requests and shell lines).
    pub fn commands(&self) -> Vec<String> {
        self.shared.commands.lock().unwrap().clone()
    }

    /// Authentication attempts as `user:method:ok|fail`.
    pub fn auth_log(&self) -> Vec<String> {
        self.shared.auth_log.lock().unwrap().clone()
    }

    /// Stops accepting, closes all connections and joins the accept thread.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        for c in self.conns.lock().unwrap().drain(..) {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
        if let Some(j) = self.accept.lock().unwrap().take() {
            let _ = j.join();
        }
    }
}

impl Drop for SshServer {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------- built-in commands

fn split_args(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'') | (None, '"') => {
                quote = Some(c);
                has = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    has = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Runs one built-in command: `echo [-n]`, `cat [files]`, `ls [dir]`, `pwd`,
/// `exit N`, `true`, `false`. Anything else exits with 127.
pub fn run_builtin(line: &str, stdin: &[u8], root: Option<&Path>) -> CommandOutput {
    let args = split_args(line);
    let local = |p: &str| root.map(|r| r.join(SftpServer::virtual_path(p).trim_start_matches('/')));
    match args.first().map(|s| s.as_str()) {
        None => (Vec::new(), Vec::new(), 0),
        Some("echo") => {
            let (nl, rest) = if args.get(1).map(|s| s.as_str()) == Some("-n") { (false, &args[2..]) } else { (true, &args[1..]) };
            let mut o = rest.join(" ").into_bytes();
            if nl {
                o.push(b'\n');
            }
            (o, Vec::new(), 0)
        }
        Some("cat") if args.len() == 1 => (stdin.to_vec(), Vec::new(), 0),
        Some("cat") => {
            let (mut out, mut err, mut code) = (Vec::new(), Vec::new(), 0);
            for f in &args[1..] {
                match local(f).map(std::fs::read) {
                    Some(Ok(d)) => out.extend(d),
                    _ => {
                        err.extend(format!("cat: {}: No such file or directory\n", f).into_bytes());
                        code = 1;
                    }
                }
            }
            (out, err, code)
        }
        Some("ls") => {
            let dir = args.get(1).map(|s| s.as_str()).unwrap_or(".");
            match local(dir).map(std::fs::read_dir) {
                Some(Ok(rd)) => {
                    let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
                    names.sort();
                    (names.iter().map(|n| format!("{}\n", n)).collect::<String>().into_bytes(), Vec::new(), 0)
                }
                _ => (Vec::new(), format!("ls: cannot access '{}': No such file or directory\n", dir).into_bytes(), 2),
            }
        }
        Some("pwd") => (b"/\n".to_vec(), Vec::new(), 0),
        Some("exit") => (Vec::new(), Vec::new(), args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0)),
        Some("true") => (Vec::new(), Vec::new(), 0),
        Some("false") => (Vec::new(), Vec::new(), 1),
        Some(other) => (Vec::new(), format!("sh: {}: command not found\n", other).into_bytes(), 127),
    }
}

// ---------------------------------------------------------------- connection handling

enum Kind {
    Session,
    /// Command waiting for stdin EOF (only `cat` without arguments and custom handlers).
    Exec(String),
    Shell(Vec<u8>),
    Sftp(SftpServer),
    /// `direct-tcpip` / `forwarded-tcpip`: a TCP connection (non-blocking) piped to the
    /// channel; `open` is false until the client confirms a `forwarded-tcpip` channel.
    Forward { tcp: TcpStream, open: bool, eof_in: bool, eof_out: bool, eof_sent: bool },
    Done,
}

/// Writes all bytes to a non-blocking socket.
fn write_all_nb(s: &mut TcpStream, mut data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    while !data.is_empty() {
        match s.write(data) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(1)),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Polls forwarding sockets and `tcpip-forward` listeners until SSH data arrives.
fn pump_forwards(t: &mut Transport<TcpStream>, chans: &mut HashMap<u32, Chan>, listeners: &mut [(String, u16, std::net::TcpListener)], next: &mut u32) -> SshResult<()> {
    use std::io::Read;
    loop {
        let mut busy = false;
        for (addr, port, l) in listeners.iter() {
            while let Ok((tcp, peer)) = l.accept() {
                busy = true;
                let _ = tcp.set_nonblocking(true);
                let id = *next;
                *next += 1;
                let w = Writer::msg(msg::CHANNEL_OPEN)
                    .str("forwarded-tcpip")
                    .u32(id)
                    .u32(WINDOW)
                    .u32(32768)
                    .str(addr)
                    .u32(*port as u32)
                    .str(&peer.ip().to_string())
                    .u32(peer.port() as u32)
                    .finish();
                t.send(&w)?;
                chans.insert(id, Chan { remote_id: 0, remote_window: 0, remote_max: 1, consumed: 0, kind: Kind::Forward { tcp, open: false, eof_in: false, eof_out: false, eof_sent: false }, stdin: Vec::new(), out: VecDeque::new(), finish: None, close_sent: false });
            }
        }
        for c in chans.values_mut() {
            if let Kind::Forward { tcp, open: true, eof_in, eof_out, eof_sent } = &mut c.kind {
                if !*eof_in && c.out.iter().map(|(_, d)| d.len()).sum::<usize>() < 256 * 1024 {
                    let mut buf = [0u8; 32768];
                    match tcp.read(&mut buf) {
                        Ok(0) => *eof_in = true,
                        Ok(n) => {
                            busy = true;
                            c.out.push_back((false, buf[..n].to_vec()));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => *eof_in = true,
                    }
                }
                if *eof_in && c.out.is_empty() && !c.close_sent {
                    if !*eof_sent {
                        *eof_sent = true;
                        t.send(&Writer::msg(msg::CHANNEL_EOF).u32(c.remote_id).finish())?;
                    }
                    if *eof_out {
                        t.send(&Writer::msg(msg::CHANNEL_CLOSE).u32(c.remote_id).finish())?;
                        c.close_sent = true;
                    }
                }
            }
        }
        flush(t, chans)?;
        if t.data_available() {
            return Ok(());
        }
        if !busy {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

struct Chan {
    remote_id: u32,
    remote_window: u64,
    remote_max: u32,
    consumed: u32,
    kind: Kind,
    stdin: Vec<u8>,
    out: VecDeque<(bool, Vec<u8>)>,
    finish: Option<u32>,
    close_sent: bool,
}

const WINDOW: u32 = 2 * 1024 * 1024;

fn serve(stream: TcpStream, sh: &Shared) -> SshResult<()> {
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let _ = stream.set_nodelay(true);
    let mut t = Transport::server(stream, sh.cfg.transport.clone(), sh.cfg.host_keys.clone())?;
    let p = t.recv()?;
    let mut r = Reader::new(&p[1..]);
    if p[0] != msg::SERVICE_REQUEST || r.str()? != "ssh-userauth" {
        t.disconnect(reason::SERVICE_NOT_AVAILABLE, "expected ssh-userauth");
        return Err(SshError::protocol("expected ssh-userauth service request"));
    }
    t.send(&Writer::msg(msg::SERVICE_ACCEPT).str("ssh-userauth").finish())?;
    if let Some(b) = &sh.cfg.banner {
        t.send(&Writer::msg(msg::USERAUTH_BANNER).str(b).str("").finish())?;
    }
    authenticate(&mut t, sh)?;
    connection(&mut t, sh)
}

fn methods(sh: &Shared) -> Vec<&'static str> {
    let mut m = vec!["publickey", "password"];
    if sh.cfg.keyboard_interactive {
        m.insert(1, "keyboard-interactive");
    }
    m
}

fn password_ok(sh: &Shared, user: &str, pw: &str) -> bool {
    match sh.cfg.users.get(user) {
        Some(p) => cameleer_tls::util::ct_eq(p.as_bytes(), pw.as_bytes()),
        None => false,
    }
}

fn authenticate(t: &mut Transport<TcpStream>, sh: &Shared) -> SshResult<()> {
    let failure = |t: &mut Transport<TcpStream>| t.send(&Writer::msg(msg::USERAUTH_FAILURE).name_list(&methods(sh)).bool(false).finish());
    for _ in 0..20 {
        let p = t.recv()?;
        if p[0] != msg::USERAUTH_REQUEST {
            return Err(SshError::protocol("expected USERAUTH_REQUEST"));
        }
        let mut r = Reader::new(&p[1..]);
        let user = r.str()?;
        let service = r.str()?;
        let method = r.str()?;
        if service != "ssh-connection" {
            t.disconnect(reason::SERVICE_NOT_AVAILABLE, "unknown service");
            return Err(SshError::protocol("unknown service"));
        }
        let ok = match method.as_str() {
            "password" => {
                let _change = r.bool()?;
                let pw = r.str()?;
                password_ok(sh, &user, &pw)
            }
            "publickey" => {
                let has_sig = r.bool()?;
                let alg = r.str()?;
                let blob = r.string()?.to_vec();
                // user certificate: trusted CA, user principal, validity; signed by the key
                let (key, sig_alg, allowed) = match crate::cert::plain_algorithm(&alg) {
                    Some(plain) => match crate::cert::SshCertificate::from_blob(&blob) {
                        Ok(c) => {
                            let ok = c.key.supports_algorithm(plain)
                                && sh.cfg.trusted_user_ca_keys.contains(&c.signature_key)
                                && c.validate(crate::cert::USER_CERT, &user, crate::cert::now_secs()).is_ok();
                            (Some(c.key.clone()), plain.to_string(), ok)
                        }
                        Err(_) => (None, plain.to_string(), false),
                    },
                    None => {
                        let key = SshPublicKey::from_blob(&blob).ok();
                        let allowed = key
                            .as_ref()
                            .map(|k| k.supports_algorithm(&alg) && sh.cfg.authorized_keys.get(&user).map(|l| l.contains(k)).unwrap_or(false))
                            .unwrap_or(false);
                        (key, alg.clone(), allowed)
                    }
                };
                if !has_sig {
                    if allowed {
                        t.send(&Writer::msg(msg::USERAUTH_60).str(&alg).string(&blob).finish())?;
                        continue;
                    }
                    false
                } else {
                    let sig = r.string()?.to_vec();
                    let mut data = Writer::new();
                    data.string(t.session_id());
                    data.u8(msg::USERAUTH_REQUEST).str(&user).str("ssh-connection").str("publickey").bool(true).str(&alg).string(&blob);
                    allowed && key.map(|k| k.verify(&sig_alg, &data.buf, &sig)).unwrap_or(false)
                }
            }
            "keyboard-interactive" if sh.cfg.keyboard_interactive => {
                let mut w = Writer::msg(msg::USERAUTH_60);
                w.str("").str("").str("").u32(1).str("Password: ").bool(false);
                t.send(&w.finish())?;
                let p = t.recv()?;
                if p[0] != msg::USERAUTH_INFO_RESPONSE {
                    return Err(SshError::protocol("expected USERAUTH_INFO_RESPONSE"));
                }
                let mut r = Reader::new(&p[1..]);
                let n = r.u32()?;
                n == 1 && password_ok(sh, &user, &r.str()?)
            }
            _ => false,
        };
        if method != "none" {
            sh.auth_log.lock().unwrap().push(format!("{}:{}:{}", user, method, if ok { "ok" } else { "fail" }));
        }
        if ok {
            t.send(&[msg::USERAUTH_SUCCESS])?;
            return Ok(());
        }
        failure(t)?;
    }
    t.disconnect(reason::NO_MORE_AUTH_METHODS, "too many authentication failures");
    Err(SshError::Auth("too many failures".into()))
}

fn run(sh: &Shared, line: &str, stdin: &[u8]) -> CommandOutput {
    sh.commands.lock().unwrap().push(line.to_string());
    match &sh.cfg.command_handler {
        Some(h) => h(line, stdin),
        None => run_builtin(line, stdin, sh.cfg.root.as_deref()),
    }
}

fn queue_output(c: &mut Chan, out: CommandOutput) {
    if !out.0.is_empty() {
        c.out.push_back((false, out.0));
    }
    if !out.1.is_empty() {
        c.out.push_back((true, out.1));
    }
    c.finish = Some(out.2);
    c.kind = Kind::Done;
}

fn connection(t: &mut Transport<TcpStream>, sh: &Shared) -> SshResult<()> {
    let mut chans: HashMap<u32, Chan> = HashMap::new();
    let mut next = 0u32;
    let mut listeners: Vec<(String, u16, std::net::TcpListener)> = Vec::new();
    loop {
        if !listeners.is_empty() || chans.values().any(|c| matches!(c.kind, Kind::Forward { .. })) {
            pump_forwards(t, &mut chans, &mut listeners, &mut next)?;
        }
        let p = t.recv()?;
        let mut r = Reader::new(&p[1..]);
        match p[0] {
            msg::GLOBAL_REQUEST => {
                let name = r.str()?;
                let want = r.bool()?;
                match name.as_str() {
                    "tcpip-forward" if sh.cfg.allow_tcp_forwarding => {
                        let addr = r.str()?;
                        let port = r.u32()? as u16;
                        let bind = if addr.is_empty() || addr == "localhost" { "127.0.0.1".to_string() } else { addr.clone() };
                        match std::net::TcpListener::bind((bind.as_str(), port)) {
                            Ok(l) => {
                                let bound = l.local_addr().map(|a| a.port()).unwrap_or(port);
                                let _ = l.set_nonblocking(true);
                                listeners.push((addr, bound, l));
                                if want {
                                    let mut w = Writer::msg(msg::REQUEST_SUCCESS);
                                    if port == 0 {
                                        w.u32(bound as u32);
                                    }
                                    t.send(&w.finish())?;
                                }
                            }
                            Err(_) if want => t.send(&[msg::REQUEST_FAILURE])?,
                            Err(_) => {}
                        }
                    }
                    "cancel-tcpip-forward" => {
                        let addr = r.str()?;
                        let port = r.u32()? as u16;
                        let before = listeners.len();
                        listeners.retain(|(a, p, _)| !(a == &addr && *p == port));
                        if want {
                            t.send(&[if listeners.len() < before { msg::REQUEST_SUCCESS } else { msg::REQUEST_FAILURE }])?;
                        }
                    }
                    _ if want => t.send(&[msg::REQUEST_FAILURE])?,
                    _ => {}
                }
            }
            msg::CHANNEL_OPEN_CONFIRMATION | msg::CHANNEL_OPEN_FAILURE => {
                let id = r.u32()?;
                if p[0] == msg::CHANNEL_OPEN_FAILURE {
                    chans.remove(&id);
                } else if let Some(c) = chans.get_mut(&id) {
                    c.remote_id = r.u32()?;
                    c.remote_window = r.u32()? as u64;
                    c.remote_max = r.u32()?.max(1);
                    if let Kind::Forward { open, .. } = &mut c.kind {
                        *open = true;
                    }
                }
            }
            msg::CHANNEL_OPEN => {
                let kind = r.str()?;
                let sender = r.u32()?;
                let window = r.u32()?;
                let max = r.u32()?;
                if kind == "direct-tcpip" && sh.cfg.allow_tcp_forwarding {
                    let host = r.str()?;
                    let port = r.u32()? as u16;
                    match TcpStream::connect((host.as_str(), port)) {
                        Ok(tcp) => {
                            let _ = tcp.set_nonblocking(true);
                            let id = next;
                            next += 1;
                            chans.insert(id, Chan { remote_id: sender, remote_window: window as u64, remote_max: max.max(1), consumed: 0, kind: Kind::Forward { tcp, open: true, eof_in: false, eof_out: false, eof_sent: false }, stdin: Vec::new(), out: VecDeque::new(), finish: None, close_sent: false });
                            t.send(&Writer::msg(msg::CHANNEL_OPEN_CONFIRMATION).u32(sender).u32(id).u32(WINDOW).u32(32768).finish())?;
                        }
                        Err(e) => {
                            t.send(&Writer::msg(msg::CHANNEL_OPEN_FAILURE).u32(sender).u32(2).str(&format!("connect failed: {}", e)).str("").finish())?;
                        }
                    }
                    continue;
                }
                if kind != "session" {
                    t.send(&Writer::msg(msg::CHANNEL_OPEN_FAILURE).u32(sender).u32(3).str("unknown channel type").str("").finish())?;
                    continue;
                }
                let id = next;
                next += 1;
                chans.insert(
                    id,
                    Chan {
                        remote_id: sender,
                        remote_window: window as u64,
                        remote_max: max.max(1),
                        consumed: 0,
                        kind: Kind::Session,
                        stdin: Vec::new(),
                        out: VecDeque::new(),
                        finish: None,
                        close_sent: false,
                    },
                );
                t.send(&Writer::msg(msg::CHANNEL_OPEN_CONFIRMATION).u32(sender).u32(id).u32(WINDOW).u32(32768).finish())?;
            }
            msg::CHANNEL_REQUEST => {
                let id = r.u32()?;
                let req = r.str()?;
                let want = r.bool()?;
                let c = match chans.get_mut(&id) {
                    Some(c) => c,
                    None => continue,
                };
                let ok = match (req.as_str(), &c.kind) {
                    ("pty-req", _) | ("env", _) | ("window-change", _) => true,
                    ("exec", Kind::Session) => {
                        let cmd = r.str()?;
                        let needs_stdin = sh.cfg.command_handler.is_some() || split_args(&cmd) == ["cat"];
                        if needs_stdin {
                            c.kind = Kind::Exec(cmd);
                        } else {
                            let out = run(sh, &cmd, &[]);
                            if want {
                                t.send(&Writer::msg(msg::CHANNEL_SUCCESS).u32(c.remote_id).finish())?;
                            }
                            queue_output(c, out);
                            flush(t, &mut chans)?;
                            continue;
                        }
                        true
                    }
                    ("shell", Kind::Session) => {
                        c.kind = Kind::Shell(Vec::new());
                        true
                    }
                    ("subsystem", Kind::Session) => {
                        let name = r.str()?;
                        match (&sh.cfg.root, name.as_str()) {
                            (Some(root), "sftp") => {
                                c.kind = Kind::Sftp(SftpServer::new(root));
                                true
                            }
                            _ => false,
                        }
                    }
                    _ => false,
                };
                if want {
                    let m = if ok { msg::CHANNEL_SUCCESS } else { msg::CHANNEL_FAILURE };
                    t.send(&Writer::msg(m).u32(c.remote_id).finish())?;
                }
            }
            msg::CHANNEL_DATA | msg::CHANNEL_EXTENDED_DATA => {
                let id = r.u32()?;
                if p[0] == msg::CHANNEL_EXTENDED_DATA {
                    r.u32()?;
                }
                let data = r.string()?;
                let c = match chans.get_mut(&id) {
                    Some(c) => c,
                    None => continue,
                };
                c.consumed += data.len() as u32;
                if c.consumed >= WINDOW / 2 {
                    t.send(&Writer::msg(msg::CHANNEL_WINDOW_ADJUST).u32(c.remote_id).u32(c.consumed).finish())?;
                    c.consumed = 0;
                }
                if p[0] == msg::CHANNEL_EXTENDED_DATA {
                    continue;
                }
                match &mut c.kind {
                    Kind::Forward { tcp, .. } => {
                        if write_all_nb(tcp, data).is_err() {
                            c.finish = None;
                        }
                    }
                    Kind::Exec(_) => c.stdin.extend_from_slice(data),
                    Kind::Sftp(s) => {
                        let reply = s.feed(data);
                        if !reply.is_empty() {
                            c.out.push_back((false, reply));
                        }
                    }
                    Kind::Shell(buf) => {
                        buf.extend_from_slice(data);
                        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                            let line: Vec<u8> = buf.drain(..=pos).collect();
                            let line = String::from_utf8_lossy(&line).trim().to_string();
                            let args = split_args(&line);
                            let out = run(sh, &line, &[]);
                            if args.first().map(|s| s.as_str()) == Some("exit") {
                                queue_output(c, out);
                                break;
                            }
                            c.out.push_back((false, out.0));
                            if !out.1.is_empty() {
                                c.out.push_back((true, out.1));
                            }
                        }
                    }
                    _ => {}
                }
            }
            msg::CHANNEL_EOF => {
                let id = r.u32()?;
                if let Some(c) = chans.get_mut(&id) {
                    if let Kind::Forward { tcp, eof_out, eof_sent, .. } = &mut c.kind {
                        let _ = tcp.shutdown(std::net::Shutdown::Write);
                        *eof_out = true;
                        if *eof_sent && c.out.is_empty() && !c.close_sent {
                            t.send(&Writer::msg(msg::CHANNEL_CLOSE).u32(c.remote_id).finish())?;
                            c.close_sent = true;
                        }
                        continue;
                    }
                    match std::mem::replace(&mut c.kind, Kind::Done) {
                        Kind::Exec(cmd) => {
                            let stdin = std::mem::take(&mut c.stdin);
                            let out = run(sh, &cmd, &stdin);
                            queue_output(c, out);
                        }
                        Kind::Shell(_) => c.finish = Some(0),
                        Kind::Sftp(_) | Kind::Session => c.finish = Some(0),
                        Kind::Forward { .. } | Kind::Done => {}
                    }
                }
            }
            msg::CHANNEL_CLOSE => {
                let id = r.u32()?;
                if let Some(c) = chans.remove(&id) {
                    if !c.close_sent {
                        t.send(&Writer::msg(msg::CHANNEL_CLOSE).u32(c.remote_id).finish())?;
                    }
                }
            }
            msg::CHANNEL_WINDOW_ADJUST => {
                let id = r.u32()?;
                let n = r.u32()?;
                if let Some(c) = chans.get_mut(&id) {
                    c.remote_window += n as u64;
                }
            }
            msg::DISCONNECT => return Ok(()),
            msg::CHANNEL_SUCCESS | msg::CHANNEL_FAILURE | msg::REQUEST_SUCCESS | msg::REQUEST_FAILURE => {}
            _ => t.send_unimplemented()?,
        }
        flush(t, &mut chans)?;
    }
}

fn flush(t: &mut Transport<TcpStream>, chans: &mut HashMap<u32, Chan>) -> SshResult<()> {
    for c in chans.values_mut() {
        while let Some((is_err, data)) = c.out.front_mut() {
            if data.is_empty() {
                c.out.pop_front();
                continue;
            }
            if c.remote_window == 0 {
                break;
            }
            let n = data.len().min(c.remote_window as usize).min(c.remote_max as usize).min(32768);
            let chunk: Vec<u8> = data.drain(..n).collect();
            let w = if *is_err {
                Writer::msg(msg::CHANNEL_EXTENDED_DATA).u32(c.remote_id).u32(1).string(&chunk).finish()
            } else {
                Writer::msg(msg::CHANNEL_DATA).u32(c.remote_id).string(&chunk).finish()
            };
            c.remote_window -= n as u64;
            t.send(&w)?;
        }
        if c.out.is_empty() && !c.close_sent {
            if let Some(code) = c.finish {
                t.send(&Writer::msg(msg::CHANNEL_REQUEST).u32(c.remote_id).str("exit-status").bool(false).u32(code).finish())?;
                t.send(&Writer::msg(msg::CHANNEL_EOF).u32(c.remote_id).finish())?;
                t.send(&Writer::msg(msg::CHANNEL_CLOSE).u32(c.remote_id).finish())?;
                c.close_sent = true;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins() {
        assert_eq!(split_args(r#"echo "a b" 'c' d\ e"#), vec!["echo", "a b", "c", "d e"]);
        assert_eq!(run_builtin("echo hello world", b"", None).0, b"hello world\n");
        assert_eq!(run_builtin("echo -n x", b"", None).0, b"x");
        assert_eq!(run_builtin("cat", b"in", None).0, b"in");
        assert_eq!(run_builtin("exit 3", b"", None).2, 3);
        let r = run_builtin("rm -rf /", b"", None);
        assert_eq!(r.2, 127);
        assert!(String::from_utf8_lossy(&r.1).contains("command not found"));
        assert_eq!(run_builtin("cat /nope", b"", None).2, 1);
    }
}
