//! A blocking IMAP4rev1 client subset (RFC 3501): `LOGIN`, `SELECT`, `SEARCH`,
//! `FETCH n BODY.PEEK[]`, `STORE`, `COPY`, `EXPUNGE`, `NOOP`, `LOGOUT`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use camel_core::error::Result;

use super::{auth_error, messaging_error};
use crate::common::{connect, io_error};

/// An untagged response (`* ...`) with the literals it carried.
#[derive(Clone, Debug, Default)]
pub struct Untagged {
    pub text: String,
    pub literals: Vec<Vec<u8>>,
}

/// The result of a tagged command.
#[derive(Clone, Debug)]
pub struct ImapResponse {
    /// `OK`, `NO` or `BAD`.
    pub status: String,
    pub text: String,
    pub untagged: Vec<Untagged>,
}

/// Quote a string for IMAP.
pub fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A connected IMAP session.
pub struct ImapClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    host: String,
    tag: u32,
}

impl ImapClient {
    /// Connect and read the `* OK` greeting.
    pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<ImapClient> {
        let s = connect(host, port, timeout).map_err(|e| messaging_error(format!("Couldn't connect to host, port: {}, {}; {}", host, port, e.message)))?;
        let t = if timeout.is_zero() { None } else { Some(timeout) };
        let _ = s.set_read_timeout(t);
        let _ = s.set_write_timeout(t);
        let writer = s.try_clone().map_err(|e| io_error(e, host))?;
        let mut c = ImapClient { reader: BufReader::new(s), writer, host: host.to_string(), tag: 0 };
        let (line, _) = c.read_response_line()?;
        if !line.starts_with("* OK") && !line.starts_with("* PREAUTH") {
            return Err(messaging_error(format!("IMAP server refused connection: {}", line)));
        }
        Ok(c)
    }

    /// Read one response line, including any literals (`{n}`) it announces.
    fn read_response_line(&mut self) -> Result<(String, Vec<Vec<u8>>)> {
        let mut text = String::new();
        let mut literals = Vec::new();
        loop {
            let mut buf = Vec::new();
            let n = self.reader.read_until(b'\n', &mut buf).map_err(|e| io_error(e, &self.host))?;
            if n == 0 {
                return Err(messaging_error(format!("Connection closed by IMAP server {}", self.host)));
            }
            while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
                buf.pop();
            }
            let line = String::from_utf8_lossy(&buf).into_owned();
            text.push_str(&line);
            let lit = line.strip_suffix('}').and_then(|l| l.rfind('{').map(|i| &l[i + 1..])).and_then(|n| n.trim_end_matches('+').parse::<usize>().ok());
            match lit {
                Some(size) => {
                    let mut data = vec![0u8; size];
                    self.reader.read_exact(&mut data).map_err(|e| io_error(e, &self.host))?;
                    literals.push(data);
                }
                None => return Ok((text, literals)),
            }
        }
    }

    /// Run a command and collect the responses up to its tagged completion.
    pub fn command(&mut self, cmd: &str) -> Result<ImapResponse> {
        self.tag += 1;
        let tag = format!("A{:03}", self.tag);
        self.writer.write_all(format!("{} {}\r\n", tag, cmd).as_bytes()).and_then(|_| self.writer.flush()).map_err(|e| io_error(e, &self.host))?;
        let mut untagged = Vec::new();
        loop {
            let (line, literals) = self.read_response_line()?;
            if let Some(rest) = line.strip_prefix("* ") {
                untagged.push(Untagged { text: rest.to_string(), literals });
            } else if let Some(rest) = line.strip_prefix(&format!("{} ", tag)) {
                let (status, text) = rest.split_once(' ').unwrap_or((rest, ""));
                return Ok(ImapResponse { status: status.to_ascii_uppercase(), text: text.to_string(), untagged });
            }
            // continuation requests ("+ ...") and unknown lines are ignored
        }
    }

    fn expect(&mut self, cmd: &str) -> Result<ImapResponse> {
        let r = self.command(cmd)?;
        if r.status == "OK" {
            Ok(r)
        } else {
            let shown = if cmd.starts_with("LOGIN") { "LOGIN" } else { cmd };
            Err(messaging_error(format!("IMAP command {} failed: {} {}", shown, r.status, r.text)))
        }
    }

    /// `LOGIN user password`.
    pub fn login(&mut self, user: &str, password: &str) -> Result<()> {
        let r = self.command(&format!("LOGIN {} {}", quote(user), quote(password)))?;
        if r.status != "OK" {
            return Err(auth_error(format!("Authentication failed for {}: {}", user, r.text)));
        }
        Ok(())
    }

    /// `SELECT folder`; returns the number of messages (`EXISTS`).
    pub fn select(&mut self, folder: &str) -> Result<usize> {
        let r = self.expect(&format!("SELECT {}", quote(folder)))?;
        Ok(r.untagged.iter().find_map(|u| u.text.strip_suffix(" EXISTS").and_then(|n| n.trim().parse().ok())).unwrap_or(0))
    }

    /// `SEARCH criteria` (e.g. `UNSEEN`, `ALL`): message sequence numbers.
    pub fn search(&mut self, criteria: &str) -> Result<Vec<u32>> {
        let r = self.expect(&format!("SEARCH {}", criteria))?;
        Ok(r.untagged
            .iter()
            .filter_map(|u| u.text.strip_prefix("SEARCH"))
            .flat_map(|s| s.split_whitespace().filter_map(|n| n.parse().ok()).collect::<Vec<u32>>())
            .collect())
    }

    /// `FETCH n BODY.PEEK[]`: the raw message (does not set `\Seen`).
    pub fn fetch_message(&mut self, n: u32) -> Result<Vec<u8>> {
        let r = self.expect(&format!("FETCH {} BODY.PEEK[]", n))?;
        for u in &r.untagged {
            if u.text.contains("FETCH") {
                if let Some(l) = u.literals.first() {
                    return Ok(l.clone());
                }
                if let Some(i) = u.text.find("BODY[] \"") {
                    let rest = &u.text[i + 8..];
                    if let Some(end) = rest.rfind('"') {
                        return Ok(rest[..end].replace("\\\"", "\"").replace("\\\\", "\\").into_bytes());
                    }
                }
            }
        }
        Err(messaging_error(format!("No message body returned for message {}", n)))
    }

    /// `STORE n flags` (e.g. `+FLAGS (\Seen)`).
    pub fn store(&mut self, n: u32, flags: &str) -> Result<()> {
        self.expect(&format!("STORE {} {}", n, flags)).map(|_| ())
    }

    /// `COPY n folder`.
    pub fn copy(&mut self, n: u32, folder: &str) -> Result<()> {
        self.expect(&format!("COPY {} {}", n, quote(folder))).map(|_| ())
    }

    /// `EXPUNGE`.
    pub fn expunge(&mut self) -> Result<()> {
        self.expect("EXPUNGE").map(|_| ())
    }

    /// `NOOP`.
    pub fn noop(&mut self) -> bool {
        matches!(self.command("NOOP"), Ok(r) if r.status == "OK")
    }

    /// `LOGOUT` and close.
    pub fn logout(mut self) {
        let _ = self.command("LOGOUT");
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }
}
