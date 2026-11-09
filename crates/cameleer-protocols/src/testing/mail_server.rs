//! Embedded SMTP, POP3 and IMAP test servers sharing an in-memory [`Mailbox`] (like
//! Camel's mock mailbox): mail sent with SMTP to `bob@localhost` can be read with POP3 or
//! IMAP by logging in as `bob@localhost` (or just `bob`).
//!
//! TLS: `start_tls(&mailbox, tls_config, implicit)` on each server serves implicit TLS
//! (`smtps`/`pop3s`/`imaps`) or plain connections that offer `STARTTLS` / `STLS` and
//! refuse authentication until TLS is negotiated.
//!
//! ```no_run
//! use cameleer_protocols::testing::{Mailbox, SmtpServer, Pop3Server};
//! let mailbox = Mailbox::new();
//! let smtp = SmtpServer::start(&mailbox).unwrap();
//! let pop3 = Pop3Server::start(&mailbox).unwrap();
//! let uri = format!("smtp://127.0.0.1:{}?to=bob@localhost", smtp.port());
//! # let _ = (uri, pop3);
//! ```

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

use cameleer_tls::net::NetStream;
use cameleer_tls::TlsServerConfig;
use std::time::Duration;

use cameleer_core::dataformat::base64;
use cameleer_core::error::Result;

use cameleer_tls::net::{read_line, TcpServer};
use crate::mail::mime::{dot_stuff, parse_address_list, read_dot_terminated};

/// A message stored in the mailbox.
#[derive(Clone, Debug)]
pub struct StoredMail {
    pub uid: u64,
    /// The raw message (CRLF line endings).
    pub raw: String,
    /// Envelope sender (`MAIL FROM`).
    pub envelope_from: String,
    pub seen: bool,
    pub deleted: bool,
}

#[derive(Default)]
struct MailboxState {
    boxes: HashMap<String, Vec<StoredMail>>,
    passwords: HashMap<String, String>,
    next_uid: u64,
    logins: Vec<(String, String)>,
}

/// An in-memory set of mailboxes keyed by (lower case) address.
#[derive(Clone, Default)]
pub struct Mailbox {
    inner: Arc<Mutex<MailboxState>>,
}

impl Mailbox {
    pub fn new() -> Mailbox {
        Mailbox::default()
    }

    /// Require `password` for logins as `user` (by default any password is accepted).
    pub fn set_password(&self, user: &str, password: &str) {
        self.inner.lock().unwrap().passwords.insert(user.to_ascii_lowercase(), password.to_string());
    }

    /// Store a raw message for `address`.
    pub fn deliver(&self, address: &str, raw: &str) {
        self.deliver_from(address, "", raw);
    }

    fn deliver_from(&self, address: &str, from: &str, raw: &str) {
        let mut s = self.inner.lock().unwrap();
        s.next_uid += 1;
        let uid = s.next_uid;
        s.boxes.entry(address.to_ascii_lowercase()).or_default().push(StoredMail {
            uid,
            raw: raw.to_string(),
            envelope_from: from.to_string(),
            seen: false,
            deleted: false,
        });
    }

    /// The messages in a mailbox.
    pub fn messages(&self, address: &str) -> Vec<StoredMail> {
        self.inner.lock().unwrap().boxes.get(&address.to_ascii_lowercase()).cloned().unwrap_or_default()
    }

    /// Number of messages in a mailbox.
    pub fn count(&self, address: &str) -> usize {
        self.messages(address).len()
    }

    /// Remove all messages.
    pub fn clear(&self) {
        self.inner.lock().unwrap().boxes.clear();
    }

    /// `(user, password)` of every successful SMTP AUTH / POP3 / IMAP login.
    pub fn logins(&self) -> Vec<(String, String)> {
        self.inner.lock().unwrap().logins.clone()
    }

    /// Resolve a login to a mailbox key, checking the password.
    fn login(&self, user: &str, password: &str) -> Option<String> {
        let mut s = self.inner.lock().unwrap();
        let u = user.to_ascii_lowercase();
        let key = if s.boxes.contains_key(&u) || s.passwords.contains_key(&u) || u.contains('@') {
            u.clone()
        } else {
            // a bare user name maps to its address; folders (`ann@localhost/Archive`) are not
            // mailboxes, and min() keeps the choice independent of HashMap order
            s.boxes.keys().filter(|k| !k.contains('/') && k.split('@').next() == Some(u.as_str())).min().cloned().unwrap_or(u.clone())
        };
        if let Some(p) = s.passwords.get(&key).or_else(|| s.passwords.get(&u)) {
            if p != password {
                return None;
            }
        }
        s.logins.push((user.to_string(), password.to_string()));
        Some(key)
    }

    fn with_box<T>(&self, key: &str, f: impl FnOnce(&mut Vec<StoredMail>) -> T) -> T {
        let mut s = self.inner.lock().unwrap();
        f(s.boxes.entry(key.to_string()).or_default())
    }
}

fn reply(out: &mut NetStream, line: &str) -> std::io::Result<()> {
    out.write_all(line.as_bytes())?;
    out.write_all(b"\r\n")?;
    out.flush()
}

fn session_streams(s: NetStream) -> std::io::Result<(BufReader<NetStream>, NetStream)> {
    s.set_read_timeout(Some(Duration::from_secs(60)))?;
    let out = s.try_clone()?;
    Ok((BufReader::new(s), out))
}

/// TLS mode of an embedded mail server.
#[derive(Clone)]
enum ServerTls {
    Plain,
    Implicit(Arc<TlsServerConfig>),
    StartTls(Arc<TlsServerConfig>),
}

impl ServerTls {
    fn new(cfg: Option<TlsServerConfig>, implicit: bool) -> ServerTls {
        match cfg {
            None => ServerTls::Plain,
            Some(c) if implicit => ServerTls::Implicit(Arc::new(c)),
            Some(c) => ServerTls::StartTls(Arc::new(c)),
        }
    }

    /// Wraps an accepted connection (handshake for implicit TLS).
    fn wrap(&self, s: TcpStream) -> std::io::Result<NetStream> {
        let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
        match self {
            ServerTls::Implicit(c) => NetStream::server(s, c),
            _ => Ok(NetStream::plain(s)),
        }
    }

    /// STARTTLS configuration (None when not offered).
    fn starttls(&self) -> Option<Arc<TlsServerConfig>> {
        match self {
            ServerTls::StartTls(c) => Some(c.clone()),
            _ => None,
        }
    }
}

/// Upgrades a session in place after a positive STARTTLS reply.
fn upgrade(r: &mut BufReader<NetStream>, out: &mut NetStream, cfg: &TlsServerConfig) -> std::io::Result<()> {
    let plain = out.try_clone()?;
    let secured = plain.start_tls_server(cfg)?;
    *r = BufReader::new(secured.try_clone()?);
    *out = secured;
    Ok(())
}

// ---------------------------------------------------------------------------
// SMTP
// ---------------------------------------------------------------------------

/// Embedded SMTP server delivering into a [`Mailbox`] (accepts any credentials unless
/// a password is set on the mailbox).
pub struct SmtpServer {
    server: TcpServer,
}

impl SmtpServer {
    pub fn start(mailbox: &Mailbox) -> Result<SmtpServer> {
        Self::start_inner(mailbox, ServerTls::Plain)
    }

    /// TLS server: implicit TLS when `implicit`, else plain with STARTTLS (required before login).
    pub fn start_tls(mailbox: &Mailbox, tls: TlsServerConfig, implicit: bool) -> Result<SmtpServer> {
        Self::start_inner(mailbox, ServerTls::new(Some(tls), implicit))
    }

    fn start_inner(mailbox: &Mailbox, tls: ServerTls) -> Result<SmtpServer> {
        let mb = mailbox.clone();
        let server = TcpServer::bind("127.0.0.1", 0, "smtp-server", move |s| {
            if let Ok(s) = tls.wrap(s) {
                let _ = smtp_session(&mb, s, tls.starttls());
            }
        })?;
        Ok(SmtpServer { server })
    }
    pub fn port(&self) -> u16 {
        self.server.port()
    }
    pub fn stop(&self) {
        self.server.stop();
    }
}

fn smtp_session(mb: &Mailbox, s: NetStream, starttls: Option<Arc<TlsServerConfig>>) -> std::io::Result<()> {
    let (mut r, mut out) = session_streams(s)?;
    reply(&mut out, "220 localhost Cameleer ESMTP test server")?;
    let mut from: Option<String> = None;
    let mut rcpts: Vec<String> = Vec::new();
    loop {
        let Some(line) = read_line(&mut r, 64 * 1024)? else { return Ok(()) };
        let upper = line.to_ascii_uppercase();
        let verb = upper.split_whitespace().next().unwrap_or("").to_string();
        match verb.as_str() {
            "EHLO" if starttls.is_some() && !out.is_tls() => reply(&mut out, "250-localhost\r\n250-STARTTLS\r\n250-8BITMIME\r\n250 SIZE 52428800")?,
            "EHLO" => reply(&mut out, "250-localhost\r\n250-AUTH PLAIN LOGIN\r\n250-8BITMIME\r\n250 SIZE 52428800")?,
            "STARTTLS" => match &starttls {
                Some(cfg) if !out.is_tls() => {
                    reply(&mut out, "220 Ready to start TLS")?;
                    upgrade(&mut r, &mut out, cfg)?;
                    from = None;
                    rcpts.clear();
                }
                _ => reply(&mut out, "502 STARTTLS not available")?,
            },
            "AUTH" | "MAIL" if starttls.is_some() && !out.is_tls() => reply(&mut out, "530 Must issue a STARTTLS command first")?,
            "HELO" => reply(&mut out, "250 localhost")?,
            "AUTH" => {
                let parts: Vec<&str> = line.split_whitespace().collect();
                let creds = match parts.get(1).map(|m| m.to_ascii_uppercase()).as_deref() {
                    Some("PLAIN") => {
                        let token = match parts.get(2) {
                            Some(t) => t.to_string(),
                            None => {
                                reply(&mut out, "334 ")?;
                                read_line(&mut r, 8192)?.unwrap_or_default()
                            }
                        };
                        base64::decode(&token).ok().and_then(|d| {
                            let s = String::from_utf8_lossy(&d).into_owned();
                            let f: Vec<&str> = s.split('\0').collect();
                            (f.len() == 3).then(|| (f[1].to_string(), f[2].to_string()))
                        })
                    }
                    Some("LOGIN") => {
                        reply(&mut out, "334 VXNlcm5hbWU6")?;
                        let u = read_line(&mut r, 8192)?.unwrap_or_default();
                        reply(&mut out, "334 UGFzc3dvcmQ6")?;
                        let p = read_line(&mut r, 8192)?.unwrap_or_default();
                        match (base64::decode(&u), base64::decode(&p)) {
                            (Ok(u), Ok(p)) => Some((String::from_utf8_lossy(&u).into_owned(), String::from_utf8_lossy(&p).into_owned())),
                            _ => None,
                        }
                    }
                    _ => {
                        reply(&mut out, "504 Unrecognized authentication type")?;
                        continue;
                    }
                };
                match creds.and_then(|(u, p)| mb.login(&u, &p)) {
                    Some(_) => reply(&mut out, "235 Authentication successful")?,
                    None => reply(&mut out, "535 Authentication credentials invalid")?,
                }
            }
            "MAIL" => {
                from = Some(parse_address_list(line.split_once(':').map(|x| x.1).unwrap_or("")).into_iter().next().unwrap_or_default());
                rcpts.clear();
                reply(&mut out, "250 OK")?;
            }
            "RCPT" => {
                if from.is_none() {
                    reply(&mut out, "503 Need MAIL command")?;
                    continue;
                }
                match parse_address_list(line.split_once(':').map(|x| x.1).unwrap_or("")).into_iter().next() {
                    Some(a) => {
                        rcpts.push(a);
                        reply(&mut out, "250 OK")?;
                    }
                    None => reply(&mut out, "501 Bad recipient")?,
                }
            }
            "DATA" => {
                if rcpts.is_empty() {
                    reply(&mut out, "503 Need RCPT command")?;
                    continue;
                }
                reply(&mut out, "354 End data with <CR><LF>.<CR><LF>")?;
                let data = read_dot_terminated(&mut r, 1024 * 1024)?;
                let f = from.take().unwrap_or_default();
                for rc in rcpts.drain(..) {
                    mb.deliver_from(&rc, &f, &data);
                }
                reply(&mut out, "250 OK message accepted")?;
            }
            "RSET" => {
                from = None;
                rcpts.clear();
                reply(&mut out, "250 OK")?;
            }
            "NOOP" => reply(&mut out, "250 OK")?,
            "QUIT" => {
                reply(&mut out, "221 Bye")?;
                return Ok(());
            }
            _ => reply(&mut out, "502 Command not implemented")?,
        }
    }
}

// ---------------------------------------------------------------------------
// POP3
// ---------------------------------------------------------------------------

/// Embedded POP3 server reading a [`Mailbox`].
pub struct Pop3Server {
    server: TcpServer,
}

impl Pop3Server {
    pub fn start(mailbox: &Mailbox) -> Result<Pop3Server> {
        Self::start_inner(mailbox, ServerTls::Plain)
    }

    /// TLS server: implicit TLS when `implicit`, else plain with STARTTLS (required before login).
    pub fn start_tls(mailbox: &Mailbox, tls: TlsServerConfig, implicit: bool) -> Result<Pop3Server> {
        Self::start_inner(mailbox, ServerTls::new(Some(tls), implicit))
    }

    fn start_inner(mailbox: &Mailbox, tls: ServerTls) -> Result<Pop3Server> {
        let mb = mailbox.clone();
        let server = TcpServer::bind("127.0.0.1", 0, "pop3-server", move |s| {
            if let Ok(s) = tls.wrap(s) {
                let _ = pop3_session(&mb, s, tls.starttls());
            }
        })?;
        Ok(Pop3Server { server })
    }
    pub fn port(&self) -> u16 {
        self.server.port()
    }
    pub fn stop(&self) {
        self.server.stop();
    }
}

fn pop3_session(mb: &Mailbox, s: NetStream, starttls: Option<Arc<TlsServerConfig>>) -> std::io::Result<()> {
    let (mut r, mut out) = session_streams(s)?;
    reply(&mut out, "+OK Cameleer POP3 test server ready")?;
    let mut user: Option<String> = None;
    let mut key: Option<String> = None;
    // snapshot of the maildrop at login: (uid, raw); deletions are marked locally
    let mut drop: Vec<(u64, String)> = Vec::new();
    let mut deleted: Vec<bool> = Vec::new();
    loop {
        let Some(line) = read_line(&mut r, 8192)? else { return Ok(()) };
        let mut parts = line.splitn(2, ' ');
        let verb = parts.next().unwrap_or("").to_ascii_uppercase();
        let arg = parts.next().unwrap_or("").trim().to_string();
        if verb == "STLS" {
            match &starttls {
                Some(cfg) if !out.is_tls() => {
                    reply(&mut out, "+OK Begin TLS negotiation")?;
                    upgrade(&mut r, &mut out, cfg)?;
                }
                _ => reply(&mut out, "-ERR STLS not available")?,
            }
            continue;
        }
        if starttls.is_some() && !out.is_tls() && matches!(verb.as_str(), "USER" | "PASS") {
            reply(&mut out, "-ERR TLS required (use STLS)")?;
            continue;
        }
        if key.is_none() && !matches!(verb.as_str(), "USER" | "PASS" | "QUIT" | "CAPA") {
            reply(&mut out, "-ERR not authenticated")?;
            continue;
        }
        let msg = |n: &str, deleted: &Vec<bool>| -> Option<usize> {
            let i = n.parse::<usize>().ok()?.checked_sub(1)?;
            (i < deleted.len() && !deleted[i]).then_some(i)
        };
        match verb.as_str() {
            "CAPA" if starttls.is_some() && !out.is_tls() => reply(&mut out, "+OK\r\nSTLS\r\nUIDL\r\nTOP\r\n.")?,
            "CAPA" => reply(&mut out, "+OK\r\nUSER\r\nUIDL\r\nTOP\r\n.")?,
            "USER" => {
                user = Some(arg);
                reply(&mut out, "+OK send PASS")?;
            }
            "PASS" => match user.as_deref().and_then(|u| mb.login(u, &arg)) {
                Some(k) => {
                    drop = mb.with_box(&k, |b| b.iter().filter(|m| !m.deleted).map(|m| (m.uid, m.raw.clone())).collect());
                    deleted = vec![false; drop.len()];
                    key = Some(k);
                    reply(&mut out, &format!("+OK maildrop has {} messages", drop.len()))?;
                }
                None => reply(&mut out, "-ERR invalid credentials")?,
            },
            "STAT" => {
                let (n, size) = drop.iter().zip(&deleted).filter(|(_, d)| !**d).fold((0, 0), |(n, s), ((_, m), _)| (n + 1, s + m.len()));
                reply(&mut out, &format!("+OK {} {}", n, size))?
            }
            "LIST" | "UIDL" => {
                let val = |i: usize| if verb == "LIST" { drop[i].1.len().to_string() } else { format!("uid-{}", drop[i].0) };
                if arg.is_empty() {
                    let mut text = String::from("+OK\r\n");
                    for i in (0..drop.len()).filter(|&i| !deleted[i]) {
                        text.push_str(&format!("{} {}\r\n", i + 1, val(i)));
                    }
                    text.push('.');
                    reply(&mut out, &text)?;
                } else {
                    match msg(&arg, &deleted) {
                        Some(i) => reply(&mut out, &format!("+OK {} {}", i + 1, val(i)))?,
                        None => reply(&mut out, "-ERR no such message")?,
                    }
                }
            }
            "RETR" | "TOP" => {
                let n = arg.split_whitespace().next().unwrap_or("");
                match msg(n, &deleted) {
                    Some(i) => {
                        let mut raw = drop[i].1.clone();
                        if verb == "TOP" {
                            let lines: usize = arg.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
                            if let Some(p) = raw.find("\r\n\r\n") {
                                let body: Vec<&str> = raw[p + 4..].split("\r\n").take(lines).collect();
                                raw = format!("{}\r\n\r\n{}", &raw[..p], body.join("\r\n"));
                            }
                        }
                        let mut text = format!("+OK {} octets\r\n{}", raw.len(), dot_stuff(&raw));
                        if !text.ends_with("\r\n") {
                            text.push_str("\r\n");
                        }
                        text.push('.');
                        reply(&mut out, &text)?;
                    }
                    None => reply(&mut out, "-ERR no such message")?,
                }
            }
            "DELE" => match msg(&arg, &deleted) {
                Some(i) => {
                    deleted[i] = true;
                    reply(&mut out, &format!("+OK message {} deleted", i + 1))?;
                }
                None => reply(&mut out, "-ERR no such message")?,
            },
            "RSET" => {
                deleted.iter_mut().for_each(|d| *d = false);
                reply(&mut out, "+OK")?;
            }
            "NOOP" => reply(&mut out, "+OK")?,
            "QUIT" => {
                if let Some(k) = &key {
                    let gone: Vec<u64> = drop.iter().zip(&deleted).filter(|(_, d)| **d).map(|((u, _), _)| *u).collect();
                    mb.with_box(k, |b| b.retain(|m| !gone.contains(&m.uid)));
                }
                reply(&mut out, "+OK bye")?;
                return Ok(());
            }
            _ => reply(&mut out, "-ERR unknown command")?,
        }
    }
}

// ---------------------------------------------------------------------------
// IMAP
// ---------------------------------------------------------------------------

/// Embedded IMAP4rev1 server (subset) over a [`Mailbox`]. Only `INBOX` holds the
/// mailbox's messages; `COPY` to another folder stores into `<address>/<folder>`.
pub struct ImapServer {
    server: TcpServer,
}

impl ImapServer {
    pub fn start(mailbox: &Mailbox) -> Result<ImapServer> {
        Self::start_inner(mailbox, ServerTls::Plain)
    }

    /// TLS server: implicit TLS when `implicit`, else plain with STARTTLS (required before login).
    pub fn start_tls(mailbox: &Mailbox, tls: TlsServerConfig, implicit: bool) -> Result<ImapServer> {
        Self::start_inner(mailbox, ServerTls::new(Some(tls), implicit))
    }

    fn start_inner(mailbox: &Mailbox, tls: ServerTls) -> Result<ImapServer> {
        let mb = mailbox.clone();
        let server = TcpServer::bind("127.0.0.1", 0, "imap-server", move |s| {
            if let Ok(s) = tls.wrap(s) {
                let _ = imap_session(&mb, s, tls.starttls());
            }
        })?;
        Ok(ImapServer { server })
    }
    pub fn port(&self) -> u16 {
        self.server.port()
    }
    pub fn stop(&self) {
        self.server.stop();
    }
}

/// Split IMAP command arguments, honouring quoted strings.
fn imap_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut escape, mut has) = (false, false, false);
    for c in s.chars() {
        if escape {
            cur.push(c);
            escape = false;
        } else if quoted && c == '\\' {
            escape = true;
        } else if c == '"' {
            quoted = !quoted;
            has = true;
        } else if c == ' ' && !quoted {
            if has || !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                has = false;
            }
        } else {
            cur.push(c);
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn imap_session(mb: &Mailbox, s: NetStream, starttls: Option<Arc<TlsServerConfig>>) -> std::io::Result<()> {
    let (mut r, mut out) = session_streams(s)?;
    reply(&mut out, "* OK [CAPABILITY IMAP4rev1] Cameleer IMAP test server ready")?;
    let mut key: Option<String> = None;
    let mut folder: Option<String> = None;
    loop {
        let Some(line) = read_line(&mut r, 64 * 1024)? else { return Ok(()) };
        let args = imap_args(&line);
        if args.len() < 2 {
            reply(&mut out, "* BAD missing command")?;
            continue;
        }
        let tag = args[0].clone();
        let mut cmd = args[1].to_ascii_uppercase();
        let mut rest: Vec<String> = args[2..].to_vec();
        if cmd == "UID" && !rest.is_empty() {
            cmd = rest.remove(0).to_ascii_uppercase();
        }
        if cmd == "STARTTLS" {
            match &starttls {
                Some(cfg) if !out.is_tls() => {
                    reply(&mut out, &format!("{} OK Begin TLS negotiation now", tag))?;
                    upgrade(&mut r, &mut out, cfg)?;
                }
                _ => reply(&mut out, &format!("{} BAD STARTTLS not available", tag))?,
            }
            continue;
        }
        if starttls.is_some() && !out.is_tls() && cmd == "LOGIN" {
            reply(&mut out, &format!("{} NO [PRIVACYREQUIRED] use STARTTLS first", tag))?;
            continue;
        }
        if key.is_none() && !matches!(cmd.as_str(), "LOGIN" | "CAPABILITY" | "LOGOUT" | "NOOP") {
            reply(&mut out, &format!("{} NO not authenticated", tag))?;
            continue;
        }
        let box_key = |k: &str, f: &str| if f.eq_ignore_ascii_case("INBOX") { k.to_string() } else { format!("{}/{}", k, f) };
        match cmd.as_str() {
            "CAPABILITY" if starttls.is_some() && !out.is_tls() => {
                reply(&mut out, &format!("* CAPABILITY IMAP4rev1 STARTTLS LOGINDISABLED\r\n{} OK CAPABILITY completed", tag))?
            }
            "CAPABILITY" => reply(&mut out, &format!("* CAPABILITY IMAP4rev1 AUTH=PLAIN\r\n{} OK CAPABILITY completed", tag))?,
            "NOOP" => reply(&mut out, &format!("{} OK NOOP completed", tag))?,
            "LOGIN" => match (rest.first(), rest.get(1)) {
                (Some(u), Some(p)) => match mb.login(u, p) {
                    Some(k) => {
                        key = Some(k);
                        reply(&mut out, &format!("{} OK LOGIN completed", tag))?;
                    }
                    None => reply(&mut out, &format!("{} NO [AUTHENTICATIONFAILED] invalid credentials", tag))?,
                },
                _ => reply(&mut out, &format!("{} BAD LOGIN needs user and password", tag))?,
            },
            "SELECT" | "EXAMINE" => {
                let f = rest.first().cloned().unwrap_or_else(|| "INBOX".into());
                let k = box_key(key.as_deref().unwrap_or(""), &f);
                let (exists, unseen) = mb.with_box(&k, |b| (b.len(), b.iter().filter(|m| !m.seen).count()));
                folder = Some(k);
                reply(
                    &mut out,
                    &format!(
                        "* {} EXISTS\r\n* 0 RECENT\r\n* OK [UNSEEN {}]\r\n* FLAGS (\\Seen \\Deleted)\r\n{} OK [READ-WRITE] {} completed",
                        exists, unseen, tag, cmd
                    ),
                )?;
            }
            "SEARCH" | "FETCH" | "STORE" | "COPY" | "EXPUNGE" | "CLOSE" if folder.is_none() => {
                reply(&mut out, &format!("{} NO no folder selected", tag))?;
            }
            "SEARCH" => {
                let crit: Vec<String> = rest.iter().map(|c| c.to_ascii_uppercase()).collect();
                let ids = mb.with_box(folder.as_deref().unwrap(), |b| {
                    b.iter()
                        .enumerate()
                        .filter(|(_, m)| {
                            crit.iter().all(|c| match c.as_str() {
                                "UNSEEN" => !m.seen,
                                "SEEN" => m.seen,
                                "DELETED" => m.deleted,
                                "UNDELETED" => !m.deleted,
                                _ => true,
                            })
                        })
                        .map(|(i, _)| (i + 1).to_string())
                        .collect::<Vec<_>>()
                });
                let sep = if ids.is_empty() { "" } else { " " };
                reply(&mut out, &format!("* SEARCH{}{}\r\n{} OK SEARCH completed", sep, ids.join(" "), tag))?;
            }
            "FETCH" => {
                let n: usize = rest.first().and_then(|x| x.parse().ok()).unwrap_or(0);
                let what = rest[1.min(rest.len())..].join(" ").to_ascii_uppercase();
                let peek = what.contains("PEEK");
                let found = mb.with_box(folder.as_deref().unwrap(), |b| {
                    let m = b.get_mut(n.wrapping_sub(1))?;
                    if !peek && (what.contains("BODY[]") || what.contains("RFC822")) {
                        m.seen = true;
                    }
                    Some((m.raw.clone(), m.seen, m.deleted))
                });
                match found {
                    Some((raw, seen, deleted)) => {
                        let mut flags = Vec::new();
                        if seen {
                            flags.push("\\Seen");
                        }
                        if deleted {
                            flags.push("\\Deleted");
                        }
                        if what.contains("BODY") || what.contains("RFC822") {
                            let item = if what.contains("RFC822") { "RFC822" } else { "BODY[]" };
                            out.write_all(format!("* {} FETCH ({} {{{}}}\r\n", n, item, raw.len()).as_bytes())?;
                            out.write_all(raw.as_bytes())?;
                            reply(&mut out, &format!(")\r\n{} OK FETCH completed", tag))?;
                        } else {
                            reply(&mut out, &format!("* {} FETCH (FLAGS ({}))\r\n{} OK FETCH completed", n, flags.join(" "), tag))?;
                        }
                    }
                    None => reply(&mut out, &format!("{} NO no such message", tag))?,
                }
            }
            "STORE" => {
                let n: usize = rest.first().and_then(|x| x.parse().ok()).unwrap_or(0);
                let op = rest.get(1).map(|s| s.to_ascii_uppercase()).unwrap_or_default();
                let flags = rest[2.min(rest.len())..].join(" ").to_ascii_uppercase();
                let ok = mb.with_box(folder.as_deref().unwrap(), |b| {
                    let Some(m) = b.get_mut(n.wrapping_sub(1)) else { return false };
                    let set = !op.starts_with('-');
                    if flags.contains("\\SEEN") {
                        m.seen = set;
                    }
                    if flags.contains("\\DELETED") {
                        m.deleted = set;
                    }
                    true
                });
                if ok {
                    reply(&mut out, &format!("{} OK STORE completed", tag))?;
                } else {
                    reply(&mut out, &format!("{} NO no such message", tag))?;
                }
            }
            "COPY" => {
                let n: usize = rest.first().and_then(|x| x.parse().ok()).unwrap_or(0);
                let target = box_key(key.as_deref().unwrap(), rest.get(1).map(|s| s.as_str()).unwrap_or("INBOX"));
                let raw = mb.with_box(folder.as_deref().unwrap(), |b| b.get(n.wrapping_sub(1)).map(|m| m.raw.clone()));
                match raw {
                    Some(raw) => {
                        mb.deliver(&target, &raw);
                        reply(&mut out, &format!("{} OK COPY completed", tag))?;
                    }
                    None => reply(&mut out, &format!("{} NO no such message", tag))?,
                }
            }
            "EXPUNGE" | "CLOSE" => {
                let removed: Vec<usize> = mb.with_box(folder.as_deref().unwrap(), |b| {
                    let idx: Vec<usize> = b.iter().enumerate().filter(|(_, m)| m.deleted).map(|(i, _)| i + 1).collect();
                    b.retain(|m| !m.deleted);
                    idx
                });
                let mut text = String::new();
                if cmd == "EXPUNGE" {
                    // sequence numbers shift down after each expunge
                    for (k, i) in removed.iter().enumerate() {
                        text.push_str(&format!("* {} EXPUNGE\r\n", i - k));
                    }
                } else {
                    folder = None;
                }
                reply(&mut out, &format!("{}{} OK {} completed", text, tag, cmd))?;
            }
            "LOGOUT" => {
                reply(&mut out, &format!("* BYE logging out\r\n{} OK LOGOUT completed", tag))?;
                return Ok(());
            }
            _ => reply(&mut out, &format!("{} BAD unknown command", tag))?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_user_login_never_resolves_to_a_folder() {
        // each Mailbox has a freshly seeded HashMap, so 50 of them cover the iteration orders
        for _ in 0..50 {
            let mb = Mailbox::new();
            mb.deliver("ann@localhost", "Subject: a\r\n\r\nx\r\n");
            // moveTo / copyTo folders of the same user
            mb.deliver("ann@localhost/Archive", "Subject: b\r\n\r\ny\r\n");
            mb.deliver("ann@localhost/archive/archive", "Subject: c\r\n\r\nz\r\n");
            assert_eq!(mb.login("ann", "pw").as_deref(), Some("ann@localhost"));
            assert_eq!(mb.login("ann@localhost", "pw").as_deref(), Some("ann@localhost"));
        }
    }
}
