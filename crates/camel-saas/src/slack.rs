//! `slack:` component (Camel `camel-slack`).
//!
//! `slack:#channel?webhookUrl=https://hooks.slack.com/services/...` (producer, incoming webhook),
//! `slack:#channel?token=xoxb-...` (producer, Web API `chat.postMessage`),
//! `slack://general?token=xoxb-...&maxResults=10` (consumer, `conversations.history` polling).
//!
//! Producer: a `String` body is the message `text`; an object body is sent as the message
//! payload (`blocks`, `attachments`...; `channel` is added when missing). Options `username`,
//! `iconUrl`, `iconEmoji` are added to webhook payloads. Web API errors (`"ok": false`) fail
//! with `ApiException` (`Slack: channel_not_found`). The Web API response is stored in the
//! `CamelSlackResponse` header.
//!
//! Consumer: the channel (name or id) is resolved with `conversations.list`
//! (`conversationType`: `PUBLIC_CHANNEL` (default), `PRIVATE_CHANNEL`, `MPIM`, `IM`), then
//! `conversations.history` is polled for messages newer than the last one seen (the first
//! poll delivers the latest `maxResults`, default 10). Each message becomes an exchange whose
//! body is the message object (`text`, `user`, `ts`...); `naturalOrder=true` delivers oldest
//! first. Scheduled poll options apply (`delay`, default 10 s).
//!
//! `serverUrl` (default `https://slack.com`) overrides the API base url.

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::{ObjectMap, Value};

use crate::rest::{api_error, body_text, emit, join_url, poll_config, poll_consumer, query, Req, Rest};

/// Header with the Web API response of `chat.postMessage`.
pub const SLACK_RESPONSE: &str = "CamelSlackResponse";

/// The `slack` component.
#[derive(Default)]
pub struct SlackComponent;

#[derive(Clone, Debug)]
struct Config {
    channel: String,
    webhook_url: Option<String>,
    token: Option<String>,
    server_url: String,
    username: Option<String>,
    icon_url: Option<String>,
    icon_emoji: Option<String>,
    max_results: usize,
    natural_order: bool,
    conversation_type: String,
}

impl Component for SlackComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let channel = remaining.trim().to_string();
        if channel.is_empty() {
            return Err(CamelError::illegal_argument("slack: a channel is required (slack:#channel)"));
        }
        let cfg = Config {
            channel,
            webhook_url: params.take("webhookUrl").filter(|s| !s.is_empty()),
            token: params.take("token").filter(|s| !s.is_empty()),
            server_url: params.take("serverUrl").unwrap_or_else(|| "https://slack.com".into()),
            username: params.take("username"),
            icon_url: params.take("iconUrl"),
            icon_emoji: params.take("iconEmoji"),
            max_results: params.take_usize("maxResults", 10)?,
            natural_order: params.take_bool("naturalOrder", false)?,
            conversation_type: params.take("conversationType").unwrap_or_else(|| "PUBLIC_CHANNEL".into()),
        };
        let poll = poll_config(params, Duration::from_secs(10))?;
        params.take("lazyStartProducer");
        Ok(Arc::new(SlackEndpoint { uri: uri.to_string(), cfg, poll, ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A slack endpoint.
pub struct SlackEndpoint {
    uri: String,
    cfg: Config,
    poll: camel_core::component::scheduled::ScheduledPollConfig,
    ctx: WeakContext,
}

impl Endpoint for SlackEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        if self.cfg.webhook_url.is_none() && self.cfg.token.is_none() {
            return Err(CamelError::illegal_argument("slack: either webhookUrl or token must be configured"));
        }
        Ok(Arc::new(SlackProducer { cfg: self.cfg.clone(), rest: Rest::default() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let Some(token) = self.cfg.token.clone() else {
            return Err(CamelError::illegal_argument("slack: the consumer requires the token option"));
        };
        let cfg = self.cfg.clone();
        let ctx = self.ctx.clone();
        let rest = Rest::default();
        let state: Arc<Mutex<(Option<String>, Option<String>)>> = Arc::new(Mutex::new((None, None))); // (channel id, last ts)
        Ok(Arc::new(poll_consumer("slack", self.poll.clone(), move || {
            let mut st = state.lock().unwrap();
            if st.0.is_none() {
                st.0 = Some(resolve_channel(&rest, &cfg, &token)?);
            }
            let ch = st.0.clone().unwrap();
            let mut q = vec![("channel", ch), ("limit", cfg.max_results.to_string())];
            if let Some(ts) = &st.1 {
                q.push(("oldest", ts.clone()));
            }
            let url = format!("{}?{}", join_url(&cfg.server_url, "/api/conversations.history"), query(&q));
            let resp = check(rest.json(Req::get(url).bearer(&token))?)?;
            let mut msgs: Vec<Value> = resp.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
            if let Some(newest) = msgs.iter().filter_map(|m| m.get("ts").map(|t| t.to_string())).max_by(|a, b| a.parse::<f64>().unwrap_or(0.0).partial_cmp(&b.parse::<f64>().unwrap_or(0.0)).unwrap()) {
                st.1 = Some(newest);
            }
            drop(st);
            if cfg.natural_order {
                msgs.reverse();
            }
            let n = msgs.len();
            for m in msgs {
                emit(&ctx, processor.as_ref(), "slack", m, vec![]);
            }
            Ok(n)
        })))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn check(v: Value) -> Result<Value> {
    if v.get("ok").and_then(|o| o.as_bool()) == Some(true) {
        Ok(v)
    } else {
        Err(api_error("Slack", v.get("error").map(|e| e.to_string()).unwrap_or_else(|| "unknown error".into())))
    }
}

fn resolve_channel(rest: &Rest, cfg: &Config, token: &str) -> Result<String> {
    let name = cfg.channel.trim_start_matches('#').to_string();
    let types = match cfg.conversation_type.to_ascii_uppercase().as_str() {
        "PRIVATE_CHANNEL" => "private_channel",
        "MPIM" => "mpim",
        "IM" => "im",
        _ => "public_channel",
    };
    let url = format!("{}?{}", join_url(&cfg.server_url, "/api/conversations.list"), query(&[("types", types.into()), ("limit", "1000".into())]));
    let resp = check(rest.json(Req::get(url).bearer(token))?)?;
    for c in resp.get("channels").and_then(|c| c.as_array()).cloned().unwrap_or_default() {
        let id = c.get("id").map(|v| v.to_string()).unwrap_or_default();
        if id == name || c.get("name").map(|v| v.to_string()) == Some(name.clone()) {
            return Ok(id);
        }
    }
    Err(api_error("Slack", format!("channel '{}' not found", cfg.channel)))
}

struct SlackProducer {
    cfg: Config,
    rest: Rest,
}

impl Service for SlackProducer {}

impl SlackProducer {
    fn payload(&self, ex: &Exchange) -> Value {
        let mut m = match &ex.message.body {
            Value::Object(o) => o.clone(),
            _ => {
                let mut o = ObjectMap::new();
                o.insert("text".to_string(), Value::String(body_text(ex).unwrap_or_default()));
                o
            }
        };
        m.entry("channel".to_string()).or_insert_with(|| Value::String(self.cfg.channel.clone()));
        Value::Object(m)
    }
}

impl Processor for SlackProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let mut payload = self.payload(ex);
        if let Some(url) = &self.cfg.webhook_url {
            if let Some(m) = payload.as_object_mut() {
                for (k, v) in [("username", &self.cfg.username), ("icon_url", &self.cfg.icon_url), ("icon_emoji", &self.cfg.icon_emoji)] {
                    if let Some(v) = v {
                        m.entry(k.to_string()).or_insert_with(|| Value::String(v.clone()));
                    }
                }
            }
            self.rest.send_ok(Req::post(url.clone()).json(&payload))?;
            return Ok(());
        }
        let token = self.cfg.token.as_deref().unwrap();
        let resp = check(self.rest.json(Req::post(join_url(&self.cfg.server_url, "/api/chat.postMessage")).bearer(token).json(&payload))?)?;
        ex.message.headers.set(SLACK_RESPONSE, resp);
        Ok(())
    }
}
