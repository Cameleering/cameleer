//! `log:` component - logs exchanges (Camel log component with showXXX options).

use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::context::CamelContext;
use crate::error::Result;
use crate::log::Level;
use crate::message::Exchange;
use crate::spi::{Component, Endpoint, Processor, Producer, Service};
use crate::util::Parameters;

#[derive(Default)]
pub struct LogComponent;

/// Formatting options (Camel `DefaultExchangeFormatter`).
#[derive(Clone, Debug)]
pub struct ExchangeFormatter {
    pub show_all: bool,
    pub show_exchange_id: bool,
    pub show_exchange_pattern: bool,
    pub show_properties: bool,
    pub show_headers: bool,
    pub show_body: bool,
    pub show_body_type: bool,
    pub show_exception: bool,
    pub show_route_id: bool,
    pub multiline: bool,
    pub max_chars: usize,
    pub skip_body_line_separator: bool,
}

impl Default for ExchangeFormatter {
    fn default() -> Self {
        ExchangeFormatter {
            show_all: false,
            show_exchange_id: false,
            show_exchange_pattern: true,
            show_properties: false,
            show_headers: false,
            show_body: true,
            show_body_type: true,
            show_exception: true,
            show_route_id: false,
            multiline: false,
            max_chars: 10_000,
            skip_body_line_separator: true,
        }
    }
}

impl ExchangeFormatter {
    pub fn format(&self, ex: &Exchange) -> String {
        let all = self.show_all;
        let mut parts: Vec<String> = Vec::new();
        if all || self.show_exchange_id {
            parts.push(format!("Id: {}", ex.exchange_id));
        }
        if all || self.show_route_id {
            parts.push(format!("RouteId: {}", ex.from_route_id.clone().unwrap_or_default()));
        }
        if all || self.show_exchange_pattern {
            parts.push(format!("ExchangePattern: {:?}", ex.pattern));
        }
        if all || self.show_properties {
            let p: Vec<String> = ex.properties.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
            parts.push(format!("Properties: {{{}}}", p.join(", ")));
        }
        if all || self.show_headers {
            let h: Vec<String> = ex.message.headers.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
            parts.push(format!("Headers: {{{}}}", h.join(", ")));
        }
        if all || self.show_body_type {
            parts.push(format!("BodyType: {}", ex.message.body.type_name()));
        }
        if all || self.show_body {
            let mut b = if ex.message.body.is_null() { "[Body is null]".to_string() } else { ex.message.body.to_string() };
            if self.skip_body_line_separator && !self.multiline {
                b = b.replace(['\n', '\r'], "");
            }
            if b.chars().count() > self.max_chars {
                b = format!("{}... [Body clipped after {} chars, total length is {}]", b.chars().take(self.max_chars).collect::<String>(), self.max_chars, b.len());
            }
            parts.push(format!("Body: {}", b));
        }
        if (all || self.show_exception) && ex.exception.is_some() {
            parts.push(format!("Exception: {}", ex.exception.as_ref().unwrap()));
        }
        if self.multiline {
            format!("Exchange[\n  {}\n]", parts.join("\n  "))
        } else {
            format!("Exchange[{}]", parts.join(", "))
        }
    }
}

pub struct LogEndpoint {
    uri: String,
    logger: String,
    level: Level,
    formatter: ExchangeFormatter,
    group_size: Option<u64>,
    group_interval: Option<std::time::Duration>,
    log_mask: bool,
}

impl Component for LogComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let level = params.take("level").and_then(|l| Level::parse(&l)).unwrap_or(Level::Info);
        let mut f = ExchangeFormatter::default();
        f.show_all = params.take_bool("showAll", false)?;
        f.show_exchange_id = params.take_bool("showExchangeId", false)?;
        f.show_exchange_pattern = params.take_bool("showExchangePattern", true)?;
        f.show_properties = params.take_bool("showProperties", false)?;
        f.show_headers = params.take_bool("showHeaders", false)?;
        f.show_body = params.take_bool("showBody", true)?;
        f.show_body_type = params.take_bool("showBodyType", true)?;
        f.show_exception = params.take_bool("showException", true)?;
        f.show_route_id = params.take_bool("showRouteId", false)?;
        f.multiline = params.take_bool("multiline", false)?;
        f.max_chars = params.take_usize("maxChars", 10_000)?;
        f.skip_body_line_separator = params.take_bool("skipBodyLineSeparator", true)?;
        for k in ["showCaughtException", "showStackTrace", "showFiles", "showFuture", "showStreams", "showCachedStreams", "showAllProperties", "showVariables", "plain", "style", "marker", "sourceLocationLoggerName", "showRouteGroup"] {
            params.take(k);
        }
        let group_size = params.take("groupSize").map(|s| s.parse::<u64>()).transpose().map_err(|_| crate::error::CamelError::illegal_argument("groupSize must be a number"))?;
        let group_interval = params.take("groupInterval").map(|s| crate::util::parse_duration(&s)).transpose()?;
        params.take("groupDelay");
        params.take("groupActiveOnly");
        let log_mask = params.take_bool("logMask", false)?;
        Ok(Arc::new(LogEndpoint {
            uri: uri.to_string(),
            logger: remaining.to_string(),
            level,
            formatter: f,
            group_size,
            group_interval,
            log_mask,
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct LogProducer {
    logger: String,
    level: Level,
    formatter: ExchangeFormatter,
    group_size: Option<u64>,
    group_interval: Option<std::time::Duration>,
    counter: AtomicU64,
    group_start: std::sync::Mutex<(Instant, u64)>,
    log_mask: bool,
}

impl Service for LogProducer {}

impl Processor for LogProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(g) = self.group_size {
            if g > 0 && n.is_multiple_of(g) {
                crate::log::log(self.level, &self.logger, format!("Received: {} new messages, with total {} so far.", g, n));
            }
            return Ok(());
        }
        if let Some(iv) = self.group_interval {
            let mut gs = self.group_start.lock().unwrap();
            gs.1 += 1;
            if gs.0.elapsed() >= iv {
                let secs = gs.0.elapsed().as_secs_f64();
                crate::log::log(self.level, &self.logger, format!("Received: {} new messages, with total {} so far. Last group took: {:.0} millis which is: {:.3} messages per second.", gs.1, n, secs * 1000.0, gs.1 as f64 / secs));
                *gs = (Instant::now(), 0);
            }
            return Ok(());
        }
        if crate::log::enabled(self.level) {
            let mut msg = self.formatter.format(ex);
            if self.log_mask {
                msg = mask_secrets(&msg);
            }
            crate::log::log(self.level, &self.logger, msg);
        }
        Ok(())
    }
}

/// Mask values of keys that look like secrets (password, secret, token, passphrase...).
pub fn mask_secrets(s: &str) -> String {
    let keys = ["password", "passphrase", "secret", "token", "accesskey", "secretkey"];
    let mut out = s.to_string();
    for k in keys {
        let mut search_from = 0;
        loop {
            let lower = out.to_ascii_lowercase();
            let Some(pos) = lower[search_from..].find(k) else { break };
            let start = search_from + pos + k.len();
            let bytes: Vec<char> = out[start..].chars().collect();
            if bytes.first() == Some(&'=') || bytes.first() == Some(&':') {
                let vstart = start + 1;
                let vend = out[vstart..].find([',', '}', '&', ' ', ']']).map(|i| vstart + i).unwrap_or(out.len());
                out.replace_range(vstart..vend, "xxxxxx");
                search_from = vstart + 6;
            } else {
                search_from = start;
            }
            if search_from >= out.len() {
                break;
            }
        }
    }
    out
}

impl Endpoint for LogEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(LogProducer {
            logger: self.logger.clone(),
            level: self.level,
            formatter: self.formatter.clone(),
            group_size: self.group_size,
            group_interval: self.group_interval,
            counter: AtomicU64::new(0),
            group_start: std::sync::Mutex::new((Instant::now(), 0)),
            log_mask: self.log_mask,
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
