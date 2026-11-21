# Cameleer 0.8.0 candidate against Apache Camel, one session, 2026-11-20

Session B of section 1.9. `run.sh bench` with sides `java rust native node` on the droplets of session A (see
`../2026-11-20-linux-0.8.0-candidate/README.md` for region `sfo2`, machines and the build; `machine.txt` here). Cameleer
is `7ee75e7` (the candidate). All sides ran in this one session, the servers swapped roles halfway; 15 s warm-up, 5 x 5 s,
64 connections, 30,000,000 Kafka records. Camel 4.23.0 on Temurin 21.0.13, Camel Quarkus 3.40.0 native (Camel 4.22.1,
Mandrel 25.0.4.1), the official MCP TypeScript SDK 1.32.2 on Node 22.23.4 (zod 4). Versions pinned as before.
No case failed. Files: `results.txt`, `summary.json`, `tables.md` (made by `scripts/cloud-bench/summarize.py`).

I ran the Camel sides although "Cameleer only" was the default, because setup builds them anyway and the owner approved
(through the session that started this one); the figures no longer mix sessions.

## Headline

Cameleer is ahead of Camel on the JVM in every scenario (`tables.md`): 1.8x (HTTP) to 6.8x (seda request-reply);
Kafka stream processing 15.2x (10,104 against 662 records/s); startup 12.3 ms against 1,127 ms (native 27 ms); peak
memory 8 to 57 MB against 195 to 435 MB.

**Kafka p99.** The p99 "loss" is gone: HTTP into Kafka p99 3.5 ms (Cameleer) against 8.1 ms (JVM) and 13.0 ms
(native), HTTP into PostgreSQL 9.4 against 11.3 and 22.8 ms. The Kafka *stream* scenario has no latency figure for
any side. Cameleer's CPU is higher than Camel's only for HTTP into Kafka (202% against 144% JVM, 166% native).

**MCP** (`mcp-session`, mean of two servers): Cameleer 43,254 calls/s, p99 4.36 ms, 22 MB; Camel `camel-mcp-server`
20,968 calls/s, p99 5.57 ms, 281 MB; TypeScript SDK 2,179 calls/s, p99 56.6 ms, 198 MB. That is **2.06x Camel and
19.9x the SDK** (the 2026-11-12 run on 0.5.1: 39,922, 22,604 and 2,257 calls/s, 1.77x and 17.7x); Cameleer's p99 is now
lower than Camel's too (it was higher). Stateless MCP tool call on Cameleer: 41,532 calls/s, p99 4.62 ms. `ai-tool`
envelope: 125k calls/s through a route, 1.24 million directly.
