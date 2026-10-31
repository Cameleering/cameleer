//! A blocking FTP client (RFC 959 with RFC 2428 `EPSV`/`EPRT` and RFC 3659 `MLSD`/`SIZE`/`MDTM`).
//!
//! ```no_run
//! use std::time::Duration;
//! use camel_protocols::ftp::FtpClient;
//! let mut c = FtpClient::connect("localhost", 21, Duration::from_secs(10), Duration::from_secs(30)).unwrap();
//! c.login("anonymous", "guest").unwrap();
//! c.set_passive(true);
//! c.set_binary(true).unwrap();
//! c.stor("hello.txt", b"Hello", false).unwrap();
//! let files = c.list("").unwrap();
//! let data = c.retr("hello.txt").unwrap();
//! c.quit();
//! ```
//!
//! FTPS (RFC 4217): [`FtpClient::connect_secure`] with [`FtpSecurity`] for explicit
//! (`AUTH TLS` on the control connection) or implicit (TLS from the first byte) mode;
//! after login [`FtpClient::protect_data`] sends `PBSZ`/`PROT P` so data connections are
//! TLS-protected too (TLS session resumption across channels is not implemented).

use std::io::{BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use camel_tls::camel_support::io_to_camel;
use camel_tls::net::NetStream;
use camel_tls::TlsClientConfig;

use camel_core::component::file::GENERIC_FILE_OPERATION_FAILED;
use camel_core::error::{CamelError, Result};

use super::listing::{parse_list_line, parse_mdtm, parse_mlsd_line, RemoteFile};
use crate::common::{connect, io_error, read_line};

/// A reply from the server: a 3 digit code and the (possibly multi-line) text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FtpReply {
    pub code: u32,
    pub text: String,
}

impl FtpReply {
    pub fn is_positive(&self) -> bool {
        (100..400).contains(&self.code)
    }
}

/// Error raised for a failed FTP operation (`GenericFileOperationFailedException`).
pub fn ftp_error(msg: impl Into<String>) -> CamelError {
    CamelError::new(GENERIC_FILE_OPERATION_FAILED, msg)
}

enum DataChannel {
    Passive(TcpStream),
    Active(TcpListener),
}

/// FTPS mode of a connection.
#[derive(Clone, Debug)]
pub enum FtpSecurity {
    /// Plain FTP.
    None,
    /// `AUTH TLS` after the greeting (RFC 4217), usually on port 21.
    Explicit { tls: Arc<TlsClientConfig>, server_name: String },
    /// TLS from the first byte, usually on port 990.
    Implicit { tls: Arc<TlsClientConfig>, server_name: String },
}

/// A connected FTP control connection.
pub struct FtpClient {
    reader: BufReader<NetStream>,
    writer: NetStream,
    /// TLS settings for protected data connections (`PROT P`).
    data_tls: Option<(Arc<TlsClientConfig>, String)>,
    control_tls: Option<(Arc<TlsClientConfig>, String)>,
    host: String,
    passive: bool,
    binary: Option<bool>,
    data_timeout: Duration,
    connect_timeout: Duration,
    epsv_first: bool,
    mlsd: Option<bool>,
    last: FtpReply,
}

impl FtpClient {
    /// Connect and read the server greeting (`220`).
    pub fn connect(host: &str, port: u16, connect_timeout: Duration, so_timeout: Duration) -> Result<FtpClient> {
        FtpClient::connect_secure(host, port, connect_timeout, so_timeout, &FtpSecurity::None)
    }

    /// Connect with the given FTPS mode and read the greeting; in explicit mode `AUTH TLS`
    /// is negotiated before returning.
    pub fn connect_secure(host: &str, port: u16, connect_timeout: Duration, so_timeout: Duration, security: &FtpSecurity) -> Result<FtpClient> {
        let tcp = connect(host, port, connect_timeout).map_err(|e| ftp_error(format!("Cannot connect to ftp://{}:{}: {}", host, port, e.message)))?;
        let t = if so_timeout.is_zero() { None } else { Some(so_timeout) };
        let _ = tcp.set_read_timeout(t);
        let _ = tcp.set_write_timeout(t);
        let _ = tcp.set_nodelay(true);
        let ipv6 = tcp.peer_addr().map(|a| a.is_ipv6()).unwrap_or(false);
        let stream = match security {
            FtpSecurity::Implicit { tls, server_name } => {
                NetStream::client(tcp, server_name, tls).map_err(|e| io_to_camel(e, &format!("ftps://{}:{}", host, port)))?
            }
            _ => NetStream::plain(tcp),
        };
        let writer = stream.try_clone().map_err(|e| io_error(e, host))?;
        let control_tls = match security {
            FtpSecurity::Implicit { tls, server_name } => Some((tls.clone(), server_name.clone())),
            _ => None,
        };
        let mut c = FtpClient {
            reader: BufReader::new(stream),
            writer,
            data_tls: None,
            control_tls,
            host: host.to_string(),
            passive: true,
            binary: None,
            data_timeout: Duration::from_secs(30),
            connect_timeout,
            epsv_first: ipv6,
            mlsd: None,
            last: FtpReply { code: 0, text: String::new() },
        };
        let mut r = c.read_reply()?;
        if r.code == 120 {
            r = c.read_reply()?;
        }
        if r.code != 220 {
            return Err(ftp_error(format!("Unexpected FTP greeting: {} {}", r.code, r.text)));
        }
        if let FtpSecurity::Explicit { tls, server_name } = security {
            let r = c.send_command("AUTH TLS")?;
            if r.code != 234 {
                return Err(ftp_error(format!("FTP server refused AUTH TLS: {} {}", r.code, r.text)));
            }
            if !c.reader.buffer().is_empty() {
                return Err(ftp_error("Unexpected data after AUTH TLS reply"));
            }
            let plain = c.writer.try_clone().map_err(|e| io_error(e, host))?;
            let secured = plain.start_tls_client(server_name, tls).map_err(|e| io_to_camel(e, &format!("ftp://{}:{} (AUTH TLS)", host, port)))?;
            c.reader = BufReader::new(secured.try_clone().map_err(|e| io_error(e, host))?);
            c.writer = secured;
            c.control_tls = Some((tls.clone(), server_name.clone()));
        }
        Ok(c)
    }

    /// Whether the control connection is TLS-protected.
    pub fn is_secure(&self) -> bool {
        self.writer.is_tls()
    }

    /// FTPS: `PBSZ <pbsz>` + `PROT <prot>` (`P` = private: data connections use TLS,
    /// `C` = clear). Requires a TLS control connection.
    pub fn protect_data(&mut self, pbsz: u64, prot: &str) -> Result<()> {
        let tls = self.control_tls.clone().ok_or_else(|| ftp_error("PROT requires a TLS control connection"))?;
        self.expect(&format!("PBSZ {}", pbsz), |c| c == 200)?;
        let prot = prot.trim().to_ascii_uppercase();
        self.expect(&format!("PROT {}", prot), |c| c == 200)?;
        self.data_tls = if prot == "P" { Some(tls) } else { None };
        Ok(())
    }

    /// Use passive (`PASV`/`EPSV`, the client connects) or active (`PORT`/`EPRT`) data connections.
    pub fn set_passive(&mut self, passive: bool) {
        self.passive = passive;
    }

    /// Timeout for opening and reading data connections.
    pub fn set_data_timeout(&mut self, t: Duration) {
        self.data_timeout = t;
    }

    /// The last reply received.
    pub fn last_reply(&self) -> &FtpReply {
        &self.last
    }

    fn read_reply(&mut self) -> Result<FtpReply> {
        let first = read_line(&mut self.reader, 64 * 1024)
            .map_err(|e| io_error(e, &self.host))?
            .ok_or_else(|| ftp_error(format!("Connection closed by FTP server {}", self.host)))?;
        if first.len() < 3 || !first.as_bytes()[..3].iter().all(|b| b.is_ascii_digit()) {
            return Err(ftp_error(format!("Malformed FTP reply: {}", first)));
        }
        let code: u32 = first[..3].parse().unwrap_or(0);
        let mut text = first.get(4..).unwrap_or("").to_string();
        if first.as_bytes().get(3) == Some(&b'-') {
            let end = format!("{} ", &first[..3]);
            loop {
                let l = read_line(&mut self.reader, 64 * 1024)
                    .map_err(|e| io_error(e, &self.host))?
                    .ok_or_else(|| ftp_error("Connection closed during multi-line reply"))?;
                if l.starts_with(&end) || l == first[..3] {
                    text.push('\n');
                    text.push_str(l.get(4..).unwrap_or(""));
                    break;
                }
                text.push('\n');
                text.push_str(&l);
            }
        }
        self.last = FtpReply { code, text: text.clone() };
        Ok(FtpReply { code, text })
    }

    /// Send a raw command and read the reply (no check of the reply code).
    pub fn send_command(&mut self, cmd: &str) -> Result<FtpReply> {
        self.writer.write_all(format!("{}\r\n", cmd).as_bytes()).and_then(|_| self.writer.flush()).map_err(|e| io_error(e, &self.host))?;
        self.read_reply()
    }

    fn expect(&mut self, cmd: &str, ok: impl Fn(u32) -> bool) -> Result<FtpReply> {
        let r = self.send_command(cmd)?;
        if ok(r.code) {
            Ok(r)
        } else {
            let shown = if cmd.starts_with("PASS ") { "PASS ****" } else { cmd };
            Err(ftp_error(format!("FTP command {} failed: {} {}", shown, r.code, r.text)))
        }
    }

    /// `USER`/`PASS` login.
    pub fn login(&mut self, user: &str, password: &str) -> Result<()> {
        let r = self.send_command(&format!("USER {}", user))?;
        match r.code {
            230 => Ok(()),
            331 | 332 => {
                let r = self.send_command(&format!("PASS {}", password))?;
                if r.code == 230 || r.code == 202 {
                    Ok(())
                } else {
                    Err(ftp_error(format!("Cannot login as {}: {} {}", user, r.code, r.text)))
                }
            }
            _ => Err(ftp_error(format!("Cannot login as {}: {} {}", user, r.code, r.text))),
        }
    }

    /// `TYPE I` (binary) or `TYPE A` (ASCII, with CRLF conversion on transfers).
    pub fn set_binary(&mut self, binary: bool) -> Result<()> {
        if self.binary == Some(binary) {
            return Ok(());
        }
        self.expect(if binary { "TYPE I" } else { "TYPE A" }, |c| c == 200)?;
        self.binary = Some(binary);
        Ok(())
    }

    /// `PWD`: the current directory.
    pub fn pwd(&mut self) -> Result<String> {
        let r = self.expect("PWD", |c| c == 257)?;
        let t = &r.text;
        Ok(match (t.find('"'), t.rfind('"')) {
            (Some(a), Some(b)) if b > a => t[a + 1..b].replace("\"\"", "\""),
            _ => t.split_whitespace().next().unwrap_or("/").to_string(),
        })
    }

    /// `CWD path`.
    pub fn cwd(&mut self, path: &str) -> Result<()> {
        self.expect(&format!("CWD {}", path), |c| (200..300).contains(&c)).map(|_| ())
    }

    /// `CDUP`.
    pub fn cdup(&mut self) -> Result<()> {
        self.expect("CDUP", |c| (200..300).contains(&c)).map(|_| ())
    }

    /// `MKD path`; returns false if the server refused (e.g. it already exists).
    pub fn mkd(&mut self, path: &str) -> Result<bool> {
        Ok(self.send_command(&format!("MKD {}", path))?.code == 257)
    }

    /// Create a directory and its missing parents (errors for existing ones are ignored).
    pub fn mkdirs(&mut self, path: &str) -> Result<()> {
        let abs = path.starts_with('/');
        let mut cur = String::new();
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            if !cur.is_empty() || abs {
                cur.push('/');
            }
            cur.push_str(seg);
            self.mkd(&cur)?;
        }
        Ok(())
    }

    /// `RMD path`.
    pub fn rmd(&mut self, path: &str) -> Result<()> {
        self.expect(&format!("RMD {}", path), |c| (200..300).contains(&c)).map(|_| ())
    }

    /// `DELE path`.
    pub fn dele(&mut self, path: &str) -> Result<()> {
        self.expect(&format!("DELE {}", path), |c| (200..300).contains(&c)).map(|_| ())
    }

    /// `RNFR from` + `RNTO to`.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        self.expect(&format!("RNFR {}", from), |c| c == 350)?;
        self.expect(&format!("RNTO {}", to), |c| (200..300).contains(&c)).map(|_| ())
    }

    /// `SIZE path` (None if unsupported or the file does not exist).
    pub fn size(&mut self, path: &str) -> Result<Option<u64>> {
        let r = self.send_command(&format!("SIZE {}", path))?;
        Ok(if r.code == 213 { r.text.trim().parse().ok() } else { None })
    }

    /// `MDTM path`: modification time in epoch millis.
    pub fn mdtm(&mut self, path: &str) -> Result<Option<i64>> {
        let r = self.send_command(&format!("MDTM {}", path))?;
        Ok(if r.code == 213 { parse_mdtm(&r.text) } else { None })
    }

    /// Whether the file exists (via `SIZE`, falling back to a listing of the parent).
    pub fn exists(&mut self, path: &str) -> Result<bool> {
        if self.size(path)?.is_some() {
            return Ok(true);
        }
        let (dir, name) = match path.rfind('/') {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", path),
        };
        let dir = if dir.is_empty() && path.starts_with('/') { "/" } else { dir };
        Ok(self.list(dir).map(|l| l.iter().any(|f| f.name == name)).unwrap_or(false))
    }

    /// `NOOP`; returns true if the connection is alive.
    pub fn noop(&mut self) -> bool {
        matches!(self.send_command("NOOP"), Ok(r) if r.code == 200)
    }

    /// `QUIT` and close (errors are ignored).
    pub fn quit(mut self) {
        let _ = self.send_command("QUIT");
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }

    fn parse_pasv(text: &str) -> Option<(IpAddr, u16)> {
        let start = text.find(|c: char| c.is_ascii_digit())?;
        let nums: Vec<u16> = text[start..]
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .take(6)
            .filter_map(|s| s.parse().ok())
            .collect();
        if nums.len() < 6 || nums.iter().any(|&n| n > 255) {
            return None;
        }
        let ip = IpAddr::from([nums[0] as u8, nums[1] as u8, nums[2] as u8, nums[3] as u8]);
        Some((ip, nums[4] * 256 + nums[5]))
    }

    fn parse_epsv(text: &str) -> Option<u16> {
        let a = text.find('(')?;
        let b = text[a..].find(')')? + a;
        let inner = &text[a + 1..b];
        let d = inner.chars().next()?;
        inner.split(d).rfind(|s| !s.is_empty())?.parse().ok()
    }

    fn open_data(&mut self) -> Result<DataChannel> {
        let peer = self.writer.peer_addr().map_err(|e| io_error(e, &self.host))?;
        if self.passive {
            let mut order = [true, false];
            if !self.epsv_first {
                order = [false, true];
            }
            let mut last_err = None;
            for epsv in order {
                let target = if epsv {
                    let r = self.send_command("EPSV")?;
                    if r.code != 229 {
                        last_err = Some(format!("{} {}", r.code, r.text));
                        continue;
                    }
                    Self::parse_epsv(&r.text).map(|p| SocketAddr::new(peer.ip(), p))
                } else {
                    let r = self.send_command("PASV")?;
                    if r.code != 227 {
                        last_err = Some(format!("{} {}", r.code, r.text));
                        continue;
                    }
                    Self::parse_pasv(&r.text).map(|(ip, p)| SocketAddr::new(if ip.is_unspecified() { peer.ip() } else { ip }, p))
                };
                let target = target.ok_or_else(|| ftp_error(format!("Cannot parse passive mode reply: {}", self.last.text)))?;
                self.epsv_first = epsv;
                let t = if self.connect_timeout.is_zero() { Duration::from_secs(30) } else { self.connect_timeout };
                let s = TcpStream::connect_timeout(&target, t).map_err(|e| ftp_error(format!("Cannot open data connection to {}: {}", target, e)))?;
                return Ok(DataChannel::Passive(s));
            }
            Err(ftp_error(format!("Server refused passive mode: {}", last_err.unwrap_or_default())))
        } else {
            let local = self.writer.local_addr().map_err(|e| io_error(e, &self.host))?;
            let listener = TcpListener::bind(SocketAddr::new(local.ip(), 0)).map_err(|e| io_error(e, "active data listener"))?;
            let port = listener.local_addr().map_err(|e| io_error(e, "active data listener"))?.port();
            match local.ip() {
                IpAddr::V4(v4) => {
                    let o = v4.octets();
                    self.expect(&format!("PORT {},{},{},{},{},{}", o[0], o[1], o[2], o[3], port >> 8, port & 0xff), |c| c == 200)?;
                }
                IpAddr::V6(v6) => {
                    self.expect(&format!("EPRT |2|{}|{}|", v6, port), |c| c == 200)?;
                }
            }
            Ok(DataChannel::Active(listener))
        }
    }

    fn accept_data(&self, ch: DataChannel) -> Result<NetStream> {
        let s = match ch {
            DataChannel::Passive(s) => s,
            DataChannel::Active(l) => {
                l.set_nonblocking(true).map_err(|e| io_error(e, "data listener"))?;
                let deadline = Instant::now() + if self.data_timeout.is_zero() { Duration::from_secs(30) } else { self.data_timeout };
                loop {
                    match l.accept() {
                        Ok((s, _)) => {
                            let _ = s.set_nonblocking(false);
                            break s;
                        }
                        Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                        Err(e) => return Err(ftp_error(format!("Server did not open the active data connection: {}", e))),
                    }
                }
            }
        };
        let t = if self.data_timeout.is_zero() { None } else { Some(self.data_timeout) };
        let _ = s.set_read_timeout(t);
        let _ = s.set_write_timeout(t);
        match &self.data_tls {
            Some((cfg, name)) => NetStream::client(s, name, cfg).map_err(|e| io_to_camel(e, &format!("ftps data connection to {}", self.host))),
            None => Ok(NetStream::plain(s)),
        }
    }

    /// Run a data transfer command: upload `data` or download the result.
    fn transfer(&mut self, cmd: &str, upload: Option<&[u8]>) -> Result<Vec<u8>> {
        let ch = self.open_data()?;
        let r = self.send_command(cmd)?;
        if !(100..200).contains(&r.code) {
            return Err(ftp_error(format!("FTP command {} failed: {} {}", cmd, r.code, r.text)));
        }
        let mut s = self.accept_data(ch)?;
        let mut out = Vec::new();
        let io = match upload {
            Some(d) => s.write_all(d).and_then(|_| s.flush()),
            None => s.read_to_end(&mut out).map(|_| ()),
        };
        let _ = s.shutdown(std::net::Shutdown::Both);
        drop(s);
        io.map_err(|e| ftp_error(format!("Data transfer for {} failed: {}", cmd, e)))?;
        let done = self.read_reply()?;
        if !(200..300).contains(&done.code) {
            return Err(ftp_error(format!("FTP command {} failed: {} {}", cmd, done.code, done.text)));
        }
        Ok(out)
    }

    /// List a directory (`MLSD`, falling back to `LIST` when unsupported). `""` lists the
    /// current directory.
    pub fn list(&mut self, path: &str) -> Result<Vec<RemoteFile>> {
        let arg = |c: &str| if path.is_empty() { c.to_string() } else { format!("{} {}", c, path) };
        // listings are transferred in ASCII mode semantics but we parse line by line either way
        if self.mlsd != Some(false) {
            match self.transfer(&arg("MLSD"), None) {
                Ok(data) => {
                    self.mlsd = Some(true);
                    return Ok(String::from_utf8_lossy(&data).lines().filter_map(parse_mlsd_line).collect());
                }
                Err(e) if self.mlsd.is_none() && (self.last.code == 500 || self.last.code == 502 || self.last.code == 504) => {
                    let _ = e;
                    self.mlsd = Some(false);
                }
                Err(e) => return Err(e),
            }
        }
        let data = self.transfer(&arg("LIST"), None)?;
        Ok(String::from_utf8_lossy(&data).lines().filter_map(parse_list_line).collect())
    }

    /// `NLST`: plain names.
    pub fn nlst(&mut self, path: &str) -> Result<Vec<String>> {
        let cmd = if path.is_empty() { "NLST".to_string() } else { format!("NLST {}", path) };
        let data = self.transfer(&cmd, None)?;
        Ok(String::from_utf8_lossy(&data).lines().map(|l| l.trim_end().to_string()).filter(|l| !l.is_empty()).collect())
    }

    /// `RETR path`: download a file.
    pub fn retr(&mut self, path: &str) -> Result<Vec<u8>> {
        let data = self.transfer(&format!("RETR {}", path), None)?;
        Ok(if self.binary == Some(false) { from_net_ascii(&data) } else { data })
    }

    /// `STOR path` (or `APPE` when `append`): upload a file.
    pub fn stor(&mut self, path: &str, data: &[u8], append: bool) -> Result<()> {
        let cmd = format!("{} {}", if append { "APPE" } else { "STOR" }, path);
        if self.binary == Some(false) {
            self.transfer(&cmd, Some(&to_net_ascii(data)))?;
        } else {
            self.transfer(&cmd, Some(data))?;
        }
        Ok(())
    }
}

/// Convert bare `\n` to `\r\n` (network ASCII).
pub fn to_net_ascii(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len() + d.len() / 32);
    let mut prev = 0u8;
    for &b in d {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        prev = b;
    }
    out
}

/// Convert `\r\n` to `\n` (from network ASCII).
pub fn from_net_ascii(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len());
    let mut i = 0;
    while i < d.len() {
        if d[i] == b'\r' && d.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(d[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passive_replies() {
        assert_eq!(
            FtpClient::parse_pasv("Entering Passive Mode (127,0,0,1,195,80)."),
            Some((IpAddr::from([127, 0, 0, 1]), 195 * 256 + 80))
        );
        assert_eq!(FtpClient::parse_epsv("Entering Extended Passive Mode (|||6446|)"), Some(6446));
    }

    #[test]
    fn ascii_conversion() {
        assert_eq!(to_net_ascii(b"a\nb\r\nc"), b"a\r\nb\r\nc");
        assert_eq!(from_net_ascii(b"a\r\nb\rc"), b"a\nb\rc");
    }
}
