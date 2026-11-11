//! A minimal embedded FTP server serving a local directory, for tests.
//!
//! Supports `USER`/`PASS`, `SYST`, `FEAT`, `OPTS`, `PWD`, `CWD`, `CDUP`, `TYPE` (A/I),
//! `MODE`, `STRU`, `PASV`, `EPSV`, `PORT`, `EPRT`, `LIST` (Unix format), `NLST`, `MLSD`,
//! `RETR` (with `REST` restart offsets), `STOR`, `APPE`, `DELE`, `MKD`, `RMD`, `RNFR`/`RNTO`, `SIZE`, `MDTM`, `SITE`,
//! `NOOP` and `QUIT`. Paths cannot escape the served root directory.
//!
//! FTPS: [`FtpServer::start_tls`] serves explicit (`AUTH TLS`, `PBSZ`, `PROT`) or implicit
//! TLS; with `PROT P` data connections are TLS-protected (handshake after the `150` reply).
//! [`FtpServer::set_require_ssl_reuse`] mimics vsftpd's `require_ssl_reuse=YES`: a data
//! connection must resume the TLS session of its control connection, otherwise the
//! transfer fails with `522 SSL connection failed; session reuse required`.

use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use cameleer_core::error::Result;
use cameleer_core::util_time::{civil_from_epoch, now_millis};

use cameleer_tls::net::NetStream;
use cameleer_tls::TlsServerConfig;

use cameleer_tls::net::{read_line, TcpServer};
use crate::ftp::client::{from_net_ascii, to_net_ascii};
use crate::ftp::join_path;
use crate::ftp::listing::{format_mdtm, month_name};

/// FTPS mode of the embedded server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FtpsMode {
    /// `AUTH TLS` upgrades the control connection; login requires TLS.
    Explicit,
    /// TLS from the first byte.
    Implicit,
}

struct State {
    tls: Option<(Arc<TlsServerConfig>, FtpsMode)>,
    root: PathBuf,
    users: HashMap<String, String>,
    commands: Mutex<Vec<String>>,
    require_ssl_reuse: AtomicBool,
}

/// An embedded FTP server bound to `127.0.0.1` on an ephemeral port.
pub struct FtpServer {
    server: TcpServer,
    state: Arc<State>,
}

impl FtpServer {
    /// Serve `root`, accepting any user name and password.
    pub fn start(root: impl Into<PathBuf>) -> Result<FtpServer> {
        Self::start_with_users(root, &[])
    }

    /// Serve `root`, accepting only the given `(user, password)` pairs (empty = any).
    pub fn start_with_users(root: impl Into<PathBuf>, users: &[(&str, &str)]) -> Result<FtpServer> {
        Self::start_inner(root.into(), users, None)
    }

    /// Serve `root` over FTPS (explicit or implicit) with the given server certificate.
    pub fn start_tls(root: impl Into<PathBuf>, users: &[(&str, &str)], tls: TlsServerConfig, mode: FtpsMode) -> Result<FtpServer> {
        Self::start_inner(root.into(), users, Some((Arc::new(tls), mode)))
    }

    fn start_inner(root: PathBuf, users: &[(&str, &str)], tls: Option<(Arc<TlsServerConfig>, FtpsMode)>) -> Result<FtpServer> {
        fs::create_dir_all(&root).map_err(|e| cameleer_core::error::CamelError::io(e.to_string()))?;
        let state = Arc::new(State {
            tls,
            root: fs::canonicalize(&root).unwrap_or(root),
            users: users.iter().map(|(u, p)| (u.to_string(), p.to_string())).collect(),
            commands: Mutex::new(Vec::new()),
            require_ssl_reuse: AtomicBool::new(false),
        });
        let st = state.clone();
        let server = TcpServer::bind("127.0.0.1", 0, "ftp-server", move |s| {
            if let Some(session) = Session::new(st.clone(), s) {
                let _ = session.run();
            }
        })?;
        Ok(FtpServer { server, state })
    }

    /// The port the server listens on.
    pub fn port(&self) -> u16 {
        self.server.port()
    }

    /// The served root directory.
    pub fn root(&self) -> &Path {
        &self.state.root
    }

    /// All commands received so far (passwords masked).
    pub fn commands(&self) -> Vec<String> {
        self.state.commands.lock().unwrap().clone()
    }

    /// Require protected data connections to resume the control connection's TLS
    /// session (vsftpd `require_ssl_reuse`).
    pub fn set_require_ssl_reuse(&self, yes: bool) {
        self.state.require_ssl_reuse.store(yes, Ordering::SeqCst);
    }

    /// Stop the server and close all connections.
    pub fn stop(&self) {
        self.server.stop();
    }
}

struct Session {
    st: Arc<State>,
    reader: BufReader<NetStream>,
    out: NetStream,
    prot_private: bool,
    user: Option<String>,
    logged_in: bool,
    cwd: String,
    binary: bool,
    rest: u64,
    pasv: Option<TcpListener>,
    port: Option<SocketAddr>,
    rnfr: Option<PathBuf>,
}

impl Session {
    fn new(st: Arc<State>, s: TcpStream) -> Option<Session> {
        let _ = s.set_read_timeout(Some(Duration::from_secs(120)));
        let s = match &st.tls {
            Some((cfg, FtpsMode::Implicit)) => NetStream::server(s, cfg).ok()?,
            _ => NetStream::plain(s),
        };
        let out = s.try_clone().ok()?;
        Some(Session { st, reader: BufReader::new(s), out, prot_private: false, user: None, logged_in: false, cwd: "/".into(), binary: false, rest: 0, pasv: None, port: None, rnfr: None })
    }

    /// Wraps an accepted data connection in TLS after `PROT P` (call after the 150 reply).
    fn secure_data(&self, s: TcpStream) -> std::io::Result<NetStream> {
        match (&self.st.tls, self.prot_private) {
            (Some((cfg, _)), true) => {
                let d = NetStream::server(s, cfg)?;
                if self.st.require_ssl_reuse.load(Ordering::SeqCst)
                    && !(d.session_resumed() && !d.session_origin_id().is_empty() && d.session_origin_id() == self.out.session_origin_id())
                {
                    let _ = d.shutdown(std::net::Shutdown::Both);
                    return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "session reuse required"));
                }
                Ok(d)
            }
            _ => Ok(NetStream::plain(s)),
        }
    }

    fn data_tls_failed(&mut self, e: &std::io::Error) -> std::io::Result<()> {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            self.reply("522 SSL connection failed; session reuse required")
        } else {
            self.reply("425 TLS negotiation on the data connection failed")
        }
    }

    fn reply(&mut self, text: &str) -> std::io::Result<()> {
        self.out.write_all(format!("{}\r\n", text).as_bytes())?;
        self.out.flush()
    }

    /// Resolve a client path to (virtual absolute path, real path).
    fn resolve(&self, arg: &str) -> (String, PathBuf) {
        let v = join_path(&self.cwd, arg);
        let v = if v.starts_with('/') { v } else { format!("/{}", v) };
        let real = self.st.root.join(v.trim_start_matches('/'));
        (v, real)
    }

    fn open_data(&mut self) -> std::io::Result<TcpStream> {
        if let Some(l) = self.pasv.take() {
            l.set_nonblocking(true)?;
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match l.accept() {
                    Ok((s, _)) => {
                        s.set_nonblocking(false)?;
                        s.set_read_timeout(Some(Duration::from_secs(30)))?;
                        return Ok(s);
                    }
                    Err(e) if Instant::now() >= deadline => return Err(e),
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        }
        if let Some(a) = self.port.take() {
            let s = TcpStream::connect_timeout(&a, Duration::from_secs(10))?;
            s.set_read_timeout(Some(Duration::from_secs(30)))?;
            return Ok(s);
        }
        Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "no data connection (use PASV or PORT)"))
    }

    fn send_data(&mut self, data: &[u8]) -> std::io::Result<()> {
        let s = match self.open_data() {
            Ok(s) => s,
            Err(_) => return self.reply("425 Can't open data connection"),
        };
        self.reply("150 Opening data connection")?;
        let mut s = match self.secure_data(s) {
            Ok(s) => s,
            Err(e) => return self.data_tls_failed(&e),
        };
        let r = s.write_all(data).and_then(|_| s.flush());
        let _ = s.shutdown(std::net::Shutdown::Both);
        match r {
            Ok(_) => self.reply("226 Transfer complete"),
            Err(_) => self.reply("426 Connection closed; transfer aborted"),
        }
    }

    fn list_line(name: &str, meta: &fs::Metadata) -> String {
        let modified = meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
        let (y, mo, d, h, mi, _) = civil_from_epoch(modified);
        let (cy, ..) = civil_from_epoch(now_millis() / 1000);
        let when = if y == cy { format!("{:02}:{:02}", h, mi) } else { format!(" {}", y) };
        format!(
            "{} 1 ftp ftp {:>12} {} {:>2} {} {}",
            if meta.is_dir() { "drwxr-xr-x" } else { "-rw-r--r--" },
            if meta.is_dir() { 4096 } else { meta.len() },
            month_name(mo),
            d,
            when,
            name
        )
    }

    fn mlsd_line(name: &str, meta: &fs::Metadata) -> String {
        let modified = meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
        if meta.is_dir() {
            format!("type=dir;modify={}; {}", format_mdtm(modified), name)
        } else {
            format!("type=file;size={};modify={}; {}", meta.len(), format_mdtm(modified), name)
        }
    }

    fn listing(&mut self, arg: &str, kind: &str) -> std::io::Result<()> {
        // ignore ls style options such as "-la"
        let arg = arg.split_whitespace().filter(|a| !a.starts_with('-')).collect::<Vec<_>>().join(" ");
        let (_, real) = self.resolve(&arg);
        let mut entries: Vec<(String, fs::Metadata)> = Vec::new();
        match fs::metadata(&real) {
            Ok(m) if m.is_dir() => {
                if let Ok(rd) = fs::read_dir(&real) {
                    for e in rd.flatten() {
                        if let Ok(m) = e.metadata() {
                            entries.push((e.file_name().to_string_lossy().into_owned(), m));
                        }
                    }
                }
            }
            Ok(m) => entries.push((real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), m)),
            Err(_) => return self.reply("550 No such file or directory"),
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut text = String::new();
        for (name, m) in &entries {
            let line = match kind {
                "MLSD" => Self::mlsd_line(name, m),
                "NLST" => name.clone(),
                _ => Self::list_line(name, m),
            };
            text.push_str(&line);
            text.push_str("\r\n");
        }
        self.send_data(text.as_bytes())
    }

    fn run(mut self) -> std::io::Result<()> {
        self.reply("220 Cameleer FTP server ready")?;
        loop {
            let Some(line) = read_line(&mut self.reader, 8192)? else { return Ok(()) };
            let (cmd, arg) = match line.split_once(' ') {
                Some((c, a)) => (c.to_ascii_uppercase(), a.to_string()),
                None => (line.trim().to_ascii_uppercase(), String::new()),
            };
            let logged = if cmd == "PASS" { "PASS ****".to_string() } else { line.clone() };
            self.st.commands.lock().unwrap().push(logged);
            if cmd == "AUTH" {
                match &self.st.tls {
                    Some((cfg, FtpsMode::Explicit)) if !self.out.is_tls() && matches!(arg.trim().to_ascii_uppercase().as_str(), "TLS" | "SSL" | "TLS-C") => {
                        let cfg = cfg.clone();
                        self.reply("234 Proceed with negotiation")?;
                        let plain = self.out.try_clone()?;
                        let secured = plain.start_tls_server(&cfg)?;
                        self.reader = BufReader::new(secured.try_clone()?);
                        self.out = secured;
                    }
                    _ => self.reply("504 AUTH not supported")?,
                }
                continue;
            }
            if self.st.tls.is_some() && !self.out.is_tls() && !matches!(cmd.as_str(), "QUIT" | "FEAT" | "NOOP" | "SYST") {
                self.reply("530 TLS required (use AUTH TLS)")?;
                continue;
            }
            if cmd == "PBSZ" && self.out.is_tls() {
                self.reply("200 PBSZ=0")?;
                continue;
            }
            if cmd == "PROT" && self.out.is_tls() {
                match arg.trim().to_ascii_uppercase().as_str() {
                    "P" => {
                        self.prot_private = true;
                        self.reply("200 Protection level set to Private")?;
                    }
                    "C" => {
                        self.prot_private = false;
                        self.reply("200 Protection level set to Clear")?;
                    }
                    _ => self.reply("536 Protection level not supported")?,
                }
                continue;
            }
            if !self.logged_in && !matches!(cmd.as_str(), "USER" | "PASS" | "QUIT" | "SYST" | "FEAT" | "NOOP" | "OPTS") {
                self.reply("530 Please login with USER and PASS")?;
                continue;
            }
            match cmd.as_str() {
                "USER" => {
                    self.user = Some(arg.clone());
                    self.logged_in = false;
                    self.reply("331 Password required")?;
                }
                "PASS" => {
                    let ok = match &self.user {
                        None => false,
                        Some(u) => self.st.users.is_empty() || self.st.users.get(u) == Some(&arg),
                    };
                    if ok {
                        self.logged_in = true;
                        self.reply("230 User logged in")?;
                    } else {
                        self.reply("530 Login incorrect")?;
                    }
                }
                "SYST" => self.reply("215 UNIX Type: L8")?,
                "FEAT" if self.st.tls.is_some() => {
                    self.reply("211-Features:\r\n AUTH TLS\r\n PBSZ\r\n PROT\r\n MLSD\r\n SIZE\r\n MDTM\r\n EPSV\r\n UTF8\r\n211 End")?
                }
                "FEAT" => self.reply("211-Features:\r\n MLSD\r\n SIZE\r\n MDTM\r\n EPSV\r\n UTF8\r\n211 End")?,
                "OPTS" | "MODE" | "STRU" | "SITE" | "ALLO" => self.reply("200 OK")?,
                "NOOP" => self.reply("200 NOOP ok")?,
                "PWD" | "XPWD" => {
                    let r = format!("257 \"{}\" is the current directory", self.cwd.replace('"', "\"\""));
                    self.reply(&r)?
                }
                "CWD" | "XCWD" | "CDUP" | "XCUP" => {
                    let target = if cmd.contains("UP") { ".." } else { arg.as_str() };
                    let (v, real) = self.resolve(target);
                    if real.is_dir() {
                        self.cwd = v;
                        self.reply("250 Directory changed")?;
                    } else {
                        self.reply("550 No such directory")?;
                    }
                }
                "TYPE" => {
                    let t = arg.trim().to_ascii_uppercase();
                    if t.starts_with('I') || t.starts_with('L') {
                        self.binary = true;
                        self.reply("200 Type set to I")?;
                    } else if t.starts_with('A') {
                        self.binary = false;
                        self.reply("200 Type set to A")?;
                    } else {
                        self.reply("504 Unsupported type")?;
                    }
                }
                "PASV" | "EPSV" => {
                    let ip = self.out.local_addr().map(|a| a.ip()).unwrap_or(IpAddr::from([127, 0, 0, 1]));
                    let l = TcpListener::bind(SocketAddr::new(ip, 0))?;
                    let p = l.local_addr()?.port();
                    self.pasv = Some(l);
                    self.port = None;
                    if cmd == "EPSV" {
                        self.reply(&format!("229 Entering Extended Passive Mode (|||{}|)", p))?;
                    } else {
                        let o = match ip {
                            IpAddr::V4(v) => v.octets(),
                            IpAddr::V6(_) => [127, 0, 0, 1],
                        };
                        self.reply(&format!("227 Entering Passive Mode ({},{},{},{},{},{})", o[0], o[1], o[2], o[3], p >> 8, p & 0xff))?;
                    }
                }
                "PORT" => {
                    let n: Vec<u16> = arg.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if n.len() == 6 {
                        self.port = Some(SocketAddr::new(IpAddr::from([n[0] as u8, n[1] as u8, n[2] as u8, n[3] as u8]), n[4] * 256 + n[5]));
                        self.pasv = None;
                        self.reply("200 PORT command successful")?;
                    } else {
                        self.reply("501 Syntax error in PORT")?;
                    }
                }
                "EPRT" => {
                    let d = arg.chars().next().unwrap_or('|');
                    let parts: Vec<&str> = arg.split(d).collect();
                    let addr = (parts.len() >= 4).then(|| format!("{}:{}", if parts[2].contains(':') { format!("[{}]", parts[2]) } else { parts[2].to_string() }, parts[3]));
                    match addr.and_then(|a| a.parse::<SocketAddr>().ok()) {
                        Some(a) => {
                            self.port = Some(a);
                            self.pasv = None;
                            self.reply("200 EPRT command successful")?;
                        }
                        None => self.reply("501 Syntax error in EPRT")?,
                    }
                }
                "LIST" | "NLST" | "MLSD" => self.listing(&arg, &cmd)?,
                "REST" => match arg.trim().parse::<u64>() {
                    Ok(n) => {
                        self.rest = n;
                        self.reply(&format!("350 Restarting at {}. Send STORE or RETRIEVE to initiate transfer", n))?;
                    }
                    Err(_) => self.reply("501 Syntax error in REST")?,
                },
                "RETR" => {
                    let (_, real) = self.resolve(&arg);
                    let rest = std::mem::take(&mut self.rest) as usize;
                    match fs::read(&real) {
                        Ok(data) if real.is_file() => {
                            let data = if self.binary { data } else { to_net_ascii(&data) };
                            let data = data[rest.min(data.len())..].to_vec();
                            self.send_data(&data)?;
                        }
                        _ => self.reply("550 File not found")?,
                    }
                }
                "STOR" | "APPE" => {
                    let (_, real) = self.resolve(&arg);
                    if real.parent().map(|p| !p.is_dir()).unwrap_or(true) || real.is_dir() {
                        self.reply("553 Cannot create file")?;
                        continue;
                    }
                    let s = match self.open_data() {
                        Ok(s) => s,
                        Err(_) => {
                            self.reply("425 Can't open data connection")?;
                            continue;
                        }
                    };
                    self.reply("150 Ok to send data")?;
                    let mut s = match self.secure_data(s) {
                        Ok(s) => s,
                        Err(e) => {
                            self.data_tls_failed(&e)?;
                            continue;
                        }
                    };
                    let mut data = Vec::new();
                    let r = s.read_to_end(&mut data);
                    drop(s);
                    if r.is_err() {
                        self.reply("426 Transfer aborted")?;
                        continue;
                    }
                    let data = if self.binary { data } else { from_net_ascii(&data) };
                    let w = if cmd == "APPE" {
                        fs::OpenOptions::new().create(true).append(true).open(&real).and_then(|mut f| f.write_all(&data))
                    } else {
                        fs::write(&real, &data)
                    };
                    match w {
                        Ok(_) => self.reply("226 Transfer complete")?,
                        Err(_) => self.reply("451 Local error writing file")?,
                    }
                }
                "DELE" => {
                    let (_, real) = self.resolve(&arg);
                    if real.is_file() && fs::remove_file(&real).is_ok() {
                        self.reply("250 File deleted")?;
                    } else {
                        self.reply("550 Cannot delete file")?;
                    }
                }
                "MKD" | "XMKD" => {
                    let (v, real) = self.resolve(&arg);
                    if !real.exists() && fs::create_dir(&real).is_ok() {
                        self.reply(&format!("257 \"{}\" created", v))?;
                    } else {
                        self.reply("550 Cannot create directory")?;
                    }
                }
                "RMD" | "XRMD" => {
                    let (_, real) = self.resolve(&arg);
                    if real.is_dir() && fs::remove_dir(&real).is_ok() {
                        self.reply("250 Directory removed")?;
                    } else {
                        self.reply("550 Cannot remove directory")?;
                    }
                }
                "RNFR" => {
                    let (_, real) = self.resolve(&arg);
                    if real.exists() {
                        self.rnfr = Some(real);
                        self.reply("350 Ready for RNTO")?;
                    } else {
                        self.reply("550 No such file")?;
                    }
                }
                "RNTO" => match self.rnfr.take() {
                    Some(from) => {
                        let (_, to) = self.resolve(&arg);
                        if fs::rename(&from, &to).is_ok() {
                            self.reply("250 Rename successful")?;
                        } else {
                            self.reply("553 Rename failed")?;
                        }
                    }
                    None => self.reply("503 RNFR required first")?,
                },
                "SIZE" => {
                    let (_, real) = self.resolve(&arg);
                    match fs::metadata(&real) {
                        Ok(m) if m.is_file() => self.reply(&format!("213 {}", m.len()))?,
                        _ => self.reply("550 No such file")?,
                    }
                }
                "MDTM" => {
                    let (_, real) = self.resolve(&arg);
                    match fs::metadata(&real).ok().filter(|m| m.is_file()).and_then(|m| m.modified().ok()) {
                        Some(t) => {
                            let ms = t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
                            self.reply(&format!("213 {}", format_mdtm(ms)))?
                        }
                        None => self.reply("550 No such file")?,
                    }
                }
                "QUIT" => {
                    self.reply("221 Goodbye")?;
                    return Ok(());
                }
                _ => self.reply("502 Command not implemented")?,
            }
        }
    }
}
