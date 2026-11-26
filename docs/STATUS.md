# Status and pending work

_Last updated: 2026-11-24 (release 0.8.0; the section "0.8.0 candidate" below holds its measurements).
Branch `main`._

## Name, packaging and release (2026-11-05)

- **Renamed rust-camel to Cameleer** after v0.1.0. Another active project, kennycallado/rust-camel,
  uses the same name and owns `camel-core` and most `camel-*` names on crates.io; "CAMEL" is also
  the CAMEL-AI agent framework. Crates are now `cameleer-*` (module paths `cameleer_*`), the repo
  is `cameleering/cameleer` (GitHub redirects the old URL), the site is
  https://cameleer.ing/. The CLI is `cameleer` (no `camel` binary: that is Apache Camel's CLI name).
- **Container image:** `cameleering/cameleer` on Docker Hub and `ghcr.io/cameleering/cameleer`
  (linux/amd64, linux/arm64), a glibc binary on distroless/cc (static musl measured 3-17% slower:
  direct -3%, seda InOut -10%, cbr -8%, splitagg -17%, same container, 2 rounds), built by `.github/workflows/container.yml` on `v*` tags (manual runs publish
  `edge`).
- **crates.io:** 19 `cameleer-*` crates at the 0.1 release (the workspace now has 22 publishable library crates, plus the
  `cameleer` wrapper in `publish/`; crates.io not re-checked for this count), published with `scripts/publish-crates.sh` (dependency
  order, skips versions already there, waits out crates.io's new-crate rate limit of about one
  per 10 minutes). `cameleer-bench` is `publish = false`. The account needs a verified email.
- **Docker Hub:** `cameleering/cameleer`. CI pushes there when the `DOCKERHUB_TOKEN` secret is set;
  `.github/workflows/dockerhub-description.yml` syncs `docker/README.dockerhub.md` as the overview.
- **Deployment guide:** `docs/DEPLOYMENT.md` (Docker, Compose, Kubernetes, Cloud Run, AWS Lambda,
  smaller builds). Images run
  with `CAMEL_LOCAL_STATUS=false` (no status or log files) and work with a read-only root filesystem.

- **0.3.0: `cameleer validate` and `cameleer schema`** (`crates/cameleer-main/src/validate.rs`,
  `schema.rs`, DSL tables in `crates/cameleer-yaml/src/model.rs`, `CamelContext::check_endpoint`).
  Codes: yaml-syntax, xml-syntax, dsl-structure, endpoint, expression, unresolved-placeholder,
  dangling-endpoint, duplicate-route-id; text or JSON output; exit 0/1/2. A test validates every
  doc example and every file in `examples/`. Guides: `docs/guides/yaml-dsl.md`,
  `docs/guides/yaml-for-coding-agents.md`, `docs/guides/validate.md`, and `AGENTS.md`.
- **0.4.0: serverless and smaller builds** (2026-11-06). AWS Lambda custom runtime in
  `cameleer run`; every component family plus `templates`, `jq`, `xpath`, `http2` and `validate`
  as cargo features of `cameleer-main` (`http2` was removed again in 0.6.1: HTTP/2 is in every build); fewer idle threads (on-demand pool workers, one timer
  thread per timer name, XQuery/XSLT on the caller); the TLS alert fix. Release notes are in
  `CHANGELOG.md`. Feature docs: `docs/guides/build-features.md`, and every component, language
  and data format page names its feature in its header (`**Cargo feature:**`, checked by
  `crates/cameleer-main/tests/features.rs` `reference_pages_name_their_cargo_feature`).
- **0.5.0: the action layer for AI agents** (2026-11-09). `ai-tool`, `ai-resource`, `ai-prompt`
  with the reliability envelope; the MCP server (both eras) and the MCP client and gateway; the
  `agent` step and tool calling in every model component; `anthropic` and `gemini`; provider
  presets and call options; `cameleer test` and `cameleer tool`; the MCP field test and the
  `examples/agents/mcp-shop` example. Release notes in `CHANGELOG.md`; the sections below give
  the details round by round.
- **Open findings from the documentation pass** are GitHub issues #2-#15 (Simple numeric string
  comparison, http producer overwriting the message on failure, S3 bucket header, webhook
  signatures, vault env vars, YAML `dataFormats`, `cameleer run <directory>`, ...).

## Where things stand

The implementation is complete against the feature list: `docs/FEATURES.md` has 231 rows, all
✅. Results of the last full run, all in one container session:

| check | result |
|---|---|
| `cargo build --workspace` | 0 warnings |
| hermetic `cargo test --workspace` | **2249 passed, 0 failed** |
| live real servers (`CAMEL_LIVE_INTEROP=1`) | **74 / 0** |
| official clients (`CAMEL_OFFICIAL_CLIENTS=1`) | **58 / 0** |
| third-party simulators (`CAMEL_SIMULATORS=1`) | **35 / 0** |

The hermetic suite has grown since that run: **2697 passed, 0 failed** on Windows 11 and on
Linux (WSL2) at the end of the FaaS round (2026-11-05), with zero warnings.

Independent verification found and fixed about 30 interoperability bugs. They're listed in
`docs/INTEROP.md`, and each has a hermetic regression test.

### What each area was verified against

- **Real servers:**
  - PostgreSQL 16, Redis 7, Mosquitto
  - RabbitMQ with AMQP 0-9-1, STOMP, AMQP 1.0 and MQTT
  - ActiveMQ 5.17.6 with STOMP, MQTT and AMQP 1.0
  - vsftpd (FTP and FTPS), Postfix + Dovecot (SMTP, IMAP, POP3), nginx, OpenSSL `s_client`
    and `s_server`, JDK JSSE
- **Official clients against Cameleer servers and emulators:**
  - librdkafka and kcat, pymongo, the DataStax Cassandra driver, elasticsearch-py, redis-py
  - boto3, the Azure SDK (storage and AMQP), gcloud, gsutil and bq
  - grpcio, Go `crypto/tls` and `net/http` (HTTP/2)
- **Third-party simulators:**
  - moto 5.2.5 (AWS), Azurite 3.37.0
  - fake-gcs-server 1.53.1, pstest (Pub/Sub over gRPC)
  - FerretDB 1.24.2 on PostgreSQL (MongoDB wire), kfake (Kafka wire)

## FaaS and efficiency round (2026-11-05, branch `performance-improvement`)

Research in `benchmarks/perf-research.md`, review with baselines in `benchmarks/perf-review.md`,
results in `docs/BENCHMARKS.md` (section "FaaS cold start and size") and
`benchmarks/analysis.md`. Linux, `dist` builds, against `main`:

- **Cold start:** HTTP route ready 3.27 → 1.17 ms (all components) and 0.69 ms (static musl,
  `core`); timer route 1.52 → 0.66 ms (musl `core`); a run-once job exits in 1.4–2.2 ms instead
  of 22 ms. Windows: HTTP route 12.1 → 10.3 ms, most of it process creation (a hello-world takes
  4.7 ms there).
- **Size:** 24.9 → 23.9 MB with every component; 11.7 MB with `--features cloud`, 7.3 MB with
  `core` (cargo features per component family in `cameleer-main`; `FEATURES` build arg in the
  Dockerfile). Then (2026-11-06, review finding 15) the template components, jq, the core XPath
  subset, HTTP/2 (and with it `cameleer-grpc`) and `validate`/`schema` became features too
  (`templates`, `jq`, `xpath`, `http2`, `validate`, all in `full`): `core` is now 5.82 MB glibc
  and 5.95 MB static musl (was 7.33 / 7.49 MB, −21%); `full` is unchanged in size and
  throughput. Details in `benchmarks/analysis.md`. Tests on Windows: `cargo test --workspace`
  2697 passed, 1 failed (the known TLS 1.3 alert-timing flake below, 5/5 on rerun);
  `cargo test -p cameleer-main --no-default-features` 29 passed (one test per gate).
- **Memory:** HTTP route RSS 15.2 → 10.4 MB (all components) and 4.2 MB (musl `core`); private
  memory 1.71 → 0.72 MB; idle HTTP threads 34 → 2.
- **Container image:** 20.5 → 20.4 MB compressed with every component, 14.5 MB with
  `FEATURES=cloud`, 12.5 MB with `FEATURES=core`; a running HTTP route uses 2.6 MiB and 3 threads
  (was 4.5 MiB and 34).
- **AWS Lambda:** built-in custom runtime (`cameleer_main::lambda`, `scripts/package-lambda.sh`),
  checked end to end in AWS's `provided:al2023` image with the Runtime Interface Emulator (init
  3.5–5.2 ms there). Not yet measured on the Lambda service itself (needs an AWS account).
- **Throughput:** unchanged within noise after fixing two regressions a bisect found (log macro
  level check, allocator bin size). Static musl is 3–22% slower, so the image stays glibc.
- **DT_RELR (2026-11-06):** the amd64 container build links with `-z pack-relative-relocs`:
  binary −400 KB (core −171 KB), peak RSS −0.48 MiB (core −0.13 MiB), start-up unchanged
  (measured inside the images, 4×40 interleaved runs), compressed image −33 KB. Needs glibc ≥ 2.36,
  so it is set only in the Dockerfile (Debian 12 runtime), never for the musl Lambda zip.
- **Tests:** hermetic suite 2697 passed, 0 failed on Linux (WSL2) and on Windows, zero warnings;
  `cargo test -p cameleer-main --no-default-features`: 24 passed. One earlier Windows run under
  heavy load failed `cameleer-tls` `mutual_tls_rejects_missing_and_untrusted_certificates` once
  (untouched by this branch; 15/15 on rerun, green in the final run). Fixed since: a library bug,
  see the TLS alert fix below.

Follow-ups: `-z pack-relative-relocs` for the arm64 image (Debian 12's
aarch64 ld ignores it; needs a newer binutils or lld in the builder); measuring
on the Lambda service across memory sizes and arm64.

**TLS alert lost to a TCP reset (2026-11-06, fixed).** The flake above was a library bug. A TLS 1.3
client sends application data right after its Finished; when the server rejected the client
certificate it sent the alert and dropped the socket, the late data drew a TCP reset, and on Windows
a reset discards the unread alert, so the client saw "connection reset" instead of
`certificate_required`. Servers now close gracefully after a fatal alert (FIN, then drain until the
peer closes, at most 2 s): `cameleer_tls::net::lingering_close` and `accept_tls`, used by
`NetStream::server`, the gRPC server and the cloud mock servers. Code that calls `TlsStream::accept`
on a raw `TcpStream` should use `accept_tls`. Regression test
`alert_survives_a_client_that_reads_late`; details in `docs/INTEROP.md` bug 18.

## Idle threads round (2026-11-06, branch `claude/mystifying-swartz-034e67`)

Perf-review finding 16, details and tables in `benchmarks/analysis.md` (section "Idle threads and
per-evaluation threads"); measured with the `idle_threads` example (`scripts/idle-threads.sh` on
Linux), old and new builds alternately on Windows 11 and WSL2 Ubuntu 24.04:

- **XQuery/XSLT** evaluate on the caller's thread and move to a new stack segment only when less
  than 512 KiB of stack is left (`cameleer_core::stack::maybe_grow`). Per evaluation: XQuery
  110 → 10 µs (Windows), 142 → 6.5 µs (Linux); XSLT 103 → 6.3 µs, 138 → 4.0 µs. Deep recursion
  still works from a 2 MiB thread; XQuery recursion 2000 deep is about 30% slower on Linux
  (unchanged on Windows), not traced further.
- **timer:** one thread per timer name (Camel's `java.util.Timer` per name): 20 routes on one
  name use 1 thread instead of 20; the thread exits after the last consumer.
- **Supervising route controller:** its thread exits when nothing is left to retry (1 → 0 threads
  after a recovery).
- **Local status file:** written only when the content changes (10 → 1 writes in 5 s idle);
  readers refresh uptimes from `updated`, and liveness comes from the operating system.
- **Tests:** hermetic suite 2712 passed, 0 failed, zero warnings, twice on Windows 11 and once on
  WSL2 Ubuntu 24.04 (Linux caught a unix-only compile error in the first build).

## Action layer for AI agents, phase 1 (2026-11-06, branch `agent-action-layer`)

Design and decisions in `docs/design/agent-action-layer.md` (internal, not on the site). Phase 1
of 5 is done: the tool model and the reliability envelope.

- **New crate `cameleer-ai`**, feature `ai` (in `full`): `ai-tool`, `ai-resource`, `ai-prompt`
  and `AiRegistry` (bound in the context as `cameleer.ai.registry`). Camel option names and
  semantics for `ai-tool`; producers, URI templates and `ai-prompt` are extensions.
- **Envelope** (`crates/cameleer-ai/src/registry.rs`), applied in this order: kill switch,
  `authorizationPolicy`, argument filtering and JSON Schema 2020-12 validation, rate limit,
  circuit breaker, approval (parked calls with handles; `approve`/`deny`), idempotency
  (concurrent repeats wait for the first call), compensation registered before the call,
  the call on a worker with a timeout that cancels the route, output schema check, audit.
- **Core changes:** `Exchange` has a cooperative cancel token (`set_cancel_token`,
  `is_cancelled`, checked by `should_continue`, shared by copies); `json-validator` gained
  `prefixItems`, `minContains`/`maxContains`, `dependentSchemas`, `unevaluatedProperties`,
  `unevaluatedItems`.
- **Docs:** `docs/components/ai-tool.md`, `ai-resource.md`, `ai-prompt.md`; agents guide
  section; FEATURES rows; `examples/agents/ai-tools.camel.yaml`. `docs/design/` and
  `docs/website-plan.md` are excluded from the site, Sphinx and the doc harness.
- **Tests:** `crates/cameleer-ai/tests/tools.rs` proves each guarantee (a retried call runs
  once, a rejected call runs nothing, a timeout cancels the route, a failed conversation
  compensates newest first, a changed tool is refused, an edited audit record is detected);
  `crates/cameleer-core/tests/cancel.rs`. Hermetic suite on Windows 11: 2756 passed, 0 failed,
  zero warnings; `cameleer-main --no-default-features` builds.
- **Next phases:** 2, the MCP server (2026-07-28 and 2025-11-25, Streamable HTTP and stdio,
  approvals over MRTR/elicitation, `camel.server.mcp-*`); 3, tool calling in the model
  components plus the `agent` step; phases 1 to 3 ship as 0.5.0. Not done in phase 1:
  exporting audit records as span attributes.

## Action layer for AI agents, phase 2: MCP server (2026-11-06, branch `agent-action-layer`)

- **`cameleer_ai::mcp`**: `McpServer` (protocol engine), `http` (Streamable HTTP on the shared
  server of `camel.server.port`), `stdio`, `auth`, `sse`. Started by Main when
  `camel.server.mcp-enabled=true`; `cameleer run --mcp` / `--mcp-stdio` (these default
  `camel.server.mcp-tags` to `*`; a config file must choose tags, nothing is exposed otherwise).
- **Dual era on one endpoint**: 2026-07-28 (stateless `_meta`, `server/discover`,
  `resultType`, `input_required` approvals with an AES-256-GCM `requestState` bound to
  principal, tool, argument digest and expiry, single use through the parked approval handle,
  `subscriptions/listen`, `ttlMs`/`cacheScope`, `Mcp-Method`/`Mcp-Name`/`MCP-Protocol-Version`
  checks, -32020/-32021/-32022) and 2025-11-25/06-18/03-26 (`initialize`, `Mcp-Session-Id`,
  GET stream, `DELETE`, `ping`, `resources/subscribe`, `logging/setLevel` accepted,
  `elicitation/create` approvals on the response stream or stdout).
- **Auth** (HTTP): API keys, JWT (JWKS RS/PS/ES or HMAC), RFC 7662 introspection, RFC 9728
  metadata, RFC 8707 audience; `Origin` check; principal in `CamelMcpSecurityPrincipal*`.
- **Registry additions**: typed change events (`subscribe`/`unsubscribe`, `Change`),
  `resource_updated` (also `ai-resource:x?operation=updated`), completion values
  (`argument.NAME.values`, `values.NAME`), `ToolCall.headers`, rich content
  (`CamelAiToolContent`, prompt content blocks), `camel.server.mcp-tool-timeout` default.
- **Fix found by the end-to-end test**: the `cameleer` binary held the stdout lock for the whole
  command, so another thread could never write; it now uses the unlocked handle.
- **Tests**: hermetic suite 2776 passed, 0 failed on Windows 11, zero warnings; `crates/cameleer-ai/tests/mcp.rs` (12, real HTTP and stdio in both eras),
  `crates/cameleer-main/tests/mcp_main.rs` (Main over HTTP, the binary with `--mcp-stdio`).
  `scripts/mcp-conformance.sh`: official suite 0.1.16 passes with the baseline, 25/31 for
  2025-11-25 and 22/26 for 2025-06-18; the gaps are log and progress notifications during a
  call, sampling, and elicitation other than approvals. The suite predates 2026-07-28, so that
  era is covered only by the hermetic tests.
- **Done since**: progress and log notifications, sampling and elicitation (conformance now
  39/39 and 26/26), the website pages (`/use-cases/mcp-tool-server/`,
  `/compare/camel-mcp-server-vs-cameleer/`, `/use-cases/llm-gateway-in-process/`).

## Action layer for AI agents, phase 3: tool calling and the agent step (2026-11-06, branch `agent-action-layer`)

- **Core** (`cameleer_core::ai`): the neutral model (`ChatMessage`, `Part`, `ToolSpec`,
  `ChatRequest`/`ChatResponse`), `ChatModel` (`Endpoint::chat_model`), `ToolProvider`
  (bound by cameleer-ai as `cameleer.ai.tools`), `run_agent`/`resume_agent`, `ToolLoopOptions`
  and `chat_exchange`, `ReplayModel`/`RecordingModel`, and `ai::wire` converters for OpenAI Chat
  Completions, Bedrock Converse, Gemini (keeps `thoughtSignature`) and Anthropic Messages.
- **The `agent` step**: `cameleer_core::processor::agent::AgentDefinition`, Rust DSL `.agent(..)`,
  YAML `agent:` (model table in `cameleer-yaml/src/model.rs`); memory in an
  `AggregationRepository`; approvals pause the conversation (state in memory) and resume from
  `AiRegistry::approval_result`; `ai-tool:x?operation=approve|deny`.
- **Providers**: `openai`/`llm` (Camel headers `CamelOpenAITool*`), `anthropic` (new,
  `cameleer-saas`), and `azure-openai`, `aws-bedrock`, `google-vertexai` through
  `CloudService::chat` in the generic cloud endpoint (`CamelAiAgent*` headers). Every mock
  scripts tool calls from `call NAME {json}` and enforces its provider's call/result pairing.
- **`cameleer test`** (`crates/cameleer-main/src/testing.rs`): YAML test files (`*.test.yaml`,
  skipped by `run` and `validate`); example `examples/agents/support-agent.camel.yaml` with
  `tests/support-agent.test.yaml` and `tests/refund.jsonl`.
- **Tests**: hermetic suite 2793 passed, 0 failed on Windows 11, zero warnings; `crates/cameleer-main/tests/agent.rs` (every provider, the approval pause and resume,
  YAML with record and replay), `cameleer_test.rs`, unit tests in `ai.rs` and `ai/wire.rs`.
- **Done since** (see the 2026-11-06/07 section below): streaming with the tool loop, the
  Responses API, Ollama `/api/chat`, MCP tools in `tools` (the MCP client), the website pages.

## Action layer: kill switch and approvals for operators (2026-11-06)

- **`cameleer tool list|disable|enable|approvals|approve|deny [name|pid]`** (`crates/cameleer-main/src/tools.rs`,
  `cli.rs`): the running process applies `AiRegistry::disable/enable/approve/deny` through the local
  status files (new `tool-*` action documents; the action result gains an optional `data` field,
  the existing format is unchanged). `--json` for `list` and `approvals`; unknown tool names and
  handles are refused. `AiRegistry::all_tools` and `disabled_tools` are new.
- **Dev console**: `GET /q/dev/tools`, `POST /q/dev/tools/{name}/disable|enable` (501 without the
  `ai` feature).
- **Tests**: `crates/cameleer-main/tests/tool_cli.rs` (the real binary: list, disable, enable,
  an approval parked by a timer route, approve; the console in-process), unit tests in
  `tools.rs`, and `features.rs` `tool_commands_name_the_ai_feature` for builds without `ai`.

## Action layer: MCP client, providers, call options, and a measured advantage per feature (2026-11-06/07, branch `agent-action-layer`)

The owner's bar for this round: every feature needs a significant, measured reason to choose
Cameleer over Apache Camel and over doing it by hand. Per-feature verdicts and the open gaps are
in `docs/design/advantage-matrix.md` (internal).

- **Finished from the earlier phases:** streaming in the tool loop, the OpenAI Responses API and
  Ollama's `/api/chat` (merged branch); the MCP client and gateway (`camel.ai.mcp.NAME.*`, both
  eras, HTTP and stdio, `list_changed` re-sync, rug-pull protection, `ai-tool` envelope options
  per server and per tool, re-sync keeps unchanged tools); the `mcp:` component; progress, logs,
  elicitation and sampling from tool routes; guardrails (`redact`, `authorize`, `cacheTtl`);
  official MCP conformance 39/39 (2025-11-25) and 26/26 (2025-06-18), checked again on
  2026-11-06.
- **Model providers:** `gemini` component (Gemini API keys, thought signatures, `GeminiMock`);
  `provider=` presets for 19 OpenAI-compatible services; `max_completion_tokens` for o-series and
  GPT-5; `reasoning_content` sent back. Guide `docs/guides/model-providers.md`.
- **Call options on every model component** (`cameleer_core::ai::reliable`): `maxRetries`,
  `retryDelay`, `fallbackModels`, `responseCache=exact`, `coalesce`; prompt caching (`anthropic`
  `cache_control` by default, `aws-bedrock` `cachePoint` with `promptCaching=true`); cache-token
  usage for every provider (`CamelAiCacheReadTokens`, `CamelAiCacheWriteTokens`).
- **Camel parity:** `hallucinatedToolNameStrategy` takes `failExchange`/`repromptModel`,
  `maxAgenticTokens=0` is unlimited, `parallelToolExecution` defaults to `false`.
- **Performance work found by the measurements:**
  - tool envelope 90 → 3 µs per call (pooled workers; fast tools on the caller's thread with a
    deadline on the exchange, `Exchange::set_deadline`; audit records written as canonical JSON
    directly; SHA-256 with the x86 SHA instructions; JSON escaping by runs; the kill-switch
    property cached through `PropertiesComponent::generation`);
  - MCP session calls 9k → 41.6k/s on the droplets (no thread per call, idle-session sweep at
    most once a second);
  - Kafka producer: up to `maxInFlightRequest` (5) requests in flight on their own connections;
    HTTP into Kafka 21.7k → 48.8k req/s, p99 7.2 → 2.4 ms.
- **Measured on DigitalOcean** (3 × c-4, `scripts/cloud-bench`, new sides: Camel's
  `camel-mcp-server`, the official MCP TypeScript SDK, and `main` as a baseline; results in
  `docs/BENCHMARKS.md` "Action layer and MCP" and
  `benchmarks/results/2026-11-06-linux-agents/`): MCP session calls 39,922/s vs Camel 22,604/s
  and the TypeScript SDK 2,257/s, at 56 MB vs 289 MB and 197 MB; no integration scenario slower
  than `main`.
  **Rerun 2026-11-12** (Cameleer 0.5.1 at `306a5df`, `benchmarks/results/2026-11-12-linux-mcp/`):
  the TypeScript baseline pinned zod 3.25.76, and zod 4 is 1.62x faster on one droplet (2,571 vs
  1,588 calls/s, three alternating rounds each), so the baseline moved to zod 4 and the whole
  comparison was rerun in one session: Cameleer 39,922/s, Camel 22,604/s, the TypeScript SDK
  2,257/s (1.77x Camel, 17.7x the SDK, not 27x), at 56, 289 and 197 MB; Camel's p99 is lower
  (5.2 vs 6.3 ms). The published figures in README, BENCHMARKS, the MCP guide and the site follow
  this run.
- **Tests:** hermetic suite on Windows 11: 2833 passed, 0 failed, zero warnings (plus the new
  deadline and inline-timeout tests). On Linux (the droplet, Ubuntu 24.04): zero warnings, 2832
  passed, 1 failed at first: `aws_gaps::kinesis_enhanced_fan_out_over_http2`, also on `main`.
  The cause was a real HTTP/2 bug, not the mock: a connection the peer closed kept its writer
  thread and socket, so every closed connection leaked a thread and two descriptors until the
  process ran out (`docs/INTEROP.md` bug 19, fixed in `cameleer-grpc` with a regression test).
- **Open gaps (see the matrix; written 2026-11-07, the event-driven server core shipped in 0.6.1):** plain HTTP was 1.67x Camel and 68% of a do-nothing server on the
  same machines (HTTP is now 2.0x Camel on the JVM, 2026-11-23 round); MCP per-request CPU is about twice
  a plain HTTP request; on CPUs without SHA instructions the envelope costs 6 µs.

## MCP field test with Claude (2026-11-07, branch `agent-action-layer`)

Plan in `docs/design/mcp-field-test/plan.md`. An online shop's back office as an MCP server
(`examples/agents/mcp-shop`, 19 tools, policies, prompts) and a harness (`scripts/mcp-fieldtest`,
the official MCP Python SDK plus Claude, admin routes in `admin.camel.yaml`) that runs 40 real
requests, answers approval forms and size questions as a user, decides manager approvals between
turns, and grades each run by tool calls, database state and a Claude judge.

- **Results:** Claude Opus 5.5 157/160 in 2025-11-25 sessions and 39/40 stateless (2026-07-28),
  about 2.9 turns and 3.3 tool calls per request, about $0.026 per request; Claude Haiku 5.5
  104/120 before the wording fixes, 25/33 on reruns of its failed cases after them. Records in
  `target/mcp-fieldtest/results/` (not committed).
- **Fixed from the findings:** `camel.server.mcp-resource-tool`; `CamelAiToolIsError` refusals;
  Accept approves; sanitized errors after approval; out-of-band approvals answered by calling
  again; deduplication scoped to the conversation; `CamelAiToolApprovedBy`; rejection text says
  nothing ran; SQL date arithmetic.
- **Showcase:** `docs/guides/mcp-shop-example.md` and the example's README.
- **Open:** a hands-on pass in Claude Desktop (the config is written locally, pointing at a copy
  of the release binary in `target/mcp-fieldtest/bin`); Haiku's remaining misses are weekday and
  sum arithmetic.

## Packs and the health pack (2026-11-09 to 11, merged into `main` 2026-11-11)

The owner chose a health pack instead of merging DICOM into `full`, with no backward
compatibility (design: `docs/design/packs/domain-packs.md` section 8). Done on this branch:

- **Core:** `cameleer_core::pack` (`PackInfo`, `Pack`, `CamelContext::add_pack`, `pack::install`),
  `Main::add_pack`; pack languages are YAML/XML DSL keys (cameleer-yaml asks the pack registry),
  `cameleer version` lists packs; the audit log moved to `cameleer_core::audit` (one per context);
  `util_time::now_utc`; socket helpers in `cameleer_tls::net`; `cameleer_http::client::EndpointClient`.
- **`cameleer-health` 0.6.0** (own version): `hl7` (hl7, hl7terser, mllp from cameleer-protocols),
  `fhir` (from cameleer-soap), `dicom` (this branch's DICOM work); depends on core, http, tls and
  (fhir) xml only. No aliases or re-exports left in the old crates.
- **cameleer-main:** feature `health`, not in `full`; `features::PACKS` names the pack's parts so a
  build without it says "`mllp` is in the health pack ...".
- **Distribution (prepared, nothing published):** `container.yml` matrix with a health variant
  (`FEATURES=full,health`, tags `health`, `<version>-health`, `edge-health`), Dockerfile note,
  `scripts/package-archive.sh` and `.github/workflows/archives.yml` (artifacts
  `cameleer-<version>-<target>` and `cameleer-health-<version>-<target>` with SHA-256, not
  attached to a release, not signed yet).
- **Docs:** the health pages are under `docs/packs/health/` with a `**Pack:**` header segment;
  `docs/packs/index.md`, `docs/packs/health/index.md`, `crates/cameleer-health/CHANGELOG.md`. The
  site navigation for packs (`scripts/build-docs.py` builds the pages but has no packs section)
  and llms.txt are left to the SEO session.
- **Binary size** (Linux, WSL2 Ubuntu 24.04, `--profile dist`, measured 2026-11-09):

  | build | bytes | MB |
  |---|---|---|
  | `--no-default-features` (core) | 6,970,672 | 6.97 |
  | `--no-default-features --features health` | 9,135,408 | 9.14 (+2.16) |
  | default (`full`) | 28,989,856 | 28.99 |
  | `--features full,health` | 30,913,032 | 30.91 (+1.92) |

  `full` no longer contains HL7, MLLP and FHIR, so it is not comparable with earlier `full`
  figures in this file; the pack costs about 2 MB.

**Test run after the pack move (Windows 11, 2026-11-09):** `cargo build --workspace` 0 warnings;
`cargo build -p cameleer-main --no-default-features` and `--no-default-features --features health`
build; `cargo build -p cameleer-health --no-default-features --features hl7` (and `fhir`, `dicom`,
none) build; `cargo test --workspace` **2952 passed, 0 failed**. After the move, on WSL2 Ubuntu
24.04 (2026-11-10): `cargo test -p cameleer-health` all passed with zero warnings, and the live
tier `CAMEL_DICOM_INTEROP=1 cargo test -p cameleer-health --test live_dicom` **7/7** against DCMTK
3.6.7 and Orthanc 1.12.2.

Built afterwards (2026-11-10), from the same design:
- `/q/info` and `/q/dev/packs` (`camel.server.infoEnabled`, also on with the dev console): the
  version, the compiled-in families and the packs.
- `.github/CODEOWNERS` with an owner for the pack's code, docs, examples and tooling.
- `scripts/sbom.py`: CycloneDX 1.5 SBOMs of exactly what a build compiles in (from `cargo tree -p`,
  so a DICOM-only pack build lists core, http and tls), reproducible; written next to every
  archive by `package-archive.sh`, plus one for the `cameleer-health` crate; checked in CI.
- `archives.yml` signs every archive, checksum and SBOM with keyless cosign on release tags only.
  `package-archive.sh health x86_64-unknown-linux-gnu` was run end to end on Linux (13.2 MB
  archive, the checksum verifies, the binary reports the pack); the workflow itself has not run on
  GitHub yet.
- CI pack gate: `cameleer-main --no-default-features --features health` tests and each crate
  feature of `cameleer-health` alone. It found that `tests/docs_reference.rs` (from main) failed in
  every reduced build; it is now gated on `full`.
- The DICOM Conformance Statement, `docs/packs/health/conformance/dicom-conformance-statement-0.6.0.md`
  (PS3.2 form), its SOP class, identification and character set tables generated from the code
  and checked by `tests/dicom_conformance_statement.rs`.
- The `CAMEL_HEALTH_INTEROP=1` tier (`scripts/health-interop.sh`): **10/10** on WSL2 Ubuntu 24.04
  (2026-11-11, run with the owner's approval): DICOM 7/7 (DCMTK 3.6.7, Orthanc 1.12.2 with
  DICOMweb 1.16), HL7 2/2 (python-hl7 0.4.5 as MLLP receiver with AA and AR acknowledgements, and
  its MLLP client against a Cameleer consumer), FHIR 1/1 (HAPI FHIR JPA server 8.12.0: create,
  read JSON and XML, update, search, history, delete). No Cameleer bug found; one test asserted the
  AR case the wrong way round (the producer template returns the error) and was fixed.

**Merged into `main` (2026-11-11)** after merging the latest `main` into the branch: `cargo build
--workspace` 0 warnings; `cargo build -p cameleer-main --no-default-features` and
`--no-default-features --features health` build; `cargo test --workspace` **2960 passed, 0 failed**
(Windows 11); the health live tier 10/10 (WSL2).

Still open: the first real run of `archives.yml` and the
health image build in `container.yml`; IHE ATNA audit messages (PS3.15 A.5); an independent review
of the TLS code before claiming the BCP 195 profiles.

### DICOM in the health pack

Built from `docs/design/medical/dicom-and-healthcare.md` (phases 1 to 5 of its plan; the owner
approved leading with edge imaging gateways and AI inference routing, no PACS). Now
`cameleer-health/src/dicom` (crate feature `dicom`). Guide: `docs/packs/health/guides/dicom.md`.

- **Phase 1, data:** Part 10 and data set codec (explicit/implicit/deflated little endian and
  explicit big endian, read and write; sequences; UN sequences; encapsulated pixel data as
  fragments; files without preamble or meta); the PS3.6/PS3.7 dictionary, UID registry and PS3.15
  Table E.1-1 generated from the 2026d DocBook (`scripts/update-dicom-dict.py`, pinned SHA-256,
  `crates/cameleer-health/DICTIONARY.md`); specific character sets
  (`scripts/gen-dicom-charsets.py`, tables from CPython's codecs, `CHARSETS.md`); the Value
  tree (DICOM JSON shape, byte-exact), `dicom` and `dicomJson` data formats; `${dicom.*}` through
  a new core hook `simple::register_function`, and the `dicom` language (also a YAML DSL key).
- **Phase 2, DIMSE:** PDUs, associations, SCU and SCP for C-ECHO, C-STORE, C-FIND, C-MOVE, C-GET,
  the `dicom:` component, `testing::TestPacs`.
- **Phase 3, DICOMweb:** the `dicomweb:` component (client and server on the platform-http
  registry); the multipart parser moved to `cameleer_core::multipart` and shared with
  `mimeMultipart`.
- **Phase 4, de-identification:** `deid` (processor and `dicomDeidentify` data format), audit
  records in `cameleer-ai`'s hash-chained log.
- **Phase 5:** `examples/medical/` (three examples, run end to end in
  `crates/cameleer-main/tests/dicom_examples.rs`), pages for `dicom`, `dicomweb`, the three data
  formats and the language, FEATURES rows, INTEROP section.

**Verified:**
- hermetic: `cargo test -p cameleer-health` (codec, model, charsets against PS3.5 Annex H-K bytes,
  DIMSE SCU/SCP incl. TLS and rejections, component routes, DICOMweb client/server, Table E.1-1
  attribute by attribute for six option sets) and the examples and docs examples in cameleer-main;
- corpus: 33 pydicom test files byte-exact (`scripts/fetch-dicom-testdata.sh`, files kept out of
  git, provenance in `crates/cameleer-health/TESTDATA.md`);
- live (`scripts/dicom-interop.sh`, `CAMEL_DICOM_INTEROP=1`, WSL2 Ubuntu 24.04): 7/7 against
  DCMTK 3.6.7 (both roles, with and without TLS) and Orthanc 1.12.2 (PACS, DICOMweb server and
  DICOMweb client). One bug found and fixed (odd deflated PDV, `docs/INTEROP.md`).

**Test run (Windows 11, 2026-11-09):** `cargo build --workspace` 0 warnings;
`cargo build -p cameleer-main --no-default-features` builds; `cargo test --workspace`
2914 passed, 3 failed: two `docs_languages_formats` checks for the new pages (fixed afterwards,
14/14 on rerun) and `cameleer-bench` `short_run_reports_throughput_and_latency` (a 50 ms run that
saw no messages under the full parallel load; 3/3 alone, crate untouched by this work).

**Not verified / not built:** real cloud DICOM services (Azure, Google, AWS; AWS needs SigV4 in
`dicomweb`), dcm4che, real modalities; Storage Commitment, MPPS, C-CANCEL in progress, async
operations, association pooling; WADO frames/rendered/thumbnail, UPS-RS; pixel codecs (JPEG,
JPEG-LS, JPEG 2000, RLE) and with them Clean Pixel Data; Encrypted Attributes Sequence;
`fhirDeidentify`; PS3.15 A.5 (ATNA) audit messages; a PS3.2 conformance statement; fuzzing of the
Part 10, PDU and multipart parsers; throughput benchmarks against dcm4che, DCMTK and Orthanc.
## Move to the `cameleering` organization (2026-11-12)

Plan in `docs/design/cameleering-org-move.md` (approved; supersedes
`docs/design/github-organization.md`): the repository, simoom and karez move to the GitHub org
`cameleering`, the images to `docker.io/cameleering/cameleer` and `ghcr.io/cameleering/cameleer`,
crates.io gets the team `github:cameleering:release`; then 0.6.0 is released from the org.
Branch `org-move` has the rewritten references (`scripts/org-move-rewrite.py`, history in
`CHANGELOG.md` and `benchmarks/results/` left as recorded) and explicit image namespaces in
`container.yml` (`GHCR_NAMESPACE`, `DOCKERHUB_NAMESPACE`, login account from the
`DOCKERHUB_USERNAME` repository variable). It is merged on transfer day, after the owner has
created the org, its teams, the org-level verification of cameleer.ing and the Docker Hub
namespace and token.

**Transferred on 2026-11-12.** The org exists (login `Cameleering`, Free plan, teams
`maintainers` and `release`); cameleer.ing is verified for the org. Verifying it removed the
custom domain from the repository while it was still `pmsudhi/cameleer` (an org-verified domain
only serves the org's repositories), so cameleer.ing answered 404 for about 40 minutes until the
transfer; the domain was set again on `Cameleering/cameleer`, HTTPS is enforced, and the old
GitHub URLs redirect. Still open: the Docker Hub `cameleering` namespace and `DOCKERHUB_TOKEN`,
a new `TRAFFIC_TOKEN`, the org profile README, the crates.io team and 0.6.0.

## Unreleased since 0.5.1 (2026-11-09, `main`)

- **`--mcp-expose`** (`camel.server.mcp-expose-routes`): existing routes served as MCP tools with no
  route edits; REST operations become typed tools from their documented parameters. Code in
  `crates/cameleer-ai/src/expose.rs`, tests in `crates/cameleer-main/tests/mcp_expose.rs`.
- **Attestation, phase 1** (`crates/cameleer-attest`, feature `attest`): evidence from configfs-tsm
  and dstack bound to nonce, TLS key and manifest; `/q/attestation`; MCP `_meta` and tool. Design
  and later phases (sealed secrets, verification of remote servers) in
  `docs/design/attestation/research.md`. Tests use a fake configfs-tsm directory and a fake dstack
  agent; nothing has run on real TEE hardware yet (a `CAMEL_REAL_TEE=1` tier waits for the owner's
  choice of cloud).
- **SAP Integration Suite gap analysis** (2026-11-11): `docs/design/sap-integration-suite/gap-analysis.md`
  maps every Cloud Integration flow step, adapter and runtime service to Cameleer, ranks the
  blockers for replacing the Camel runtime under CPI and Edge Integration Cell, and proposes
  phases. Research only; nothing built.
- **Healthcare research**: `docs/design/medical/dicom-and-healthcare.md` (DICOM, DICOMweb,
  de-identification, Mirth alternatives, positioning); not started.

## The HTTP server runs on simoom (2026-11-13, branch `simoom-server`)

Decision and measurements: `docs/design/simoom-adoption.md` (sections 8 to 12; raw data in
`docs/design/simoom-adoption-results/`). Owner rule: no optional feature, either simoom replaces
Cameleer's own HTTP code or it is not used.

- **PR 1 (this change): the server.** `cameleer-http` serves plain HTTP and HTTPS on simoom (an event
  loop per core): `server_simoom.rs` (the engine, the per-route governor between the loop thread and
  simoom's offload pool, response streaming), `HttpHandler::{is_async, handle_async, route_key}`, async
  routes in the registry, the MCP endpoint answering `tools/call` from its call worker,
  `cameleer_tls::TlsServerConfig::export` for the TLS bridge, the client percent-encoding unsafe bytes in
  request targets. The thread-per-connection server is gone.
- **Measured** (three rounds on DigitalOcean `c-4`, classic against simoom, same machines): `http`
  1.33x at 64 connections and 1.56x at 256, `rest-json` 1.31x, `mcp-session` 1.37x; p99 2 to 3x lower;
  CPU per request 40 to 47% lower; 4 threads instead of 67 to 202. Binary: `full` +1.7%.
- **PR 2: the HTTP client** on `simoom-client` (`client_engine.rs`: one shared client per set of settings;
  `HttpClient::execute_streaming` replaces `open_stream` for the event-stream readers of the MCP and SaaS
  clients; `HttpConnection`, the producer's own pool and its HTTP/2 client are gone; the `http2` feature
  is gone). `cameleer_tls::TlsClientConfig::export` is the bridge. Certificate verification cannot be
  turned off in these clients.
- **PR 3 (branch `simoom-h2`): the gRPC client** on `simoom-client`'s full-duplex `Call`
  (`cameleer-grpc` `grpc/client.rs`: `GrpcClient`, `ClientCall` and `CallSender` keep their API; the
  engine is created on first use from the public settings, stream opens are serialized so racing calls
  share one connection). The TLS bridge is `cameleer_http::client_engine::tls_options`; certificate
  verification cannot be turned off (the gRPC component has no such option). Kinesis `SubscribeToShard`
  (`cameleer-cloud` `h2_event_stream`) uses the same client. `H2Connection` is no longer used by library
  code; the tests of the old HTTP/2 server still use it until the server moves.
- **PR 4 (branch `simoom-srv`): the gRPC server and the cloud mocks** on `simoom` (`cameleer-grpc`
  `grpc/server.rs`: `GrpcServer` and `ServerCall` keep their API; `Limits::body_buffer` is 1 byte, so a call
  starts with its first request bytes and bidirectional streams work; one thread per call, a reader thread
  feeds the request messages through a channel so `recv` can wait with the deadline; the response head goes
  out with the first message, or the call ends trailers-only). The TLS bridge is
  `cameleer_http::server_simoom::server_tls`. The old `h2` module (`H2Server`, `H2Connection`, frames,
  connection engine) and `hpack` are deleted; `cameleer_grpc::h2` keeps `H2Config`. The `MockServer` of
  `cameleer-cloud` serves its h2c connections (the AWS mock's Kinesis `SubscribeToShard`) from an internal
  simoom server behind a tunnel. The HTTP/2 tests of `cameleer-http` use simoom's server and client.
- **PR 5: cleanup.** The HTTP/1.1 reader and writer in `cameleer-http/src/http.rs` (`read_request`,
  `read_response`, `write_response`, `write_request`, chunked coding, `BodyReader`, `HttpError`) had no
  callers left and are deleted; the file keeps the message model and the URL, query, media-type and date
  helpers. The official tier ran in WSL (Go 1.24.7): the three HTTP/2 tests against Go and the two grpcio
  tests pass; the gcloud CLI test needs the Google Cloud SDK, which is not installed.
- **PR 6: the HTTP server speaks HTTP/2** (`http2(false)` removed from `server_simoom.rs`; h2c prior
  knowledge, `Upgrade: h2c`, ALPN `h2`; test `platform_http_serves_http2_next_to_http11`). Go's client
  negotiates `HTTP/2.0` with a `jetty:https` route.
- **PR 7: the cloud components always verify certificates** (`Opts::tls` in `cameleer-cloud/src/util.rs`:
  `sslContextParameters` and `camel.ssl.*`; `trustAllCertificates` and a global
  `camel.ssl.trustAllCertificates` are refused; `HttpClient` has no insecure mode).
- **0.7.0 (2026-11-16): the simoom replacement is finished.** Release notes in `docs/release-notes/0.7.0.md`.
  Hermetic suite on Windows 11: 2961 passed, 0 failed, zero warnings; the official tier (WSL2, Go 1.24.7
  and grpcio): HTTP/2 3/3, TLS 2/2, gRPC 2/2.
- **Benchmark round of 2026-11-18 (0.7.0 against 0.6.1, three DigitalOcean `c-4` droplets, deleted afterwards):**
  gRPC unary 1.06x requests/s with 29% less server CPU per call and a 35% lower p99; plain HTTP and MCP 1.00x;
  `rest-json` 0.95x (later shown to be noise: 1.01x in the 0.8.0 round); HTTP/2 at 0.51x of HTTP/1.1 for small calls
  (explained on 2026-11-19: mostly the load generator's HTTP/2 client, partly lazily started event loops, see below);
  `full` binary -0.7%, `core` unchanged. Raw data and method in `benchmarks/results/2026-11-18-linux-grpc-http2/`,
  report in `docs/BENCHMARKS.md`. The gRPC ceiling of about 11,000 calls/s on both builds was thread creation (fixed by
  the pool below). Not measured: gRPC streaming, HTTP/2 over TLS, the gRPC client, Windows.
- **gRPC calls on a pool of threads (2026-11-18/19, after 0.7.0, PR 62 then PR 65):** a call is one job on a
  process-wide cached `ThreadPool` (`grpc/server.rs`, waker based body reads, one shared deadline thread); idle pool
  workers hand their allocator cache to other threads (`alloc::release_thread_cache`, shared regions). The first
  version (three threads per call) was 4.65x but used 49 MB at 64 calls and 169 MB at 256 (`2026-11-18-linux-grpc-pool/`).
  Droplet round of the fix (`benchmarks/results/2026-11-19-linux-grpc-memory/`, sfo3): gRPC unary 3.90x at 64 calls and
  3.56x at 256 against 0.7.0, CPU per call 37 against 214 microseconds, p99 4.0 against 12.7 ms, peak memory 13.5 against
  13.8 MB and 18.6 against 20.1 MB; against the first pool build the same speed (1.05x) at 14.3 against 49.7 MB. With
  the order of the sides swapped `rest-json` was 0.98x (identical code), so the earlier 0.93x to 0.95x was probably the
  harness. The 41,020 calls/s of that round was limited by cameleer-bench's client and is superseded by the 0.8.0
  candidate round below.
- **HTTP/2 against HTTP/1.1 explained (2026-11-19, `docs/design/http2-throughput.md`).** The 0.51x had two causes:
  - **Mostly cameleer-bench's HTTP/2 client:** simoom-client's reader thread handed each response to the request
    thread, two hand-offs per response.
  - **Partly the server's lazily started event loops,** which left most long-lived HTTP/2 connections on one thread
    when they arrived in a burst. The HTTP and gRPC servers now start every event loop (lazy start only on Lambda or
    with `CAMEL_SERVER_LAZY_LOOPS=true`). Worth 2.07x for gRPC unary on WSL2, 1.04x on the droplets.

  Driven by nghttp2's h2load (`scripts/cloud-bench/peer.sh`, `benchmarks/results/2026-11-19-linux-http2-peer/`; 16
  streams per connection, one run on two droplets), the server served HTTP/2 at 1.6x to 1.8x its HTTP/1.1 rate, and
  gRPC unary at 73k to 93k calls/s. simoom 0.2.0 places a few long-lived connections on the least loaded loop. Open: an
  event-driven HTTP/2 client in simoom-client.
- **Asynchronous routing, phase 0 (2026-11-19; on by default, `CAMEL_ASYNC_ROUTING=false` turns it
  off):**
  - **What was built:**
    - an `AsyncProcessor` path in the routing engine: `cameleer_core::reactive`, plus the async forms
      of the pipeline, the route, the error channel and `to`;
    - the `http` producer on simoom-client's event-loop client;
    - `platform-http` routes answering from tasks;
    - `CAMEL_ASYNC_ROUTING=false` as the control.
  - **Measured** (WSL2, `platform-http → http` proxy, 4 CPUs): 2.2x req/s at 64 connections and 2.9x
    at 256, 26-33% less CPU per request, 0.07-0.2 context switches per request instead of 1, 8 threads
    instead of 69-205. With a 20 ms backend the synchronous path stops at the 200-thread bound (9,900
    req/s) and the asynchronous one does not.
  - **Tests:** hermetic suite 2977 passed, 0 failed.
  - **Where to read more:** design and results in `docs/design/async-routing.md`.
  - **In-flight limit:** `maxInflightRequests` on `platform-http` (default
    `camel.server.maxInflightRequests` or 10,000) answers 503 beyond it.
  - **Client on the server's loops** (simoom 0.2.0, which this change requires; measured on 2026-11-20
    with the same simoom code as a local `[patch]` before the release):
    - 0.00-0.03 context switches per request;
    - +13-15% req/s and a 35% lower p99 at 64 connections;
    - the same CPU per request (about 45 µs), so the route is now CPU-bound.
  - **Profile of the remaining 45 µs** (section 14 of the design): mostly Camel's header mapping and
    simoom's HTTP work; the URL is now handed to the client once.
  - **Open:** phases 1-4.
- **Still on the old code:** WebSocket, and the cloud components' own HTTP/1.1 client.

## 0.8.0 released (2026-11-24)

0.8.0 is released from `main`: version bump PR, tag `v0.8.0`, 22 library crates (`cameleer-health` stays at 0.6.0, its
code is unchanged), the `cameleer` wrapper 0.8.0, images, archives and the MCP Registry entry. Release gate: the owner
accepted publishing with the 2026-11-23 Camel ratios, four of which are lower than the 2026-11-06 ones (seda
request-reply 5.6x against 6.8x, router 2.6x against 3.0x, CSV 2.2x against 2.3x, HTTP into Kafka 4.3x against 5.0x on
other machines). Tests on Windows 11 before the tag: `cargo build --workspace` 0 warnings, `cargo test --workspace`
2,990 passed, 0 failed (one run of `component_timer` `stopping_a_route_from_its_own_timer_exchange_does_not_hang`
failed once under load and passed 5/5 alone and in the full rerun; a timing-sensitive test, 10 ms period),
`cameleer-main --no-default-features` 24 passed, with `--features health` 24 passed. The live, official and simulator
tiers were not rerun for this release. Known: the published `cameleer-health` 0.6.0 requires `cameleer-core ^0.6`, so
`cargo install cameleer-main --features health` from crates.io pulls two core versions; fix by releasing the pack at a
new version.

## 0.8.0 candidate (2026-11-20/21)

`main` (PRs 65, 67, 68, 69, 71 and simoom 0.2.0 from crates.io) is the 0.8.0 candidate; the release itself (version
bump, tag, images, crates, archives, the site) is a separate step. Two droplet sessions on 2026-11-20 (three `c-4` in
`sfo2`: `c-4` is no longer offered in `blr1` or `sfo3`; only `sfo2`, `ams3` and `lon1` had it), builds at `7ee75e7` (main
plus PR 67 and PR 68). Results, raw records and READMEs: `benchmarks/results/2026-11-20-linux-0.8.0-candidate/` (against
0.7.0 and against nghttp2's h2load) and `.../2026-11-20-linux-0.8.0-camel/` (against Apache Camel, one session). The
review that led to this documentation pass: `docs/design/docs-review-2026-11-20.md`.

- **gRPC unary:** 94,400 calls/s at 64 calls and 106,200 at 256 (cameleer-bench's generator) against about 8,300 and
  8,000 for 0.7.0, at 24 against 270 microseconds of server CPU per call, p99 1.2 against 14.8 ms, peak memory 9.2 against
  13.6 MB. h2load: 129,000 to 135,000 calls/s. 0.7.0 lacks the PR 65 pool, so the 11x to 13x contains it (3.6x to 3.9x on
  its own, other droplets); the other parts were not measured one by one.
- **HTTP/2:** `rest-json-h2` equals `rest-json` (1.00x); h2load serves HTTP/2 at 1.5x to 1.6x its HTTP/1.1 rate (not a
  like-for-like protocol test).
- **Eager event loops:** no measurable cost on `http` (0.86x to 0.97x within the noise) or `rest-json` (1.01x). Cost in start-up
  (+0.3 to 0.5 ms) and RSS (+0.3 to 0.4 MB). `CAMEL_SERVER_LAZY_LOOPS=true` was not run as a control.
- **MCP:** 0.96x to 0.98x of 0.7.0 on every pass, probably a real 2% to 4% slowdown; the cause (eager loops or the header
  storage change) was not isolated. Open.
- **Size:** `full` 26.72 MB (+0.8%), `core` 7.22 MB (+2.4%), stripped, glibc.
- **Against Camel, one session** (this replaces the 0.4.0 table): 1.8x (HTTP) to 6.8x (seda request-reply), Kafka
  stream 15.2x, MCP session 43,254 calls/s against Camel's 20,968 (2.06x) and the TypeScript SDK's 2,179 (19.9x); p99 lower
  in every scenario, so the HTTP-into-Kafka "loss" is gone (3.5 against 8.1 ms). Open: HTTP into Kafka is 31,351 req/s here
  (3.2x Camel), against 48,800 (5.0x) in the 2026-11-06 run on other droplets, and the startup scenario reads 12.3 ms
  against 2.5 ms in the 0.4.0 run; neither was investigated.
- **Asynchronous routing** was measured on WSL2 only, never on droplets.
- **Not measured:** h2load over TLS, gRPC streaming, the gRPC client, Windows, the lazy-loops control.
- **Not verified:** the live and simulator tiers (WSL2) on the new server and client, and the `gcloud` CLI
  check of the official tier (needs the Google Cloud SDK).

## Two fixes after 0.8.0 (2026-11-23, PRs 74, 75 and 76)

Merged into `main`: the governor sends a route back to the pool when its calls keep the loop threads over half busy (no
throughput dips in 50 runs, where 6 of 21 runs on `main` fell to 7,000 to 15,000 req/s), and the `kafka` producer has an
asynchronous form for asynchronous routes (a send queued after stop fails instead of hanging). Droplet figures (`sfo2`, 64
connections, servers swapped, `main` `7952563` against the fixes against `v0.6.0`): HTTP into Kafka 35,472 against 32,527
against 40,744 req/s, peak memory 31.5 against 60.8 against 76.9 MB; results in
`benchmarks/results/2026-11-23-linux-fixes-final/`, summary in `docs/BENCHMARKS.md`. Linux (WSL2): hermetic suite 2,989
passed, 0 failed, 0 warnings; kfake 5/5 and 2/2, kcat and librdkafka 9/9. **Release gate not clean:** HTTP into Kafka is
0.87x of `v0.6.0`, stateless MCP 0.95x, HTTP into PostgreSQL 0.92x with one unexplained run at 7,700 req/s. No Apache Camel
side was run in that session.

## Against Apache Camel again, keep-alive fix and soak (2026-11-23, PR 78)

- **Keep-alive fix (PR 78).** The HTTP server closed every keep-alive connection after 1,000 requests (simoom's
  `max_requests_per_connection`), so the load generator reconnected 700 to 5,700 times per run against 64 for Camel.
  The limit is now unlimited by default.
- **Full suite against Camel on `main` `beb3385`** (`sfo2`, servers Xeon 8358 and 8280, one session, 78 records, 0 failed;
  `benchmarks/results/2026-11-23-linux-main-beb3385-camel/`):
  - HTTP fixed reply +21% and REST with JSON +15% against the run before the fix.
  - 2.0x (HTTP) to 19.8x (Kafka stream) Camel on the JVM; p99 lower in every scenario.
  - MCP session 55,313 calls/s against 20,254 (2.73x) and the TypeScript SDK's 2,684 (20.6x).
  - Startup 3.05 against 1,034 ms; peak memory 8 to 39 MB against 211 to 421 MB.

  This session is now the published comparison (README, `docs/BENCHMARKS.md`, the site).
- **Lower than the previous published ratios:** seda request-reply 6.8x to 5.6x, the router 3.0x to 2.6x, CSV 2.3x to
  2.2x, the startup headline 450x to 300x (339x exact), and memory up to 54x to 44x. Camel on the JVM gained more on
  these CPUs than Cameleer did; Cameleer's own figures rose in all but the router. Under the release performance rule
  these need a decision before the next release.
- **One-hour soaks** (`scripts/cloud-bench/soak.sh`, `benchmarks/results/2026-11-23-linux-soak/`):
  - HTTP into Kafka 49,530 req/s (4.6x Camel); MCP session 62,967 calls/s (2.5x).
  - Cameleer: flat throughput and p99, constant threads and descriptors, 0.6 to 1.3 MB of RSS growth that slows.
  - Camel on the JVM grows by 17 to 71 MB and the TypeScript SDK by 89 MB in the hour.

## Alibaba Cloud Simple Log Service (2026-11-25, unreleased)

- **`alibaba-sls`** (Camel's `camel-alibaba-sls`, Preview since Camel 4.23) in `cameleer-cloud`, feature `cloud`:
  `putLogs` (protobuf `LogGroup`, lz4 / deflate / none), `getLogs` (the 2020-12-30 `POST /logstores/{name}/logs`),
  `listLogStores`; Camel's URI, options and `CamelAlibabaSls*` headers; signature V1 (HMAC-SHA256 as Camel's SDK, or
  HMAC-SHA1) and V4. Code in `crates/cameleer-cloud/src/alibaba/`, page `docs/components/alibaba-sls.md`, example
  `examples/syslog-to-sls.camel.yaml`.
- **Verified:** the official Go and Python SDKs' signature test vectors (V1 HMAC-SHA1, V4, percent-encoding);
  `SlsMock` (`testing/alibaba.rs`) recomputes every signature, checks Content-MD5 and the raw size, decompresses and
  decodes the protobuf (`tests/alibaba_sls.rs`, `cameleer-main/tests/docs_cloud_alibaba.rs`).
- **Official client:** `tests/official_alibaba.rs` (`CAMEL_OFFICIAL_CLIENTS=1`) passes with Alibaba's Python SDK,
  aliyun-log-python-sdk 0.9.53 (sdist checksum-pinned in `scripts/official-clients.sh`), on Windows 11 with Python 3.12.10
  (dependencies from pip: six 1.17.0, requests 2.32.3, python-dateutil 2.9.0.post0, protobuf 5.29.3, lz4 4.3.3, jmespath
  1.0.1): V1 (HMAC-SHA1) and V4 signatures, CreateLogstore, PutLogs lz4 and uncompressed with topic, source and tags,
  GetLogs with a query, ListLogStores, a wrong secret refused; the component reads what the SDK wrote. It found one mock
  bug (V4 requests carry no Content-MD5; `docs/INTEROP.md` official-clients bug 20). Not yet run in the Ubuntu set-up of
  `official-clients.sh` (apt dependencies).
- **Not verified:** the real service (no Alibaba Cloud account). Not built: zstd (the default of Camel's SDK), Camel's
  `serviceKeys` / `slsClient` objects, ECS RAM roles and OIDC credentials.

## Start-up regression found and fixed (2026-11-22)

The in-process `startup` scenario went from about 2 ms (0.4.0 to 0.6.0) to 27-31 ms on WSL2 (12 ms on the
4-CPU droplets) in 0.6.1: simoom grows the descriptor table at server start, and Linux waits for an RCU grace
period when a second thread exists. `cameleer_http::register` now does it while the process is single-threaded
(test `early_descriptors`). Back to 2.8 ms. Details and per-release table: `docs/design/startup-regression-2026-11.md`.
Not yet re-measured on a droplet.

## OpenTelemetry tracing (2026-11-25, branch `opentelemetry`)

Design and the mapping to Camel in `docs/design/opentelemetry.md`; user guide `docs/guides/opentelemetry.md`.

- **Built:** `cameleer_core::telemetry` (route, producer and processor spans with `camel-opentelemetry2`'s names,
  kinds and attributes; W3C Trace Context and Baggage on the message headers; samplers, limits, `excludePatterns`,
  `encoding`; the producer-wrapper SPI `CamelContext::add_producer_wrapper`); the new crate `cameleer-opentelemetry`
  (feature `opentelemetry`, in `full`: OTLP/HTTP protobuf exporter, batch span processor, `OTEL_*`, an embedded OTLP
  receiver in `testing`); `Main` installs it from `camel.opentelemetry.*` and flushes at stop and after each Lambda
  invocation; trace and span ids in log lines; `camel.main.log-format` / `CAMEL_LOG_FORMAT` / `--log-format` /
  `--logging-json` (`text`, `ecs`, `logstash`).
- **Tests:** core `tests/telemetry.rs` (7), `cameleer-opentelemetry` (8 unit, 6 exporter against the receiver: gzip,
  headers, retries with `Retry-After`, final statuses, budget, partial success, drop and count with a dead collector),
  `cameleer-main/tests/opentelemetry.rs` (one trace across HTTP, Kafka on the embedded broker and HTTP over three
  services; `Main` from properties), propagation over JMS on STOMP, AMQP 1.0 and gRPC metadata.
- **Official Collector:** `scripts/otel-collector.sh` (otelcol 0.165.0, SHA-256 pinned) and
  `tests/official_otel_collector.rs` (`CAMEL_OFFICIAL_CLIENTS=1`). **Not run yet**: downloading the Collector was not
  permitted in the session that built this; run `scripts/otel-collector.sh all` in WSL.
- **Overhead** (`benchmarks/results/2026-11-25-windows-opentelemetry/`, one Windows desktop): off 0.92x to 1.08x of
  `main` (noise); on, 0.1 to 0.35 µs per span; the exporter sends about 0.8 M spans/s and drops beyond.
- **Left out:** OTLP metrics and logs, OTLP gRPC and JSON, B3/Jaeger propagators, Camel's MDC keys in log lines, the
  `log` EIP as span events, otel2's `includePatterns`/`traceHeadersInclusion`; not measured on Linux or droplets.

## Log pipeline components (2026-11-25)

- **`splunk-hec`** (`cameleer-saas`, feature `saas`): Camel's producer to Splunk's HTTP Event Collector, its options,
  the `CamelSplunkHECIndexTime` header, the JSON envelope and the 200-only success rule (`RuntimeCamelException`
  otherwise). `skipTlsVerify=true` is refused. Hermetic tests against `testing::SplunkHecMock` (HTTP and HTTPS).
  Not tested against a real Splunk: its image needs the license accepted, which is the owner's call.
- **`lumberjack`** (`cameleer-protocols`, feature `protocols`): Camel's consumer, Lumberjack v1/v2 (window, JSON,
  data, compressed frames; one ACK per window after the route; no ACK and a closed connection on failure), TLS.
  Hermetic tests with `testing::LumberjackClient`; official tier `official_lumberjack.rs` with Filebeat 9.2.0 OSS
  (3/3 on Windows 11 and on WSL2 Ubuntu 24.04, 2026-11-25).
- Pages `docs/components/splunk-hec.md` and `lumberjack.md`, "Log pipelines" in the observability guide,
  `examples/logs-to-splunk.camel.yaml`, FEATURES rows (231 with alibaba-sls).

## pathscale crates evaluated, five fixes from it (2026-11-25, branch `claude/busy-booth-479e16`)

- **Evaluation.** `docs/design/pathscale-evaluation.md` covers nagoya, worktable, ekostd,
  endpoint-libs and the ps- crates: none is adopted, and the std-only rule stays.
  - No Windows.
  - nagoya's timers never fire on x86_64 since 0.1.22.
  - None of them is faster than the code it would replace.
- **Fixes, from comparing against them** (section 9 there, `CHANGELOG.md` Unreleased):
  - the aggregator's completion timeout keeps its deadlines in order (2.9x to 14.4x per message);
  - regex matches share the subject text (64 KiB `split` 418 ms to 2.9 ms);
  - the embedded `TcpServer` accept threads block instead of polling every 5 ms;
  - the cloud `HttpClient::send_to` uses the keep-alive pool;
  - `JdbcAggregationRepository` reads `MAX(seq)` once.
- **Tests.**
  - Windows 11: `cargo test --workspace --no-fail-fast` 3,001 passed, 0 failed, zero warnings.
    In an earlier run, `cameleer-bench` `short_run_reports_throughput_and_latency` failed once
    under load; it passed 3/3 alone.
  - WSL2: the changed crates (core, tls, amqp, kafka-mqtt, cloud, sql) 1,332 passed and 1 failed,
    the known timing-sensitive `component_timer` test. It passed 5/5 alone. `cancel.rs`
    `a_passed_deadline_cancels_the_exchange_and_its_copies` failed once in an earlier run and
    passed 5/5 alone.
  - Live tiers not run. `JdbcAggregationRepository`'s new `CREATE INDEX IF NOT EXISTS` was
    checked only against the embedded engine, not PostgreSQL.

## Pending / todo

Everything below is extra verification of an implementation that's already done. None of it
is a missing feature. Each item was blocked in the cloud container by network policy, missing
Docker, or the lack of real accounts.

1. **Real cloud accounts.** Run the AWS (S3, SQS, SNS, DynamoDB, Lambda), Azure (Blob, Queue,
   Service Bus, Event Hubs, Cosmos) and GCP (Storage, Pub/Sub, BigQuery) components against
   real services.
   - Credentials must come from the owner. Never sign up automatically.
   - Add a new tier gated by `CAMEL_REAL_CLOUD=1` (`crates/cameleer-cloud/tests/real_*.rs`).
   - Use uniquely named, short-lived resources and delete them in teardown.
2. **Docker-based real servers**, which need Docker plus the ability to pull images:
   - Apache Kafka (KRaft) or Redpanda
   - Elasticsearch and OpenSearch
   - Apache Cassandra or ScyllaDB
   - CouchDB, real MongoDB (including change streams, which FerretDB lacks), ActiveMQ Artemis

   Extend `scripts/live-interop.sh` (or add `scripts/docker-interop.sh`) and add `live_*`
   tests.
3. **Emulators that need Docker or Windows:** LocalStack, the Azure Cosmos DB emulator, the
   Azure Service Bus emulator, and the gcloud Pub/Sub, Firestore and Datastore emulators.
4. **Windows: hermetic tier done (2026-11-03).** On Windows 11 with Rust 1.99.1 MSVC,
   `cargo build --workspace` gives 0 warnings and `cargo test --workspace` gives
   **2252 passed, 0 failed**. Fixes made along the way:
   - `cameleer-tls` RNG: seeds from `BCryptGenRandom` on Windows (it only had `/dev/urandom`).
   - `NetStream`: clones and TLS halves share one `Arc<TcpStream>`. On Windows read
     timeouts belong to the handle, so `try_clone`d handles ignored `set_read_timeout`.
   - `net::shutdown_socket`: Windows `shutdown` doesn't wake a blocked `recv`, so it also
     calls `CancelIoEx`. Used by `NetStream::shutdown`, `HttpServer::stop` and the three
     `TcpServer::stop`s. Before this, stopping MLLP took minutes.
   - `net::bind_listener`: binds IPv4 first, like Java. On Windows `localhost` resolves to
     `::1` first. Used by the protocols, amqp and kafka-mqtt `TcpServer`s and `HttpServer`.
   - `cameleer_core::stack` (Linux and Windows stack bounds), shared by `cameleer-script` and a
     new PL/pgSQL `54001` stack check.
   - `.gitattributes` `eol=lf`, because Git for Windows defaults to `core.autocrlf=true`
     and that broke the byte-exact fixtures.
   - Unix-only tests are `cfg(unix)`: SSH agent sockets, `exec` with `echo`/`sh`, file
     modes. Windows variants were added where they make sense.

   - **Bundled IANA time-zone database (2026-11-04).** `cameleer-core/tzdata/zones.bin` has
     IANA 2026f, 598 zones in about 350 KB, compiled into `cameleer-core`. `tz.rs` uses it after
     `$TZDIR` and the system zoneinfo directories, so named zones work on Windows and in
     minimal containers. Zone tests no longer skip. To refresh it, bump the pinned
     version and SHA-256 in `scripts/update-tzdata.py` (source: PyPI `tzdata`, which is
     IANA compiled by `zic`) and rerun it; the script checks the digest. IANA publishes
     several releases a year, so check at every release.

   - The test IMAP server resolved a bare login (`ann`) to a random mailbox starting with
     `ann@`, sometimes a `moveTo` folder. That made `imap_consumer_with_attachments_and_move`
     fail about half the time. Folders are now excluded and the choice is deterministic.

   Still open on Windows:
   - The process default zone (`TimeZone::local`) is UTC unless `TZ` is set. Java uses the
     Windows zone; matching that needs the Windows-to-IANA name map (CLDR `windowsZones`).
   - About 50 other direct `TcpStream::shutdown` calls, mostly client readers and test
     brokers, should move to `net::shutdown_socket` so `stop()` doesn't hang on idle peers.
   - The live, official-client and simulator scripts are bash and Ubuntu-only. Run them
     under WSL2.
5. **Independent security audit** of the from-scratch crypto and protocol code in
   `cameleer-tls` (TLS state machine, X.509 path validation, constant-time code), `cameleer-ssh`,
   `cameleer-xml` xmlsecurity, and the PGP and crypto data formats. Fuzz the parsers (HTTP/1,
   HTTP/2/HPACK, TLS records, BSON, CQL, AMQP frames, YAML, XML).
6. **Optional cleanup.** Run `cargo clippy --workspace`: `cameleer-script` has about 70 lints, and
   there are older lints elsewhere. Builds are already warning-free.
7. **Performance.** **Published comparison (2026-11-06):** `scripts/cloud-bench` on three
   DigitalOcean `c-4` droplets (two servers that swap roles, one load node with Kafka 4.3.2 and
   PostgreSQL 16) against Apache Camel 4.23.0 on Java 21 and Camel Quarkus 3.40.0 native (Camel
   4.22.1, Mandrel 25.0.4.1). Cameleer is ahead in every scenario: 1.7–8.7x Camel JVM throughput,
   3 ms startup vs 1.1 s (native 30 ms), up to 54x less memory (Cameleer 0.4.0; replaced on 2026-11-20 by the 0.8.0 session:
   1.8–15x, see "0.8.0 candidate", and on 2026-11-23 by `main` `beb3385`: 2.0–19.8x, see "Against Apache Camel again"). Report: `docs/BENCHMARKS.md`
   (tables generated by `scripts/cloud-bench/summarize.py` from `benchmarks/results/`); landing
   page `site/compare/apache-camel-vs-cameleer/`. The run found and fixed three bugs: HTTP server
   32-worker keep-alive churn (now 200 workers), Kafka producer serializing concurrent sends (now
   batched per broker), PostgreSQL `poolSize` not capping open connections (now a hard maximum
   with `poolTimeout`). Droplets are deleted after each run.

   **Local suite (history):** three tuning rounds against a 4x target on one Windows machine.
   `scripts/bench.sh` (Linux or Git Bash; `scripts/bench.ps1` on Windows) builds and
   runs `crates/cameleer-bench` and `benchmarks/java-camel`, then writes `benchmarks/local.md`. The Java
   side uses Apache Camel 4.23.0 on Temurin 21.0.13 with Maven 3.9.16, pinned and checksum-verified.
   Throughput on a Ryzen 5 5600X under Windows 11, idle machine (rust ÷ java, first round → final):
   - direct 0.68x → **4.27x**, seda InOnly 1.09x → **4.18x**, seda InOut 0.77x → **8.37x**
   - cbr 0.56x → 3.48x, splitagg 0.64x → 3.18x, HTTP 2.02x → 2.45x
   - memory: 25–46x less peak RSS; startup: 77x faster (10 ms vs 751 ms)

   What changed (details in `benchmarks/analysis.md`): a std-only thread-caching allocator
   (`cameleer_core::alloc`, opt-in, installed by the `cameleer` CLI and the benchmark), lazy exchange
   and message ids, vector-backed headers (with shared names) and properties (interned Camel
   names), a copy-on-write `ObjectMap`, a TSC tick clock and striped counters for the statistics,
   Camel's `ManagementStatisticsLevel` with **`RoutesOnly` as Cameleer's default** (per-step
   timing is opt-in; Camel collects nothing without `camel-management`), a two-lock seda queue with
   spin-then-park and a reusable InOut reply slot, a borrowing producer cache, faster Simple
   comparisons, a JsonPath parse cache, in-place aggregation and moving split parts into an
   aggregate, lazy HTTP read timeouts. `cameleer-bench profile <scenario> <secs>` is a std-only
   sampling profiler for Windows; `cameleer-bench raw-http <port> threads|iocp8|poll` measures the HTTP
   ceiling. Tests: `crates/cameleer-core/tests/perf_semantics.rs`, `mgmt_stats.rs` (each statistics
   level), plus unit tests in the changed modules.

   Still under 4x (why, and next steps, in `benchmarks/analysis.md`):
   - HTTP: a do-nothing server (threads or IOCP) reaches only about 400k req/s with the shared load
     generator on this machine, 3.6x Java, so 4x cannot be shown here; Cameleer reaches about 70%
     of that. Needs the load generator on another machine (history: the event-driven server
     core was built later, on simoom, in 0.6.1).
   - splitagg: JSON parsing and per-part exchange copies; next are shared JSON keys (an `ObjectMap`
     API change) and a smaller `Exchange` (public field changes).
   - cbr: the remaining cost is spread over many small steps.
   - Kafka scenario (optional) not done: it needs a real broker on both sides.

## Resuming in a new session

1. `git checkout main && cargo test --workspace`. Expect about 2,990 passing
   (Windows 11, 2026-11-24).
2. Pick an item above.
3. Follow the workflow in `CLAUDE.md`.
4. Update this file, and `docs/INTEROP.md` or `docs/FEATURES.md` where relevant, as part of
   the same change.

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
