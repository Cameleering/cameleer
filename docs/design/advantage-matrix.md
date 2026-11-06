# Advantage matrix: why Cameleer, per feature and use case

Internal working document (not on the site). The owner's bar: for every feature and use case
there must be a compelling reason to pick Cameleer over Apache Camel **and** over writing it by
hand, and the gain must be significant, not incremental. Each row names the evidence and a
verdict:

- **significant**: a large, measured or structural gain (several times faster or smaller, or a
  guarantee the alternative does not give at all);
- **incremental**: better, but not enough to change anyone's decision;
- **gap**: no advantage yet, or the alternative is better.

Every row that is not **significant** on both sides has an item in [Gaps and work](#gaps-and-work).
Numbers come from `docs/BENCHMARKS.md` (DigitalOcean c-4 droplets, 2026-11-06) unless a row says
otherwise; rows marked *(droplet run 2)* are filled from the run in `target/cloud-bench/`.

## Integration runtime

| feature / use case | vs Apache Camel | vs by hand | verdict |
|---|---|---|---|
| Routing and EIPs in process (direct, seda, cbr, split/aggregate) | 3.1–6.1x throughput, a third of the latency, up to 54x less memory | the same EIP semantics hand-written are hundreds of lines per pattern; per-message cost is about 1 µs | significant / significant (productivity) |
| Startup | 3 ms vs 1.1 s (JVM), 30 ms (Quarkus native) | same order as a hand-written Rust service | significant / parity |
| HTTP, fixed reply | **1.67x** throughput (112,910 vs 67,682 req/s), 16x less memory (17 vs 276 MB) | 68% of a do-nothing Rust server (166,374 req/s on the same droplets) | **incremental** on throughput, significant on memory / gap: 1.5x headroom |
| REST API with JSON | 2.1x throughput, p99 2.6 vs 6.3 ms | | significant (latency) / productivity |
| HTTP into Kafka | **5.0x** throughput (48,800 vs 9,800 req/s), p99 2.4 vs 7.9 ms (was 1.7x with a worse p99; fixed by requests in flight on several connections) | | significant |
| HTTP into PostgreSQL | 2.3x throughput, p99 halved | | significant |
| Kafka stream processing | 29x throughput (17,881 vs 620 records/s); 1.8x `main` | | significant |
| CSV and file batches | 2.1x | | incremental–significant |
| Serverless (AWS Lambda) | run-once job done in 1.4 ms vs 1.1 s JVM; 12.5 MB image | a hand-written Rust Lambda is similar; Cameleer adds 230 components and Camel's DSLs | significant / productivity |
| Edge and IoT (MQTT, HL7/MLLP, syslog) | 4.2 MB RSS (static musl, core) vs 100+ MB JVM | | significant / productivity |
| Binary and image size | 5.8 MB core, 24 MB full vs a JVM plus jars | | significant |

## Action layer for AI agents

| feature / use case | vs Apache Camel 4.23 (`camel-ai-tool`, `camel-mcp-server`) | vs by hand (MCP SDKs, agent frameworks) | verdict |
|---|---|---|---|
| Tool envelope (schema checks, deduplication, approvals, rate limits, breakers, timeouts that cancel, compensation, hash-chained audit, pinning, kill switch, redaction) | Camel: required-argument checks, advisory hints, timeouts that don't stop the route, denial events; no deduplication, approvals, rate limits, compensation or audit log | each guarantee is code the author writes and tests; the SDKs give none of them | significant / significant |
| Envelope cost | | 6 µs per call on a Xeon 8280 without SHA instructions, 3.0 µs on a Ryzen 5 5600X with every check, audit record and hash on (was 90 µs: a thread per call, JSON-object audit records, software SHA-256); hand-written checks of the same kind cost the same order, Python/TypeScript validators more | significant (the envelope is affordable on every call) |
| MCP server: protocol | both eras on one endpoint (2026-07-28 stateless and 2025-11-25 sessions), stdio with resources, progress, logs, elicitation, sampling; Camel: sessions only, stdio tools only | the SDKs implement the protocol; the envelope is still missing | significant / significant |
| MCP server: auth | API keys, JWT (JWKS, HMAC), introspection, RFC 9728/8707 built in; Camel: platform-http basic or JWT, OAuth through camel-oauth | hand-written per server | significant / significant |
| MCP server: throughput, latency, memory | session calls 41,620/s vs 22,458/s (1.85x), p99 5.9 vs 5.2 ms, 53 vs 317 MB (6x less); stateless calls 51,700/s (Camel has no stateless mode) | 27x the official TypeScript SDK (1,536/s, p99 79 ms, 214 MB) | significant on memory, throughput and vs by hand; p99 at parity with Camel |
| MCP server: horizontal scale | stateless 2026-07-28 calls need no sticky sessions; Camel keeps sessions in memory | | significant |
| MCP conformance | official suite 39/39 (2025-11-25), 26/26 (2025-06-18) | | evidence |
| MCP client and gateway | Camel has no MCP client | one endpoint, one auth, one audit log, per-server and per-tool rate limits, breakers, authorize and redaction, rug-pull protection; separate gateway products add a network hop | significant / significant |
| Agent step | Camel has no agent EIP with approvals that pause a conversation, record/replay or a guarantee that a retried model turn doesn't repeat a tool call | frameworks leave idempotency, approvals and audit to the author | significant / significant |
| `cameleer test` with recorded models | | deterministic agent tests in CI without a model | significant |
| Tool calling in model components | Camel 4.23 `openai`/`llm` options mirrored; Cameleer adds anthropic, gemini, bedrock, vertex, azure with the same loop | | significant (breadth with one loop) |
| Model providers | Camel reaches many providers through LangChain4j | 23 out of the box; any OpenAI-compatible server | **parity** (not a reason on its own) |
| Retries, fallbacks, response cache, coalescing | Camel: redelivery policies, no model-aware cache or coalescing | LiteLLM, Portkey and Cloudflare do this as a separate proxy (a network hop, ms); Cameleer does it in process | significant / *(to measure: in-process overhead)* |
| Prompt caching | automatic Anthropic breakpoints; Bedrock cache points | by hand in every request | significant (cost: cached input at about a tenth of the price) |
| Pooled keep-alive connections to providers | | the SDKs pool too | parity / parity |

## Gaps and work

1. **HTTP fixed reply, 1.67x.** Measured on the droplets: one `recv` and one `send` per request
   already; a do-nothing thread-per-connection server reaches 166k req/s, Cameleer 113k, so the
   rest is the request path and scheduling 64 threads. Next: an event-driven core. Thread-per-connection server. Options: an event-driven server
   (epoll on Linux, IOCP on Windows), zero-copy response writes, fewer allocations per request.
   Measured before: the server is at about 70% of a do-nothing server on Windows; the ceiling on
   the droplets is not known. Measure the do-nothing ceiling on the droplets first
   (`cameleer-bench raw-http`).
2. **Done: HTTP into Kafka** is 5.0x with a third of Camel's p99. Was: **p99 worse than Camel.** Find what dominates the tail (waiting for a
   batch, the broker round trip, or one connection per broker shared by every sender) and fix
   it so p99 drops below Camel's while throughput stays ahead.
3. **Envelope cost: done.** 90 µs → 3.0 µs per call (Windows, Ryzen 5 5600X): pooled workers and,
   for tools whose calls finish well within their timeout, the caller's thread with a deadline
   on the exchange; audit records written as canonical JSON directly; SHA-256 with the CPU's SHA
   instructions; JSON string escaping copies runs instead of characters (helps every JSON
   route); the kill-switch property cached. **MCP throughput** is measured in droplet run 2.
4. **In-process LLM overhead**: measure a model call through `openai:` against a local mock vs a
   raw HTTP call, to back the "no hop" claim with a number.
5. **Model providers** are at parity with Camel through LangChain4j; the reason to choose
   Cameleer there is the in-process reliability and caching layer, not the count.
