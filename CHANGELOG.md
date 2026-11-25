# Changelog

All notable changes to Cameleer. Versions follow [Semantic Versioning](https://semver.org/); before
1.0, a minor version can change behaviour, and each such change is listed under **Changed**.

## Unreleased

- **The `alibaba-sls` component** (Camel's `camel-alibaba-sls`, Preview since Camel 4.23), in `cameleer-cloud` and the
  `cloud` feature: `putLogs` (the body as a protobuf `LogGroup`, compressed with lz4 or deflate), `getLogs` and
  `listLogStores` on Alibaba Cloud Simple Log Service, with Camel's URI, options and `CamelAlibabaSls*` headers. Requests
  are signed with the SLS signature V1 (HMAC-SHA256 as Camel's SDK, or HMAC-SHA1) or V4, checked against the official Go
  and Python SDK test vectors; STS tokens with `securityToken`. Tested against `SlsMock`, an embedded emulator that
  verifies every signature, and against Alibaba's own Python SDK (`aliyun-log-python-sdk` 0.9.53, checksum-verified)
  in the `CAMEL_OFFICIAL_CLIENTS` tier, V1 and V4. Nothing has run against the real service. Not done: zstd
  compression (the default of Camel's SDK; Cameleer sends lz4, which the service accepts).

Fixes found while evaluating third-party crates (`docs/design/pathscale-evaluation.md`, section 9). WSL2 figures on a
Ryzen 5 5600X; none of them was measured on droplets.

- **Aggregator with `completionTimeout`: no scan of every group per message.** Each exchange that left its group open
  woke the timeout checker, which scanned every group's deadline while holding the lock the route needs. Deadlines are
  now also kept in order, and the checker is woken only when a new deadline is the earliest. Per message, with one
  sender: 2.9x faster with 10 open groups and 14.4x with 10,000 (18.7 µs to 1.3 µs).
- **Regular expressions: iterating the matches of a large body is linear.** Every match copied the whole subject text,
  so `split`, `replaceAll`, `find_iter`, Simple's `split`/`replaceAll` and `tokenize` with `regex=true` were
  quadratic in the number of matches. Splitting a 64 KiB body with 13,000 separators: 418 ms to 2.9 ms. `replaceFirst`
  now stops at the first match (441 ms to 0.08 ms on the same body).
- **Embedded TCP servers no longer poll.** The accept threads of the `TcpServer`s behind websocket, syslog, mllp, the
  netty TCP consumer and the test brokers slept 5 ms between non-blocking accepts. They now block, and `stop` wakes
  them with a connection of its own (`cameleer_tls::net::wake_accept`).
- **Cloud components reuse connections for streamed responses.** `HttpClient::send_to` (streamed model calls of Azure
  OpenAI and Vertex AI/Gemini, S3 downloads) opened a new TCP and TLS connection per request; it now takes keep-alive
  connections from the same pool as `send`. Streamed uploads and requests through a proxy still use a connection each.
- **`JdbcAggregationRepository`: no `MAX(seq)` per new group.** The insertion sequence is read once when the
  repository is created (over a new index on `seq`) and counted in the process afterwards.

## 0.8.0 (2026-11-24)

gRPC and HTTP/2 on simoom 0.2.0: the gRPC server runs its calls on a pool of threads, the event loops start at
once, unary calls are cheaper, routes that call the `http` producer from a `platform-http` route run asynchronously,
and the HTTP server's keep-alive limit and the governor that moves slow routes off the event loops are fixed. The
release has **behaviour changes** (marked **Changed** below; `docs/release-notes/0.8.0.md` lists them under
Upgrading). The 0.8.0 against 0.7.0 figures are from `docs/BENCHMARKS.md` and
`benchmarks/results/2026-11-20-linux-0.8.0-candidate/` (three 4-vCPU DigitalOcean droplets in `sfo2`, one session);
the comparison with Apache Camel is the 2026-11-23 session
(`benchmarks/results/2026-11-23-linux-main-beb3385-camel/`).

- **Changed: the HTTP server no longer closes a keep-alive connection after 1,000 requests.** `ServerConfig::max_keep_alive_requests`
  defaulted to 1000, a leftover of the thread-per-connection server, so every client had to reconnect after 1,000
  requests: in the 2026-11-23 droplet benchmark the load generator opened 700 to 5,700 connections per run against
  Cameleer instead of 64 (one per 1,000 requests). The default is now 0, unlimited, as in Jetty, Vert.x and Undertow;
  a limit can still be set in `ServerConfig`. Cameleer figures published before this change were measured with the
  reconnects; on one Windows machine over loopback the change was worth 0% to 5% throughput.
- **Changed: a route that blocks briefly no longer collapses the server's throughput.** On an idle server such a route (a Kafka or
  database call of 0.3 to 1 ms) looked fast and moved to the event-loop thread, where under load its calls ran one after
  the other; in the 2026-11-22 benchmark runs this cut some runs to 7,000-15,000 requests/s. A route is now also sent
  back to the pool for a minute when its calls keep more than half of the loop threads' time busy over 100 ms.
- **Changed: the `kafka` producer has an asynchronous form.** A `platform-http`, `jetty` or `netty-http` route that sends to Kafka
  now waits for the broker's acknowledgement without holding a thread: sends are queued, up to `maxInFlightRequest`
  sender threads (one per connection, started with the first asynchronous send) take them in batches, and the route
  continues on the sender thread. Batching, partitioning, offsets and the exchange headers are the same as on the
  synchronous path, which other routes still use. A send queued when the producer stops fails with an error.
- **Fixed: start-up regression of 0.6.1 to 0.7.0.** The `startup` scenario went from about 2 ms to 12 to 31 ms because
  simoom grows the descriptor table at server start and Linux then waits for an RCU grace period once a second thread
  exists. `cameleer_http::register` now does it while the process is still single-threaded. Back to 2.8 ms in-process
  and 3.0 ms with an HTTP route on the droplets (`docs/design/startup-regression-2026-11.md`).
- **The gRPC server runs the threads of its calls on a pool** (PRs 62 and 65). A call used to start up to three
  threads (the call, the reader of its request messages and, with a deadline, the handler), and starting them limited
  the server to about 11,000 calls per second. A call is now one job on a process-wide cached pool (a worker starts
  only when none is idle; an idle worker exits after 30 seconds); the request body is read with a waker on the
  call's thread and deadlines are kept by one shared timer thread. A pool worker with no job hands its allocator
  cache to other threads (`cameleer_core::alloc::release_thread_cache`), so the pool costs no memory. Measured on its
  own (other droplets, 64 concurrent callers): 3.9x the calls per second of 0.7.0, 37 against 214 microseconds of
  server CPU per call, same peak memory (13.5 against 13.8 MB); the first figure was limited by the load generator
  (see below).
- **gRPC unary calls are cheaper on the server.** The consumer reads from the proto descriptor which methods are
  unary. Their reply is sent as one response with its trailers, instead of a stream of head, message and trailers.
  A unary call whose request arrived whole runs on the event loop while its route is fast, under the same governor
  as REST routes; a route that stalls the loop goes back to the pool.
- **gRPC, together:** 0.8.0 served 94,400 calls/s at 64 concurrent calls and 106,200 at 256 (cameleer-bench's
  generator) against about 8,300 and 8,000 for 0.7.0, at 24 against 270 microseconds of server CPU per call, a p99 of
  1.2 against 14.8 ms (4.7 against 54 ms at 256) and 9.2 against 13.6 MB of peak memory. nghttp2's `h2load` drove
  the same server to 129,000 to 135,000 calls/s. **0.7.0 does not have the pool above**, so the 11x to 13x contains
  it; the round did not measure the other changes one by one. This replaces the "41,020 calls/s, 3.9x" figure of the
  pool round, which was limited by the benchmark's client.
- **simoom 0.2.0** (every simoom crate moves from 0.1 to 0.2; a minor version before 1.0 because two config structs
  of simoom-client gained public fields): one write per reply batch on a multiplexed connection, the client's head
  and body in one write with one wake per batch, and the blocking client's HTTP/2 on event loops. **A gRPC client
  call sends its head, first message and end of stream in one write** (`Client::call_with_body`), and **the gRPC
  client no longer serializes stream openings** (the lock that guarded a cold channel is taken only while the channel
  has no connection). Not measured alone; together with the other client fixes, gRPC unary through the crate's client
  went from 95k to 121k calls/s on WSL2 (`docs/design/http2-throughput.md`).
- **Changed: the HTTP and gRPC servers start every event loop** (one per CPU). Loops used to start lazily, and
  connections stay on the loop that accepted them, so a few long-lived HTTP/2 connections could all land on one
  thread when they arrived in a burst. With 4 HTTP/2 connections the gain was 1.04x for gRPC unary and 1.05x for
  `rest-json-h2` on three droplets (one run), and 2.07x and +14% on WSL2, where client and server share the CPUs
  (means of three noisy rounds). The cost: a `platform-http` route is ready about 0.3 to 0.5 ms later (3.0 against 2.7
  ms on the droplets; 0.8 ms on 12 CPUs), an idle server uses 0.3 to 0.5 MB more, the `full` binary is 0.8% larger
  and `core` 2.4%. No cost was measurable on `http` or `rest-json` throughput. `CAMEL_SERVER_LAZY_LOOPS=true`
  restores the lazy start (it was not measured as a control); the lazy start stays on AWS Lambda
  (`false` forces the eager one). See `docs/design/http2-throughput.md`.
- **HTTP/2 at 0.51x of HTTP/1.1 (benchmark round of 2026-11-18) was mostly the benchmark.** Mostly it was
  cameleer-bench's blocking HTTP/2 client, which makes two thread hand-offs per response; partly it was the lazily
  started event loops above. In the 0.8.0 round `rest-json` over HTTP/2 equals HTTP/1.1 (1.00x), and with nghttp2's
  `h2load` the server serves HTTP/2 at 1.5x to 1.6x its HTTP/1.1 rate (not a like-for-like protocol test: 64
  connections of one request against 4 or 16 of many streams). `rest-json` is 1.01x of 0.7.0, so the 5% loss of the
  0.7.0 round is not there.
- **MCP is 0.96x to 0.98x of 0.7.0** (tool calls per second, stateless and in sessions, on every pass and with the
  sides in either order). That is a 2% to 4% slowdown that is probably real. Whether the eager event loops or the
  header change below causes it was not isolated; p99 and CPU per call are within 1 to 3% and peak memory is lower.
- **Message headers are kept sorted by name** (PR 68). `Headers` keeps its entries sorted by name ignoring case
  (Camel's case-insensitive order, so the iteration order users see is unchanged), a lookup is a binary search and
  iterating needs no sort; it used to collect and sort the headers on every iteration. The HTTP binding and the
  `http` producer no longer allocate a lowercase copy of each header name to filter it or to detect repeats. No
  gain is claimed: it was not measured on its own.
- **Changed: asynchronous routing, on by default.** A `platform-http` (or `jetty`/`undertow`) route that calls the
  `http` producer now runs as a task (`cameleer_core::reactive`, Camel's `AsyncProcessor` contract). It no longer
  holds a server thread while it waits for the reply.
  - **How it runs:** the producer sends on simoom-client's event-loop client, and the route continues on the event
    loop that read the reply.
    - Steps that only compute run there too: `setHeader`, `setBody` or `transform` with constant, header, JsonPath, jq
      or Simple expressions (Simple without `bean:`, `ref:` or `properties:`).
    - Any other step may block (a processor, a bean, a blocking component), so the route moves to a worker thread
      before it.
  - **Not affected:**
    - endpoints with `consumes`, `produces` or `disableStreamCache`, and routes with `interceptFrom`, which stay on
      the server's threads as before;
    - `http` requests that stream their body, use HTTP/2, a proxy or chunked request coding, which keep the blocking
      client, on a worker.
  - **What changes for you:** the server's thread pool (200 by default) no longer bounds how many requests such a
    route works on at once. The new endpoint option `maxInflightRequests` does instead (default
    `camel.server.maxInflightRequests`, else 10,000; `0` means no limit). Beyond it the route answers `503` with
    `Retry-After: 1` at once.
  - **Switches:**
    - `CAMEL_ASYNC_ROUTING=false` keeps every route on the synchronous path, as before.
    - `CAMEL_HTTP_CLIENT_LOOPS` sets the number of event loops of the `http` producer's client (default half the
      CPUs, at most 64). With simoom 0.2.0, a request sent from a server loop goes out on that loop.
  - **Measured** on one WSL2 machine (Cameleer on 4 CPUs), against the synchronous path of the same build, for a
    `platform-http` -> `http` proxy, three guarded rounds:
    - fast backend: 2.2-2.9x requests/s, 26-33% less CPU per request in the Cameleer process, 8 threads instead of
      69-205;
    - 20 ms backend: 12,600 against 9,900 requests/s, where the synchronous path stops at its thread pool.
  - **Not measured:** cloud machines, Windows, HTTPS, and routes other than this proxy.
  - Design, all measurements and the porting plan: `docs/design/async-routing.md`.
- **Benchmarks and docs.** `scripts/cloud-bench/peer.sh` drives the server with nghttp2's `h2load` as an
  independent client, `BENCH_SWAP=1` swaps the order of the sides, and `droplets.sh` defaults to `sfo2` (`c-4` is no
  longer offered in `blr1` or `sfo3`). The comparison with Apache Camel is re-measured in one session on 2026-11-23 (`main` at `beb3385`):
  2.0x (HTTP) to 19.8x (Kafka stream) the throughput of Camel on the JVM, p99 lower in every scenario, start-up 3.0 ms
  against 1,034 ms, 8 to 39 MB of peak memory against 211 to 421 MB, MCP session calls 2.73x Camel's `camel-mcp-server`
  and 20.6x the TypeScript SDK. **Four ratios are lower than the ones published with 0.4.0 (2026-11-06):** seda
  request-reply 5.6x (was 6.8x), the content-based router 2.6x (3.0x), CSV 2.2x (2.3x) and HTTP into Kafka 4.3x (5.0x,
  measured on other machines). The 2026-11-23 droplets are faster and Camel on the JVM gained more from them than
  Cameleer did. One-hour soaks: HTTP into Kafka 49.5k req/s (4.6x Camel), MCP session 63.0k calls/s (2.5x), flat
  throughput, p99, threads and descriptors. `docs/BENCHMARKS.md`
  and the READMEs carry the new figures; the docs harness checks that `cameleer-*` version snippets match the
  workspace minor and that every `CAMEL_*` environment variable read in the code is documented.
- **Not measured:** asynchronous routing on cloud machines (WSL2 only), h2load over TLS, gRPC streaming, the gRPC
  client, Windows, and `CAMEL_SERVER_LAZY_LOOPS=true` as a control.

**Upgrading from 0.7.x.** Event loops start at once, so an idle server has one thread per CPU instead of a few;
set `CAMEL_SERVER_LAZY_LOOPS=true` to keep the old start. `platform-http` routes that call `http` answer from tasks
(`CAMEL_ASYNC_ROUTING=false` turns that off) and are bounded by `maxInflightRequests` instead of the server's
thread pool. The header iteration order is unchanged. Nothing else breaks.

**The 0.8.0 images were built from `main` at `6722d3a`, after the tag**, because the tag's `rust-toolchain.toml` (1.99.1)
did not match the builder image (1.100.0) and the arm64 build failed. The commit differs from `v0.8.0` only by the
toolchain pin, the `version` input of `container.yml` and documentation; the crates and archives come from the tag.

## 0.7.1 (2026-11-17)

- **The `cameleer` crate 0.7.1** changes only the MCP Registry name in its README and `server.json`:
  the registry compares the namespace case-sensitively, so it is `io.github.Cameleering/cameleer`.
  Nothing else changed; the other crates, the tag, the images and the archives stay at 0.7.0.

## 0.7.0 (2026-11-16)

The move onto [simoom](https://crates.io/crates/simoom) is finished: the gRPC client and server and
everything that spoke HTTP/2 run on it, the old HTTP/2 and HPACK code is deleted, the HTTP server speaks
HTTP/2, and no client can be told to skip certificate verification any more. The release has **behaviour
changes** (marked **Changed** below; `docs/release-notes/0.7.0.md` lists them under Upgrading).

- **The AWS, Azure and Google components always verify the server certificate.** `trustAllCertificates=true`
  (and a global `camel.ssl.trustAllCertificates`) is refused at endpoint creation with a message that says
  what to do. The components take `sslContextParameters=#bean` and the global `camel.ssl.*` settings, so a
  private CA or a local emulator's certificate is trusted through a trust store. **Changed:** routes that
  set `trustAllCertificates=true` on these components fail until they trust the server's certificate.
- **The HTTP server speaks HTTP/2** (`platform-http`, `jetty`, `undertow`, `netty-http`, `rest`, the MCP
  endpoint): h2c prior knowledge and `Upgrade: h2c` on plain HTTP, ALPN `h2` next to `http/1.1` on HTTPS.
  Routes need no change. **Changed:** an HTTPS consumer now negotiates `h2` with clients that offer it
  (before, it answered HTTP/1.1 only).
- **The gRPC server and the cloud mock servers run on simoom.** The byte-level gRPC server (`GrpcServer`,
  the `grpc` consumer, the Pub/Sub emulators) is a simoom server: an event loop per core, HTTP/2 over h2c
  prior knowledge, `Upgrade: h2c` or TLS with ALPN `h2`, one thread per call. A call starts when its
  first request bytes arrive, so bidirectional streams are answered while the client is still sending.
  The old HTTP/2 and HPACK implementation in `cameleer-grpc` (`H2Server`, `H2Connection`, `hpack`) is
  deleted; `cameleer_grpc::h2` keeps only `H2Config`. **Changed:** the server's flow-control windows and
  frame size are simoom's own (`flowControlWindow` and the frame settings now apply to the client side
  only), and a plain HTTP/1.1 request to a gRPC port is refused with a 4xx instead of 505.
- **The gRPC client runs on simoom-client** (the `grpc` producer, Pub/Sub streaming and Pub/Sub Lite, and
  Kinesis `SubscribeToShard`): HTTP/2 connections are pooled and re-established by the engine, calls
  stream in both directions. **Changed:** the gRPC client always verifies the server certificate and its
  host name against the trust anchors it is given.

## 0.6.1 (2026-11-13)

Cameleer's HTTP stack moves onto [simoom](https://crates.io/crates/simoom), the std-only HTTP engine of
the Cameleering family: the server (an event loop per core instead of a thread per connection) and the
client (pooling, HTTP/2 multiplexing, streaming). The numbers are in `docs/design/simoom-adoption.md`.
This is a patch number for a release that contains **breaking changes** (listed under **Changed** below):
the `http2` cargo feature is gone, response header names are lower case, and the HTTP clients no longer
accept turning certificate verification off. The release also carries the entry for the official MCP
Registry (`server.json`, the `mcp-name` line in the `cameleer` crate README and the image label), which
checks published artifacts and so needed a new version.

- **The HTTP client runs on simoom-client** (the `http` and `https` producers, `HttpClient`, the MCP,
  SaaS, datastore, FHIR and DICOMweb clients): one pool per set of settings, HTTP/1.1 keep-alive and HTTP/2
  multiplexing, proxies, redirects, streamed request and response bodies, the same Camel exceptions and
  messages. `httpVersion=HTTP/2` works in every build, also with a streamed body, so the **`http2` cargo
  feature is gone** (`--features http2` is now an error: drop it). **Changed:** the HTTP clients always
  verify the server certificate and its host name; `hostnameVerification=false` and
  `trustAllCertificates` are refused there with a message that says what to do (trust a private server
  with `trustStore`, or set `serverName`); the cloud components keep their own client and their
  `trustAllCertificates` for now. A plain `http://` request to a TLS port reports
  "Connection closed unexpectedly: the answer is not HTTP".
- **The HTTP server runs on simoom** (https://crates.io/crates/simoom, in the Cameleering family).
  An event loop per core replaces a thread per connection, for plain HTTP and HTTPS (TLS 1.2 and
  1.3, client certificates), in `platform-http`, `http`, REST DSL, the MCP endpoint, the dev console
  and the emulators. Measured on DigitalOcean `c-4` machines against the previous server, same
  machines and rounds (`docs/design/simoom-adoption.md`): `http` 1.33x at 64 connections and 1.56x at
  256, `rest-json` 1.31x, MCP session calls 1.37x; p99 latency 2 to 3x lower, CPU per request 40 to 47%
  lower, 4 threads instead of 67 to 202, and half the memory. **Changed:** response header names are
  lower case; an HTTP/1.0 client gets an HTTP/1.1 status line; the server refuses requests that are
  ambiguous or malformed (request smuggling shapes, `|` and other unsafe bytes in the target); and
  `ServerConfig.pool_size` limits the threads for routes that block, not connections. The HTTP
  client now percent-encodes unsafe bytes in the request target, as curl and Java's client do.

- **The `cameleer` crate.** `cargo install cameleer` installs the `cameleer` command and
  `cargo add cameleer` gives the `cameleer-main` API: a thin wrapper (`publish/cameleer`, outside
  the workspace because its binary has the same name as `cameleer-main`'s) that forwards every
  feature. First published as 0.6.0; `cameleer-main` is unchanged.

## 0.6.0 (2026-11-12)

Cameleer gets packs: optional, industry-specific sets of components with their own version. The
first is the health pack (`cameleer-health` 0.6.0): HL7 v2, MLLP and FHIR R4, moved out of the
default build, and new DICOM support (DIMSE, DICOMweb, de-identification), which Apache Camel has
no component for. It passed 10 of 10 live tests against DCMTK 3.6.7, Orthanc 1.12.2, python-hl7
0.4.5 and HAPI FHIR 8.12.0, reads and writes 33 public DICOM test files byte for byte, and adds
about 2 MB to the binary. Existing routes can now be served as MCP tools with no edits
(`--mcp-expose`), and in a confidential VM Cameleer can serve hardware attestation evidence bound
to the TLS key and a manifest of its routes and tools; the attestation code has run only against
simulated evidence sources so far, not on real TEE hardware. The project moved to the GitHub
organization `Cameleering`, and the images to `cameleering/cameleer` and
`ghcr.io/cameleering/cameleer`. The MCP comparison was rerun in one session on 2026-11-12:
39,922 tool calls per second against 22,604 for Apache Camel's `camel-mcp-server` and 2,257 for
the official TypeScript SDK, in 56 MB against 289 MB and 197 MB; Camel's p99 is lower (5.2 ms
against 6.3 ms) ([report](docs/BENCHMARKS.md)).

### Changed

- **Breaking: `mllp`, `hl7`, `hl7terser`, `fhir`, `fhirJson` and `fhirXml` moved to the health pack.**
  The default build and image no longer have them: use `cameleering/cameleer:health` or build with
  `--features health`. A route that names one of them in a build without the pack fails at start
  with a message naming the pack, the image and the feature. In Rust, `cameleer_protocols::{hl7, mllp}` are now
  `cameleer_health::hl7::{self, mllp}` and `cameleer_soap::fhir` is `cameleer_health::fhir`, with no
  aliases. The socket helpers of `cameleer-protocols` are in `cameleer_tls::net`, the SOAP producer
  client is `cameleer_http::client::EndpointClient`, `now_utc` is in `cameleer_core::util_time`.
- **`onException` and `doCatch` for `Exception` now catch Camel's exchange failures**:
  `CamelExchangeException` was listed as its own parent, so `HttpOperationFailedException`,
  `DirectConsumerNotAvailableException`, `ResolveEndpointFailedException`, `ValidationException`
  and the other kinds under it never matched `Exception` (or `Throwable`). Their parent is now
  `Exception`, as in Camel; `RuntimeException` still does not catch them, also as in Camel. Routes
  whose `Exception` handler used to let these failures through now handle them.
- **Route configurations apply whatever the file order**: a top-level `interceptSendToEndpoint`,
  `intercept`, `onException`, or a `routeConfiguration` without id now applies to every YAML route,
  as in Camel, and a `routeConfigurationId` may name a configuration from a later file; an unknown
  id still fails at start. Before, they applied only to routes in files loaded after them.
- **Moved to the `Cameleering` organization**: the repository is
  [github.com/cameleering/cameleer](https://github.com/cameleering/cameleer) (the old GitHub URLs
  redirect), the images are `cameleering/cameleer` on Docker Hub and `ghcr.io/cameleering/cameleer`,
  and the crates' `repository` URLs point to the organization. The old image paths
  (`pmsudhi/cameleer`, `ghcr.io/pmsudhi/cameleer`) stay at 0.5.1 and receive no new tags.

### Added

- **Packs, and the health pack** (`cameleer-health` 0.6.0, cargo feature `health` of
  `cameleer-main`, **not** in `full`; image tags `cameleering/cameleer:health` and
  `:<version>-health`; archive `cameleer-health-<version>-<target>`). A pack is an optional,
  industry-specific set of components with its own version (`docs/packs/index.md`). The health
  pack holds HL7 v2 and MLLP, FHIR R4 and the new DICOM support (Apache Camel has no DICOM
  component, so these are Cameleer extensions):
  - the `dicom` component: DIMSE over TCP and TLS, the consumer an SCP (C-ECHO, and C-STORE,
    C-FIND, C-MOVE and C-GET answered by routes) and the producer an SCU
    (`operation=echo|store|find|move|get`), with `CamelDicom*` headers;
  - the `dicomweb` component: STOW-RS, QIDO-RS, WADO-RS and metadata as a client, and a
    DICOMweb server on the shared HTTP server;
  - the `dicom` data format (Part 10 to a Value tree in the DICOM JSON shape and back, byte for
    byte; explicit, implicit, deflated and big endian; encapsulated pixel data as fragments;
    specific character sets including ISO 2022 Japanese, Korean and Chinese, GB18030 and UTF-8),
    `dicomJson` (PS3.18 Annex F) and `dicomDeidentify` (PS3.15 Annex E basic profile and options,
    HMAC-keyed and deterministic, with audit records in the hash-chained log);
  - `${dicom.PatientID}` and sequence paths in Simple, and the `dicom` language;
  - the PS3.6 dictionary and PS3.15 Table E.1-1 generated from the 2026d standard
    (`scripts/update-dicom-dict.py`, pinned digests);
  - examples in `examples/medical/`, the guide `docs/packs/health/guides/dicom.md`;
  - verified live against DCMTK 3.6.7 and Orthanc 1.12.2 with its DICOMweb plugin
    (`scripts/dicom-interop.sh`, `CAMEL_DICOM_INTEROP=1`), and on 33 public test files.
- **The pack extension point**: `cameleer_core::pack` (`PackInfo`, `Pack`,
  `CamelContext::add_pack`, `pack::install`), `Main::add_pack`; pack languages are YAML and XML
  DSL keys; `cameleer version` lists the packs; a route that names a pack component in a build
  without the pack fails with a message naming the pack, image and feature.
- **Pack release artifacts**: CycloneDX SBOMs of exactly what each build compiles in
  (`scripts/sbom.py`), SHA-256 sums and keyless cosign signatures of the release archives, a DICOM
  Conformance Statement generated in part from the code, CODEOWNERS, and the
  `CAMEL_HEALTH_INTEROP=1` live tier (`scripts/health-interop.sh`: DCMTK, Orthanc, python-hl7,
  HAPI FHIR). Release archives `cameleer-<version>-<target>` and
  `cameleer-health-<version>-<target>` are built by `.github/workflows/archives.yml`, and the
  container workflow builds a health image next to the general one. The images are published with
  0.6.0. The archive workflow failed on its first run, on the `v0.6.0` tag (a script without the
  execute bit; no `zip` in Git Bash on the Windows runner), so 0.6.0 has no archives; both are
  fixed and a test run built all six archives unsigned. Signed archives arrive with the next
  release.
- **`/q/info`** (`camel.server.infoEnabled`, and with the dev console): the version, the compiled-in
  feature families and the packs of a running instance; also `/q/dev/packs`.
- **`cameleer_core::audit`**: the hash-chained audit log moved from `cameleer-ai` into core, one
  log per context (`AuditLog::of`), shared by AI tool calls and the health pack.
- **Simple functions from other crates**: `cameleer_core::language::simple::register_function`
  adds `${prefix.rest}` functions (used by `${dicom.*}`).
- **`cameleer_core::multipart`**: a shared, binary-safe MIME multipart parser and builder (the
  `mimeMultipart` data format and DICOMweb use it).
- **Attestation in confidential VMs** (new crate `cameleer-attest`, feature `attest`, in the default
  build): with `camel.attestation.enabled=true` in an Intel TDX or AMD SEV-SNP guest (Linux
  configfs-tsm) or a dstack CVM, Cameleer serves hardware evidence at `/q/attestation`, bound to the
  client's nonce, the embedded server's TLS key and a manifest of the routes, configuration (secrets
  left out) and tools, with a hash-chained log of later changes. The MCP server adds it to
  `server/discover` and `initialize` (`_meta["ing.cameleer/attestation"]`, the full report inline
  when the client sends a nonce) and, with `camel.attestation.mcp-tool=true`, as a tool. Start-up
  fails when attestation is on and no evidence can be produced. Guide: `docs/guides/attestation.md`.
  Limits: the tests use a simulated configfs-tsm directory and a simulated dstack agent, and the
  code has not yet run on real TEE hardware; Cameleer produces the evidence and the binding but
  does not verify Intel's or AMD's signature chain itself (a platform verifier does).
- **Existing routes as MCP tools, with no route edits**: `cameleer run --mcp-expose rest:*`
  (property `camel.server.mcp-expose-routes`). REST DSL operations become tools with typed
  arguments from their documented parameters; other routes (`direct:*`, `route:<id>`) take a
  body and headers. Writes are destructive and need approval by default, GET is read-only, every
  call goes through the tool envelope, and per-route overrides
  (`camel.server.mcp-expose-routes.<routeId>.<option>`) take every `ai-tool` option. The REST DSL
  now keeps its operations on the context (`cameleer_http::rest::rest_operations`).

### Fixed

- **HTTPS routes and the management endpoints on one port**: with `camel.server.sslEnabled=true`,
  `/q/*`, `/q/attestation` and the MCP endpoint now speak HTTPS on `camel.server.port` next to the
  HTTPS routes; start-up used to fail with "already used by an https server". Camel's separate
  management server is supported too: `camel.management.enabled=true` serves `/q/*` on
  `camel.management.port` (default `9876`) and `camel.management.host`, plain HTTP.
- The management endpoints no longer fix every route's worker pool at 8 when they register on the
  shared port first; they use the default.
- `/q/metrics` sends one `Content-Type` (`text/plain; version=0.0.4; charset=utf-8`), not two.
- Readiness: a suspended route's consumer is `DOWN` in the check's details too, matching its status.
- `cameleer validate` checks `routeConfigurationId` across all the files it validates: a
  configuration from a later file is accepted, an unknown id is an error.

## 0.5.1 (2026-11-09)

### Fixed

- HTTP/2 client (gRPC): a complete response followed by RST_STREAM(NO_ERROR) is no longer
  reported as `stream reset: NO_ERROR`, as RFC 9113 section 8.1 requires (INTEROP bug 20).

## 0.5.0 (2026-11-09)

Cameleer becomes an action layer for AI agents: any route can be a tool that agents call through
a reliability envelope (schema checks, deduplication, approvals, rate limits, timeouts that
cancel, compensation, a hash-chained audit log). Cameleer serves those tools as an MCP server
(both protocol eras, Streamable HTTP and stdio; official conformance 39/39 and 26/26), imports
other MCP servers' tools as a client and gateway, and runs tool-calling agents as a route step
with seven model components and 23 providers. Measured on the same cloud machines, its MCP
server handles 41,620 tool calls per second against 22,458 for Apache Camel's
`camel-mcp-server` and 1,536 for the official TypeScript SDK, in 53 MB against 317 MB and 214 MB
([report](docs/BENCHMARKS.md)). In a field test of 40 real requests through Claude and the
official MCP client, Claude Opus 5.5 completed 157 of 160 runs correctly
([guide](https://cameleer.ing/docs/guides/mcp-shop-example.html)).

### Added

- **Routes as tools for AI agents** (new crate `cameleer-ai`, new `ai` feature, in `full`):
  `ai-tool`, `ai-resource` and `ai-prompt` with Camel's option names and semantics, and a tool
  registry. Every call goes through a reliability envelope: JSON Schema 2020-12 checks of the
  arguments (with an error the model can act on), structured results, deduplication of tools
  with side effects, approval of destructive or matching calls, timeouts that cancel the
  tool's route, rate limits per principal, circuit breakers, compensation per conversation, a
  hash-chained audit log (`camel.ai.audit.*`), definition pinning (`version`,
  `camel.ai.manifest`) and a kill switch (`camel.ai.disabled-tools`). See
  [ai-tool](docs/components/ai-tool.md).
- **MCP server** with Camel's `camel.server.mcp-*` properties, serving those routes to MCP
  clients over Streamable HTTP and stdio (`cameleer run --mcp`, `--mcp-stdio`). It speaks
  2026-07-28 (stateless, `server/discover`, approvals as multi round-trip requests with a
  sealed `requestState`, `subscriptions/listen`, caching hints) and 2025-11-25, 2025-06-18 and
  2025-03-26 (sessions, `elicitation/create` approvals) on the same endpoint. Tools,
  resources, templates, prompts, completion, pagination and change notifications;
  authentication with API keys, JWTs or token introspection, with protected-resource metadata
  and audience checks. Tool routes can return images, audio and embedded resources
  (`CamelAiToolContent`). Checked with the official conformance suite
  (`scripts/mcp-conformance.sh`). See the [MCP server guide](docs/guides/mcp-server.md).
- **The `agent` step** (Cameleer extension): a model and its tools in a loop, as one step of a
  route, with turn, token and time limits, conversation memory, approvals that pause the
  conversation until a person decides, and record/replay of model responses. See
  [the agent step](docs/guides/agent-step.md).
- **Tool calling in every model component**: `openai` (and its Camel 4.23 name `llm`),
  `azure-openai`, `aws-bedrock` (Converse), `google-vertexai` (Gemini) and the new
  `anthropic` component take Camel's `tags`, `autoToolExecution`, `maxToolIterations`,
  `maxAgenticTokens` and related options and run your `ai-tool` routes for the model.
- **`anthropic` component** for the Anthropic Messages API.
- **`openai`/`llm`: streaming, the Responses API and Ollama's native API**: `streaming=true`
  streams plain chats and every turn of the tool loop, passing text deltas to a
  `streamingChunkProcessor` bean while the body stays the whole answer; `openai:responses`
  uses OpenAI's Responses API; `apiFormat=ollama` calls Ollama's `/api/chat`. All three work
  with `tags`, the agent step and record/replay.
- **`cameleer test`** runs YAML test files: send messages, call tools, approve, compensate, and
  check bodies, headers, deduplication and mock endpoints, with models replayed from
  recordings.
- **`json-validator`: JSON Schema 2020-12 keywords** `prefixItems`, `minContains`,
  `maxContains`, `dependentSchemas`, `unevaluatedProperties` and `unevaluatedItems`.
- **`cameleer tool`**: the kill switch and approvals of a running integration from the command
  line (`tool list`, `disable`, `enable`, `approvals`, `approve`, `deny`), and `GET /q/dev/tools`
  with `POST /q/dev/tools/{name}/disable|enable` on the dev console. MCP clients get
  `list_changed` when a tool is withdrawn.
- **Cooperative cancellation of an exchange** (`Exchange::set_cancel_token`): once the token is
  set, the exchange and its copies stop before their next pipeline step.
- **MCP client and gateway**: `camel.ai.mcp.NAME.*` connects to MCP servers over HTTP or stdio
  (both protocol eras) and imports their tools into the tool registry, where they run through
  the same envelope (approvals, deduplication, rate limits, audit) and can be used by the agent
  step (`tools: mcp:github`), by model components with `tags`, and re-exported by Cameleer's own
  MCP server. Tool lists re-sync on `list_changed` and reconnects; a tool whose definition
  changes after it was pinned is withdrawn. The new `mcp:` component calls a server's tools,
  resources and prompts from a route. See the [MCP client guide](docs/guides/mcp-client.md).
- **Progress, logs, elicitation and sampling from tool routes**: `ai-tool:x?operation=progress`,
  `log`, `elicit` and `sample` inside a tool route send MCP progress and log notifications, ask
  the client's user a question, or ask the client's model, in both protocol eras. The official
  conformance suite now passes every scenario (39 of 39 for 2025-11-25, 26 of 26 for
  2025-06-18).
- **Tool guardrails** on `ai-tool`: `redact` replaces secrets, e-mail addresses, card numbers
  (Luhn-checked), IBANs, SSNs, IPv4 addresses, phone numbers or custom patterns in results
  before they reach the model, the client or the audit log; `authorize` is a Simple predicate
  over the arguments and the caller; `cacheTtl` reuses results.
- **`gemini` component** for the Gemini API with API keys, with tool calling and thought
  signatures.
- **Provider presets** on `openai`/`llm`: `provider=groq` (or `mistral`, `deepseek`, `xai`,
  `openrouter`, `together`, `fireworks`, `cerebras`, `perplexity`, `huggingface`, `dashscope`,
  `moonshot`, `cohere`, `nvidia`, `gemini`, `ollama`, `vllm`, `lmstudio`, `llamacpp`) sets the
  base URL and the key's environment variable. OpenAI reasoning models get
  `max_completion_tokens`; DeepSeek, Qwen and Kimi get their `reasoning_content` back during a
  tool loop. See [model providers](docs/guides/model-providers.md).
- **Model call options on every model component**: `maxRetries` (429, 5xx and timeouts, with
  backoff), `fallbackModels`, an exact `responseCache` and `coalesce` for identical concurrent
  requests, all in process. Prompt caching: `anthropic` adds `cache_control` breakpoints by
  default and `aws-bedrock` adds Converse `cachePoint`s with `promptCaching=true`;
  `CamelAiCacheReadTokens` and `CamelAiCacheWriteTokens` report cached input.
- **Example `examples/agents/mcp-shop`**: an online shop's back office as an MCP server (19
  tools, policies, prompts, approvals, a manager in the loop, a size question), with the requests
  that the field test ran and the guide "Ask Claude about your business"
  (`docs/guides/mcp-shop-example.md`).
- **What a field test with Claude and the official MCP client found** (`scripts/mcp-fieldtest`,
  40 real requests against an online shop's back office served by Cameleer):
  `camel.server.mcp-resource-tool` lets the model read resources through a tool;
  a tool route can refuse with `CamelAiToolIsError=true` and its own words; an approval
  form's Accept approves; the route of an approved call sees `CamelAiToolApprovedBy`; with an
  `approvalEndpoint`, calling again returns "still waiting" or the decision instead of a second
  request; date arithmetic (`date - date`, `date ± n`) in the embedded SQL engine.
- **Keep-alive connection pools** for the REST clients of the SaaS components and the cloud
  clients: model and API calls after the first skip the TCP and TLS handshakes.

### Changed

- `openai`/`llm` and the other model components: `parallelToolExecution` now defaults to
  `false` and `hallucinatedToolNameStrategy` takes Camel's `failExchange` and `repromptModel`
  (the default; `report` and `fail` still work); `maxAgenticTokens=0` means unlimited, as in
  Camel. The agent step's `parallelToolCalls` is unchanged.
- MCP server, as in Camel: `camel.server.mcp-tags=*` no longer exposes untagged items (the
  new pattern `untagged` does; `--mcp` and `--mcp-stdio` default to `*,untagged`); the server
  name defaults to the context's name; route exceptions reach clients as a generic message
  with an error id unless `camel.server.mcp-expose-errors=true`; the keep-alive interval is
  30 s and idle legacy sessions expire after 5 minutes. New properties
  `camel.server.mcp-server-title`, `-server-description`, `-server-website-url` and
  `-server-icons`.
- `ai-tool`: `description` defaults to the tool's name, as in Camel.
- `ai-tool` deduplication without a call id now includes the conversation (over MCP the
  session or `io.cameleer/conversationId`): retries are still caught, two conversations asking
  for the same thing no longer get each other's result.
- MCP: errors in a tool's route after an approval are sanitized like other route errors.
- A rejected or declined approval now tells the model that the call did not run and nothing was
  changed, and a decline in the client says the user declined it in the approval form (smaller
  models read "declined in the client" as the system refusing).

## 0.4.0 (2026-11-06)

Measured against Apache Camel with the same routes, Cameleer starts 450 times faster (3 ms vs
1.1 s), uses up to 54 times less memory and handles 1.7 to 8.7 times the throughput, across
routing, REST APIs, Kafka, PostgreSQL and batch jobs
([comparison](https://pmsudhi.github.io/cameleer/compare/apache-camel-vs-cameleer/),
[report](docs/BENCHMARKS.md)). This release also adds a built-in AWS Lambda runtime and builds
that contain only the components your routes use.

### Added

- **AWS Lambda custom runtime** in `cameleer run`, for the `provided.al2023` runtime. API
  Gateway, function URL and ALB events go to your `platform-http` routes in process, without a
  socket; other events go to `direct:lambda` or the endpoint named in the function's Handler.
  Startup failures go to `/init/error` and failed exchanges to `/error`. Package a function
  with `scripts/package-lambda.sh`. See [AWS Lambda](docs/DEPLOYMENT.md#aws-lambda).
- **Build features.** Every component family is a cargo feature of `cameleer-main` (`amqp`,
  `amqp10`, `cloud`, `dataformats`, `datastores`, `grpc`, `kafka-mqtt`, `protocols`, `saas`,
  `script`, `soap`, `sql`, `ssh`, `xml`), and so are five smaller parts: `templates`, `jq`,
  `xpath`, `http2` and `validate`. The default, `full`, includes everything; `core` includes
  none. The Dockerfile takes `--build-arg FEATURES=...`. A route that uses a left-out
  component fails at startup with the feature to add. Every component, language and data
  format page names its feature. See the [build features guide](docs/guides/build-features.md).
- **Startup recorder:** `camel.main.startupRecorder=logging` logs each startup step with its
  offset and duration, like Camel's `StartupStepRecorder`.
- `cameleer-bench coldstart` measures spawn-to-ready, spawn-to-exit and peak memory of the CLI.
- Guide: [AI agent tool calls, direct or through Cameleer](docs/guides/agent-tool-calls.md), with a
  tested saga route in `examples/agents/`.
- **A reproducible cloud benchmark** against Apache Camel on the JVM and on Quarkus native:
  `scripts/cloud-bench` creates three cloud machines, runs every scenario (in-process routing,
  REST APIs, HTTP into Kafka and PostgreSQL, Kafka stream processing, CSV batch) and deletes
  them. Raw results are in `benchmarks/results/`.
- Comparison pages on the website: Apache Camel vs Cameleer, Apache Camel alternatives, and AI
  agent tool calls made directly or through Cameleer.

### Changed

- **A feature list now leaves out the template engines, jq, the core XPath subset, HTTP/2 and
  `cameleer validate`.** If you built with a feature list in 0.3 (for example `--features
  cloud`), add the parts you use: `--features cloud,jq,validate`. The default build is
  unchanged. Rust programs that depend on `cameleer-core` or `cameleer-http` keep these parts
  through the crates' default features.
- **Timer routes that share a timer name share one thread**, as Camel keeps one `java.util.Timer`
  per name: 20 routes on one name use 1 thread instead of 20. A slow route delays the others on
  its name, as in Camel. Routes with different names still get a thread each.
- **XQuery and XSLT run on the caller's thread** and move to a larger stack only when they
  need it: an ordinary evaluation is 10 to 35 times faster (for example 142 µs to 6.5 µs for
  a small XQuery on Linux). XQuery recursion 2000 levels deep is about 30% slower on Linux.
- **Thread pools start their workers on demand** and idle workers exit after 60 s, like core
  threads in Java's `ThreadPoolExecutor`. An idle HTTP route uses 2 threads instead of 34.
  `ThreadPool::prestarted` keeps the old behaviour.
- The supervising route controller's thread exits when no route is left to retry.
- The local status file for `cameleer ps` is rewritten only when its content changes, and
  `cameleer ps` checks that a process is alive through the operating system. No status files
  are written on Lambda, Cloud Run, Knative or Azure Functions.
- Smaller and leaner binaries: the `dist` profile uses fat LTO, log macros format only enabled
  records, the allocator carves memory lazily, and the amd64 image uses packed relocations.
  The full binary is 23.9 MB (was 24.9 MB); the `core` build is 5.8 MB. An HTTP route uses
  about 10 MB of memory (was 15 MB) and is ready in about 1.2 ms on Linux (was 3.3 ms).

### Fixed

- **HTTP server keep-alive under load.** The server had 32 worker threads and closed keep-alive
  connections while all were busy, so more than 32 concurrent clients reconnected on every
  request. The default is now 200 workers, started on demand; REST throughput roughly doubled.
- **Kafka producer throughput.** Concurrent sends waited in line one record at a time. Records
  sent while another send is in flight are now batched into one produce request per broker,
  as Kafka's own producer does; each exchange still waits for its own acknowledgement.
- **PostgreSQL connection pool.** `poolSize` only limited idle connections, so a burst opened
  extra connections (each with a full login) and closed them again. It is now a hard maximum,
  like HikariCP's `maximumPoolSize`; a caller waits up to `poolTimeout` seconds (default 30)
  for a free connection.
- **TLS:** a server that rejected a client certificate could lose its alert to a TCP reset on
  Windows, so the client saw "connection reset" instead of `certificate_required`. Servers now
  close gracefully after a fatal alert. Code that calls `TlsStream::accept` on a raw
  `TcpStream` should use `cameleer_tls::net::accept_tls`.
- The run loop stops as soon as `camel.main.duration-max-messages` is reached, instead of up to
  20 ms later.

## 0.3.0 (2026-11-05)

### Added

- `cameleer validate`: checks YAML and XML route files for syntax, DSL structure, endpoint
  options, expressions, unresolved placeholders, dangling endpoints and duplicate route ids,
  without starting a route. Text or JSON output; exit code 0, 1 or 2.
  See [validate](docs/guides/validate.md).
- `cameleer schema`: a JSON Schema (draft 2020-12) of the YAML DSL for editors and agents.
- A complete [YAML DSL reference](docs/guides/yaml-dsl.md), a
  [YAML guide for coding agents](docs/guides/yaml-for-coding-agents.md) and `AGENTS.md`.

## 0.2.1 (2026-11-05)

### Changed

- The CLI is `cameleer` only. The `camel` binary is gone, since `camel` is Apache Camel's own CLI
  name; the subcommands are unchanged.

### Fixed

- Suspending a polling route waits for a poll in progress, as Apache Camel does.
- `mongodb-gridfs` no longer skips a file stored in the same millisecond as the previous one.
- Property values like `camel.server.port={{env:PORT:8080}}` resolve.
- `cameleer get health` exits with status 1 when the integration is DOWN.
- Status files are best-effort, so a read-only filesystem no longer stops the integration;
  `CAMEL_LOCAL_STATUS=false` turns them off.

## 0.2.0 (2026-11-05)

### Changed

- **rust-camel is now Cameleer.** Crates are `cameleer-*`, module paths `cameleer_*`, and the
  repository is `pmsudhi/cameleer`.

### Added

- The container image `pmsudhi/cameleer` on Docker Hub and `ghcr.io/pmsudhi/cameleer`, for
  linux/amd64 and linux/arm64.
- 19 crates on crates.io.
- Tested real-world examples in `examples/`.

## 0.1.0 (2026-11-05)

The first public release: Apache Camel's routing engine, every EIP, about 230 components,
languages and data formats, the Rust, YAML and XML DSLs, the REST DSL, Kamelets, route
templates, management, health checks and a Camel-JBang-style CLI, written in Rust on the
standard library alone.
