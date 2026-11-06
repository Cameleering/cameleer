# Benchmarks: Apache Camel vs Cameleer

> Measured with the same routes on separate cloud machines, Cameleer starts in milliseconds
> instead of about a second, uses a fraction of Apache Camel's memory, and moves more messages
> per second on the same CPUs, in routing, REST APIs, Kafka, PostgreSQL and batch jobs.

This is the full report behind the
[Apache Camel vs Cameleer comparison](https://pmsudhi.github.io/cameleer/compare/apache-camel-vs-cameleer/).
It compares three runtimes on the same routes:

- **Cameleer** 0.4.0, the native Rust binary;
- **Apache Camel on the JVM**: Camel 4.23.0 on Java 21, the way most Camel runs today;
- **Apache Camel on Quarkus, native**: Camel 4.22.1 compiled ahead of time with Quarkus 3.40.0
  and Mandrel, the fastest-starting way to run Camel.

Every number below is generated from the raw results in
[`benchmarks/results/`](../benchmarks/results/) by `scripts/cloud-bench/summarize.py`; none is
typed by hand. Each figure is the mean of two runs on two identical servers.

## Results

<!-- bench:md-tables -->
### Throughput (higher is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) | Cameleer vs Camel JVM | Cameleer vs Camel native |
|---|---:|---:|---:|---:|---:|
| Direct call through a route | 2,230,208 msg/s | 663,701 msg/s | 243,522 msg/s | 3.3x | 9.1x |
| In-memory queue (seda), one-way | 1,644,186 msg/s | 603,426 msg/s | 316,442 msg/s | 2.7x | 5.1x |
| In-memory queue (seda), request-reply | 719,433 msg/s | 117,442 msg/s | 72,113 msg/s | 6.1x | 9.9x |
| Content-based router | 1,144,857 msg/s | 375,212 msg/s | 119,416 msg/s | 3.0x | 9.5x |
| Split and aggregate a JSON order | 70,926 orders/s | 26,622 orders/s | 9,274 orders/s | 2.6x | 7.6x |
| CSV batch (1,000-row files) | 998 files/s | 468 files/s | 163 files/s | 2.1x | 6.1x |
| Kafka stream processing | 6,081 records/s | 698 records/s | 694 records/s | 8.7x | 8.7x |
| HTTP, fixed reply | 99,012 req/s | 56,735 req/s | 28,584 req/s | 1.7x | 3.4x |
| REST API with JSON | 80,367 req/s | 37,814 req/s | 17,860 req/s | 2.1x | 4.4x |
| HTTP into Kafka | 17,854 req/s | 10,052 req/s | 9,267 req/s | 1.7x | 1.9x |
| HTTP into PostgreSQL | 11,007 req/s | 4,710 req/s | 3,540 req/s | 2.3x | 3.1x |

### Latency, p99 (lower is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| Direct call through a route | 1 µs | 3 µs | 10 µs |
| In-memory queue (seda), request-reply | 38 µs | 85 µs | 137 µs |
| Content-based router | 2 µs | 6 µs | 22 µs |
| Split and aggregate a JSON order | 30 µs | 70 µs | 382 µs |
| CSV batch (1,000-row files) | 1.7 ms | 3.2 ms | 12.5 ms |
| HTTP, fixed reply | 2.9 ms | 3.8 ms | 7.4 ms |
| REST API with JSON | 2.6 ms | 6.3 ms | 12.3 ms |
| HTTP into Kafka | 8.6 ms | 7.8 ms | 12.6 ms |
| HTTP into PostgreSQL | 10.6 ms | 19.8 ms | 27.7 ms |

### Memory and CPU

| scenario | Cameleer: peak memory | Apache Camel (JVM): peak memory | Apache Camel (Quarkus native): peak memory | Cameleer: CPU | Apache Camel (JVM): CPU | Apache Camel (Quarkus native): CPU |
|---|---:|---:|---:|---:|---:|---:|
| Direct call through a route | 7 MB | 399 MB | 113 MB | - | - | - |
| In-memory queue (seda), one-way | 9 MB | 440 MB | 199 MB | - | - | - |
| In-memory queue (seda), request-reply | 9 MB | 257 MB | 113 MB | - | - | - |
| Content-based router | 7 MB | 332 MB | 113 MB | - | - | - |
| Split and aggregate a JSON order | 8 MB | 299 MB | 114 MB | - | - | - |
| CSV batch (1,000-row files) | 8 MB | 337 MB | 126 MB | - | - | - |
| Kafka stream processing | 36 MB | 214 MB | 130 MB | - | - | - |
| HTTP, fixed reply | 18 MB | 244 MB | 121 MB | 318% | 315% | 306% |
| REST API with JSON | 19 MB | 402 MB | 129 MB | 318% | 313% | 302% |
| HTTP into Kafka | 67 MB | 328 MB | 129 MB | 108% | 148% | 168% |
| HTTP into PostgreSQL | 25 MB | 273 MB | 133 MB | 80% | 176% | 208% |

### Startup

| | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| time to a started context | 2.5 ms | 1106.1 ms | 30.4 ms |
| memory at startup | 8 MB | 114 MB | 69 MB |

<!-- /bench:md-tables -->

## What the results show

- **Startup.** Cameleer has its routes running in a few milliseconds. Apache Camel on the JVM takes
  about a second; compiled natively with Quarkus it takes tens of milliseconds.
- **Memory.** Cameleer runs these routes in tens of megabytes. Camel on the JVM uses hundreds;
  native Camel sits in between.
- **Throughput.** Cameleer handles more messages per second than Camel on the JVM in every
  scenario, and more than native Camel by a wider margin: native Camel has no JIT compiler, so
  it trades throughput for startup.
- **Latency.** Cameleer's p99 latency is lower in every scenario but one. In HTTP into Kafka it
  is slightly higher than Camel on the JVM (8.6 ms against 7.8 ms) while serving 1.7 times the
  requests: Cameleer batches the sends of concurrent requests, which raises the slowest
  requests' wait a little.

## The scenarios

The routes are defined once per runtime from one specification:
[`crates/cameleer-bench/src/scenarios.rs`](../crates/cameleer-bench/src/scenarios.rs) and
[`realworld.rs`](../crates/cameleer-bench/src/realworld.rs) for Cameleer,
[`Bench.java`](../benchmarks/java-camel/src/main/java/bench/Bench.java) for Apache Camel (the
Quarkus build compiles the same class from
[`benchmarks/camel-quarkus`](../benchmarks/camel-quarkus/)).

| scenario | where it runs | what the route does |
|---|---|---|
| Direct call through a route | in process | `direct:` route that sets a header and counts the message |
| In-memory queue, one-way | in process | 4 producers into `seda:` with 4 concurrent consumers |
| In-memory queue, request-reply | in process | the same queue with a reply for every message |
| Content-based router | in process | `choice` on a header, `filter` on an amount, `transform` with Simple |
| Split and aggregate a JSON order | in process | `split` an order's 10 items by JSONPath and `aggregate` them back |
| CSV batch | in process | `split` a 1,000-row file into lines, keep the EU rows, reshape, aggregate batches of 100 |
| HTTP, fixed reply | over the network | `netty-http` consumer that answers with a constant JSON document |
| REST API with JSON | over the network | read `orderId` and `customer` with JSONPath, answer with a JSON confirmation |
| HTTP into Kafka | over the network | publish each order to Kafka keyed by order id, then answer |
| HTTP into PostgreSQL | over the network | insert each order with the `sql` component through a pool of 16 connections |
| Kafka stream processing | Kafka on the load machine | consume orders with 4 consumers, filter by amount, reshape, publish to another topic |
| Startup | in process | start a context with every in-process route and an HTTP route |

## How we tested

**Machines.** Three DigitalOcean CPU-optimized droplets (`c-4`) in region `blr1`: 4 dedicated
vCPUs (Intel Xeon Platinum 8168 at 2.7 GHz), 8 GB RAM, Ubuntu 24.04, on one private network. Two
of them are servers that take turns running each runtime; the third runs the HTTP load
generator, Apache Kafka 4.3.2 (KRaft, one broker, 4 partitions per topic) and PostgreSQL 16. All
three get the same kernel settings
([`scripts/cloud-bench/setup-node.sh`](../scripts/cloud-bench/setup-node.sh)).

**Runtimes.** Temurin 21.0.13 for Apache Camel 4.23.0, with no JVM flags. Camel Quarkus 3.40.0
(Apache Camel 4.22.1) compiled with Mandrel 25.0.4.1. Cameleer 0.4.0 built with Rust 1.99.1 in
release mode. Every download is pinned and checksum-verified.

**Method.**
- Each scenario runs in a fresh process: 15 s of warmup, then five timed 5-second iterations.
- The two servers swap roles halfway, so every runtime runs on both machines and each figure is
  the mean of the two. Only one runtime runs at a time.
- The HTTP scenarios use 64 concurrent keep-alive connections from the load machine; the load
  generator ([`loadgen.rs`](../crates/cameleer-bench/src/loadgen.rs)) is the same for every
  runtime and sends a different order id with every request.
- Default settings everywhere: no GC or thread-pool tuning on either side, and the same
  PostgreSQL pool size (16; HikariCP on the JVM, Agroal on Quarkus).
- Peak memory and CPU of each server process come from `/usr/bin/time`; startup is the time from
  spawning the process to its `READY` line, median of five.

**Reproduce it.** A run costs about one US dollar of cloud time:

```sh
export DO_TOKEN_FILE=~/do-token.txt     # a DigitalOcean API token
scripts/cloud-bench/droplets.sh up      # three c-4 droplets
scripts/cloud-bench/run.sh all          # build, fill Kafka, run every scenario
scripts/cloud-bench/droplets.sh down    # delete the droplets
```

See [`scripts/cloud-bench/README.md`](../scripts/cloud-bench/README.md).

## Results that need context

- **Kafka stream processing** waits for every record's acknowledgement (`acks=all`) before the
  next one, on every runtime: that is the default behaviour of both Camel's and Cameleer's Kafka
  producer. The ratio is fair; the absolute numbers are far below what Kafka can do with
  batching or asynchronous sends.
- **HTTP** is measured with one load machine of the same size as the server. It drives every
  runtime the same way, but the absolute requests per second are not a ceiling.
- **Apache Camel on Quarkus** runs Camel 4.22.1, one minor version behind the JVM side, because
  that is what the latest Camel Quarkus release ships.

## Bugs this benchmark found and fixed

Running real workloads on separate machines found three problems in Cameleer, fixed before the
numbers above were taken. Each fix has a regression test.

- **HTTP keep-alive.** The HTTP server had 32 worker threads and closed keep-alive connections
  when all were busy, so more than 32 concurrent clients reconnected on every request. The
  default is now 200 workers, started on demand.
- **Kafka producer.** Concurrent sends waited in line one record at a time. They are now batched
  into one produce request per broker, as Kafka's own producer does.
- **PostgreSQL pool.** `poolSize` only limited idle connections, so a burst opened extra
  connections. It is now a hard maximum, like HikariCP's, with callers waiting up to
  `poolTimeout`.

## Action layer and MCP (2026-11-06, second run)

Same three droplets and plan (64 HTTP connections, 15 s warmup, 5 × 5 s, servers swap roles),
with three new sides: Apache Camel 4.23.0's own `camel-mcp-server` on the JVM, a hand-written
MCP server on the official TypeScript SDK 1.32.2 (Node.js 22.23.4, written as the SDK's
documentation shows), and Cameleer's `main` branch as a baseline for regressions. Raw records:
[benchmarks/results/2026-11-06-linux-agents](../benchmarks/results/2026-11-06-linux-agents/).

### MCP tool calls over the network

One tool, `lookup_order`, answers a JSON document; every side exposes it the same way. The
session flow is what Claude Desktop and agent frameworks do: `initialize`, then `tools/call`
with `Mcp-Session-Id`.

| scenario | Cameleer | Apache Camel `camel-mcp-server` (JVM) | MCP TypeScript SDK (Node.js) |
|---|---:|---:|---:|
| `tools/call` in a session (2025-06-18), throughput | **41,620 calls/s** | 22,458 calls/s | 1,536 calls/s |
| p99 latency | 5.9 ms | 5.2 ms | 79 ms |
| peak memory | **53 MB** | 317 MB | 214 MB |
| stateless `tools/call` (2026-07-28), throughput | **51,700 calls/s** | not offered | not offered |
| stateless, p99 / peak memory | 5.7 ms / 26 MB | | |

Cameleer serves 1.85x Camel's MCP throughput in a sixth of the memory, and 27x the TypeScript
SDK. Every Cameleer call goes through the whole tool envelope (argument schema check, rate-limit
and breaker bookkeeping, a hash-chained audit record); Camel checks required arguments only and
the SDK server checks the schema.

### The tool envelope

| scenario (in process) | throughput | p99 |
|---|---:|---:|
| a tool call through the envelope (`AiRegistry::call`) | 151,446 calls/s | 7.7 µs |
| the same route called with `direct:` | 1,343,450 calls/s | 0.8 µs |

So the envelope costs about 6 µs per call on this Xeon (3 µs on a Ryzen 5 5600X, which has SHA
instructions). The first build measured 59 µs here: it started a thread per call to enforce the
timeout and built every audit record as a JSON object. Tools whose calls finish well within their
timeout now run on the caller's thread with a deadline on the exchange (the route still stops at
its next step once the timeout passes); others run on pooled workers.

### Integration routes against `main`

No scenario got slower than `main`. Two got faster, from the Kafka producer's requests in flight
on several connections (`maxInFlightRequest`, default 5):

| scenario | this branch | `main` | Apache Camel (JVM) |
|---|---:|---:|---:|
| HTTP into Kafka | **48,800 req/s**, p99 2.4 ms | 21,658 req/s, p99 7.2 ms | 9,800 req/s, p99 7.9 ms |
| Kafka stream processing | **17,881 records/s** | 10,072 records/s | 620 records/s |

HTTP into Kafka went from 1.7x Camel's throughput with a worse p99 to 5.0x with a third of its
p99. The other scenarios are within noise of the first run (direct 3.2x, seda request-reply 5.3x,
content-based router 3.1x, split and aggregate 2.7x, CSV 2.3x, REST with JSON 2.2x, HTTP into
PostgreSQL 2.3x Camel on the JVM).

### Size and cold start against `main`

`cameleer` CLI, `dist` builds, stripped, on the same droplet (cold start: spawn to `Routes
startup` of one `platform-http` route, median of 80 runs alternating with `main`):

| build | `main` | this branch |
|---|---:|---:|
| every component (`full`) | 24.26 MB, ready in 2.00 ms, 10.0 MB RSS | 25.78 MB (+1.5 MB for the MCP server and client, `gemini`, the agent step and the call options), 2.12 ms, 11.0 MB |
| `core` | 5.87 MB, ready in 1.33 ms, 6.1 MB RSS | 5.93 MB (+63 KB), 1.33 ms, 6.1 MB |

The AI core and the agent step are `cameleer-core`'s `agent` feature, so `core` builds leave them
out (they were +165 KB before that change).

### The HTTP ceiling

A do-nothing HTTP server without Camel (`cameleer-bench raw-http`, one thread per connection, one
`recv` and one `send` per request) reaches 166,374 req/s on these machines with the same load
generator; Cameleer's fixed-reply route reaches 112,910 req/s (68% of that ceiling), Camel 67,682.
The difference is spread over the request path (allocation, header parsing, the exchange) and
kernel scheduling of 64 threads; an event-driven server core is the next step for this scenario.

## FaaS cold start and size

Startup and size of the `cameleer` command itself, measured with `cameleer-bench coldstart`
(spawn to the `Routes startup` log line, 40 runs, medians) on Linux x86-64 (WSL2 Ubuntu 24.04 on
a Ryzen 5 5600X):

| build (Linux x86-64) | binary | HTTP route ready | run-once job done | memory, HTTP route |
|---|---:|---:|---:|---:|
| all components, glibc | 23.9 MB | 1.17 ms | 2.15 ms | 10.4 MB |
| `--features cloud`, static musl | 11.8 MB | 0.75 ms | 1.48 ms | 5.3 MB |
| `--features core`, static musl | 7.5 MB | 0.69 ms | 1.41 ms | 4.2 MB |

Container images (linux/amd64, distroless, compressed): about 20 MB with every component, 14.5 MB
with `FEATURES=cloud`, 12.5 MB with `FEATURES=core`. In AWS's `provided:al2023` image with the
Lambda Runtime Interface Emulator, init took 3.5–5.2 ms and warm invocations 0.7–1.3 ms. The
detailed measurements, the Windows numbers and the tuning history are in
[`benchmarks/local.md`](../benchmarks/local.md) and
[`benchmarks/analysis.md`](../benchmarks/analysis.md); the build options are in
[build features](guides/build-features.md).

## Found a problem?

If a number here doesn't match what you measure, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) with your setup and results, or
fork the repository, fix the harness, and open a pull request.
