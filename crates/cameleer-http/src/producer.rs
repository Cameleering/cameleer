//! `http` / `https` producer component.
//!
//! `http://host:port/path?httpMethod=POST&throwExceptionOnFailure=true&...`
//! `https://host:8443/path?sslContextParameters=#ssl`
//!
//! HTTPS uses `cameleer-tls` with SNI and hostname verification. Trust/key material comes
//! from `sslContextParameters=#bean` (or `camel.component.https.sslContextParameters`),
//! `useGlobalSslContextParameters=true`, the global `camel.ssl.*` parameters when
//! `camel.ssl.enabled=true`, else the system trust store. Proxies: `proxyHost`/`proxyPort`
//! (+ `proxyAuthUsername`/`proxyAuthPassword`), or `useSystemProperties=true` to use
//! `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY`; `https` targets are reached through a `CONNECT` tunnel.
//!
//! Supported options: `httpMethod`, `bridgeEndpoint`, `throwExceptionOnFailure`,
//! `okStatusCodeRange`, `connectTimeout`, `socketTimeout`/`soTimeout`/`responseTimeout`,
//! `followRedirects`, `copyHeaders`, `skipRequestHeaders`, `skipResponseHeaders`,
//! `userAgent`, `authUsername`/`authPassword`/`authMethod` (Basic), `chunked`,
//! `getWithBody`, `deleteWithBody`, `ignoreResponseBody`, `customHostHeader`,
//! `preserveHostHeader` (and some accepted-and-ignored options). Any other query
//! parameter is forwarded to the target URL.
//!
//! Connections: HTTP/1.1 keep-alive connections are pooled per producer and target
//! (`connectionsPerRoute` idle connections, default 20; `connectionClose=true` disables
//! pooling; stale pooled connections are retried once). `httpVersion=HTTP/2` (or `2`, `h2`)
//! uses HTTP/2 through cameleer-grpc's client (`https`: TLS with ALPN `h2`; `http`: h2c prior
//! knowledge), one multiplexed connection per target.
//!
//! Content coding: unless `contentCompressionDisabled=true`, requests advertise
//! `Accept-Encoding: gzip, deflate` and `gzip` / `deflate` (zlib or raw) response bodies are
//! decoded (`Content-Encoding` is removed); a request whose `Content-Encoding` header is
//! `gzip` or `deflate` has its body compressed accordingly.
//!
//! Message mapping: the method comes from header `CamelHttpMethod`, else the
//! `httpMethod` option, else `POST` when there is a body and `GET` otherwise. Headers
//! `CamelHttpUri` (ignored with `bridgeEndpoint`), `CamelHttpPath` (appended to the
//! endpoint path) and `CamelHttpQuery`/`CamelHttpRawQuery` (replace the query) override the
//! target. Non-Camel headers are sent as request headers. The response body becomes the
//! message body; `CamelHttpResponseCode`, `CamelHttpResponseText` and the response headers
//! are set. Responses outside `okStatusCodeRange` fail with `HttpOperationFailedException`
//! when `throwExceptionOnFailure=true`; see [`HttpOperationFailed`].

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::io::{Read, Write};
use std::time::Duration;

use cameleer_core::context::CamelContext;
use cameleer_core::error::{kinds, CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::{ObjectMap, Value};

use crate::binding::{add_header_value, body_to_value, headers, value_to_body, write_message_headers};
use crate::client::{HttpClient, HttpConnection, ProxyAddr, ProxyConfig, Url};
use crate::http::HttpResponse;
use cameleer_grpc::h2::{H2Config, H2Connection, H2Request};
use std::collections::HashMap;
use std::sync::Mutex;
use cameleer_tls::TlsClientConfig;
use crate::http::{base64_encode, percent_encode, HttpHeaders};

/// The `http` / `https` component.
pub struct HttpComponent {
    scheme: String,
}

impl HttpComponent {
    pub fn new(scheme: &str) -> Self {
        HttpComponent { scheme: scheme.to_string() }
    }
}

/// Endpoint options of the `http` producer.
#[derive(Clone, Debug)]
pub struct HttpProducerOptions {
    pub http_method: Option<String>,
    pub bridge_endpoint: bool,
    pub throw_exception_on_failure: bool,
    pub ok_status_code_range: Vec<(u16, u16)>,
    pub connect_timeout: Duration,
    pub socket_timeout: Duration,
    pub follow_redirects: bool,
    pub copy_headers: bool,
    pub skip_request_headers: bool,
    pub skip_response_headers: bool,
    pub user_agent: Option<String>,
    pub auth: Option<(String, String)>,
    pub chunked: bool,
    pub get_with_body: bool,
    pub delete_with_body: bool,
    pub ignore_response_body: bool,
    /// `disableStreamCache`: the response body is a `cameleer_tls::stream_body` handle reading
    /// the connection lazily instead of a buffered byte array.
    pub disable_stream_cache: bool,
    pub custom_host_header: Option<String>,
    pub preserve_host_header: bool,
    /// TLS configuration for `https` targets (None = system trust store).
    pub tls: Option<Arc<TlsClientConfig>>,
    /// TLS server name override (`SslContextParameters::server_name`).
    pub tls_server_name: Option<String>,
    pub proxy: Option<ProxyConfig>,
    /// `httpVersion=HTTP/2`.
    pub http2: bool,
    pub content_compression: bool,
    /// Pool keep-alive connections (`connectionClose=false`).
    pub pooling: bool,
    pub connections_per_route: usize,
}

fn parse_ranges(s: &str) -> Result<Vec<(u16, u16)>> {
    s.split(',')
        .map(|r| {
            let r = r.trim();
            let (a, b) = r.split_once('-').unwrap_or((r, r));
            let a: u16 = a.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid okStatusCodeRange: {}", s)))?;
            let b: u16 = b.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid okStatusCodeRange: {}", s)))?;
            Ok((a, b))
        })
        .collect()
}

impl Component for HttpComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let https = self.scheme.eq_ignore_ascii_case("https") || remaining.starts_with("https://");
        let remaining = remaining.strip_prefix("http://").or_else(|| remaining.strip_prefix("https://")).unwrap_or(remaining);
        let mut base = Url::parse(&format!("{}://{}", if https { "https" } else { "http" }, remaining))?;
        let ssl = cameleer_tls::camel_support::take_endpoint_ssl(ctx, params, &self.scheme)?;
        let ssl = match ssl {
            Some(p) => Some(p),
            None => cameleer_tls::camel_support::global_ssl_context_parameters(ctx)?,
        };
        let tls = match &ssl {
            Some(p) => Some(Arc::new(p.client_config().map_err(|e| cameleer_tls::camel_support::tls_error(&e, uri))?)),
            None => None,
        };
        let tls_server_name = ssl.as_ref().and_then(|p| p.server_name.clone());
        let mut proxy = None;
        if let Some(h) = params.take("proxyHost").or_else(|| params.take("proxyAuthHost")) {
            let port = params.take("proxyPort").or_else(|| params.take("proxyAuthPort")).unwrap_or_else(|| "80".into());
            let port: u16 = port.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid proxyPort: {}", port)))?;
            let credentials = params.take("proxyAuthUsername").map(|u| format!("{}:{}", u, params.take("proxyAuthPassword").unwrap_or_default()));
            proxy = Some(ProxyConfig::new(ProxyAddr { host: h, port, credentials }));
        }
        params.take("proxyAuthScheme");
        params.take("proxyAuthMethod");
        if params.take_bool("useSystemProperties", false)? && proxy.is_none() {
            proxy = ProxyConfig::from_env();
        }
        let socket_timeout = match params.take("socketTimeout").or_else(|| params.take("soTimeout")).or_else(|| params.take("responseTimeout")) {
            Some(v) => cameleer_core::util::parse_duration(&v)?,
            None => Duration::from_secs(60),
        };
        let auth_user = params.take("authUsername");
        let auth_pass = params.take("authPassword").unwrap_or_default();
        if let Some(m) = params.take("authMethod") {
            if !m.eq_ignore_ascii_case("basic") {
                return Err(CamelError::illegal_argument(format!("Unsupported authMethod: {} (only Basic is supported)", m)));
            }
        }
        let opts = HttpProducerOptions {
            http_method: params.take("httpMethod").map(|m| m.to_ascii_uppercase()),
            bridge_endpoint: params.take_bool("bridgeEndpoint", false)?,
            throw_exception_on_failure: params.take_bool("throwExceptionOnFailure", true)?,
            ok_status_code_range: parse_ranges(&params.take_string("okStatusCodeRange", "200-299"))?,
            connect_timeout: params.take_duration("connectTimeout", Duration::from_secs(30))?,
            socket_timeout,
            follow_redirects: params.take_bool("followRedirects", false)?,
            copy_headers: params.take_bool("copyHeaders", true)?,
            skip_request_headers: params.take_bool("skipRequestHeaders", false)?,
            skip_response_headers: params.take_bool("skipResponseHeaders", false)?,
            user_agent: params.take("userAgent"),
            auth: auth_user.map(|u| (u, auth_pass)),
            chunked: params.take_bool("chunked", false)?,
            get_with_body: params.take_bool("getWithBody", false)?,
            delete_with_body: params.take_bool("deleteWithBody", false)?,
            ignore_response_body: params.take_bool("ignoreResponseBody", false)?,
            custom_host_header: params.take("customHostHeader"),
            preserve_host_header: params.take_bool("preserveHostHeader", false)?,
            tls,
            tls_server_name,
            proxy,
            http2: match params.take("httpVersion").or_else(|| params.take("protocolVersion")) {
                None => false,
                Some(v) => match v.trim().to_ascii_uppercase().trim_start_matches("HTTP/") {
                    "2" | "2.0" | "H2" | "H2C" => true,
                    "1.1" | "1.0" | "1" => false,
                    other => return Err(CamelError::illegal_argument(format!("Unsupported httpVersion: {} (HTTP/1.1 or HTTP/2)", other))),
                },
            },
            content_compression: !params.take_bool("contentCompressionDisabled", false)?,
            pooling: !params.take_bool("connectionClose", false)?,
            disable_stream_cache: params.take_bool("disableStreamCache", false)?,
            connections_per_route: params.take_usize("connectionsPerRoute", 20)?,
        };
        for ignored in [
            "connectionRequestTimeout",
            "headerFilterStrategy",
            "httpClientConfigurer",
            "clientConnectionManager",
            "maxTotalConnections",
            "cookieHandler",
            "clearExpiredCookies",
            "transferException",
            "lazyStartProducer",
            "httpBinding",
            "x509HostnameVerifier",
        ] {
            params.take(ignored);
        }
        // forward unknown parameters as query parameters
        let rest: BTreeMap<String, String> = params.take_all();
        if !rest.is_empty() {
            let extra = rest.iter().map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v))).collect::<Vec<_>>().join("&");
            base.query = Some(match base.query.take() {
                Some(q) if !q.is_empty() => format!("{}&{}", q, extra),
                _ => extra,
            });
        }
        Ok(Arc::new(HttpEndpoint { uri: uri.to_string(), base, opts }))
    }

    fn lenient_properties(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An `http` producer endpoint.
pub struct HttpEndpoint {
    uri: String,
    base: Url,
    opts: HttpProducerOptions,
}

impl HttpEndpoint {
    /// The target URL (including forwarded query parameters).
    pub fn target(&self) -> &Url {
        &self.base
    }
    pub fn options(&self) -> &HttpProducerOptions {
        &self.opts
    }
}

impl Endpoint for HttpEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(HttpProducer { base: self.base.clone(), opts: self.opts.clone(), pool: Mutex::new(HashMap::new()), h2: Mutex::new(HashMap::new()) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Sends exchanges as HTTP requests.
pub struct HttpProducer {
    base: Url,
    opts: HttpProducerOptions,
    pool: Mutex<HashMap<String, Vec<HttpConnection>>>,
    h2: Mutex<HashMap<String, H2Connection>>,
}

impl Service for HttpProducer {
    fn stop(&self) -> Result<()> {
        self.pool.lock().unwrap().clear();
        for (_, c) in self.h2.lock().unwrap().drain() {
            c.close();
        }
        Ok(())
    }
}

/// Decodes a `gzip` / `deflate` body (deflate: zlib, or raw DEFLATE as some servers send).
pub fn decode_content(encoding: &str, body: &[u8]) -> Result<Vec<u8>> {
    use cameleer_core::dataformat::compress::{gzip_decompress, inflate_raw, zlib_decompress};
    match encoding.trim().to_ascii_lowercase().as_str() {
        "gzip" | "x-gzip" => gzip_decompress(body),
        "deflate" => zlib_decompress(body).or_else(|_| inflate_raw(body).map(|(d, _)| d)),
        "identity" | "" => Ok(body.to_vec()),
        other => Err(CamelError::io(format!("Unsupported Content-Encoding: {}", other))),
    }
}

impl HttpProducer {
    fn client(&self) -> HttpClient {
        HttpClient {
            connect_timeout: self.opts.connect_timeout,
            read_timeout: self.opts.socket_timeout,
            follow_redirects: self.opts.follow_redirects,
            chunked_requests: self.opts.chunked,
            tls: self.opts.tls.clone(),
            tls_server_name: self.opts.tls_server_name.clone(),
            proxy: self.opts.proxy.clone(),
            ..HttpClient::default()
        }
    }

    fn send(&self, method: &str, url: &Url, headers: &HttpHeaders, body: Option<&[u8]>) -> Result<HttpResponse> {
        if self.opts.http2 {
            return self.send_h2(method, url, headers, body);
        }
        let proxied = self.opts.proxy.as_ref().and_then(|p| p.proxy_for(url)).is_some();
        if !self.opts.pooling || proxied || self.opts.tls_server_name.is_some() || self.opts.follow_redirects || self.opts.chunked {
            return self.client().execute(method, url, headers, body, None);
        }
        let key = format!("{}://{}", url.scheme, url.authority());
        let mut h: Vec<(String, String)> = headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("Connection")).map(|(k, v)| (k.to_string(), v.to_string())).collect();
        if !headers.contains("Host") {
            h.push(("Host".into(), url.host_header()));
        }
        let hr: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let target = url.request_target();
        for attempt in 0..2 {
            let pooled = if attempt == 0 { self.pool.lock().unwrap().get_mut(&key).and_then(|v| v.pop()) } else { None };
            let reused = pooled.is_some();
            let mut conn = match pooled {
                Some(c) => c,
                None => {
                    let tls = if url.is_https() { Some(self.opts.tls.clone().unwrap_or_else(crate::client::default_tls_config)) } else { None };
                    HttpConnection::connect_with(&url.host, url.port, tls.as_deref(), self.opts.socket_timeout.max(self.opts.connect_timeout))?
                }
            };
            match conn.send(method, &target, &hr, body) {
                Ok(resp) => {
                    if !conn.is_closed() {
                        let mut g = self.pool.lock().unwrap();
                        let v = g.entry(key.clone()).or_default();
                        if v.len() < self.opts.connections_per_route.max(1) {
                            v.push(conn);
                        }
                    }
                    return Ok(resp);
                }
                // a pooled connection the server closed meanwhile: retry on a new one
                Err(_) if reused => continue,
                Err(e) => return Err(e),
            }
        }
        Err(CamelError::io(format!("Cannot send request to {}", url)))
    }

    /// One request on a dedicated connection: a streamed (chunked) request body when `reader`
    /// is given, and a lazy response body reader.
    fn send_streaming(
        &self,
        method: &str,
        url: &Url,
        headers: &HttpHeaders,
        reader: Option<Box<dyn std::io::Read + Send>>,
        body: Option<&[u8]>,
    ) -> Result<(HttpResponse, crate::http::BodyReader<std::io::BufReader<cameleer_tls::net::NetStream>>)> {
        let what = url.to_string();
        let proxy = self.opts.proxy.as_ref().and_then(|p| p.proxy_for(url));
        let t = if self.opts.socket_timeout.is_zero() { None } else { Some(self.opts.socket_timeout) };
        let (stream, absolute) = crate::client::open_stream(url, self.opts.tls.as_deref(), self.opts.tls_server_name.as_deref(), proxy, self.opts.connect_timeout, t)?;
        let mut h = headers.clone();
        if !h.contains("Host") {
            h.set("Host", url.host_header());
        }
        h.set("Connection", "close");
        let target = if absolute {
            if let Some(c) = proxy.and_then(|p| p.credentials.as_ref()) {
                h.set("Proxy-Authorization", format!("Basic {}", base64_encode(c.as_bytes())));
            }
            url.to_string()
        } else {
            url.request_target()
        };
        let mut w = stream.try_clone().map_err(|e| crate::client::map_io_error(e, &what))?;
        match reader {
            Some(mut r) => {
                crate::http::write_request_head_chunked(&mut w, method, &target, &h).map_err(|e| crate::client::map_io_error(e, &what))?;
                crate::http::write_chunked_stream(&mut w, &mut r).map_err(|e| crate::client::map_io_error(e, &what))?;
            }
            None => {
                let mut buf = Vec::new();
                crate::http::write_request(&mut buf, method, &target, &h, body, self.opts.chunked && body.is_some()).map_err(|e| crate::client::map_io_error(e, &what))?;
                w.write_all(&buf).map_err(|e| crate::client::map_io_error(e, &what))?;
            }
        }
        crate::http::read_response_streaming(std::io::BufReader::new(stream), method, &crate::http::Limits::default())
            .map_err(|e| CamelError::io(format!("Invalid HTTP response from {}: {}", what, e)))
    }

    fn send_h2(&self, method: &str, url: &Url, headers: &HttpHeaders, body: Option<&[u8]>) -> Result<HttpResponse> {
        let key = format!("{}://{}", url.scheme, url.authority());
        let what = url.to_string();
        let conn = {
            let mut g = self.h2.lock().unwrap();
            match g.get(&key).filter(|c| !c.is_closed()) {
                Some(c) => c.clone(),
                None => {
                    let cfg = H2Config::default();
                    let addr = url.authority();
                    let c = if url.is_https() {
                        let tls = self.opts.tls.clone().unwrap_or_else(crate::client::default_tls_config).with_alpn(&["h2"]);
                        H2Connection::connect_tls(&addr, self.opts.tls_server_name.as_deref().unwrap_or(&url.host), &tls, cfg)
                    } else {
                        H2Connection::connect(&addr, cfg)
                    }
                    .map_err(|e| CamelError::new(kinds::CONNECT, format!("HTTP/2 connection to {} failed: {}", addr, e)))?;
                    g.insert(key.clone(), c.clone());
                    c
                }
            }
        };
        let mut req = H2Request { method: method.to_string(), path: url.request_target(), authority: Some(headers.get("Host").map(|s| s.to_string()).unwrap_or_else(|| url.host_header())), headers: Vec::new(), body: body.map(|b| b.to_vec()).unwrap_or_default() };
        for (k, v) in headers.iter() {
            let lower = k.to_ascii_lowercase();
            if matches!(lower.as_str(), "host" | "connection" | "keep-alive" | "proxy-connection" | "transfer-encoding" | "upgrade" | "te") {
                continue;
            }
            req.headers.push((lower, v.to_string()));
        }
        let t = if self.opts.socket_timeout.is_zero() { None } else { Some(self.opts.socket_timeout) };
        let r = conn.request(&req, t).map_err(|e| CamelError::io(format!("HTTP/2 request to {} failed: {}", what, e)))?;
        let mut resp = HttpResponse::new(r.status);
        for (k, v) in r.headers.iter().chain(r.trailers.iter()) {
            resp.headers.add(k.clone(), v.clone());
        }
        resp.body = r.body;
        Ok(resp)
    }
}

impl HttpProducer {
    fn target_url(&self, ex: &Exchange) -> Result<Url> {
        let mut url = self.base.clone();
        if !self.opts.bridge_endpoint {
            if let Some(u) = ex.header(headers::HTTP_URI).map(|v| v.to_string()) {
                let lower = u.to_ascii_lowercase();
                if lower.starts_with("http://") || lower.starts_with("https://") {
                    url = Url::parse(&u)?;
                }
            }
        }
        if let Some(p) = ex.header(headers::HTTP_PATH).map(|v| v.to_string()).filter(|p| !p.is_empty()) {
            let base = url.path.trim_end_matches('/');
            url.path = format!("{}/{}", base, p.trim_start_matches('/'));
        }
        if let Some(q) = ex.header(headers::HTTP_RAW_QUERY).or_else(|| ex.header(headers::HTTP_QUERY)).map(|v| v.to_string()) {
            url.query = if q.is_empty() { None } else { Some(encode_query_string(&q)) };
        }
        Ok(url)
    }
}

/// Make a query string safe for the request line (encode spaces and other unsafe
/// characters, keep `=`/`&` and existing escapes).
fn encode_query_string(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    for b in q.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~=&%+/:,;@!$'()*?".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Details of a failed HTTP operation (Camel `HttpOperationFailedException`).
#[derive(Clone, Debug, PartialEq)]
pub struct HttpOperationFailed {
    pub uri: String,
    pub status_code: u16,
    pub status_text: String,
    pub redirect_location: Option<String>,
    pub response_body: Option<String>,
    pub response_headers: BTreeMap<String, String>,
}

impl HttpOperationFailed {
    /// Read the details stored on an exchange that failed with `HttpOperationFailedException`.
    pub fn from_exchange(ex: &Exchange) -> Option<HttpOperationFailed> {
        let v = ex.property(headers::HTTP_OPERATION_FAILED)?;
        let s = |k: &str| v.get(k).filter(|x| !x.is_null()).map(|x| x.to_string());
        Some(HttpOperationFailed {
            uri: s("uri").unwrap_or_default(),
            status_code: v.get("statusCode").and_then(|x| x.as_i64()).unwrap_or(0) as u16,
            status_text: s("statusText").unwrap_or_default(),
            redirect_location: s("redirectLocation"),
            response_body: s("responseBody"),
            response_headers: v
                .get("responseHeaders")
                .and_then(|h| h.as_object())
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.to_string())).collect())
                .unwrap_or_default(),
        })
    }

    /// The status code encoded in an `HttpOperationFailedException` message.
    pub fn status_code_of(err: &CamelError) -> Option<u16> {
        let e = err.chain().find(|e| e.kind == kinds::HTTP_OPERATION_FAILED)?;
        let i = e.message.find("statusCode: ")? + "statusCode: ".len();
        e.message[i..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
    }

    fn to_value(&self) -> Value {
        let mut m = ObjectMap::new();
        m.insert("uri".to_string(), Value::from(self.uri.clone()));
        m.insert("statusCode".to_string(), Value::Int(self.status_code as i64));
        m.insert("statusText".to_string(), Value::from(self.status_text.clone()));
        m.insert("redirectLocation".to_string(), self.redirect_location.clone().map(Value::from).unwrap_or(Value::Null));
        m.insert("responseBody".to_string(), self.response_body.clone().map(Value::from).unwrap_or(Value::Null));
        m.insert(
            "responseHeaders".to_string(),
            Value::Object(self.response_headers.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect()),
        );
        Value::Object(m)
    }
}

impl Processor for HttpProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let url = self.target_url(ex)?;
        let has_body = !ex.body().is_null();
        let method = ex
            .header(headers::HTTP_METHOD)
            .map(|v| v.to_string().to_ascii_uppercase())
            .filter(|m| !m.is_empty())
            .or_else(|| self.opts.http_method.clone())
            .unwrap_or_else(|| if has_body { "POST".into() } else { "GET".into() });
        let send_body = has_body
            && match method.as_str() {
                "GET" => self.opts.get_with_body,
                "DELETE" => self.opts.delete_with_body,
                "HEAD" | "OPTIONS" | "TRACE" => false,
                _ => true,
            };
        // a stream handle body is sent with chunked transfer coding without buffering
        let mut stream_reader = if send_body { cameleer_tls::stream_body::take(ex.body()) } else { None };
        let (body, default_ct) = match stream_reader.take() {
            Some((mut r, _)) if self.opts.http2 => {
                let mut b = Vec::new();
                r.read_to_end(&mut b).map_err(|e| CamelError::io(format!("Cannot read the stream body: {}", e)))?;
                (b, Some("application/octet-stream"))
            }
            Some(sr) => {
                stream_reader = Some(sr);
                (Vec::new(), Some("application/octet-stream"))
            }
            None => value_to_body(ex.body()),
        };
        let mut req_headers = HttpHeaders::new();
        if self.opts.copy_headers && !self.opts.skip_request_headers {
            write_message_headers(&ex.message.headers, &mut req_headers);
        }
        if send_body {
            let ct = ex.header(headers::CONTENT_TYPE).map(|v| v.to_string()).filter(|s| !s.is_empty()).or_else(|| default_ct.map(String::from));
            if let Some(ct) = ct {
                req_headers.set("Content-Type", ct);
            }
        }
        if let Some(h) = &self.opts.custom_host_header {
            req_headers.set("Host", h.clone());
        } else if self.opts.preserve_host_header {
            if let Some(h) = ex.header("Host").map(|v| v.to_string()) {
                req_headers.set("Host", h);
            }
        }
        if !req_headers.contains("User-Agent") {
            req_headers.set("User-Agent", self.opts.user_agent.clone().unwrap_or_else(|| format!("cameleer/{}", env!("CARGO_PKG_VERSION"))));
        }
        if let Some((u, p)) = &self.opts.auth {
            req_headers.set("Authorization", format!("Basic {}", base64_encode(format!("{}:{}", u, p).as_bytes())));
        }
        let mut body = body;
        if send_body && stream_reader.is_none() {
            let enc = req_headers.get("Content-Encoding").map(|s| s.to_string()).or_else(|| ex.header("Content-Encoding").map(|v| v.to_string()));
            if let Some(enc) = enc.map(|s| s.trim().to_ascii_lowercase()) {
                let compressed = match enc.as_str() {
                    "gzip" | "x-gzip" => Some(cameleer_core::dataformat::compress::gzip_compress(&body, 6)),
                    "deflate" => Some(cameleer_core::dataformat::compress::zlib_compress(&body, 6)),
                    _ => None,
                };
                if let Some(c) = compressed {
                    body = c;
                    req_headers.set("Content-Encoding", enc);
                }
            }
        }
        if self.opts.content_compression && !req_headers.contains("Accept-Encoding") {
            req_headers.set("Accept-Encoding", "gzip, deflate");
        }
        let streaming = !self.opts.http2 && (stream_reader.is_some() || self.opts.disable_stream_cache);
        let mut streamed_body: Option<Value> = None;
        let mut resp = if streaming {
            let reader = stream_reader.take().map(|(r, _)| r);
            let (mut resp, mut body_reader) = self.send_streaming(&method, &url, &req_headers, reader, if send_body { Some(&body) } else { None })?;
            let ok = self.opts.ok_status_code_range.iter().any(|(a, b)| (*a..=*b).contains(&resp.status));
            if self.opts.disable_stream_cache && !self.opts.ignore_response_body && (ok || !self.opts.throw_exception_on_failure) {
                let len = body_reader.content_length();
                streamed_body = Some(cameleer_tls::stream_body::register(Box::new(body_reader), len, &url.to_string()));
            } else {
                body_reader.read_to_end(&mut resp.body).map_err(|e| CamelError::io(format!("Cannot read the response of {}: {}", url, e)))?;
            }
            resp
        } else {
            self.send(&method, &url, &req_headers, if send_body { Some(&body) } else { None })?
        };
        if self.opts.content_compression && streamed_body.is_none() {
            if let Some(enc) = resp.headers.get("Content-Encoding").map(|s| s.to_string()) {
                if !resp.body.is_empty() && !enc.eq_ignore_ascii_case("identity") {
                    resp.body = decode_content(&enc, &resp.body)?;
                    resp.headers.remove("Content-Encoding");
                    resp.headers.set("Content-Length", resp.body.len().to_string());
                }
            }
        }

        // populate the response
        let ct = resp.headers.get("Content-Type").map(|s| s.to_string());
        let msg = &mut ex.message;
        if !self.opts.skip_response_headers {
            let mut seen: Vec<String> = Vec::new();
            for (k, v) in resp.headers.iter() {
                if crate::binding::filter_inbound(k) {
                    continue;
                }
                let lower = k.to_ascii_lowercase();
                if !seen.contains(&lower) {
                    msg.headers.remove(k);
                    seen.push(lower);
                }
                add_header_value(&mut msg.headers, k, Value::String(v.to_string()));
            }
        }
        msg.headers.set(headers::HTTP_RESPONSE_CODE, resp.status as i64);
        msg.headers.set(headers::HTTP_RESPONSE_TEXT, resp.reason_phrase().to_string());
        msg.body = match streamed_body {
            Some(h) => h,
            None if self.opts.ignore_response_body => Value::Null,
            None => body_to_value(&resp.body, ct.as_deref()),
        };

        let ok = self.opts.ok_status_code_range.iter().any(|(a, b)| (*a..=*b).contains(&resp.status));
        if !ok && self.opts.throw_exception_on_failure {
            let location = resp.headers.get("Location").map(|s| s.to_string());
            let info = HttpOperationFailed {
                uri: url.to_string(),
                status_code: resp.status,
                status_text: resp.reason_phrase().to_string(),
                redirect_location: location.clone(),
                response_body: if resp.body.is_empty() { None } else { Some(resp.body_text()) },
                response_headers: resp.headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            };
            ex.set_property(headers::HTTP_OPERATION_FAILED, info.to_value());
            let mut message = format!("HTTP operation failed invoking {} with statusCode: {}", url, resp.status);
            if let Some(l) = location {
                message.push_str(&format!(", redirectLocation: {}", l));
            }
            return Err(CamelError::new(kinds::HTTP_OPERATION_FAILED, message));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_ranges("200-299").unwrap(), vec![(200, 299)]);
        assert_eq!(parse_ranges("200-204,209,301-304").unwrap(), vec![(200, 204), (209, 209), (301, 304)]);
        assert!(parse_ranges("abc").is_err());
    }

    #[test]
    fn status_code_from_error() {
        let e = CamelError::new(kinds::HTTP_OPERATION_FAILED, "HTTP operation failed invoking http://x/ with statusCode: 404");
        assert_eq!(HttpOperationFailed::status_code_of(&e), Some(404));
        assert_eq!(encode_query_string("a=b c&d=é"), "a=b%20c&d=%C3%A9");
    }
}
