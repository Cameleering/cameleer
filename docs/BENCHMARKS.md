# Benchmarks: Apache Camel vs Cameleer

> Measured with the same routes on separate cloud machines, Cameleer starts in milliseconds
> instead of about a second, uses a fraction of Apache Camel's memory, and moves more messages
> per second on the same CPUs, in routing, REST APIs, Kafka, PostgreSQL and batch jobs.

This is the full report behind the
[Apache Camel vs Cameleer comparison](https://cameleer.ing/cameleer/compare/apache-camel-vs-cameleer/).
It compares three runtimes on the same routes:

- **Cameleer** `main` at `beb3385` (2026-11-23: 0.8.0's candidate plus the governor, the asynchronous Kafka
  producer and the keep-alive fix), the native Rust binary;
- **Apache Camel on the JVM**: Camel 4.23.0 on Java 21, the way most Camel runs today;
- **Apache Camel on Quarkus, native**: Camel 4.22.1 compiled ahead of time with Quarkus 3.40.0
  and Mandrel, the fastest-starting way to run Camel.

Every number below is generated from the raw results in
[`benchmarks/results/`](../benchmarks/results/) by `scripts/cloud-bench/summarize.py`; none is
typed by hand. Each figure is the mean of two runs on two servers, all from **one session on
2026-11-23** (Cameleer, both Camel builds and the MCP TypeScript SDK ran on the same droplets, in one go;
raw records in [`benchmarks/results/2026-11-23-linux-main-beb3385-camel/`](../benchmarks/results/2026-11-23-linux-main-beb3385-camel/)).
It replaces the 0.8.0 candidate's session of 2026-11-20, which replaced Cameleer 0.4.0 (2026-11-06), so this page no longer mixes
Cameleer releases or sessions in its main table. Absolute figures differ by 10 to 25% between the two servers of
one session and by about 40% between sessions and regions, so compare ratios inside a session.

## Results

<!-- bench:md-tables -->
### Throughput (higher is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) | Cameleer vs Camel JVM | Cameleer vs Camel native |
|---|---:|---:|---:|---:|---:|
| Direct call through a route | 2,419,660 msg/s | 761,660 msg/s | 297,438 msg/s | 3.1x | 8.1x |
| In-memory queue (seda), one-way | 1,786,960 msg/s | 662,535 msg/s | 386,344 msg/s | 2.6x | 4.6x |
| In-memory queue (seda), request-reply | 775,338 msg/s | 137,808 msg/s | 91,450 msg/s | 5.6x | 8.4x |
| Content-based router | 1,135,904 msg/s | 433,457 msg/s | 158,872 msg/s | 2.6x | 7.1x |
| Split and aggregate a JSON order | 84,354 orders/s | 30,885 orders/s | 11,684 orders/s | 2.7x | 7.2x |
| CSV batch (1,000-row files) | 1,264 files/s | 560 files/s | 237 files/s | 2.2x | 5.3x |
| Kafka stream processing | 14,036 records/s | 706 records/s | 698 records/s | 19.8x | 20.1x |
| HTTP, fixed reply | 146,114 req/s | 70,737 req/s | 35,360 req/s | 2.0x | 4.1x |
| REST API with JSON | 141,276 req/s | 45,700 req/s | 22,604 req/s | 3.0x | 6.2x |
| HTTP into Kafka | 44,402 req/s | 10,182 req/s | 9,550 req/s | 4.3x | 4.6x |
| HTTP into PostgreSQL | 17,386 req/s | 8,088 req/s | 5,276 req/s | 2.1x | 3.2x |

### Latency, p99 (lower is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| Direct call through a route | 0 µs | 2 µs | 7 µs |
| In-memory queue (seda), request-reply | 28 µs | 68 µs | 101 µs |
| Content-based router | 1 µs | 4 µs | 15 µs |
| Split and aggregate a JSON order | 21 µs | 55 µs | 180 µs |
| CSV batch (1,000-row files) | 868 µs | 2.9 ms | 9.5 ms |
| HTTP, fixed reply | 606 µs | 3.6 ms | 6.6 ms |
| REST API with JSON | 836 µs | 5.0 ms | 10.2 ms |
| HTTP into Kafka | 2.5 ms | 7.6 ms | 11.8 ms |
| HTTP into PostgreSQL | 8.7 ms | 11.0 ms | 20.1 ms |

### Memory and CPU

| scenario | Cameleer: peak memory | Apache Camel (JVM): peak memory | Apache Camel (Quarkus native): peak memory | Cameleer: CPU | Apache Camel (JVM): CPU | Apache Camel (Quarkus native): CPU |
|---|---:|---:|---:|---:|---:|---:|
| Direct call through a route | 8 MB | 334 MB | 112 MB | - | - | - |
| In-memory queue (seda), one-way | 10 MB | 419 MB | 205 MB | - | - | - |
| In-memory queue (seda), request-reply | 9 MB | 240 MB | 113 MB | - | - | - |
| Content-based router | 8 MB | 302 MB | 113 MB | - | - | - |
| Split and aggregate a JSON order | 8 MB | 326 MB | 113 MB | - | - | - |
| CSV batch (1,000-row files) | 8 MB | 329 MB | 133 MB | - | - | - |
| Kafka stream processing | 39 MB | 211 MB | 129 MB | - | - | - |
| HTTP, fixed reply | 9 MB | 267 MB | 120 MB | 216% | 315% | 304% |
| REST API with JSON | 9 MB | 421 MB | 133 MB | 236% | 310% | 303% |
| HTTP into Kafka | 29 MB | 333 MB | 129 MB | 202% | 134% | 142% |
| HTTP into PostgreSQL | 29 MB | 289 MB | 142 MB | 132% | 232% | 271% |

### Startup

| | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| time to a started context | 3.0 ms | 1034.2 ms | 23.8 ms |
| memory at startup | 9 MB | 116 MB | 69 MB |

<!-- /bench:md-tables -->

## What the results show

- **Startup.** Cameleer has its routes running in about 3 ms in this scenario (every in-process route plus an
  HTTP route). The 0.8.0 candidate measured 12 ms here; the cause was the descriptor-table growth at server start,
  fixed on 2026-11-22 (`docs/design/startup-regression-2026-11.md`). Apache Camel on the JVM takes about a second;
  compiled natively with Quarkus it takes tens of milliseconds.
- **Memory.** Cameleer runs these routes in tens of megabytes. Camel on the JVM uses hundreds;
  native Camel sits in between.
- **Throughput.** Cameleer handles more messages per second than Camel on the JVM in every
  scenario, and more than native Camel by a wider margin: native Camel has no JIT compiler, so
  it trades throughput for startup.
- **Latency.** Cameleer's p99 latency is lower than both Camel builds in every scenario of this session,
  including HTTP into Kafka (2.5 ms against 7.6 ms on the JVM). The 0.4.0 run had one loss there (8.6 against
  7.8 ms); the Kafka producer change of 2026-11-06 removed it. The one place where Cameleer uses more CPU than the
  JVM is HTTP into Kafka (202% against 134%), because it sends on several connections at once.
- **Against the 2026-11-20 session.** Most ratios rose (HTTP 1.8x to 2.0x, Kafka stream 15.2x to 19.8x, HTTP into
  Kafka 3.2x to 4.3x, MCP session 2.06x to 2.73x). Three in-process ratios fell: seda request-reply 6.8x to 5.6x,
  the content-based router 3.0x to 2.6x and CSV 2.3x to 2.2x. Cameleer's own figures rose in every in-process
  scenario but the router; Camel on the JVM gained more on these servers (Xeon 8358 and 8280 against 8168 before).
- **Against the 2026-11-06 session (Cameleer 0.4.0), the figures published until 0.7.0.** Four ratios are lower:
  seda request-reply 5.6x (6.8x then), the content-based router 2.6x (3.0x), CSV 2.2x (2.3x) and HTTP into Kafka 4.3x
  (5.0x, measured on other machines). The 2026-11-23 droplets in `sfo2` are faster, and Camel on the JVM gained more
  from them than Cameleer did; the table above is the current comparison and 0.8.0 is published with it, not with the
  older, higher ratios.
- **Over an hour.** One-hour soaks at constant load
  ([`benchmarks/results/2026-11-23-linux-soak/`](../benchmarks/results/2026-11-23-linux-soak/)): HTTP into Kafka
  49,530 req/s (4.6x Camel on the JVM) and the MCP session 62,967 calls/s (2.5x), with flat throughput and p99,
  constant threads and descriptors, and 0.6 to 1.3 MB of memory growth. Camel on the JVM grew 17 to 71 MB and
  the TypeScript SDK 89 MB in the same hour.

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

**Machines.** Three DigitalOcean CPU-optimized droplets (`c-4`) in region `sfo2` (this session; earlier rounds
used `blr1`, then `sfo3`, which no longer offer `c-4`): 4 dedicated vCPUs (Intel Xeon Platinum 8358 at 2.6 GHz on
server a and the load machine, Platinum 8280 at 2.7 GHz on server b; the 2026-11-20 session had 8168s), 8 GB RAM, Ubuntu 24.04, on one private network. Two
of them are servers that take turns running each runtime; the third runs the HTTP load
generator, Apache Kafka 4.3.2 (KRaft, one broker, 4 partitions per topic) and PostgreSQL 16. All
three get the same kernel settings
([`scripts/cloud-bench/setup-node.sh`](../scripts/cloud-bench/setup-node.sh)).

**Runtimes.** Temurin 21.0.13 for Apache Camel 4.23.0, with no JVM flags. Camel Quarkus 3.40.0
(Apache Camel 4.22.1) compiled with Mandrel 25.0.4.1. Cameleer `main` at `beb3385` built with Rust
1.99.1 in release mode. Every download is pinned and checksum-verified.

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
export DO_REGION=sfo2                   # a region that offers c-4 (2026-11-20: sfo2, ams3, lon1)
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

The MCP rows below come from a rerun on 2026-11-12 (Cameleer 0.5.1 at `306a5df`, the same droplet
type and plan, Apache Camel, Cameleer and the TypeScript SDK in one session, the servers swapping
roles halfway; each figure is the mean of the two phases). Raw records:
[benchmarks/results/2026-11-12-linux-mcp](../benchmarks/results/2026-11-12-linux-mcp/). The first
run's figures, and why the TypeScript number changed, are under the table.

### MCP tool calls over the network

**On `main` (2026-11-23, `beb3385`, the session of the main table):** `tools/call` in a session, mean of the two
servers: Cameleer **55,313 calls/s**, p99 4.0 ms, 23 MB; Apache Camel `camel-mcp-server` 20,254 calls/s, 5.8 ms,
295 MB; the MCP TypeScript SDK 2,684 calls/s, 47.4 ms, 202 MB. That is **2.73x Camel and 20.6x the SDK**, with a
lower p99 than Camel. Stateless `tools/call` on Cameleer: 52,435 calls/s. Records:
[`benchmarks/results/2026-11-23-linux-main-beb3385-camel/`](../benchmarks/results/2026-11-23-linux-main-beb3385-camel/).

**On 0.8.0 (2026-11-20, one session, Cameleer, Camel and the TypeScript SDK on the same droplets in `sfo2`):**
`tools/call` in a session, mean of the two servers: Cameleer **43,254 calls/s**, p99 4.36 ms, 22 MB; Apache Camel
`camel-mcp-server` 20,968 calls/s, 5.57 ms, 281 MB; the MCP TypeScript SDK (zod 4) 2,179 calls/s, 56.6 ms, 198 MB.
That is **2.06x Camel and 19.9x the SDK**, and Cameleer's p99 is now lower than Camel's. Stateless `tools/call` on
Cameleer: 41,532 calls/s, p99 4.62 ms. Records:
[`benchmarks/results/2026-11-20-linux-0.8.0-camel/`](../benchmarks/results/2026-11-20-linux-0.8.0-camel/). The table
below is the earlier run, on Cameleer 0.5.1 on `blr1` droplets (2026-11-12); the two sessions' figures are not
comparable in absolute terms. Against 0.7.0 on the same droplets, Cameleer's MCP is 0.96x to 0.98x (see "0.8.0
candidate against 0.7.0" below).

One tool, `lookup_order`, answers a JSON document; every side exposes it the same way. The
session flow is what Claude Desktop and agent frameworks do: `initialize`, then `tools/call`
with `Mcp-Session-Id`.

| scenario | Cameleer | Apache Camel `camel-mcp-server` (JVM) | MCP TypeScript SDK (Node.js) |
|---|---:|---:|---:|
| `tools/call` in a session (2025-06-18), throughput | **39,922 calls/s** | 22,604 calls/s | 2,257 calls/s |
| p99 latency | 6.3 ms | 5.2 ms | 55 ms |
| peak memory | **56 MB** | 289 MB | 197 MB |
| stateless `tools/call` (2026-07-28), throughput | **50,514 calls/s** | not offered | not offered |
| stateless, p99 / peak memory | 6.0 ms / 26 MB | | |

Cameleer serves 1.77x Camel's MCP throughput in a fifth of the memory, and 17.7x the TypeScript
SDK; Camel's p99 is lower (5.2 ms against 6.3 ms). Every Cameleer call goes through the whole tool envelope (argument schema check, rate-limit
and breaker bookkeeping, a hash-chained audit record); Camel checks required arguments only and
the SDK server checks the schema.

### The first run, and why the TypeScript number changed

The first run (2026-11-06) measured Cameleer at 41,620 calls/s, p99 5.9 ms and 53 MB, Camel at
22,458 calls/s, 5.2 ms and 317 MB, the TypeScript SDK at 1,536 calls/s, 79 ms and 214 MB, and
stateless calls at 51,700 calls/s, 5.7 ms and 26 MB. Cameleer and Camel moved by 4% and 1%, which
is within the difference between the two machines of one run (Cameleer ran 41,210 and 38,634
calls/s on them).

The TypeScript number moved by 47%, and that is not noise. The baseline server pinned `zod`
3.25.76, and the SDK's schema check is faster with zod 4. Run on one droplet, alternating three
rounds each, with the other settings the same:

| | zod 3.25.76 | zod 4.6.7 |
|---|---:|---:|
| throughput, three rounds | 1,557 / 1,572 / 1,636 calls/s | 2,537 / 2,578 / 2,598 calls/s |
| mean | 1,588 calls/s | 2,571 calls/s (1.62x) |
| p99 | 73 to 74 ms | 47 to 49 ms |
| peak memory | about 212 MB | about 197 MB |

The zod 3 build reproduces the first run's 1,536 calls/s, so the machines were not the difference.
The baseline now uses zod 4 (`scripts/cloud-bench/mcp-ts`), which is what the SDK's current
documentation installs, and the first run's 27x became 17.7x. Raw records:
[zod-ab](../benchmarks/results/2026-11-12-linux-mcp/zod-ab/).

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

### Two fixes after 0.8.0: the governor and the asynchronous Kafka producer (2026-11-23)

Three `c-4` droplets in `sfo2` (new ones), servers swapped, 64 connections, 15 s warm-up, 5 x 5 s, `main` at `7952563`
against the same plus the two fixes (PRs 74 and 75) against `v0.6.0`. Raw data, method and tables:
`benchmarks/results/2026-11-23-linux-fixes-final/`. These droplets and this session differ from the 0.8.0 round above,
and no Apache Camel side was run, so the figures are comparable with each other and not with the ratios above.

| scenario | with the fixes | `main` | `v0.6.0` |
|---|---:|---:|---:|
| HTTP into Kafka, req/s | **35,472** | 32,527 | 40,744 |
| HTTP into Kafka, p99 | 3.09 ms | 3.40 ms | 2.71 ms |
| HTTP into Kafka, peak memory | **31.5 MB** | 60.8 MB | 76.9 MB |
| HTTP into Kafka, CPU per 1000 requests | 0.060 s | 0.072 s | 0.071 s |
| HTTP into PostgreSQL, req/s | 9,654 | 9,665 | 10,536 |
| `http`, `rest-json`, gRPC unary, `mcp-session` | within noise of `main` | | |
| `mcp` (stateless) | 43,330 | 43,385 | 45,392 |

- No throughput dips in 50 runs; on `main` 6 of 21 earlier runs fell to 7,000 to 15,000 req/s (a route that blocks briefly was
  moved to the event-loop thread, fixed by PR 74).
- HTTP into Kafka is 1.09x `main` at 17% less CPU per request and half the memory, and **still 0.87x of `v0.6.0`**; stateless
  MCP is 0.95x of `v0.6.0` and HTTP into PostgreSQL 0.92x. These gaps are open.
- One run of HTTP into PostgreSQL sat at about 7,700 req/s on both the fixed build and `main`; the cause is not known.
- Not measured: Apache Camel in this session, the other producers (SQL, models, MCP client), which stay synchronous, and the
  sender threads under long broker failure.

### Integration routes against `main`

*(Cameleer 0.5.x, 2026-11-06. On `main` on 2026-11-23 in `sfo2` the same two scenarios measured 44,402 req/s with
p99 2.5 ms for HTTP into Kafka (4.3x Camel) and 14,036 records/s for Kafka stream processing (19.8x Camel); on 0.8.0
on 2026-11-20, 31,351 req/s (3.2x) and 10,104 records/s (15.2x). The machines and the sessions differ; the ratio
below 5.0x for HTTP into Kafka was not investigated further.)*

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

*(Cameleer 0.5.x. For 0.8.0 see "Size, cold start and memory of 0.8.0" below.)*

`cameleer` CLI, `dist` builds, stripped, on the same droplet (cold start: spawn to `Routes
startup` of one `platform-http` route, median of 80 runs alternating with `main`):

| build | `main` | this branch |
|---|---:|---:|
| every component (`full`) | 24.26 MB, ready in 2.00 ms, 10.0 MB RSS | 25.78 MB (+1.5 MB for the MCP server and client, `gemini`, the agent step and the call options), 2.12 ms, 11.0 MB |
| `core` | 5.87 MB, ready in 1.33 ms, 6.1 MB RSS | 5.93 MB (+63 KB), 1.33 ms, 6.1 MB |

The AI core and the agent step are `cameleer-core`'s `agent` feature, so `core` builds leave them
out (they were +165 KB before that change).

### The HTTP ceiling

*(Measured on 2026-11-06, before the HTTP server moved onto simoom.)* A do-nothing HTTP server without Camel
(`cameleer-bench raw-http`, one thread per connection, one `recv` and one `send` per request) reached 166,374 req/s
on these machines with the same load generator; Cameleer's fixed-reply route reached 112,910 req/s (68% of that
ceiling), Camel 67,682. The event-driven server core that this section named as the next step shipped in 0.6.1
(simoom, an event loop per core): in the 0.7.0 round the fixed reply served 146,477 req/s on other droplets, and
in the 0.8.0 camel round 102,131 req/s in `sfo2`. The ceiling was not re-measured since, and figures from
different droplets are not comparable, so there is no current "percent of ceiling".

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

*The figures above are from 2026-11-05 (WSL2, 0.4.x). The current ones, from the droplets, are in "Size, cold start
and memory of 0.8.0" at the end of this page: the `full` stripped binary is 26.7 MB and `core` 7.2 MB (glibc), a
`platform-http` route is ready in about 3.0 ms (`full`) and 2.5 ms (`core`), at 11.3 and 6.8 MB of peak memory.
The "about 20 MB" container image was not re-measured since the pack and MCP growth.*

## gRPC and HTTP/2 on simoom (0.7.0, 2026-11-18)

Release 0.7.0 moved the gRPC client and server and the HTTP/2 code onto simoom
(see the [0.7.0 release notes](https://github.com/cameleering/cameleer/blob/main/docs/release-notes/0.7.0.md)). This round compares two builds of Cameleer, 0.7.0
against 0.6.1, on the three-droplet setup above: the same machines, the same load generator for both, each
build on both servers in both phases, two runs at 64 connections and one at 256. (This round and the next two
were on `blr1`/`sfo3` droplets. The 0.8.0 round at the end of this page supersedes the absolute gRPC figures and
the HTTP/2 conclusion.) The raw records, the
method and the cases that failed in the first attempt are in
[`benchmarks/results/2026-11-18-linux-grpc-http2/`](../benchmarks/results/2026-11-18-linux-grpc-http2/).
It is not a comparison with Apache Camel: no Camel side was written for gRPC.

| scenario, 64 connections | new req/s | old req/s | new / old | p99 new / old | CPU per request new / old | peak memory new / old |
|---|---:|---:|---:|---|---|---|
| HTTP, fixed reply | 146,477 | 146,770 | 1.00x | 1.42 / 1.52 ms | 20 / 20 us | 10.4 / 10.6 MB |
| REST API with JSON | 123,793 | 130,956 | **0.95x** | 1.56 / 1.45 ms | 23 / 22 us | 10.9 / 11.1 MB |
| gRPC unary call (protobuf) | 10,472 | 9,926 | **1.06x** | 10.85 / 16.66 ms | 204 / 287 us | 13.1 / 16.3 MB |
| MCP tool call, stateless | 54,179 | 54,187 | 1.00x | 4.16 / 4.08 ms | 54 / 54 us | 30.6 / 30.5 MB |
| MCP tool call, session | 55,816 | 56,082 | 1.00x | 4.10 / 3.96 ms | 51 / 50 us | 30.8 / 30.6 MB |

At 256 connections gRPC is 1.05x (10,328 / 9,882 requests per second, CPU per request 199 / 284 us, p99
39 / 45 ms) and the other rows are within 2%.

### Gains

- **gRPC server, per call:** 29% less CPU per request (204 against 287 us) and a p99 that is 35% lower at 64
  connections (10.85 against 16.66 ms). Throughput is 1.05x to 1.06x, within what two runs can separate from
  noise, so the claim is the CPU and the tail, not the throughput.
- **Binary size:** the `full` build is 0.7% smaller (26,523,736 against 26,714,760 bytes, stripped) and the
  `core` build is unchanged (7,047,640 against 7,047,768). That is the net of deleting the old HTTP/2 and HPACK
  code and using simoom's; the 0.6.1 growth (+1.7% and +7.4%) was measured before that code was removed.
- **Cold start and memory of the CLI:** no difference (ready in 2.2 ms for `full`, 1.6 ms for `core`).

### Losses and limits

- **`rest-json` came out 5% slower** (0.94x and 0.95x in the two runs, 123,793 against 130,956 requests per
  second; p99 1.56 against 1.45 ms), but this is probably noise: in the later round with the gRPC thread pool,
  two builds with identical HTTP code gave 0.98x and 0.91x for the same route (and 1.05x and 0.93x for MCP),
  so swings of 5 to 9% between two builds on this setup are not results. `http` and MCP showed no change.
- **HTTP/2 measured at 0.51x of HTTP/1.1 (63,941 against 125,721 requests per second) in this round. The gap had
  two causes.**
  - **Mostly the load generator:** cameleer-bench's HTTP/2 client hands every response from a reader thread to
    the waiting request thread, two thread hand-offs per response (about 53 us of load-node CPU per call against
    21 us for its HTTP/1.1 client).
  - **Partly the server:** simoom started its event loops lazily, and a connection never moves between loops. When
    connections arrive in a burst, most of the four long-lived HTTP/2 connections stayed on one loop. The servers
    now start every loop.
    - On WSL2, where client and server share the CPUs, that was worth 2.07x for gRPC unary and +14% for
      `rest-json-h2` (means of three noisy rounds).
    - On the droplets the same change gave 1.04x and 1.05x (one run, `benchmarks/results/2026-11-19-linux-http2-loops/`),
      because connections from another machine arrive a few ms apart.

  Driven by nghttp2's `h2load` on three `c-4` droplets (2026-11-19, sfo3, build `916bbca`,
  `scripts/cloud-bench/peer.sh`, records in `benchmarks/results/2026-11-19-linux-http2-peer/`), the same server
  served (server a / server b, one run per case):

  | client | HTTP/1.1, 64 connections | HTTP/2, 4 x 16 streams | HTTP/2, 16 x 4 | gRPC unary, 4 x 16 |
  |---|---:|---:|---:|---:|
  | `h2load` | 86,429 / 70,696 | 153,010 / 115,820 | 139,469 / 125,521 | 93,157 / 73,426 |
  | cameleer-bench | 103,390 / 75,271 | 79,182 / 75,013 | - | 58,046 / 49,911 |

  The cameleer-bench row comes from the same session as the `h2load` row, not from the 0.7.0 round above (blr1),
  hence its different HTTP/1.1 figure. The two protocols are not loaded alike: HTTP/1.1 is 64 connections with one
  request in flight each, HTTP/2 is 4 connections with 16 streams each. `h2load`'s HTTP/1.1 is also below
  cameleer-bench's, so neither reached an HTTP/1.1 ceiling. (A later round with eager loops measured `rest-json-h2`
equal to `rest-json`, 1.00x, and `h2load` at 1.5x to 1.6x: see the 0.8.0 section.) So, with `h2load` and 16 streams per connection, one
  run on two droplets, HTTP/2 came out at 1.6x to 1.8x HTTP/1.1 on this server, and gRPC unary higher than the
  cameleer-bench figures in this document. The analysis is in `docs/design/http2-throughput.md`.
- **gRPC stopped at about 11,000 calls per second on both builds** (release 0.7.0 and 0.6.1), from 16 concurrent
  calls up, while neither the server nor the load generator was out of CPU. Starting threads for each call was the
  cause: on a pool of threads the 0.7.0 server does 4.3x to 4.8x as many calls (next section). cameleer-bench's own
  client also has a ceiling, which a later `h2load` run showed to be below the server's.
- **At 256 connections** the 0.7.0 gRPC server's peak memory was 18.9 against 17.5 MB (one thread per call).
- Noise: the two servers differ by 10 to 25% in speed and phase 1 was slower than phase 2 in the first run, so
  differences of a few percent between builds are not results. The figures are means over both servers and
  both phases.
- Not measured: gRPC streaming calls, HTTP/2 with TLS, the gRPC client against the old client, Windows.

### gRPC on a pool of threads (2026-11-18, same day)

The ceiling above was tested by running the threads of a call (the call, the reader of its request messages and,
with a deadline, the handler) on one process-wide cached pool instead of starting them per call. New is that
change (`80e3d08`), old is `main` at release 0.7.0 (`d14472e`); the setup, the load generator and the way of
counting are those of the round above. Raw records, method and a noise control:
[`benchmarks/results/2026-11-18-linux-grpc-pool/`](../benchmarks/results/2026-11-18-linux-grpc-pool/).

| gRPC unary call | 64 concurrent calls (two runs) | 256 concurrent calls |
|---|---|---|
| requests per second, new / old | **47,282 / 10,174 = 4.65x** (per run 4.82x and 4.48x) | 45,108 / 10,400 = 4.34x |
| p99 | 2.52 / 13.96 ms | 10.22 / 39.06 ms |
| server CPU per request | 57 / 221 us | 56 / 200 us |
| **server peak memory** | **49.0 / 14.1 MB** | **168.5 / 20.5 MB** |

- **The suspicion was right:** with the thread starts removed, the same server does 4.3x to 4.8x the calls per second
  on the same machines, at a quarter of the CPU per call and a fifth of the p99.
- **The price was memory:** the pool kept many threads, each with its stack and buffers, so peak memory was 3.5x at
  64 concurrent calls and 8x at 256 (49 and 169 MB). The next section removes that cost.
- **Everything else is unchanged within the noise** (HTTP 1.03x, REST 0.95x, MCP 0.99x; the code is identical in
  both builds, so these show the size of the noise: 0.91x to 1.06x from run to run).
- Not measured: streaming calls, TLS, the gRPC client, Windows.

### gRPC on a pool of threads, without the memory cost (2026-11-19)

The first pool used up to three threads per call and kept them. Two changes remove the cost: a call is now one job on
the pool (the request body is read with simoom's waker based `BodySource::poll_read` on the call's own thread, and
deadlines are kept by one shared timer thread), and a pool worker that has no job hands the blocks in its allocator
cache (including the unused rest of its chunk) to other threads before it waits, so memory freed by one worker is
reused by the next. New is branch `grpc-mem` (`a3b3879`); one session on `c-4` droplets in `sfo3` (the earlier rounds
used `blr1`, where `c-4` is no longer offered). Raw records, method and the tables:
[`benchmarks/results/2026-11-19-linux-grpc-memory/`](../benchmarks/results/2026-11-19-linux-grpc-memory/).

| gRPC unary call | against 0.7.0, 64 calls | against 0.7.0, 256 calls | against the first pool build, 64 calls |
|---|---|---|---|
| requests per second, new / old | **41,020 / 10,514 = 3.90x** (second run 3.96x) | 36,266 / 10,194 = 3.56x | 40,788 / 39,020 = 1.05x |
| p99 | 3.96 / 12.71 ms | 23.07 / 41.16 ms | 4.01 / 4.12 ms |
| server CPU per request | 37 / 214 us | 38 / 204 us | 35 / 60 us |
| **server peak memory** | **13.5 / 13.8 MB** | **18.6 / 20.1 MB** | **14.3 / 49.7 MB** |

- **Superseded absolute figure.** 41,020 calls/s was limited by cameleer-bench's client, not the server: the same
  server under `h2load` served 73,000 to 93,000 on these machines, and the 0.8.0 round below measured 94,000 at 64
  calls (129,000 to 135,000 under `h2load`). Only ratios inside one session hold, and CPU per call.
- **Same speed as the first pool, a quarter of its memory, and no more memory than 0.7.0** (13.5 against 13.8 MB at
  64 calls, 18.6 against 20.1 MB at 256), at 3.6x to 3.9x the call rate, a third of the p99 and about a sixth of the
  CPU per call.
- **The rest-json 0.93x to 0.95x of earlier rounds was probably the harness, not the code:** with the order of the
  sides swapped, REST measured 0.98x against the pool build (identical code), and the HTTP, REST and MCP ratios
  stayed between 0.90x and 1.04x from run to run.
- Not measured: streaming calls, TLS, the gRPC client, Windows, long-lived idle calls.

## 0.8.0 candidate against 0.7.0, and against an independent client (2026-11-20)

The build for release 0.8.0 (`main` plus the HTTP/2 and header changes: simoom 0.2.0, event loops that start at once,
cheaper gRPC unary calls, headers kept sorted; commit `7ee75e7`) against release 0.7.0, on three `c-4` droplets in
`sfo2` (Xeon Platinum 8168 on the servers, 8280 on the load machine; `sfo3` no longer offers `c-4`, so absolute
figures differ from the earlier rounds and only comparisons inside this session count). Same load generator for both
builds (cameleer-bench with simoom-client 0.2.0), servers swapping roles, two runs at 64 connections (the second with
the order of the sides swapped) and one at 256. Raw records and method:
[`benchmarks/results/2026-11-20-linux-0.8.0-candidate/`](../benchmarks/results/2026-11-20-linux-0.8.0-candidate/).
**0.7.0 does not contain the gRPC pool fix** (the section "gRPC on a pool of threads" above), so the gRPC ratio mixes
that fix with the later changes; the round cannot say how much each contributed, and the pool change alone was
measured at 3.6x to 3.9x on other droplets.

| scenario | 64 connections, run 1 | 64, sides swapped | 256 connections |
|---|---:|---:|---:|
| HTTP, fixed reply | 0.86x | 0.92x | 0.97x |
| REST API with JSON | 1.03x | 0.99x | 1.03x |
| REST API with JSON, HTTP/2 (4 connections) | 1.00x | 1.02x | 0.94x |
| gRPC unary call | 11.5x | 11.2x | 13.3x |
| MCP tool call, stateless | 0.96x | 0.96x | 0.98x |
| MCP tool call, session | 0.97x | 0.97x | 0.97x |

The ratios are 0.8.0 over 0.7.0. In absolute terms, 0.8.0: `http` 85,000 to 106,000 req/s, `rest-json` 78,000 to 87,000,
`rest-json-h2` 79,000 to 94,000, gRPC unary **94,400 calls/s at 64 calls and 106,200 at 256** (0.7.0: 8,300 and
8,000), p99 1.2 and 4.7 ms (0.7.0: 14.8 and 54 ms), **24 us of server CPU per call** (0.7.0: 270 us), peak memory
9.2 MB at 64 calls and 10.2 MB at 256 (0.7.0: 13.6 and 19.3 MB). The "41,020 calls per second, 3.9x" figure of the
pool round is superseded: it was limited by the load generator and measured before the later changes.

- **HTTP/2 is not slower than HTTP/1.1 on this server.** `rest-json` over HTTP/2 equals `rest-json` over HTTP/1.1
  (1.00x). Driven by nghttp2's `h2load` (`scripts/cloud-bench/peer.sh`, two runs on the two servers, same build),
  HTTP/1.1 with 64 connections served 67,000 to 92,000 req/s (mean about 80,000), HTTP/2 with 4 connections of 16
  streams 116,000 to 126,000 and with 16 connections of 4 streams 121,000 to 127,000, and gRPC unary (4 x 16)
  129,000 to 135,000 calls/s: **HTTP/2 at 1.5x to 1.6x its HTTP/1.1 rate**, and gRPC 1.4x the rate cameleer-bench's own
  generator reaches (94,000). The two protocols are not loaded alike (one request in flight per HTTP/1.1
  connection, 16 or 4 streams per HTTP/2 connection), so this says the server is not slow at HTTP/2, not that
  HTTP/2 is 1.5x faster. One `h2load` run had 8 failed requests out of 3.07 million.
- **Event loops that start at once cost nothing measurable on `http` or `rest-json`.** `http` is 0.86x to 0.97x
  but moved between 0.84x and 1.02x per server and between passes, which is the noise of this setup (there is no
  identical-code control in this round); `rest-json` is 1.01x on average, so the 5% loss seen in the 0.7.0 round is
  not there. The cost shows only in start-up and memory (below).
- **MCP is 2% to 4% slower than 0.7.0, and probably really is.** It is 0.96x to 0.98x on every pass with the sides in
  either order and 0.94x to 0.99x per server, which is tighter than the noise on the other rows (p99 and CPU per call
  are within 1 to 3%; peak memory is lower). This round cannot say whether the eager event loops or the change to
  how headers are stored causes it, because neither was measured alone. Not isolated.
- **Not measured on droplets: asynchronous routing.** The 2.2x to 2.9x for a `platform-http` to `http` proxy is from
  one WSL2 machine only (see the CHANGELOG and `docs/design/async-routing.md`); this round's `http` and `rest-json`
  routes do not use the `http` producer. `CAMEL_SERVER_LAZY_LOOPS=true` (the old start) was not run as a control, h2load
  over TLS and gRPC streaming have no scenario, and Windows was not measured.

### Size, cold start and memory of 0.8.0

`cameleer` CLI, `dist` builds on the `sfo2` server (`scripts/cloud-bench/run.sh sizes`, 4 rounds of 20 cold starts,
means; 0.8.0 against 0.7.0):

| build | binary, stripped | HTTP route ready | timer route ready | peak RSS, HTTP route |
|---|---:|---:|---:|---:|
| every component (`full`) | 26.72 / 26.52 MB (+0.8%) | 3.02 / 2.72 ms | 3.38 / 3.31 ms | 11.3 / 10.9 MB |
| `core` | 7.22 / 7.05 MB (+2.4%) | 2.45 / 1.99 ms | 2.60 / 2.51 ms | 6.8 / 6.5 MB |

Unstripped: `full` 30,219,544 against 29,979,312 bytes, `core` 8,416,632 against 8,210,640. The start-up and memory
difference (about 0.3 to 0.5 ms and 0.3 to 0.4 MB) is the cost of starting every event loop, one per core (4 here).
These droplets are slower per core than the Ryzen of the FaaS table above, so the two tables are not comparable.
Raw: `sizes.txt` in the results folder.

## Found a problem?

If a number here doesn't match what you measure, please
[open an issue](https://github.com/cameleering/cameleer/issues/new) with your setup and results, or
fork the repository, fix the harness, and open a pull request.
