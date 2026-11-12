//! A blocking POP3 client (RFC 1939): `USER`/`PASS`, `STAT`, `LIST`, `UIDL`, `RETR`,
//! `DELE`, `RSET`, `NOOP`, `QUIT`.

use std::io::{BufReader, Write};
use std::time::Duration;

use cameleer_tls::net::NetStream;

use cameleer_core::error::Result;

use super::mime::read_dot_terminated;
use super::{auth_error, messaging_error};
use super::{MailTls, StartTls};
use cameleer_tls::net::{connect_stream, io_error, read_line, upgrade_stream};

/// A connected POP3 session.
pub struct Pop3Client {
    reader: BufReader<NetStream>,
    writer: NetStream,
    host: String,
}

impl Pop3Client {
    /// Connect and read the `+OK` greeting.
    pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<Pop3Client> {
        Pop3Client::connect_with(host, port, timeout, None)
    }

    /// Connect with implicit TLS (`pop3s`) or STARTTLS (`STLS`, RFC 2595).
    pub fn connect_with(host: &str, port: u16, timeout: Duration, tls: Option<&MailTls>) -> Result<Pop3Client> {
        let implicit = tls.filter(|t| t.implicit).map(|t| &t.tls);
        let s = connect_stream(host, port, timeout, implicit).map_err(|e| {
            if e.is_a("SSLException") {
                e
            } else {
                messaging_error(format!("Couldn't connect to host, port: {}, {}; {}", host, port, e.message))
            }
        })?;
        let t = if timeout.is_zero() { None } else { Some(timeout) };
        let _ = s.set_read_timeout(t);
        let _ = s.set_write_timeout(t);
        let writer = s.try_clone().map_err(|e| io_error(e, host))?;
        let mut c = Pop3Client { reader: BufReader::new(s), writer, host: host.to_string() };
        let (ok, text) = c.read_status()?;
        if !ok {
            return Err(messaging_error(format!("POP3 server refused connection: {}", text)));
        }
        if let Some(t) = tls.filter(|t| !t.implicit && t.starttls != StartTls::Off) {
            match c.command("STLS")? {
                Ok(_) => upgrade_stream(&mut c.reader, &mut c.writer, &t.tls, host)?,
                Err(e) if t.starttls == StartTls::Required => {
                    return Err(messaging_error(format!("STARTTLS is required but POP3 server {} refused STLS: {}", host, e)))
                }
                Err(_) => {}
            }
        }
        Ok(c)
    }

    fn read_status(&mut self) -> Result<(bool, String)> {
        let line = read_line(&mut self.reader, 64 * 1024)
            .map_err(|e| io_error(e, &self.host))?
            .ok_or_else(|| messaging_error(format!("Connection closed by POP3 server {}", self.host)))?;
        if let Some(rest) = line.strip_prefix("+OK") {
            Ok((true, rest.trim().to_string()))
        } else if let Some(rest) = line.strip_prefix("-ERR") {
            Ok((false, rest.trim().to_string()))
        } else {
            Err(messaging_error(format!("Malformed POP3 response: {}", line)))
        }
    }

    /// Send a command and read the status line: `Ok(Ok(text))` for `+OK`, `Ok(Err(text))` for `-ERR`.
    pub fn command(&mut self, cmd: &str) -> Result<std::result::Result<String, String>> {
        self.writer.write_all(format!("{}\r\n", cmd).as_bytes()).and_then(|_| self.writer.flush()).map_err(|e| io_error(e, &self.host))?;
        let (ok, text) = self.read_status()?;
        Ok(if ok { Ok(text) } else { Err(text) })
    }

    fn expect(&mut self, cmd: &str) -> Result<String> {
        self.command(cmd)?.map_err(|t| messaging_error(format!("POP3 command {} failed: {}", cmd.split_whitespace().next().unwrap_or(cmd), t)))
    }

    fn multi_line(&mut self) -> Result<String> {
        read_dot_terminated(&mut self.reader, 1024 * 1024).map_err(|e| io_error(e, &self.host))
    }

    /// `USER` + `PASS`.
    pub fn login(&mut self, user: &str, password: &str) -> Result<()> {
        if let Err(t) = self.command(&format!("USER {}", user))? {
            return Err(auth_error(format!("Authentication failed for {}: {}", user, t)));
        }
        if let Err(t) = self.command(&format!("PASS {}", password))? {
            return Err(auth_error(format!("Authentication failed for {}: {}", user, t)));
        }
        Ok(())
    }

    /// `STAT`: (message count, total size).
    pub fn stat(&mut self) -> Result<(usize, u64)> {
        let t = self.expect("STAT")?;
        let mut it = t.split_whitespace();
        Ok((it.next().and_then(|n| n.parse().ok()).unwrap_or(0), it.next().and_then(|n| n.parse().ok()).unwrap_or(0)))
    }

    /// `LIST`: (message number, size) pairs.
    pub fn list(&mut self) -> Result<Vec<(usize, u64)>> {
        self.expect("LIST")?;
        let data = self.multi_line()?;
        Ok(data
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            })
            .collect())
    }

    /// `UIDL`: (message number, unique id) pairs, or None if unsupported.
    pub fn uidl(&mut self) -> Result<Option<Vec<(usize, String)>>> {
        if self.command("UIDL")?.is_err() {
            return Ok(None);
        }
        let data = self.multi_line()?;
        Ok(Some(
            data.lines()
                .filter_map(|l| {
                    let mut it = l.split_whitespace();
                    Some((it.next()?.parse().ok()?, it.next()?.to_string()))
                })
                .collect(),
        ))
    }

    /// `RETR n`: the raw message.
    pub fn retr(&mut self, n: usize) -> Result<String> {
        self.expect(&format!("RETR {}", n))?;
        self.multi_line()
    }

    /// `DELE n` (applied at `QUIT`).
    pub fn dele(&mut self, n: usize) -> Result<()> {
        self.expect(&format!("DELE {}", n)).map(|_| ())
    }

    /// `RSET`: undo deletions.
    pub fn rset(&mut self) -> Result<()> {
        self.expect("RSET").map(|_| ())
    }

    /// `NOOP`.
    pub fn noop(&mut self) -> bool {
        matches!(self.command("NOOP"), Ok(Ok(_)))
    }

    /// `QUIT` (commits deletions) and close.
    pub fn quit(mut self) -> Result<()> {
        let r = self.expect("QUIT").map(|_| ());
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
        r
    }
}
