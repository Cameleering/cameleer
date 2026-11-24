# Cameleer `main` against Apache Camel, one session, 2026-11-23

`main` at `fddff03` (0.8.0 candidate plus the governor and Kafka producer fixes of PRs 74 and 75) against Apache Camel 4.23.0
on Temurin 21.0.13, Camel Quarkus 3.40.0 native, and the official MCP TypeScript SDK (side `node`), all in this one session.
Three new `c-4` droplets in `sfo2` (servers a: Xeon Platinum 8358, b: 8280, see `machine.txt`), servers swapped halfway,
15 s warm-up, 5 x 5 s, 64 connections, 30,000,000 Kafka records, `scripts/cloud-bench/run.sh all` with
`BENCH_SIDES="java rust native node"`. No case failed (78 RESULT records, 0 FAILED). Files: `results.txt`, `tables.md` and
`summary.json` (made by `scripts/cloud-bench/summarize.py`), `machine.txt`, `run.log`. The droplets are not the ones of the
2026-11-20 round (Xeon 8168), so absolute figures differ by session; compare ratios inside this session.

## Headline

Cameleer is ahead of Camel on the JVM in every scenario: 1.6x (HTTP, fixed reply) to 5.8x (seda request-reply) on the
routing and served scenarios, Kafka stream processing 20.0x; HTTP into Kafka 44,043 against 10,012 req/s (4.3x, p99 2.4
against 7.8 ms, peak memory 31 against 313 MB); HTTP into PostgreSQL 2.1x; startup 3.0 ms against 1,014 ms (native 23.6 ms);
peak memory 8 to 37 MB against 213 to 428 MB. Cameleer's CPU is higher than Camel's only for HTTP into Kafka (202% against
130% JVM and 141% native).

**MCP** (`mcp-session`, mean of the two servers): Cameleer 54,921 calls/s (47,493 and 62,348), p99 3.9 ms, 23 MB; Camel
`camel-mcp-server` 23,845 calls/s, p99 5.1 ms, 287 MB; TypeScript SDK 2,752 calls/s, p99 46.9 ms, 203 MB: **2.30x Camel and
20.0x the SDK** (2026-11-20: 2.06x and 19.9x). Stateless MCP tool call on Cameleer: 52,787 calls/s.

## Against the earlier rounds

- The 2026-11-20 session (0.8.0 candidate, other droplets) measured HTTP into Kafka at 31,351 req/s (3.2x Camel) and
  2026-11-06 at 48,800 (5.0x); this session reads 44,043 (4.3x). All three are different machines; the ratio against Camel
  on the same machines is what to compare, and it is lower than the 2026-11-06 ratio.
- Kafka stream processing is 20.0x here against 15.2x on 2026-11-20 (Camel 706 against 662 records/s).

## Not covered

Long-running behaviour: see the soak results (`../2026-11-23-linux-soak/`) for memory, threads and descriptors over an hour.
**Open finding:** the `connects` column of `results.txt` (connections the load generator had to open) is 64 for Camel and the
SDK in every served scenario, and 700 to 5,700 for Cameleer (`http` 4,000 to 5,800, `rest-json` 4,700 to 5,300,
`http-kafka` 1,600 to 2,000, `http-sql` 700 to 800, `mcp-session` 1,900 to 2,500): the Cameleer server (or its client) ends
keep-alive connections that Camel keeps. Each reconnect costs the benchmark, so Cameleer's figures may be understated; the cause
(a request or age limit of the server, or the load generator) was not looked into.
