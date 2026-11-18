# gRPC and HTTP/2 on simoom: Cameleer 0.7.0 against 0.6.1, 2026-11-18

Raw records behind the "gRPC and HTTP/2 on simoom" section of
[docs/BENCHMARKS.md](../../../docs/BENCHMARKS.md).

**Setup.** Three DigitalOcean `c-4` droplets in `blr1` (4 dedicated vCPUs of an Intel Xeon Platinum
8280, 8 GB, Ubuntu 24.04, kernel 6.8.0-147; `machine.txt`), as in `scripts/cloud-bench`: two servers
that swap roles (phase 1 and 2) and a load node. 15 s warm-up and 5 x 5 s per run, load over the private
network.

- **New:** the library code of release 0.7.0 (`acf9bc4`; the measured commit `0b777b3` differs from it only in
  one test file), with the benchmark scenarios added.
- **Old:** release 0.6.1 (`ef034b6`) with the same benchmark scenarios added (branch `bench-base-0.6.1`), so
  it serves the same gRPC route through its own HTTP/2 server.
- **The load generator is the same for both**, built from the new commit, so the old and the new server are
  driven by one client. gRPC: unary calls with a protobuf request and reply (`crates/cameleer-bench/proto/bench.proto`),
  64 or 256 concurrent calls over 4 HTTP/2 connections. HTTP: the existing keep-alive generator.

| file | what |
|---|---|
| `results-64-run1.txt`, `results-64-run2.txt` | two complete runs at 64 connections, both builds, both servers, both phases |
| `results-256.txt` | 256 connections: the runs of both builds, with the two cases that failed in the first attempt replaced by their rerun |
| `results-256-raw-with-failed-cases.txt`, `results-256-rerun-of-failed-cases.txt` | what that merge came from. The first attempt lost two cases of the new build (`http` and `grpc`, phase 1, server b, `FAILED`) because a diagnostic server of mine still held port 8080; they were run again, nothing else changed |
| `results-http2.txt` | the new build only: `rest-json` over HTTP/1.1 and over HTTP/2 (`rest-json-h2`), 64 concurrent calls, 4 connections |
| `sizes.txt` | binary size and cold start of the `cameleer` CLI (`dist` builds, `full` and `core`), new against old, 4 rounds of 20 starts, on server a |
| `tables.md` | the tables below, made by `scripts/cloud-bench/compare.py` |

Method notes that matter for reading the numbers:

- The two servers differ in speed (server b was 10 to 25% faster in these rounds) and phase 1 was slower than
  phase 2 in the first run. Every build was measured on both servers, once in each phase, so a figure is the
  mean of its records and a ratio is the ratio of means. The per-run ratios show the spread.
- Two runs at 64 connections, one at 256. Throughput of one iteration varies by about 2% inside a run; the
  differences between runs on one server are larger, which is why differences of a few percent are not
  claimed as results.
- CPU per request is an estimate: user plus system seconds of the server process over the requests of the
  warm-up and the timed iterations at the timed rate. It is the same estimate for both builds.

## Tables

See `tables.md`. In short (new / old, means over the records):

| scenario | 64 connections | 256 connections |
|---|---|---|
| HTTP, fixed reply | 1.00x (146,477 / 146,770 req/s) | 0.98x |
| REST API with JSON | **0.95x** (123,793 / 130,956 req/s; per run 0.94 and 0.95) | not run |
| gRPC unary call | **1.06x** (10,472 / 9,926 req/s), p99 10.85 / 16.66 ms, CPU per request 204 / 287 us | 1.05x, p99 39.06 / 44.83 ms, CPU per request 199 / 284 us |
| MCP tool call, stateless / session | 1.00x / 1.00x | 0.99x / 1.01x |

## HTTP/2 against HTTP/1.1 on the same route (new build only)

`rest-json`, 64 concurrent calls, mean of the two phases:

| | HTTP/1.1 | HTTP/2 (4 connections) |
|---|---:|---:|
| requests per second | 125,721 | 63,941 (**0.51x**) |
| mean latency | 0.47 and 0.55 ms | 0.86 and 1.20 ms |
| p99 | 1.48 and 1.65 ms | 1.65 and 1.88 ms |
| server CPU per request | 21 and 25 us | 21 and 19 us |
| server CPU used | 225 and 230% of 400% | 126 and 79% of 400% |

HTTP/2 serves about half the requests per second of HTTP/1.1 for these small requests, with the same CPU per
request and with the server far from saturated. Not explained. What was checked on the droplets afterwards (not
in the record files): 4, 16 and 64 HTTP/2 connections gave 72,000 to 81,000 requests/s, so it is not the number of
connections; the threads of the load generator were at most 17% busy, so it is not the load generator. A `top`
sample of the server showed event-loop threads between 45% and 91% busy, but two servers were running at that
moment by mistake, so it is only a hint.

## Binary size and cold start

`sizes.txt` (stripped `dist` builds, new / old): `full` 26,523,736 / 26,714,760 bytes (**-0.7%**), `core`
7,047,640 / 7,047,768 bytes (unchanged). Cold start to a ready HTTP route, mean of four rounds: `full` 2.17 / 2.12 ms,
`core` 1.59 / 1.56 ms (no difference); timer route `full` 2.87 / 3.01 ms. Peak memory of the HTTP start: `full`
11.1 / 11.0 MB, `core` 6.7 / 6.8 MB.

## What was not measured

- Apache Camel's gRPC and HTTP/2 (no such side was written); this round compares two builds of Cameleer.
- The gRPC client against the old client (the load generator is the new client for both servers).
- Server-streaming and bidirectional gRPC calls; HTTP/2 with TLS; Windows.
- Why gRPC stops at about 11,000 calls/s on both builds (see the section in docs/BENCHMARKS.md).
