# Evaluation: pathscale's no_std async crates for Cameleer and simoom

_Internal design note, 2026-11-25, for the owner to decide on. Nothing was added to any
`Cargo.toml` of the workspace. All measurements ran in scratch crates outside the workspace on
one machine (below). Raw data, sources and scripts are kept outside the repository, in the session
notes folder `pathscale-eval/` (`bench/`, `tools/`, `tools/results.txt`, `tools/builds.txt`,
`summary.txt`)._

## 1. Question and answer

C. Bergström (pathscale) suggested his std-free, non-tokio ecosystem as a foundation for Cameleer:
`nagoya` (async runtime), `worktable` (embedded table storage), `ekostd` (OS layer),
`endpoint-libs` (WebSocket RPC and MCP), and the `ps-*` forks. The owner asked whether they give
enough real value to reconsider the std-only rule.

**Answer: no, not today.** The decisive facts are measured, not argued:

1. **None of the core crates builds on Windows.** `ekostd` says "no Windows" in its README, and a
   build fails with 589 errors. `nagoya`, `worktable`, `endpoint-libs`, `ps-reclaim` and
   `parking_lot_lite_hack` all depend on it. Cameleer must run on Linux, Windows and macOS.
2. **nagoya's timers do not work on x86_64.** In nagoya 0.1.22 to 0.1.27 (the current release),
   `nagoya::now_ns()` is always `0` on x86_64 Linux, so `sleep` never completes. Even the user
   guide's own timer example hangs. The clock moved to `eko::clock::now_ns` in 0.1.22, and ekostd
   documents that on any non-aarch64 target it returns zero. 0.1.21 works. `worktable 1.13.9`
   requires nagoya `^0.1.22`.
3. **A fresh resolve of `worktable 1.13.9` does not compile on Linux x86_64.** It pulls in
   `parking_lot_lite_hack_core 0.9.16` (2026-11-19), which uses `libc_lite::EFAULT`, and
   `ekostd 0.2.13` does not export that name on Linux. Pinning 0.9.15 works around it. worktable
   also needs `RUSTC_BOOTSTRAP = "arc_swap"` (unstable features on a stable compiler) in every
   workspace that builds it, directly or not.
4. **Where they work, they are not faster than what Cameleer has.**
   - **TCP echo:** simoom-rt does 1.48x nagoya's round trips at 64 connections and 1.66x at one
     connection, with 16% and 54% less server CPU per round trip.
   - **Timers:** simoom-rt uses 2.3x less CPU and has 1.9x lower p99 lateness than nagoya 0.1.21.
   - **Repositories:** Cameleer's in-memory repositories beat worktable in every operation at every
     size. The only ties are aggregation `get`, and a miss lookup at 100,000 keys. They are 4.5x to
     18.5x faster on a duplicate idempotent add.
5. **The supply-chain cost is large.**
   - `worktable` brings 95 crates (80 third-party, 14 proc-macro crates, 10 build scripts), 837k
     lines and 7,283 `unsafe` keywords.
   - `nagoya` with its reactor brings 13 crates (10 third-party) and 1,860 `unsafe`.
   - Cameleer's whole workspace has 111 `unsafe`. simoom has 299, all in `simoom-sys`, the one crate
     allowed to call the OS.

| crate | verdict | one-line reason |
|---|---|---|
| nagoya | **not useful** (borrow two ideas) | no Windows; timers broken on x86_64; slower than simoom-rt and against Cameleer's run-on-the-waking-thread design |
| worktable | **not useful** | no Windows; does not build fresh; slower than the repositories it would replace; persistence is best-effort, not durable |
| ekostd | **not useful** | no Windows by design; zero clock on x86_64; simoom-sys already does this job |
| endpoint-libs | **not useful** | duplicates Cameleer's MCP server (conformance 39/39, measured) over a WebSocket RPC Cameleer does not use; MIT; 46 crates |
| ps-st3 | **not needed** (section 9) | builds on Windows, no deps, has Miri CI; no Cameleer pool has fork-join work to steal |
| ps-regexr | **borrow ideas** (section 9) | the comparison exposed a quadratic copy in Cameleer's regex (64 KiB `split` 73 ms against 0.34 ms); fix std-only, then a literal prefilter |
| jsonmax | **revisit when JSON shows in a profile** (section 9) | 1.8x faster parse and serialize at 64 KiB, slower on small documents |
| ps-rusty_alloc, ps-reclaim, parking_lot_lite_hack, ps-simdutf8, ps-spsc | **not useful** | three of them fail on Windows; the rest duplicate Cameleer's own `alloc`, std locks and UTF-8 |

The std-only rule does not need to change for any of these. Section 7 explains what changing it
would cost.

## 2. Method

- **Machine.** Windows 11 Pro host, 12 logical CPUs, 16 GB, with WSL2 Ubuntu 24.04 (kernel
  6.18.58.1, 12 vCPUs, 5.9 GB). The host was shared with other sessions (another `rustc`, Python
  jobs). WSL2 stalled once under host memory pressure (0.6 GB free); every run below happened
  after it recovered. Spreads are reported: they are wide on a shared desktop, so trust ratios over
  absolute numbers.
- **Toolchain.** Rust 1.100.0 stable on both sides. Release profile, no debug info.
- **Pinned versions.** `nagoya =0.1.27` (and `=0.1.21` for timers, see section 5.2),
  `worktable =1.13.9`, `ekostd =0.2.13`, `endpoint-libs =4.3.0`, `simoom-rt =0.2.0`, and
  `cameleer-core` from this worktree (path dependency, `default-features = false`).
- **Placement.** Servers were pinned to CPUs 0-3 (`taskset`) with 4 workers or loops each. The
  load client was pinned to CPUs 4-11. Rounds alternate the two sides.
- **Due diligence sources.** crates.io API (versions, owners, dependencies), the GitHub API
  (commits, contributors, CI files), the published sources in `~/.cargo/registry`, `cargo tree`
  for Linux x86_64, and `cargo build` on Windows.
- **Counting.** The `unsafe` counts are keyword occurrences in `.rs` files, with `//` comments
  removed and `tests/`, `benches/` and `examples/` excluded. Cameleer and simoom are counted the
  same way. Line counts are physical lines of the same files.

## 3. Due diligence per crate

### 3.1 Summary table

| | nagoya 0.1.27 | worktable 1.13.9 | ekostd 0.2.13 | endpoint-libs 4.3.0 |
|---|---|---|---|---|
| what | async executor (work-stealing, ps-st3) + opt-in epoll/kqueue reactor + timers | typed in-memory tables from a `worktable!` macro, optional disk/S3 persistence | one OS API (files, sockets, threads, epoll, clock) over libc or libc-rs, crate name `eko` | WebSocket RPC server; every endpoint also an MCP tool |
| licence | MIT OR Apache-2.0 | **MIT** | MIT OR Apache-2.0 on crates.io (no licence file detected on GitHub) | **MIT** |
| first release | 2026-09-09 | 2024-09-06 | 2026-09-08 | 2024-10-28 |
| versions | 24 in 72 days; 6 between 11-18 and 11-20 | 105; 8 in November 2026 | 12 in 73 days | 80; majors 1 to 4 in 13 months |
| crates.io owners | pathscale | pathscale, Handy-caT | pathscale | pathscale, dVeon-loch, Handy-caT |
| commits | about 167, last 100 all by one author name ("meh") | about 816; Handy-caT 178, 10 others | about 214, one author | about 359; dVeon-loch 152 |
| stars / forks | 1 / 0 | 2 / 1 | 1 / 0 | 2 / 4 |
| MSRV / edition | 1.85 / 2024 | none stated / 2024, **needs `RUSTC_BOOTSTRAP=arc_swap`** | none / 2021 | none / 2024 |
| Linux x86_64 | builds; **timers never fire** (0.1.22+) | **fresh resolve fails** (`parking_lot_lite_hack_core 0.9.16`); builds with 0.9.15 pinned | builds; `clock::now_ns` returns 0 | builds |
| macOS | stated (kqueue); not tested here | through ekostd; not tested | stated (aarch64-apple-darwin) | not tested |
| **Windows** | **fails** (ekostd) | **fails** (ekostd) | **fails, by design** ("no Windows") | **fails** (ekostd) |
| CI | Linux only (ubicloud): `check --no-default-features`, fmt, `test --all-features`, clippy | Linux (`rust.yml`) | no workflow | Linux (`rust.yml`) |
| tests (`#[test]` attributes in the package) | 150 | 993 | 16 | 200 |
| fuzzing | none | none | none | none |
| Miri | no | mentioned in two files | no | no |
| `unsafe` in the crate itself (lines) / `SAFETY` comments | 180 / 95 | 184 / 82 | 762 / 25 | 22 / 4 |
| docs | Typst guides (user guide, design), docs.rs | Typst guides, durability contract | README | README, docs/, CHANGELOG |
| semver | 0.x, patch-level API additions | 1.x, but behaviour changes in minors (e.g. default index backend in 1.9) | 0.x | 4.x, three majors in 13 months |

### 3.2 Transitive trees (Linux x86_64, normal and build edges)

| root | crates | third-party crates | lines of source | `unsafe` | proc-macro crates | build scripts |
|---|---:|---:|---:|---:|---:|---:|
| nagoya (default features, no_std core) | 5 | 2 (async-task, spin) | 41,355 | 1,163 | 0 | 0 |
| nagoya (`std`, `reactor`) | 13 | 10 (async-task, bytes, futures-core/-io/-task/-util, memchr, pin-project-lite, slab, spin) | 97,768 | 1,860 | 0 | 0 |
| ekostd | 1 | 0 | 14,881 | 761 | 0 | 0 |
| worktable | 95 | 80 (rkyv, tracing, futures, uuid, hashbrown, derive_more, syn, ...) | 836,796 | 7,283 | 14 | 10 |
| endpoint-libs (default) | 46 | 42 (serde, serde_json, chrono, futures, tracing, ...) | 386,659 | 2,447 | 2 | 7 |
| *for comparison:* Cameleer workspace | 23 workspace crates + simoom | 0 | 348,881 | 111 | 0 | 0 |
| *for comparison:* simoom 0.2.0 (11 crates) | 11 | 0 | 89,096 | 299, all in `simoom-sys` | 0 | 0 |

**None of these trees is std-only in the family sense.** nagoya and ekostd are mostly first-party
(pathscale), but nagoya with a reactor already pulls in the futures family. worktable and
endpoint-libs are ordinary crates.io dependency trees.

### 3.3 The ps- crates that could matter

| crate | what | Windows | tree | relevance |
|---|---|---|---|---|
| ps-st3 0.6.4 | fork of `st3`, a work-stealing LIFO deque | builds | 1 crate, 60 `unsafe`, Miri CI | nagoya's scheduler. Cameleer has no work-stealing pool, and section 4.1 argues it does not need one |
| ps-regexr 0.6.0 | fork of `regexr`, JIT and SIMD regex | builds | 3 crates (aho-corasick, memchr), 681 `unsafe` | Cameleer has its own `regex.rs`. Ideas only, and only if regex shows up in a profile |
| jsonmax 0.1.1 | JSON parse, query, schema, derive | builds | 5 crates (bytes, cpufeatures, rustversion), 540 `unsafe` | Cameleer has its own JSON and JSON Schema 2020-12. Ideas only |
| ps-rusty_alloc 2.4.1 | Rust remake of mimalloc | **fails** | 2 crates, 1,522 `unsafe` | Cameleer has `cameleer_core::alloc` (thread-caching, measured) |
| ps-reclaim 0.1.9 | grace-period reclamation | **fails** (ekostd) | 8 crates, 1,368 `unsafe` | nothing to replace |
| parking_lot_lite_hack 0.12.10 | parking_lot without std | **fails** (ekostd) | 7 crates, 1,339 `unsafe` | std `Mutex` is futex-based already |
| ps-simdutf8, ps-spsc | SIMD UTF-8 check, bounded SPSC queue | not tested / builds | small | not on a measured hot path |
| nago-http 0.2.3 | HTTP/1.1 client over nagoya and rustls | not tested | 78 crates (rustls, RustCrypto) | simoom-client already does this std-only, with HTTP/2 |

The ps- forks of rendering and media crates (blitz, vello, boa, jxl, h264, opus, str0m) have no
place in an integration runtime.

### 3.4 How this code is produced

The repositories carry `CLAUDE.md` and `AGENTS.md` files (WorkTable, endpoint-libs). The release
pace (six nagoya releases in three days, `ekostd` being one author's "janky stuff ... tailored to
his needs") and the defects in sections 1 and 5 suggest code that moves faster than its own CI
covers. This is not a criticism of the approach: Cameleer is written the same way. But it means
Cameleer would inherit the review burden of every release, not just of one audited version.

## 4. Fit, mapped to Cameleer

### 4.1 nagoya as the runtime under asynchronous routing (against simoom-rt)

| | simoom-rt 0.2.0 (today) | nagoya 0.1.27 |
|---|---|---|
| I/O model | completion style. epoll or io_uring on Linux, **IOCP on Windows**, kqueue on macOS. The driver reads into pooled buffers. Readiness is never exposed | readiness style. epoll on Linux, kqueue on macOS and BSD, **nothing on Windows**. One shared reactor thread (`Runtime::readiness`), or `Reactor::sharded(n)` |
| threads | one loop per core, shared-nothing; each loop owns its sockets, buffer pool and timer wheel | N workers with work stealing (ps-st3) plus a reactor thread. A ready socket wakes a task, and the task runs on whichever worker takes it |
| timers | hierarchical wheel per loop (4×64 slots, 1 ms tick, O(1) insert and cancel) | one global binary heap behind a spin mutex, driven by the reactor or a dedicated thread |
| wakers | not needed: callbacks run on the loop | `async-task` wakers; a wake submits the task to the pool |
| blocking work | bounded offload pool with CoDel admission | `submit` of closures on the same pool, plus `par_for` |
| no_std | no (std plus `simoom-sys` externs) | yes, for the executor core. The reactor needs `std` |
| `unsafe` | `#![forbid(unsafe_code)]` in simoom-rt; 299 in simoom-sys | 180 in nagoya, 762 in ekostd, and futures-util |

Cameleer's asynchronous routing (`crates/cameleer-core/src/reactive.rs`,
`docs/design/async-routing.md`) runs an exchange as a `std::future::Future` **on the thread that
wakes it**: the event loop that read the reply polls the route on. That design removed the queue,
the thread wake and the system call per completion. It is where the measured gains came from:
0.00 to 0.03 context switches per request, 2.2x to 2.9x requests per second.

A work-stealing executor puts back exactly the hand-off this design removed. The reactor thread
sees readiness and submits the task to a worker. The echo benchmark shows the cost, at one
connection, where it is not hidden by batching: nagoya spends 66 µs of server CPU per round trip
against simoom-rt's 31 µs (section 5.1). Work stealing helps unbalanced CPU-bound tasks.
Integration routes are I/O-bound, and the governor already moves CPU-heavy routes to the pool.

Adopting nagoya would also mean:

- dropping Windows;
- a second reactor beside simoom's (the HTTP, gRPC and MCP servers run on simoom-rt);
- moving timers and the governor off the loops.

**Ideas considered for borrowing:**

- the `Sharded` reactor handle, for components outside simoom that need readiness (the old
  thread-per-connection clients: WebSocket, the cloud components' HTTP/1.1). Section 9 found it
  is not needed: simoom-rt already places a socket on a chosen loop;
- the "timers live beside the pool, a program that never sleeps pays nothing" design note, which
  is how Cameleer's per-name timer threads already behave.

### 4.2 A no_std core for edge gateways

`cameleer-core` has 138 files and 61k lines. 127 of the files use `std::`:

- `std::sync`: 111 files
- `std::time`: 56
- `std::collections`: 52 (movable to `alloc`)
- `std::thread`: 30
- `std::io`: 9
- `std::fs`: 8
- `std::env`: 8
- `std::process`: 5

Threads, clocks, files and environment variables are the substance of an integration runtime:

- timers and the seda queues;
- the file component;
- property placeholders from the environment;
- the `exec` component;
- the thread-caching allocator.

A no_std core would mean a host abstraction for all of these, which is what ekostd is. It would be
a rewrite of the context, the executor and most core components.

The target that would justify it does not exist in Cameleer's market:

- Edge gateways in `docs/guides/edge.md` are arm64 Linux boards (Raspberry Pi 3 to 5, Jetson,
  Graviton). There a route uses about 10 MB of memory and the `core` image is 12.5 MB.
- Microcontrollers without an OS do not run MQTT-to-Kafka bridges with Simple expressions, YAML
  routes and TLS 1.3.
- The unikernel target ekostd serves (`aarch64-unknown-none` through libc-rs) is pathscale's own
  compiler bootstrap, not a gateway product.

Making `cameleer-core` no_std costs a rewrite and buys nothing a customer has asked for. No device
or market was found that changes this.

### 4.3 worktable for the aggregation, idempotent and claim-check repositories

What Cameleer has today:

- **`MemoryIdempotentRepository`** (`processor/errors/idempotent.rs`): `HashMap` plus `BTreeMap`
  LRU, capacity-bounded like Camel's. A simpler FIFO variant is used by the file consumer
  (`component/file.rs`).
- **`MemoryAggregationRepository`** (`processor/routing/aggregate.rs`): `HashMap` with a "hot"
  slot for the newest group and in-place `with_slot` aggregation. Exchanges are moved, not copied.
- **Claim check:** in the exchange properties (`claim_check.rs`).
- **Durable:** `JdbcIdempotentRepository`, `JdbcAggregationRepository` (PostgreSQL) and the Redis
  repositories.

worktable gives typed tables with indexes and optional persistence. It falls short of the
repositories on each point:

- It has no eviction, so it cannot be Camel's capacity-bounded `MemoryIdempotentRepository`
  without extra code.
- It stores rows, not `Exchange` values. An aggregation repository would serialise every exchange
  (rkyv), where the memory repository moves it.
- Its persistence is documented as "best-effort, not crash-atomic". A successful insert means
  "queued", and `SIGKILL` can lose acknowledged rows. The JDBC repository's guarantee (a committed
  row) is the reason a user picks a persistent idempotent repository at all. A store that can lose
  acknowledged keys re-delivers duplicates after a crash, which is the failure the EIP exists to
  prevent.
- Every operation is `async`, and a duplicate insert returns an error, which is the slow path
  (section 5.3).

The idea worth keeping is secondary indexes on a repository (e.g. "groups older than T" without a
scan). An earlier version of this note said the aggregator's completion timeout already keeps its
own deadline structure. That was wrong: it is a `HashMap<key, Instant>` scanned in full. Section 9
measures what a deadline index gains.

### 4.4 Proc macros for a typed route DSL

worktable's `worktable!` macro (and `endpoint-gen`'s RON schemas) show how a macro can turn a
declaration into typed code. For Cameleer:

- **What exists.** The Rust DSL is a builder (`crates/cameleer-core/src/dsl/`). Its names mirror
  Camel's Java DSL, so Camel users and the docs translate one to one. Most users write YAML, which
  `cameleer validate` and `cameleer schema` check without compiling anything.
- **What a macro would add.** Compile-time checks of endpoint URIs and header names in Rust
  routes. The cost is a proc-macro crate, and with it `syn`, `quote` and `proc-macro2`, unless
  Cameleer writes its own token parser on `proc_macro` (std-only is possible, since `proc_macro`
  ships with the compiler).
- **Conclusion.** This is independent of pathscale's crates, and nothing in them would be reused.
  If wanted, it is a separate design. The checks `validate` already runs at load time cover most of
  the value.

### 4.5 endpoint-libs

endpoint-libs serves WebSocket RPC endpoints described in RON and exposes each as an MCP tool.
Cameleer already has:

- an MCP server in both protocol eras, with official conformance 39/39 (2025-11-25) and 26/26
  (2025-06-18);
- `--mcp-expose`, which serves existing routes as tools without route edits;
- a measured 2.73x Camel's MCP session throughput.

endpoint-libs would add a second MCP implementation, under MIT, with 46 crates. It is not useful.

## 5. Measurements

### 5.1 TCP echo: nagoya 0.1.27 against simoom-rt 0.2.0 (WSL2)

- **Workload.** Closed loop, 64-byte messages, one client thread per connection (std blocking
  sockets, `TCP_NODELAY`), 10 s per run, 5 alternating rounds.
- **Servers.** 4 workers (nagoya: `Runtime::new(4)` plus the shared reactor, one task per
  connection) or 4 loops (simoom-rt: `Connection::on_data` echoing into a pooled buffer).
- **nodelay.** nagoya's `TcpStream` has no `set_nodelay`. Ping-pong traffic does not trigger Nagle
  (each reply is acknowledged by the next request), so this does not bias the result.
- **Server CPU** is read from `/proc/PID/stat` around the timed window.

Median of 5 [min-max]:

| conns | side | round trips/s | p50 | p99 | server CPU per round trip | threads | peak RSS |
|---:|---|---:|---:|---:|---:|---:|---:|
| 1 | simoom-rt | **18,418** [14,191-19,232] | 47.9 µs | 105.0 µs | **30.7 µs** [29.5-39.0] | 5 | 2.6 MB |
| 1 | nagoya | 11,125 [8,195-11,602] | 82.5 µs | 210.8 µs | 66.4 µs [63.9-88.0] | 6 | 2.3 MB |
| 64 | simoom-rt | **194,774** [111,798-215,373] | 172.3 µs | 2,974 µs | **20.8 µs** [19.5-34.4] | 5 | 3.1 MB |
| 64 | nagoya | 131,569 [47,688-140,893] | 366.2 µs | 2,478 µs | 24.8 µs [24.1-50.5] | 6 | 2.7 MB |

**Ratios (simoom-rt ÷ nagoya):**

| conns | throughput | p50 latency | server CPU per round trip | p99 |
|---:|---:|---:|---:|---|
| 1 | 1.66x | 1.7x lower | 2.2x less | 2.0x lower |
| 64 | 1.48x | 2.1x lower | 1.19x less | nagoya's is 17% lower, within the wide spread of a shared host |

nagoya's memory is 0.3 to 0.4 MB lower.

### 5.2 Timers: nagoya against simoom-rt (WSL2)

**nagoya 0.1.22 to 0.1.27 could not be measured.** Its timers never fire on x86_64. The
reproduction is nagoya's own user-guide example:

```rust
nagoya::block_on(async { nagoya::sleep(Duration::from_millis(1)).await; });
```

It hangs with features `std` and with `std, reactor`, on 0.1.22, 0.1.24, 0.1.26 and 0.1.27. It
completes on 0.1.15, 0.1.20 and 0.1.21. A probe prints `nagoya::now_ns` reading 0 before and after
a 50 ms sleep, and `sleep(1ms).deadline() = 1000000`, so the deadline is never reached. A
`strace` was not possible (not installed). The cause is in the source:

- nagoya 0.1.22 `time.rs` replaced its default clock with `set_clock(eko::clock::now_ns)`;
- ekostd `clock.rs` says "Off aarch64 there is no counter here and `now_ns` returns zero".

nagoya's CI runs `cargo test --all-features` on Linux x86_64 runners and does not catch it.

**The comparison therefore uses nagoya 0.1.21**, the last working release, with the same API.

- **Workload.** 10,000 entities, each arming 20 sequential timers of 1-10 ms (xorshift, the same
  sequence on both sides): 200,000 timer fires. Lateness is the time after the deadline at which
  the entity runs.
- **Both sides.** 4 workers or loops on CPUs 0-3, started together after a common start instant,
  with wall and CPU measured from that instant.
- **Entities.** nagoya: 10,000 tasks calling `sleep`. simoom-rt: 10,000 idle TCP connections,
  each arming `Ctx::set_timer`, which is how simoom gives timers to request deadlines and idle
  timeouts.

Median of 5 [min-max]:

| side | wall | process CPU | lateness p50 | p99 | max | threads | peak RSS |
|---|---:|---:|---:|---:|---:|---:|---:|
| simoom-rt 0.2.0 | 0.184 s [0.183-0.186] | **0.09 s** [0.04-0.12] | 1,129 µs | **2,077 µs** | 2,399 µs | 5 | 8.5 MB (holds 20,000 sockets) |
| nagoya 0.1.21 | 0.186 s [0.185-0.243] | 0.21 s [0.17-0.42] | 1,178 µs | 3,881 µs | 5,603 µs | 6 | 7.6 MB |

- Wall time is set by the sleeps themselves (20 × 5.5 ms on average).
- simoom-rt uses 2.3x less CPU and has 1.9x lower p99 lateness.
- Both have a p50 lateness of about 1.1 ms, which matches simoom's 1 ms wheel tick and nagoya's
  heap granularity.

A first version of this benchmark measured simoom-rt while its 10,000 connections were still being
accepted. Accepts competed with timers, and simoom-rt looked 4x slower. That run is discarded;
only the corrected one is reported.

### 5.3 Repositories: worktable 1.13.9 against Cameleer's in-memory repositories (WSL2)

**Setup.**

- Both sides are pinned to one CPU.
- Keys are `msg-00000000`-style strings. Lookups follow one fixed shuffle and misses use other
  keys, the same on both sides.
- Each value is the median ns/op of 7 rounds, and the table gives the median of 2 runs.
- Cameleer is the real code: `MemoryIdempotentRepository::new(n)`, so it never evicts, as
  worktable never does, and `MemoryAggregationRepository` with `Exchange::with_body(1 KiB)`.
- worktable is `worktable!` tables `{key: String primary_key}` and `{key: String primary_key, body: String}`.
  Its async calls run under one `nagoya::block_on` per phase. "add-duplicate" is an `insert` that
  must be refused.

| n | operation | Cameleer ns/op | worktable ns/op | worktable ÷ Cameleer |
|---:|---|---:|---:|---:|
| 1,000 | idempotent add | 110.5 | 345.2 | 3.1x slower |
| 1,000 | contains (hit) | 22.6 | 183.9 | 8.1x |
| 1,000 | contains (miss) | 21.5 | 70.3 | 3.3x |
| 1,000 | add duplicate | 131.7 | 2,442.6 | 18.5x |
| 1,000 | aggregation add 1 KiB | 297.6 | 587.2 | 2.0x |
| 1,000 | aggregation get | 235.2 | 237.5 | 1.0x |
| 1,000 | aggregation remove | 97.2 | 1,326.0 | 13.6x |
| 10,000 | idempotent add | 122.4 | 389.3 | 3.2x |
| 10,000 | contains (hit) | 38.9 | 211.7 | 5.4x |
| 10,000 | contains (miss) | 77.7 | 107.7 | 1.4x |
| 10,000 | add duplicate | 194.4 | 2,693.2 | 13.9x |
| 10,000 | aggregation add 1 KiB | 515.4 | 963.2 | 1.9x |
| 10,000 | aggregation get | 514.7 | 585.0 | 1.1x |
| 10,000 | aggregation remove | 293.6 | 2,566.3 | 8.7x |
| 100,000 | idempotent add | 177.2 | 425.4 | 2.4x |
| 100,000 | contains (hit) | 210.2 | 587.5 | 2.8x |
| 100,000 | contains (miss) | 119.3 | 144.6 | 1.2x |
| 100,000 | add duplicate | 704.5 | 3,145.7 | 4.5x |
| 100,000 | aggregation add 1 KiB | 767.0 | 1,248.2 | 1.6x |
| 100,000 | aggregation get | 1,113.1 | 1,167.5 | 1.05x |
| 100,000 | aggregation remove | 949.4 | 4,864.6 | 5.1x |

Spreads (min-max of the two runs) are mostly within ±20% and at most about ±30%. The widest
rows are worktable's contains-miss at 1,000 (48.9-91.8 ns) and Cameleer's aggregation add at
100,000 (595-939 ns). The two sides' ranges overlap only for aggregation `get` (all sizes) and contains-miss at 100,000;
those rows are ties. Every range is in
`pathscale-eval/summary.txt`.

worktable's "nanosecond-scale queries" claim did not show at the sizes routes use. Its versioned
row publication and paged storage cost more than a `HashMap` behind a mutex. Not measured:

- worktable's persisted mode against `JdbcIdempotentRepository`, which needs PostgreSQL. The
  durability difference in section 4.3 makes that comparison moot.
- Concurrent access from several threads, where worktable's lock-free indexes could do better than
  one mutex. Cameleer's repositories are called once per exchange, and the aggregator takes a
  per-key slot, so contention was never seen in a profile.

### 5.4 Build cost (WSL2)

- Clean `cargo build --release` of each scratch crate, 3 passes; the median is reported.
- Each crate is a small `main` using the library; the size delta over a hello-world (444,936 B)
  approximates what the library adds.

| crate | depends on | crates in graph | clean build (median of 3) [range] | binary | over hello |
|---|---|---:|---:|---:|---:|
| sim | simoom-rt 0.2.0 | 3 | 2.5 s [2.2-46.6] | 910,880 B | +466 KB |
| nag | nagoya 0.1.27 (`std, reactor`) | 14 | 3.9 s [3.5-5.8] | 721,976 B | +277 KB |
| nag21 | nagoya 0.1.21 | 15 | 3.8 s [3.4-5.1] | 762,064 B | +317 KB |
| wt | worktable 1.13.9 + nagoya | 96 | **39.6 s** [36.0-64.9] | 1,287,208 B | +842 KB |
| cam | cameleer-core (all of it, 61k lines) | 2 | 23.0 s [21.1-26.9] | 986,136 B | +541 KB |

- nagoya is smaller than simoom-rt here. simoom-rt carries the full driver set: io_uring, the
  buffer pools and UDP/GSO for QUIC.
- worktable alone takes 1.7x longer to build than all of `cameleer-core`, and its binary is larger.
- The first pass of each column includes downloads and the host's load spikes (the 46.6 s and
  64.9 s maxima).

### 5.5 Windows

Windows was measured by building, since nothing in the core set compiles there:

- **fails:** `ekostd`, `nagoya` (default, `std`, `reactor`), `worktable`, `endpoint-libs`,
  `ps-reclaim` and `parking_lot_lite_hack` (589 errors, all in ekostd's `libc_lite`), and
  `ps-rusty_alloc` (2 errors of its own);
- **builds:** `ps-st3`, `ps-spsc`, `ps-regexr` and `jsonmax`.

No runtime comparison on Windows is possible. simoom-rt runs there on IOCP (its own CI and
Cameleer's Windows test suite).

## 6. Conclusion per crate

- **nagoya: not useful.**
  - Windows is impossible, and its x86_64 timers are broken in every release since 0.1.22.
  - Where it works it is slower than simoom-rt: 1.5-1.7x fewer echo round trips, 2.2x the CPU per
    round trip at one connection, 2.3x the CPU for timers.
  - Its work-stealing model reintroduces the hand-off Cameleer's asynchronous routing removed.
  - Borrow: the `Sharded` reactor handle, for clients outside simoom.
- **worktable: not useful.**
  - It does not build on Windows, does not build from a fresh resolve on Linux, and needs
    `RUSTC_BOOTSTRAP`.
  - It is slower than the repositories it would replace, at every size.
  - Its persistence cannot back an idempotent repository.
  - It costs 95 crates and 40 s of build.
  - Borrow: secondary indexes on repositories, if a use appears.
- **ekostd: not useful.** No Windows by design, and no clock on x86_64. Cameleer's OS layer is
  std plus `simoom-sys`.
- **endpoint-libs: not useful.** It duplicates the MCP server Cameleer already ships, with a
  larger tree, and it is MIT.
- **ps-st3: not needed. ps-regexr: borrow ideas. jsonmax: revisit when JSON shows in a
  profile.** Measured in section 9.
- **The other ps- crates: not useful** for an integration runtime.

## 7. What changing the std-only rule would cost and give

**What it would give.** Measured, nothing. No crate in the set is faster than the code it would
replace or adds a feature Cameleer lacks. The no_std edge story has no target device. The one
plausible gain, less code to maintain, is outweighed by importing code that is younger, moves
faster and is less tested on Cameleer's platforms.

**What it would cost:**

- **The claim.** The site says "No third-party crates: every protocol is written on the Rust
  standard library", and `scripts/sbom.py` enforces it in CI.
  - With nagoya: 10 third-party crates.
  - With worktable: 80 third-party crates, 14 proc-macro crates and 10 build scripts that run
    arbitrary code at build time.
  - The SBOM would go from first-party only to a normal crates.io graph. That is the opposite of
    what the Cameleering positioning sells ("one audit scope", `cargo build --offline`).
- **`unsafe`.**
  - Today: 111 occurrences in the workspace, plus 299 in `simoom-sys`, which is confined by design.
  - With nagoya: about 1,860 more.
  - With worktable: about 7,300 more.
  - Most of it has no Miri or fuzzing coverage.
- **Platforms.** Windows support, which Cameleer has had since 2026-11-03 with the full hermetic
  suite green, would be lost.
- **Toolchain.** worktable's `RUSTC_BOOTSTRAP` would leak into every user's workspace.
  `cargo install cameleer-main` would need it too, or fail.
- **Licence.** worktable and endpoint-libs are MIT only. That is compatible with Apache-2.0, but
  it breaks the family rule of one licence (`docs/design/family-crates-std-only.md`).
- **Stability.**
  - 24 nagoya releases in 72 days.
  - A worktable release that breaks the build within six days of a dependency release.
  - A clock regression unnoticed for four releases.
  - Cameleer's release gate ("no release if a published number degrades") would depend on someone
    else's release cadence.

If pathscale fixes Windows and the clock, the timer and echo comparison can be rerun with the same
harness (`pathscale-eval/tools/run.sh`). The verdict on worktable and endpoint-libs does not depend
on those fixes.

## 8. Findings worth reporting upstream

Not reported. Filing issues on another project is an outward action for the owner to decide on.

1. **nagoya 0.1.22+:** `sleep` never completes on x86_64. `now_ns()` is `eko::clock::now_ns`,
   which is 0 off aarch64. Reproduction in section 5.2.
2. **worktable 1.13.9:** a fresh resolve fails on Linux x86_64.
   `parking_lot_lite_hack_core 0.9.16` uses `eko::libc_lite::EFAULT`, which `ekostd 0.2.13` exports
   only in its libc-rs stub.
3. **nagoya:** `net::TcpStream` has no `set_nodelay` (the underlying `TcpSocket` does).

## 9. Follow-up: the ideas worth borrowing (2026-11-25)

A second pass checked each idea against Cameleer's code.

- **Code measured.** Copies of `cameleer-core` and `cameleer-tls` taken at the commit that added
  this note, in scratch crates.
- **Machine.** Ryzen 5 5600X, native Windows 11 and WSL2 Ubuntu 24.04.
- **Rounds.** Median [min-max] of 7 rounds (5 for the aggregator), sides alternated, runs pinned.
- **Kept outside the repository.** Write-up, sources, the two prototype patches (`.diff`) and raw
  results are in the session notes folder, `pathscale-eval/borrow-ideas.md` and
  `pathscale-eval/borrow/`.
- **Not landed.** Nothing below has been applied to `main`. The patches are prototypes.

Code locations in this section were rechecked against the source.

### 9.1 nagoya's sharded reactor: not needed

simoom-rt already offers the same placement, with IOCP on Windows and without nagoya's hand-off
to a worker:

- `RuntimeHandle::loops()`
- `LoopHandle::run`
- `LoopCtx::adopt`

What is still thread-per-connection outside simoom:

| component | threads |
|---|---|
| the protocol `TcpServer`s (websocket, syslog, mllp consumers) | 1 accept thread plus 1 per connection: 2, 65 and 257 threads measured at 1, 64 and 256 connections |
| the netty TCP consumer | 1 per connection |
| the DICOM SCP | 1 per connection |
| the thrift server | 1 per connection |
| the MQTT client | 2 per connection |
| the AMQP, STOMP and AMQP 1.0 clients | 1 to 2 per connection |

These belong on simoom-rt, not on a second reactor.

Two cheap fixes found on the way:

- **Accept loops poll.** The three copies of the accept loop sleep 5 ms between polls:
  - `cameleer-tls/src/net/endpoint.rs:236`
  - `cameleer-amqp/src/common.rs:205`
  - `cameleer-kafka-mqtt/src/net.rs:164`

  That is about 5 ms of CPU per second for every idle server on Linux.
- **AMQP 1.0 heartbeat.** The watchdog wakes every 20 ms per connection
  (`cameleer-amqp10/src/transport.rs`).

The cloud components' HTTP client is the most worthwhile move to simoom-client. The reason is not
threads: it sends `Connection: close` on every request (`cameleer-cloud/src/http.rs`, module
docs), so every cloud call pays a TCP connect and a full TLS handshake. Not measured.

### 9.2 Secondary index: adopt, std-only, in the aggregator

The completion timeout keeps a `HashMap<key, Instant>` (`processor/routing/aggregate.rs`).

- Every exchange that leaves its group open inserts a deadline and calls `notify_all`.
- The checker thread wakes and scans every deadline, under the mutex the route also takes.

The prototype adds a `BTreeMap` ordered by deadline. It wakes the checker only when the new
deadline becomes the earliest. It passes `routing_aggregate` (19 tests) and `conformance_eip`
(48 tests).

Linux, ns per message, one sender:

| open groups | before | after | without a timeout (floor) | gain |
|---:|---:|---:|---:|---:|
| 10 | 2,339 | 806 | 595 | 2.9x |
| 1,000 | 5,823 | 998 | 643 | 5.8x |
| 10,000 | 18,650 | 1,296 | 979 | 14.4x |

- Expiring 30,000 groups took 483 ms of CPU before and 136 ms after.
- On Windows the pinned rounds point the same way but were noisy on the shared host. An unpinned
  run gave 10.8x at 10,000 groups.

Other findings, not measured:

- The stream resequencer has the same wake-and-scan pattern, bounded by its capacity.
- The JDBC aggregation repository runs `SELECT MAX(seq)` on an unindexed column for every new
  group (`cameleer-sql/src/repository.rs`).
- The throttler's per-key map never evicts keys.
- The idempotent and Redis repositories are already keyed lookups.

### 9.3 ps-st3 (work stealing): not needed

Every job in Cameleer's pools comes from outside the pool:

- `executor::ThreadPool`
- the asynchronous-routing offload pool
- the gRPC call pool (one job per call)
- seda

Most of these jobs block for their whole life. Work stealing pays off for fork-join CPU work,
which Cameleer does not have, and short CPU work already runs on the loop that woke it. If a
profile ever shows the pool's receiver mutex, the fix is a cheaper queue, not stealing.

### 9.4 ps-regexr: fix a quadratic copy now, a literal prefilter later

The comparison exposed a bug in Cameleer's engine. `to_captures` (`crates/cameleer-core/src/regex.rs`)
copies the whole subject text and the group-name map into every match:

```rust
Captures { text: text.to_string(), spans, names: self.prog.names.clone() }
```

Everything that iterates matches pays it once per match:

- `find_iter`, `split`, `replace_all`;
- Simple's `split` and `replaceAll`;
- `tokenize` with `regex=true`.

Linux, 64 KiB input:

| case | Cameleer today | with shared captures (prototype) | ps-regexr 0.6.0 |
|---|---:|---:|---:|
| `split` | 73 ms | 2.7-4.4 ms | 0.34 ms |
| find all numbers | 77 ms | 2.7-4.4 ms | 0.28 ms |
| `replaceAll` | 187 ms | about 3 ms (3.5x faster than ps-regexr) | 10.4 ms |

- On Windows the "today" column is 131 to 280 ms.
- The prototype shares the text and the names through an `Arc`. It passes 9 suites (200 tests).
- The prototype was measured as a separate binary. Cases it does not touch came out 10 to 50%
  slower there, which looks like code layout. It must be measured again in one A/B binary before
  it lands.

Elsewhere:

- **Short inputs.** Cameleer is 4x to 17x slower than ps-regexr, but that is 0.2 to 2 µs per
  evaluation.
- **Compilation.** Cameleer compiles patterns 4x to 290x faster. That matters because Simple's
  `replaceAll`, `split` and `matches`, and json-validator's `pattern` and `format`, compile on
  every evaluation. A compiled-pattern cache helps there whatever the engine.
- **Scanning.** The remaining gap is long input without a match: 9x for an email pattern, 463x for
  an alternation of literals. A required-literal prefilter would close most of it.
- **ps-regexr's JIT.** It made no measurable difference and compiles 3x to 8x slower. Without the
  JIT the tree is 3 crates. With it, the tree is 17 to 18 crates, including two versions of syn.

### 9.5 jsonmax: revisit when JSON shows in a route profile

Linux:

| case | Cameleer | jsonmax | Cameleer ÷ jsonmax |
|---|---:|---:|---:|
| parse, 185 B | 689 ns | 781 ns | 0.88x (Cameleer faster) |
| parse, 64 KiB | 239 µs | 130 µs | 1.8x |
| serialize, 64 KiB | 127 µs | 70 µs | 1.8x |
| read one field, 185 B | 698 ns | 1.1 µs | 0.63x (Cameleer faster) |
| read one field, 64 KiB | 255 µs | 87 µs, or 54 µs when skipping | 2.9x to 4.8x |
| validate a small message | 4.5 µs | 1.4 µs | 3.2x |

jsonmax's schema is its own flat format, not JSON Schema 2020-12, so the validation row compares
only what the two have in common.

Ideas worth porting std-only, in order:

1. a schema compiled once per endpoint, with its regexes;
2. a JsonPath that skips through the raw body for definite paths;
3. a lazy or tape `Value`, only if a measured need appears.

### 9.6 What to do with this

None of it needs a dependency, and none of it changes the verdict in section 1. Three changes
are worth their own branches:

1. the aggregator deadline index;
2. the shared captures in the regex engine, with an A/B rerun;
3. the accept-loop polling.

The cloud client move to simoom-client and the regex prefilter are larger and come after.
