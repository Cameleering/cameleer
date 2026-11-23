# HTTP into Kafka and MCP bisect, async and lazy-loop controls, v0.6.0 against main, 2026-11-22

Three DigitalOcean `c-4` droplets in `sfo2` (see `../2026-11-22-linux-0.8.0-gate/machine.txt`), two servers that swap
roles (pass 1 on server b in plan order = phase 1; pass 2 on server a in reverse order = phase 2), a load node with
Kafka 4.3.2 and PostgreSQL, 15 s warm-up, 5 x 5 s, 64 connections, the same load generator (`main`'s `cameleer-bench`)
for every build. Every binary was built on the servers from a git archive with Rust 1.99.1 (`scripts/vbuild.sh`) and
run by `scripts/vserve.sh`; `scripts/vrun.sh` drives `plan-*.txt`; `tables.txt` is `raw/*` averaged over both phases.

**Builds** (`k575` = `575d460`, the build behind the 48,800 req/s of 2026-11-06): `k575`; `v060` = tag `v0.6.0` + the
benchmark scenarios (branch `bench-base-0.6.0`, last classic server); `v061` = `v0.6.1` + scenarios (first simoom
server); `v070` = `v0.7.0` + scenarios; `pre` = `ec33695` (main before PRs 67 and 68); `pr67` = `pre` + PR 67 (simoom 0.2.0,
eager loops, cheaper gRPC unary); `pr68` = `pre` + its commit `f212a04` (header copies); `main` = `7952563` (async
routing, PR 73 start-up fix, PRs 67 and 68). Controls: `main-ctl` is the same binary as `main` (identical-code control),
`main-lazy` sets `CAMEL_SERVER_LAZY_LOOPS=true`, `main-noasync` sets `CAMEL_ASYNC_ROUTING=false`. All local branches, not pushed.

## 1. HTTP into Kafka: 31k now against 48.8k published

| build | req/s | p99 | |
|---|---|---|---|
| `k575` (2026-11-06 build) | 39,338 | 2.8 ms | the published 48.8k, on this hardware |
| `v060` (classic thread server) | 38,759 | 2.8 ms | |
| `v061` (first simoom server) | 31,225 | 3.7 ms | |
| `v070` | 30,849 (phase 1; phase 2 run gave 12,296, an outlier) | | |
| `pre`, `main`, `main-lazy`, `main-noasync` | 32,484, 31,496, 32,416, 31,596 | 3.3 to 3.4 ms | |

Two causes. (a) **Hardware and region**: the same 2026-11-06 build does 39.3k here instead of 48.8k (the servers are Xeon
8168 in `sfo2`; the old ones were 8280 in `blr1`), so the machines alone explain a factor of 0.81. (b) **The move onto simoom in 0.6.1**:
`v060` 38.8k against `v061` 31.2k, 0.80x, p99 +0.9 ms, and every later build sits on `v061`'s level. The route blocks in the
Kafka producer, so it runs on simoom's offload pool (200 threads) instead of directly on a connection thread; neither
eager/lazy loops nor async routing change it. Candidate fix to try (not done): let a blocking route that is fast on
average avoid the loop-to-pool hand-off, or measure with a smaller pool.

## 2. MCP at 0.96x to 0.98x of 0.7.0: not reproduced

`mcp-session` (calls/s, mean of 2): `v070` 44,570; `pre` 44,508; `pr67` 45,236; `pr68` 44,473; `main` 44,218 (n=4);
`main-lazy` 44,667; `main-noasync` 44,578; `main-ctl` 44,263. Noise of the identical-code control: `main` against `main-ctl`
0.1%. Nothing differs from `v070` by more than 1%: eager loops, the header changes and async routing cost nothing
measurable. The 0.96x of 2026-11-20 does not reproduce (there the 7ee75e7 build ran 43.3k against 44.6k; the one thing
changed since is PR 73 and the async-routing merge). **Stateless MCP** (`mcp`): `main` 42,772 against `v060` 45,440,
0.94x, and 0.93x in the gate run: a real, small regression against 0.6.0 (not isolated further).

## 3. Async routing (default on in 0.8.0)

`http-proxy` (forward to a raw-http server on the load node): on 50,956 req/s p99 2.4 ms, off (`CAMEL_ASYNC_ROUTING=false`)
10,480 req/s p99 16.3 ms: 4.9x. `http` 108.6k on, 111.8k off; `rest-json` 85.8k on, 85.7k off; `http-kafka` 31.5k on, 31.6k
off: no difference. (`v070` proxy was not run: its scenarios have no `http-proxy`.)

## 4. Lazy loops: no effect on `http` (112.4k against `main` 108.6k, `main-ctl` 110.4k), `rest-json`, `mcp-session` or
`http-kafka`. The `rest-json` "5%" of earlier rounds is not there: `v070` 86.7k, `pre` 90.3k, `main` 85.8k, `main-ctl` 82.9k;
per-phase spread is 80k to 91k for the same binary.

## 5. v0.6.0 against main (`raw/v060-vs-main.txt`)

| scenario | v0.6.0 | main | main / v0.6.0 | p99 v0.6.0 / main | verdict |
|---|---|---|---|---|---|
| `http` | 90,152 | 108,645 (this plan) / 111,919 (gate run) | 1.20x to 1.24x | 2.5 / 1.5 ms | better |
| `rest-json` | 74,724 | 85,801 / 83,332 | 1.15x | 2.4 / 2.1 ms | better |
| gRPC unary | 8,486 | 83,274 | 9.8x | 19.5 / 1.4 ms | better |
| `rest-json-h2` | no HTTP/2 server in 0.6.0 | | | | not measured |
| `mcp-session` | 35,290 | 44,218 | 1.25x | 6.0 / 4.2 ms | better |
| `mcp` (stateless) | 45,440 | 42,772 | 0.94x | 4.3 / 4.2 ms | worse (6%) |
| `http-kafka` | 38,759 | 31,496 | 0.81x | 2.8 / 3.4 ms | worse (blocking route) |
| `http-sql` (gate run) | 14,673 | 9,768 (14,316 phase 1; 5,220 phase 2, an unexplained outlier) | 0.67x, 0.98x without the outlier | | needs a rerun |
| peak memory under load, `http` / `rest-json` | 18.4 / 20.1 MB | 9.3 / 10.0 MB | half | | better |
| idle RSS, `http` / `rest-json` / `mcp-session` | 7.0 / 7.3 / 7.5 MB | 7.7 / 8.0 / 8.3 MB | +0.7 MB, 4 threads instead of 2 | | worse, slightly |
| binary, `full` | 28,996,840 | 30,335,712 | +4.6% | | worse |
| binary, `core` | 6,978,064 | 8,554,248 | +22.6% | | worse |
| cold start `http`, `full` / `core` | 2.60 / 1.87 ms | 3.00 / 2.44 ms | +15% / +30% | | worse |
| in-process `startup` | 2.9 ms | 3.4 ms | +0.5 ms | | worse (was 12.3 ms before PR 73) |

On real routes the simoom move is a net gain for plain HTTP, gRPC, REST and MCP sessions (1.15x to 9.8x, lower p99, half
the memory under load) and a loss for routes that block (`http-kafka` 0.81x, `http-sql` unclear) and for binary size and
start-up.

## Harness faults and reruns

The first v0.6.0 attempt failed on gRPC (the `.proto` path compiled into each binary pointed at a source directory that
later builds overwrote) and the first proxy attempt failed (my `pkill` killed its own shell, the proxy target never
started); both were fixed (`raw/v060-first-attempt-before-proto-fix.txt` holds the four cases that had run) and rerun. The
`v070` proxy case failed twice because that build has no such scenario. Outliers: `v070` `http-kafka` phase 2 (12.3k) and
`main` `http-sql` phase 2 on server a (5.2k) are single runs on server a that are not repeated by the other server or the
next build; the droplets were deleted before they could be rerun.
