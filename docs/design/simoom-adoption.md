# Plan: Cameleer adopts simoom for its HTTP stack

_Internal design note, 2026-11-13, for the owner to decide on. Nothing here has been built, and no
`Cargo.toml` was changed (hard rule 1 needs the owner's approval for a dependency). Sources are
simoom's own docs (`docs/STATUS.md`, `docs/parity/*.md`, `docs/SECURITY.md`,
`bench/results/2026-11-12-campaign4/`) and a read of the Cameleer code. Figures marked
"simoom's measurement" were not re-run here._

## 1. Decision wanted

Should Cameleer replace its own HTTP server, HTTP client, HTTP/2 and the HTTP side of its TLS with
the published simoom 0.1.0 crates?

**Recommendation: yes, in stages, opt-in first.** The owner already set this direction (simoom
plan §0 items 0 and 8: Cameleer deletes its HTTP, HTTP/2, WebSocket and TLS code once simoom is
tested, verified and benchmarked, and depends on the published crates). What this note adds is the
gain-and-loss accounting and a path with a gate and a way back at every step.

## 2. What would change

| area | today | after |
|---|---|---|
| HTTP/1.1 server | `cameleer-http/src/server.rs` (556 lines), thread per connection (`pool_size` workers) | `simoom` (event loop per core, epoll/io_uring/kqueue/IOCP) behind the same `HttpHandler` trait |
| HTTP client | `cameleer-http/src/client.rs` (786 lines) | `simoom-client` (pool, h1/h2/h3, proxies, redirects, WebSocket) |
| HTTP/2, HPACK | `cameleer-grpc/src/h2`, `hpack` (3,291 lines), used by the http producer, AWS cloud code and mocks, gRPC | `simoom-h2`, `simoom-hpack` |
| TLS for HTTP | `cameleer-tls/src/tls` | `simoom-tls` (TLS 1.3 and 1.2, QUIC) |
| WebSocket, HTTP/3, compression | WebSocket in `cameleer-http`; no HTTP/3 | `simoom` WebSocket (h1, h2, h3), HTTP/3 over QUIC, `simoom-compress` |
| **Not changing** | Camel semantics: `http.rs` (1,183), `rest.rs`, `platform.rs`, `registry.rs`, `producer.rs`, `netty.rs`, headers and exception kinds | stays; only the transport under it moves |

Users of the server API inside Cameleer: `HttpServer` is used by the platform-http registry,
the MCP HTTP endpoint (`cameleer-ai`), the CouchDB and Elasticsearch emulators, the SaaS test
servers, the health FHIR test server, the Lambda runtime and the bench crate. 152 call sites use
`cameleer_http::`. A facade that keeps `HttpServer`, `ServerConfig`, `HttpHandler`, `HttpRequest`
and `HttpResponse` unchanged keeps all of them untouched.

`cameleer-tls` cannot go away: 19 crates use it, and much of it is not TLS (SASL, key stores,
X.509, certgen, PGP, SSH and data-format crypto, `camel_support`, `stream_body`). Only the TLS
state machine for HTTP moves. Kafka, AMQP, PostgreSQL, MQTT, SSH and the rest keep using it until
the planned `ksar` crypto family absorbs the primitives.

## 3. What we gain

Measured by simoom on DigitalOcean c-4 droplets, same machines and rounds, simoom against
Cameleer's current server (`cameleer-http-min`), campaign 4 (2026-11-12):

| measure | cameleer-http | simoom | result |
|---|---:|---:|---|
| status endpoint, 256 clients, req/s | 113,042 | 137,443 | 1.22x |
| JSON API, req/s | 103,814 | 133,243 | 1.28x |
| CPU per request at a fixed 50k req/s | 36.2 µs | 24.3 µs | 1.49x less |
| HTTP/1.1 p99 latency (one scenario) | 608 µs | 356 µs | 1.71x lower |
| new TLS handshakes per second | 1,517 and 4,487 | 8,973 and 9,924 | 5.9x and 2.2x |
| 10,000 idle connections, memory | 70.4 MB | 3.0 MB | 23x less |
| overload: target rate met | not met (p99 up to 32 s) | met | only simoom |
| HTTP/2 service-to-service | — | 2nd of 12 servers | Cameleer has no event-driven h2 server |

Earlier WSL2 measurement (plan §0 item 7 requirement): 1.7–1.8x requests per second, p99 2–2.7x
lower, 43% less CPU per request, 1,000 idle connections in 2.7 MB instead of 7.7 MB. Cold start is
within 5% of today's or faster on Linux and Windows.

What that buys Cameleer, by workload:

1. **HTTP-facing routes** (`platform-http`, REST DSL, webhooks, health checks): today's measured
   ceiling is 68% of a do-nothing server and about 1.67x Camel. The event loop removes the
   thread-per-connection cap, so the ceiling rises. **How much of this shows up in real routes is
   not measured**: a route spends most of its time in the route engine, not the socket, so expect
   less than the raw 1.2–1.8x. Milestone 1 measures it.
2. **MCP server and gateway:** per-request CPU is about twice a plain HTTP request (STATUS, open
   gaps); the transport share shrinks. Idle sessions cost kilobytes, not a thread each. SSE and
   long-lived streams stop pinning workers.
3. **Many connections, small devices, FaaS:** 23x less memory at 10k idle connections. Fewer idle
   threads on top of the 0.4.0 work.
4. **HTTPS:** 2–6x handshake rate, which matters for clients that connect per request and for
   cold-start bursts.
5. **New capabilities Camel has no equivalent for:** HTTP/3 and WebSocket over h2/h3, h2 server
   without a thread per connection, strict anti-smuggling parsing, bounded limits and slow-attack
   guards, response compression negotiation.
6. **One HTTP/TLS/h2 implementation instead of two.** simoom's TLS and h2 were ported from
   Cameleer and have since diverged (TLS 1.2 interop with OpenSSL, the K11 and K12 fixes, h2 DoS
   accounting, fuzzing). Every fix would otherwise be made twice. Deleting about 10,000 lines
   (server 556, client 786, h2 and hpack 3,291, HTTP TLS state machine roughly 5,000) removes that
   maintenance.
7. **Family coherence:** Cameleering ships one engine that Cameleer, simoom users and later
   packs share, and the `cameleer.ing/simoom/` story ("Cameleer's server, used in Cameleer")
   becomes literally true.

## 4. What we lose, or risk

| risk | detail | how to contain it |
|---|---|---|
| **Maturity.** simoom is 0.1.0, about 790 tests, published 2026-11-12. Cameleer's HTTP code has years of the 2,960-test suite and 74 live, 58 official-client and 35 simulator checks behind it. | A new engine will have bugs the old one lost already. | Opt-in feature first; rerun every live, official and simulator tier before any default flip; keep the old code for two releases. |
| **Release coupling.** Cameleer pins simoom crates; a simoom security fix needs a simoom release, then a Cameleer release. 12 more crates in the dependency tree (22 Cameleer + 12 simoom). | Slower, two-repo loop. | Same owner and CI; `Cargo.lock` pinning; one publish order script (simoom first). |
| **`unsafe` in the tree.** `simoom-sys` has about 310 `unsafe` uses (epoll, io_uring, IOCP, SIMD, AES intrinsics). Cameleer today has little. | Memory-safety risk moves from "none in HTTP" to "audited in one crate". | simoom forbids `unsafe` elsewhere, has SAFETY comments, fuzzing and dudect timing tests; add an independent review to the todo already open for TLS. |
| **Threading model.** Cameleer routes block. Slow routes run in simoom's offload pool, so each request pays a thread hop. | Possible small latency cost for I/O-heavy routes; the offload pool is a new tuning knob. | Measure in milestone 1; per-route inline or offload choice; the stall watchdog. |
| **Behaviour changes.** Stricter parsing than today's server (rejects ambiguous framing, obs-fold, `Transfer-Encoding` with `Content-Length`); `insecureSkipVerify` and `hostnameVerification=false` are refused; no 0-RTT; default `Date` and header ordering may differ. | A route that relied on leniency breaks. | Strictness matches Camel's own Netty/Jetty in practice but must be checked; list in release notes under "Changed"; a lenient option only if a real case appears. |
| **Windows and low-end numbers.** Windows private memory is 7–14% higher idle (110–140 KB); campaign 4 shows two memory rows where Cameleer's server uses less (1,307 vs 1,550 KB and 1,142 vs 1,844 KB), and one throughput row where it is 1.09x ahead. | Tiny single-connection programs get no gain, a few KB worse. | State it in docs; none of it changes the idle-connection win. |
| **Binary size.** simoom budgets: 250 KB (h1), 600 KB (h1+h2+tls), 1 MB (everything). Cameleer's `core` build is 5.82 MB; savings from deleted code are not yet measured against that. | Possible net growth. | Measure in milestone 1 for `core` and `full`; gate on "no worse than +2% for `full`". |
| **Test and mock code.** Cloud mocks (`cameleer-cloud/src/testing/aws*.rs`, `gcp_more.rs`) call `cameleer_grpc::h2` directly; they are the largest rewrite. | Real work, no gain for users. | Keep `cameleer-grpc` h2 in place for tests until a late stage; port mocks last. |
| **Documented gaps.** simoom's parity docs list: no RSA-OAEP (XML Encryption stays in cameleer-xml), no `shutdown_socket` helper, client session cache keyed by server name only. | Small, known. | Stay in Cameleer. |

## 5. Plan

Every stage is a separate branch and PR, builds with zero warnings, and passes
`cargo test --workspace` before the next starts.

**Stage 0: approval (owner).** Approve adding simoom crates as optional dependencies behind a
feature. Nothing else.

**Stage 1: spike, server only (about one week).**
- Cargo feature `simoom` on `cameleer-http`, off by default; `server_simoom.rs` implementing
  `HttpServer`/`HttpHandler`/`ServerConfig` over `simoom`. Platform-http registry, MCP, emulators
  untouched.
- Measure with `cameleer-bench` and `scripts/cloud-bench` (same droplets, same method as the
  published comparison): the HTTP scenario, MCP session, JSON, a route with a blocking `to()`,
  idle connections, `core` and `full` binary size, Windows.
- **Gate:** HTTP scenario and MCP at least 1.2x, no scenario slower than `main`, size within
  +2% for `full`. If it fails, stop; Cameleer keeps its server and simoom stays standalone.

**Stage 2: server parity.** Run `crates/cameleer-http/tests`, `cameleer-ai/tests/mcp*.rs`,
`mcp-conformance`, the live tier (nginx, curl, Go `net/http`), the official-client tier and
`scripts/simulators.sh`. Add regression tests for every behaviour change in §4. Ship in 0.7.0 as
`--features simoom`, with the server swap documented.

**Stage 3: client and HTTP/2.** The http producer over `simoom-client`; `cameleer-cloud` AWS
HTTP/2 and gRPC over `simoom-h2` / `Client::call`; WebSocket client and server. Keep old code
behind the feature. Re-run `docs/INTEROP.md` bug regressions (19 HTTP/2 leak, 18 TLS alert).

**Stage 4: TLS for HTTP.** `simoom-tls` for HTTP listeners and clients, with `SslContextParameters`
mapped through `camel_support`. Other protocols keep `cameleer-tls`. Independent TLS review before
claiming the BCP 195 profiles.

**Stage 5: default and cleanup.** After one full release as opt-in and a clean field report:
make `simoom` the default (0.8), keep a `classic-http` feature one more release, then delete the
old server, client and HTTP/TLS code (0.9). Update `docs/FEATURES.md`, STATUS, the site and the
benchmark pages in the same change (SEO session).

**Rollback at every stage:** the feature flag. Nothing is deleted before stage 5.

## 6. Overall

- **Certain:** one maintained HTTP/TLS/h2 stack, about 10,000 fewer lines in Cameleer, HTTP/3
  and h2-without-threads for users, 23x less idle-connection memory, 2–6x TLS handshake rate,
  1.2–1.8x raw server throughput with lower CPU and tail latency.
- **Probable but unmeasured:** a smaller, real-route gain (route engine bound), lower MCP cost.
- **Cost:** a young engine in the hot path, a second release train, about 310 audited `unsafe`
  uses, a few behaviour changes, and several weeks of work spread over stages 1 to 5.
- **Net:** worth doing because the gain is large where Cameleer is weakest (connections, TLS,
  tails), the downside is bounded by the feature flag, and the alternative is to keep fixing two
  copies of the same code.

## 7. Open questions for the owner

1. Approve stage 0 (optional simoom dependency behind a feature)?
2. Is "no worse than +2% binary size for `full`" the right gate, or should size be allowed to grow
   for HTTP/3?
3. Should the lenient-parsing escape hatch exist, or does Cameleer follow simoom's strictness?
4. Order: do this before the SAP pack work, or after?
