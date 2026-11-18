# gRPC server on a pool of threads, 2026-11-18

Raw records behind the second gRPC section of [docs/BENCHMARKS.md](../../../docs/BENCHMARKS.md). The
round of [2026-11-18-linux-grpc-http2](../2026-11-18-linux-grpc-http2/) found that the gRPC server stops at
about 11,000 calls per second on both builds, with CPU to spare, and suspected that starting up to three
threads per call was the limit. This round tests that by running the calls on a shared pool of threads.

**Setup.** As in the first round: three DigitalOcean `c-4` droplets in `blr1` (4 dedicated vCPUs of an Intel
Xeon Platinum 8280, 8 GB, Ubuntu 24.04; `machine.txt`), two servers that swap roles and a load node,
15 s warm-up, 5 x 5 s, the same load generator for both builds (unary gRPC calls with a protobuf request
and reply, 4 HTTP/2 connections; HTTP with the keep-alive generator).

- **New:** the branch `grpc-pool` at `80e3d08`: the threads of a call (the call, the reader of its request
  messages and, with a deadline, the handler) come from one process-wide cached pool
  (`cameleer_core::executor::ThreadPool`, a worker starts only when none is idle, idle workers exit after 30 s).
- **Old:** `main` at `d14472e`, that is release 0.7.0 with the benchmark scenarios: a thread started per call.
- Each build ran on both servers in both phases. Two runs at 64 connections, one at 256. `tables.md` is made by
  `scripts/cloud-bench/compare.py` (mean over the records of a build, ratio of the means).

| file | what |
|---|---|
| `results-64-run1.txt`, `results-64-run2.txt` | two complete runs at 64 connections |
| `results-256.txt` | one run at 256 connections (`http`, `grpc` and the MCP scenarios) |
| `machine.txt`, `tables.md` | the machines and the tables |

## Result (new / old)

| scenario | 64 connections | 256 connections |
|---|---|---|
| gRPC unary call, requests per second | **47,282 / 10,174 = 4.65x** (per run 4.82x and 4.48x) | 45,108 / 10,400 = 4.34x |
| gRPC p99 | 2.52 / 13.96 ms | 10.22 / 39.06 ms |
| gRPC server CPU per request | 57 / 221 us | 56 / 200 us |
| **gRPC server peak memory** | **49.0 / 14.1 MB** | **168.5 / 20.5 MB** |
| HTTP, fixed reply | 1.03x | 1.03x |
| REST API with JSON | 0.95x (0.98 and 0.91) | not run |
| MCP tool call, stateless / session | 0.99x / 0.99x (per run 1.05 and 0.93) | 1.00x / 0.98x |

**The cost is memory.** The pooled server keeps many threads (each holds its stack and buffers): 3.5x the
memory at 64 concurrent calls and 8x at 256. Absolute figures are small (49 and 169 MB), but this is a cost,
and it grows with the number of concurrent calls.

**A control for the noise.** The HTTP, REST and MCP routes run identical code in both builds here, so every
ratio other than gRPC should be 1.00. They are 0.91 to 1.06 from one run to the next (REST 0.98 and 0.91, MCP
1.05 and 0.93). Differences of 5 to 9% between two builds on this setup are therefore not results; this applies
to the 0.95x of `rest-json` in the first round as well.

A local test in WSL2 (server on 4 cores, load generator on 4 others, not part of the records) gave 6,400 calls
per second before and 20,000 to 30,000 after: thread creation is more expensive there than on the droplets.

## What was not measured

- Streaming gRPC calls, TLS, the gRPC client, Windows.
- Whether fewer threads per call (no separate handler thread, no reader thread for a unary call) would keep the
  gain with less memory.
- Apache Camel's gRPC.
