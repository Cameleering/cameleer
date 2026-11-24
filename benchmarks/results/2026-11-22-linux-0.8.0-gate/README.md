# 0.8.0 release gate: main (7952563) against Apache Camel and v0.6.0, one session, 2026-11-22

`run.sh bench` with sides `java rust native node base` on three `c-4` droplets in `sfo2` (Xeon Platinum 8168 servers, 8280
load node; `machine.txt` says "Commit" of the working tree; the Cameleer side is `main` at `7952563`, built on the
droplets from a git archive, real `Cargo.lock`, simoom 0.2.0, no `[patch]`). `base` is `v0.6.0` + the benchmark scenarios
(branch `bench-base-0.6.0`). Servers swap roles; 15 s warm-up, 5 x 5 s, 64 connections. Camel 4.23.0 on Java 21, Camel
Quarkus 3.40.0 native, MCP TypeScript SDK 1.32.2. No case failed. Files: `results.txt`, `tables-main-vs-camel.md`,
`tables-v060-vs-camel.md` (v0.6.0 in the Cameleer column), `sizes.txt` (4 x 20 cold starts per build, `new` = main, `base` =
v0.6.0), `idle.txt` (RSS 8 s after READY), `summary.json`. The bisect and controls are in
`../2026-11-22-linux-kafka-mcp-bisect/`.

## Gate table: published figure, this run, verdict

"Published" = the figures now in README and BENCHMARKS (session of 2026-11-20 on 7ee75e7, or 2026-11-06 where noted).
Absolute figures differ by up to 40% between sessions and regions, so the verdict uses ratios inside a session (against
Camel, or against v0.6.0 / v0.7.0 in the same run) and says so.

| headline | published | this run | verdict |
|---|---|---|---|
| Throughput against Camel JVM, direct | 3.1x | 3.2x | pass |
| seda one-way / request-reply | 2.6x / 6.8x | 2.7x / 7.0x | pass |
| content-based router | 3.0x | 2.9x | pass (within noise) |
| split and aggregate | 2.5x | 2.6x | pass |
| CSV batch | 2.3x | 2.5x | pass |
| Kafka stream processing | 15.2x | 13.9x (9,230 against 660 records/s; `v0.6.0` 9,269 in the same run) | pass (-9% is session noise: v0.6.0 equal) |
| HTTP fixed reply | 1.8x | 1.9x | pass |
| REST with JSON | 2.4x | 2.1x (83.3k; 7ee75e7 88.4k, Camel 39.6k against 36.0k) | pass (ratio within the 10 to 25% spread) |
| HTTP into Kafka | 3.2x (2026-11-06 build: 5.0x) | 3.3x; but **0.81x of v0.6.0** | **regression** against 0.6.0 and against the 5.0x (hardware explains 0.81 of it, the simoom server the rest): bisect in the other folder |
| HTTP into PostgreSQL | 2.3x | 1.5x (9,768 against 6,482; phase 1 14.3k = 2.2x, phase 2 on server a 5.2k) | **needs a rerun**: one unexplained outlier run; v0.6.0 does 14.7k in the same run |
| MCP session against Camel / TypeScript SDK | 2.06x / 19.9x (2026-11-12: 1.77x / 17.7x) | 2.34x (43,759 against 18,710) / 19.6x (2,237) | pass |
| MCP session against 0.7.0 | 0.96x to 0.98x (2026-11-20) | 0.99x (44,218 against 44,570; control 0.1%) | pass, the 4% was not reproduced |
| MCP stateless against v0.6.0 | | 0.93x (42.4k against 45.5k) | **regression** (small, 6 to 7%) |
| gRPC against 0.7.0 | 11.4x (2026-11-20) | 83.3k against 8.5k for v0.6.0, 9.8x | pass |
| p99 (HTTP, REST, Kafka, seda) | 1.7 / 2.1 / 3.5 ms, 33 µs | 1.5 / 2.2 / 3.4 ms, 31 µs | pass |
| p99 HTTP into PostgreSQL | 9.4 ms | 17.8 ms (outlier run) | rerun, see above |
| peak memory under load | 8 to 57 MB (HTTP into Kafka 57 MB) | 8 to 58 MB | pass (equal) |
| idle RSS | | 7.7 to 8.5 MB (v0.6.0 7.0 to 7.6 MB) | +0.7 MB, 4 threads instead of 2: slight worsening |
| start-up (in-process, started context) | 2.45 ms (cold start, core, published); 12.3 ms in the 2026-11-20 run | 3.4 ms (Camel JVM 1,092 ms, native 27 ms; v0.6.0 2.9 ms) | pass against the 2026-11-20 run (PR 73 fixed it); **+0.5 ms against v0.6.0** |
| cold start to ready, core http route | 2.45 ms (0.7.0 candidate, `docs/BENCHMARKS.md`); v0.6.0 1.87 ms here | 2.44 ms | equal to the published figure, **+30% against v0.6.0**; `full` 3.00 against 2.60 ms (+15%) |
| binary size, `full` | 29.98 MB (0.7.0), 28.99 MB (0.6.0) | 30.34 MB | +1.2% against 0.7.0, +4.6% against 0.6.0: small growth |
| binary size, `core` | 7.05 MB | 8.55 MB unstripped, 7.32 MB stripped (v0.6.0 6.98 / 5.97) | **regression**: +22.6% against v0.6.0, +3.8% against the published stripped figure |
| async routing proxy route | | 4.9x on against off | improvement |

**Would 0.8.0 pass "no published number degrades"?** Not yet, by this table: HTTP into Kafka (-19% against 0.6.0, and
against its published 5.0x), HTTP into PostgreSQL (one run unexplained), stateless MCP (-6% against 0.6.0), idle memory
(+0.7 MB), cold start and `core` binary size against 0.6.0. Against the *published 0.7.0 and 2026-11-20 figures* only
HTTP into Kafka and HTTP into PostgreSQL are open; everything else holds or improved.
