# gRPC on a pool of threads without the memory cost, 2026-11-19

Raw records behind the third gRPC section of [docs/BENCHMARKS.md](../../../docs/BENCHMARKS.md). The round of
[2026-11-18-linux-grpc-pool](../2026-11-18-linux-grpc-pool/) found that running gRPC calls on a pool of threads
quadrupled the call rate but raised peak memory to 49 MB (64 calls) and 169 MB (256 calls). This round measures the
fix: one pool job per call (no reader thread, waker based reads), and an allocator that hands the blocks of an idle
worker back to other threads.

**Setup.** Three DigitalOcean `c-4` droplets in `sfo3` (4 dedicated vCPUs of an Intel Xeon Platinum 8280, 8 GB,
Ubuntu 24.04; `machine.txt`). The earlier rounds used `blr1`, where `c-4` was no longer available, so absolute
figures differ a little from them; each comparison here is within one session. Two servers that swap roles, a load
node, 15 s warm-up, 5 x 5 s, the same load generator for both builds.

- **New:** branch `grpc-mem` at `a3b3879` (PR 65).
- **0.7.0 baseline:** `main` at `d14472e` (a thread started per call). Pass 1.
- **Pool baseline:** the first pool build, `880e4a8` (PR 62: three threads per call from a pool). Pass 2, run with
  the order of the sides swapped (the baseline first) to check for an order effect.

| file | what |
|---|---|
| `results-vs-0.7.0-64.txt`, `results-vs-0.7.0-64-run2.txt` | new against 0.7.0, two runs at 64 connections |
| `results-vs-0.7.0-256.txt` | new against 0.7.0 at 256 connections (`http`, `grpc`, MCP) |
| `results-vs-pool-64.txt` | new against the pool build, 64 connections, sides swapped |
| `machine.txt`, `tables.md` | the machines and the tables (made by `scripts/cloud-bench/compare.py`) |

See `tables.md` for every figure. In short (gRPC unary call, new / baseline):

| | 64 connections vs 0.7.0 | 256 connections vs 0.7.0 | 64 connections vs the pool build |
|---|---|---|---|
| requests per second | 41,020 / 10,514 = 3.90x | 36,266 / 10,194 = 3.56x | 40,788 / 39,020 = 1.05x |
| p99 | 3.96 / 12.71 ms | 23.07 / 41.16 ms | 4.01 / 4.12 ms |
| server CPU per request | 37 / 214 us | 38 / 204 us | 35 / 60 us |
| **peak memory** | **13.5 / 13.8 MB** | **18.6 / 20.1 MB** | **14.3 / 49.7 MB** |

The other scenarios run identical code in both builds, so their ratios show the noise: HTTP 0.94x to 1.00x, REST 0.90x
to 0.98x, MCP 0.99x to 1.00x. The REST figure moved from 0.90x to 0.98x when the order of the sides was swapped, so
the 0.93x to 0.95x seen in earlier rounds is probably the harness and not the code.

Not measured: streaming calls, TLS, the gRPC client, Windows, real traffic with long-lived idle calls.
