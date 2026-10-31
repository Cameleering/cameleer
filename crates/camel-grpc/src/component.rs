//! The Camel `grpc` component.
//!
//! `grpc://host:port/package.Service?method=sayHello&protoFile=proto/greeter.proto`
//!
//! Messages are described by the `.proto` file (`protoFile`) and mapped to/from
//! [`Value`] objects by the dynamic protobuf codec (see [`crate::proto::codec`]). Without
//! `protoFile` message bodies are raw protobuf bytes.
//!
//! **Producer** (`to("grpc://...")`) — calls `method` (lowerCamel stub name `sayHello` or
//! proto name `SayHello`):
//! * `producerStrategy=SIMPLE` (default): unary → the body becomes the response object;
//!   server streaming → the body becomes an array of responses (each one is also sent to
//!   `streamRepliesTo` as it arrives when that option is set); client streaming / bidi →
//!   the body (an array, or one message) is streamed and the response (array for bidi)
//!   becomes the body.
//! * `producerStrategy=STREAMING`: client-streaming / bidi calls stay open across
//!   exchanges; each exchange body is sent as one message (an array as several), responses
//!   are routed to `streamRepliesTo`. An exchange with header
//!   `CamelGrpcEventType=onCompleted` half-closes the current call.
//! * Failed calls raise `StatusRuntimeException` with message `"CODE: description"`.
//!   Exchange headers (non `Camel*`, scalar values) are sent as metadata; response
//!   metadata becomes headers. Option `deadline` sets `grpc-timeout`.
//!
//! **Consumer** (`from("grpc://0.0.0.0:port/package.Service?protoFile=...")`) — hosts the
//! service; the route implements its methods. Header `CamelGrpcMethodName` holds the
//! lowerCamel method name, request metadata is copied to headers.
//! * `consumerStrategy=AGGREGATION` (default): the route runs once per call with the
//!   request (an array of requests for client streaming); the resulting body is the
//!   response (an array streams several responses for server-streaming methods).
//! * `consumerStrategy=PROPAGATION` (`DELEGATION` is treated alike): the route runs for
//!   every request message with `CamelGrpcEventType=onNext`; with `forwardOnCompleted`
//!   also once at the end with `onCompleted`. Non-null result bodies are streamed back
//!   (server streaming) or the last one is the response.
//! * A failed exchange finishes the call with a gRPC status: `IllegalArgumentException`
//!   → INVALID_ARGUMENT, `TimeoutException` → DEADLINE_EXCEEDED,
//!   `UnsupportedOperationException` → UNIMPLEMENTED, `StatusRuntimeException("CODE: msg")`
//!   → that code, anything else → INTERNAL with the exception message. Client deadlines
//!   (`grpc-timeout`) are enforced (DEADLINE_EXCEEDED).
//!
//! TLS: `usePlainText=false` (or `negotiationType=TLS`) uses camel-tls with ALPN `h2`;
//! the consumer needs `keyCertChainResource` + `keyResource` (PEM files), the producer
//! trusts `trustCertCollectionResource` (PEM) or the system store.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::{Exchange, ExchangePattern};
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;
use camel_tls::{TlsClientConfig, TlsServerConfig};

use crate::dataformat::body_to_native;
use crate::grpc::{is_reserved_header, CallSender, Code, GrpcClient, GrpcServer, Metadata, ServerCall, Status};
use crate::h2::H2Config;
use crate::proto::{load_cached, to_lower_camel, DescriptorPool, MethodDescriptor};

/// Header: the called method (lowerCamel stub name, e.g. `sayHello`).
pub const GRPC_METHOD_NAME: &str = "CamelGrpcMethodName";
/// Header: the consumer event (`onNext`, `onCompleted`, `onError`).
pub const GRPC_EVENT_TYPE: &str = "CamelGrpcEventType";
/// Header: the client's user agent.
pub const GRPC_USER_AGENT: &str = "CamelGrpcUserAgent";
/// `CamelGrpcEventType` values.
pub const GRPC_EVENT_TYPE_ON_NEXT: &str = "onNext";
pub const GRPC_EVENT_TYPE_ON_COMPLETED: &str = "onCompleted";
pub const GRPC_EVENT_TYPE_ON_ERROR: &str = "onError";

/// Consumer strategies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsumerStrategy {
    Aggregation,
    Propagation,
}

/// Producer strategies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProducerStrategy {
    Simple,
    Streaming,
}

/// Endpoint configuration.
#[derive(Clone, Debug)]
pub struct GrpcConfig {
    pub host: String,
    pub port: u16,
    pub service: String,
    pub method: Option<String>,
    pub proto_file: Option<String>,
    pub consumer_strategy: ConsumerStrategy,
    pub producer_strategy: ProducerStrategy,
    pub stream_replies_to: Option<String>,
    pub plain_text: bool,
    pub forward_on_completed: bool,
    pub forward_on_error: bool,
    pub max_message_size: usize,
    pub max_concurrent_calls_per_connection: u32,
    pub flow_control_window: u32,
    pub deadline: Option<Duration>,
    pub trust_cert_collection_resource: Option<String>,
    pub key_cert_chain_resource: Option<String>,
    pub key_resource: Option<String>,
    pub shutdown_timeout: Duration,
}

impl GrpcConfig {
    fn h2_config(&self) -> H2Config {
        H2Config {
            max_concurrent_streams: self.max_concurrent_calls_per_connection,
            initial_window_size: self.flow_control_window,
            connection_window_size: self.flow_control_window.max(4 << 20),
            ..H2Config::default()
        }
    }
    fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn strip_resource(p: &str) -> String {
    p.strip_prefix("classpath:").or_else(|| p.strip_prefix("file:")).unwrap_or(p).to_string()
}

/// The `grpc` component.
#[derive(Default)]
pub struct GrpcComponent;

impl Component for GrpcComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let rem = remaining.trim_start_matches("//");
        let (authority, service) = rem.split_once('/').ok_or_else(|| CamelError::illegal_argument(format!("Invalid grpc uri {} (expected grpc://host:port/package.Service)", uri)))?;
        let service = service.trim_matches('/').to_string();
        if service.is_empty() {
            return Err(CamelError::illegal_argument(format!("Missing service name in {}", uri)));
        }
        let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
            let (h, p) = rest.split_once("]:").ok_or_else(|| CamelError::illegal_argument(format!("Invalid authority in {}", uri)))?;
            (h.to_string(), p)
        } else {
            let (h, p) = authority.rsplit_once(':').ok_or_else(|| CamelError::illegal_argument(format!("Missing port in {}", uri)))?;
            (h.to_string(), p)
        };
        let port: u16 = port.parse().map_err(|_| CamelError::illegal_argument(format!("Invalid port in {}", uri)))?;
        let consumer_strategy = match params.take_string("consumerStrategy", "AGGREGATION").to_ascii_uppercase().as_str() {
            "AGGREGATION" => ConsumerStrategy::Aggregation,
            "PROPAGATION" | "DELEGATION" => ConsumerStrategy::Propagation,
            other => return Err(CamelError::illegal_argument(format!("Invalid consumerStrategy: {}", other))),
        };
        let producer_strategy = match params.take_string("producerStrategy", "SIMPLE").to_ascii_uppercase().as_str() {
            "SIMPLE" => ProducerStrategy::Simple,
            "STREAMING" => ProducerStrategy::Streaming,
            other => return Err(CamelError::illegal_argument(format!("Invalid producerStrategy: {}", other))),
        };
        let mut plain_text = params.take_bool("usePlainText", true)?;
        if let Some(n) = params.take("negotiationType") {
            plain_text = match n.to_ascii_uppercase().as_str() {
                "PLAINTEXT" | "PLAINTEXT_UPGRADE" => true,
                "TLS" => false,
                other => return Err(CamelError::illegal_argument(format!("Invalid negotiationType: {}", other))),
            };
        }
        let deadline = match params.take("deadline") {
            Some(d) => Some(camel_core::util::parse_duration(&d)?),
            None => None,
        };
        let cfg = GrpcConfig {
            host,
            port,
            service,
            method: params.take("method"),
            proto_file: params.take("protoFile").map(|p| strip_resource(&p)),
            consumer_strategy,
            producer_strategy,
            stream_replies_to: params.take("streamRepliesTo"),
            plain_text,
            forward_on_completed: params.take_bool("forwardOnCompleted", false)?,
            forward_on_error: params.take_bool("forwardOnError", false)?,
            max_message_size: params.take_usize("maxMessageSize", 4 << 20)?,
            max_concurrent_calls_per_connection: params.take_u64("maxConcurrentCallsPerConnection", 256)?.min(u32::MAX as u64) as u32,
            flow_control_window: params.take_u64("flowControlWindow", 1 << 20)?.clamp(65_535, (1 << 31) - 1) as u32,
            deadline,
            trust_cert_collection_resource: params.take("trustCertCollectionResource").map(|p| strip_resource(&p)),
            key_cert_chain_resource: params.take("keyCertChainResource").map(|p| strip_resource(&p)),
            key_resource: params.take("keyResource").map(|p| strip_resource(&p)),
            shutdown_timeout: params.take_duration("shutdownTimeout", Duration::from_secs(5))?,
        };
        for ignored in [
            "synchronous",
            "lazyStartProducer",
            "bridgeErrorHandler",
            "exchangePattern",
            "exceptionHandler",
            "userAgent",
            "keyPassword",
            "authenticationType",
            "autoDiscoverClientInterceptors",
            "autoDiscoverServerInterceptors",
            "keepAliveTime",
            "keepAliveTimeout",
            "maxInboundMetadataSize",
            "maxRstFramesPerWindow",
            "maxRstPeriodSeconds",
            "permitKeepAliveTime",
            "permitKeepAliveWithoutCalls",
            "routeControlledStreamObserver",
            "toRouteControlledStreamObserver",
            "inheritExchangePropertiesForReplies",
            "initialFlowControlWindow",
            "idleTimeout",
        ] {
            params.take(ignored);
        }
        let pool = match &cfg.proto_file {
            Some(p) => {
                let pool = load_cached(p)?;
                let svc = pool
                    .service(&cfg.service)
                    .ok_or_else(|| CamelError::illegal_argument(format!("Service {} not found in {} (services: {:?})", cfg.service, p, pool.services.keys().collect::<Vec<_>>())))?;
                if let Some(m) = &cfg.method {
                    if svc.method(m).is_none() {
                        return Err(CamelError::illegal_argument(format!("Method {} not found in service {}", m, svc.full_name)));
                    }
                }
                Some(pool)
            }
            None => None,
        };
        Ok(Arc::new(GrpcEndpoint { uri: uri.to_string(), cfg, pool, ctx: ctx.downgrade(), bound_port: Arc::new(Mutex::new(None)) }))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `grpc` endpoint.
pub struct GrpcEndpoint {
    uri: String,
    cfg: GrpcConfig,
    pool: Option<Arc<DescriptorPool>>,
    ctx: WeakContext,
    bound_port: Arc<Mutex<Option<u16>>>,
}

impl GrpcEndpoint {
    pub fn config(&self) -> &GrpcConfig {
        &self.cfg
    }
    /// The bound port of a running consumer (useful with port 0).
    pub fn local_port(&self) -> Option<u16> {
        *self.bound_port.lock().unwrap()
    }
}

impl Endpoint for GrpcEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        if self.cfg.method.is_none() {
            return Err(CamelError::illegal_argument(format!("grpc producer {} requires the method option", self.uri)));
        }
        Ok(Arc::new(GrpcProducer {
            cfg: self.cfg.clone(),
            pool: self.pool.clone(),
            ctx: self.ctx.clone(),
            client: Mutex::new(None),
            streaming: Mutex::new(None),
        }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(GrpcConsumer {
            shared: Arc::new(ConsumerShared { cfg: self.cfg.clone(), pool: self.pool.clone(), ctx: self.ctx.clone(), processor }),
            server: Mutex::new(None),
            bound_port: self.bound_port.clone(),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// message helpers
// ---------------------------------------------------------------------------

fn encode_value(pool: Option<&DescriptorPool>, type_name: Option<&str>, v: &Value) -> Result<Vec<u8>> {
    match (pool, type_name) {
        (Some(pool), Some(t)) => match v {
            Value::Bytes(b) => Ok(b.clone()),
            other => pool.encode(t, &body_to_native(pool, t, other)?),
        },
        _ => Ok(match v {
            Value::Null => Vec::new(),
            Value::Bytes(b) => b.clone(),
            other => other.to_string().into_bytes(),
        }),
    }
}

fn decode_value(pool: Option<&DescriptorPool>, type_name: Option<&str>, b: &[u8]) -> Result<Value> {
    match (pool, type_name) {
        (Some(pool), Some(t)) => pool.decode(t, b),
        _ => Ok(Value::Bytes(b.to_vec())),
    }
}

/// Exchange headers sent as call metadata: non-`Camel*` headers with scalar values and
/// valid lower-case header names.
fn headers_to_metadata(ex: &Exchange) -> Metadata {
    let mut md = Vec::new();
    for (k, v) in ex.message.headers.iter() {
        let name = k.to_ascii_lowercase();
        if k.starts_with("Camel") || k.starts_with("camel") || is_reserved_header(&name) || name.starts_with("grpc-") {
            continue;
        }
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) || name.is_empty() {
            continue;
        }
        let value = match v {
            Value::String(_) | Value::Int(_) | Value::Float(_) | Value::Bool(_) => v.to_string(),
            Value::Bytes(b) if name.ends_with("-bin") => camel_core::dataformat::base64::encode(b, false),
            _ => continue,
        };
        if value.bytes().all(|b| (0x20..0x7f).contains(&b)) {
            md.push((name, value));
        }
    }
    md
}

// ---------------------------------------------------------------------------
// producer
// ---------------------------------------------------------------------------

struct StreamingState {
    sender: CallSender,
}

struct GrpcProducer {
    cfg: GrpcConfig,
    pool: Option<Arc<DescriptorPool>>,
    ctx: WeakContext,
    client: Mutex<Option<Arc<GrpcClient>>>,
    streaming: Mutex<Option<StreamingState>>,
}

impl GrpcProducer {
    fn client(&self) -> Result<Arc<GrpcClient>> {
        let mut g = self.client.lock().unwrap();
        if let Some(c) = g.as_ref() {
            return Ok(c.clone());
        }
        let addr = self.cfg.authority();
        let mut client = if self.cfg.plain_text {
            GrpcClient::plaintext(&addr)
        } else {
            let mut b = TlsClientConfig::builder().alpn_protocols(&["h2"]);
            if let Some(trust) = &self.cfg.trust_cert_collection_resource {
                b = b.system_roots(false).add_root_pem_file(trust).map_err(|e| CamelError::illegal_argument(format!("trustCertCollectionResource {}: {}", trust, e)))?;
            }
            if self.cfg.key_cert_chain_resource.is_some() {
                camel_core::camel_warn!("grpc", "Client certificates (keyCertChainResource on a producer) are not supported by camel-tls and are ignored");
            }
            GrpcClient::tls(&addr, &self.cfg.host, b.build())
        };
        client.config = self.cfg.h2_config();
        client.max_message_size = self.cfg.max_message_size;
        let c = Arc::new(client);
        *g = Some(c.clone());
        Ok(c)
    }

    /// (path, method descriptor)
    fn method(&self) -> (String, Option<MethodDescriptor>) {
        let m = self.cfg.method.clone().unwrap_or_default();
        match self.pool.as_ref().and_then(|p| p.service(&self.cfg.service)) {
            Some(svc) => {
                let md = svc.method(&m).cloned();
                let name = md.as_ref().map(|d| d.name.clone()).unwrap_or(m);
                (format!("/{}/{}", svc.full_name, name), md)
            }
            None => {
                let mut c = m.chars();
                let proto_name = c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default();
                (format!("/{}/{}", self.cfg.service, proto_name), None)
            }
        }
    }

    fn forward_reply(&self, v: Value) -> Result<()> {
        if let (Some(uri), Some(ctx)) = (&self.cfg.stream_replies_to, self.ctx.upgrade()) {
            let p = ctx.get_producer(uri)?;
            let mut ex = Exchange::with_body(Some(ctx.clone()), v);
            ex.set_header(GRPC_EVENT_TYPE, GRPC_EVENT_TYPE_ON_NEXT);
            p.process(&mut ex)?;
            if let Some(e) = ex.exception {
                return Err(e);
            }
        }
        Ok(())
    }

    fn process_simple(&self, ex: &mut Exchange) -> Result<()> {
        let client = self.client()?;
        let (path, md) = self.method();
        let pool = self.pool.as_deref();
        let input = md.as_ref().map(|m| m.input_type.as_str());
        let output = md.as_ref().map(|m| m.output_type.as_str());
        let metadata = headers_to_metadata(ex);
        let body = ex.message.body.clone();
        let (cs, ss) = match &md {
            Some(m) => (m.client_streaming, m.server_streaming),
            None => (matches!(body, Value::Array(_)), matches!(body, Value::Array(_))),
        };
        let items = |b: &Value| -> Result<Vec<Vec<u8>>> {
            match b {
                Value::Array(a) => a.iter().map(|x| encode_value(pool, input, x)).collect(),
                other => Ok(vec![encode_value(pool, input, other)?]),
            }
        };
        let (reply, resp_md) = if !cs && !ss {
            let req = encode_value(pool, input, &body)?;
            let (resp, rmd) = client.unary(&path, &req, &metadata, self.cfg.deadline).map_err(|s| s.to_camel_error())?;
            (decode_value(pool, output, &resp)?, rmd)
        } else if !cs && ss {
            let req = encode_value(pool, input, &body)?;
            let mut out = Vec::new();
            let mut fwd_err: Option<CamelError> = None;
            let rmd = client
                .server_streaming(&path, &req, &metadata, self.cfg.deadline, &mut |m| {
                    let v = decode_value(pool, output, &m).map_err(|e| Status::new(Code::Internal, e.message.clone()))?;
                    if self.cfg.stream_replies_to.is_some() {
                        if let Err(e) = self.forward_reply(v.clone()) {
                            fwd_err = Some(e.clone());
                            return Err(Status::new(Code::Cancelled, e.message));
                        }
                    }
                    out.push(v);
                    Ok(())
                })
                .map_err(|s| fwd_err.take().unwrap_or_else(|| s.to_camel_error()))?;
            (Value::Array(out), rmd)
        } else {
            let reqs = items(&body)?;
            let (resps, rmd) = client.streaming(&path, &reqs, &metadata, self.cfg.deadline).map_err(|s| s.to_camel_error())?;
            let mut vals = resps.iter().map(|r| decode_value(pool, output, r)).collect::<Result<Vec<_>>>()?;
            if ss {
                for v in &vals {
                    self.forward_reply(v.clone())?;
                }
                (Value::Array(vals), rmd)
            } else {
                if vals.len() != 1 {
                    return Err(Status::new(Code::Internal, format!("expected one response, got {}", vals.len())).to_camel_error());
                }
                (vals.remove(0), rmd)
            }
        };
        for (k, v) in resp_md {
            ex.set_header(k, v);
        }
        ex.message.body = reply;
        Ok(())
    }

    fn process_streaming(&self, ex: &mut Exchange) -> Result<()> {
        let (path, md) = self.method();
        let pool = self.pool.clone();
        let input = md.as_ref().map(|m| m.input_type.clone());
        let output = md.as_ref().map(|m| m.output_type.clone());
        let mut guard = self.streaming.lock().unwrap();
        if guard.is_none() {
            let client = self.client()?;
            let metadata = headers_to_metadata(ex);
            let mut call = client.start_call(&path, &metadata, self.cfg.deadline).map_err(|s| s.to_camel_error())?;
            let sender = call.sender();
            let (ctx, replies_to) = (self.ctx.clone(), self.cfg.stream_replies_to.clone());
            let (pool2, output2) = (pool.clone(), output.clone());
            std::thread::Builder::new()
                .name("grpc-stream-replies".into())
                .spawn(move || loop {
                    let r = call.recv();
                    let Some(ctx) = ctx.upgrade() else { break };
                    let (body, event, done) = match r {
                        Ok(Some(m)) => match decode_value(pool2.as_deref(), output2.as_deref(), &m) {
                            Ok(v) => (v, GRPC_EVENT_TYPE_ON_NEXT, false),
                            Err(e) => (Value::String(e.message), GRPC_EVENT_TYPE_ON_ERROR, true),
                        },
                        Ok(None) => (Value::Null, GRPC_EVENT_TYPE_ON_COMPLETED, true),
                        Err(s) => (Value::String(s.to_string()), GRPC_EVENT_TYPE_ON_ERROR, true),
                    };
                    if let Some(uri) = &replies_to {
                        if event == GRPC_EVENT_TYPE_ON_NEXT || done {
                            if let Ok(p) = ctx.get_producer(uri) {
                                let mut rex = Exchange::with_body(Some(ctx.clone()), body);
                                rex.set_header(GRPC_EVENT_TYPE, event);
                                let _ = p.process(&mut rex);
                            }
                        }
                    }
                    if done {
                        break;
                    }
                })
                .map_err(|e| CamelError::io(e.to_string()))?;
            *guard = Some(StreamingState { sender });
        }
        let state = guard.as_ref().expect("started");
        let msgs: Vec<Value> = match &ex.message.body {
            Value::Array(a) => a.clone(),
            Value::Null => Vec::new(),
            other => vec![other.clone()],
        };
        let mut result = Ok(());
        for m in msgs {
            let bytes = encode_value(pool.as_deref(), input.as_deref(), &m)?;
            if let Err(s) = state.sender.send(&bytes) {
                result = Err(s.to_camel_error());
                break;
            }
        }
        let complete = ex.header(GRPC_EVENT_TYPE).map(|v| v.to_string() == GRPC_EVENT_TYPE_ON_COMPLETED).unwrap_or(false);
        if complete || result.is_err() {
            let _ = state.sender.close_send();
            *guard = None;
        }
        result
    }
}

impl Processor for GrpcProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        match self.cfg.producer_strategy {
            ProducerStrategy::Simple => self.process_simple(ex),
            ProducerStrategy::Streaming => self.process_streaming(ex),
        }
    }
}

impl Service for GrpcProducer {
    fn stop(&self) -> Result<()> {
        if let Some(s) = self.streaming.lock().unwrap().take() {
            let _ = s.sender.close_send();
        }
        if let Some(c) = self.client.lock().unwrap().take() {
            c.close();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// consumer
// ---------------------------------------------------------------------------

struct ConsumerShared {
    cfg: GrpcConfig,
    pool: Option<Arc<DescriptorPool>>,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
}

impl ConsumerShared {
    fn route(&self, ctx: &CamelContext, call: &ServerCall, method: &str, body: Value, event: Option<&str>) -> std::result::Result<Value, Status> {
        let mut ex = Exchange::new(Some(ctx.clone()));
        ex.pattern = ExchangePattern::InOut;
        for (k, v) in call.metadata() {
            ex.set_header(k.clone(), v.clone());
        }
        ex.set_header(GRPC_METHOD_NAME, to_lower_camel(method));
        if let Some(ua) = call.user_agent() {
            ex.set_header(GRPC_USER_AGENT, ua);
        }
        if let Some(e) = event {
            ex.set_header(GRPC_EVENT_TYPE, e);
        }
        ex.message.body = body;
        if let Err(e) = self.processor.process(&mut ex) {
            ex.set_exception(e);
        }
        match ex.exception {
            Some(e) => Err(Status::from_camel_error(&e)),
            None => Ok(ex.message.body),
        }
    }

    fn send_replies(&self, call: &ServerCall, output: Option<&str>, reply: Value, server_streaming: bool) -> std::result::Result<(), Status> {
        let pool = self.pool.as_deref();
        let enc = |v: &Value| encode_value(pool, output, v).map_err(|e| Status::new(Code::Internal, format!("cannot encode response: {}", e.message)));
        if server_streaming {
            match reply {
                Value::Array(a) => {
                    for v in a {
                        call.send(&enc(&v)?)?;
                    }
                }
                Value::Null => {}
                other => call.send(&enc(&other)?)?,
            }
            return Ok(());
        }
        let single = match reply {
            Value::Array(mut a) if output.is_some() => {
                if a.len() != 1 {
                    return Err(Status::new(Code::Internal, format!("method returns a single message but the route produced {} values", a.len())));
                }
                a.remove(0)
            }
            other => other,
        };
        call.send(&enc(&single)?)
    }

    fn serve(&self, call: &ServerCall) -> Status {
        match self.serve_inner(call) {
            Ok(()) => Status::ok(),
            Err(s) => s,
        }
    }

    fn serve_inner(&self, call: &ServerCall) -> std::result::Result<(), Status> {
        let ctx = self.ctx.upgrade().ok_or_else(|| Status::new(Code::Unavailable, "CamelContext stopped"))?;
        let (method, input, output, cs, ss) = match &self.pool {
            Some(pool) => {
                let svc = pool.service(&self.cfg.service).ok_or_else(|| Status::new(Code::Unimplemented, "unknown service"))?;
                if call.service() != svc.full_name {
                    return Err(Status::new(Code::Unimplemented, format!("unknown service {}", call.service())));
                }
                let m = svc.methods.iter().find(|m| m.name == call.method()).ok_or_else(|| Status::new(Code::Unimplemented, format!("unknown method {} for service {}", call.method(), svc.full_name)))?;
                (m.name.clone(), Some(m.input_type.clone()), Some(m.output_type.clone()), m.client_streaming, m.server_streaming)
            }
            None => {
                if call.service() != self.cfg.service {
                    return Err(Status::new(Code::Unimplemented, format!("unknown service {}", call.service())));
                }
                (call.method().to_string(), None, None, false, true)
            }
        };
        let pool = self.pool.as_deref();
        let dec = |b: &[u8]| decode_value(pool, input.as_deref(), b).map_err(|e| Status::new(Code::Internal, format!("cannot parse request: {}", e.message)));
        match self.cfg.consumer_strategy {
            ConsumerStrategy::Aggregation => {
                let mut reqs = Vec::new();
                while let Some(m) = call.recv()? {
                    reqs.push(dec(&m)?);
                }
                let body = if cs || (pool.is_none() && reqs.len() != 1) {
                    Value::Array(reqs)
                } else {
                    if reqs.len() > 1 {
                        return Err(Status::new(Code::Internal, "too many requests for a unary-request method"));
                    }
                    reqs.into_iter().next().ok_or_else(|| Status::new(Code::Internal, "missing request message"))?
                };
                let reply = self.route(&ctx, call, &method, body, None)?;
                self.send_replies(call, output.as_deref(), reply, ss)
            }
            ConsumerStrategy::Propagation => {
                let mut last = Value::Null;
                loop {
                    match call.recv() {
                        Ok(Some(m)) => {
                            let reply = self.route(&ctx, call, &method, dec(&m)?, Some(GRPC_EVENT_TYPE_ON_NEXT))?;
                            if ss {
                                self.send_replies(call, output.as_deref(), reply, true)?;
                            } else if !reply.is_null() {
                                last = reply;
                            }
                        }
                        Ok(None) => break,
                        Err(s) => {
                            if self.cfg.forward_on_error {
                                let _ = self.route(&ctx, call, &method, Value::String(s.to_string()), Some(GRPC_EVENT_TYPE_ON_ERROR));
                            }
                            return Err(s);
                        }
                    }
                }
                if self.cfg.forward_on_completed {
                    let reply = self.route(&ctx, call, &method, Value::Null, Some(GRPC_EVENT_TYPE_ON_COMPLETED))?;
                    if ss {
                        self.send_replies(call, output.as_deref(), reply, true)?;
                    } else if !reply.is_null() {
                        last = reply;
                    }
                }
                if !ss {
                    self.send_replies(call, output.as_deref(), last, false)?;
                }
                Ok(())
            }
        }
    }
}

struct GrpcConsumer {
    shared: Arc<ConsumerShared>,
    server: Mutex<Option<GrpcServer>>,
    bound_port: Arc<Mutex<Option<u16>>>,
}

impl Service for GrpcConsumer {
    fn start(&self) -> Result<()> {
        let mut g = self.server.lock().unwrap();
        if g.is_some() {
            return Ok(());
        }
        let cfg = &self.shared.cfg;
        let tls = if cfg.plain_text {
            None
        } else {
            let (Some(chain), Some(key)) = (&cfg.key_cert_chain_resource, &cfg.key_resource) else {
                return Err(CamelError::illegal_argument("grpc consumer with TLS requires keyCertChainResource and keyResource"));
            };
            Some(
                TlsServerConfig::builder()
                    .cert_chain_pem_file(chain)
                    .private_key_pem_file(key)
                    .alpn_protocols(&["h2"])
                    .build()
                    .map_err(|e| CamelError::illegal_argument(format!("Invalid TLS configuration: {}", e)))?,
            )
        };
        let shared = self.shared.clone();
        let service: Arc<dyn crate::grpc::GrpcService> = Arc::new(move |call: &ServerCall| shared.serve(call));
        let addr = cfg.authority();
        let server = GrpcServer::bind(&addr, cfg.h2_config(), tls, cfg.max_message_size, service).map_err(|s| CamelError::io(format!("Failed to start grpc consumer on {}: {}", addr, s.message)))?;
        *self.bound_port.lock().unwrap() = Some(server.local_addr().port());
        camel_core::camel_info!("grpc", "gRPC consumer for {} listening on {} ({})", cfg.service, server.local_addr(), if cfg.plain_text { "plaintext" } else { "TLS" });
        *g = Some(server);
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        if let Some(s) = self.server.lock().unwrap().take() {
            s.stop(self.shared.cfg.shutdown_timeout);
        }
        *self.bound_port.lock().unwrap() = None;
        Ok(())
    }
}

/// Option names understood by the component (for documentation / tooling).
pub fn option_names() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("method", "producer: method to call (lowerCamel or proto name)"),
        ("protoFile", ".proto file describing the service (messages are raw bytes without it)"),
        ("consumerStrategy", "AGGREGATION (default) | PROPAGATION | DELEGATION"),
        ("producerStrategy", "SIMPLE (default) | STREAMING"),
        ("streamRepliesTo", "endpoint receiving streamed responses"),
        ("usePlainText", "true (default): h2c; false: TLS with ALPN h2"),
        ("negotiationType", "PLAINTEXT | TLS"),
        ("forwardOnCompleted", "PROPAGATION: route an onCompleted event"),
        ("forwardOnError", "PROPAGATION: route an onError event"),
        ("maxMessageSize", "maximum message size (default 4 MiB)"),
        ("maxConcurrentCallsPerConnection", "consumer: SETTINGS_MAX_CONCURRENT_STREAMS"),
        ("flowControlWindow", "HTTP/2 stream window"),
        ("deadline", "producer: call deadline (grpc-timeout)"),
        ("trustCertCollectionResource", "producer TLS: trusted CA PEM file"),
        ("keyCertChainResource", "consumer TLS: certificate chain PEM file"),
        ("keyResource", "consumer TLS: private key PEM file"),
        ("shutdownTimeout", "consumer: graceful shutdown timeout"),
    ])
}
