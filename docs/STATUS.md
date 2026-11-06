# Status and pending work

_Last updated: 2026-11-05. Branch `main`. The verified head before this file
was added is `7de6166`._

## Name, packaging and release (2026-11-05)

- **Renamed rust-camel to Cameleer** after v0.1.0. Another active project, kennycallado/rust-camel,
  uses the same name and owns `camel-core` and most `camel-*` names on crates.io; "CAMEL" is also
  the CAMEL-AI agent framework. Crates are now `cameleer-*` (module paths `cameleer_*`), the repo
  is `pmsudhi/cameleer` (GitHub redirects the old URL), the site is
  https://pmsudhi.github.io/cameleer/. The CLI is `cameleer` (no `camel` binary: that is Apache Camel's CLI name).
- **Container image:** `pmsudhi/cameleer` on Docker Hub and `ghcr.io/pmsudhi/cameleer`
  (linux/amd64, linux/arm64), a glibc binary on distroless/cc (static musl measured 3-17% slower:
  direct -3%, seda InOut -10%, cbr -8%, splitagg -17%, same container, 2 rounds), built by `.github/workflows/container.yml` on `v*` tags (manual runs publish
  `edge`).
- **crates.io:** 19 `cameleer-*` crates, published with `scripts/publish-crates.sh` (dependency
  order, skips versions already there, waits out crates.io's new-crate rate limit of about one
  per 10 minutes). `cameleer-bench` is `publish = false`. The account needs a verified email.
- **Docker Hub:** `pmsudhi/cameleer`. CI pushes there when the `DOCKERHUB_TOKEN` secret is set;
  `.github/workflows/dockerhub-description.yml` syncs `docker/README.dockerhub.md` as the overview.
- **Deployment guide:** `docs/DEPLOYMENT.md` (Docker, Compose, Kubernetes, Cloud Run, AWS Lambda,
  smaller builds). Images run
  with `CAMEL_LOCAL_STATUS=false` (no status or log files) and work with a read-only root filesystem.

- **0.3.0: `cameleer validate` and `cameleer schema`** (`crates/cameleer-main/src/validate.rs`,
  `schema.rs`, DSL tables in `crates/cameleer-yaml/src/model.rs`, `CamelContext::check_endpoint`).
  Codes: yaml-syntax, xml-syntax, dsl-structure, endpoint, expression, unresolved-placeholder,
  dangling-endpoint, duplicate-route-id; text or JSON output; exit 0/1/2. A test validates every
  doc example and every file in `examples/`. Guides: `docs/guides/yaml-dsl.md`,
  `docs/guides/yaml-for-coding-agents.md`, `docs/guides/validate.md`, and `AGENTS.md`.
- **Open findings from the documentation pass** are GitHub issues #2-#15 (Simple numeric string
  comparison, http producer overwriting the message on failure, S3 bucket header, webhook
  signatures, vault env vars, YAML `dataFormats`, `cameleer run <directory>`, ...).

## Where things stand

The implementation is complete against the feature list: `docs/FEATURES.md` has 228 rows, all
✅. Results of the last full run, all in one container session:

| check | result |
|---|---|
| `cargo build --workspace` | 0 warnings |
| hermetic `cargo test --workspace` | **2249 passed, 0 failed** |
| live real servers (`CAMEL_LIVE_INTEROP=1`) | **74 / 0** |
| official clients (`CAMEL_OFFICIAL_CLIENTS=1`) | **58 / 0** |
| third-party simulators (`CAMEL_SIMULATORS=1`) | **35 / 0** |

Independent verification found and fixed about 30 interoperability bugs. They're listed in
`docs/INTEROP.md`, and each has a hermetic regression test.

### What each area was verified against

- **Real servers:**
  - PostgreSQL 16, Redis 7, Mosquitto
  - RabbitMQ with AMQP 0-9-1, STOMP, AMQP 1.0 and MQTT
  - ActiveMQ 5.17.6 with STOMP, MQTT and AMQP 1.0
  - vsftpd (FTP and FTPS), Postfix + Dovecot (SMTP, IMAP, POP3), nginx, OpenSSL `s_client`
    and `s_server`, JDK JSSE
- **Official clients against Cameleer servers and emulators:**
  - librdkafka and kcat, pymongo, the DataStax Cassandra driver, elasticsearch-py, redis-py
  - boto3, the Azure SDK (storage and AMQP), gcloud, gsutil and bq
  - grpcio, Go `crypto/tls` and `net/http` (HTTP/2)
- **Third-party simulators:**
  - moto 5.2.5 (AWS), Azurite 3.37.0
  - fake-gcs-server 1.53.1, pstest (Pub/Sub over gRPC)
  - FerretDB 1.24.2 on PostgreSQL (MongoDB wire), kfake (Kafka wire)

## FaaS and efficiency round (2026-11-05, branch `performance-improvement`)

Research in `benchmarks/perf-research.md`, review with baselines in `benchmarks/perf-review.md`,
results in `docs/BENCHMARKS.md` (section "FaaS cold start and size") and
`benchmarks/analysis.md`. Linux, `dist` builds, against `main`:

- **Cold start:** HTTP route ready 3.27 → 1.17 ms (all components) and 0.69 ms (static musl,
  `core`); timer route 1.52 → 0.66 ms (musl `core`); a run-once job exits in 1.4–2.2 ms instead
  of 22 ms. Windows: HTTP route 12.1 → 10.3 ms, most of it process creation (a hello-world takes
  4.7 ms there).
- **Size:** 24.9 → 23.9 MB with every component; 11.7 MB with `--features cloud`, 7.3 MB with
  `core` (cargo features per component family in `cameleer-main`; `FEATURES` build arg in the
  Dockerfile).
- **Memory:** HTTP route RSS 15.2 → 10.4 MB (all components) and 4.2 MB (musl `core`); private
  memory 1.71 → 0.72 MB; idle HTTP threads 34 → 2.
- **Container image:** 20.5 → 20.4 MB compressed with every component, 14.5 MB with
  `FEATURES=cloud`, 12.5 MB with `FEATURES=core`; a running HTTP route uses 2.6 MiB and 3 threads
  (was 4.5 MiB and 34).
- **AWS Lambda:** built-in custom runtime (`cameleer_main::lambda`, `scripts/package-lambda.sh`),
  checked end to end in AWS's `provided:al2023` image with the Runtime Interface Emulator (init
  3.5–5.2 ms there). Not yet measured on the Lambda service itself (needs an AWS account).
- **Throughput:** unchanged within noise after fixing two regressions a bisect found (log macro
  level check, allocator bin size). Static musl is 3–22% slower, so the image stays glibc.
- **Tests:** hermetic suite 2697 passed, 0 failed on Linux (WSL2) and on Windows, zero warnings;
  `cargo test -p cameleer-main --no-default-features`: 24 passed. One earlier Windows run under
  heavy load failed `cameleer-tls` `mutual_tls_rejects_missing_and_untrusted_certificates` once
  (untouched by this branch; 15/15 on rerun, green in the final run; see follow-ups).

Follow-ups: core cargo features for template engines, jq and the core XPath subset; optional
HTTP/2 (`cameleer-grpc`) in `cameleer-http`; `validate`/`schema` behind a feature;
`-z pack-relative-relocs` for the glibc image; measuring
on the Lambda service across memory sizes and arm64; the TLS 1.3 alert-timing flake above (a
reset can arrive before the alert on Windows under load).

## Idle threads round (2026-11-06, branch `claude/mystifying-swartz-034e67`)

Perf-review finding 16, details and tables in `benchmarks/analysis.md` (section "Idle threads and
per-evaluation threads"); measured with the `idle_threads` example (`scripts/idle-threads.sh` on
Linux), old and new builds alternately on Windows 11 and WSL2 Ubuntu 24.04:

- **XQuery/XSLT** evaluate on the caller's thread and move to a new stack segment only when less
  than 512 KiB of stack is left (`cameleer_core::stack::maybe_grow`). Per evaluation: XQuery
  110 → 10 µs (Windows), 142 → 6.5 µs (Linux); XSLT 103 → 6.3 µs, 138 → 4.0 µs. Deep recursion
  still works from a 2 MiB thread; XQuery recursion 2000 deep is about 30% slower on Linux
  (unchanged on Windows), not traced further.
- **timer:** one thread per timer name (Camel's `java.util.Timer` per name): 20 routes on one
  name use 1 thread instead of 20; the thread exits after the last consumer.
- **Supervising route controller:** its thread exits when nothing is left to retry (1 → 0 threads
  after a recovery).
- **Local status file:** written only when the content changes (10 → 1 writes in 5 s idle);
  readers refresh uptimes from `updated`, and liveness comes from the operating system.
- **Tests:** hermetic suite 2712 passed, 0 failed, zero warnings, twice on Windows 11 and once on
  WSL2 Ubuntu 24.04 (Linux caught a unix-only compile error in the first build).

## Pending / todo

Everything below is extra verification of an implementation that's already done. None of it
is a missing feature. Each item was blocked in the cloud container by network policy, missing
Docker, or the lack of real accounts.

1. **Real cloud accounts.** Run the AWS (S3, SQS, SNS, DynamoDB, Lambda), Azure (Blob, Queue,
   Service Bus, Event Hubs, Cosmos) and GCP (Storage, Pub/Sub, BigQuery) components against
   real services.
   - Credentials must come from the owner. Never sign up automatically.
   - Add a new tier gated by `CAMEL_REAL_CLOUD=1` (`crates/cameleer-cloud/tests/real_*.rs`).
   - Use uniquely named, short-lived resources and delete them in teardown.
2. **Docker-based real servers**, which need Docker plus the ability to pull images:
   - Apache Kafka (KRaft) or Redpanda
   - Elasticsearch and OpenSearch
   - Apache Cassandra or ScyllaDB
   - CouchDB, real MongoDB (including change streams, which FerretDB lacks), ActiveMQ Artemis

   Extend `scripts/live-interop.sh` (or add `scripts/docker-interop.sh`) and add `live_*`
   tests.
3. **Emulators that need Docker or Windows:** LocalStack, the Azure Cosmos DB emulator, the
   Azure Service Bus emulator, and the gcloud Pub/Sub, Firestore and Datastore emulators.
4. **Windows: hermetic tier done (2026-11-03).** On Windows 11 with Rust 1.99.1 MSVC,
   `cargo build --workspace` gives 0 warnings and `cargo test --workspace` gives
   **2252 passed, 0 failed**. Fixes made along the way:
   - `cameleer-tls` RNG: seeds from `BCryptGenRandom` on Windows (it only had `/dev/urandom`).
   - `NetStream`: clones and TLS halves share one `Arc<TcpStream>`. On Windows read
     timeouts belong to the handle, so `try_clone`d handles ignored `set_read_timeout`.
   - `net::shutdown_socket`: Windows `shutdown` doesn't wake a blocked `recv`, so it also
     calls `CancelIoEx`. Used by `NetStream::shutdown`, `HttpServer::stop` and the three
     `TcpServer::stop`s. Before this, stopping MLLP took minutes.
   - `net::bind_listener`: binds IPv4 first, like Java. On Windows `localhost` resolves to
     `::1` first. Used by the protocols, amqp and kafka-mqtt `TcpServer`s and `HttpServer`.
   - `cameleer_core::stack` (Linux and Windows stack bounds), shared by `cameleer-script` and a
     new PL/pgSQL `54001` stack check.
   - `.gitattributes` `eol=lf`, because Git for Windows defaults to `core.autocrlf=true`
     and that broke the byte-exact fixtures.
   - Unix-only tests are `cfg(unix)`: SSH agent sockets, `exec` with `echo`/`sh`, file
     modes. Windows variants were added where they make sense.

   - **Bundled IANA time-zone database (2026-11-04).** `cameleer-core/tzdata/zones.bin` has
     IANA 2026f, 598 zones in about 350 KB, compiled into `cameleer-core`. `tz.rs` uses it after
     `$TZDIR` and the system zoneinfo directories, so named zones work on Windows and in
     minimal containers. Zone tests no longer skip. To refresh it, bump the pinned
     version and SHA-256 in `scripts/update-tzdata.py` (source: PyPI `tzdata`, which is
     IANA compiled by `zic`) and rerun it; the script checks the digest. IANA publishes
     several releases a year, so check at every release.

   - The test IMAP server resolved a bare login (`ann`) to a random mailbox starting with
     `ann@`, sometimes a `moveTo` folder. That made `imap_consumer_with_attachments_and_move`
     fail about half the time. Folders are now excluded and the choice is deterministic.

   Still open on Windows:
   - The process default zone (`TimeZone::local`) is UTC unless `TZ` is set. Java uses the
     Windows zone; matching that needs the Windows-to-IANA name map (CLDR `windowsZones`).
   - About 50 other direct `TcpStream::shutdown` calls, mostly client readers and test
     brokers, should move to `net::shutdown_socket` so `stop()` doesn't hang on idle peers.
   - The live, official-client and simulator scripts are bash and Ubuntu-only. Run them
     under WSL2.
5. **Independent security audit** of the from-scratch crypto and protocol code in
   `cameleer-tls` (TLS state machine, X.509 path validation, constant-time code), `cameleer-ssh`,
   `cameleer-xml` xmlsecurity, and the PGP and crypto data formats. Fuzz the parsers (HTTP/1,
   HTTP/2/HPACK, TLS records, BSON, CQL, AMQP frames, YAML, XML).
6. **Optional cleanup.** Run `cargo clippy --workspace`: `cameleer-script` has about 70 lints, and
   there are older lints elsewhere. Builds are already warning-free.
7. **Performance: three rounds against the 4x target (2026-11-05); 4x is met in three of six
   scenarios.** `scripts/bench.sh` (Linux or Git Bash; `scripts/bench.ps1` on Windows) builds and
   runs `crates/cameleer-bench` and `benchmarks/java-camel`, then writes `docs/BENCHMARKS.md`. The Java
   side uses Apache Camel 4.23.0 on Temurin 21.0.13 with Maven 3.9.16, pinned and checksum-verified.
   Throughput on a Ryzen 5 5600X under Windows 11, idle machine (rust ÷ java, first round → final):
   - direct 0.68x → **4.27x**, seda InOnly 1.09x → **4.18x**, seda InOut 0.77x → **8.37x**
   - cbr 0.56x → 3.48x, splitagg 0.64x → 3.18x, HTTP 2.02x → 2.45x
   - memory: 25–46x less peak RSS; startup: 77x faster (10 ms vs 751 ms)

   What changed (details in `benchmarks/analysis.md`): a std-only thread-caching allocator
   (`cameleer_core::alloc`, opt-in, installed by the `cameleer` CLI and the benchmark), lazy exchange
   and message ids, vector-backed headers (with shared names) and properties (interned Camel
   names), a copy-on-write `ObjectMap`, a TSC tick clock and striped counters for the statistics,
   Camel's `ManagementStatisticsLevel` with **`RoutesOnly` as Cameleer's default** (per-step
   timing is opt-in; Camel collects nothing without `camel-management`), a two-lock seda queue with
   spin-then-park and a reusable InOut reply slot, a borrowing producer cache, faster Simple
   comparisons, a JsonPath parse cache, in-place aggregation and moving split parts into an
   aggregate, lazy HTTP read timeouts. `cameleer-bench profile <scenario> <secs>` is a std-only
   sampling profiler for Windows; `cameleer-bench raw-http <port> threads|iocp8|poll` measures the HTTP
   ceiling. Tests: `crates/cameleer-core/tests/perf_semantics.rs`, `mgmt_stats.rs` (each statistics
   level), plus unit tests in the changed modules.

   Still under 4x (why, and next steps, in `benchmarks/analysis.md`):
   - HTTP: a do-nothing server (threads or IOCP) reaches only about 400k req/s with the shared load
     generator on this machine, 3.6x Java, so 4x cannot be shown here; Cameleer reaches about 70%
     of that. Needs the load generator on another machine. An event-driven cameleer-http server is
     not built (it measured no faster here); the decision is with the owner.
   - splitagg: JSON parsing and per-part exchange copies; next are shared JSON keys (an `ObjectMap`
     API change) and a smaller `Exchange` (public field changes).
   - cbr: the remaining cost is spread over many small steps.
   - Kafka scenario (optional) not done: it needs a real broker on both sides.

## Resuming in a new session

1. `git checkout main && cargo test --workspace`. Expect 2712 passing
   on Windows and on Linux.
2. Pick an item above.
3. Follow the workflow in `CLAUDE.md`.
4. Update this file, and `docs/INTEROP.md` or `docs/FEATURES.md` where relevant, as part of
   the same change.

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
