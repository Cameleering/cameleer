# Performance review for FaaS and efficiency-critical deployments

_2026-11-05, branch `performance-improvement` from `main` at `0b44c98` (v0.3.0). Baselines
were measured on the unchanged `main` build before any change; the "after" columns are in
[`docs/BENCHMARKS.md`](../docs/BENCHMARKS.md#faas-cold-start-and-size) and
[`analysis.md`](analysis.md#fourth-round-what-changed). The landscape and the techniques
behind the priorities are in [`perf-research.md`](perf-research.md)._

## How it was measured

- **Machine:** Ryzen 5 5600X (6 cores / 12 threads), Windows 11 Pro, and WSL2 Ubuntu 24.04 on
  the same machine (6 GB cap, the repo cloned into the WSL filesystem, not `/mnt/e`).
- **Cold start of the real CLI:** new `cameleer-bench coldstart` spawns a command N times and
  records spawn → first output line containing a marker (`Routes startup` for `cameleer run`,
  `Cameleer` for `cameleer version`), spawn → exit, and peak RSS. The older `startup` scenario
  measured the benchmark binary with the benchmark routes, not the CLI.
- **Workloads:** `cameleer version` (process and binary-loading floor); `run hello.camel.yaml
  --max-messages 1` (a timer route that fires once, then exits: a run-once job); `run
  http.camel.yaml` (a `platform-http` route, killed when ready). A Rust hello-world gives the
  OS floor.
- **Page cache warm** (the binary was run before). Lambda and Cloud Run fetch the image lazily, so
  a cold-cache start there also pays for the pages the binary touches; that scales with the
  binary size, which is why size is treated as a startup metric below.
- Medians of 20–40 runs. The Windows figures move by ±1 ms with Defender and with where the
  binary lives (a copy in `%TEMP%` started 4 ms slower than the same file under `E:\`), so
  both sides of every comparison were run from the same directory, back to back.

## Baseline (before any change)

### Startup

| | Linux (WSL2) | Windows |
|---|---:|---:|
| Rust hello-world (OS floor) | 0.48 ms | 4.4 ms |
| `cameleer version` | 0.77 ms | 6.3 ms |
| timer route: spawn → routes started | 1.58 ms | 8.4 ms |
| HTTP route: spawn → routes started | 3.25 ms | 10.8 ms |
| timer route, run once: spawn → process exit | 22.2 ms | 31.2 ms |

The published "10 ms startup" is mostly Windows process creation: a hello-world costs 4.4 ms
there. On Linux the whole CLI starts a timer route in 1.6 ms. Two costs stood out:

1. **The HTTP consumer spawned 32 worker threads at startup** (`ThreadPool::new` in
   `cameleer-core/src/executor.rs`, used by `cameleer-http/src/server.rs` with
   `pool_size: 32`): 1.7 ms of the HTTP route's 3.25 ms on Linux and about 2.4 ms on Windows,
   plus 32 threads in every idle HTTP container.
2. **A run-once job lingered for 20 ms after its last message**: `Main::run` checked
   `duration-max-messages` every 20 ms (`thread::sleep(20 ms)` in the loop).

Startup steps inside the process (new startup recorder, see below; Windows, so absolute numbers
are inflated):

| step | time |
|---|---:|
| register ~300 component factories and languages (`new_main`) | 0.41 ms |
| merge configuration (files, profile, environment, aliases, beans) | 0.66 ms |
| first YAML route file (parser, DSL tables) | 0.31 ms |
| create two routes | 0.18 ms |
| start the HTTP consumer (bind, accept thread) | 1.6 ms |

Component registration is already lazy (factories, not instances), so it is not the problem it
was assumed to be; the cost is per-process work that is small next to exec on Linux.

### Size

`dist` profile (thin LTO, one codegen unit), stripped, Linux x86_64: **24.9 MB** (28.7 MB
unstripped). Sections: `.text` 19.3 MB, `.eh_frame` 1.36 MB, `.rodata` 1.04 MB,
`.gcc_except_table` 1.0 MB, `.rela.dyn` 0.52 MB. Code by crate (symbol sizes):

| crate | MB | | crate | MB |
|---|---:|---|---|---:|
| cameleer-core | 3.29 | | cameleer-sql | 0.66 |
| cameleer-cloud | 2.30 | | cameleer-amqp10 | 0.62 |
| cameleer-script | 2.11 | | cameleer-datastores | 0.61 |
| cameleer-xml | 1.61 | | cameleer-protocols | 0.53 |
| cameleer-dataformats | 1.35 | | cameleer-grpc | 0.50 |
| std | 1.06 | | cameleer-yaml | 0.47 |
| cameleer-tls | 0.83 | | cameleer-amqp, -main, -http | 0.40 each |
| cameleer-soap | 0.74 | | cameleer-ssh, -kafka-mqtt, -saas | 0.30–0.39 |

Every binary carried every component family, because `cameleer-main` depended on all crates
unconditionally. A function that only needs HTTP and S3 still shipped SOAP, five script
interpreters, XQuery and the Cassandra driver.

### Memory

Linux, one route, `/proc/<pid>/status` while running:

| | VmRSS | anonymous | file-backed | threads |
|---|---:|---:|---:|---:|
| HTTP route | 15.1 MB | 1.7 MB | 13.4 MB | 34 |
| timer route | 14.4 MB | 1.1 MB | 13.3 MB | 1–2 |

**The private memory of a running route is 1.1–1.7 MB.** The rest of the "10–15 MB" is pages
of the binary itself (shared, clean, reclaimable, but counted in RSS and in a container's
memory accounting while mapped), so it shrinks with the binary. Windows peak working set is
8–9 MB for the same routes.

### Packaging for AWS Lambda

- There was no Lambda runtime: the roadmap described building one from routes
  (`docs/ROADMAP.md` 1.4).
- **A Linux build from Ubuntu 24.04 does not load on Lambda's `provided.al2023`.** The binary
  requires `GLIBC_2.39` (weak `pidfd_*` from std) and `GLIBC_2.35` (`hypot`, a hard
  requirement); AL2023 has glibc 2.34. The container image's builder (Debian 12, glibc 2.36)
  has the same `hypot@GLIBC_2.35` problem. A Lambda build needs static musl or a build against
  glibc ≤ 2.34.

## Findings and priorities

Ranked by expected impact for FaaS (cold start, then image size and RSS, then throughput).
"Done" items are on this branch with tests; measurements in `docs/BENCHMARKS.md`.

| # | finding | change | effort / risk | status |
|---:|---|---|---|---|
| 1 | No Lambda support; every Lambda user needs an adapter | built-in Lambda custom runtime in `cameleer run` (`cameleer-main/src/lambda.rs`): Runtime API loop, init errors, HTTP events to `platform-http` routes, `bootstrap` name | M / low (embedded fake Runtime API tests) | done |
| 2 | Every build carries all ~230 components | cargo features per component family in `cameleer-main` (default: all), errors name the missing feature, `FEATURES` build arg in the Dockerfile | M / low (default unchanged) | done |
| 3 | HTTP consumer starts 32 threads eagerly | `ThreadPool` starts workers on demand (Java `ThreadPoolExecutor` core-thread semantics), `prestarted()` for the old behaviour | S / low | done |
| 4 | Run-once jobs linger 20 ms | run loop waits on a condition variable woken when max messages is reached | S / low | done |
| 5 | Startup cost is invisible | `camel.main.startupRecorder=logging` (Camel's `StartupStepRecorder`), `cameleer_core::startup` | S / none | done |
| 6 | Thin LTO leaves speed and RSS on the table | fat LTO in `dist` (splitagg +6–8%, peak RSS −25%, +8% size on the same code) | S / longer builds | done |
| 7 | glibc version too new for AL2023 | Lambda zips use static musl (`scripts/package-lambda.sh`); starts fastest of all (0.31 ms `version`), 3–22% slower throughput, so the image stays glibc | S / low | done |
| 8 | 0.5 MB of relocations processed at every start | `-z pack-relative-relocs` (DT_RELR): `.rela.dyn` 535 KB → 4 KB + 6 KB `.relr.dyn`, −0.5 MB; needs glibc ≥ 2.36 at run time (Debian 12 distroless yes, AL2023 no) | S / ties the image to glibc ≥ 2.36 | measured, not adopted (see BENCHMARKS) |
| 9 | Configuration merge ~0.5 ms | relaxed-binding lookups scan and re-normalise every key per lookup; precompute | S | follow-up |
| 11 | Log macros inline a `format!` at ~190 call sites and format disabled-level records | one out-of-line `log_args(format_args!)`, inline atomic level check | S / low | done: −3.1 MB (11%) |
| 12 | A new allocator chunk is written block by block (every page resident) | bump-pointer carving | S / low | done: private memory 1.71 → 0.72 MB |
| 13 | Pool workers never exit after a burst | 60 s keep-alive (Camel's default profile) | S / low | done |
| 14 | Status files and a 500 ms status thread on serverless platforms | off on Lambda, Cloud Run/Knative, Azure Functions | S / none | done |
| 15 | Template engines, jq, the core XPath subset are always in core (~0.7–1 MB of the 7.3 MB `core` build); `cameleer-http` pulls `cameleer-grpc` for HTTP/2; `validate`/`schema` in every binary | core cargo features; optional HTTP/2 | M | follow-up |
| 16 | XQuery/XSLT start a thread with a 128–256 MiB stack per evaluation; one thread per timer; supervising controller thread never exits | run on the caller unless the stack is low (as `cameleer-script` does); shared timer thread per name; exit when idle | S–M | done (status file also written only on change) |
| 10 | `opt-level = "s"` | −13% size (thin) but −10 to −14% throughput | S | rejected as default |

Not pursued, with reasons (details in `perf-research.md`):

- **Snapshot/restore (CRIU, Firecracker snapshots, SnapStart):** Cameleer's start on Linux is
  about 1 ms after exec; restoring a snapshot costs more than that. SnapStart does not cover
  custom runtimes.
- **WASM (wasm32-wasip2):** the only route to microsecond "starts", but those numbers are module
  instantiation inside an already running host, and the code needs threads and sockets that
  WASI preview 2 does not offer in the same form. High effort; recorded as a direction.
- **io_uring:** Docker's default seccomp profile blocks it, Lambda's support is unclear, and the
  HTTP bottleneck on this machine is the load generator (`analysis.md`).
- **`panic = "abort"`:** the executor and the HTTP server catch panics in routes.

## Startup recorder

`camel.main.startupRecorder=logging` (environment `CAMEL_MAIN_STARTUP_RECORDER=logging`) logs
every startup step with its offset from process start and its duration, like Camel's
`LoggingStartupStepRecorder`:

```text
     78 µs     406 µs  main register components
    833 µs     661 µs  main configure
   1.67 ms     306 µs  routes-loader hello.camel.yaml
   2.35 ms    1.87 ms  context start
   2.37 ms     184 µs    routes create
   2.56 ms    1.64 ms    routes start
   2.60 ms    1.60 ms      consumer api
```
