//! A blocking SMTP client (RFC 5321): `EHLO`/`HELO`, `AUTH PLAIN`/`AUTH LOGIN`,
//! `MAIL FROM`, `RCPT TO`, `DATA` with dot-stuffing, `RSET`, `NOOP`, `QUIT`.

use std::io::{BufReader, Write};
use std::time::Duration;

use cameleer_tls::camel_support::EndpointTls;
use cameleer_tls::net::NetStream;

use cameleer_core::dataformat::base64;
use cameleer_core::error::Result;

use super::mime::dot_stuff;
use super::{auth_error, messaging_error};
use super::{MailTls, StartTls};
use cameleer_tls::net::{connect_stream, io_error, read_line, upgrade_stream};

/// A connected SMTP session.
pub struct SmtpClient {
    reader: BufReader<NetStream>,
    writer: NetStream,
    host: String,
    extensions: Vec<String>,
}

impl SmtpClient {
    /// Connect and read the `220` greeting.
    pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<SmtpClient> {
        SmtpClient::connect_with(host, port, timeout, None)
    }

    /// Connect with implicit TLS (`smtps`) when `implicit_tls` is given.
    pub fn connect_with(host: &str, port: u16, timeout: Duration, implicit_tls: Option<&EndpointTls>) -> Result<SmtpClient> {
        let s = connect_stream(host, port, timeout, implicit_tls).map_err(|e| {
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
        let mut c = SmtpClient { reader: BufReader::new(s), writer, host: host.to_string(), extensions: Vec::new() };
        let (code, text) = c.read_reply()?;
        if code != 220 {
            return Err(messaging_error(format!("Unexpected SMTP greeting: {} {}", code, text)));
        }
        Ok(c)
    }

    fn read_reply(&mut self) -> Result<(u32, String)> {
        let mut text = String::new();
        loop {
            let line = read_line(&mut self.reader, 64 * 1024)
                .map_err(|e| io_error(e, &self.host))?
                .ok_or_else(|| messaging_error(format!("Connection closed by SMTP server {}", self.host)))?;
            if line.len() < 3 {
                return Err(messaging_error(format!("Malformed SMTP reply: {}", line)));
            }
            let code: u32 = line[..3].parse().map_err(|_| messaging_error(format!("Malformed SMTP reply: {}", line)))?;
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(line.get(4..).unwrap_or(""));
            if line.as_bytes().get(3) != Some(&b'-') {
                return Ok((code, text));
            }
        }
    }

    /// Send a command line and read the reply.
    pub fn command(&mut self, cmd: &str) -> Result<(u32, String)> {
        self.writer.write_all(format!("{}\r\n", cmd).as_bytes()).and_then(|_| self.writer.flush()).map_err(|e| io_error(e, &self.host))?;
        self.read_reply()
    }

    fn expect(&mut self, cmd: &str, ok: &[u32]) -> Result<String> {
        let (code, text) = self.command(cmd)?;
        if ok.contains(&code) {
            Ok(text)
        } else {
            Err(messaging_error(format!("SMTP command {} failed: {} {}", cmd.split_whitespace().next().unwrap_or(cmd), code, text)))
        }
    }

    /// `EHLO domain`, falling back to `HELO`. Records the advertised extensions.
    pub fn hello(&mut self, domain: &str) -> Result<()> {
        let (code, text) = self.command(&format!("EHLO {}", domain))?;
        if code == 250 {
            self.extensions = text.lines().skip(1).map(|l| l.trim().to_string()).collect();
            return Ok(());
        }
        self.expect(&format!("HELO {}", domain), &[250]).map(|_| ())
    }

    /// Whether the connection is TLS-protected.
    pub fn is_secure(&self) -> bool {
        self.writer.is_tls()
    }

    /// `STARTTLS` (RFC 3207): upgrades the connection and repeats `EHLO`. Returns
    /// false (and stays plain) if the server does not advertise STARTTLS.
    pub fn starttls(&mut self, tls: &EndpointTls, domain: &str) -> Result<bool> {
        if !self.extensions.iter().any(|e| e.eq_ignore_ascii_case("STARTTLS")) {
            return Ok(false);
        }
        self.expect("STARTTLS", &[220])?;
        let host = self.host.clone();
        upgrade_stream(&mut self.reader, &mut self.writer, tls, &host)?;
        self.extensions.clear();
        self.hello(domain)?;
        Ok(true)
    }

    /// Extensions advertised in the `EHLO` reply (e.g. `AUTH PLAIN LOGIN`, `8BITMIME`).
    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    fn auth_mechanisms(&self) -> Vec<String> {
        self.extensions
            .iter()
            .filter_map(|e| {
                let u = e.to_ascii_uppercase();
                u.strip_prefix("AUTH ").or_else(|| u.strip_prefix("AUTH=")).map(|m| m.split_whitespace().map(|s| s.to_string()).collect::<Vec<_>>())
            })
            .flatten()
            .collect()
    }

    /// Authenticate with `AUTH PLAIN` (preferred) or `AUTH LOGIN`.
    pub fn authenticate(&mut self, user: &str, password: &str) -> Result<()> {
        let mechs = self.auth_mechanisms();
        let fail = |code: u32, text: &str| auth_error(format!("Authentication failed for {}: {} {}", user, code, text));
        if mechs.iter().any(|m| m == "PLAIN") || !mechs.iter().any(|m| m == "LOGIN") {
            let token = base64::encode(format!("\0{}\0{}", user, password).as_bytes(), false);
            let (code, text) = self.command(&format!("AUTH PLAIN {}", token))?;
            return if code == 235 { Ok(()) } else { Err(fail(code, &text)) };
        }
        let (code, text) = self.command("AUTH LOGIN")?;
        if code != 334 {
            return Err(fail(code, &text));
        }
        let (code, text) = self.command(&base64::encode(user.as_bytes(), false))?;
        if code != 334 {
            return Err(fail(code, &text));
        }
        let (code, text) = self.command(&base64::encode(password.as_bytes(), false))?;
        if code != 235 {
            return Err(fail(code, &text));
        }
        Ok(())
    }

    /// Send one message: `MAIL FROM`, `RCPT TO` for each recipient, `DATA`.
    pub fn send_mail(&mut self, from: &str, recipients: &[String], message: &str) -> Result<()> {
        if recipients.is_empty() {
            return Err(messaging_error("No recipient addresses"));
        }
        self.expect(&format!("MAIL FROM:<{}>", from), &[250])?;
        for r in recipients {
            self.expect(&format!("RCPT TO:<{}>", r), &[250, 251])?;
        }
        self.expect("DATA", &[354])?;
        let mut data = dot_stuff(message);
        if !data.ends_with("\r\n") {
            data.push_str("\r\n");
        }
        data.push_str(".\r\n");
        self.writer.write_all(data.as_bytes()).and_then(|_| self.writer.flush()).map_err(|e| io_error(e, &self.host))?;
        let (code, text) = self.read_reply()?;
        if code != 250 {
            return Err(messaging_error(format!("Message rejected: {} {}", code, text)));
        }
        Ok(())
    }

    /// `QUIT` and close.
    pub fn quit(mut self) {
        let _ = self.command("QUIT");
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }
}

/// Convenience: deliver one message (connect, hello, optional auth, send, quit).
pub fn send(host: &str, port: u16, timeout: Duration, auth: Option<(&str, &str)>, from: &str, recipients: &[String], message: &str) -> Result<()> {
    send_with(host, port, timeout, None, auth, from, recipients, message)
}

/// Like [`send`] with TLS: implicit (`smtps`) or STARTTLS.
#[allow(clippy::too_many_arguments)]
pub fn send_with(host: &str, port: u16, timeout: Duration, tls: Option<&MailTls>, auth: Option<(&str, &str)>, from: &str, recipients: &[String], message: &str) -> Result<()> {
    let implicit = tls.filter(|t| t.implicit).map(|t| &t.tls);
    let mut c = SmtpClient::connect_with(host, port, timeout, implicit)?;
    c.hello("localhost")?;
    if let Some(t) = tls.filter(|t| !t.implicit && t.starttls != StartTls::Off) {
        if !c.starttls(&t.tls, "localhost")? && t.starttls == StartTls::Required {
            return Err(messaging_error(format!("STARTTLS is required but host {} does not support STARTTLS", host)));
        }
    }
    if let Some((u, p)) = auth {
        c.authenticate(u, p)?;
    }
    let r = c.send_mail(from, recipients, message);
    c.quit();
    r
}
