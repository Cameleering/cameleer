# Cameleer HTTP, HTTP/2 and TLS: what depends on what

_Read-only inventory, 2026-11-09, `main` at `b91bd70` (0.5.1). Its purpose: when Cameleer's own
HTTP/1.1 server, HTTP/2 stack and possibly its TLS internals are replaced with simoom
(`E:\Work\DataHQ\Code\simoom`), nothing is lost. All line numbers refer to that commit._

Related notes: `docs/design/http-server/research.md` and `plan.md` (the simoom design; §3.1 "How
Cameleer plugs in"). simoom's README describes it as "an embeddable HTTP/1.1, HTTP/2 and HTTP/3
**server** engine". It has no client crate (`simoom-{rt,sys,tls,http1,h2,hpack,h3,qpack,quic}`),
so **simoom replaces servers only.** The clients listed in §2 stay in Cameleer unless a separate
decision moves them.

---

## 0. Summary of the integration surfaces

| # | surface | code | goes to simoom? |
|---|---|---|---|
| A | `cameleer_http::server::HttpServer` + `HttpHandler` + `ServerConfig` | `crates/cameleer-http/src/server.rs` | yes, the core of the switch |
| B | shared port registry and router (`registry()`, `Router`, `HttpRouteSpec`, `RouteHandler`) | `crates/cameleer-http/src/registry.rs` | the API stays; dispatch may move into the loop |
| C | wire model `HttpRequest`/`HttpResponse`/`HttpHeaders`/`Limits`, `write_response`, `stream_body` handles | `crates/cameleer-http/src/http.rs`, `crates/cameleer-tls/src/stream_body.rs` | adapter needed. The client shares the same types |
| D | `H2Server`, `serve_transport`, `GrpcServer` | `crates/cameleer-grpc/src/h2/server.rs`, `grpc/server.rs` | yes (plan Milestone 5) |
| E | cloud `MockServer` (its own HTTP/1.1 codec, h2c sniffing) | `crates/cameleer-cloud/src/testing/mod.rs`, `http.rs` | optional; test-only |
| F | websocket server (`TcpServer`, its own port) | `crates/cameleer-protocols/src/websocket/mod.rs` | not today: no upgrade on the shared server |
| G | LRA participant server (hand-written HTTP on a raw `TcpListener`) | `crates/cameleer-core/src/processor/saga/lra.rs:178` | out of scope (core has no cameleer-http dependency) |
| H | TLS server handshakes: `NetStream::server`, `accept_tls`, `lingering_close` | `crates/cameleer-tls/src/net.rs` | simoom-tls wraps cameleer-tls (plan §3.1) |

---

## 1. HTTP servers

### 1.1 `cameleer-http` server (A)

**API** (`server.rs`):
- `trait HttpHandler { fn handle(&self, &mut HttpRequest) -> HttpResponse; fn stream_request(&self, &HttpRequest) -> bool }` (34-44). Closures `Fn(&mut HttpRequest) -> HttpResponse` implement it (46-53).
- `ServerConfig` (57-94): `name`, `pool_size` (**200**), `limits` (`Limits`: `max_header_bytes` 64 KiB, `max_headers` 100, `max_body_bytes` 10 MiB, `http.rs:243-255`), `read_timeout` 30 s, `keep_alive_timeout` 15 s, `write_timeout` 30 s, `shutdown_timeout` 10 s, `max_keep_alive_requests` 1000, `tls: Option<Arc<TlsServerConfig>>`.
- `HttpServer::bind / bind_arc` (115-139), `local_addr`, `port`, `is_tls`, `is_running`, `connection_count`, `stop` (166-189, graceful and idempotent), `Drop` calls stop.

**Threading model:**
- One accept thread per server (`accept_loop`, 207-238) on a blocking `TcpListener` from `cameleer_tls::net::bind_listener`.
- Each connection is a job on `cameleer_core::executor::ThreadPool` (workers start on demand and exit after 60 s idle, `executor.rs:26-30`). A worker serves **one connection at a time, for the whole connection** (`serve_connection`, 252-394), with blocking `std::net` I/O.
- When the pool is saturated (`pool.is_saturated()`, 372), a keep-alive connection is closed after its response so that waiting connections get a turn. Before 0.3 the pool had 32 workers. The keep-alive churn this caused was found by the cloud benchmark (`docs/BENCHMARKS.md` "HTTP keep-alive").
- `stop()` wakes `accept()` with a loopback connect (171-177), waits up to `shutdown_timeout` for busy requests, then calls `shutdown_socket` on every open connection (184-186). This is the Windows `CancelIoEx` path.

**Features relied on:**
- HTTP/1.0 and 1.1 only (`Version`, `http.rs:76-96`). There is **no HTTP/2 on this server**: no ALPN `h2`, no h2c preface or `Upgrade: h2c`, no websocket upgrade, and no CONNECT.
- Keep-alive with a separate idle timeout. The read-timeout syscall is skipped when the value is unchanged (278-303). `Connection: close` set by the handler is honoured (381-383). HTTP/1.0 gets `Connection: keep-alive` (`http.rs:855-859`).
- Request heads split across packets work (test `a_request_head_split_across_packets_is_read`, 457). So does pipelining: buffered bytes are kept between requests.
- `Expect: 100-continue`: the writer is passed to `read_request_head` (306), and an unsupported expectation returns 417 (`http.rs:286`).
- Bodies: `Content-Length` and chunked requests (`read_chunked`, `http.rs:443`), with `max_body_bytes` → 413. Transfer codings other than chunked → 501 (test `http.rs:1142`). Errors write `text/plain` with the status from `HttpError::status()` and close the connection (313-319, 351-358).
- **Streaming request bodies**: when `HttpHandler::stream_request` returns true, the rest of the connection is wrapped in a `BodyReader` and registered as a `cameleer_tls::stream_body` handle in `HttpRequest::body_stream`. **The connection is closed after such a request** (327-340, 367).
- **Streaming response bodies**: when `HttpResponse::stream` is set, `write_response` sends chunked encoding straight from the reader with 8 KiB chunks and no buffering (`http.rs:791-827`, `write_chunked_stream` 576-592). On HTTP/1.0, HEAD and bodiless statuses it buffers or drops the stream instead. The worker thread stays blocked until the reader returns EOF. **This is how SSE works** (MCP, §1.4).
- Chunked responses for buffered bodies (`resp.chunked`, `CHUNK_SIZE` 8192). `Date` is added automatically. Framing headers set by handlers (`Content-Length`, `Transfer-Encoding`, `Connection`) and invalid header names or values are dropped (`is_framing_header`, 784-789).
- HEAD: there is no body, but `Content-Length` reflects the body. 1xx, 204 and 304 have no body (`status_has_no_body`).
- A panicking handler returns 500 (376-379).
- TLS: when `cfg.tls` is set, `NetStream::server(t, tls)` is called with `read_timeout` as the handshake timeout (258-268). A failure closes the connection. **mTLS**: `stream.peer_certificates()` → `HttpRequest::peer_certificates` (363-365). `remote_addr` is set (362).
- TCP_NODELAY on and a write timeout (254-255).
- **Not present** on the server, so there is nothing to preserve: response compression (gzip only exists in the client), multipart parsing, TRACE handling beyond normal routing, built-in CORS, per-endpoint body limits.

### 1.2 Shared port registry and router (B)

`registry.rs`. A process-wide `ServerRegistry` (`registry()`, 303) maps port → `SharedServer { server: Option<HttpServer>, tls, router }`.

- `register(host, port, &ServerConfig, HttpRouteSpec, RouteHandler) -> Registration` (329-370). The first registration on a port binds the server. Later ones only add a route. Port 0 always binds a new server. The call fails when http and https are mixed on one port (333-340).
  **Gotcha**: only the TLS flag is compared. **The first registrant's `ServerConfig` wins**, including `pool_size`. The dev console registers with `pool_size: 8` (`crates/cameleer-main/src/console.rs:116`). If it binds `camel.server.port` before the routes do, every route on that port gets 8 workers.
- `unregister` stops the server when its last route goes away (373-392).
- `set_in_process_only(true)`: routes are kept and no socket is opened. Port 0 gets virtual ports from 40000 up (312-361). `dispatch(&mut HttpRequest)` serves in process, with servers tried in port order and the first non-404 answer winning (409-424). This is used by the **AWS Lambda runtime** (`crates/cameleer-main/src/lib.rs:755`, `lambda.rs:160`).
- `set_streaming(reg, true)` → `Router::wants_stream`, which backs `HttpHandler::stream_request` (193-209, 271-273). It is set by `disableStreamCache=true`.
- **Router semantics** (148-262):
  - templates `/users/{id}`, trailing `*`, `matchOnUriPrefix`;
  - percent-decoded segment matching;
  - most specific route first: exact before prefix, then more literal segments, then a longer template;
  - duplicate shape with overlapping methods → `IllegalStateException`;
  - 404 `Not Found`;
  - 405 with an `Allow` header;
  - **OPTIONS on a method mismatch → 204 with `Allow: ..., OPTIONS`** (240-242);
  - HEAD allowed where GET is (123);
  - 415 on a `consumes` mismatch (body non-empty);
  - 406 on a `produces` vs `Accept` mismatch.
  - The handler receives the path parameters.

**Who registers on the shared server** (every call to `registry().register`):

| consumer | file:line | path / spec | notes |
|---|---|---|---|
| `platform-http`, `jetty`, `undertow`, `netty-http` | `crates/cameleer-http/src/platform.rs:312-328` | endpoint path, `httpMethodRestrict`, `matchOnUriPrefix`, `consumes`, `produces` | suspended consumer → 503 (306-309); shared ephemeral port per component (186, 312-323) |
| REST DSL / `rest:` | `crates/cameleer-http/src/rest.rs` (verbs become `platform-http:` routes; CORS adds OPTIONS routes, 572-606) | per verb | OpenAPI at `/api-doc` |
| YAML `rest:` | `crates/cameleer-main/src/yaml_rest.rs:37` | through the REST DSL | |
| `webhook:` | `crates/cameleer-saas/src/webhook.rs:88-141` | creates a `platform-http` consumer (`webhookComponentName`) | |
| dev console, health, metrics `/q/*` | `crates/cameleer-main/src/console.rs:115-117` | `/q/*`, `pool_size: 8` | `camel.server.{devConsoleEnabled,healthCheckEnabled,metricsEnabled}` |
| MCP Streamable HTTP | `crates/cameleer-ai/src/mcp/http.rs:34`, `:46` | `camel.server.mcp-path` + `/.well-known/oauth-protected-resource[...]` | SSE, see §1.4 |
| `cxf`, `cxfrs`, `spring-ws` | `crates/cameleer-soap/src/transport.rs:77-78` (`Binding::start`), callers `cxf.rs:405`, `cxfrs.rs:203`, `springws.rs:218` | address path, prefix match | `ListenAddress::parse` takes `sslContextParameters`, `clientAuth` |
| Lambda | `crates/cameleer-main/src/lambda.rs:160` | `registry().dispatch` (no socket) | API Gateway, function URL and ALB events → `HttpRequest::new` (275) |

**Direct `HttpServer::bind` users** (no registry), all embedded emulators or mocks:
- `cameleer-datastores/src/couchdb/server.rs:61` (`pool_size` 16). The `_changes` longpoll **blocks a worker** in a handler waiting on a condvar.
- `cameleer-datastores/src/elasticsearch/server.rs:169`.
- `cameleer-saas/src/testing/mod.rs:124` (SaaS `MockServer`, used by `testing/services.rs:45,180,339` and the model-provider mocks).
- `cameleer-soap/src/fhir/testing.rs:255`.

### 1.3 Mapping exchanges to requests and responses (consumer side)

`ConsumerBinding` (`binding.rs:138-263`, built in `platform.rs:162-170`):
- **Inbound** (`read_request`, 168-210):
  - InOut pattern.
  - HTTP headers → message headers, except `Transfer-Encoding`, `Connection`, `Keep-Alive` and `Content-Length`. Repeated headers become arrays.
  - Query parameters → headers, and form-urlencoded body parameters → headers (`mapHttpMessageFormUrlEncodedBody`). Path parameters → headers.
  - `CamelHttpMethod`, `CamelHttpUri` (raw target), `CamelHttpPath` (**raw** path), `CamelHttpQuery` (decoded), `CamelHttpRawQuery`, `CamelHttpRemoteAddress`, `Content-Type`.
  - mTLS headers: `CamelHttpSslClientCert` (leaf PEM) plus `...SubjectName/IssuerName/SerialNo/NotBefore/NotAfter` (`cameleer_tls::camel_support::client_cert_headers`).
  - Body: null when empty, a string for text or no content type with valid UTF-8, otherwise bytes. With `disableStreamCache` it is the stream handle.
- **Outbound** (`write_response`, 213-262):
  - On an exception: `CamelHttpResponseCode` or 500, with the `text/plain` exception text unless `muteException`.
  - A null or blank body → 204, otherwise 200 or `CamelHttpResponseCode`. `CamelHttpResponseText` is the reason phrase.
  - Headers are filtered by `filter_outbound`: `Camel*`, `org.apache.camel*`, hop-by-hop and framing headers, `accept-encoding`, `content-encoding` (33-59).
  - Objects and arrays → JSON (`application/json`). `Content-Type` falls back to the first `produces` entry.
  - `chunked` / `CamelHttpChunked` → chunked encoding. A stream body → `resp.stream`, chunked.
- Accepted-and-ignored Camel options: `platform.rs:171-185`.
- Port and host resolution: `port` option → `camel.component.<scheme>.port` → `camel.server.port` → 8080. Host: `camel.server.host` → `0.0.0.0` (`platform.rs:96-113`). `jetty:http(s)://host:port/path` takes them from the URI.

### 1.4 MCP Streamable HTTP (`crates/cameleer-ai/src/mcp/http.rs`, `sse.rs`)

- Registers a `RouteHandler` with `ServerConfig { name: "camel-mcp" }` (31-34). It handles POST, GET (the legacy SSE session stream, 166-175) and DELETE (`Mcp-Session-Id`). It returns 405 with `Allow` (127-129), checks `Origin` → 403 (107-110), and authenticates from `Authorization` / `X-API-Key`.
- **SSE**: `SseStream::new()` (`sse.rs:56-61`) creates a `sync_channel(256)` reader registered as a `stream_body` handle. `sse_response` (`http.rs:87-94`) sets `text/event-stream`, `Cache-Control: no-cache`, `X-Accel-Buffering: no` and `resp.stream`. The HTTP worker then writes the chunked stream until the channel closes. When the client disconnects, the server drops the reader, and `ChannelReader::drop` sets `open=false` (`sse.rs:48-52`). The next `send` then returns false.
  - Backpressure is a full channel: the writer sleeps 50 ms, then gives up (`sse.rs:64-81`).
  - Keep-alive comments come from a thread per stream (`sse.rs:100-110`).
- **Deferred replies**: `tools/call` runs on `call_pool()`. The HTTP worker blocks on `rx.recv()` until the first event, then answers either with JSON or by switching to SSE (`http.rs:242-277`). With the current server, every open SSE stream or pending call **holds one of the 200 HTTP workers**.
- Header checks in the 2026-07-28 era: `MCP-Protocol-Version`, `Mcp-Method`, `Mcp-Name` (base64 `=?base64?...?=`, 96-101).

### 1.5 gRPC and `H2Server` (D), `crates/cameleer-grpc`

- `H2Server::bind(addr, H2Config, Option<TlsServerConfig>, handler)` (`h2/server.rs:107-136`).
  - It uses plain `TcpListener::bind`, **not** `bind_listener`, so `localhost` binds IPv6 first on Windows.
  - One accept thread, **one thread per connection** (`h2-conn`, 127), **one reader thread and one writer thread per connection** (`conn.rs:336, 341`), **one thread per stream** (`h2-stream`, `server.rs:233`). Application threads block on condition variables. The design is not sans-io.
- TLS: `accept_tls` with `connect_timeout` (`server.rs:206-219`). It refuses an ALPN other than `h2` (`HTTP_1_1_REQUIRED`). Cleartext h2c with prior knowledge, or **HTTP/1.1 `Upgrade: h2c`** detected from the first 24 bytes (`conn.rs:219-228`, `upgrade.rs:65-108`).
- `serve_transport(Transport, H2Config, handler)` (`server.rs:242`) serves an externally accepted connection. Today only the cloud `MockServer` uses it.
- `H2Config` (`conn.rs:31-63`): `max_concurrent_streams` 256, `initial_window_size` 1 MiB, `connection_window_size` 4 MiB, `max_frame_size` 16384, `header_table_size` 4096, `max_header_list_size` 64 KiB, `connect_timeout` 10 s, `write_timeout` 30 s. `protocol_defaults()` sets 64 KiB−1 windows.
- `H2Stream` (`conn.rs:983-1250`): `send_headers`, `send_data[_timeout]` (blocks on flow control), `send_trailers`, `recv_headers`, `recv_data`, `read_body`, `reset`, `reset_code`, `is_remote_closed`, `peer_addr`, `is_tls`. gRPC needs trailers, streaming in both directions and RST_STREAM codes.
- Implemented: SETTINGS/ACK, HPACK, CONTINUATION, padding, flow control at connection and stream level, PING, GOAWAY (graceful `stop_graceful`), stream state machine, PRIORITY parsed and ignored. Not implemented: server push, RFC 8441 extended CONNECT.
- `GrpcServer::bind` (`grpc/server.rs:210`) runs on `H2Server` and spawns a `grpc-call` thread per call (274). It is used by the `grpc` consumer (`component.rs:1015`):
  - TLS from `keyCertChainResource`/`keyResource` with ALPN `h2`;
  - mTLS from `trustCertCollectionResource` → `ClientAuth::Need` (997-1010);
  - `maxConcurrentCallsPerConnection`, `flowControlWindow` → `H2Config` (`component.rs:200-207`);
  - `grpc-timeout` deadlines.
- The GCP Pub/Sub gRPC mock also uses it (`crates/cameleer-cloud/src/testing/gcp_more.rs:1035`).

### 1.6 Cloud mock servers (E), `crates/cameleer-cloud/src/testing/mod.rs`

cameleer-cloud **does not depend on cameleer-http**. It has its own HTTP/1.1 codec (`http.rs:647-780`: `read_request`, `ServerRequest`, `write_response`) and its own `MockServer` (91-215):
- `bind_loopback` (retries `WSAENOBUFS`/`EADDRINUSE` for up to 5 s, `net.rs:108-121`) and one thread per connection;
- keep-alive, a 30 s read timeout, `catch_unwind` → 500;
- `start_tls` uses a self-signed certificate and `accept_tls` (154);
- `start_with_h2` peeks for `PRI` and hands the socket to `cameleer_grpc::h2::serve_transport` (141-151). This is used by the Kinesis enhanced fan-out mock (`testing/aws.rs:191`).

The AWS, Azure and GCP mocks are built on this server and serve the hermetic cloud tests and the official-client tier (`sim_cloud_tls.rs`).

### 1.7 Websocket server (F), `crates/cameleer-protocols/src/websocket/mod.rs`

- A separate listener per host and port, **not on the shared HTTP server**. `websocket` defaults to `0.0.0.0:9292` and `vertx-websocket` to port 0. It uses `TcpServer` (`crates/cameleer-protocols/src/common.rs:192-240`: `bind_listener`, a nonblocking accept loop, a thread per connection).
- TLS (`wss`) comes from `sslContextParameters` / `useGlobalSslContextParameters` through `EndpointTls::accept` (312-321). It has its own handshake parser and checks `Sec-WebSocket-Key`/`-Protocol`, `allowedOrigins`, `maxMessageSize`/`maxFrameSize` (347-354, `protocol.rs`).
- A second websocket client lives in `crates/cameleer-amqp10/src/ws.rs` (AMQP over WebSockets, the `AMQPWSB10` subprotocol, Azure).
- simoom's plan does not mention websocket. If websocket endpoints should share `camel.server.port` like Camel's `platform-http` + `vertx-websocket`, that is **new** work, not a parity requirement.

### 1.8 Other servers out of scope

- `netty:tcp/udp` (`crates/cameleer-http/src/netty.rs:459`) is not HTTP.
- The LRA participant (`lra.rs:178`) is HTTP written by hand in core.
- `thrift_rpc.rs:727` is not HTTP.
- `cameleer-bench` `rawhttp`, `rawiocp` and `loadgen` are measurement tools.

---

## 2. HTTP clients (simoom has none)

| client | file | used by | features |
|---|---|---|---|
| `cameleer_http::HttpClient` | `crates/cameleer-http/src/client.rs:398-417`, pool 543-575 | `http`/`https` producer, SOAP producers (`soap/src/transport.rs` `Client`), couchdb/elasticsearch components, MCP client JWKS and introspection (`ai/src/mcp/auth.rs:311,347`), MCP client DELETE/POST (`mcp/client.rs:293-438`), Lambda Runtime API (`main/src/lambda.rs:94`), `cameleer` CLI (`cli.rs:675`) | keep-alive pool (`keep_alive`), proxy and `CONNECT`, `NO_PROXY`, SNI, hostname verification, ALPN `http/1.1` (`default_tls_config`, 312-315), redirects, chunked requests |
| low-level `open_stream` + `write_request` + `read_response_streaming` | `client.rs:320`, `http.rs:595-631` | SaaS REST clients (`saas/src/rest.rs:262-288`), MCP client SSE (`mcp/client.rs:603-647`) | streamed response bodies, SSE |
| producer options on top | `crates/cameleer-http/src/producer.rs` | `http` component | per-producer pool (`connectionsPerRoute` 20, `connectionClose`), gzip/deflate (`contentCompressionDisabled`, 294-296, 594-605), `httpVersion=HTTP/2` |
| cloud `HttpClient` | `crates/cameleer-cloud/src/http.rs:240-460` | every AWS, Azure and GCP component | process-wide pool `CloudPool` (30 s idle, 449-480), `send_streaming`, `send_to` (SSE via `cloud/src/sse.rs`), env proxies, `TlsStream` directly |
| HTTP/2 client `H2Connection` | `crates/cameleer-grpc/src/h2/client.rs:24-190` | gRPC producer (`grpc/client.rs:62-64`: prior knowledge, `Upgrade: h2c`, TLS with ALPN), http producer HTTP/2 (`producer.rs:418-420`), Kinesis enhanced fan-out (`cloud/src/aws/mod.rs:266-268`), Pub/Sub streaming and Lite (`gcp/pubsub_stream.rs`, `pubsub_lite.rs`) | multiplexed streams, trailers, RST_STREAM(NO_ERROR) after a complete response (INTEROP bug 20) |

**Consequence:** the HTTP/2 client and the server share `conn.rs` (`Shared::start(Role::...)`). If simoom-h2 replaces the server side, `cameleer-grpc`'s `h2` module must **stay for the client**, together with HPACK, frames and `Transport`. Removing it is possible only if simoom (or a sibling crate) grows a client. The same applies to `http.rs`: `HttpRequest`/`HttpResponse`/`write_request`/`read_response*` serve both directions and must survive the switch.

---

## 3. TLS usage

**Protocol engine** (replaceable in principle): `crates/cameleer-tls/src/tls/*` (TLS 1.3/1.2 state machines, `TlsStream`, `TlsServerConfig`, `TlsClientConfig`, ALPN, `ClientAuth`, session resumption), `crypto/*`, `x509.rs`, `ocsp.rs`. `TlsStream` wraps blocking `Read`/`Write` and has no sans-io record API (research.md §1). AES-GCM is bitsliced, with no AES-NI.

**Camel-specific configuration** (must stay in Cameleer whatever engine runs):
- `SslContextParameters` (`params.rs`, Camel `SSLContextParameters`, including `alpn`/`applicationProtocols`, 447).
- `camel_support.rs`:
  - `take_endpoint_ssl` (118): `sslContextParameters=#bean`, `camel.component.<scheme>.sslContextParameters`.
  - `client_config` (149) and `server_config_with_client_auth` (162).
  - `take_client_auth` (190): `needClientAuth`, `clientAuth=NONE|WANT|REQUIRE`.
  - `client_cert_headers` (272).
  - `server_ssl_enabled` (308): `camel.server.ssl.enabled` / `camel.server.sslEnabled`, `camel.server.ssl.*` key material, `camel.server.useGlobalSslContextParameters`.
  - Global `camel.ssl.*` (97-98, 435-442).
  - `endpoint_tls` (398) and the exception mapping (`SSLHandshakeException`).
- `keystore/*` (PEM, PKCS#12, JKS, JCEKS), `certgen`, `testing` (test PKI), `sasl.rs` (PLAIN, SCRAM: amqp10, datastores, kafka-mqtt; unrelated to HTTP).

**Network glue** (`net.rs`), the pieces that interact with any new server:
- `NetStream` (65-330): a cloneable plain-or-TLS stream whose clones share one `Arc<TcpStream>` (Windows timeouts are per handle). It provides `server`, `client`, `peer_certificates`, `alpn_protocol` and STARTTLS upgrades. Used by amqp, amqp10, dataformats, datastores, http, kafka-mqtt, protocols and sql.
- `accept_tls` (189) and `lingering_close` (167, `ALERT_LINGER` 2 s / 256 KiB): after a fatal alert, close gracefully so that Windows does not throw the alert away with a RST (INTEROP bug 18). Used by `NetStream::server`, the gRPC server and the cloud mocks. **simoom-tls must reproduce this**, or `mtls_resumption.rs alert_survives_a_client_that_reads_late` and the mTLS rejection tests will turn flaky on Windows.
- `shutdown_socket` (132, `CancelIoEx` on Windows), `bind_listener` (98, IPv4 first), `bind_loopback` (108), `connect_tcp` (83).
- `stream_body` (`crates/cameleer-tls/src/stream_body.rs`): a process-wide registry of `Box<dyn Read + Send>` behind `{"@stream": id}` handles. It is the only way to carry streaming bodies through `Value`. It is used by the http server and producer, binding, MCP SSE and FTP. **A simoom adapter must convert handles to streaming replies and streaming requests to handles.**

**TLS servers on HTTP paths today:**
- `HttpServer` (`server.rs:258-268`, via `NetStream::server`);
- `H2Server` (`accept_tls`);
- cloud `MockServer` (`accept_tls`);
- websocket (`EndpointTls::accept`).

Every server config comes from `camel_support` or from `TlsServerConfig::builder()` (gRPC).

---

## 4. Camel semantics that must be preserved

- **Consumer options** (`platform.rs:23-30, 152-185`): `httpMethodRestrict`, `matchOnUriPrefix`, `consumes`, `produces`, `muteException`, `chunked`, `mapHttpMessageBody`, `mapHttpMessageHeaders`, `mapHttpMessageFormUrlEncodedBody`, `disableStreamCache`, `port`, `host`, `ssl`, `sslContextParameters`, `useGlobalSslContextParameters`, `needClientAuth`/`clientAuth`. Ignored options: `transferException`, `useStreaming`, `headerFilterStrategy`, `fileNameExtWhitelist`, `useCookieHandler`, `returnHttpRequestHeaders`, `httpBinding`, `bridgeErrorHandler`, `exchangePattern`, `sessionSupport`, `handlers`.
- **Producer options** (`producer.rs:13-31`): `httpMethod`, `bridgeEndpoint`, `throwExceptionOnFailure`, `okStatusCodeRange`, `connectTimeout`, `socketTimeout`/`soTimeout`/`responseTimeout`, `followRedirects`, `copyHeaders`, `skipRequestHeaders`, `skipResponseHeaders`, `userAgent`, `authUsername`/`authPassword`/`authMethod`, `chunked`, `getWithBody`, `deleteWithBody`, `ignoreResponseBody`, `customHostHeader`, `preserveHostHeader`, `connectionClose`, `connectionsPerRoute`, `contentCompressionDisabled`, `httpVersion`/`protocolVersion`, `disableStreamCache`, the `proxy*` options, `useSystemProperties`. Unknown query parameters are forwarded to the target.
- **Headers** (`binding.rs:10-29`): `CamelHttpMethod`, `CamelHttpUri`, `CamelHttpUrl`, `CamelHttpPath`, `CamelHttpQuery`, `CamelHttpRawQuery`, `CamelHttpResponseCode`, `CamelHttpResponseText`, `CamelHttpChunked`, `CamelHttpRemoteAddress`, `CamelHttpSslClientCert*`, and the property `CamelHttpOperationFailed`.
- **Errors**:
  - An unhandled exception → 500, or `CamelHttpResponseCode` when set, with a text body unless `muteException`.
  - A handled exception → 200 with the handled body (`conformance_http.rs`).
  - A suspended consumer → 503.
  - Router errors 404/405/406/415, and 204 for OPTIONS.
  - Parse errors (`HttpError::status`, `http.rs:280-290`): 400 bad request, 408 timeout, 413 body too large, 431 headers too large, 417 unsupported `Expect`, 501 unknown transfer coding.
  - On the producer side, `HttpOperationFailedException` (`producer.rs:483-520`: `uri`, `statusCode`, `statusText`, `redirectLocation`, `responseBody`, `responseHeaders`), plus `ConnectException`, `SocketTimeoutException` and `SSLHandshakeException` kinds.
- **Server properties**:
  - `camel.server.port` and `camel.server.host`, with one shared port for platform-http, REST, the console, MCP and SOAP.
  - `camel.server.enabled`.
  - `camel.server.{devConsoleEnabled,healthCheckEnabled,metricsEnabled}` (dashed aliases accepted).
  - The `camel.server.ssl*` properties.
  - About 40 `camel.server.mcp-*` keys.
  - Nothing configures `ServerConfig` (pool size, limits, timeouts) from properties today. Camel's `camel.server.maxBodySize` and similar have no equivalent, so simoom brings no regression there, only an opportunity.
- **Lifecycle**: registration on route start, unregistration on stop, and the server stops when its last route is removed (port released, test `routes_share_a_port_and_release_it`). Port 0 gives one shared ephemeral port per component (`platform_http_port`, `lib.rs:93`). `local_port(ctx, uri)` returns the bound port.

---

## 5. Regression net (test files and `#[test]` counts)

Core HTTP, in `crates/cameleer-http`:
- unit tests: 22;
- `tests/http_components.rs` 12: round trips, 404/405, chunked on the wire, keep-alive and malformed requests, shared port and its release, proxying, 503 when suspended, **streaming request and response bodies**, pooled keep-alive;
- `conformance_http.rs` 10 (Camel semantics);
- `tls.rs` 10: https producer to jetty https, hostname mismatch, `camel.server.ssl.*`, global SSL, REST over https, **platform-http mTLS and client certificate headers**, CONNECT proxy, openssl `s_server`;
- `http_transport.rs` 2 (pooling and content coding, h2 prior knowledge);
- `rest_dsl.rs` 3, `netty.rs` 5, `service_call_http.rs` 1;
- gated tiers: `official_http2.rs` 3 (Go client against the Cameleer h2 server and the https consumer), `live_nginx.rs` 4, `live_java.rs` 1.

HTTP/2 and gRPC:
- `cameleer-grpc` unit tests: 20;
- `tests/h2.rs` 11: trailers, flow control, max streams, RST, RST(NO_ERROR), GOAWAY, raw protocol errors, ALPN, curl h2c, `serve_transport`, peer-closed leak;
- `grpc_component.rs` 13, `proto.rs` 7, `official_grpc.rs` 2;
- `cameleer-cloud/tests/aws_gaps.rs` 17, including `kinesis_enhanced_fan_out_over_http2`.

TLS:
- `cameleer-tls` unit tests: 74;
- `loopback.rs` 16, `mtls_resumption.rs` 11, `keystores.rs` 7, `openssl_interop.rs` 8, `openssl_mtls.rs` 4, `official_tls.rs` 2, `official_regressions.rs` 1, `public_site.rs` 1.

MCP, console and Main:
- `cameleer-ai/tests/mcp.rs` 18 (real HTTP and stdio, both eras, SSE), `mcp_client.rs` 6, `tools.rs` 22;
- `cameleer-main/tests/console.rs` 4, `mcp_main.rs` 3, `mcp_shop.rs` 3, `lambda.rs` 6, `tool_cli.rs` 2, `agent*.rs` 14;
- `docs_web.rs` 29;
- the docs harness files (`docs_guides` 66, `docs_data_saas` 65, `docs_cloud_aws_azure` 51, `docs_yaml` 39) start many `platform-http` and REST examples;
- `scripts/mcp-conformance.sh` (39/39 and 26/26).

Others on these servers:
- `cameleer-soap/tests/*` 25 (cxf, cxfrs, spring-ws, FHIR mock);
- `cameleer-saas/tests/connectors*.rs` 17 (SaaS mocks, webhook);
- `cameleer-protocols/tests/websocket.rs` 5;
- `cameleer-yaml/tests/yaml_routes.rs` and `yaml_eips.rs` (65, some on HTTP);
- `cameleer-core/tests/eip_saga.rs` 10 (LRA).

Grepping `crates/*/tests` for files that touch HTTP finds **about 660 tests in 52 files**. Gated tiers: `CAMEL_LIVE_INTEROP`, `CAMEL_OFFICIAL_CLIENTS`, `CAMEL_SIMULATORS`.

---

## 6. Performance-relevant facts

- **Worker model**: up to 200 on-demand workers, one connection per worker. A keep-alive connection is closed only when the pool is saturated. This is pinned by `server.rs` tests `keep_alive_survives_new_connections_while_workers_are_idle`, `keep_alive_is_closed_when_connections_wait_for_a_worker` and `more_concurrent_clients_than_the_old_32_workers_keep_their_connections` (472-555). Under simoom these tests describe the classic engine. They need equivalents (no forced reconnects under 64+ clients).
- **Blocking handlers**: every Camel route runs synchronously in `HttpHandler::handle`. MCP calls, SSE, CouchDB longpoll and `CamelHttp` proxy routes block for a long time. The offload pool and inline execution in plan §3.1 are therefore a requirement, not an option.
- **Windows workarounds**:
  - `shutdown_socket` (`CancelIoEx`), so that `stop()` returns quickly (test `serves_and_stops`: under 2 s with an idle keep-alive connection);
  - `bind_listener` (IPv4 first; `H2Server` does not use it);
  - one shared `Arc<TcpStream>` in `NetStream` for per-handle timeouts;
  - `bind_loopback` retries on `WSAENOBUFS`;
  - `lingering_close` for alerts.
- **Numbers** (`docs/BENCHMARKS.md`, DigitalOcean c-4, 64 keep-alive connections):
  - HTTP fixed reply: 99,012 req/s, against 56,735 for Camel JVM and 28,584 for Quarkus native (1.7x / 3.4x). p99 2.9 ms. 18 MB.
  - REST with JSON: 80,367 req/s (2.1x).
  - HTTP into Kafka: 48,800 req/s, p99 2.4 ms (after the Kafka fix).
  - HTTP into PostgreSQL: 11,007 req/s.
  - HTTP ceiling (`raw-http threads`): 166,374 req/s. Cameleer reaches 112,910 (68%) and Camel 67,682.
  - MCP session calls: 41,620/s, against 22,458 for Camel and 1,536 for the TypeScript SDK, at 53 MB.
  - The STATUS open gap: plain HTTP is 1.67x Camel; MCP CPU per request is about twice a plain request.
- **Cold start**: HTTP route ready in 1.17 ms (full, glibc) and 0.69 ms (musl core). An idle HTTP route has 2 threads (accept plus the on-demand pool). Any engine with a thread per core at startup must not lose this (idle threads round, STATUS).

---

## 7. Cargo features and size

- `cameleer-http` is a **non-optional** dependency of `cameleer-main` (`crates/cameleer-main/Cargo.toml`), so the HTTP/1.1 server and client are in the `core` build. simoom's HTTP/1.1 part lands in `core` and must fit the budget. Today `core` is **5.82 MB** glibc and 5.95 MB musl (STATUS; 5.93 MB in BENCHMARKS after the action layer).
- `cameleer-http` has the feature `http2 = ["dep:cameleer-grpc"]`, default on (`crates/cameleer-http/Cargo.toml`). `cameleer-main` maps `http2 = ["cameleer-http/http2"]` and the separate `grpc = ["dep:cameleer-grpc"]`. Both are in `full`. Without `http2`, `httpVersion=HTTP/2` fails and names the feature (`features.rs`).
- `cameleer-cloud` depends on `cameleer-grpc` **unconditionally** (for Pub/Sub and Kinesis HTTP/2). The `cloud` feature therefore pulls h2 in.
- `cameleer-ai`, `cameleer-saas`, `cameleer-soap` and `cameleer-datastores` depend on `cameleer-http` with `default-features = false` (no h2).
- TLS (`cameleer-tls`) is always in: it is a dependency of `cameleer-http`.
- Per plan §3.1, simoom-h2 belongs behind `http2`/`grpc`, and simoom-h3/QUIC behind a new feature that is not in `core`. `crates/cameleer-main/tests/features.rs` (14 tests) checks that each gate builds and that docs name their feature.

---

## 8. Riskiest dependencies (ranked)

1. **Blocking handler contract plus `stream_body`**:
   - `HttpHandler::handle` is synchronous and returns a complete `HttpResponse` whose body may be a `stream_body` reader that blocks (SSE).
   - Streaming requests hand over the rest of the socket, and the connection then closes.
   - An event-loop engine needs an offload pool, a reply-stream adapter with real backpressure (today a 256-message channel and a 50 ms give-up), and a request-body pipe.
   - MCP, `disableStreamCache` and CouchDB longpoll all depend on this.
2. **The h2 client and server share one code base** (`conn.rs`): replacing the server cannot delete `cameleer-grpc::h2`. gRPC semantics (trailers, `grpc-timeout`, RST codes, bug 20) must be re-tested on simoom-h2, as must h2c upgrade and prior knowledge on the same port.
3. **Shared registry quirks**: the first registrant's `ServerConfig` wins (console `pool_size` 8), http and https are rejected on one port, in-process Lambda dispatch, the per-component ephemeral port, 503 on suspend, and OPTIONS → 204. These live in `registry.rs` and `platform.rs` and must keep working whether routing runs in Cameleer or in the loop.
4. **TLS close and mTLS behaviour on Windows** (`lingering_close`, `CancelIoEx`, peer certificate chain → `CamelHttpSslClientCert*` headers). simoom-tls must expose the verified peer chain to the handler.
5. **Two HTTP/1.1 codecs**: cameleer-cloud has its own HTTP codec and mock server, outside cameleer-http. A switch that touches only cameleer-http leaves the mocks on the classic model. That is fine for tests, but bug fixes will not be shared.
6. **Size**: the HTTP/1.1 engine sits in `core` (5.82 MB). Anything simoom adds there (io_uring, IOCP back ends, timer wheel) counts directly against the budget.
