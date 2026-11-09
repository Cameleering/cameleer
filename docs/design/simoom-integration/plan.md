# Bringing simoom into Cameleer: the long-term plan

_Internal plan, 2026-11-09. Inputs, in this folder:_

- [cameleer-http-inventory.md](cameleer-http-inventory.md): what Cameleer depends on today;
- [simoom-embedding-study.md](simoom-embedding-study.md): how simoom embeds, and its gaps;
- [duplicate-code-map.md](duplicate-code-map.md): the code the two repositories share, and the
  fixes that exist in one copy only.

_Rules it builds on: [../family-crates-std-only.md](../family-crates-std-only.md). Not started:
the owner asked to wait for simoom's second DigitalOcean campaign before any code moves._

> **Update, 2026-11-09 (later the same day).** The owner widened simoom's first release to a
> complete replacement for Cameleer's HTTP stack, so that Cameleer can delete its own code after
> verification. Now in simoom's scope (its plan.md §0 item 0, BUGS.md C1-C11):
> - TLS 1.2 client and server;
> - PKCS#12, JKS and JCEKS keystores with an `SSLContextParameters`-equivalent configuration;
> - CRL, OCSP checking and OCSP stapling;
> - a `simoom-client` crate: HTTP/1.1, HTTP/2 and HTTP/3, pooling, proxies, decompression, and a
>   WebSocket client;
> - WebSocket over HTTP/2, HTTP/2 PING keepalive, gzip/deflate compression and gRPC parity;
> - a blocking TLS stream adapter for Cameleer's non-HTTP protocols.
>
> simoom will keep parity tables (Cameleer feature → simoom → test) in
> `docs/parity/{tls,client,server-extras}.md`.
>
> Consequences for this plan:
> - the "clients stay" non-goal below becomes a later phase: Cameleer's HTTP, cloud, SaaS, MCP and
>   HTTP/2 clients move to `simoom-client` behind the same kind of flag, after the server phases;
> - decision D3 (TLS 1.2) is settled in favour of simoom-tls;
> - Phase 4 shrinks to moving cameleer-tls onto simoom-tls through the blocking adapter, keeping
>   only the Camel configuration layer, with the crypto-core question (D2) still open for SSH, PGP
>   and XML signatures;
> - simoom's parity tables become the checklist for each deletion in Cameleer.

## 1. Goal and non-goals

**Goal.** Cameleer's servers run on simoom's event-driven engine, so that:

- HTTP throughput, latency and idle cost improve. Plain HTTP is Cameleer's one scenario under 2x
  Camel today: 1.67x, and 68% of a do-nothing server;
- Cameleer gains HTTP/2 and HTTP/3 on the shared port;
- every protocol algorithm has one home, with one security surface to audit;
- Cameleer stays std-only, with no third-party code anywhere in its graph.

**Non-goals.**

- **Replacing Cameleer's HTTP clients.** simoom is a server engine with no client. The
  `cameleer-http` client, the cloud and SaaS REST clients, the MCP client and the HTTP/2 client
  (gRPC, Kinesis, Pub/Sub) stay.
- **Changing Camel semantics.** Every option, header, status code and error mapping in
  inventory §1.3 and §4 stays exactly as it is.
- **Changing the route programming model.** Routes stay synchronous, blocking code.

## 2. Target architecture

```
                        Camel layer (stays in Cameleer)
  platform-http / jetty / undertow / netty-http, REST DSL, webhook, /q/* console, MCP HTTP,
  cxf / cxfrs / spring-ws, Lambda dispatch
                │  HttpRouteSpec + RouteHandler (unchanged API)
  ┌─────────────▼──────────────────────────────────────────────────────────┐
  │ cameleer-http: ServerRegistry + Router (unchanged semantics)            │
  │   ServerEngine trait ── ClassicEngine (today's thread-per-connection)   │
  │                      └─ SimoomEngine  (adapter, new)                    │
  │   worker pool (Cameleer's executor: names, limits, backpressure)        │
  └─────────────┬──────────────────────────────────────────────────────────┘
                │ simoom::Service (thin, inline on the loop)
  ┌─────────────▼──────────────────────────────────────────────────────────┐
  │ simoom: runtime (epoll / io_uring / IOCP / kqueue), HTTP/1.1, h2, h3    │
  │ simoom-tls (TLS 1.3 engine) ◄── certificates and keys built by          │
  │                                 cameleer-tls (keystores, SslContextParameters) │
  └────────────────────────────────────────────────────────────────────────┘
```

**The seam is one trait inside `cameleer-http`.** `ServerRegistry` keeps owning ports, routes
and every Camel rule:

- the first registrant's config;
- http and https never mixed on one port;
- 503 for a suspended consumer;
- 204 for OPTIONS on a method mismatch;
- one shared ephemeral port per component for port 0.

Under the trait, an engine only accepts connections and delivers requests. Nothing above
`cameleer-http` changes, and Lambda's in-process `dispatch` never touches an engine.

**How a request flows on simoom** (the pattern the embedding study recommends):

1. A thin `simoom::Service` runs inline on the loop thread. It answers what needs no route code
   there: 404, 405 with `Allow`, the CORS preflight from the registry's tables, and 503 for a
   suspended consumer.
2. Everything else calls `to_owned_request()` and `defer()`, and is submitted to **Cameleer's own
   worker pool**. That keeps Cameleer's limits and thread names, and keeps 503 load-shedding out:
   Camel users don't expect it. simoom's CoDel offload pool is not used.
3. The worker runs the exchange. It answers with `Responder::send`, or streams with
   `Responder::stream` and `BodySink` (SSE, `disableStreamCache`, chunked).
4. Large request bodies are read with `BodySource`'s blocking `io::Read` on the worker, never on
   the loop.
5. A watchdog in the adapter (not simoom's stall watchdog) times out a deferred reply that
   never comes.

**One simoom `Server` per port.** Each port has its own runtime. The shared port
(`camel.server.port`) is one server, so an idle integration still costs one loop thread plus
idle workers, inside today's two-thread goal.

**Configuration.** `camel.server.engine=classic|simoom`, also per endpoint for jetty/undertow
URIs that carry their own port. The default is `classic` until the gates in §6 pass, then
`simoom`, and the classic engine is removed one release later.

## 3. What simoom must add first (upstream work, in simoom's repo)

From the embedding study §6, ranked by what blocks Cameleer:

| # | Gap | Blocks | Notes |
|---|---|---|---|
| U1 | **Connection info on the request**: peer and local address, listener index, TLS version, cipher, SNI, ALPN, **verified client certificate chain** | mTLS headers (`CamelHttpSslClientCert*`), `CamelHttpRemoteAddress` | simoom-tls has them internally; the server API drops them |
| U2 | **Deferred-reply timeout** and cancellation when the client goes away | any route that hangs; MCP calls that wait | today a deferred reply waits forever |
| U3 | **Benchmark the deferred path** (`defer` → foreign thread → `Responder`) | the go/no-go itself | offload measured 34-35 µs CPU against 22.65 µs inline; deferred is unmeasured |
| U4 | **Per-request hook** for access logs and metrics (status, bytes, duration) | `/q/metrics`, route statistics | polled `stats()` only today |
| U5 | **Response trailers** (h2, h3) | moving gRPC (`grpc-status`) | not needed for the HTTP/1.1 phase |
| U6 | **Certificate reload** without restart | rotating certificates on long-running servers | Camel users expect it with `SslContextParameters` reloads |
| U7 | **macOS and arm64 CI** | Cameleer's arm64 image | never run so far |
| U8 | **A stable API and a 0.1 release on crates.io** from the family org | any Cameleer dependency | three days of history, API still moving |
| U9 | **Size headroom** | Cameleer's `core` budget (5.82 MB) | simoom's full build is at 1,040,024 of 1,048,576 bytes; the HTTP/1.1-only build must stay small |

Not needed from simoom: routing (Cameleer keeps its router), multipart and CORS (both
Camel-layer), compression (Camel's own data formats and options), and an HTTP client.

Websocket stays on Cameleer's own server on its own port (9292) in the first phases; moving it
onto simoom's upgrade support is a later step.

TLS 1.2, PKCS#12, JKS, CRL and OCSP are missing from simoom-tls. For the server side this is
covered by the bridge in §5: cameleer-tls decodes keystores and passes DER to
`CertifiedKey::from_der`. **TLS 1.2 on the server** is a decision (§9): it either stays on the
classic engine for those ports, or simoom-tls gains 1.2.

## 4. Phases

Each phase ends with a Cameleer release. No phase starts before the previous one's gate passes.

### Phase 0: parity and guard rails (now, independent of simoom's readiness)

1. **Security fixes Cameleer lacks** (duplicate map, "divergence"). Released as a patch:
   - **HTTP/2 flood limits** in `cameleer-grpc` `h2`: Rapid Reset (CVE-2023-44487), and RST,
     PING, SETTINGS and empty-frame floods, ported from simoom-h2's counters;
   - **a cap on TLS records that deliver nothing** (empty records, ignorable alerts, KeyUpdate
     floods: 32, as in simoom) in `cameleer-tls/src/tls/conn.rs`;
   - **a limit on X.509 chain length before parsing**.
   Each comes with a test that floods a server and checks that it closes the connection.
2. **Port Cameleer's bug-20 regression test to simoom** (RST_STREAM(NO_ERROR) after a complete
   response). simoom handles the case but has no test for it.
3. **The family-only dependency test** and the rule text in all three `CLAUDE.md` files
   (std-only plan, step 1).
4. **The duplicate check** (`scripts/check-duplicates.py`, std-only Python, as designed in the
   duplicate map §3):
   - a reviewed `baseline.json` of the tracked duplicates;
   - a nightly CI job that checks out simoom (and later reads the published crate sources);
   - it fails on a new clone above the thresholds;
   - it raises a **"port the fix" alarm** when a tracked duplicate changes on one side only.
   Until a duplicate is removed, the alarm is the rule "a fix lands in both copies in the same
   week".
5. **Remove duplicates inside Cameleer:**
   - `cameleer-ssh` uses `cameleer-tls`'s Ed25519 (98% identical today);
   - one SHA-256, SHA-1, HMAC and base64 each, in `cameleer-core` or `cameleer-tls`, for the
     several copies across crates.
6. **The `ServerEngine` trait in `cameleer-http`**, with today's server as `ClassicEngine`. This is
   a pure refactor: behaviour is identical, and all of about 660 HTTP tests pass. It removes all
   risk from the later switch, because the switch becomes "add an engine".

### Phase 1: simoom behind a flag for HTTP/1.1 (after U1-U4 and U7-U9)

- `SimoomEngine` in `cameleer-http`, behind the cargo feature `simoom` (in `full`) and
  `camel.server.engine=simoom`.
- **The test matrix:** every HTTP test runs against both engines, selected with
  `CAMEL_HTTP_ENGINE=classic|simoom` in CI, on Windows and Linux. That is about 660 tests in 52
  files (inventory §5). The live, official-client and simulator tiers run on simoom too.
- Behaviour that needs special care (inventory §8):
  - **MCP SSE and long-held responses:** stream from the worker; backpressure moves from the
    256-message channel with a 50 ms give-up to `BodySink`'s blocking write;
  - **CouchDB longpoll:** a worker waits, not the loop;
  - **streamed requests** (`disableStreamCache`): today they take over the socket and close it
    afterwards; on simoom they read `BodySource` and the connection stays usable;
  - **the first registrant's `ServerConfig` wins:** keep it, but fix the `pool_size: 8` trap from
    the console (inventory §1.2) in both engines: the pool size comes from
    `camel.server.*`, not from whoever registers first;
  - **Windows:** simoom's IOCP replaces `shutdown_socket`/`CancelIoEx` and `bind_listener`. The
    alert-before-reset behaviour (INTEROP bug 18) is retested on simoom.
- Release: **0.6**, with simoom as an opt-in engine. The benchmarks are published for both
  engines.

### Phase 2: simoom becomes the default (after the gates in §6, on simoom)

- The default flips to `simoom`; `classic` stays one release as the fallback.
- The cloud mocks and SaaS/datastore emulators that call `HttpServer::bind` move to the registry
  or to the engine trait, so the test servers exercise the production engine.
- Release **0.7**. The classic engine and its Windows socket workarounds are deleted in **0.8**,
  if no regression was reported against `simoom`.

### Phase 3: HTTP/2 and HTTP/3 on the shared port; the gRPC server (after U5)

- h2 and h3 on `camel.server.port` through simoom: ALPN `h2` on TLS ports, prior-knowledge h2c,
  h3 with `camel.server.http3=true` (QUIC on UDP, same port number).
- **First split the h2 server from the client in `cameleer-grpc`.** Today both share
  `h2/conn.rs` (inventory §8.2). The server side moves to simoom-h2; the client side stays and is
  slimmed down to what clients need.
- The gRPC server (`GrpcServer`) moves onto simoom with response trailers. INTEROP bugs 19 and 20
  are re-verified on the new path, with grpcio and Go's `net/http` (the official-client tier).
- Release **0.9**.

### Phase 4: one TLS and crypto core

The largest audit win, and the largest change. The duplicate map shows:

- X.509, ASN.1, PEM, keys and certgen copied almost whole (about 2,500 lines);
- RSA and EC partly copied;
- the fast primitives (P-256, AES-GCM, X25519, ChaCha20, SHA-2) rewritten in simoom with
  hardware paths;
- Cameleer ahead on breadth (TLS 1.2, OCSP, CRL, keystores, RSA-OAEP, SHA-1).

Target layering, with no cycles (simoom must never depend on Cameleer):

```
crypto + X.509 core (one family crate; home is decision D2)
   ▲                         ▲
simoom-tls (TLS 1.3, QUIC)   cameleer-tls (Camel layer: SslContextParameters, keystores,
   ▲                           TLS 1.2 client and server, OCSP, CRL, SASL; TLS 1.3 by calling
simoom                         simoom-tls)
                               ▲
                     cameleer-ssh, pgp, xmlsecurity, crypto data format, cameleer-http
```

Steps: move one primitive family at a time into the core crate:

1. hashes and HMAC;
2. AES-GCM and ChaCha20-Poly1305;
3. X25519 and P-256;
4. Ed25519;
5. RSA;
6. X.509 and ASN.1.

Each step gets the cross-checks against OpenSSL and Go `crypto/tls` (the official-client tier),
and the before/after crypto benchmarks. Then cameleer-tls's TLS 1.3 path calls simoom-tls, and
its TLS 1.2 path stays until decision D3 says otherwise. Release in steps from **0.10**.

### Phase 5: karez for database protocols (after karez's own go/no-go milestone)

The same pattern, later: the PostgreSQL wire client in `cameleer-sql` moves to karez behind a
flag. The embedded `rustsql` engine stays in Cameleer.

## 5. Keeping std-only

Unchanged from [../family-crates-std-only.md](../family-crates-std-only.md), and enforced from
Phase 0:

- **Closed family:** Cameleer depends only on `cameleer-*`, `simoom*` and `karez*`, each std-only
  recursively. The test reads `cargo metadata --locked` and fails on anything else, on any
  non-crates.io or git source, and on outside build scripts or proc macros.
- **simoom's competitor programs** (`bench/competitors/`, outside its workspace) are never
  published and never a dependency. The check confirms they don't appear in the graph.
- **Released, not pathed:** Cameleer's `main` depends on published simoom versions. For
  co-development, use an uncommitted `[patch.crates-io]` that points at a local simoom checkout.
- **Offline builds stay possible:** release source tarballs include `cargo vendor` output. CI
  keeps one job that builds with `--offline` from the vendored sources.
- **Same rules everywhere:** zero warnings, a test for every fix, hermetic tests, measured claims,
  Apache-2.0. `unsafe` stays confined to `simoom-sys` on simoom's side; Cameleer adopts the same
  rule over time.

## 6. Gates (each must hold before the next step)

| gate | measured how | pass when |
|---|---|---|
| Semantics | the HTTP test matrix on both engines, Windows and Linux; the live, official and simulator tiers | identical results; zero new failures |
| Throughput and latency | `scripts/cloud-bench` on DigitalOcean (the published method): fixed reply, REST JSON, HTTP into Kafka, MCP session calls | simoom engine ≥ classic on throughput **and** p99 at equal CPU, in every scenario; HTTP fixed reply reaches at least 2x Camel |
| Blocking routes | a new scenario: a route that blocks 100 ms (a slow backend), 500 concurrent clients | no 503s, no loop stalls, throughput ≥ classic with 200 workers |
| Idle cost | `scripts/idle-threads.sh` | ≤ today: 2 threads for an idle HTTP route; RSS within +5% |
| Size | `core` and `full` dist builds | `core` within +150 KB of 5.82 MB (tracked in `benchmarks/analysis.md`); `full` within +300 KB |
| Start-up | the cold-start measurements in docs/BENCHMARKS.md | HTTP route ready within +5% of 1.17 ms |
| Platforms | CI on Windows, Linux x86-64 and Linux arm64; macOS build | all green |
| Security | Phase 0 flood tests; HTTP Garden and h2spec against Cameleer's server on simoom | pass, as simoom does standalone |

## 7. Versions, releases and syncing

- **Order:** a simoom release first, then the Cameleer release that uses it. Cameleer's
  `scripts/publish-crates.sh` refuses to run if a family dependency version is missing from
  crates.io.
- **Versions:**
  - simoom is 0.x with semver, and Cameleer uses caret requirements;
  - a simoom breaking version is adopted on purpose, in its own commit, with the benchmarks rerun;
  - a compatibility table in Cameleer's docs says which simoom each release uses.
- **Cross-repo CI:**
  - Cameleer runs nightly against simoom `main` (patched to a git checkout in CI only);
  - simoom runs Cameleer's HTTP test matrix on its pull requests (downstream check);
  - either side learns about a break the day it happens.
- **Issues:** a bug seen through Cameleer in the engine is filed in simoom and linked. The
  Cameleer release that picks up the fix says so in its changelog.
- **Claims:** every switch reruns the published benchmarks and updates docs/BENCHMARKS.md, the
  comparison pages and the site in the same release.

## 8. Risks

| risk | effect | mitigation |
|---|---|---|
| simoom's API keeps changing | rework in the adapter | wait for U8 (a 0.1 with an API freeze); the adapter is one module behind one trait |
| the deferred path is slower than expected | no throughput win for blocking routes | U3 before Phase 1; if it loses, keep `classic` and fix simoom's handoff first |
| behaviour drift on edge cases (OPTIONS, HEAD, 415/406, chunking, headers) | Camel users see changes | the engine matrix runs every existing test on both engines; the router stays Cameleer's |
| Windows regressions | the IOCP path differs from today's | Windows in the matrix from day one; bug 18 retested |
| size growth | the `core` budget breaks | U9 and the size gate; HTTP/2 and HTTP/3 stay optional features |
| fixes land in one copy only during the transition | security gaps (already happened: Rapid Reset, record flood) | Phase 0 fixes now; the nightly duplicate alarm; delete each duplicate one release after its switch |
| a single maintainer for two engines during the overlap | slower releases | the classic engine is frozen (security fixes only) from Phase 1, and removed in 0.8 |
| crates.io availability for builds | builds that used to be offline now fetch | vendored release tarballs and an offline CI job |

## 9. Decisions for the owner

- **D1. Order of Phase 0 security fixes:** release them as 0.5.2 now (recommended), or with
  Phase 0's other work.
- **D2. Home of the shared crypto and X.509 core:**
  - (a) inside simoom's workspace, as a crate such as `simoom-crypto`. Simplest to start, but the
    name ties general crypto (used by SSH, PGP, XML signatures) to an HTTP brand;
  - (b) a neutral family crate in its own repository;
  - (c) keep the duplicates and rely on the alarm.
  Recommended: (b), named in the family's style, created when Phase 4 starts.
- **D2, decided (2026-11-09): saltire.** Agreed by both sessions, and the owner chose the name **saltire** (`saltire` and `saltire-pki`, repository `<family-org>/saltire`): option (b), a neutral std-only
  crypto family in its own repository under the family org, in two crates:
  - **primitives** (`<name>`): hashes, HMAC, HKDF and PBKDF2; AES-CBC, CTR and GCM;
    ChaCha20-Poly1305; X25519, P-256 and P-384, Ed25519, RSA PKCS#1 v1.5, PSS and OAEP; bigint;
    OS random; base64 and hex for PEM. The legacy algorithms (MD5, SHA-1 HMAC for old protocols,
    DES and 3DES, RC2, PBE) sit behind a `legacy` feature, off by default;
  - **PKI** (`<name>-pki`): ASN.1 and DER, PEM, PKCS#8, X.509 parsing and path validation, CRL,
    OCSP, and PKCS#12, JKS and JCEKS decoding.

  The API is concrete types and free functions: streaming and one-shot hashes, in-place AEAD
  with caller-owned buffers, keys cleared on drop, and a minimal `Digest` trait only for
  HMAC/HKDF/PBKDF2. Errors are small enums, with no panics on attacker input. Cameleer keeps its
  free-function style as thin wrappers during the move.

  All `unsafe` (the hardware intrinsics and OS random, today in `simoom-sys/src/arch.rs`) goes into
  one `arch` module or crate, under SAFETY comments and an unsafe counter in CI. simoom's rule
  becomes "unsafe only in -sys/-arch".

  Protocol engines stay with their products: TLS 1.3/1.2 and QUIC in simoom-tls; SSH, OpenPGP,
  XML-DSig and Camel's `SslContextParameters` layer in Cameleer.

  The crate owns the constant-time (dudect-style) tests, the Wycheproof-style vectors, the fuzz
  harnesses and the speed comparison against OpenSSL. No FIPS claim, only "follows the FIPS
  test vectors". One external audit covers the crypto crates, simoom-tls, and Cameleer's SSH,
  PGP and XML-DSig.

  Sequence:
  1. ~~The owner picks the name~~: saltire.
  2. After simoom's second campaign, the repository is created; simoom's faster primitives and
     its PKI move first.
  3. Cameleer's breadth is added (OAEP, the legacy primitives, keystores; simoom's agents already
     port Cameleer's tested code into self-contained modules for this).
  4. simoom switches.
  5. Cameleer switches and deletes `cameleer-tls/src/crypto` and the stray SHA, HMAC and base64
     copies in core, cloud, ai, sql, protocols and ssh.

  Name candidates, free on crates.io on 2026-11-09: **saltire** (and `saltire-pki`), carnelian,
  adamant. The GitHub user name `saltire` is taken, so the repository would be
  `<family-org>/saltire`.
- **D3. TLS 1.2 on servers:** keep it on Cameleer's own TLS (cameleer-tls stays the 1.2 engine
  under the simoom engine), or add TLS 1.2 to simoom-tls. Recommended: keep it in cameleer-tls
  until simoom has a reason of its own to add it; most Camel deployments still need 1.2 for
  clients.
- **D4. Websocket:** keep Cameleer's websocket server on its own port, or move it onto simoom's
  upgrade support in Phase 3. Recommended: move it in Phase 3, behind the same engine flag.
- **D5. Release numbers:** the plan assumes 0.6 (opt-in), 0.7 (default), 0.8 (classic removed),
  0.9 (h2/h3 and gRPC), 0.10+ (TLS core). Adjust to taste.

## 10. Order of work, in one list

1. Phase 0.1: the security fixes (D1), as a patch release.
2. Phase 0.3-0.4: the family-only dependency test, the rule text, and the duplicate check with
   its baseline.
3. Phase 0.5-0.6: duplicates removed inside Cameleer; the `ServerEngine` trait with
   `ClassicEngine`.
4. simoom U1-U4, U7-U9, in simoom's repo; simoom 0.1 published from the family org.
5. Phase 1: `SimoomEngine` behind the flag; the engine test matrix; the benchmarks. Release 0.6.
6. Phase 2: the default flips (0.7); classic removed (0.8).
7. Phase 3: the h2 server and client split; h2/h3 on the shared port; gRPC on simoom (0.9).
8. Phase 4: one crypto and X.509 core, one primitive family at a time (0.10+).
9. Phase 5: karez, after its milestone.
