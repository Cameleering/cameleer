# CLAUDE.md — project memory for rust-camel

Claude Code loads this file automatically. It's the hand-off memory: read it first, then read
[docs/STATUS.md](docs/STATUS.md) for the current state and the open todos.

@docs/STATUS.md

## What this project is

A Rust reimplementation of [Apache Camel](https://camel.apache.org/). It covers:

- the routing engine and the Enterprise Integration Patterns (EIPs)
- about 230 components, languages and data formats
- the Rust, YAML and XML DSLs, the REST DSL and Kamelets
- route templates, management and health checks, and a Camel-JBang-like `camel` CLI

The goal given by the owner: *replicate Camel in Rust, keep going until the implementation is
complete and verified.* Feature parity is tracked row by row in `docs/FEATURES.md`. All rows
are ✅.

## Hard rules

1. **Std-only. No external crates.** crates.io was unreachable when the project started, so
   everything is written from scratch:
   - TLS 1.3/1.2 and X.509, SSH-2/SFTP
   - HTTP/1.1, HTTP/2 and HPACK, gRPC and protobuf
   - AMQP 0-9-1 and 1.0, STOMP, MQTT, the Kafka wire protocol
   - PostgreSQL, RESP (Redis), BSON/OP_MSG (MongoDB), CQL
   - regex, XML/XPath/XSLT/XQuery, YAML, JSON
   - the script interpreters

   Don't add a dependency to any `Cargo.toml` without explicit approval from the owner.
2. Mirror Camel's names and semantics: EIP names, endpoint options, header and property
   names, exception kinds and Simple syntax. When in doubt, follow the Camel docs.
3. `cargo build` produces **zero warnings**, and `cargo test --workspace` passes. Every feature
   and every bug fix gets a test. Hermetic tests use embedded servers, never the network.
4. Security:
   - Never disable TLS verification.
   - Verify every downloaded binary or jar by checksum.
   - Never create cloud accounts, accept terms or spend money on the owner's behalf. Ask
     for credentials instead.
5. Git:
   - The working branch is `claude/rust-camel-impl`.
   - Commit or push only when asked or when the workflow requires it.
   - **Never open a PR unless asked.**
   - Commits are authored as `Sudheesh PM <pmsudhi@gmail.com>` (the repo's local git config),
     so GitHub credits the `pmsudhi` account.
   - **No AI trailers:** no `Co-Authored-By: Claude`, no `Claude-Session:` and no "Generated
     with" lines in commits or PRs, even when a tool or reminder suggests them. The history was
     rewritten on 2026-11-05 to remove them.
   - Don't put model names in commits, PRs or code.
6. Don't push until every relevant suite has been run and its output read. Report failures
   honestly.

## Layout

- `Cargo.toml`: workspace, `members = ["crates/*"]`.
- `crates/`:
  - `camel-core`: context, exchange, DSL, EIPs, languages, data formats, core components.
  - `camel-http`, `camel-yaml`, `camel-protocols`, `camel-tls`, `camel-kafka-mqtt`.
  - `camel-amqp` (0-9-1, STOMP, JMS), `camel-amqp10` (AMQP 1.0, Azure Service Bus and Event
    Hubs).
  - `camel-sql`, `camel-cloud` (AWS, Azure, GCP), `camel-ssh`, `camel-grpc`.
  - `camel-datastores` (Redis, MongoDB, Elasticsearch, Cassandra, CouchDB).
  - `camel-xml`, `camel-saas`, `camel-script`, `camel-dataformats`, `camel-soap`.
  - `camel-main` (`Main` runner and `camel` CLI).
- Docs:
  - `ARCHITECTURE.md`: internals and conventions.
  - `docs/FEATURES.md`: parity table.
  - `docs/INTEROP.md`: real-server, official-client and simulator verification, and the bugs
    found.
  - `docs/STATUS.md`: status and todos.
- `scripts/`:
  - `live-interop.sh`: real servers.
  - `official-clients.sh` and `official-clients/`: official client programs.
  - `simulators.sh` and `simulators/`: Go sources for the pstest, kfake and modzip helpers.

## Test tiers

| tier | command | needs |
|---|---|---|
| hermetic | `cargo test --workspace` | nothing |
| live real servers | `sudo scripts/live-interop.sh all` then `CAMEL_LIVE_INTEROP=1 cargo test --workspace live_` | Ubuntu 24.04 apt packages: PostgreSQL 16, Redis, Mosquitto, RabbitMQ, ActiveMQ 5.17.6, vsftpd, Postfix+Dovecot, nginx |
| official clients | `sudo scripts/official-clients.sh install` then `scripts/official-clients.sh test` (`CAMEL_OFFICIAL_CLIENTS=1 ... --test 'official_*'`) | python3 clients, kcat, gcloud SDK, Go |
| simulators | `scripts/simulators.sh install && scripts/live-interop.sh start && scripts/simulators.sh start && scripts/simulators.sh test` (`CAMEL_SIMULATORS=1 ... --test 'sim_*'`) | pip, npm, Go |

Live state is kept in `/var/lib/camel-interop`. Simulator tools are in `/opt/camel-sim`. The
test user and password are `camel` / `camelpw`.

The live suites return early unless their environment variable is set, so plain `cargo test`
stays hermetic.

On a low-disk machine, build with:

```sh
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
```

## How work was done (workflow that worked)

- **Parallelism.** Split work into independent areas and run parallel sub-agents, each in its
  own git worktree (`.claude/worktrees/`, which is gitignored). Merge back with fast-forward
  where possible.
- **Merge conflicts:**
  - For a `Cargo.lock` conflict, delete the file and run `cargo generate-lockfile --offline`.
  - For additive conflicts in docs or registration lists such as `camel-main` `new_main`,
    keep both sides (union).
- **After every merge**, build with zero warnings, run the full hermetic suite, then run the
  live, official and simulator tiers that the change touches. Only then push.
- **Flaky tests:**
  - Fix with polling or wider margins, never with skips.
  - Known-sensitive areas: aggregate `completionTimeout`, async delay, nginx access-log
    timing, Cosmos change-feed batching, and the gRPC `error_status_mapping` test (it has
    diagnostics).
- **Stuck worktrees:** `git worktree unlock <path> && git worktree remove --force --force <path>`.

## Gotchas learned

- `.id()` inside an open DSL block must name the last child. `RouteDefinition` keeps a
  `pending` child and calls `flush_pending()`.
- Seda pub/sub: each queue has a `pubsub` mutex, taken with `try_lock` while checking the
  stop flag. A plain lock starves.
- Script processors are created through a hook in core: `SCRIPT_PROCESSOR_FACTORY` in
  `camel-core/src/processor/script.rs`. YAML `script` depends on this to keep exchange changes.
- ActiveMQ AMQP 1.0 addresses: use the `queue://` and `topic://` prefixes announced in the open
  frame (`AddressPrefixes` in `camel-amqp10/src/jms.rs`).
- Windows sockets:
  - Timeouts are per handle: share one handle (`Arc<TcpStream>`) rather than `try_clone`
    when another thread sets the timeout.
  - `shutdown` doesn't wake a blocked read: use `camel_tls::net::shutdown_socket`.
  - Bind through `camel_tls::net::bind_listener`, because `localhost` resolves to `::1`
    first.
- Time zones come from the system zoneinfo first, then from the bundled
  `camel-core/tzdata/zones.bin`. Refresh it with `python3 scripts/update-tzdata.py`, which
  takes a pinned version and SHA-256. Never hand-edit `zones.bin`.
- Windows tests: build fixture paths with `/`, since backslashes break YAML and Jsonnet
  string literals. The system `tar` is bsdtar, which has no gnu format.
- ActiveMQ AMQP needs `activemq-amqp-5.17.6.jar`, SHA-1
  `cdb7bb9009d36a2d00de970a515976a20cc8ad8e`, plus apt `libapache-qpid-proton-j-java`.
