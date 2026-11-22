# Cloud benchmark: Cameleer and Apache Camel on separate Linux machines

These scripts run the benchmark suite on three DigitalOcean droplets:

- two identical servers that take turns running Apache Camel and Cameleer
- a load node that runs the HTTP load generator, Apache Kafka and PostgreSQL

The setup is close to production: clients and brokers are on other machines and talk over the
private network. Halfway through, the servers swap roles, so a difference between the two
machines cancels out.

```sh
export DO_TOKEN_FILE=~/do-token.txt          # a DigitalOcean API token; never printed
scripts/cloud-bench/droplets.sh up           # 3 x c-4 (4 dedicated vCPU, 8 GB) in sfo2
scripts/cloud-bench/run.sh all               # setup, fill Kafka, run every scenario
scripts/cloud-bench/run.sh test              # optional: the hermetic test suite on Linux
scripts/cloud-bench/droplets.sh down         # delete the droplets and the uploaded SSH key
```

Results are written to `target/cloud-bench/` (`results.txt`, `machine.txt`, `run.log`).
`DO_REGION` (default `sfo2`) and `DO_SIZE` choose another region or size. `c-4` is not offered everywhere: on 2026-11-20 it was available in `sfo2`, `ams3` and `lon1` only (not in `blr1` or `sfo3`, where earlier rounds ran), so `droplets.sh up` fails in a region without it; set `DO_REGION`. The CPU differs by region and machine (Xeon Platinum 8168 and 8280 in `sfo2`), so absolute figures differ between sessions by about 40%: compare ratios inside one session. `BENCH_WARMUP`, `BENCH_ITER`,
`BENCH_ITERS` and `BENCH_CONNS` change the plan. Delete the droplets when you're done,
because they're billed by the hour.

## Scenarios

| scenario | where | what it does |
|---|---|---|
| `direct`, `seda`, `seda-inout`, `cbr`, `splitagg` | in process | the routing scenarios of `scripts/bench.sh` |
| `csv` | in process | split a 1000-row CSV file, keep the EU rows, reshape them, aggregate batches of 100 |
| `http` | over the network | a fixed JSON reply: the cost of the HTTP stack |
| `rest-json` | over the network | an order API: read fields from the JSON request, answer with JSON |
| `http-kafka` | over the network | order ingestion: publish each order to Kafka, keyed by id, then answer |
| `http-sql` | over the network | persistence: insert each order into PostgreSQL, then answer |
| `grpc` | over the network | a unary gRPC call through the `grpc` consumer, protobuf request and reply (`crates/cameleer-bench/proto/bench.proto`); `BENCH_H2_CONNS` sets the HTTP/2 connections (default 4) |
| `rest-json-h2` | over the network | the `rest-json` route called over HTTP/2 (Cameleer only; there is no Java side) |
| `kafka-stream` | Kafka on the load node | consume orders, filter, reshape, publish to another topic |
| `startup` | in process | time to a started context with every route |

Both sides define the same routes: `crates/cameleer-bench/src/scenarios.rs` and
`realworld.rs`, and `benchmarks/java-camel/src/main/java/bench/Bench.java`. Each
served scenario records the server's peak memory and CPU use with `/usr/bin/time`.

Pinned and checksum-verified downloads:
- Rust 1.99.1 through `rustup-init`
- Temurin 21 and Maven, the same as `scripts/bench.sh`
- Apache Kafka 4.3.2

PostgreSQL is Ubuntu 24.04's package.

## Comparing two builds of Cameleer

`BENCH_BASE=<git ref> BENCH_SIDES="rust base"` measures this commit (`rust`) against another one (`base`) on the
same droplets, and `BENCH_INPROC=0 BENCH_SERVED="http rest-json grpc"` limits the run to served scenarios. The
two servers differ in speed, so run it at least twice and read the result with
`python3 scripts/cloud-bench/compare.py "64 connections" results-1.txt results-2.txt`, which takes the mean of
each build over both servers and both phases. The baseline needs the benchmark scenarios too: make a branch from
the old release and cherry-pick the scenario commit onto it (the round of 2026-11-18 did this with
`bench-base-0.6.1`, see `benchmarks/results/2026-11-18-linux-grpc-http2/`). `run.sh sizes` compares binary
size, cold start and memory of the `cameleer` command.

`BENCH_SWAP=1` runs `base` before `rust` in the first phase on server a and swaps the pair in the second phase, so
a second pass can show whether the order of the sides matters (the 0.8.0 candidate round used it as its second 64
connection pass).

## Telling a server limit from a client limit

`scripts/cloud-bench/peer.sh` drives the same Cameleer server with nghttp2's `h2load` (HTTP/1.1, HTTP/2 with 4
connections of 16 streams and with 16 of 4, and gRPC unary) next to `cameleer-bench`'s own load generator, so a limit
of the server can be told apart from a limit of the benchmark's client. It runs from your machine after `droplets.sh up`
and `run.sh setup`, installs `nghttp2-client` on the load node and writes `target/cloud-bench/peer.txt`
(`PEER_SECS`, `PEER_WARMUP`). The HTTP/2 client of `cameleer-bench` is a blocking one that hands each response between
two threads; `BENCH_H2_CLIENT_HTTP1=1` runs the same library client over HTTP/1.1 as a control. That client limited the
2026-11-18 round, see `docs/BENCHMARKS.md`. Compare `h2load` and `cameleer-bench` only as a check of where a ceiling is: the two protocols
are not loaded alike (one request per HTTP/1.1 connection, many streams per HTTP/2 connection).
