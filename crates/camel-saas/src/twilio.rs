//! `twilio:` component (Camel `camel-twilio`, API `message`).
//!
//! `twilio:message/create?from=+15005550006&to=+15005550001&username=AC...&password=token`
//!
//! Methods: `message/create` (send an SMS / MMS: parameters `to`, `from` or
//! `messagingServiceSid`, `body`, `mediaUrl`, `statusCallback`), `message/fetch`
//! (`pathSid`) and `message/read` (list). Parameters come from endpoint options or
//! `CamelTwilio.<name>` headers (headers win); `inBody` names the parameter taken from the
//! message body (default `body` for `message/create` when no body parameter is set).
//! The result (Twilio message resource, or the list for `read`) becomes the body.
//!
//! Credentials: `username` (account SID) / `password` (auth token), `accountSid`
//! (defaults to the username). `serverUrl` (extension, default `https://api.twilio.com`)
//! overrides the API base url. Twilio errors fail with `HttpOperationFailedException`
//! containing the Twilio error code and message.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;

use crate::rest::{body_text, join_url, required, Req, Rest};

/// Prefix of the parameter headers.
pub const HEADER_PREFIX: &str = "CamelTwilio.";

/// The `twilio` component.
#[derive(Default)]
pub struct TwilioComponent;

#[derive(Clone, Debug)]
struct Config {
    method: String,
    username: String,
    password: String,
    account_sid: String,
    server_url: String,
    in_body: Option<String>,
    params: BTreeMap<String, String>,
}

const API_PARAMS: &[(&str, &str)] = &[
    ("to", "To"),
    ("from", "From"),
    ("body", "Body"),
    ("mediaUrl", "MediaUrl"),
    ("statusCallback", "StatusCallback"),
    ("messagingServiceSid", "MessagingServiceSid"),
    ("pathSid", "pathSid"),
    ("pathAccountSid", "pathAccountSid"),
];

impl Component for TwilioComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let method = remaining.trim().trim_matches('/').to_string();
        if !matches!(method.as_str(), "message/create" | "message/creator" | "message/fetch" | "message/fetcher" | "message/read" | "message/reader") {
            return Err(CamelError::illegal_argument(format!("twilio: unsupported api method '{}' (message/create, message/fetch, message/read)", method)));
        }
        let username = required(params, "username", "twilio")?;
        let password = required(params, "password", "twilio")?;
        let account_sid = params.take("accountSid").unwrap_or_else(|| username.clone());
        let mut api = BTreeMap::new();
        for (k, _) in API_PARAMS {
            if let Some(v) = params.take(k) {
                api.insert(k.to_string(), v);
            }
        }
        let cfg = Config {
            method: method.replace("creator", "create").replace("fetcher", "fetch").replace("reader", "read"),
            username,
            password,
            account_sid,
            server_url: params.take("serverUrl").unwrap_or_else(|| "https://api.twilio.com".into()),
            in_body: params.take("inBody"),
            params: api,
        };
        params.take("lazyStartProducer");
        Ok(Arc::new(TwilioEndpoint { uri: uri.to_string(), cfg }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A twilio endpoint (producer only).
pub struct TwilioEndpoint {
    uri: String,
    cfg: Config,
}

impl Endpoint for TwilioEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(TwilioProducer { cfg: self.cfg.clone(), rest: Rest::default() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct TwilioProducer {
    cfg: Config,
    rest: Rest,
}

impl Service for TwilioProducer {}

impl Processor for TwilioProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let mut p = self.cfg.params.clone();
        for (k, _) in API_PARAMS {
            if let Some(v) = ex.message.headers.get(&format!("{}{}", HEADER_PREFIX, k)).filter(|v| !v.is_null()) {
                p.insert(k.to_string(), v.to_string());
            }
        }
        let in_body = self.cfg.in_body.clone().or_else(|| if self.cfg.method == "message/create" && !p.contains_key("body") && !p.contains_key("mediaUrl") { Some("body".into()) } else { None });
        if let Some(name) = in_body {
            if let Some(b) = body_text(ex) {
                p.insert(name, b);
            }
        }
        let account = p.get("pathAccountSid").cloned().unwrap_or_else(|| self.cfg.account_sid.clone());
        let base = join_url(&self.cfg.server_url, &format!("/2010-04-01/Accounts/{}/Messages", account));
        let req = match self.cfg.method.as_str() {
            "message/create" => {
                let form: Vec<(&str, String)> = API_PARAMS.iter().filter(|(k, _)| !k.starts_with("path")).filter_map(|(k, wire)| p.get(*k).map(|v| (*wire, v.clone()))).collect();
                if !form.iter().any(|(k, _)| *k == "To") {
                    return Err(CamelError::illegal_argument("twilio: message/create requires the 'to' parameter"));
                }
                Req::post(format!("{}.json", base)).form(&form)
            }
            "message/fetch" => {
                let sid = p.get("pathSid").ok_or_else(|| CamelError::illegal_argument("twilio: message/fetch requires the 'pathSid' parameter"))?;
                Req::get(format!("{}/{}.json", base, sid))
            }
            _ => Req::get(format!("{}.json", base)),
        };
        let resp = self.rest.send(req.basic(&self.cfg.username, &self.cfg.password))?;
        if !resp.is_success() {
            let v = resp.json().unwrap_or(camel_core::value::Value::Null);
            let msg = format!(
                "HTTP operation failed invoking Twilio {} with statusCode: {}, code: {}, message: {}",
                self.cfg.method,
                resp.status,
                v.get("code").map(|c| c.to_string()).unwrap_or_default(),
                v.get("message").map(|c| c.to_string()).unwrap_or_else(|| resp.text())
            );
            return Err(CamelError::new(camel_core::error::kinds::HTTP_OPERATION_FAILED, msg));
        }
        let v = resp.json()?;
        ex.message.body = if self.cfg.method == "message/read" { v.get("messages").cloned().unwrap_or(v) } else { v };
        Ok(())
    }
}
