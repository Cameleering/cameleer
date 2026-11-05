//! HTTP consumer components: `platform-http`, `jetty`, `undertow`, `netty-http`.
//!
//! - `platform-http:/path?httpMethodRestrict=GET,POST&matchOnUriPrefix=true` binds the
//!   path on the platform server; the port comes from the endpoint option `port`, the
//!   component (`HttpConsumerComponent::with_port`), the context properties
//!   `camel.component.platform-http.port` / `camel.server.port`, or defaults to 8080.
//!   The host comes from `camel.server.host` (default `0.0.0.0`).
//! - `jetty:http://0.0.0.0:8081/path` (also `undertow:`, `netty-http:`) take host and port
//!   from the uri.
//!
//! HTTPS: `jetty:https://0.0.0.0:8443/path` (also `undertow:`/`netty-http:`) and
//! `platform-http` endpoints with `ssl=true` or `sslContextParameters=#bean` serve TLS. The
//! server certificate comes from `sslContextParameters` (endpoint option or
//! `camel.component.<scheme>.sslContextParameters`), else the embedded server properties
//! `camel.server.ssl.keyStore` / `certFile` + `keyFile` (or the global `camel.ssl.*`
//! parameters with `camel.server.useGlobalSslContextParameters=true`). Setting
//! `camel.server.sslEnabled=true` makes every `platform-http` endpoint use TLS.
//!
//! Port `0` picks a free port; use [`crate::local_port`] to discover it. For
//! `platform-http`, all endpoints of a component with port 0 share the same ephemeral
//! port (like a single platform server).
//!
//! Endpoint options: `httpMethodRestrict`, `matchOnUriPrefix`, `consumes`, `produces`,
//! `muteException`, `chunked`, `mapHttpMessageBody`, `mapHttpMessageHeaders`,
//! `mapHttpMessageFormUrlEncodedBody`, `port`, `host`, `ssl`, `sslContextParameters`,
//! `useGlobalSslContextParameters` (plus a few accepted and ignored
//! Camel options such as `transferException`, `useStreaming`). `disableStreamCache=true`: request
//! bodies are not read into memory but passed as `cameleer_tls::stream_body` handles (the
//! connection closes after such a request); stream-handle response bodies are always sent with
//! chunked transfer coding without buffering.

use std::any::Any;
use std::sync::{Arc, Mutex};

use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Service};
use cameleer_core::util::Parameters;

use crate::binding::ConsumerBinding;
use crate::client::parse_authority;
use crate::http::{HttpRequest, HttpResponse};
use crate::registry::{registry, HttpRouteSpec, Registration, RouteHandler};
use crate::server::ServerConfig;

/// Default platform HTTP port.
pub const DEFAULT_PORT: u16 = 8080;

/// Consumer component for `platform-http` and its aliases.
pub struct HttpConsumerComponent {
    scheme: String,
    port: Mutex<Option<u16>>,
    host: Mutex<Option<String>>,
    server_config: Mutex<ServerConfig>,
    /// Ephemeral port shared by endpoints of this component that requested port 0.
    shared_port: Arc<Mutex<Option<u16>>>,
}

impl HttpConsumerComponent {
    /// Create a component registered under `scheme` (used to look up
    /// `camel.component.<scheme>.port`).
    pub fn new(scheme: &str) -> Self {
        HttpConsumerComponent {
            scheme: scheme.to_string(),
            port: Mutex::new(None),
            host: Mutex::new(None),
            server_config: Mutex::new(ServerConfig::default()),
            shared_port: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the default port (0 = pick a free port).
    pub fn with_port(self, port: u16) -> Self {
        *self.port.lock().unwrap() = Some(port);
        self
    }

    /// Set the default bind host.
    pub fn with_host(self, host: &str) -> Self {
        *self.host.lock().unwrap() = Some(host.to_string());
        self
    }

    /// Server settings used when this component binds a new server.
    pub fn with_server_config(self, cfg: ServerConfig) -> Self {
        *self.server_config.lock().unwrap() = cfg;
        self
    }

    /// The ephemeral port picked for port 0 endpoints, if any server was bound.
    pub fn shared_port(&self) -> Option<u16> {
        *self.shared_port.lock().unwrap()
    }

    fn default_port(&self, ctx: &CamelContext) -> Result<u16> {
        if let Some(p) = *self.port.lock().unwrap() {
            return Ok(p);
        }
        for key in [format!("camel.component.{}.port", self.scheme), "camel.server.port".to_string()] {
            if let Some(v) = ctx.properties().get(&key) {
                return v.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid port in property {}: {}", key, v)));
            }
        }
        Ok(DEFAULT_PORT)
    }

    fn default_host(&self, ctx: &CamelContext) -> String {
        if let Some(h) = self.host.lock().unwrap().clone() {
            return h;
        }
        ctx.properties().get("camel.server.host").unwrap_or_else(|| "0.0.0.0".to_string())
    }
}

impl Component for HttpConsumerComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (uri_scheme, rest) = match (remaining.strip_prefix("http://"), remaining.strip_prefix("https://")) {
            (Some(r), _) => (Some(false), r),
            (_, Some(r)) => (Some(true), r),
            _ => (None, remaining),
        };
        let (host, port, path, from_uri) = if let Some(https) = uri_scheme {
            let split = rest.find('/').unwrap_or(rest.len());
            let (h, p) = parse_authority(&rest[..split], if https { 443 } else { 80 }).map_err(CamelError::illegal_argument)?;
            (h, p, rest[split..].to_string(), true)
        } else {
            (self.default_host(ctx), self.default_port(ctx)?, remaining.to_string(), false)
        };
        // TLS: `jetty:https://...`, or for platform-http `ssl=true`, `sslContextParameters=#bean`
        // or the embedded server properties (`camel.server.ssl.*`, `camel.server.sslEnabled`)
        let ssl = cameleer_tls::camel_support::take_endpoint_ssl(ctx, params, &self.scheme)?;
        let ssl_flag = params.take_bool("ssl", false)?;
        // mutual TLS: needClientAuth=true / clientAuth=NONE|WANT|REQUIRE
        let client_auth = cameleer_tls::camel_support::take_client_auth(params)?;
        let use_tls = match uri_scheme {
            Some(https) => https,
            None => ssl_flag || ssl.is_some() || cameleer_tls::camel_support::server_ssl_enabled(ctx),
        };
        let mut server_config = self.server_config.lock().unwrap().clone();
        if use_tls {
            let tls = cameleer_tls::camel_support::server_config_with_client_auth(ctx, ssl.as_ref(), uri, client_auth)?;
            server_config.tls = Some(Arc::new(tls));
        }
        let host = params.take("host").unwrap_or(host);
        let explicit_port = params.take("port");
        let (port, share) = match explicit_port {
            Some(p) => (p.trim().parse::<u16>().map_err(|_| CamelError::illegal_argument(format!("Invalid port: {}", p)))?, true),
            None => (port, !from_uri),
        };
        let path = if path.starts_with('/') { path } else { format!("/{}", path) };
        let methods = params.take("httpMethodRestrict").map(|m| {
            m.split(',').map(|s| s.trim().to_ascii_uppercase()).filter(|s| !s.is_empty()).collect::<Vec<_>>()
        });
        let spec = HttpRouteSpec {
            path,
            methods,
            match_on_uri_prefix: params.take_bool("matchOnUriPrefix", false)?,
            consumes: params.take("consumes").filter(|s| !s.is_empty()),
            produces: params.take("produces").filter(|s| !s.is_empty()),
        };
        let binding = ConsumerBinding {
            map_http_message_headers: params.take_bool("mapHttpMessageHeaders", true)?,
            map_http_message_body: params.take_bool("mapHttpMessageBody", true)?,
            map_form_url_encoded_body: params.take_bool("mapHttpMessageFormUrlEncodedBody", true)?,
            mute_exception: params.take_bool("muteException", false)?,
            chunked: params.take_bool("chunked", false)?,
            produces: spec.produces.clone(),
            disable_stream_cache: params.take_bool("disableStreamCache", false)?,
        };
        for ignored in [
            "transferException",
            "useStreaming",
            "headerFilterStrategy",
            "fileNameExtWhitelist",
            "useCookieHandler",
            "returnHttpRequestHeaders",
            "httpBinding",
            "bridgeErrorHandler",
            "exchangePattern",
            "sessionSupport",
            "handlers",
        ] {
            params.take(ignored);
        }
        let shared_port = if port == 0 && share { Some(self.shared_port.clone()) } else { None };
        Ok(Arc::new(HttpConsumerEndpoint {
            uri: uri.to_string(),
            host,
            port,
            spec,
            binding,
            ctx: ctx.downgrade(),
            server_config,
            shared_port,
            bound_port: Arc::new(Mutex::new(None)),
        }))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An HTTP consumer endpoint.
pub struct HttpConsumerEndpoint {
    uri: String,
    host: String,
    port: u16,
    spec: HttpRouteSpec,
    binding: ConsumerBinding,
    ctx: WeakContext,
    server_config: ServerConfig,
    shared_port: Option<Arc<Mutex<Option<u16>>>>,
    bound_port: Arc<Mutex<Option<u16>>>,
}

impl HttpConsumerEndpoint {
    /// The actual port while a consumer is started.
    pub fn local_port(&self) -> Option<u16> {
        *self.bound_port.lock().unwrap()
    }
    /// The bound path (template).
    pub fn path(&self) -> &str {
        &self.spec.path
    }
    /// The configured port (0 = ephemeral).
    pub fn configured_port(&self) -> u16 {
        self.port
    }
    /// Whether the endpoint serves HTTPS.
    pub fn is_tls(&self) -> bool {
        self.server_config.tls.is_some()
    }
}

impl Endpoint for HttpConsumerEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(HttpConsumer {
            host: self.host.clone(),
            port: self.port,
            spec: self.spec.clone(),
            binding: self.binding.clone(),
            ctx: self.ctx.clone(),
            server_config: self.server_config.clone(),
            shared_port: self.shared_port.clone(),
            bound_port: self.bound_port.clone(),
            processor,
            registration: Mutex::new(None),
            suspend: cameleer_core::component::support::SuspendState::new(),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct HttpConsumer {
    host: String,
    port: u16,
    spec: HttpRouteSpec,
    binding: ConsumerBinding,
    ctx: WeakContext,
    server_config: ServerConfig,
    shared_port: Option<Arc<Mutex<Option<u16>>>>,
    bound_port: Arc<Mutex<Option<u16>>>,
    processor: Arc<dyn Processor>,
    registration: Mutex<Option<Registration>>,
    /// Suspended consumers answer 503 Service Unavailable (as Camel's HTTP consumers).
    suspend: Arc<cameleer_core::component::support::SuspendState>,
}

/// Process a request through a route processor and build the response.
pub(crate) fn process_request(
    ctx: &WeakContext,
    processor: &Arc<dyn Processor>,
    binding: &ConsumerBinding,
    req: &mut HttpRequest,
    params: &[(String, String)],
) -> HttpResponse {
    let ctx = match ctx.upgrade() {
        Some(c) => c,
        None => return HttpResponse::text(503, "Service Unavailable"),
    };
    let mut ex = Exchange::new(Some(ctx));
    binding.read_request(req, params, &mut ex);
    if let Err(e) = processor.process(&mut ex) {
        ex.set_exception(e);
    }
    binding.write_response(&ex)
}

impl Service for HttpConsumer {
    fn start(&self) -> Result<()> {
        let mut reg = self.registration.lock().unwrap();
        if reg.is_some() {
            return Ok(());
        }
        let ctx = self.ctx.clone();
        let processor = self.processor.clone();
        let binding = self.binding.clone();
        let suspend = self.suspend.clone();
        let handler: RouteHandler = Arc::new(move |req: &mut HttpRequest, params: Vec<(String, String)>| {
            if suspend.get() {
                return HttpResponse::text(503, "Service Unavailable");
            }
            process_request(&ctx, &processor, &binding, req, &params)
        });
        let registration = match &self.shared_port {
            Some(shared) => {
                let mut sp = shared.lock().unwrap();
                let first = sp.map(|p| registry().register(&self.host, p, &self.server_config, self.spec.clone(), handler.clone()));
                let r = match first {
                    Some(Ok(r)) => r,
                    Some(Err(e)) if e.message.contains("already registered") => return Err(e),
                    _ => registry().register(&self.host, 0, &self.server_config, self.spec.clone(), handler)?,
                };
                *sp = Some(r.port);
                r
            }
            None => registry().register(&self.host, self.port, &self.server_config, self.spec.clone(), handler)?,
        };
        if self.binding.disable_stream_cache {
            registry().set_streaming(&registration, true);
        }
        *self.bound_port.lock().unwrap() = Some(registration.port);
        *reg = Some(registration);
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        if let Some(r) = self.registration.lock().unwrap().take() {
            registry().unregister(&r);
        }
        *self.bound_port.lock().unwrap() = None;
        self.suspend.set(false);
        Ok(())
    }
    fn as_suspendable(&self) -> Option<&dyn cameleer_core::spi::SuspendableService> {
        Some(&*self.suspend)
    }
}

impl Drop for HttpConsumer {
    fn drop(&mut self) {
        if let Some(r) = self.registration.lock().unwrap().take() {
            registry().unregister(&r);
        }
    }
}
