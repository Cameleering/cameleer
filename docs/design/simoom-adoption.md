# Plan: Cameleer adopts simoom for its HTTP stack

_Internal design note, 2026-11-13, for the owner to decide on. Nothing here has been built, and no
`Cargo.toml` was changed (hard rule 1 needs the owner's approval for a dependency). Sources are
simoom's own docs (`docs/STATUS.md`, `docs/parity/*.md`, `docs/SECURITY.md`,
`bench/results/2026-11-12-campaign4/`) and a read of the Cameleer code. Figures marked
"simoom's measurement" were not re-run here._

## 1. Decision wanted

Should Cameleer replace its own HTTP server, HTTP client, HTTP/2 and the HTTP side of its TLS with
the published simoom 0.1.0 crates?

**Recommendation (revised 2026-11-13, see �9): adopt only if a measured gate is met, and then fully; no optional feature.** The owner already set this direction (simoom
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

## 8. Stage 1 spike result (2026-11-13, local, not droplets)

Branch `simoom-spike`: `cameleer-http` feature `simoom`, selected at run time with
`CAMEL_HTTP_ENGINE=simoom` (plain HTTP; TLS keeps the classic engine). Handlers run on simoom's
offload pool by default, or on the loop thread with `CAMEL_SIMOOM_INLINE=1`. Script: `ab-local.sh`.

Method: WSL2 Ubuntu 24.04, server pinned to 4 CPUs, load generator on 8 others, 64 connections,
`cameleer-bench serve` and `loadgen-remote`, 5 s warm-up, 3 x 5 s, 2 interleaved rounds. Requests
per second, mean of the rounds:

| scenario | classic | simoom, offload pool | simoom, inline |
|---|---:|---:|---:|
| `http` (small JSON route) | 109,300 | 111,600 (+2%) | 137,100 (+25%) |
| `rest-json` | 95,700 | 101,800 (+6%) | 113,400 (+18%) |
| `mcp-session` | 67,300 | 54,200 (**-19%**) | 36,200 (**-46%**) |

p99 is higher with simoom in every scenario (HTTP 1.7 ms against 1.3 ms; MCP 4.0 ms against 3.4
ms with the pool).

**Gate (HTTP and MCP at least 1.2x, nothing slower than `main`): not met.** What the numbers say:

- The raw-server gain (1.2 to 1.8x) does not carry into routes that block: with the pool, every
  request pays a thread hop and a copy, and the gain falls to 2 to 6%.
- Inline works for routes that never block (+18 to +25%) but is unsafe for the rest: the MCP
  session scenario waits inside the handler, stalls the loops and loses 46%.
- So the adapter, not the engine, decides the result. The next step would be per-route mode (inline
  for registry routes marked non-blocking, pool for the rest), and finding what makes
  `mcp-session` slow even on the pool (it is 19% slower, more than a hop explains).
- Not measured: droplets (the plan's reference machines), TLS, idle connections, binary size.
- Test differences found: 3 of the http tests assert wire details simoom does differently (lower-case
  header names, no per-connection worker cap, 8 KiB chunk sizes); all others pass with the engine on.

## 9. MCP slowdown found, and the decision rule (2026-11-13)

**Owner rule:** no optional feature. Either simoom is a measured advantage and Cameleer uses it
(and deletes the old server), or Cameleer does not use it. The `simoom` feature and
`CAMEL_HTTP_ENGINE` switch exist only on the spike branch to take measurements, and sections 5 and 6
are superseded: there is no opt-in release, no `classic-http` fallback, and no staged default flip.
Stages 2 to 5 run only after a go decision, as one change.

**Why `mcp-session` is slower** (same A/B, plus CPU per request from `/proc/<pid>/stat` and thread
states while under load, `cpu-ab.sh` and `thr.sh`):

| scenario | engine | req/s | server CPU per request | threads |
|---|---|---:|---:|---:|
| `mcp-session` | classic | 64,300 | 59 �s | 141 |
| `mcp-session` | simoom, pool | 50,700 | 70 �s (+19%) | 145 |
| `mcp-session` | simoom, loop thread | 36,700 | 75 �s, CPUs 69% busy | 40 |
| `http` | classic | 97,600 | 40 �s | 67 |
| `http` | simoom, pool | 113,400 | 36 �s | 12 |
| `http` | simoom, loop thread | 130,300 | 30 �s | 5 |

- In a legacy MCP session every `tools/call` runs on the MCP server's own worker pool
  (`mcp-call`, `call_with_channel`) and the HTTP handler thread waits on a channel for the result.
  That is one hop already. Classic has it once (connection thread, then `mcp-call`).
- With simoom's pool there are two hops (offload worker, then `mcp-call`): 11 �s more CPU per
  request on a box where CPU is the limit, so 21% fewer requests.
- On the loop thread the handler blocks the loop while `mcp-call` works. Four loops can then have
  only four calls in flight and the CPUs sit idle (31%): 43% fewer requests.
- Non-blocking routes (`http`) do gain, and with fewer threads (5 against 67): 20 to 34% more
  requests, 10 to 25% less CPU per request.

**What it takes to win on MCP and every other blocking route:** the handler API must become
completion-based, so the handler hands the request on and answers later through a responder
(simoom's `Reply::defer`). Then no thread waits, the `mcp-call` worker answers straight to the
connection, and the hop disappears. That changes `HttpHandler`, the registry's `RouteHandler`,
`platform-http`, the REST DSL and the MCP endpoint, not just the engine. It is the real cost of
adoption; the figures above suggest roughly a 20 to 35% gain for non-blocking routes and, if it
removes a hop, a gain on MCP too, but that is not yet measured.

**Go / no-go gate, replacing the old one:** build the completion-based path for `platform-http` and
the MCP endpoint on the spike branch, and re-measure on the droplets. Go only if `http`, `rest-json`
and `mcp-session` are each at least 1.2x classic with lower or equal CPU per request and p99 no worse.
Otherwise no-go: close the spike, keep `cameleer-http`.

## 10. DigitalOcean round (2026-11-13)

Method as simoom's same-droplet A/B: three c-4 droplets in blr1 (Ubuntu 24.04), server droplet `a` in
round 1 and `b` in round 2, a separate load node on the private network, variants interleaved,
`cameleer-bench loadgen-remote`, 5 s warm-up and 3 x 5 s per run. Server CPU from `/proc/<pid>/stat`
over 14 s of load. Two rounds, so a ratio is the mean of two runs on two droplets (droplet `b`
was about 25% faster than `a` for every variant). Raw lines, machine and script:
`docs/design/simoom-adoption-results/`. Droplets deleted afterwards.

| scenario | variant | req/s | vs classic | p99 | CPU per request | threads | peak RSS |
|---|---|---:|---:|---:|---:|---:|---:|
| `http`, 64 conns | classic | 116,300 | 1.00x | 2.51 ms | 33.8 µs | 67 | 12.1 MB |
| | simoom, pool | 133,600 | 1.15x | 1.38 ms | 27.3 µs | 12 | 10.9 MB |
| | simoom, loop thread | 160,700 | 1.38x | 0.81 ms | 16.2 µs | 4 | 8.4 MB |
| `http`, 256 conns | classic | 98,600 | 1.00x | 10.9 ms | 37.1 µs | 202 | 25.9 MB |
| | simoom, pool | 139,800 | 1.42x | 4.22 ms | 24.9 µs | 14 | 11.9 MB |
| | simoom, loop thread | 154,900 | 1.57x | 4.27 ms | 16.1 µs | 4 | 9.1 MB |
| `rest-json`, 64 | classic | 94,300 | 1.00x | 2.75 ms | 42.1 µs | 67 | 13.5 MB |
| | simoom, pool | 113,400 | 1.20x | 1.57 ms | 34.0 µs | 13 | 12.0 MB |
| | simoom, loop thread | 134,300 | 1.42x | 1.34 ms | 20.8 µs | 4 | 9.2 MB |
| `mcp-session`, 64 | classic | 41,500 | 1.00x | 6.03 ms | 89.6 µs | 146 | 45.5 MB |
| | simoom, pool | 36,700 | **0.88x** | 6.39 ms | 89.7 µs | 140 | 56.4 MB |
| | simoom, loop thread | 31,800 | **0.77x** | 5.19 ms | 62.0 µs | 13 | 13.3 MB |

**Gate (each scenario at least 1.2x with CPU and p99 no worse): not met.** Plain routes win clearly,
and more so at 256 connections (1.42x with the pool, p99 2.6x lower, 8x fewer threads, half the
memory). `mcp-session` loses (0.88x) with the same CPU per request; on the loop thread it uses 31%
less CPU per request but the blocked loops cap throughput. This confirms section 9: the cause is
the blocking handler path, not the engine. The decision now rests on whether a completion-based
handler API for `platform-http` and the MCP endpoint (section 9) lifts `mcp-session` to 1.2x.
