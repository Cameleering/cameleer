# Cloud benchmark: Cameleer and Apache Camel on separate Linux machines

These scripts run the benchmark suite on three DigitalOcean droplets:

- two identical servers that take turns running Apache Camel and Cameleer
- a load node that runs the HTTP load generator, Apache Kafka and PostgreSQL

The setup is close to production: clients and brokers are on other machines and talk over the
private network. Halfway through, the servers swap roles, so a difference between the two
machines cancels out.

```sh
export DO_TOKEN_FILE=~/do-token.txt          # a DigitalOcean API token; never printed
scripts/cloud-bench/droplets.sh up           # 3 x c-4 (4 dedicated vCPU, 8 GB) in blr1
scripts/cloud-bench/run.sh all               # setup, fill Kafka, run every scenario
scripts/cloud-bench/run.sh test              # optional: the hermetic test suite on Linux
scripts/cloud-bench/droplets.sh down         # delete the droplets and the uploaded SSH key
```

Results are written to `target/cloud-bench/` (`results.txt`, `machine.txt`, `run.log`).
`DO_REGION` and `DO_SIZE` choose another region or size. `BENCH_WARMUP`, `BENCH_ITER`,
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
