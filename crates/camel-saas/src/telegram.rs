//! `telegram:` component (Camel `camel-telegram`).
//!
//! `telegram:bots?authorizationToken=123:ABC&chatId=42`
//!
//! Producer: the chat id comes from the `CamelTelegramChatId` header or the `chatId`
//! option. A text body is sent with `sendMessage` (`parseMode` option or
//! `CamelTelegramParseMode` header: `HTML`, `MARKDOWN`, `MarkdownV2`). A byte body with the
//! `CamelTelegramMediaType` header (`PHOTO_JPG`, `PHOTO_PNG`, `AUDIO`, `VIDEO`, `DOCUMENT`)
//! is uploaded as multipart (`sendPhoto`, `sendAudio`, `sendVideo`, `sendDocument`) with the
//! `CamelTelegramMediaTitleCaption` header as caption. An object body is sent as the
//! parameters of `sendPhoto` / `sendDocument` (when it has a `photo` / `document` field) or
//! `sendMessage`. For InOut exchanges the body becomes the API `result`; the message id is
//! always set in `CamelTelegramMessageId`. API errors fail with `ApiException`.
//!
//! Consumer: `getUpdates` long polling (`timeout` seconds, default 30; `limit`, default 100;
//! scheduled poll `delay`, default 500 ms). Each update becomes an exchange whose body is
//! the message object (`text`, `chat`, `from`...), or the whole update for other update
//! kinds, with the `CamelTelegramChatId` header.
//!
//! Webhooks: `webhook:telegram:bots?...` registers the webhook with `setWebhook` (see
//! [`crate::webhook`]).
//!
//! `baseUri` (default `https://api.telegram.org`) overrides the API base url.

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::{Exchange, ExchangePattern};
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

use crate::rest::{api_error, body_text, emit, header_str, poll_config, poll_consumer, required, Part, Req, Rest};
use crate::webhook::WebhookCapable;

pub const CHAT_ID: &str = "CamelTelegramChatId";
pub const MEDIA_TYPE: &str = "CamelTelegramMediaType";
pub const MEDIA_TITLE_CAPTION: &str = "CamelTelegramMediaTitleCaption";
pub const PARSE_MODE: &str = "CamelTelegramParseMode";
pub const MESSAGE_ID: &str = "CamelTelegramMessageId";

/// The `telegram` component.
#[derive(Default)]
pub struct TelegramComponent;

#[derive(Clone, Debug)]
struct Config {
    token: String,
    chat_id: Option<String>,
    base_uri: String,
    timeout: u64,
    limit: u64,
    parse_mode: Option<String>,
}

impl Config {
    fn method_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base_uri.trim_end_matches('/'), self.token, method)
    }
}

impl Component for TelegramComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let ty = remaining.trim().trim_matches('/');
        if ty != "bots" {
            return Err(CamelError::illegal_argument(format!("telegram: unknown endpoint type '{}' (only 'bots' is supported)", ty)));
        }
        let cfg = Config {
            token: required(params, "authorizationToken", "telegram")?,
            chat_id: params.take("chatId").filter(|s| !s.is_empty()),
            base_uri: params.take("baseUri").unwrap_or_else(|| "https://api.telegram.org".into()),
            timeout: params.take_u64("timeout", 30)?,
            limit: params.take_u64("limit", 100)?,
            parse_mode: params.take("parseMode"),
        };
        for k in ["proxyHost", "proxyPort", "proxyType", "client", "bufferSize", "lazyStartProducer"] {
            params.take(k);
        }
        let poll = poll_config(params, Duration::from_millis(500))?;
        let rest = Rest::new(Duration::from_secs(cfg.timeout + 15));
        Ok(Arc::new(TelegramEndpoint { uri: uri.to_string(), cfg, poll, rest, ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A telegram endpoint.
pub struct TelegramEndpoint {
    uri: String,
    cfg: Config,
    poll: camel_core::component::scheduled::ScheduledPollConfig,
    rest: Rest,
    ctx: WeakContext,
}

fn check(v: Value) -> Result<Value> {
    if v.get("ok").and_then(|o| o.as_bool()) == Some(true) {
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(api_error("Telegram", v.get("description").map(|e| e.to_string()).unwrap_or_else(|| "unknown error".into())))
    }
}

fn call(rest: &Rest, req: Req) -> Result<Value> {
    let r = rest.send(req)?;
    match r.json() {
        Ok(v) if v.get("ok").is_some() => check(v),
        _ if !r.is_success() => Err(crate::rest::http_failed("telegram", &r)),
        other => other,
    }
}

/// Body and chat id headers for an incoming update.
fn update_message(update: &Value) -> (Value, Option<Value>) {
    for k in ["message", "edited_message", "channel_post", "edited_channel_post"] {
        if let Some(m) = update.get(k) {
            let chat = m.get("chat").and_then(|c| c.get("id")).cloned();
            return (m.clone(), chat);
        }
    }
    let chat = update.get("callback_query").and_then(|c| c.get("message")).and_then(|m| m.get("chat")).and_then(|c| c.get("id")).cloned();
    (update.clone(), chat)
}

impl Endpoint for TelegramEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(TelegramProducer { cfg: self.cfg.clone(), rest: self.rest.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let cfg = self.cfg.clone();
        let ctx = self.ctx.clone();
        let rest = self.rest.clone();
        let offset: Arc<Mutex<i64>> = Arc::new(Mutex::new(0));
        Ok(Arc::new(poll_consumer("telegram", self.poll.clone(), move || {
            let off = *offset.lock().unwrap();
            let body = Value::object([("offset", Value::Int(off)), ("limit", Value::Int(cfg.limit as i64)), ("timeout", Value::Int(cfg.timeout as i64))]);
            let updates = call(&rest, Req::post(cfg.method_url("getUpdates")).json(&body))?;
            let list = updates.as_array().cloned().unwrap_or_default();
            let n = list.len();
            for u in list {
                if let Some(id) = u.get("update_id").and_then(|i| i.as_i64()) {
                    let mut o = offset.lock().unwrap();
                    *o = (*o).max(id + 1);
                }
                let (body, chat) = update_message(&u);
                let headers = chat.map(|c| vec![(CHAT_ID, c)]).unwrap_or_default();
                emit(&ctx, processor.as_ref(), "telegram", body, headers);
            }
            Ok(n)
        })))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl WebhookCapable for TelegramEndpoint {
    fn register_webhook(&self, url: &str) -> Result<()> {
        call(&self.rest, Req::post(self.cfg.method_url("setWebhook")).json(&Value::object([("url", url)])))?;
        Ok(())
    }
    fn unregister_webhook(&self, _url: &str) -> Result<()> {
        call(&self.rest, Req::post(self.cfg.method_url("deleteWebhook")).json(&Value::object(Vec::<(String, Value)>::new())))?;
        Ok(())
    }
    fn webhook_processor(&self, next: Arc<dyn Processor>) -> Arc<dyn Processor> {
        Arc::new(move |ex: &mut Exchange| {
            let text = body_text(ex).unwrap_or_default();
            let update = camel_core::json::parse(&text).map_err(|e| CamelError::illegal_argument(format!("telegram webhook: invalid update: {}", e.message)))?;
            let (body, chat) = update_message(&update);
            ex.message.body = body;
            if let Some(c) = chat {
                ex.message.headers.set(CHAT_ID, c);
            }
            next.process(ex)?;
            ex.message.headers.set("CamelHttpResponseCode", Value::Int(200));
            ex.message.body = Value::String(String::new());
            Ok(())
        })
    }
}

struct TelegramProducer {
    cfg: Config,
    rest: Rest,
}

impl Service for TelegramProducer {}

impl Processor for TelegramProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let chat = header_str(ex, CHAT_ID).or_else(|| self.cfg.chat_id.clone()).ok_or_else(|| CamelError::illegal_argument("telegram: chat id is not set (CamelTelegramChatId header or chatId option)"))?;
        let parse_mode = header_str(ex, PARSE_MODE).or_else(|| self.cfg.parse_mode.clone());
        let media = header_str(ex, MEDIA_TYPE).map(|m| m.to_ascii_uppercase());
        let result = match (&ex.message.body, media.as_deref()) {
            (Value::Bytes(data), Some(mt)) => {
                let (method, field, filename, ct) = match mt {
                    "PHOTO_JPG" => ("sendPhoto", "photo", "photo.jpg", "image/jpeg"),
                    "PHOTO_PNG" => ("sendPhoto", "photo", "photo.png", "image/png"),
                    "AUDIO" => ("sendAudio", "audio", "audio.mp3", "audio/mpeg"),
                    "VIDEO" => ("sendVideo", "video", "video.mp4", "video/mp4"),
                    "DOCUMENT" => ("sendDocument", "document", "file", "application/octet-stream"),
                    other => return Err(CamelError::illegal_argument(format!("telegram: unsupported media type {}", other))),
                };
                let mut parts = vec![Part { name: "chat_id".into(), filename: None, content_type: None, data: chat.into_bytes() }];
                if let Some(c) = header_str(ex, MEDIA_TITLE_CAPTION) {
                    parts.push(Part { name: "caption".into(), filename: None, content_type: None, data: c.into_bytes() });
                }
                parts.push(Part { name: field.into(), filename: Some(filename.into()), content_type: Some(ct.into()), data: data.clone() });
                call(&self.rest, Req::post(self.cfg.method_url(method)).multipart(&parts))?
            }
            (Value::Object(o), _) => {
                let mut o = o.clone();
                o.entry("chat_id".to_string()).or_insert(Value::String(chat));
                if let Some(pm) = &parse_mode {
                    o.entry("parse_mode".to_string()).or_insert(Value::String(pm.clone()));
                }
                let method = if o.contains_key("photo") {
                    "sendPhoto"
                } else if o.contains_key("document") {
                    "sendDocument"
                } else {
                    "sendMessage"
                };
                call(&self.rest, Req::post(self.cfg.method_url(method)).json(&Value::Object(o)))?
            }
            _ => {
                let text = body_text(ex).ok_or_else(|| CamelError::illegal_argument("telegram: the message body is empty"))?;
                let mut p = vec![("chat_id", Value::String(chat)), ("text", Value::String(text))];
                if let Some(pm) = parse_mode {
                    p.push(("parse_mode", Value::String(pm)));
                }
                call(&self.rest, Req::post(self.cfg.method_url("sendMessage")).json(&Value::object(p)))?
            }
        };
        if let Some(id) = result.get("message_id") {
            ex.message.headers.set(MESSAGE_ID, id.clone());
        }
        if ex.pattern == ExchangePattern::InOut {
            ex.message.body = result;
        }
        Ok(())
    }
}
