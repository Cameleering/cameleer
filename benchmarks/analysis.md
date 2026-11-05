### FaaS cold start and size

Fourth round (branch `performance-improvement`, 2026-11-05): Cameleer as a function runtime.
The throughput tables above are from the third round and still stand; this round's changes
moved in-process throughput by no more than the noise (see below). The prioritised review is in
[`benchmarks/perf-review.md`](../benchmarks/perf-review.md), the landscape in
[`benchmarks/perf-research.md`](../benchmarks/perf-research.md).

**Method.** `cameleer-bench coldstart` spawns the real `cameleer` CLI 40 times and records
spawn → the `Routes startup` log line (ready), spawn → exit, and peak RSS; medians. Routes: a
`timer` that fires once with `--max-messages 1` (a run-once job), and one `platform-http` route
(killed when ready). Linux is WSL2 Ubuntu 24.04 on the Ryzen 5 5600X, page cache warm, `dist`
builds, stripped. *Base* is `main` at `0b44c98` as shipped (thin LTO, glibc); the other rows are
this branch (fat LTO). Timings are from the quietest run (branch at `5d496fe`); sizes at the
final head (`a2c0639`, within 8 KB). A Rust hello-world takes 0.48 ms on the same setup.

| build (Linux x86-64) | binary | `version` | timer: ready | timer: exit after 1 message | HTTP route: ready | RSS, HTTP route | of which private | threads, idle HTTP |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| base: `main`, all components, glibc | 24.9 MB | 0.82 ms | 1.52 ms | 21.9 ms | 3.27 ms | 15.2 MB | 1.71 MB | 34 |
| all components, glibc | 23.9 MB | 0.71 ms | 1.14 ms | 2.15 ms | 1.17 ms | 10.4 MB | 0.72 MB | 2 |
| `--features cloud`, glibc | 11.7 MB | 0.66 ms | 1.01 ms | 2.01 ms | 1.07 ms | 7.4 MB | 0.56 MB | 2 |
| `--features core`, glibc | 7.3 MB | 0.64 ms | 0.94 ms | 1.95 ms | 0.97 ms | 6.2 MB | 0.49 MB | 2 |
| all components, static musl | 24.0 MB | 0.40 ms | 0.96 ms | 1.78 ms | 0.92 ms | 7.4 MB | 0.75 MB | 2 |
| `--features cloud`, static musl | 11.8 MB | 0.34 ms | 0.71 ms | 1.48 ms | 0.75 ms | 5.3 MB | 0.58 MB | 2 |
| `--features core`, static musl | 7.5 MB | **0.31 ms** | **0.66 ms** | **1.41 ms** | **0.69 ms** | **4.2 MB** | 0.51 MB | 2 |

So a function build starts its routes in **0.7 ms** (static musl, `core` or `cloud`), a run-once
job is done in 1.4 ms instead of 22 ms, and an idle HTTP function holds 2 threads and about
0.5 MB of private memory. RSS is mostly pages of the binary (shared and clean), which is why it
follows the binary size. Static musl starts fastest because there is no dynamic loader and no
relocation pass: `version` costs less than a dynamically linked hello-world.

Windows 11, same machine, `dist` builds, medians of 30 (the machine was shared with other
work and moves by ±1.5 ms; a hello-world takes 4.7 ms to spawn there):

| build (Windows x86-64) | exe | HTTP route: ready | timer: exit after 1 message | peak working set, HTTP |
|---|---:|---:|---:|---:|
| base: `main` | 33.5 MB | 12.1 ms | 33.5 ms | 9.1 MB |
| all components | 30.0 MB | 10.3 ms | 10.3 ms | 6.8 MB |
| `--features core` | 8.7 MB | 9.4 ms | 10.6 ms | 6.4 MB |

**AWS Lambda.** The `core` musl build as `bootstrap` in AWS's own `provided:al2023` image
(`public.ecr.aws/lambda/provided:al2023@sha256:ca6c7b9e…`, which runs the Lambda Runtime
Interface Emulator) handled a direct invoke, an HTTP API (payload 2.0) event through a
`platform-http` route and a 404, all correctly. The emulator reported `INIT REPORT` of
**3.5–5.2 ms** (5 cold starts, binary baked into the image; this includes the emulator's own
process start) and warm invocations of **0.7–1.3 ms**. That is an emulator on a desktop, not the
Lambda service; for comparison, the public `maxday/lambda-perf` dashboard (2026-11-05) shows a
Rust hello-world on `provided.al2023` at 17.5 ms (arm64) / 19.5 ms (x86-64) init on real
Lambda, GraalVM native Java at 131–154 ms and managed Java 21 at 184–244 ms
([perf-research.md](../benchmarks/perf-research.md)). Measuring on the service needs an AWS
account (`docs/STATUS.md`).

**Throughput did not regress** (Linux, `cargo build --release` as in `scripts/bench.sh`,
in-process scenarios, base vs branch interleaved). A first pass showed `direct` 5.6% slower; a
bisect found two causes, both fixed: the log macros had stopped checking the level inline
(debug statements sit on hot paths), and the allocator's per-class bins had doubled to 32 bytes.
After the fixes, pinned to two cores, 6 interleaved runs each: `direct` base 4.77M msg/s (max
5.24M), branch 4.81M (max 5.12M); `cbr` 2.13M / 2.14M; `seda`, `seda-inout` and `splitagg`
within ±5% run to run, with `splitagg` 4–10% faster on the branch in every round. Static musl
is slower for allocation-heavy work: −3% `direct`, −11% `cbr`, −14% `seda-inout`, −22%
`splitagg` against glibc, so the container image stays glibc and musl is used where it is
needed (Lambda).

### Fourth round: what changed

1. **Thread pools start workers on demand** (`cameleer_core::executor::ThreadPool`), like Java's
   `ThreadPoolExecutor` core threads, and an idle worker exits after the keep-alive (60 s,
   Camel's default thread pool profile allows core threads to time out). The HTTP server's
   32-thread pool was the largest startup cost: 1.7 ms of 3.3 ms on Linux. `ThreadPool::prestarted`
   keeps the old behaviour. Tests: `workers_start_on_demand_up_to_the_size`,
   `idle_workers_exit_after_the_keep_alive_and_come_back_on_demand` (a job submitted while a
   worker is leaving is never stranded).
2. **Component families are cargo features of `cameleer-main`** (`amqp`, `amqp10`, `cloud`,
   `dataformats`, `datastores`, `grpc`, `kafka-mqtt`, `protocols`, `saas`, `script`, `soap`,
   `sql`, `ssh`, `xml`; default `full`, `core` = none). Core, the YAML/XML DSL, HTTP and TLS are
   always in. A route that names a left-out component fails with the feature to enable
   (`cameleer_core::component::set_unavailable_hint`). `tests/features.rs` checks the scheme
   table against what each family registers; CI builds and tests the reduced build. The
   Dockerfile takes `--build-arg FEATURES=...`.
3. **Log macros format only enabled records** and call one out-of-line function
   (`log::log_args` with `format_args!`) instead of an inlined `format!` at each of about 190
   call sites; the level check is one relaxed atomic load. This alone made the full binary 3.1 MB
   (11%) smaller.
4. **Fat LTO** for `dist`: measured against thin LTO on the same code, `splitagg` +6–8%, peak RSS
   −25%, binary +8% (before item 3). `opt-level = "s"` was 13% smaller but 10–19% slower, so it
   is not used.
5. **The run loop wakes when max messages is reached** instead of polling every 20 ms (a
   condition variable; signals and file changes are still polled).
6. **The allocator carves chunks lazily**: a new 4–16 KiB chunk was split into a free list,
   which wrote into, and so made resident, every page of it; now blocks are taken from a bump
   region as needed. Private memory of an HTTP route: 1.71 → 0.72 MB.
7. **AWS Lambda custom runtime** built into `cameleer run` (`cameleer_main::lambda`): Runtime API
   loop, `/init/error` for startup failures, `/error` with the exception kind, HTTP events served
   in process by the `platform-http` routes (no socket is bound), `bootstrap` as the executable
   name. Tests: `tests/lambda.rs` against an embedded Runtime API, plus the AL2023 emulator run
   above (which found that binding port 8080 failed inside the sandbox). Packaging:
   `scripts/package-lambda.sh` (static musl, because AL2023's glibc 2.34 is older than the
   `GLIBC_2.35` `hypot` a current build links against).
8. **Startup recorder** (`camel.main.startupRecorder=logging`, Camel's `StartupStepRecorder`):
   per-step offsets and durations in the log.
9. No local status files or status thread on Lambda, Cloud Run/Knative (`K_SERVICE`) and Azure
   Functions custom handlers.

Measured and not adopted: `-z pack-relative-relocs` cuts `.rela.dyn` from 535 KB to 10 KB
(−0.5 MB) but needs glibc ≥ 2.36 at run time (fine for the Debian 12 image, not for AL2023);
its timing was measured on a loaded machine, so it is left for a follow-up.

Follow-ups (from the review): core cargo features for the template engines, jq and the core
XPath subset (an estimated 0.7–1 MB of the 7.3 MB `core` build); `cameleer-http` without
`cameleer-grpc` unless HTTP/2 is used; CLI-only code (`validate`, `schema`) behind a feature;
XQuery/XSLT start a thread with a 128–256 MiB stack per evaluation; the local status thread
rewrites its file every 500 ms even when nothing changed; a shared timer thread per timer name
(Camel's `java.util.Timer` model).

### Where Cameleer stands against the 4x target

Cameleer reaches **4x in `direct` (4.27x), `seda` (4.18x) and `seda-inout` (8.37x)** and is
**below 4x in `cbr` (3.48x), `splitagg` (3.18x) and `http` (2.45x)**. Memory stays 25–46x lower and
startup 77x faster. The final run was on an idle machine (the WSL/Docker VM was paused and no
`vmmemWSL` process was running; 98% idle before the run). Throughput, rust ÷ java, on the same
machine:

| scenario | first round | second round | third round (final) |
|---|---:|---:|---:|
| direct | 0.68x | 4.06x | **4.27x** |
| seda | 1.09x | 4.07x | **4.18x** |
| seda-inout | 0.77x | 8.51x | **8.37x** |
| cbr | 0.56x | 3.24x | 3.48x |
| splitagg | 0.64x | 2.99x | 3.18x |
| http | 2.02x | 2.31x | 2.45x |

Java's own numbers move between runs (direct 1.06M → 1.16M msgs/s, http 76k → 112k req/s, the latter
because the second-round run shared the CPUs with a busy WSL VM), so compare ratios, not absolute
numbers, across rounds. Peak RSS is 2–7 MiB above the first round: the thread-caching allocator keeps
freed small blocks for reuse.

### First round (3x target): profiling method

In the first round there was no sampling profiler on the Windows machine, so `cameleer-bench breakdown` times individual
stages in a loop (ns/op, release build). Temporary timing probes inside the route wrappers then split
the cost of one `direct:` exchange. Times are from that tool, run before and after the fixes on the
same machine:

| stage (ns/op unless noted) | before | after |
|---|---:|---:|
| `ctx.get_producer("direct:direct")` (used by every `ProducerTemplate` send) | 517 | 35 |
| `Exchange::new` (exchange + message ids, timestamp) | 248 | 123 |
| `generate_id` | 108 | 44 |
| `template.send_body` to the `direct` route, end to end | 2,124 | 1,280 |
| JsonPath `$.items` on the order text | 9,227 | 6,420 |
| `splitagg`: one order through split + aggregate (µs) | 69.3 | 40.6 |
| HTTP consumer, 16 keep-alive connections (req/s, p99; from `bench.sh` runs) | 43,358, 4.3 ms | 151,547, 0.44 ms |

### First round: fixes made (each has a regression test)

1. **HTTP server closed keep-alive connections under load.** This was a bug.
   `cameleer-http/src/server.rs` stopped keeping a connection alive whenever *any* job was pending in
   the worker pool. Every newly accepted connection is pending for the few microseconds before an idle
   worker picks it up, so with idle workers available, connections were closed anyway. That caused a
   self-sustaining reconnect storm: 15,000+ sockets in TIME_WAIT, ephemeral-port exhaustion on
   Windows, and p99 in the milliseconds. Now connections are closed only when the pool is saturated
   (`ThreadPool::is_saturated`: more pending jobs than idle workers), which keeps the fairness intent.
   HTTP throughput went up about 3.6x. Tests: `keep_alive_survives_new_connections_while_workers_are_idle`
   fails on the old code; `keep_alive_is_closed_when_connections_wait_for_a_worker` covers the fairness
   case.
2. **`get_producer` re-parsed and re-normalized the URI on every send.** That was two `EndpointUri`
   parses, placeholder resolution and two map lookups. There is now a cache keyed by the URI exactly as
   given. URIs with `{{placeholders}}` and `ref:` bypass it, and it is cleared with the producer cache
   on stop.
3. **Inflight repository** took a global `Mutex<HashMap>` and allocated a `String` for every exchange
   in every route. All seda consumer threads contended on it. Each route now holds its own atomic
   counter.
4. **Events built without listeners.** `RouteProcessor` cloned the exchange id and route id into
   three `CamelEvent`s per exchange even with no `EventNotifier` registered. `has_event_notifiers()`
   now guards event construction.
5. **Header lookups allocated** a lowercased copy of the key on every `get`, `contains` and `remove`.
   They now lowercase short keys on the stack.
6. **`generate_id`** used `format!` (called twice per exchange). It is now built by hand.
7. **JsonPath deep-copied the document twice per evaluation.** The language input (the body) was
   cloned, structured input was cloned again, and `select` cloned the root and every intermediate
   node. It now borrows the input and selects by reference, cloning only the result.
8. **Splitter copied the original body into every part** (`correlated_copy` before the part replaced
   the body). The non-streaming path now copies the exchange without its body, as the streaming path
   already did.
9. **Aggregator copied the whole group for every incoming exchange** (`MemoryAggregationRepository::get`
   clones), which is O(n²) per group. New `AggregationRepository::take` (default: `get`, so persistent
   repositories are unchanged) moves the group out of the in-memory repository. The aggregator uses it
   only when nothing can fail before the result is stored: the strategy says `never_fails()`
   (use-latest, use-original, grouped body and grouped exchange do) and there is no completion
   predicate or size expression. A failing strategy still leaves the group untouched, as in Camel
   (`a_failing_strategy_leaves_the_group_in_the_repository`).

### Second round: the 4x target

The owner raised the target to **4x**. The first round left the in-process scenarios slower than
Java. In the second round a std-only sampling profiler was added (`cameleer-bench profile <scenario>
<secs>`: it suspends each thread, unwinds its stack with `RtlVirtualUnwind` and resolves symbols with
`dbghelp`; build with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`). Every change below came from a
profile, keeps Camel's semantics, and has tests (`crates/cameleer-core/tests/perf_semantics.rs` plus unit
tests in the changed modules).

Allocation and copying:

1. **A std-only thread-caching allocator** (`cameleer_core::alloc::CamelAllocator`), installed by the
   `cameleer` CLI and the benchmark binary (applications opt in with `#[global_allocator]`). Blocks up to
   4 KiB come from per-thread size-class free lists; threads that allocate on one side and free on the
   other (seda) exchange whole batches through a lock-free stack. An allocate+free pair went from about
   30 ns (`HeapAlloc`) to about 7 ns.
2. **Lazy exchange and message ids** (`LazyId`, 8 bytes). Camel 4 also creates ids on first use.
   Copies generate the id first, so a copy keeps it.
3. **Headers** are a vector sorted by lowercased key instead of a `BTreeMap` that stored every key
   twice. **Properties** are a sorted vector with the `BTreeMap` method names, and Camel's own
   property names are stored as `'static` strings without allocating.
4. **`ObjectMap` is copy-on-write**: cloning a `Value::Object` (a body, a JSON document) bumps a
   reference count, and the first change to a shared map copies it. This matches the shallow
   copies of a Java exchange copy and keeps value semantics. `Value` shrank from 72 to 32 bytes.
5. **`from_route_id` / `from_endpoint` are `Arc<str>`**, shared instead of copied per exchange.
6. The splitter creates sequential parts one at a time; a split whose only step is an `aggregate`
   lets the aggregator move each part's message instead of copying it (the part is discarded
   afterwards). JsonPath parses a body once per thread for several paths and moves large results
   out of the parsed document.

Bookkeeping and clocks:

7. **Statistics use a cheap tick clock** (`cameleer_core::clock`: the invariant TSC, about 8 ns, instead
   of `QueryPerformanceCounter` plus `SystemTime::now`, about 50 ns) and keep raw ticks, converted
   when read. A route is timed once (the route processor feeds management's route counters), and the
   steps of a pipeline chain their readings (one clock read per step instead of two). Blocks (choice,
   filter) pass out the last reading of their children.
8. **Striped counters** (`cameleer_core::stripe`): per-node and per-route statistics, inflight counts and
   the context counters each have one cache line per stripe; a thread owns a stripe and updates it
   with plain stores. Before, four seda consumers wrote the same cache lines for every exchange.
9. Management no longer registers an event notifier to count exchanges (that made every exchange
   build and clone events). The route processor counts root exchanges directly.
10. A thread-local one-entry cache in `get_producer`, and `ProducerTemplate` sends in place instead of
    moving the 232-byte exchange through several calls.

seda:

11. **Two-lock queue** (Java `LinkedBlockingQueue` style): producers append to an inbox, consumers take
    from an outbox and swap the inbox in when it is empty, so producers and consumers do not contend.
    The hot fields are on separate cache lines. Parked threads are signalled only when some are
    waiting (a signal is a system call), and both sides spin briefly before parking.
12. **InOut replies use a reusable per-thread reply slot** instead of a channel per request, with a
    generation number so a reply to a timed-out request never reaches the next one.

Language and EIPs:

13. Simple comparisons borrow header, property and body values; `${header.x} <op> literal` is
    evaluated directly; string/integer comparisons and templates skip the formatting machinery.
14. The in-memory aggregation repository keeps the current group outside its hash map (consecutive
    exchanges of one group hash nothing), correlation keys are not copied, and repositories that do
    not confirm do not force the completed exchange's id.

HTTP:

15. The server sets the socket read timeout only before a read that can block and only when it
    changes (it was two `setsockopt` system calls per request).

### Third round

- **Statistics levels (Camel's `ManagementStatisticsLevel`).** `Extended`, `Default`, `RoutesOnly`
  and `Off`, set with `CamelContext::set_statistics_level` or
  `camel.main.jmxManagementStatisticsLevel`. **Cameleer's default is `RoutesOnly`**: route and
  context statistics are kept, per-step (processor) timing is opt-in. This differs from Camel, where
  the default level is `Default` but statistics exist only when `camel-management` is loaded; without
  it (as in the Java benchmark, and in a plain `DefaultCamelContext`) there are none. Cameleer always
  has management, so its default keeps the cheap route-level numbers and leaves per-step timing to
  those who ask for it. Tracing and message history still see every step at any level. In the same
  process, the `cbr` route costs about 400 ns per message at `RoutesOnly` and 430–470 ns at
  `Default`.
- **Aggregation in place.** `AggregationStrategy::aggregate_into` (default: `aggregate`) and
  `AggregationRepository::with_slot` let the aggregator change the group inside the in-memory
  repository instead of moving the 232-byte group exchange out and back for every exchange.
- **Shared header names.** `setHeader` keeps its header name as one `Arc<str>`; headers store owned
  or shared names, so copies of an exchange (split parts, seda, multicast) share the name instead of
  copying it.
- The cameleer-cloud secrets refresh tasks count a reload once it is done (listeners called, routes
  restarted); a test that waited for the count could see the reload half done.

Not done, and why: **JSON key interning** only saves anything if the parsed objects share their key
strings, and `ObjectMap` hands out `&String` keys across about 740 uses in the workspace, so it needs
an API change first. **A smaller `Exchange`** needs public field types to change (`exception`,
`created`, `from_*`).

### Why some scenarios are still below 4x

- **http (2.45x): above what this machine can show.** With the shared load generator (16 keep-alive
  connections, same machine), a do-nothing responder with no Camel at all reaches about 390–400k req/s
  whether it is thread-per-connection (`cameleer-bench raw-http <port> threads`) or an I/O completion port
  server with 8 workers (`raw-http <port> iocp8`); a single-thread `WSAPoll` loop reaches 108k. That
  ceiling is 3.5–3.6x Java's 112k, below the 446k that 4x needs, so no server design can prove 4x here.
  Cameleer's server reaches 273k, about 70% of the ceiling. An event-driven server in cameleer-http
  (IOCP/epoll) was not built: on this machine it measured no faster than threads. Proving 4x needs the
  load generator on another machine.
- **splitagg (3.18x).** Per order about 2 µs of JSON parsing (a `String` per key and value), then for
  each of the ten parts a correlated copy (its own id, properties and header) and the aggregation.
  4x needs about 7 µs per order; it takes about 8.8 µs. Next: shared JSON keys (an `ObjectMap` API
  change) and a smaller `Exchange` (public field changes).
- **cbr (3.48x).** About 470 ns per message including the benchmark loop: the exchange and its two
  headers (allocations), the Simple predicates, five steps of routing, the `setHeader` value and the
  transformed body. The remaining cost is spread evenly; no single step dominates.
- **seda** reaches 4.18x, but its result varies by about ±10% between runs (8 threads spinning and
  parking on 12 hardware threads).
- Not done: the optional Kafka scenario (it needs a real broker on both sides).
