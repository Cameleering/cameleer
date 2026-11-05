# Status and pending work

_Last updated: 2026-11-03. Branch `main`. The verified head before this file
was added is `7de6166`._

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
- **Official clients against rust-camel servers and emulators:**
  - librdkafka and kcat, pymongo, the DataStax Cassandra driver, elasticsearch-py, redis-py
  - boto3, the Azure SDK (storage and AMQP), gcloud, gsutil and bq
  - grpcio, Go `crypto/tls` and `net/http` (HTTP/2)
- **Third-party simulators:**
  - moto 5.2.5 (AWS), Azurite 3.37.0
  - fake-gcs-server 1.53.1, pstest (Pub/Sub over gRPC)
  - FerretDB 1.24.2 on PostgreSQL (MongoDB wire), kfake (Kafka wire)

## Pending / todo

Everything below is extra verification of an implementation that's already done. None of it
is a missing feature. Each item was blocked in the cloud container by network policy, missing
Docker, or the lack of real accounts.

1. **Real cloud accounts.** Run the AWS (S3, SQS, SNS, DynamoDB, Lambda), Azure (Blob, Queue,
   Service Bus, Event Hubs, Cosmos) and GCP (Storage, Pub/Sub, BigQuery) components against
   real services.
   - Credentials must come from the owner. Never sign up automatically.
   - Add a new tier gated by `CAMEL_REAL_CLOUD=1` (`crates/camel-cloud/tests/real_*.rs`).
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
   - `camel-tls` RNG: seeds from `BCryptGenRandom` on Windows (it only had `/dev/urandom`).
   - `NetStream`: clones and TLS halves share one `Arc<TcpStream>`. On Windows read
     timeouts belong to the handle, so `try_clone`d handles ignored `set_read_timeout`.
   - `net::shutdown_socket`: Windows `shutdown` doesn't wake a blocked `recv`, so it also
     calls `CancelIoEx`. Used by `NetStream::shutdown`, `HttpServer::stop` and the three
     `TcpServer::stop`s. Before this, stopping MLLP took minutes.
   - `net::bind_listener`: binds IPv4 first, like Java. On Windows `localhost` resolves to
     `::1` first. Used by the protocols, amqp and kafka-mqtt `TcpServer`s and `HttpServer`.
   - `camel_core::stack` (Linux and Windows stack bounds), shared by `camel-script` and a
     new PL/pgSQL `54001` stack check.
   - `.gitattributes` `eol=lf`, because Git for Windows defaults to `core.autocrlf=true`
     and that broke the byte-exact fixtures.
   - Unix-only tests are `cfg(unix)`: SSH agent sockets, `exec` with `echo`/`sh`, file
     modes. Windows variants were added where they make sense.

   - **Bundled IANA time-zone database (2026-11-04).** `camel-core/tzdata/zones.bin` has
     IANA 2026f, 598 zones in about 350 KB, compiled into `camel-core`. `tz.rs` uses it after
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
   `camel-tls` (TLS state machine, X.509 path validation, constant-time code), `camel-ssh`,
   `camel-xml` xmlsecurity, and the PGP and crypto data formats. Fuzz the parsers (HTTP/1,
   HTTP/2/HPACK, TLS records, BSON, CQL, AMQP frames, YAML, XML).
6. **Optional cleanup.** Run `cargo clippy --workspace`: `camel-script` has about 70 lints, and
   there are older lints elsewhere. Builds are already warning-free.
7. **Performance: three rounds against the 4x target (2026-11-05); 4x is met in three of six
   scenarios.** `scripts/bench.sh` (Linux or Git Bash; `scripts/bench.ps1` on Windows) builds and
   runs `crates/camel-bench` and `benchmarks/java-camel`, then writes `docs/BENCHMARKS.md`. The Java
   side uses Apache Camel 4.23.0 on Temurin 21.0.13 with Maven 3.9.16, pinned and checksum-verified.
   Throughput on a Ryzen 5 5600X under Windows 11, idle machine (rust ÷ java, first round → final):
   - direct 0.68x → **4.27x**, seda InOnly 1.09x → **4.18x**, seda InOut 0.77x → **8.37x**
   - cbr 0.56x → 3.48x, splitagg 0.64x → 3.18x, HTTP 2.02x → 2.45x
   - memory: 25–46x less peak RSS; startup: 77x faster (10 ms vs 751 ms)

   What changed (details in `benchmarks/analysis.md`): a std-only thread-caching allocator
   (`camel_core::alloc`, opt-in, installed by the `camel` CLI and the benchmark), lazy exchange
   and message ids, vector-backed headers (with shared names) and properties (interned Camel
   names), a copy-on-write `ObjectMap`, a TSC tick clock and striped counters for the statistics,
   Camel's `ManagementStatisticsLevel` with **`RoutesOnly` as rust-camel's default** (per-step
   timing is opt-in; Camel collects nothing without `camel-management`), a two-lock seda queue with
   spin-then-park and a reusable InOut reply slot, a borrowing producer cache, faster Simple
   comparisons, a JsonPath parse cache, in-place aggregation and moving split parts into an
   aggregate, lazy HTTP read timeouts. `camel-bench profile <scenario> <secs>` is a std-only
   sampling profiler for Windows; `camel-bench raw-http <port> threads|iocp8|poll` measures the HTTP
   ceiling. Tests: `crates/camel-core/tests/perf_semantics.rs`, `mgmt_stats.rs` (each statistics
   level), plus unit tests in the changed modules.

   Still under 4x (why, and next steps, in `benchmarks/analysis.md`):
   - HTTP: a do-nothing server (threads or IOCP) reaches only about 400k req/s with the shared load
     generator on this machine, 3.6x Java, so 4x cannot be shown here; rust-camel reaches about 70%
     of that. Needs the load generator on another machine. An event-driven camel-http server is
     not built (it measured no faster here); the decision is with the owner.
   - splitagg: JSON parsing and per-part exchange copies; next are shared JSON keys (an `ObjectMap`
     API change) and a smaller `Exchange` (public field changes).
   - cbr: the remaining cost is spread over many small steps.
   - Kafka scenario (optional) not done: it needs a real broker on both sides.

## Resuming in a new session

1. `git checkout main && cargo test --workspace`. Expect 2322 passing
   on Windows. Linux runs the Unix-only variants instead.
2. Pick an item above.
3. Follow the workflow in `CLAUDE.md`.
4. Update this file, and `docs/INTEROP.md` or `docs/FEATURES.md` where relevant, as part of
   the same change.
