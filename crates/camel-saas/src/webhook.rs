//! `webhook:` meta-component (Camel `camel-webhook`): exposes a webhook-capable delegate
//! endpoint through an HTTP consumer and registers the webhook with the remote service.
//!
//! `webhook:telegram:bots?authorizationToken=...&webhookExternalUrl=https://my.host&webhookPath=/tg`
//!
//! Options (all other options are passed to the delegate endpoint):
//! * `webhookExternalUrl`: the public base url announced to the service (or property
//!   `camel.component.webhook.configuration.webhook-external-url` /
//!   `camel.component.webhook.webhookExternalUrl`). Required when registering.
//! * `webhookBasePath` (default empty) and `webhookPath` (default
//!   `/webhook/<scheme>-<hash of the delegate uri>`): where the HTTP consumer listens.
//! * `webhookAutoRegister` (default true): register on start and unregister on stop.
//! * `webhookComponentName` (default `platform-http`): the HTTP consumer component
//!   (`camel.server.port` sets the port of `platform-http`).
//!
//! Webhook-capable delegates: `telegram` (`setWebhook` / `deleteWebhook`) and `github`
//! (repository hooks; the `X-GitHub-Event` request header becomes `GitHubEvent`).

use std::any::Any;
use std::sync::{Arc, Mutex};

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Service};
use camel_core::util::{url_encode, Parameters};

/// An endpoint that can receive its messages through a webhook.
pub trait WebhookCapable: Send + Sync {
    /// Register `url` with the remote service.
    fn register_webhook(&self, url: &str) -> Result<()>;
    /// Remove the registration.
    fn unregister_webhook(&self, url: &str) -> Result<()>;
    /// Processor converting an incoming HTTP exchange into the delegate's message format and
    /// passing it to `next` (the route).
    fn webhook_processor(&self, next: Arc<dyn Processor>) -> Arc<dyn Processor>;
}

/// The webhook-capable view of an endpoint, if it supports webhooks.
pub fn as_webhook_capable(ep: &dyn Endpoint) -> Option<&dyn WebhookCapable> {
    let any = ep.as_any();
    if let Some(t) = any.downcast_ref::<crate::telegram::TelegramEndpoint>() {
        return Some(t);
    }
    if let Some(g) = any.downcast_ref::<crate::github::GithubEndpoint>() {
        return Some(g);
    }
    None
}

/// The `webhook` component.
#[derive(Default)]
pub struct WebhookComponent;

#[derive(Clone, Debug)]
struct Config {
    delegate_uri: String,
    external_url: Option<String>,
    base_path: String,
    path: String,
    auto_register: bool,
    component: String,
}

fn hash(s: &str) -> u64 {
    // FNV-1a
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

impl Component for WebhookComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let delegate = remaining.trim();
        if delegate.is_empty() || !delegate.contains(':') {
            return Err(CamelError::illegal_argument("webhook: a delegate endpoint uri is required (webhook:telegram:bots?...)"));
        }
        let props = ctx.properties();
        let external_url = params
            .take("webhookExternalUrl")
            .or_else(|| props.get("camel.component.webhook.configuration.webhook-external-url"))
            .or_else(|| props.get("camel.component.webhook.webhookExternalUrl"))
            .filter(|s| !s.is_empty());
        let base_path = params.take("webhookBasePath").unwrap_or_default();
        let auto_register = params.take_bool("webhookAutoRegister", true)?;
        let component = params.take("webhookComponentName").unwrap_or_else(|| "platform-http".into());
        let explicit_path = params.take("webhookPath");
        let rest = params.take_all();
        let mut delegate_uri = delegate.to_string();
        if !rest.is_empty() {
            let q: Vec<String> = rest.iter().map(|(k, v)| format!("{}={}", k, url_encode(v))).collect();
            delegate_uri.push(if delegate_uri.contains('?') { '&' } else { '?' });
            delegate_uri.push_str(&q.join("&"));
        }
        let scheme = delegate.split(':').next().unwrap_or("x").to_string();
        let path = explicit_path.unwrap_or_else(|| format!("/webhook/{}-{:x}", scheme, hash(&delegate_uri)));
        let path = if path.starts_with('/') { path } else { format!("/{}", path) };
        let cfg = Config { delegate_uri, external_url, base_path: base_path.trim_end_matches('/').to_string(), path, auto_register, component };
        Ok(Arc::new(WebhookEndpoint { uri: uri.to_string(), cfg, ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A webhook endpoint (consumer only).
pub struct WebhookEndpoint {
    uri: String,
    cfg: Config,
    ctx: WeakContext,
}

impl WebhookEndpoint {
    /// The full path the HTTP consumer listens on.
    pub fn path(&self) -> String {
        let p = format!("{}{}", self.cfg.base_path, self.cfg.path);
        if p.starts_with('/') {
            p
        } else {
            format!("/{}", p)
        }
    }
}

impl Endpoint for WebhookEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let ctx = self.ctx.upgrade().ok_or_else(|| CamelError::illegal_state("webhook: context is gone"))?;
        let delegate = ctx.get_endpoint(&self.cfg.delegate_uri)?;
        if as_webhook_capable(delegate.as_ref()).is_none() {
            return Err(CamelError::illegal_argument(format!("webhook: the endpoint {} does not support webhooks", self.cfg.delegate_uri)));
        }
        let path = self.path();
        let http_uri = format!("{}:{}?httpMethodRestrict=POST", self.cfg.component, path);
        let http = ctx.get_endpoint(&http_uri)?;
        let wrapped = as_webhook_capable(delegate.as_ref()).unwrap().webhook_processor(processor);
        let http_consumer = http.create_consumer(wrapped)?;
        let url = self.cfg.external_url.as_ref().map(|e| format!("{}{}", e.trim_end_matches('/'), path));
        if self.cfg.auto_register && url.is_none() {
            return Err(CamelError::illegal_argument(
                "webhook: webhookExternalUrl is required to register the webhook (or set webhookAutoRegister=false)",
            ));
        }
        Ok(Arc::new(WebhookConsumer { delegate, http: http_consumer, url, auto_register: self.cfg.auto_register, registered: Mutex::new(false) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct WebhookConsumer {
    delegate: Arc<dyn Endpoint>,
    http: Arc<dyn Consumer>,
    url: Option<String>,
    auto_register: bool,
    registered: Mutex<bool>,
}

impl Service for WebhookConsumer {
    fn start(&self) -> Result<()> {
        self.http.start()?;
        if self.auto_register {
            let wc = as_webhook_capable(self.delegate.as_ref()).unwrap();
            if let Err(e) = wc.register_webhook(self.url.as_deref().unwrap()) {
                let _ = self.http.stop();
                return Err(e);
            }
            *self.registered.lock().unwrap() = true;
        }
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        let mut reg = self.registered.lock().unwrap();
        if *reg {
            let wc = as_webhook_capable(self.delegate.as_ref()).unwrap();
            if let Err(e) = wc.unregister_webhook(self.url.as_deref().unwrap_or("")) {
                camel_core::camel_warn!("webhook", "Failed to unregister webhook: {}", e.message);
            }
            *reg = false;
        }
        self.http.stop()
    }
}
