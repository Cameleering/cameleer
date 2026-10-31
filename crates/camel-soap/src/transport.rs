//! HTTP plumbing shared by the SOAP / REST components: consumer addresses bound on
//! camel-http's shared server registry, and a client for producers.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::context::CamelContext;
use camel_core::error::{kinds, CamelError, Result};
use camel_core::util::Parameters;
use camel_http::client::parse_authority;
use camel_http::registry::{registry, HttpRouteSpec, Registration, RouteHandler};
use camel_http::{HttpClient, HttpHeaders, HttpResponse, ServerConfig};

/// Where a consumer listens.
#[derive(Clone)]
pub struct ListenAddress {
    pub host: String,
    pub port: u16,
    pub path: String,
    pub server_config: ServerConfig,
}

impl ListenAddress {
    /// Parse `http(s)://host:port/path` or a relative `/path` (host `camel.server.host` or
    /// `0.0.0.0`, port `camel.component.<scheme>.port`, `camel.server.port` or 8080).
    /// Consumes `sslContextParameters`, `useGlobalSslContextParameters`, `clientAuth`.
    pub fn parse(ctx: &CamelContext, scheme: &str, address: &str, params: &mut Parameters) -> Result<ListenAddress> {
        let ssl = camel_tls::camel_support::take_endpoint_ssl(ctx, params, scheme)?;
        let client_auth = camel_tls::camel_support::take_client_auth(params)?;
        let (https, rest) = match (address.strip_prefix("http://"), address.strip_prefix("https://")) {
            (Some(r), _) => (Some(false), r),
            (_, Some(r)) => (Some(true), r),
            _ => (None, address),
        };
        let (host, port, path) = match https {
            Some(tls) => {
                let split = rest.find('/').unwrap_or(rest.len());
                let (h, p) = parse_authority(&rest[..split], if tls { 443 } else { 80 }).map_err(CamelError::illegal_argument)?;
                (h, p, rest[split..].to_string())
            }
            None => {
                let mut port = 8080u16;
                for key in [format!("camel.component.{}.port", scheme), "camel.server.port".to_string()] {
                    if let Some(v) = ctx.properties().get(&key) {
                        port = v.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid port in {}: {}", key, v)))?;
                        break;
                    }
                }
                (ctx.properties().get("camel.server.host").unwrap_or_else(|| "0.0.0.0".into()), port, rest.to_string())
            }
        };
        let mut server_config = ServerConfig::default();
        if https == Some(true) || ssl.is_some() {
            let tls = camel_tls::camel_support::server_config_with_client_auth(ctx, ssl.as_ref(), address, client_auth)?;
            server_config.tls = Some(Arc::new(tls));
        }
        let path = if path.is_empty() { "/".to_string() } else if path.starts_with('/') { path } else { format!("/{}", path) };
        Ok(ListenAddress { host, port, path, server_config })
    }
}

/// A started registration on the shared HTTP server registry.
#[derive(Default)]
pub struct Binding {
    registration: Mutex<Option<Registration>>,
    /// Actual port while started.
    pub bound_port: Arc<Mutex<Option<u16>>>,
}

impl Binding {
    /// Register `handler` for `addr` (prefix match when `prefix`).
    pub fn start(&self, addr: &ListenAddress, prefix: bool, handler: RouteHandler) -> Result<()> {
        let mut reg = self.registration.lock().unwrap();
        if reg.is_some() {
            return Ok(());
        }
        let spec = HttpRouteSpec { path: addr.path.clone(), methods: None, match_on_uri_prefix: prefix, consumes: None, produces: None };
        let r = registry().register(&addr.host, addr.port, &addr.server_config, spec, handler)?;
        *self.bound_port.lock().unwrap() = Some(r.port);
        *reg = Some(r);
        Ok(())
    }

    /// Unregister.
    pub fn stop(&self) {
        if let Some(r) = self.registration.lock().unwrap().take() {
            registry().unregister(&r);
        }
        *self.bound_port.lock().unwrap() = None;
    }

    /// The bound port.
    pub fn port(&self) -> Option<u16> {
        *self.bound_port.lock().unwrap()
    }
}

impl Drop for Binding {
    fn drop(&mut self) {
        self.stop();
    }
}

/// An HTTP client for producers (TLS from `sslContextParameters`, timeouts, basic auth).
#[derive(Clone)]
pub struct Client {
    pub http: HttpClient,
    pub basic: Option<(String, String)>,
    pub timeout: Duration,
}

impl Client {
    /// Build from endpoint options: `sslContextParameters`, `receiveTimeout` /
    /// `connectionTimeout` (ms), `username` / `password` (HTTP basic auth).
    pub fn from_params(ctx: &CamelContext, scheme: &str, params: &mut Parameters) -> Result<Client> {
        let ssl = camel_tls::camel_support::take_endpoint_ssl(ctx, params, scheme)?;
        let mut http = HttpClient::new();
        if ssl.is_some() {
            http = http.with_tls(camel_tls::camel_support::client_config(ctx, ssl.as_ref())?);
        }
        let timeout = Duration::from_millis(params.take_u64("receiveTimeout", 60_000)?);
        http.connect_timeout = Duration::from_millis(params.take_u64("connectionTimeout", 30_000)?);
        http.read_timeout = timeout;
        let basic = match (params.take("username"), params.take("password")) {
            (Some(u), p) => Some((u, p.unwrap_or_default())),
            _ => None,
        };
        Ok(Client { http, basic, timeout })
    }

    /// Send a request.
    pub fn send(&self, method: &str, url: &str, headers: &[(String, String)], body: Option<&[u8]>) -> Result<HttpResponse> {
        let u = camel_http::Url::parse(url)?;
        let mut h = HttpHeaders::new();
        for (k, v) in headers {
            h.set(k.clone(), v.clone());
        }
        if let Some((user, pw)) = &self.basic {
            h.set("Authorization", format!("Basic {}", camel_tls::pem::base64_encode(format!("{}:{}", user, pw).as_bytes())));
        }
        self.http.execute(method, &u, &h, body, Some(self.timeout))
    }
}

/// An `HttpOperationFailedException` for an unexpected response.
pub fn http_failed(url: &str, resp: &HttpResponse) -> CamelError {
    CamelError::new(kinds::HTTP_OPERATION_FAILED, format!("HTTP operation failed invoking {} with statusCode: {}, body: {}", url, resp.status, resp.body_text()))
}
