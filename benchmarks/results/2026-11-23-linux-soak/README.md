# Soak: HTTP into Kafka and MCP session, one hour each (2026-11-23)

Six one-hour runs with `scripts/cloud-bench/soak.sh`. They used the same droplets, builds and sides as
`../2026-11-23-linux-main-beb3385-camel/` (Cameleer `main` `beb3385` with the keep-alive fix, Apache Camel
4.23.0 on Java 21, Camel Quarkus native, the MCP TypeScript SDK on Node). The machines are listed in
that folder's `machine.txt`.

All runs were on server a and used 64 connections. Each run had a 30 s warm-up, then twelve epochs
of 300 s of constant load from the load node. A SAMPLE line records the server process every 30 s.
Every epoch kept its 64 connections; no epoch failed and no server died.

## Throughput and latency over the hour

| scenario | side | mean / s | lowest epoch | highest epoch | p99 range (ms) | max (ms) |
|---|---|---:|---:|---:|---|---:|
| HTTP into Kafka | Cameleer | 49,530 | 48,641 | 50,306 | 2.11 to 2.28 | 17.6 |
| HTTP into Kafka | Camel JVM | 10,812 | 10,791 | 10,828 | 6.91 to 6.98 | 29.6 |
| HTTP into Kafka | Camel native | 10,010 | 9,985 | 10,035 | 10.68 to 11.34 | 27.5 |
| MCP session | Cameleer | 62,967 | 62,288 | 63,581 | 3.29 to 3.49 | 26.7 |
| MCP session | Camel JVM | 25,267 | 23,997 | 26,668 | 4.15 to 5.28 | 46.6 |
| MCP session | TypeScript SDK | 3,160 | 3,047 | 3,281 | 36.96 to 40.63 | 827.2 |

Most sides hold their throughput within about 2% across the hour:
- Camel on the JVM moves between 24.0k and 26.7k MCP calls/s from epoch to epoch, with no trend.
- The TypeScript SDK slows by about 5%, from 3,243 to 3,092 calls/s, and its p99 rises from 37.0 to
  39.6 ms.
- Its largest latency per epoch is 720 to 830 ms from epoch 2 on, while it was 64 ms in epoch 1. This
  points to garbage-collection pauses.

At this hour-long, steady load, Cameleer is 4.6x Camel on the JVM for HTTP into Kafka and 2.5x for the
MCP session. In the short runs of the same day these ratios were 4.3x and 2.7x.

## Server process over the hour

RSS was read 10 minutes in (after warm-up) and at the end.

| scenario | side | RSS at 10 min | RSS at 60 min | growth | threads | descriptors |
|---|---|---:|---:|---:|---:|---:|
| HTTP into Kafka | Cameleer | 30.6 MB | 31.9 MB | +1.3 MB | 76 | 87 |
| HTTP into Kafka | Camel JVM | 314.5 MB | 332.0 MB | +17.5 MB | 50 | 219 |
| HTTP into Kafka | Camel native | 107.6 MB | 110.1 MB | +2.4 MB (moves 105 to 116) | 36 | 103 |
| MCP session | Cameleer | 24.8 MB | 25.4 MB | +0.6 MB | 72 | 77 |
| MCP session | Camel JVM | 339.6 MB | 410.9 MB | +71.3 MB | 61 to 64 | 218 |
| MCP session | TypeScript SDK | 206.7 MB | 296.0 MB | +89.3 MB | 7 | 87 |

- **Cameleer.** Thread and descriptor counts are constant. RSS grows slowly and the growth slows down:
  - HTTP into Kafka: +0.58 MB in the first ten minutes of the measured period (minutes 10 to 20),
    +0.13 MB in the last ten (minutes 50 to 60).
  - MCP session: +0.6 MB over 50 minutes.

  This looks like allocator caches settling, not a leak. A longer run would be needed to rule out a
  slow leak completely.
- **Camel on the JVM.** RSS grows in steps, which is the heap expanding. It did not settle within the hour.
- **TypeScript SDK.** RSS grows steadily, about 18 MB every 10 minutes, and did not settle within the
  hour.
- **Thread counts.** Cameleer's 72 to 76 threads are:
  - four event loops;
  - the offload and routing pool;
  - for HTTP into Kafka, the Kafka sender threads.

  The count does not grow.

## Stopped run before the keep-alive fix

`soak-http-kafka-rust-before-keepalive-fix-stopped.txt` is the first Cameleer HTTP-into-Kafka soak. It
ran on the build before the keep-alive fix (PR #78) and was stopped after 10 epochs to rerun on the
fixed build.

The load generator reconnected about 14,800 times per epoch, against 64 after the fix. Throughput
was about the same (49,262 against 49,530 req/s mean), and so were p99 (2.2 ms on both) and RSS
(33 to 34 MB). So for this scenario the reconnects had cost almost nothing. The gain from the fix was
in the plain HTTP scenarios; see `../2026-11-23-linux-main-beb3385-camel/README.md`.

## Files

- `soak-<scenario>-<side>.txt`: the RESULT line of each epoch, the SAMPLE lines (RSS, threads,
  descriptors, CPU ticks), the PROC record (peak RSS, CPU), and a SUMMARY line.
- Reproduce with `SOAK_MINUTES=60 scripts/cloud-bench/soak.sh <scenario> <side> a` after
  `droplets.sh up` and `run.sh setup` (see `scripts/cloud-bench/README.md`).
