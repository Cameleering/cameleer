# Cameleer `main` (with the keep-alive fix) against Apache Camel, one session, 2026-11-23

`main` at `beb3385` (PR 78: the HTTP server no longer closes a keep-alive connection after 1,000 requests), run as
`bench/soak` `8ac9556` (the same code plus the soak scripts). Same droplets, plan and sides as
`../2026-11-23-linux-main-fddff03-camel/` earlier the same day: three `c-4` in `sfo2` (server a Xeon Platinum 8358, b 8280),
servers swapped halfway, 15 s warm-up, 5 x 5 s, 64 connections, 30,000,000 Kafka records, sides java, rust, native and node
(Camel 4.23.0 on Temurin 21.0.13, Camel Quarkus 3.40.0 native, the MCP TypeScript SDK). The Java and native builds were not
rebuilt. 78 RESULT records, 0 FAILED. Files: `results.txt`, `tables.md`, `summary.json` (`scripts/cloud-bench/summarize.py`),
`machine.txt`, `run.log`.

## The fix

The load generator now opens 64 connections in every Cameleer scenario (65 to 66 in `rest-json`), against 700 to 5,700 in
the morning run. Cameleer, mean of both servers, morning against now:

| scenario | `fddff03` (reconnects) | `beb3385` | change |
|---|---:|---:|---:|
| HTTP, fixed reply | 120,542 req/s, p99 1.5 ms | 146,114 req/s, p99 0.61 ms | +21% |
| REST API with JSON | 123,280 req/s, p99 1.4 ms | 141,276 req/s, p99 0.84 ms | +15% |
| HTTP into Kafka | 44,043 req/s | 44,402 req/s | +1% |
| HTTP into PostgreSQL | 17,238 req/s | 17,386 req/s | +1% |
| MCP session | 54,921 calls/s | 55,313 calls/s | +1% |

The reconnects cost throughput and p99 where requests are cheap (HTTP, REST); where each request waits for Kafka, PostgreSQL
or the MCP engine, one reconnect per 1,000 requests was within noise.

## Against Camel (`tables.md`)

Cameleer is ahead of Camel on the JVM in every scenario: 2.0x (HTTP, fixed reply) to 5.6x (seda request-reply), REST with
JSON 3.0x, HTTP into Kafka 4.3x (p99 2.5 against 7.6 ms), HTTP into PostgreSQL 2.1x, Kafka stream processing 19.8x; p99 lower
in every served scenario (HTTP 0.61 against 3.6 ms); startup 3.0 against 1,034 ms (native 23.8 ms); peak memory 8 to 37 MB
against 213 to 428 MB. Cameleer's CPU is higher than Camel's only for HTTP into Kafka (202% against 134% JVM and 142% native).

**MCP** (`mcp-session`, mean of two servers): Cameleer 55,313 calls/s, p99 4.0 ms, 23 MB; Camel `camel-mcp-server` 20,254
calls/s, p99 5.8 ms, 295 MB; TypeScript SDK 2,684 calls/s, p99 47.4 ms, 202 MB: **2.73x Camel and 20.6x the SDK**.
Stateless MCP tool call on Cameleer: 52,435 calls/s.

## Compared with published figures

Absolute figures differ between sessions (these droplets are faster than the Xeon 8168 of 2026-11-20); compare ratios. Against
the ratios published from the 2026-11-20 session (`docs/BENCHMARKS.md`): HTTP 1.8x then, 2.0x now; MCP session 2.06x then,
2.73x now; Kafka stream 15.2x then, 19.8x now; HTTP into Kafka 3.2x then, 4.3x now; seda request-reply 6.8x then, 5.6x now; content-based router 3.0x then, 2.6x now; CSV 2.3x then, 2.2x
now; direct (3.1x) and seda one-way (2.6x) unchanged; split and aggregate 2.5x then, 2.7x now. The three in-process ratios that
are lower moved because Camel on the JVM is faster on these machines (seda request-reply 105k then, 138k now) more than
Cameleer is; Cameleer's own figures are higher than on 2026-11-20 in every in-process scenario but the router.
The 2026-11-06 session (0.5.x) published HTTP into Kafka at 5.0x Camel; it is 4.3x here.

Not measured here: long-running behaviour (the soak results are in `../2026-11-23-linux-soak/`), gRPC (no Camel side), HTTP/2.
