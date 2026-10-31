//! Mail components (Camel `camel-mail`): `smtp`/`smtps` producer, `pop3`/`pop3s` and
//! `imap`/`imaps` polling consumers.
//!
//! **TLS**: `smtps` (port 465), `pop3s` (995) and `imaps` (993) use implicit TLS.
//! STARTTLS on the plain schemes is enabled with the JavaMail-style options
//! `mail.smtp.starttls.enable=true` / `mail.pop3.starttls.enable=true` /
//! `mail.imap.starttls.enable=true` (upgrade when offered) and
//! `mail.<protocol>.starttls.required=true` (fail when not offered). Trust material:
//! `sslContextParameters=#bean`, `useGlobalSslContextParameters=true`, the global
//! `camel.ssl.*` parameters, else the system trust store.
//!
//! **SMTP producer** `smtp://[username@]host[:port][?options]` (port 25): options
//! `username`, `password` (authenticates with `AUTH PLAIN` or `AUTH LOGIN`), `from`
//! (default `camel@localhost`), `to` (default `username@host`), `cc`, `bcc`, `replyTo`,
//! `subject`, `contentType` (default `text/plain`), `connectionTimeout` (30s).
//! Message headers `To`, `From`, `Cc`, `Bcc`, `Reply-To`, `Subject` and `Content-Type`
//! override the options; other headers with simple values (except `Camel*` ones) are
//! copied to the mail. Attachments are taken from the exchange property
//! `CamelAttachments` (an object `file name -> content`) and sent as `multipart/mixed`.
//!
//! **POP3 / IMAP consumers** `pop3://[username@]host[:port]` (110) and
//! `imap://[username@]host[:port]` (143), scheduled polling (`delay` default 60s,
//! `initialDelay` 1s and all scheduler options): `username`, `password`, `delete`
//! (false), `unseen` (true: only new messages; IMAP uses the `\Seen` flag, POP3 tracks
//! `UIDL`s in memory), `maxMessagesPerPoll` / `fetchSize`, `folderName` (IMAP, `INBOX`),
//! `copyTo` / `moveTo` (IMAP), `mapMailMessage` (true: body is the text content, false:
//! the raw message), `connectionTimeout`. Each mail header becomes a message header
//! (repeated headers become a list); the body is the first `text/plain` part; other
//! MIME parts are put in the `CamelAttachments` exchange property as bytes.

pub mod imap;
pub mod mime;
pub mod pop3;
pub mod smtp;

use std::any::Any;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::scheduled::{ScheduledPollConfig, ScheduledPollConsumer};
use camel_core::component::support::process_consumer_exchange;
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

use crate::common::{take_ignored, Authority, COMMON_IGNORED};
use camel_tls::camel_support::{endpoint_tls, EndpointTls};

/// STARTTLS policy of a plain mail endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartTls {
    Off,
    /// Upgrade when the server offers it.
    Enable,
    /// Fail when the server does not offer it.
    Required,
}

/// TLS settings of a mail endpoint.
#[derive(Clone, Debug)]
pub struct MailTls {
    pub tls: EndpointTls,
    /// Implicit TLS (`smtps`, `pop3s`, `imaps`).
    pub implicit: bool,
    pub starttls: StartTls,
}
use mime::{build_message, parse_address_list, parse_message, Attachment};

/// Exception kind for mail failures (JavaMail `MessagingException`).
pub const MESSAGING_EXCEPTION: &str = "MessagingException";
/// Exception kind for authentication failures.
pub const AUTHENTICATION_FAILED: &str = "AuthenticationFailedException";
/// Exchange property holding attachments (`name -> bytes`).
pub const ATTACHMENTS: &str = "CamelAttachments";

pub(crate) fn messaging_error(msg: impl Into<String>) -> CamelError {
    CamelError::new(MESSAGING_EXCEPTION, msg)
}

pub(crate) fn auth_error(msg: impl Into<String>) -> CamelError {
    CamelError::new(AUTHENTICATION_FAILED, msg)
}

/// Mail protocol of an endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailProtocol {
    Smtp,
    Pop3,
    Imap,
}

impl MailProtocol {
    pub fn from_scheme(s: &str) -> Option<MailProtocol> {
        match s {
            "smtp" => Some(MailProtocol::Smtp),
            "pop3" => Some(MailProtocol::Pop3),
            "imap" => Some(MailProtocol::Imap),
            _ => None,
        }
    }
    pub fn default_port(&self) -> u16 {
        match self {
            MailProtocol::Smtp => 25,
            MailProtocol::Pop3 => 110,
            MailProtocol::Imap => 143,
        }
    }
    /// Default port with implicit TLS (`smtps` 465, `pop3s` 995, `imaps` 993).
    pub fn default_secure_port(&self) -> u16 {
        match self {
            MailProtocol::Smtp => 465,
            MailProtocol::Pop3 => 995,
            MailProtocol::Imap => 993,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            MailProtocol::Smtp => "smtp",
            MailProtocol::Pop3 => "pop3",
            MailProtocol::Imap => "imap",
        }
    }
}

/// Configuration of a mail endpoint.
#[derive(Clone, Debug)]
pub struct MailConfig {
    pub protocol: MailProtocol,
    /// TLS settings (implicit TLS or STARTTLS), None for plain connections.
    pub tls: Option<MailTls>,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: String,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub bcc: Option<String>,
    pub reply_to: Option<String>,
    pub subject: Option<String>,
    pub content_type: String,
    pub connection_timeout: Duration,
    pub delete: bool,
    pub unseen: bool,
    pub folder_name: String,
    pub max_messages_per_poll: usize,
    pub copy_to: Option<String>,
    pub move_to: Option<String>,
    pub map_mail_message: bool,
    pub scheduler: ScheduledPollConfig,
}

impl MailConfig {
    pub fn parse(protocol: MailProtocol, remaining: &str, params: &mut Parameters) -> Result<MailConfig> {
        MailConfig::parse_with(protocol, remaining, params, None)
    }

    /// Parse with TLS settings resolved by the component (`ctx` + secure flag).
    pub fn parse_with(protocol: MailProtocol, remaining: &str, params: &mut Parameters, tls: Option<(&CamelContext, &str, bool)>) -> Result<MailConfig> {
        let mut mail_tls = None;
        if let Some((ctx, scheme, secure)) = tls {
            let prefix = format!("mail.{}.", protocol.name());
            let flag = |params: &mut Parameters, key: &str| -> Result<bool> {
                Ok(match params.take(&format!("{}{}", prefix, key)).or_else(|| params.take(&format!("mail.{}s.{}", protocol.name(), key))) {
                    Some(v) => v.trim().eq_ignore_ascii_case("true"),
                    None => false,
                })
            };
            let required = flag(params, "starttls.required")?;
            let enable = flag(params, "starttls.enable")? || required;
            let ssl_enable = flag(params, "ssl.enable")?;
            let implicit = secure || ssl_enable;
            let starttls = if implicit {
                StartTls::Off
            } else if required {
                StartTls::Required
            } else if enable {
                StartTls::Enable
            } else {
                StartTls::Off
            };
            if let Some(t) = endpoint_tls(ctx, params, scheme, implicit || starttls != StartTls::Off, &[])? {
                mail_tls = Some(MailTls { tls: t, implicit, starttls: if implicit || starttls != StartTls::Off { starttls } else { StartTls::Enable } });
            }
        }
        let default_port = if mail_tls.as_ref().map(|t| t.implicit).unwrap_or(false) { protocol.default_secure_port() } else { protocol.default_port() };
        let a = Authority::parse(remaining, default_port)?;
        let fetch_size = params.take_i64("fetchSize", -1)?;
        let max = params.take_i64("maxMessagesPerPoll", 0)?;
        let max = if max > 0 { max as usize } else if fetch_size > 0 { fetch_size as usize } else { 0 };
        let scheduler = ScheduledPollConfig::from_params(params, Duration::from_millis(1000), Duration::from_millis(60_000))?;
        let cfg = MailConfig {
            protocol,
            tls: mail_tls,
            host: if a.host == "0.0.0.0" { "localhost".to_string() } else { a.host },
            port: a.port,
            username: params.take("username").or(a.user),
            password: params.take("password").or(a.password),
            from: params.take_string("from", "camel@localhost"),
            to: params.take("to"),
            cc: params.take("cc"),
            bcc: params.take("bcc"),
            reply_to: params.take("replyTo"),
            subject: params.take("subject"),
            content_type: params.take_string("contentType", "text/plain"),
            connection_timeout: params.take_duration("connectionTimeout", Duration::from_secs(30))?,
            delete: params.take_bool("delete", false)?,
            unseen: params.take_bool("unseen", true)?,
            folder_name: params.take_string("folderName", "INBOX"),
            max_messages_per_poll: max,
            copy_to: params.take("copyTo").filter(|s| !s.is_empty()),
            move_to: params.take("moveTo").filter(|s| !s.is_empty()),
            map_mail_message: params.take_bool("mapMailMessage", true)?,
            scheduler,
        };
        if protocol == MailProtocol::Pop3 && (cfg.copy_to.is_some() || cfg.move_to.is_some()) {
            return Err(CamelError::illegal_argument("copyTo/moveTo are only supported by the imap protocol"));
        }
        take_ignored(params, COMMON_IGNORED);
        take_ignored(
            params,
            &[
                "debugMode",
                "peek",
                "closeFolder",
                "disconnect",
                "handleFailedMessage",
                "skipFailedMessage",
                "failOnDuplicateFileAttachment",
                "generateMissingAttachmentNames",
                "handleDuplicateAttachmentNames",
                "ignoreUnsupportedCharset",
                "ignoreUriScheme",
                "useInlineAttachments",
                "mailUidGenerator",
                "postProcessAction",
                "searchTerm",
                "decodeFilename",
                "useFixedDelay",
                "javaMailSender",
                "javaMailProperties",
                "session",
                "attachmentsContentTransferEncodingResolver",
                "authenticator",
                "binding",
                "contentTypeResolver",
                "headerFilterStrategy",
                "additionalJavaMailProperties",
                "alternativeBodyHeader",
            ],
        );
        params.take_prefixed("mail.");
        params.take_prefixed("searchTerm.");
        Ok(cfg)
    }
}

/// The mail component for `smtp`, `pop3` and `imap`.
pub struct MailComponent {
    protocol: MailProtocol,
    secure: bool,
}

impl MailComponent {
    pub fn new(protocol: MailProtocol) -> Self {
        MailComponent { protocol, secure: false }
    }
    /// The implicit-TLS variant (`smtps`, `pop3s`, `imaps`).
    pub fn secure(protocol: MailProtocol) -> Self {
        MailComponent { protocol, secure: true }
    }
}

impl Component for MailComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let scheme = format!("{}{}", self.protocol.name(), if self.secure { "s" } else { "" });
        let cfg = MailConfig::parse_with(self.protocol, remaining, params, Some((ctx, &scheme, self.secure)))?;
        Ok(Arc::new(MailEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg, seen: Arc::new(Mutex::new(HashSet::new())) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A mail endpoint.
pub struct MailEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: MailConfig,
    seen: Arc<Mutex<HashSet<String>>>,
}

impl MailEndpoint {
    pub fn config(&self) -> &MailConfig {
        &self.cfg
    }
}

impl Endpoint for MailEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        if self.cfg.protocol != MailProtocol::Smtp {
            return Err(CamelError::new(
                camel_core::error::kinds::UNSUPPORTED_OPERATION,
                format!("Cannot send mail with protocol {:?}; use smtp: {}", self.cfg.protocol, self.uri),
            ));
        }
        Ok(Arc::new(SmtpProducer { cfg: self.cfg.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        if self.cfg.protocol == MailProtocol::Smtp {
            return Err(CamelError::new(
                camel_core::error::kinds::UNSUPPORTED_OPERATION,
                format!("Cannot consume mail with smtp; use pop3 or imap: {}", self.uri),
            ));
        }
        let core = Arc::new(MailConsumerCore { cfg: self.cfg.clone(), ctx: self.ctx.clone(), seen: self.seen.clone(), logger: self.uri.clone() });
        let poll = ScheduledPollConsumer::new(self.uri.clone(), self.cfg.scheduler.clone(), move || core.poll(processor.as_ref()));
        Ok(Arc::new(MailConsumer { poll }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// producer
// ---------------------------------------------------------------------------

const MAPPED_HEADERS: &[&str] = &["to", "from", "cc", "bcc", "subject", "reply-to", "content-type", "content-transfer-encoding", "mime-version"];

struct SmtpProducer {
    cfg: MailConfig,
}

impl Service for SmtpProducer {}

fn header_or(ex: &Exchange, name: &str, default: &Option<String>) -> Option<String> {
    match ex.header(name) {
        Some(Value::Array(a)) => Some(a.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")),
        Some(v) if !v.is_null() => Some(v.to_string()),
        _ => default.clone(),
    }
    .filter(|s| !s.trim().is_empty())
}

impl Processor for SmtpProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let c = &self.cfg;
        let from = header_or(ex, "From", &Some(c.from.clone())).unwrap_or_else(|| c.from.clone());
        let default_to = c.to.clone().or_else(|| c.username.as_ref().map(|u| if u.contains('@') { u.clone() } else { format!("{}@{}", u, c.host) }));
        let to = header_or(ex, "To", &default_to);
        let cc = header_or(ex, "Cc", &c.cc);
        let bcc = header_or(ex, "Bcc", &c.bcc);
        let reply_to = header_or(ex, "Reply-To", &c.reply_to);
        let subject = header_or(ex, "Subject", &c.subject);
        let content_type = header_or(ex, "Content-Type", &Some(c.content_type.clone())).unwrap_or_else(|| c.content_type.clone());

        let mut headers: Vec<(String, String)> = vec![("From".into(), from.clone())];
        if let Some(t) = &to {
            headers.push(("To".into(), t.clone()));
        }
        if let Some(t) = &cc {
            headers.push(("Cc".into(), t.clone()));
        }
        if let Some(t) = &reply_to {
            headers.push(("Reply-To".into(), t.clone()));
        }
        headers.push(("Subject".into(), subject.unwrap_or_default()));
        for (k, v) in ex.message.headers.iter() {
            let lower = k.to_ascii_lowercase();
            if MAPPED_HEADERS.contains(&lower.as_str())
                || lower.starts_with("camel")
                || lower.starts_with("org.apache.camel")
                || lower == "breadcrumbid"
                || k.is_empty()
                || !k.bytes().all(|b| b.is_ascii_graphic() && b != b':')
            {
                continue;
            }
            let value = match v {
                Value::String(s) => s.clone(),
                Value::Int(_) | Value::Float(_) | Value::Bool(_) => v.to_string(),
                _ => continue,
            };
            headers.push((k.to_string(), value.replace(['\r', '\n'], " ")));
        }
        let attachments: Vec<Attachment> = match ex.property(ATTACHMENTS) {
            Some(Value::Object(m)) => m
                .iter()
                .map(|(name, v)| Attachment {
                    name: name.clone(),
                    content_type: if matches!(v, Value::Bytes(_)) { "application/octet-stream".into() } else { "text/plain".into() },
                    data: v.to_bytes(),
                })
                .collect(),
            _ => Vec::new(),
        };
        let body = match ex.body() {
            Value::Null => Vec::new(),
            v => v.to_bytes(),
        };
        let message = build_message(&headers, &body, &content_type, &attachments);
        let mut recipients = Vec::new();
        for list in [&to, &cc, &bcc].into_iter().flatten() {
            recipients.extend(parse_address_list(list));
        }
        let envelope_from = parse_address_list(&from).into_iter().next().unwrap_or(from);
        let auth = match (&c.username, &c.password) {
            (Some(u), Some(p)) => Some((u.as_str(), p.as_str())),
            _ => None,
        };
        smtp::send_with(&c.host, c.port, c.connection_timeout, c.tls.as_ref(), auth, &envelope_from, &recipients, &message)
    }
}

// ---------------------------------------------------------------------------
// consumers
// ---------------------------------------------------------------------------

/// Turn a raw mail into an exchange (headers, body, attachments).
pub fn mail_to_exchange(ex: &mut Exchange, raw: &[u8], map_mail_message: bool) {
    let msg = parse_message(raw);
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for (k, v) in &msg.headers {
        match grouped.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(k)) {
            Some((_, vals)) => vals.push(v.clone()),
            None => grouped.push((k.clone(), vec![v.clone()])),
        }
    }
    for (k, mut vals) in grouped {
        let v = if vals.len() == 1 { Value::String(vals.remove(0)) } else { Value::Array(vals.into_iter().map(Value::String).collect()) };
        ex.set_header(k, v);
    }
    if map_mail_message {
        ex.set_body(msg.body);
    } else {
        ex.set_body(String::from_utf8_lossy(raw).into_owned());
    }
    if !msg.attachments.is_empty() {
        let m: BTreeMap<String, Value> = msg.attachments.into_iter().map(|a| (a.name, Value::Bytes(a.data))).collect();
        ex.set_property(ATTACHMENTS, Value::Object(m));
    }
}

struct MailConsumerCore {
    cfg: MailConfig,
    ctx: WeakContext,
    seen: Arc<Mutex<HashSet<String>>>,
    logger: String,
}

impl MailConsumerCore {
    fn credentials(&self) -> (String, String) {
        (self.cfg.username.clone().unwrap_or_default(), self.cfg.password.clone().unwrap_or_default())
    }

    fn poll(&self, processor: &dyn Processor) -> Result<usize> {
        match self.cfg.protocol {
            MailProtocol::Pop3 => self.poll_pop3(processor),
            MailProtocol::Imap => self.poll_imap(processor),
            MailProtocol::Smtp => Ok(0),
        }
    }

    fn route(&self, processor: &dyn Processor, raw: &[u8]) -> bool {
        let mut ex = Exchange::new(self.ctx.upgrade());
        mail_to_exchange(&mut ex, raw, self.cfg.map_mail_message);
        process_consumer_exchange(&self.logger, processor, &mut ex) && !ex.rollback_only
    }

    fn poll_pop3(&self, processor: &dyn Processor) -> Result<usize> {
        let c = &self.cfg;
        let mut client = pop3::Pop3Client::connect_with(&c.host, c.port, c.connection_timeout, c.tls.as_ref())?;
        let (u, p) = self.credentials();
        client.login(&u, &p)?;
        let ids: Vec<(usize, Option<String>)> = match client.uidl()? {
            Some(list) => list.into_iter().map(|(n, id)| (n, Some(id))).collect(),
            None => client.list()?.into_iter().map(|(n, _)| (n, None)).collect(),
        };
        let mut n = 0;
        for (num, uid) in ids {
            if c.max_messages_per_poll > 0 && n >= c.max_messages_per_poll {
                break;
            }
            if c.unseen && !c.delete {
                if let Some(id) = &uid {
                    if self.seen.lock().unwrap().contains(id) {
                        continue;
                    }
                }
            }
            let raw = client.retr(num)?;
            let id = uid.unwrap_or_else(|| parse_message(raw.as_bytes()).header("Message-ID").map(|s| s.to_string()).unwrap_or_else(|| format!("#{}", raw.len())));
            if c.unseen && !c.delete && self.seen.lock().unwrap().contains(&id) {
                continue;
            }
            n += 1;
            if self.route(processor, raw.as_bytes()) {
                self.seen.lock().unwrap().insert(id);
                if c.delete {
                    client.dele(num)?;
                }
            }
        }
        client.quit()?;
        Ok(n)
    }

    fn poll_imap(&self, processor: &dyn Processor) -> Result<usize> {
        let c = &self.cfg;
        let mut client = imap::ImapClient::connect_with(&c.host, c.port, c.connection_timeout, c.tls.as_ref())?;
        let (u, p) = self.credentials();
        client.login(&u, &p)?;
        client.select(&c.folder_name)?;
        let ids = client.search(if c.unseen { "UNSEEN UNDELETED" } else { "UNDELETED" })?;
        let mut n = 0;
        let mut expunge = false;
        for num in ids {
            if c.max_messages_per_poll > 0 && n >= c.max_messages_per_poll {
                break;
            }
            let raw = client.fetch_message(num)?;
            n += 1;
            if self.route(processor, &raw) {
                client.store(num, "+FLAGS (\\Seen)")?;
                if let Some(f) = c.copy_to.as_ref().or(c.move_to.as_ref()) {
                    client.copy(num, f)?;
                }
                if c.delete || c.move_to.is_some() {
                    client.store(num, "+FLAGS (\\Deleted)")?;
                    expunge = true;
                }
            }
        }
        if expunge {
            client.expunge()?;
        }
        client.logout();
        Ok(n)
    }
}

/// A polling mail consumer (`pop3` / `imap`).
pub struct MailConsumer {
    poll: ScheduledPollConsumer,
}

impl MailConsumer {
    /// Run one poll now.
    pub fn poll_once(&self) -> Result<usize> {
        self.poll.poll_once()
    }
}

impl Service for MailConsumer {
    fn start(&self) -> Result<()> {
        self.poll.start()
    }
    fn stop(&self) -> Result<()> {
        self.poll.stop()
    }
}
