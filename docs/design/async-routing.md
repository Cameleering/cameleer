# Asynchronous routing: an `AsyncProcessor` path for Cameleer

_2026-11-19. Phase 0 built and measured (section 12); the later phases are not. Follows the cost model in
[http2-throughput.md](http2-throughput.md), section "Root cause of the client gap"._

## 1. Why

Cameleer's routing engine is synchronous: `spi::Processor::process(&self, &mut Exchange)`. A route
holds its thread from the first step to the last, and a producer that talks to the network waits
for its reply on that thread. Two costs follow, and the cost model measured both.

**The hand-off cost.** A thread that waits for a reply must be woken when the reply arrives. That
wake is a context switch plus, in a VM, an inter-processor interrupt: about 12 µs of latency
across vCPUs against 0.2 µs for a plain system call. Measurements of the
same HTTP/2 client library (simoom branch `h2-event-client`, 64 in flight):

| client | client CPU per request | wakes billed to the client per request |
|---|---:|---:|
| blocking `Client` today | 36 µs | 1 + 1/k |
| event-driven client, caller blocks (facade) | 24 µs | 1 |
| event-driven client, caller continues in a callback | 3.2-4.8 µs | 0 |

A synchronous engine can at best use the facade. The 5x below it needs a caller that does not
wait.

**The thread bound.** An HTTP route that calls a slow backend holds an offload thread for the
backend's response time R. With T threads (`ServerConfig::pool_size`, 200 by default), Little's
law (L = λW) caps the route's throughput independently of CPU:

    X ≤ T / R          e.g. T = 200, R = 20 ms  →  X ≤ 10,000 req/s
                            T = 200, R = 2 s (a model call)  →  X ≤ 100 req/s

An asynchronous route holds no thread while it waits, so the cap becomes the CPU one, X ≤ P / C
(P CPUs, C CPU per request). For the action layer (model calls of seconds, MCP tools that call
APIs) this matters more than the microseconds.

### What a proxied request pays today

Take `platform-http → setHeader → http (HTTP/2) → respond`, the shape of most integration routes.
These hand-offs are counted from the code (`server_simoom.rs`, `platform.rs`, `producer.rs`):

| step | today | asynchronous design |
|---|---|---|
| request arrives on server loop Li | the governor sees a slow route and sends it to simoom's offload pool: queue push + worker wake | the route starts on Li |
| `http` producer | the facade: submit to a client loop, park, unpark (1 wake, billed to the client) | submit on Li, return `Pending` |
| response read by a client loop | wakes the parked route thread | the route continues on the thread that read it, Li when the client shares the server's loops |
| respond | worker → Li: a post, plus a signal when Li is parked | written on Li directly |
| **cross-thread hand-offs** | **2-3** | **0** |

At about 12 µs each on this VM that is 25-40 µs of CPU per request, on top of the client's own
36 → 5 µs. These are predictions from the model. Phase 0 (section 9) measures them before the
engine changes.

## 2. How Apache Camel does it

Camel ([Asynchronous Routing Engine](https://camel.apache.org/manual/asynchronous-routing-engine.html))
has two contracts:

- `Processor.process(Exchange)`: synchronous;
- `AsyncProcessor.process(Exchange, AsyncCallback) → boolean`: returns `true` when it completed
  synchronously (the callback has already run with `doneSync = true`). Otherwise it returns `false`
  and calls `callback.done(false)` later, from any thread.

The rest of the design follows from those two contracts:

- **Adapting synchronous steps:** a synchronous processor is adapted by running it, then calling
  `done(true)`.
- **Pipelines and error handlers:** these are state machines (`PipelineTask`, the redelivery tasks
  of `RedeliveryErrorHandler`). Since Camel 3 they are scheduled on a `ReactiveExecutor` queue
  instead of recursing, so a long route does not grow the stack.
- **Synchronous callers:** a `ProducerTemplate`, or a component with the `synchronous=true`
  endpoint option, block until the callback runs. This goes through the
  `AsyncProcessorAwaitManager`.
- **Transacted routes** run synchronously, because the transaction is bound to the thread.
- **Where a reply continues:** a producer completes on its I/O thread (Netty, Vert.x), and the
  route continues there. Blocking in such a continuation stalls the event loop. Vert.x warns about
  it, and Camel leaves it to the user.

Cameleer keeps the semantics (names, `synchronous`, transacted runs synchronously, the same
ordering and error handling) and takes a different mechanism and different placement rules.

## 3. Mechanism: std futures, polled where they are woken

Three ways to express "the rest of the route" without a thread:

| mechanism | cost per suspended step | porting 9,700 lines of EIPs | verdict |
|---|---|---|---|
| continuation passing (Camel's callbacks: owned exchange + boxed `FnOnce`) | one box per suspension | every container becomes a hand-written state machine (the error handler's retry loop, multicast's join, split, loop, doTry) | correct but costly to write and to review |
| green threads / stackful coroutines | a stack per exchange; switching needs assembly | none | needs code outside std (context switching), which the std-only rule forbids |
| **`std::future::Future` + `async fn`, Cameleer's own single-task executor** | one box per call of an asynchronous child (`dyn Future`) | an EIP reads like its synchronous form with `.await` at child calls; the compiler writes the state machine | **chosen** |

Everything the futures approach needs is in std: `Future`, `Pin`, `Context`, `Waker`, `task::Wake`,
`thread::park`. No async runtime is used. A task is polled on the thread that wakes it, which gives
Camel's "continue on the I/O thread" with no queue, no wake and no syscall.

`async fn` also makes the compiler check two things Camel leaves to discipline:

- a `MutexGuard` held across an `.await` is not `Send`, so it does not compile;
- a future borrowing an exchange cannot outlive it.

Rejected detail: making the trait take `self: Arc<Self>` (owned, `'static` futures). Every call
would then increment a refcount on a cache line shared by all cores. The cost model's spin case
measured 55 ns per cache-line round trip between two CPUs, paid per step per exchange. Borrowed
futures plus one audited `unsafe` in the executor (section 5) avoid it.

## 4. The contract

Additive. `Processor` keeps its signature, so user processors, closures and every existing EIP keep
working unchanged.

```rust
// spi.rs
pub type ProcessFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// Whether a synchronous step may hold its thread for long (I/O, locks, user code).
pub enum Blocking { Never, Maybe }

pub trait Processor: Send + Sync {
    fn process(&self, exchange: &mut Exchange) -> Result<()>;
    fn process_timed(&self, exchange: &mut Exchange, start: u64) -> (Result<()>, u64) { /* as today */ }

    /// The asynchronous form of this processor (Camel `AsyncProcessor`), when it has one.
    fn as_async(&self) -> Option<&dyn AsyncProcessor> { None }

    /// Used by the scheduler to keep blocking steps off event loops. Built-in steps that
    /// only compute (setHeader, transform with the built-in languages, choice and filter
    /// predicates, marshal, convertBodyTo) return `Never`; user processors default to `Maybe`.
    fn blocking(&self) -> Blocking { Blocking::Maybe }
}

pub trait AsyncProcessor: Send + Sync {
    /// Process `exchange`; the future completes when the step is done. Dropping the future
    /// cancels the step (the producer resets its stream, frees its slot).
    fn process_async<'a>(&'a self, exchange: &'a mut Exchange) -> ProcessFuture<'a>;
}
```

These rules decide which path a route takes:

- **Async-capable routes:** a route is async-capable when at least one step returns `Some` from
  `as_async` and no step forbids it (`transacted`, `synchronous=true`). Containers report
  themselves async-capable when a child is. This is decided once, when the route is built.
- **Every other route** keeps today's code path, at today's cost. A route that does not use the
  feature must not regress.
- **An async producer's synchronous `process`** is `block_on(self.process_async(ex))`: the facade,
  for callers that are themselves synchronous.
- **Inside an asynchronous container,** a synchronous step that returns `Never` is called directly,
  with no box and no poll. Only asynchronous children are awaited. A route of ten steps with one
  HTTP call pays three to five boxes per exchange (route entry, error channel, intercept, the
  producer). With the thread-caching allocator that is about 100 ns.

## 5. The executor (`cameleer_core::reactive`)

About 300 lines, std only.

- **`Task`:** owns the route processor (`Arc<dyn Processor>`), the exchange (`Box<Exchange>`, a
  stable address) and the root future, which borrows both. The borrow is self-referential: one
  `unsafe` lifetime extension. The future is declared first so that it drops first. This is the
  same pattern, and the same review bar, as the borrowed hook in `async_delay.rs`.
- **Waking and polling:** the task is its own `Waker` (`task::Wake` on `Arc<Task>`). Its state word
  is `IDLE → POLLING → (NOTIFIED) → IDLE | DONE`.
  - A `wake` from any thread while `IDLE` polls the task on that thread.
  - A `wake` while `POLLING` sets `NOTIFIED`, and the poller loops again.
  - So a step that completes synchronously never recurses, and the stack stays flat. This is
    Camel's `ReactiveExecutor` trampoline, done with a state word instead of a queue.
- **Allocation:** one allocation per exchange of an async route (the task), plus the boxes of
  section 4.
- **`spawn_route(route, exchange, on_done)`:** starts a task and polls it once on the calling
  thread. `on_done(Box<Exchange>)` runs on whichever thread finishes it. The HTTP consumer passes
  the responder.
- **`block_on(future)`:** for synchronous callers (a `ProducerTemplate`, `direct` called from a
  synchronous route, file and timer consumers). It polls inline. If the future is not ready, it
  parks on a one-shot slot (an atomic state with `park`/`unpark`, two futex calls, as the
  facade's design note recommends). It refuses to run on an event loop thread: that would
  deadlock the moment the reply is delivered to the same loop. Debug builds panic and name the
  step. Release builds submit to another loop and log once.
- **`offload().await`:** moves the task to the worker pool. On its first poll it pushes the task
  onto the pool and returns `Pending`, and a worker polls it from then on. This costs one hand-off,
  the same price as today, paid only by routes that need a worker.
- **Panics:** `catch_unwind` around each poll. A panic fails the exchange with a
  `CamelExchangeException` instead of killing an event loop.
- **Timers:** `sleep(d).await` for redelivery delays, `delay`/`throttle` with `asyncDelayed`, and
  deadlines. It uses the loop's timer when polled on a loop, otherwise one shared timer thread
  (a heap). This replaces `async_delay.rs`'s thread release:
  - no replacement consumer thread per waiting exchange;
  - no `MAX_RELEASED_THREADS = 1000` cap.

## 6. Where code runs

These placement rules decide whether the zero-hand-off case is reached:

1. **Start.** A request for an async-capable route starts inline on the server loop that parsed
   it. The existing `Governor` keeps its job, judged per route on the time of the first poll
   instead of the whole handler. A route whose first poll is slow goes back to the offload pool.
2. **Submit.** An asynchronous producer submits its request from the thread that polls it. When
   that thread is a loop of the same simoom-rt runtime, the client uses that loop (requirement S1).
   The connection's read, the completion and the continuation are then all on Li.
3. **Continue.** The continuation runs on the thread that completed the I/O.
4. **Never block a loop.** Before a step that returns `Maybe` (a user processor, `sql`, `file`, a
   synchronous container), an asynchronous pipeline polled on a loop thread awaits `offload()`.
   After that the task stays on the worker until its next suspension.
5. **Answer.** The responder, called on the loop that owns the connection, writes directly
   (requirement S2). Called from a worker, it posts as today.

Hand-offs per request with these rules:

| route | today | asynchronous |
|---|---:|---:|
| `platform-http → setHeader → http/grpc → respond` (proxy, gateway, MCP tool calling an API) | 2-3 | 0 |
| `platform-http → http → sql → respond` (a blocking step after the call) | 2-3 | 1 (the offload) to 2 (the answer from a worker) |
| `platform-http → bean (user code) → respond` | 1-2 | 1-2: unchanged, the step is `Maybe` |
| routes with no asynchronous step | unchanged | unchanged, same code path |

## 7. Porting the engine

The work splits into what blocks the hot path and what can follow. Each item is a separate
change with its own tests.

| phase | processors | notes |
|---|---|---|
| 0, prototype | executor; `RouteProcessor` (route entry: inflight, statistics, events, `run_on_completions` after the await); `Pipeline`; the `http` producer on simoom's `RtClient`; the `platform-http` consumer through `register_async` | the only goal is to measure section 1's predictions (section 9) |
| 1, the channel | `ErrorChannel` (the redelivery loop with `sleep().await`, `onException` processors); `InterceptProcessor`, `SendInterceptProcessor`; `RouteWrapper` (route policies, `interceptFrom`, `onCompletion`); `choice`, `filter`, `doTry`; `direct` (awaits the called route inline, as Camel's direct runs in the caller's thread); `Blocking::Never` on the computing steps | after this, ordinary routes with error handlers run asynchronously |
| 2, producers | `grpc` (`RtCall`); HTTP/1.1 through `RtClient`; `netty-http`; `kafka` (its in-flight requests become futures); `seda` InOut (a waker slot instead of the per-thread `REPLY_SLOT` condition variable); the MCP client; the model components (`openai`, `anthropic`, `gemini`, Bedrock, Vertex) and the `agent` step's tool loop | the thread-bound gains (section 1) are largest for model calls |
| 3, waiting EIPs | `delay`/`throttle` (`asyncDelayed`), redelivery delays (retire `async_delay.rs`); circuit breaker timeouts; `enrich`/`pollEnrich`; `multicast`, `recipientList`, `split` with `parallelProcessing` (a join over futures instead of a thread per part, `executorService` still honoured for `Maybe` steps); `wireTap` (spawn); `loop`; saga | |
| 4, consumers | `seda` and timer consumers starting async tasks, with `concurrentConsumers` kept as an in-flight limit; the MCP server's `tools/call` | |
| never | `transacted` routes (the transaction stack is a thread-local in `transaction.rs`, as Camel's are thread-bound); endpoints with `synchronous=true` | built on the synchronous path |

A container that has not been ported yet stays correct. It is `Maybe`, so the pipeline offloads
before it, and inside it its asynchronous children are run through `block_on` on a worker. It loses
the benefit for its subtree but nothing else.

## 8. Hazards and how each is handled

| hazard | handling |
|---|---|
| **No back-pressure.** Today the 200 offload threads are an implicit in-flight limit, and asynchronous routes remove it. | An explicit in-flight limit per async route (default 10,000, configurable), answering 503 beyond it, plus HTTP/2's stream limit per connection. Without it, overload turns into memory growth instead of latency. |
| Thread-locals that assume one thread per exchange | Audited list in `cameleer-core`: `transaction::STACK` (synchronous only, see above); `seda::REPLY_SLOT` (replaced on the async path); `async_delay::HOOK` (retired); `context::LAST_PRODUCER`, `message::ID_RANGE`, the jq/jsonpath caches, `stack` and `clock` are caches or per-thread counters and stay correct. Any new thread-local must be listed here. |
| Allocator | Exchanges now allocate on one thread and free on another. `alloc.rs` already serves remote frees through its shared regions; measure that its free path does not become a contention point (phase 0). |
| Statistics clock | `clock::ticks` readings taken on different cores: correct on invariant-TSC CPUs, which `clock.rs` already requires for its fast path. |
| Cancellation | Dropping a future cancels. Producers make their futures cancel-safe: drop resets the stream and frees the slot. The exchange's cancel token and deadline map onto a timer `select`, so the `ai-tool` timeout cancels the route without a watchdog. |
| Stack depth | A suspended route keeps no stack. Nested `async fn`s become nested state machines, boxed at each `dyn` boundary, so future sizes stay bounded. |
| Blocking a loop by mistake | `Blocking::Maybe` by default for anything unknown; the governor's timing per route; `block_on` refuses loop threads. |
| Ordering and semantics | Per-exchange order is kept (a pipeline awaits each step); Camel guarantees nothing across exchanges either. Error-handler decisions, `onCompletion` timing and events are unchanged, because the same code runs after the await. |

## 9. Verification

**Phase 0 gate (falsifiable).** Before phase 1, the prototype route
`platform-http → setHeader → http (HTTP/2) → respond` is measured three ways: today's build, the
facade (synchronous engine on `RtClient`), and the asynchronous path. It runs on WSL2 with the
guarded scripts of the cost-model round, then on droplets with the owner's approval. The model
predicts:

- client CPU per request: 36 µs (today), 24 µs (facade), about 5 µs (asynchronous);
- whole-machine CPU per request: lower by the 2-3 hand-offs removed, about 25-40 µs on WSL2;
- context switches per request: from about 3 to under 0.2;
- with a backend that sleeps 20 ms: throughput that is no longer capped at T / R.

If the measured saving is less than half the prediction, the model is missing a cost: profile
before porting more.

**Correctness.** Differential testing against the synchronous engine:

- a test-only context switch builds every route on the synchronous path;
- the route tests of `cameleer-core`, the YAML tests and the EIP tests then run in both modes and
  must give identical exchanges, events and error-handler decisions.

Each ported processor also gets tests of its own: a step that completes synchronously (no
recursion), a step completed from another thread, cancellation by drop, a panic in a step, and
redelivery with timer delays.

## 10. What simoom needs

simoom is a separate repository and its releases are the owner's decision. The design needs:

- **S1.** `RtClient` running on the loops of an existing simoom-rt runtime (the HTTP server's),
  with requests submitted on the current loop when the caller is one of them.
- **S2.** A responder that, when called on the loop that owns the connection, writes directly
  instead of posting.
- **S3.** A query for "is this thread a loop of runtime R" (`current_loop()`), used by `block_on`'s
  refusal and by the placement rules.
- **S4 (optional).** Timers usable from a task polled on a loop.
- **S5.** The blocking `Client` as a facade over `RtClient` (the event-driven client's design note,
  section "Should the blocking `Client` become a facade over it?"), for synchronous routes.

## 11. Decisions for the owner

1. The mechanism: std futures with Cameleer's own executor (recommended) or Camel-style callbacks.
2. Phase 0: build the prototype and measure it against the predictions above before any further
   engine change.
3. The simoom changes S1-S3, on simoom's own schedule.

## 12. Phase 0: built and measured (2026-11-19)

**What was built.**

- **Core:**
  - `cameleer_core::reactive`: the executor (tasks polled where they are woken, `block_on`, `offload`,
    `oneshot`, `enter_loop`).
  - `Processor::as_async` / `blocking` and `AsyncProcessor` in `spi.rs`.
  - Async forms of `Pipeline`, the route entry (`RouteProcessor`), `RouteWrapper` (not with
    `interceptFrom`), `ErrorChannel` (the first attempt is awaited; redeliveries and failure processors
    run on a worker), the management instrumentation and `to`.
  - `setHeader`/`setBody`/`transform` with constant, header, JsonPath, jq or Simple (without `bean:`)
    expressions report `Blocking::Never`.
- **HTTP:**
  - **`http` producer:** sends over simoom-client's event-loop client (`rt::RtClient`, already in the
    released 0.1.0, HTTP/1.1). It uses a process-wide runtime of half the CPUs
    (`CAMEL_HTTP_CLIENT_LOOPS`).
  - **Requests that stay on the blocking client:** streamed bodies, HTTP/2, proxies and chunked request
    coding. They move to a worker first.
  - **`platform-http` routes (and the `jetty`/`undertow` aliases):** routes that are async-capable answer
    from a task. Routes with `consumes`/`produces` or `disableStreamCache` stay synchronous.
- **The control:** `CAMEL_ASYNC_ROUTING=false` (or `reactive::set_enabled(false)`) builds every
  consumer on the synchronous path. It is used by the differential test and by the benchmark.

**Not built in phase 0:**
- S1-S3: the client still runs on its own loops, not the server's;
- the HTTP/2 event-loop client, which is not released;
- phases 1-4.

**Tests.**
- `crates/cameleer-core/src/reactive.rs`, 5 tests:
  - a step completed from another thread;
  - 100,000 synchronous completions on a 256 KiB stack (no recursion);
  - a panic fails the exchange;
  - a blocking step moves off the loop;
  - `block_on`.
- `crates/cameleer-http/tests/async_routing.rs`: the same routes with asynchronous routing on and off
  must give the same answers (a proxy, a handled `onException`, redelivery to success, a failure).
  The step after the HTTP call runs on the client's event loop. 40 requests to a 200 ms backend
  through a server with 2 offload threads finish in under 2.5 s; the synchronous path takes over
  3.5 s.
- Hermetic suite on Windows 11: **2977 passed, 0 failed**, zero warnings.

**Measured.** `platform-http → http → respond` (bridge), WSL2, three guarded rounds. Raw data and
method are in `benchmarks/results/2026-11-19-wsl-async-proxy/`. The Cameleer process is pinned to 4
CPUs, and CPU and switches are counted for that process only:

| backend, connections | path | req/s (3 rounds) | CPU µs/req | ctx switches/req | threads | p99 |
|---|---|---:|---:|---:|---:|---:|
| fast, 64 | synchronous | 35,377 / 33,973 / 34,346 | 61.8 / 62.3 / 62.1 | 1.01-1.05 | 69 | 4.4 ms |
| fast, 64 | asynchronous | 78,543 / 77,456 / 79,630 | 45.7 / 45.6 / 45.7 | 0.21-0.22 | 8 | 2.1-2.2 ms |
| fast, 256 | synchronous | 28,992 / 29,244 / 30,821 | 61.2 / 57.1 / 61.9 | 0.92-1.00 | 205 | 15.7-16.9 ms |
| fast, 256 | asynchronous | 86,424 / 88,743 / 88,858 | 37.0 / 40.9 / 41.8 | 0.07-0.08 | 8 | 7.2-8.0 ms |
| 20 ms, 256 | synchronous | 9,891 / 9,897 / 9,916 | 77-91 | 1.7-2.1 | 205 | 37.0-37.5 ms |
| 20 ms, 256 | asynchronous | 12,608 / 12,587 / 12,559 | 71-86 | 1.2-1.7 | 8 | 20.8-21.1 ms |

**Against the predictions of section 9:**

- **Context switches: met.** They fell from 1.0 to 0.07-0.22 per request. The prediction had counted
  about 3 hand-offs per request for the synchronous path; the measurement shows 1. Under load the
  server loops rarely park, so a post to them costs no switch. The worker that parks for every request
  is the one switch.
- **Thread bound: met, to the request.** The synchronous path with a 20 ms backend stops at 9,900
  req/s, which is the T/R = 200 / 0.020 s = 10,000 of section 1. The asynchronous path reaches
  12,600 = N/R = 256 / 0.0203 s: the closed-loop load generator, not Cameleer, is then the limit. Its
  p99 is the backend's 20.8 ms against 37 ms.
- **CPU per request: 16-20 µs saved (26-33%),** against 25-40 µs predicted for the whole machine.
  - That is above half the prediction (the gate) but at its lower end.
  - Part of the difference is billing. In the synchronous path the parked worker is woken by the
    backend's send, so that wake is billed to the backend process, which is not counted here. The
    asynchronous path pays its two cross-loop posts itself.
  - S1 and S2 (the client on the server's loops) remove those two posts. They are the next
    measurement.
- **Throughput: 2.2x at 64 connections and 2.9x at 256.** That is more than the CPU saving alone
  explains. The synchronous path was not CPU-bound: it kept 1.7-2.2 of 4 CPUs busy, held back by
  the hand-off latency. The asynchronous path uses 3.2-3.7 of them.

**Verdict.** The gate is passed. The model's mechanism is confirmed: hand-offs and the thread bound,
not the work. Its count of hand-offs was too high, as explained above.

**In-flight limit (done after phase 0).**
- **The option:** `maxInflightRequests` on `platform-http` (and its aliases). The default comes from
  `camel.server.maxInflightRequests`, else 10,000; `0` means no limit.
- **Beyond the limit:** an asynchronous route answers `503` with `Retry-After: 1` at once instead of
  queueing. The slot is released when the answer is built, or when the task is dropped unfinished.
- **Test:** `crates/cameleer-http/tests/async_inflight.rs` sends 20 requests at once to a 1 s
  backend. Exactly the limit are served and the rest refused, and the route serves again afterwards.
- **Not counted here:** the synchronous path, which is still bounded by the server's threads.

**Next steps:**
1. S1/S2 in simoom: the client on the server's loops, and a same-loop responder. The prediction: the
   remaining 0.07-0.2 switches go, and CPU per request falls by the two posts.
2. Phase 1.

## References

- Apache Camel, "Asynchronous Routing Engine" and the `AsyncProcessor`, `AsyncCallback`,
  `ReactiveExecutor` and `AsyncProcessorAwaitManager` APIs (camel.apache.org manual).
- J. Little, "A Proof for the Queuing Formula L = λW", Operations Research 9(3), 1961.
- M. Welsh, D. Culler, E. Brewer, "SEDA: An Architecture for Well-Conditioned, Scalable Internet
  Services", SOSP 2001: stages connected by queues, and admission control as the answer to overload
  once threads stop being the limit.
- P. Enberg, A. Rao, S. Tarkoma, "The Impact of Thread-Per-Core Architecture on Application Tail
  Latency", ANCS 2019: requests kept on the core that received them, no cross-core hand-off.
- D. Schmidt et al., "Leader/Followers", PLoP 2000; C. Li, C. Ding, K. Shen, "Quantifying the Cost
  of Context Switch", ExpCS 2007; N. Har'El et al., "Efficient and Scalable Paravirtual I/O
  System", USENIX ATC 2013: the costs behind section 1, as cited in http2-throughput.md.
