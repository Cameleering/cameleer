# simoom embedding study: the simoom side of a Cameleer integration

_Internal design note, 2026-11-09. Read-only study of `E:\Work\DataHQ\Code\simoom` at `1e50579`
(22 commits, 2026-11-07 to 2026-11-09). Paths below are relative to the simoom repository unless
they start with `crates/cameleer-`. Line numbers are from that commit. Nothing was built or run
for this note; behaviour described as "by reading" was not exercised._

## Summary

simoom can host Cameleer's blocking routes, but not inline. Its services run **on the event-loop
thread** and must return in microseconds. Blocking work has to leave the loop, either through
simoom's own offload pool (`Reply::blocking`, `Reply::offload`, `simoom::blocking`) or, the better
fit for Cameleer, through `Reply::defer()`: the service returns a `Send` `Responder` that Cameleer
answers from its own threads. The stall watchdog is a safety net, not a mechanism to build on.
Its defaults (1 ms CPU budget, 5 stalls in 1 s) never move a route that blocks for more than about
200 ms per call on one connection, and every stall before the move freezes all connections on
that loop.

The main API gaps for Cameleer:

- no TLS session information on the request (client certificate, SNI, ALPN, cipher);
- no local address or listener index on the request, and one service and one TLS config per
  server;
- no response trailers (gRPC needs `grpc-status`);
- no response deadline for deferred replies, and no access-log or per-request metrics hook;
- no TLS 1.2, PKCS#12, JKS, CRL or OCSP in simoom-tls;
- no HTTP client.

## 1. Public API for embedding

The facade is the `simoom` crate. Its public surface is re-exported in `crates/simoom/src/lib.rs:84-104`:

- the server types: `Server`, `Builder`, `ServerHandle`, `Stats`, `Limits`, `FrameBudgets`;
- the handler types: `Service`, `Outcome`, `Reply`, `Request`, `OwnedRequest`, `Response`,
  `Body`, `StaticReply`, `Router`;
- the cross-thread handles: `Responder`, `BodySink`, `BodySource`;
- WebSocket: `WebSocket`, `WebSocketHandler`, `WsHandle`, `WsMessage`;
- `tls::{ServerConfig, CertifiedKey, certgen}` behind the `tls` feature.

The crate docs say the API is "synchronous at its core and needs no async runtime"
(`lib.rs:3-5`).

### Starting, registering, stopping

- `Server::builder()` returns a `Builder` (`server.rs:283`). `Server::from_listeners(Vec<TcpListener>)`
  serves sockets the host already bound (`server.rs:288`), which is useful for socket activation
  or a shared port.
- Listeners:
  - `listen(addr)`: plain HTTP/1.1, plus h2c when HTTP/2 is on (`:373`);
  - `listen_tls(addr)` (`:380`);
  - `listen_on(TcpListener)` and `listen_on_tls(...)`;
  - `listen_quic(addr)` for HTTP/3.

  `tls(ServerConfig)` (`:410`) sets **one** TLS config for every TLS listener, and the server
  overwrites its ALPN list with `h2,http/1.1` (`:493-499`). `quic_tls` sets the config for
  HTTP/3.
- `service(impl Service)` (`:471`) takes **one** service for the whole server; `start()` fails
  without one (`:490`).
- Tuning:
  - `threads(n)`: loop threads, 0 means one per CPU (`:439`);
  - `backend(Backend)`;
  - `limits(Limits)`;
  - `http2(bool)`;
  - `runtime(|&mut simoom_rt::Config| ..)` for raw runtime settings (`:477`);
  - `log(Fn(&str))` for watchdog warnings, which go to stderr by default (`:483`).
- `start()` binds every listener, starts the loops and returns a `ServerHandle` (`:489-614`).
  The handle offers:
  - `local_addr()`, `local_addrs()` and `quic_addrs()`. Port 0 works, and the bound ports are
    reported in `listen` order (`:620-635`).
  - `stats()` (`:648`).
  - `stop(grace)`: graceful stop. Listeners close; HTTP/1.1 connections close after their
    current response with `Connection: close`; HTTP/2 and HTTP/3 connections get GOAWAY.
    Whatever is still open after `grace` is aborted (`:672-679`).
  - `join()` (`:682`).
  - Dropping the handle stops the server at once.

The minimal embedding, a blocking handler suitable for a Camel route:

```rust
use simoom::{Server, Response, OwnedRequest, Outcome, Reply, Request};
use std::time::Duration;

let server = Server::builder()
    .listen("0.0.0.0:0")                 // port 0: ask the handle for the port
    .threads(1)
    .service(simoom::blocking(|req: OwnedRequest| {
        // runs on simoom's offload pool; may block
        Response::text(format!("hello {}", req.path_str()))
    }))
    .start()?;
let port = server.local_addr().port();
// ...
server.stop(Duration::from_secs(5));
server.join();
```

The shape Cameleer would more likely use: defer the reply and hand the exchange to its own
executor.

```rust
.service(move |req: &mut Request<'_>, reply: Reply<'_>| -> Outcome {
    let owned = req.to_owned_request();    // copies head + buffered body, takes the BodySource
    let responder = reply.defer();         // Send; answer from any thread, exactly once
    camel_executor.execute(move || {
        let resp = run_route(owned);       // blocking Camel exchange
        responder.send(resp);              // false if the client went away
    });
    Outcome::Deferred
})
```

### Handler signature

`trait Service: Send + Sync + 'static { fn call(&self, req: &mut Request<'_>, reply: Reply<'_>) -> Outcome; }`
(`service.rs:39-50`). Any `Fn(&mut Request, Reply) -> Outcome` closure is a service
(`service.rs:62-69`). The call is **synchronous and runs on the loop thread** (`service.rs:19-21`).
Panics are caught: the request gets a 500 and its connection closes.

- **`Request<'a>`** (`request.rs:23-43`) is borrowed from the connection for the duration of the
  call and is zero-copy on HTTP/1.1. It offers:
  - `method`, `target`, `path`, `path_str`, `query`, `query_param`, `version`, `host`;
  - `header(name)` (case-insensitive) and `headers()`;
  - `body()` for a buffered body; `is_streamed()` and `take_body() -> Option<BodySource>`;
  - `trailers()` and `trailer()`;
  - `param()` for Router captures;
  - `peer_addr()`, `is_secure()`, `is_websocket_upgrade()`, `loop_index()`;
  - `to_owned_request()` (`:227`), which copies the head and the buffered body and moves the
    body source, so the request can cross threads.
- **`Reply<'a>`** (`reply.rs:93-287`) is consumed by exactly one finisher. Dropping it unanswered
  sends a 500. The builders are `status()` and `header()`; the finishers:
  - immediate: `body`, `body_static`, `body_copy`, `empty`, `send(Response)`,
    `fixed(&StaticReply)`;
  - `defer() -> Responder`;
  - `stream() -> BodySink` and `sse() -> BodySink`;
  - `offload(req, FnOnce(&mut Request, Reply))` and `blocking(req, FnOnce(OwnedRequest) -> Response)`;
  - `websocket(handler)` and `websocket_with_protocol`.

  Framing headers (`content-length`, `transfer-encoding`) are written by the server and ignored
  if set (`types.rs:244-246`). `connection: close` closes the connection after the response. An
  invalid header name or value turns the response into a 500 (`reply.rs:118-128`).
- **`Response`** (`types.rs:219-280`) is an owned status, headers and `Body`
  (`Empty | Static(&'static [u8]) | Bytes(Vec<u8>)`). It is what `Responder::send` and
  `blocking` take.

### Streaming bodies

- **Requests.** Bodies up to `Limits::body_buffer` (64 KiB) are buffered whole. Larger ones, or
  chunked bodies of unknown length, stream through a `BodySource` (`handle.rs:395-488`), which is
  `Send`. It provides:
  - `io::Read` (blocking; never call it on a loop thread);
  - `try_read`, and `poll_read` with a `Waker`;
  - `is_complete`;
  - `trailers()`.

  Backpressure is pull-based: the connection stops reading the socket while the source holds a
  full buffer. `100-continue` is sent only when the service has taken the body source
  (`h1.rs:377-382`).
- **Responses.** `BodySink` (`handle.rs:160-285`) provides:
  - `write` (blocks while `Limits::stream_buffer`, 64 KiB, is pending), `try_write`
    (`WouldBlock`, safe on a loop thread), and `poll_ready` / `ready()` as a future;
  - `send_event(&str)` for SSE `data:` lines;
  - `is_closed`, `finish` (also on drop) and `abort`;
  - `io::Write`.

  The body is chunked on HTTP/1.1 and DATA frames on HTTP/2 and HTTP/3. Each write copies the
  slice into a `Vec` and posts it to the loop as a message (`handle.rs:176-185`).
  `Responder::stream(head)` starts a stream from another thread (`handle.rs:83`).

### Headers, trailers, SSE, upgrade, connection info, limits, shutdown

- **Request trailers** reach the service on all three protocols (BUGS.md item 14).
- **Response trailers: not supported.** There is no way to send them through `Reply`,
  `Response`, `Msg` or `BodySink` (`handle.rs:30-44`). The sans-io codec has
  `simoom_h2::Connection::send_trailers` (`simoom-h2/src/conn.rs:1476`), but the facade never
  calls it. **This blocks gRPC** (`grpc-status` and `grpc-message` are trailers).
- **SSE and long-lived responses.** `Reply::sse()` sets `content-type: text/event-stream` and
  `cache-control: no-cache` (`reply.rs:200`). There is no SSE `id:`, `event:` or `retry:` helper,
  but `write` takes raw bytes. `write_timeout` (30 s) closes only a connection whose output
  makes no progress, so an idle stream is not cut.
- **Upgrade.**
  - WebSocket (RFC 6455) runs on HTTP/1.1 only, through `Reply::websocket`. The handler runs on
    the loop thread; `WsHandle` sends from other threads. There is no permessage-deflate
    (`simoom-http1/src/ws.rs:7`) and no WebSocket over HTTP/2 (RFC 8441 is a known gap).
  - h2c works both with prior knowledge and through `Upgrade: h2c` (`h1.rs:353`, `:547`).
  - There is no generic upgrade or tunnel: `CONNECT` closes the connection after its response
    (`h1.rs:337-341`).
- **Connection info.** The request has `peer_addr()` and `is_secure()` (`request.rs:205-213`),
  and nothing else. There is no local address, no listener index (the runtime has
  `ConnInfo::listener`, `simoom-rt/src/handler.rs:15-23`, but the facade drops it), and no TLS
  data (SNI, ALPN, cipher, peer certificates). simoom-tls has `server_name()`,
  `alpn_protocol()`, `negotiated_cipher_suite()` and `peer_certificates()`
  (`simoom-tls/src/tls/conn.rs:582-599`), but `crates/simoom/src` never reads them except ALPN
  for protocol selection (`conn.rs:406`).
- **Limits** are per server, not per request or route (`limits.rs:8-99`):
  - sizes: request line, head size, header count and line length; `max_body` (10 MiB, a 413
    before any body byte is read);
  - timeouts: `head_timeout`, `keep_alive_timeout`, `min_body_rate` and `body_rate_grace`,
    `write_timeout`;
  - connection caps: total, per loop and per IP; `max_requests_per_connection`;
  - HTTP/2 and HTTP/3: stream concurrency, header list size and table size, and the HTTP/2
    frame budgets;
  - `max_ws_message`;
  - the watchdog fields;
  - `shed_lag` (admission control, off by default);
  - the offload pool: threads, queue and CoDel.

  There is **no handler or response deadline**: a deferred `Responder` can hold a request open
  forever. The h1 timer covers only the head, the body rate and keep-alive (`h1.rs:715-765`).
- **Graceful shutdown**: see `ServerHandle::stop` above.

## 2. Runtime and threading model (the key question)

`simoom-rt` (`simoom-rt/src/lib.rs:1-71`) runs **one event loop per core, shared-nothing**,
completion style. Each loop owns:

- an I/O driver from `simoom-sys`: epoll (edge-triggered) or io_uring on Linux, IOCP on
  Windows, kqueue on macOS and BSD;
- a buffer pool and a timer wheel;
- its connections.

`Backend::Auto` resolves to epoll on Linux unless `SIMOOM_BACKEND=io_uring` is set
(`simoom-sys/src/driver/mod.rs:50-62`). Accepting differs by platform. On Linux each loop has
its own `SO_REUSEPORT` listener. On Windows loop 0 posts `AcceptEx` calls and hands each
connection to the less busy of two loops. On macOS one shared listener is handed out the same
way.

**Busy polling** is adaptive (`DriverConfig::busy_poll`, default 30 µs, `driver/mod.rs:130-154`,
overridable with `SIMOOM_BUSY_POLL_US`). After a turn that delivered events the loop spins only
while events keep arriving sooner than a quarter of that window. An idle server sleeps in the
kernel and costs no CPU.

**Threads.**

- Loop threads: `threads(0)` gives one per CPU (honouring the cgroup quota), and `threads(n)`
  fixes the count.
- The facade turns on `lazy_loops` (`server.rs:546-551`): `start()` runs only loop 0, and the
  other loops start in the background once loop 0 holds three connections at once
  (`simoom-rt/src/runtime.rs:58-70`). The exception is a server with HTTP/3, which starts every
  loop at once, because a socket added later to a `SO_REUSEPORT` group would steal established
  QUIC flows.
- The offload pool (`simoom-rt/src/offload.rs`) starts workers on demand, up to
  `offload_threads` (0 means twice the CPU count, at least 4). Idle workers exit after 10 s.
- A short-lived `-start` helper thread exists while lazy loops come up (`runtime.rs:556`).

So **an idle simoom server costs one loop thread**, plus the host's main thread. That meets
Cameleer's goal of two threads for an idle HTTP route. Every `Server` has its own runtime and
offload pool, though, so N separately built servers cost N idle loop threads.

**Blocking is not allowed on a loop thread** (`service.rs:19-26`). A blocked loop freezes every
connection pinned to it. There are three ways out:

1. **`Reply::defer()` → `Responder`** (`reply.rs:187`, `handle.rs:62-98`). The service returns
   at once and the host answers from any thread. `Responder::send` posts the response to the
   connection's loop through a `ConnHandle` message. On HTTP/1.1 the connection reads no further
   requests until the deferred response is sent, which keeps replies in order. HTTP/2 and
   HTTP/3 streams are independent, up to `max_concurrent_streams` (100). **This is the natural
   fit for Cameleer:** Cameleer keeps its own executor, its thread naming and its
   backpressure, and simoom's CoDel is bypassed.
2. **`simoom::blocking(f)` / `Reply::blocking` / `Reply::offload`** (`service.rs:93-119`,
   `reply.rs:207-267`). The request is copied into an `OwnedRequest` and run on the shared
   offload pool. When the queue is full (4096) or CoDel reports overload, the client gets a fast
   **503 with `Retry-After: 1`** (`reply.rs:253-265`). CoDel declares overload once jobs wait
   longer than `offload_target` (5 ms) for a whole `offload_interval` (100 ms)
   (`offload.rs:226-230`). With the defaults this is wrong for integration routes. On an 8-CPU
   box there are 16 workers, so routes that block for 100 ms saturate at about 160 req/s, and
   then queueing beyond 5 ms produces 503s. Cameleer's thread-per-connection server today
   allows 200 concurrent workers. Using this path needs `offload_threads` set to Cameleer's
   worker count and `offload_target` raised to the route SLO (seconds). Even then, 503
   load-shedding is a behaviour change that Camel users don't expect.
3. **The stall watchdog** (`watchdog.rs:1-12`, `limits.rs:64-79`). A call over 1 ms of wall time
   makes its route suspect. Its next 16 calls are measured with the thread's CPU clock and its
   voluntary context switches, and a call counts as a stall if it used more CPU than the budget
   or blocked (`watchdog.rs:79-91`). After 5 stalls within 1 s the route moves to the offload
   pool, and it comes back after 64 fast calls. Do not rely on it for Cameleer:
   - every call before the move blocks the whole loop;
   - the 5-in-1-s window means a route that blocks 250 ms or more per call on one connection
     never collects 5 stalls in a window, so it stays inline (by reading the ring logic; not
     tested);
   - routes are tracked per `Router` route, or for a custom `Service` by hashing the path into
     64 slots (`server.rs:218-219`).

   It remains useful as protection against a Cameleer bug that does real work inline.

**Recommended pattern for Cameleer.** Run a thin inline service that:

- answers 404, 405 and CORS preflight on the loop;
- calls `to_owned_request()` and `defer()`;
- submits the exchange to Cameleer's existing worker pool;
- streams responses with `Responder::stream` and `BodySink::write`, which block the Cameleer
  thread, not the loop;
- reads large request bodies with `BodySource`'s blocking `io::Read` on the Cameleer thread.

The cost per request is one copy of the head and the buffered body, plus two cross-thread
messages (job in, response out). That is what the offload path costs too: the 2026-11-07
campaign measured offloaded calls at 34-35 µs CPU against 22.65 µs inline
(`bench/results/2026-11-07-campaign/analysis.md`, item 1). This is still below
`cameleer-http`'s 23 µs per request on WSL, but by less than headline numbers suggest. **The
deferred path has not been benchmarked**; measure it before committing.

## 3. TLS

### API (`simoom-tls`, sans-io, TLS 1.3 only)

- **Keys.** `CertifiedKey::new(chain_der, PrivateKey)`, `from_pem(cert_pem, key_pem)` and
  `from_der` (`tls/config.rs:31-73`). Keys can be PKCS#8, SEC1 or PKCS#1. Supported key types:
  - ECDSA P-256 always;
  - RSA, P-384 and Ed25519 as features (`simoom` `tls` turns RSA on; `tls-extra` adds the rest).
- **`ServerConfig`** (`tls/config.rs:188-256`):
  - `alpn`, which the facade overwrites;
  - `client_auth: ClientAuth::{None, Optional(RootStore), Required(RootStore)}` (`:80-88`), which
    matches Camel's `NONE`, `WANT` and `NEED`;
  - `suites`, a selectable list of the three TLS 1.3 suites;
  - `groups`: X25519 and P-256 only;
  - `ticketer: Option<Arc<Ticketer>>`, rotating ticket keys (default 6 h) for PSK-DHE
    resumption;
  - `ticket_lifetime_secs`, `ticket_count`;
  - `add_sni_cert(CertifiedKey)` for SNI selection (wildcards included) and `strict_sni`;
  - `max_handshake_message`;
  - a pluggable `time` source.
- **`RootStore`**: `empty()`, `system()`, `add_pem`, `add_pem_file`, `add_der`
  (`x509.rs:354-456`). Path validation (`verify_chain`, `x509.rs:490`) checks hostnames and EKU.
- **0-RTT** is never offered or accepted, on purpose (`lib.rs:51-52`, SECURITY.md known gaps).
- **QUIC interface**: `quic::QuicConnection` with per-level handshake messages, header
  protection, key update, Retry tags, and QUIC v1 and v2.
- Also: KeyUpdate, the exporter, and `export_traffic_secrets` for a future kTLS.
- The TLS client (`ClientConfig`, `ClientConnection`, a session cache) sits behind the `client`
  feature.

### Missing compared with Camel's `SslContextParameters` (and with cameleer-tls)

`crates/cameleer-tls` has `keystore/{jks.rs,pkcs12.rs}`, `pbe.rs`, `ocsp.rs`, `params.rs`
(SslContextParameters) and TLS 1.2. simoom-tls lacks:

- **TLS 1.2**. The `tls12` feature is planned, not built; clients that offer only TLS 1.2 get
  `protocol_version`. Older Java 7 clients, some IoT devices and some load balancers are
  affected.
- **PKCS#12 and JKS keystores**, and password-protected PEM keys. A bridge is easy:
  cameleer-tls decodes the keystore, and the leaf chain DER plus a PKCS#8 key go to
  `CertifiedKey::from_der`.
- **Cipher selection** for TLS 1.2 suites (moot without 1.2), and named-group selection beyond
  X25519 and P-256 (no P-384 or ML-KEM hybrid groups).
- **CRL and OCSP**: no revocation checking of client certificates and no OCSP stapling
  (SECURITY.md known gaps; testssl notes "no CRL or OCSP URI").
- **Per-listener TLS config** (a facade limit) and **certificate reload** without a restart:
  the config is cloned into an `Arc` at `start()` (`server.rs:493-500`).
- **Client-certificate details on the request.** Camel's `CamelHttpServletRequest`-style access
  to the peer certificate is impossible today.

### Origin

simoom-tls was ported from cameleer-tls, then extended. By verbatim long-line overlap (lines of
40 or more characters):

- nearly identical:
  - `x509.rs` (177/181 long lines found in cameleer-tls `x509.rs`);
  - `asn1.rs` (76/78);
  - `pem.rs` (37/37);
  - `certgen.rs` (52/56);
  - `keys.rs` (132/165);
  - `crypto/rsa.rs` (89/102);
  - `crypto/ec.rs` (62/82).
- partly shared:
  - `sha2.rs` (47/118, from `hash.rs`);
  - `ed25519.rs` (44/95);
  - `bigint.rs` (36/69).
- mostly new:
  - `chacha.rs` (24/143);
  - `tls/mod.rs` (26/102), `tls/server.rs` (16/178), `tls/keyschedule.rs` (15/60);
  - `x25519.rs` (15/54);
  - `crypto/p256.rs` (13/262, a new MULX/ADX field implementation with a comb table);
  - the whole sans-io connection and engine (`tls/conn.rs`, `tls/engine.rs`), `quic.rs`, and
    the AES-NI, PCLMUL and SHA-NI intrinsics in `simoom-sys/src/arch.rs`.

The two have **diverged substantially in the protocol engine**: cameleer-tls is stream-based
with TLS 1.2, simoom-tls is sans-io and TLS 1.3 only, with QUIC. They still share the
certificate, ASN.1 and RSA code, so a fix to X.509 path validation now has to land twice.

## 4. Features, size, MSRV, unsafe, platforms

`crates/simoom/Cargo.toml` features:

- default: `http1`, `http2`, `websocket`, `io-uring`, `watchdog`;
- `tls` (adds RSA) and `tls-extra` (P-384, Ed25519, ChaCha20);
- `http3` (pulls in `tls`, QUIC and UDP);
- `tls-client`, the alias `ws`, and `full`.

simoom-tls's own defaults are `client`, `rsa`, `p384`, `ed25519` and `chacha20`, and
`--no-default-features` is the smallest set.

**Size budgets** (plan.md §0 item 4; `scripts/size.sh` measures `examples/hello.rs`, stripped,
`dist` profile, minus an empty `main`, on x86-64 Linux):

| build | budget | last measured |
|---|---|---|
| HTTP/1.1 only | 250 KB | 218,264 B |
| HTTP/1.1 + HTTP/2 + TLS 1.3 | 600 KB | under budget (BUGS.md item 22) |
| everything, HTTP/3 included | 1 MB | 1,040,024 B, only 8.5 KB of headroom |

The `full` row is close to its limit, so any added facade feature (TLS info, trailers, an
access-log hook) will press on it.

**MSRV**: `rust-version = "1.89"` (`Cargo.toml`). The third-party suites were built with
Rust 1.99.1. Cameleer's workspace declares no MSRV.

**`unsafe`**: allowed only in `simoom-sys`. Every other crate's `lib.rs` has
`#![forbid(unsafe_code)]` (CLAUDE.md rule 2), and the workspace denies `unsafe_op_in_unsafe_fn`.
`simoom-sys` holds the sockets, the drivers, mmap, the SIMD scan and the crypto intrinsics.
SECURITY.md notes that no Miri, ASan or TSan run has been done.

**Platforms**:

- Linux, epoll and io_uring: tested, benchmarked and run against the third-party suites.
- Windows, IOCP: the hermetic tests pass (CLAUDE.md rule 3).
- **macOS and kqueue: compile-checked only.** The ARM SIMD and crypto paths have never run on
  ARM hardware (BUGS.md, known gaps). This matters for Cameleer's linux/arm64 container image.

## 5. Maturity

- **Age**: three days of history (2026-11-07 to 2026-11-09), 22 commits. Version `0.1.0`, not
  tagged as a release. **Not on crates.io**: plan.md §0 item 8 says Cameleer depends on it by
  git tag until the owner decides to publish. The repository is private, so Cameleer's public
  CI and crates.io publishing of `cameleer-http` would need simoom to be published or vendored
  first.
- **Tests**: about 638 `#[test]` functions by grep:
  - simoom-tls 126, simoom (facade) 94, simoom-quic 86, simoom-h2 84, simoom-http1 79;
  - simoom-sys 47, simoom-rt 34, simoom-qpack 24, simoom-bench 23, simoom-h3 22, simoom-hpack 19.

  No suite total is recorded in docs.
- **Conformance** (SECURITY.md "Third-party suites", 2026-11-09):
  - h2spec 2.6.0: 146/146 over h2c in 30 consecutive runs. h2 over TLS can't run, because h2spec
    offers only TLS 1.2; the in-repo list in `tests/h2_live.rs` covers 120/120 over both.
  - HTTP Garden as an origin: 0 desyncs, 0 dangerous discrepancies (HTTP/1.1 only; the
    `h2fanout` run was not done).
  - QUIC Interop Runner against the ngtcp2, quiche and quic-go clients: passes except `ecn`,
    `zerortt` and `connectionmigration`, which are by design or not built.
  - testssl.sh: TLS 1.3 only, no vulnerabilities found.
  - openssl, curl, gnutls, python `ssl` and Schannel interop: all pass, including client
    certificates (good, rogue, missing) and resumption.
  - nghttp and h2load: 14/14.
  - A desync differential behind nginx and haproxy: 0 desyncs.
- **Hardening**:
  - every limit is bounded and tested (`tests/limits.rs`, 11/11);
  - HTTP/2 flood budgets cover CVE-2019-9511..9518, Rapid Reset and MadeYouReset;
  - slowloris defences hold partial heads without a buffer and pace head timeouts;
  - lingering close;
  - std-only fuzzing: 2 h 40 min of mutation fuzzing over six harnesses, and coverage-guided
    fuzzing of HTTP/1 only, Linux only;
  - a security pass fixed 14 engine bugs (commit `26bf6a5`);
  - no external audit.
- **Open issues**: BUGS.md lists all 22 items as closed (2026-11-09). The known gaps are:
  - QUIC: ECN, 0-RTT, CUBIC, migration, DATAGRAM;
  - HTTP/2: RFC 8441 and priorities;
  - TLS: TLS 1.2, kTLS, OCSP and CRL, P-384 key exchange;
  - facade: an access-log hook and HTTP/2 PING keepalive;
  - platforms: macOS and ARM never run.
- **Benchmarks**:
  - `bench/results/2026-11-07-campaign/` (`analysis.md`, `summary.md`): DigitalOcean c-4,
    against hyper, axum, actix, nginx, h2o and others. The NIC limits everyone at 160-180k
    req/s. simoom led TLS handshakes/s (8.6k), HTTP/1.1 over TLS (114k) and HTTP/3 (82k), had
    the lowest CPU per request at 60k req/s (29.7 µs), and trailed on plain HTTP/1.1 at high
    connection counts. Fixes landed afterwards (adaptive busy polling, the watchdog redesign,
    one io_uring enter per turn), but **the campaign has not been rerun**. BUGS.md is titled
    "before the second DigitalOcean campaign".
  - `docs/STATUS.md` "Start-up and memory against cameleer-http" (2026-11-09):
    - cold start is within 5% or faster on WSL and Windows, plain and TLS;
    - TLS PSS is +8-9% (file-backed code pages);
    - 1,000 idle connections use 2.4-2.7 MB against `cameleer-http`'s 7.6-7.7 MB;
    - on WSL with 64 connections simoom does 293-311k req/s against `cameleer-http`'s
      172-173k, with p99 2-2.7x lower and 43% less CPU per request.
- **API stability**: the API is still changing. Since the facade commit (`1d03172`) the
  watchdog fields, `shed_lag`, `h2_frame_budgets`, the trailer APIs and `Config::linger` were
  added, and `#[doc(hidden)] WatchRef` leaks into the `Service` trait. Expect breaking changes
  until a tag.

## 6. Gaps for Cameleer

| need | simoom today | severity |
|---|---|---|
| Blocking route execution | must `defer()` or offload; never inline | design constraint, solvable |
| Offload pool tuned for 100 ms-plus calls | defaults: 2×CPU threads, CoDel 5 ms, 503 on overload | high if the offload path is used; avoid by deferring to Cameleer's pool |
| Response deadline and timeout for deferred replies | none | medium: Cameleer must time out and send a 504/503 itself |
| Client-disconnect notification during a deferred call | only `Responder::send` returning `false`, or `BodySink::is_closed` | low |
| TLS info on the request (peer certificate chain, SNI, ALPN, cipher, protocol) | none | **high** for mTLS routes and Camel's `CamelHttpSslSession`-style headers |
| Local address and listener index on the request | none (`ConnInfo::listener` exists in rt) | **high** if several ports share one server |
| Several listeners with different services or TLS configs | one service and one TLS config per `Server`; separate servers mean separate loop threads and pools | medium |
| Dynamic handler registration while running | `service` is fixed at `start()` and `Router` is build-once (`router.rs:81`) | low: Cameleer does its own routing, with a service that reads its route table through `RwLock` or `Arc` swap |
| Path-prefix routing | `Router` has exact paths, `:param`, `{param}` and `*rest`, plus fallback | none: Cameleer keeps its own matcher |
| Response trailers | none in the facade | **high** for gRPC on simoom (replacing cameleer-grpc `h2`) |
| HTTP/2 PING keepalive, extended CONNECT, priorities | none | low (gRPC keepalive pings from servers) |
| WebSocket over HTTP/2, permessage-deflate | none | low |
| Compression (gzip/deflate) | none | medium: Cameleer has to compress itself (std-only deflate exists in Cameleer's data formats) |
| CORS, multipart, cookies | none (by scope) | none: Cameleer already implements them above the server |
| Request body size limit | `max_body` per server (10 MiB default) | low: Camel per-endpoint limits must be enforced by Cameleer, with the server cap set high |
| Metrics hooks | polling `stats()` counters only; no per-request callback | medium for micrometer-style per-route timings (Cameleer can time inside its own service) |
| Access logging | none (known gap) | medium: Cameleer has to log from its service, missing bytes-written and 4xx parse errors |
| Bind address, port 0, local port reported | yes (`local_addrs`) | none |
| Multiple listeners | yes, any number of TCP, TLS and QUIC | none |
| `from_listeners` / socket activation | yes | none |
| Cleartext h2c, h2 over ALPN | yes | none |
| HTTP/3 | yes (`http3` feature, +~400 KB) | optional |
| TLS 1.2, PKCS#12, JKS, CRL, OCSP, cert reload | none | **high** for SslContextParameters parity; keystores bridgeable through cameleer-tls |
| HTTP client (the `http`, `http2` and gRPC producers) | sans-io parts only: `simoom_http1::parse_response`, the `simoom_h2::Connection::client` role, the TLS client, the QUIC client; no runnable client | high if Cameleer wants to drop cameleer-grpc `h2` entirely: its client side stays |
| Log routing | `Builder::log` callback (stall warnings only) | low |
| ARM and macOS verified | not run | medium (Cameleer ships a linux/arm64 image) |
| crates.io availability | private repo, git tag only | **high** for Cameleer's crates.io releases |

## 7. Duplicate-code origin

Judging by the code (verbatim long-line overlap, computed against `crates/cameleer-tls`,
`cameleer-grpc`, `cameleer-http` and `cameleer-bench`) and the history: every crate arrived
whole in its first commit, with no `--follow` history before that, and no commit message names
its source.

| simoom module | came from | how much |
|---|---|---|
| `simoom-tls/src/{x509,asn1,pem,certgen}.rs` | `cameleer-tls/src/{x509,asn1,pem,certgen}.rs` | copied, light edits (93-100% of long lines) |
| `simoom-tls/src/keys.rs`, `crypto/rsa.rs`, `crypto/ec.rs` | same names in cameleer-tls | copied and adapted (75-87%) |
| `simoom-tls/src/crypto/{sha2,ed25519,bigint}.rs` | `cameleer-tls/src/crypto/{hash,ed25519,bigint}.rs` | partly ported (40-52%) |
| `simoom-tls/src/crypto/{chacha,x25519,p256}.rs`, `tls/*` | cameleer-tls | mostly rewritten (5-28%); p256 is new MULX/ADX code; the TLS engine is a new sans-io TLS 1.3 design (commit `60345ef`) |
| `simoom-sys/src/arch.rs` (AES-NI, PCLMUL, SHA-NI) | new; a few constants from cameleer-tls `hash.rs` | new |
| `simoom-hpack/src/huffman.rs` | `cameleer-grpc/src/hpack/huffman_table.rs` | the table copied (64/96); the RFC 7541 test vectors also match `hpack/mod.rs` |
| `simoom-h2/src/frame.rs` | `cameleer-grpc/src/h2/frame.rs` | partly (24/80); the connection state machine (`conn.rs`) is new sans-io code (commit `82aa21e`), not the thread-based cameleer-grpc `h2` |
| `simoom-http1` | `cameleer-http/src/http.rs` | not ported: a new SIMD parser. Cameleer's parser appears only as a baseline copy in `simoom-http1/examples/bench_parse.rs:111-115` |
| `simoom-sys` sockets (`net.rs`, `os/windows.rs`) | patterns from `cameleer-tls/src/net.rs` | no verbatim overlap above threshold; rewritten |
| `simoom-bench` (histogram, load generator) | `cameleer-bench` | no verbatim overlap above threshold; rewritten (open-loop, HdrHistogram-style) |
| `simoom-quic`, `simoom-qpack`, `simoom-h3`, `simoom-rt` | none (Cameleer has no QUIC or event loop) | new |

If Cameleer adopts simoom-tls under cameleer-tls, the certificate, ASN.1, PEM, key and RSA code
exists twice today and could be shared. Who depends on whom has to be settled first:
cameleer-tls has the keystores, TLS 1.2 and OCSP; simoom-tls has the faster primitives and QUIC.

## 8. Riskiest points

1. **Threading mismatch.** Inline blocking is fatal for a loop. Every Cameleer HTTP consumer must
   go through `defer()` with Cameleer's pool, or through a retuned offload pool. The deferred
   path is unmeasured, and its per-request cost (a copy plus two cross-thread messages) erodes
   the headline gains.
2. **Offload pool defaults return 503s** under ordinary slow-backend load (CoDel 5 ms target,
   2×CPU threads).
3. **No TLS session data and no response trailers**: mTLS routes and gRPC can't move without
   facade changes, and the `full` build has only 8.5 KB of size headroom left.
4. **TLS 1.2, keystores and revocation are missing** in simoom-tls: SslContextParameters parity
   needs cameleer-tls to stay in the loop.
5. **Maturity and distribution**: three days old, API still moving, private, unpublished, the
   post-fix benchmark campaign not rerun, macOS and ARM never run, and no external audit of the
   crypto code.
