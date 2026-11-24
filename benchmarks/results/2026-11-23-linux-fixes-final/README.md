# Final numbers for the two fixes, 2026-11-23

`candidate-fixes` (local branch, `origin/main` `7952563` + `fix/governor-busy-demote` `f731e9e` + `fix/kafka-async`
`9143b2c`/`410894e`, merge `2842da1`; not pushed) against `main` (`7952563`) and `v0.6.0` (+ the benchmark scenarios),
same three `c-4` droplets in `sfo2` (new ones; Xeon 8168 servers), servers swapped (phase 1 server b, phase 2 server a, reversed
order in phase 2), 15 s warm-up, 5 x 5 s, 64 connections, `cameleer-bench` of `main` as the load generator. The binaries
were built on the droplets from `git archive` tarballs with `vbuild.sh` (which touches the sources first); md5 of `main`
(`482964be`) and `v0.6.0` (`3dcb6414`) are the same as in the earlier rounds, `cand` is `2a82aef9`, and only `cand` has the
`kafka-sender` threads. Hermetic suite on the candidate (Windows 11): **2,987 passed, 0 failed, 0 warnings**
(`workspace-test-candidate-fixes.log`). Raw: `results.txt`; per-build means: `tables.txt`; plan: `plan.txt`.

| scenario | candidate | `main` | `v0.6.0` | candidate / `main` | candidate / `v0.6.0` |
|---|---|---|---|---|---|
| HTTP into Kafka, req/s (6 runs, 4 for v0.6.0) | **35,472** (33.8k to 38.0k) | 32,527 (31.6k to 35.7k) | 40,744 | **1.09x** | 0.87x |
| HTTP into Kafka, p99 | 3.09 ms | 3.40 ms | 2.71 ms | | |
| HTTP into Kafka, CPU per 1000 requests | **0.060 s** | 0.072 s | 0.071 s | **-17%** | -15% |
| HTTP into Kafka, process CPU | 211% | 232% | 290% | | |
| HTTP into Kafka, peak memory | **31.5 MB** | 60.8 MB | 76.9 MB | **-48%** | -59% |
| HTTP into PostgreSQL, req/s (4 runs) | 9,654 | 9,665 | 10,536 | 1.00x | 0.92x |
| `http` (fixed reply) | 120,240 | 104,510 | | 1.15x | |
| `rest-json` | 88,142 | 90,224 | | 0.98x | |
| gRPC unary | 77,954 | 78,004 | | 1.00x | |
| `mcp-session` | 44,618 | 45,036 | | 0.99x | |
| `mcp` (stateless) | 43,330 | 43,385 | 45,392 | 1.00x | 0.95x |

- **No throughput dips** in any of the 50 runs (slowest second within 8% of the fastest; the governor fix). The HTTP-into-PostgreSQL
  route (a synchronous producer) shows one whole-run slow state of about 7,700 req/s in one run of the candidate and one of `main`
  (not `v0.6.0`); it is not a within-run dip and the cause is not known (PostgreSQL-side state is the suspect).
- **The Kafka producer's gain is throughput, CPU and memory together**: 9% more requests per second at 17% less CPU per request
  and half the memory (no pool of blocked worker threads: 65 workers idle, 5 sender threads); `v0.6.0` is still 15% faster
  at 3.1 against 2.7 ms p99 (the two thread hand-offs, see `../2026-11-22-linux-kafka-mcp-bisect/README.md`).
- `http` +15% is within the spread of this scenario between phases (101k to 123k); the other rows are unchanged within noise.
- Not measured: the other producers (SQL, models, MCP client), which stay synchronous; a long-run test of the sender threads
  under broker failure beyond the unit tests.
