//! # rust-camel protocols
//!
//! Network protocol components built on `std::net` only (no TLS, no SSH):
//!
//! | scheme | module | role |
//! |---|---|---|
//! | `ftp` | [`ftp`] | FTP consumer (polling) and producer; `ftps`/`sftp` fail with a clear error |
//! | `smtp` | [`mail`] | send mail; `smtps` fails |
//! | `pop3`, `imap` | [`mail`] | polling mail consumers; `pop3s`/`imaps` fail |
//! | `websocket`, `vertx-websocket`, `ws` | [`websocket`] | RFC 6455 server and client; `wss` fails |
//! | `mllp` | [`mllp`] | HL7 v2 over MLLP with auto acknowledgement |
//! | `syslog` | [`syslog`] | syslog over UDP/TCP (RFC 5424 / 3164) |
//!
//! Data formats bound in the registry by [`register`]: `hl7` ([`hl7::Hl7DataFormat`]) and
//! `syslog` ([`syslog::SyslogDataFormat`]); language `hl7terser`.
//! Embedded FTP, SMTP, POP3 and IMAP servers for tests live in [`testing`].
//!
//! ```no_run
//! use camel_core::prelude::*;
//!
//! let ctx = CamelContext::new();
//! camel_protocols::register(&ctx);
//! ctx.add_routes(|b| {
//!     b.from("ftp://admin@localhost:21/inbox?password=secret&passiveMode=true&move=done").to("file:target/in");
//!     b.from("mllp://0.0.0.0:2575").log("${header.CamelMllpMessageControlId}");
//!     b.from("websocket://0.0.0.0:9292/echo").to("websocket://0.0.0.0:9292/echo");
//!     b.from("syslog:udp://0.0.0.0:5514").log("${header.CamelSyslogSeverity} ${body}");
//! }).unwrap();
//! ctx.start().unwrap();
//! ```

pub mod common;
pub mod ftp;
pub mod hl7;
pub mod mail;
pub mod mllp;
pub mod syslog;
pub mod testing;
pub mod websocket;

use std::any::Any;
use std::sync::Arc;

use camel_core::context::CamelContext;
use camel_core::dataformat::{bind_data_format, lookup_data_format};
use camel_core::error::{kinds, CamelError, Result};
use camel_core::spi::{Component, Endpoint};
use camel_core::util::Parameters;

/// A component for a scheme that cannot be supported in a std-only build (TLS / SSH):
/// creating an endpoint fails with `UnsupportedOperationException`.
pub struct UnsupportedComponent {
    scheme: String,
    reason: String,
}

impl UnsupportedComponent {
    pub fn new(scheme: &str, reason: &str) -> Self {
        UnsupportedComponent { scheme: scheme.to_string(), reason: reason.to_string() }
    }
}

impl Component for UnsupportedComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, _remaining: &str, _params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        Err(CamelError::new(
            kinds::UNSUPPORTED_OPERATION,
            format!("The {} component is not supported by rust-camel ({}): {}", self.scheme, self.reason, uri),
        ))
    }
    fn lenient_properties(&self) -> bool {
        true
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn add(ctx: &CamelContext, scheme: &'static str, f: impl Fn() -> Arc<dyn Component> + Send + Sync + 'static) {
    if !ctx.has_component(scheme) {
        ctx.add_component_factory(scheme, f);
    }
}

/// Register all components of this crate on a context (lazily created): `ftp`, `ftps`
/// and `sftp` (errors), `smtp`, `pop3`, `imap` (`smtps`, `pop3s`, `imaps` error),
/// `websocket`, `vertx-websocket`, `ws` (`wss` errors), `mllp`, `syslog`; binds the
/// `hl7` and `syslog` data formats and adds the `hl7terser` language. Schemes that
/// already have a component are left untouched.
pub fn register(ctx: &CamelContext) {
    use websocket::{Flavor, WebSocketComponent};
    add(ctx, "ftp", || Arc::new(ftp::FtpComponent));
    add(ctx, "ftps", || Arc::new(UnsupportedComponent::new("ftps", "no TLS in a std-only build; use plain ftp")));
    add(ctx, "sftp", || Arc::new(UnsupportedComponent::new("sftp", "no SSH in a std-only build; use plain ftp")));
    add(ctx, "smtp", || Arc::new(mail::MailComponent::new(mail::MailProtocol::Smtp)));
    add(ctx, "pop3", || Arc::new(mail::MailComponent::new(mail::MailProtocol::Pop3)));
    add(ctx, "imap", || Arc::new(mail::MailComponent::new(mail::MailProtocol::Imap)));
    for scheme in ["smtps", "pop3s", "imaps"] {
        add(ctx, scheme, move || Arc::new(UnsupportedComponent::new(scheme, "no TLS in a std-only build; use smtp, pop3 or imap")));
    }
    add(ctx, "websocket", || Arc::new(WebSocketComponent::new(Flavor::Camel)));
    add(ctx, "vertx-websocket", || Arc::new(WebSocketComponent::new(Flavor::Vertx)));
    add(ctx, "ws", || Arc::new(WebSocketComponent::new(Flavor::Client)));
    add(ctx, "wss", || Arc::new(UnsupportedComponent::new("wss", "no TLS in a std-only build; use ws")));
    add(ctx, "mllp", || Arc::new(mllp::MllpComponent));
    add(ctx, "syslog", || Arc::new(syslog::SyslogComponent));
    if lookup_data_format(ctx.registry(), "hl7").is_none() {
        bind_data_format(ctx.registry(), "hl7", hl7::Hl7DataFormat::new());
    }
    if lookup_data_format(ctx.registry(), "syslog").is_none() {
        bind_data_format(ctx.registry(), "syslog", syslog::SyslogDataFormat);
    }
    if ctx.resolve_language("hl7terser").is_err() {
        ctx.add_language(hl7::Hl7TerserLanguage);
    }
}

/// The actual listening port of a started server endpoint of this crate
/// (`websocket`/`vertx-websocket`, `mllp`, `syslog`) — useful with port 0.
pub fn local_port(ctx: &CamelContext, uri: &str) -> Option<u16> {
    let ep = ctx.get_endpoint(uri).ok()?;
    let any = ep.as_any();
    if let Some(e) = any.downcast_ref::<websocket::WebSocketEndpoint>() {
        return e.local_port();
    }
    if let Some(e) = any.downcast_ref::<mllp::MllpEndpoint>() {
        return e.local_port();
    }
    if let Some(e) = any.downcast_ref::<syslog::SyslogEndpoint>() {
        return e.local_port();
    }
    None
}
