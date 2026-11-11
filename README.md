<p align="center">
  <img src="site/brand/cameleer-logo.png" alt="Cameleer: Camel routes in Rust, the reliable action layer for AI agents" width="520">
</p>

# Cameleer

[![CI](https://github.com/pmsudhi/cameleer/actions/workflows/ci.yml/badge.svg?branch=main&event=push)](https://github.com/pmsudhi/cameleer/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![Dependencies: none](https://img.shields.io/badge/dependencies-std%20only-brightgreen.svg)](Cargo.toml)
[![Apache Camel compatible](https://img.shields.io/badge/follows-Apache%20Camel%204.x-d4523f.svg)](https://camel.apache.org/)

### Camel routes in Rust. The reliable action layer for AI agents.

Cameleer is Apache Camel's integration model, rebuilt in Rust: **if you know Apache Camel, you
already know Cameleer.** The same endpoint URIs, Enterprise Integration Patterns, Simple
expressions, YAML routes, headers and error handling. About 230 components, languages and data
formats, ported one by one and verified against the real systems.

- **Act reliably.** An agent decides; a route does it. Serve routes as MCP tools with one
  command (`cameleer run tools.camel.yaml --mcp`), and every call is validated against a JSON
  schema, runs once, waits for a person when the stakes are high, cancels on timeout, rolls back
  and lands in a hash-chained audit log ([the action layer for agents](#the-action-layer-for-ai-agents)).
- **Keep your routes.** Camel YAML and XML routes run unchanged; your team's Camel designs and
  habits come with you ([migrating from Apache Camel](docs/guides/migrating-from-apache-camel.md)).
- **Run anywhere.** One native binary that starts in **3 ms**, runs in **megabytes**, is its own AWS
  Lambda runtime, and moves **1.7–8.7x the messages** of Apache Camel on the same CPUs, in every
  scenario we measured ([Apache Camel vs Cameleer](https://cameleer.ing/compare/apache-camel-vs-cameleer/)).

**Cameleering** is running Apache Camel routes this way: the same patterns, URIs and YAML, driven
by one native binary instead of the JVM, at the edge, in serverless functions and behind AI agents
([what is cameleering?](https://cameleer.ing/cameleering/)).

[Website](https://cameleer.ing/) · [Docs](https://cameleer.ing/docs/) · [Use cases](https://cameleer.ing/use-cases/) ·
[Quick start](#quick-start) · [Benchmarks](#rebuilt-for-speed-memory-and-scale) ·
[Agents](#the-action-layer-for-ai-agents) · [MCP tools](https://cameleer.ing/use-cases/mcp-tool-server/) · [Serverless](#rebuilt-for-serverless-and-the-edge) ·
[Roadmap](docs/ROADMAP.md) · [FAQ](#faq)

_Cameleer was called rust-camel up to v0.1.0._

> Based on [Apache Camel](https://camel.apache.org/) ([source](https://github.com/apache/camel)), the
> integration framework by the Apache Software Foundation. Cameleer follows its names, options and
> semantics but contains no Camel source code. It is an independent project, not affiliated with or
> endorsed by the ASF. Apache, Apache Camel and Camel are trademarks of the Apache Software Foundation.

---

## Rebuilt for speed, memory and scale

The same routes on Cameleer and on Apache Camel, on the JVM and compiled natively with Quarkus:

```text
Startup (spawn → routes running)                        lower is better
  Cameleer     ▏                                          3 ms
  Camel JVM    ████████████████████████████████████████   1,106 ms
  Camel native █                                          30 ms

Memory, same route (peak)                               lower is better
  Cameleer     ▏                                          8 MB
  Camel JVM    ████████████████████████████████████████   399 MB
  Camel native ███████████▏                               113 MB

REST API with JSON                                      higher is better
  Cameleer     ████████████████████████████████████████   80.3k req/s
  Camel JVM    ██████████████████▏                        37.9k req/s
  Camel native ████████▏                                  17.9k req/s

HTTP into PostgreSQL                                    higher is better
  Cameleer     ████████████████████████████████████████   11.0k req/s
  Camel JVM    █████████████████▏                         4.8k req/s
  Camel native ████████████▏                              3.6k req/s

Kafka stream processing                                 higher is better
  Cameleer     ████████████████████████████████████████   6.0k records/s
  Camel JVM    ████▏                                      0.7k records/s
  Camel native ████▏                                      0.7k records/s

Content-based router                                    higher is better
  Cameleer     ████████████████████████████████████████   1.14M msg/s
  Camel JVM    █████████████▏                             375.3k msg/s
  Camel native ████▏                                      119.5k msg/s
```

| | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| **Startup** | **3 ms** | 1.1 s | 30 ms |
| **Memory under load** | **8–68 MB** | 213–440 MB | 112–198 MB |
| REST API with JSON | **80.3k req/s** | 37.9k req/s | 17.9k req/s |
| HTTP into Kafka | **17.8k req/s** | 10.1k req/s | 9.3k req/s |
| HTTP into PostgreSQL | **11.0k req/s** | 4.8k req/s | 3.6k req/s |
| Kafka stream processing | **6.0k records/s** | 698 records/s | 694 records/s |
| CSV batch, 1,000-row files | **998 files/s** | 468 files/s | 163 files/s |
| Split and aggregate a JSON order | **70.9k orders/s** | 26.7k orders/s | 9.3k orders/s |
| Content-based router | **1.14M msg/s** | 376k msg/s | 120k msg/s |
| In-memory request-reply (`seda`) | **719k msg/s** | 118k msg/s | 73k msg/s |

Every scenario, the method and the raw data: [Apache Camel vs Cameleer](https://cameleer.ing/compare/apache-camel-vs-cameleer/)
and the full report, [docs/BENCHMARKS.md](docs/BENCHMARKS.md). Run it yourself with
[`scripts/cloud-bench`](scripts/cloud-bench/) for about a dollar of cloud time.

**How, without changing what Camel means:**
- **Nothing but the Rust standard library.** TLS, SSH, HTTP/2, gRPC, AMQP, MQTT, the Kafka
  protocol, the PostgreSQL, Redis, MongoDB and Cassandra wire protocols, XML and JSON are all
  written from scratch. There's no dependency tree, no build-time downloads and no supply-chain
  surprise.
- **No JVM, no garbage collector.** The compiler guarantees memory safety, and routing never pauses
  for GC.
- **Engineered hot paths:**
  - a thread-caching allocator;
  - exchange ids created only when read;
  - copy-on-write JSON objects;
  - a two-lock seda queue that spins before it parks;
  - statistics on the CPU's time-stamp clock with per-thread counters.

  The details are in [benchmarks/analysis.md](benchmarks/analysis.md).

---

## Same Camel. Same signature.

Here is a route that sends big orders from Kafka to SQS and taps every order into an audit topic,
written for Apache Camel and for Cameleer:

<table>
<tr><th>Apache Camel (Java)</th><th>Cameleer</th></tr>
<tr><td>

```java
from("kafka:orders")
    .routeId("big-orders")
    .filter(simple("${header.amount} > 100"))
        .to("aws2-sqs:big-orders")
    .end()
    .wireTap("kafka:audit");
```

</td><td>

```rust
b.from("kafka:orders")
    .route_id("big-orders")
    .filter(simple("${header.amount} > 100"))
        .to("aws2-sqs:big-orders")
    .end()
    .wire_tap("kafka:audit");
```

</td></tr>
</table>

Same URIs, same EIPs, same expression. Only the casing follows Rust. If you write Camel YAML, you
don't change anything at all:

<!-- example: readme-big-orders -->
```yaml
- route:
    id: big-orders
    from:
      uri: kafka:orders
      steps:
        - wireTap: kafka:audit
        - filter:
            simple: "${header.amount} > 100"
            steps:
              - to: aws2-sqs:big-orders
```

```text
$ cameleer run big-orders.camel.yaml        # the Camel JBang workflow, as one native binary
```

**What carries over from Camel:**
- endpoint URIs and their options;
- EIP names and semantics: split, aggregate, multicast, circuit breaker, saga, idempotent consumer
  and the rest;
- the Simple, JSONPath, XPath and jq languages;
- `Camel*` headers and exchange properties;
- `onException` and the dead letter channel;
- the YAML and XML DSLs, the REST DSL, Kamelets and route templates;
- health checks, metrics, tracing;
- Camel's `ai-tool` and `ai-resource` endpoints and its `camel.server.mcp-*` properties;
- a Camel JBang-style CLI, `cameleer` (`run`, `init`, `export`, `get`, `trace`), plus `validate`,
  `test` and `tool`.

Your team's Camel experience, designs and routes come with you.

---

## The action layer for AI agents

**The model decides *what*; a tested route decides *how*.** Cameleer feeds events to models,
routes on their answers, and turns agent tool calls into reliable actions with validation,
idempotency, retries, sagas, human approval, rate limits, fallbacks and an audit log. Routes are
served as MCP tools to Claude Desktop, VS Code, Cursor and agent frameworks, called by models in
an `agent` step, or exposed as plain HTTP endpoints.

**🔌 Serve routes as MCP tools.** An existing route becomes a tool by changing one line: its
`from` URI becomes an `ai-tool` URI that names the tool and its arguments
(`uri: "ai-tool:lookup_order?description=Look up an order by id&parameter.orderId=string"`,
[example](examples/agents/route-to-mcp-tool.camel.yaml)). Then one command serves it:
`cameleer run orders.camel.yaml --mcp`, the same in Docker
(`docker run ... pmsudhi/cameleer run /routes/orders.camel.yaml --mcp`), or
`camel.server.mcp-enabled=true` in the Rust crate. Cameleer is both an MCP server and an MCP
client. Coming in the next release (on `main` now): `cameleer run orders.camel.yaml
--mcp-expose rest:*` serves existing routes as tools with no edits, REST operations with typed
arguments, and writes need approval by default
([how](docs/guides/mcp-server.md#expose-existing-routes-without-editing-them)). Written with Camel's `parameters` map, a tool route looks like this:

<!-- example: readme-mcp-tool -->
```yaml
- route:
    id: lookup-order
    from:
      uri: ai-tool:lookup_order
      parameters:
        description: Look up an order by id
        parameter.orderId: string
        parameter.orderId.required: true
        readOnlyHint: true
      steps:
        - setBody:
            simple: '{"orderId": "${header.orderId}", "status": "shipped"}'
```

```text
$ cameleer run tools.camel.yaml --mcp          # Streamable HTTP on http://localhost:8080/mcp
$ cameleer run tools.camel.yaml --mcp-stdio    # for clients that start the server themselves
```

- **Every call goes through the same envelope:** kill switch, authorization, JSON Schema 2020-12
  argument checks, rate limit, circuit breaker, approval, idempotency, compensation, a timeout
  that cancels the route, output checks and redaction, and a hash-chained audit record. Tool
  definitions are pinned, and `cameleer tool disable|approve|deny` acts on a running process.
- **Both MCP protocol eras on one endpoint** (2026-07-28 and 2025-11-25/06-18/03-26), over
  Streamable HTTP and stdio, with API-key, JWT and OAuth token checks. Approvals use the
  protocol's own `input_required` results or `elicitation/create`. The official MCP conformance
  suite passes 39/39 (2025-11-25) and 26/26 (2025-06-18).
- **An MCP client and gateway:** import other MCP servers' tools into the same envelope, with
  rug-pull protection, and serve them again.
- **Prove what runs** (next release, on `main`): inside an Intel TDX or AMD SEV-SNP confidential
  VM, or a dstack CVM, Cameleer returns hardware evidence of its routes, configuration and MCP
  tools, bound to the client's nonce and its own TLS key, over HTTP and in the MCP handshake. It
  does not verify the hardware signature itself yet and has not run on real TEE hardware
  ([attestation](docs/guides/attestation.md)).
- **Measured:** 41,620 MCP tool calls per second in sessions against 22,458 for Apache Camel's
  `camel-mcp-server` on the same machines, in 53 MB against 317 MB (p99 5.9 ms against 5.2 ms)
  ([Apache Camel's MCP server vs Cameleer](https://cameleer.ing/compare/camel-mcp-server-vs-cameleer/),
  [MCP server guide](docs/guides/mcp-server.md)).

An agent decides; something still has to *do* it:
- read from Kafka, call the model, write to Postgres;
- open the ticket, post to Slack;
- retry when the API times out, and never charge a card twice.

That's what Camel has done for enterprises for years. Cameleer does it for agents, as a library
inside your agent or as a sidecar that starts in milliseconds.

**🖐 Give agents hands.** Every action becomes a route the agent can call over HTTP, a queue or
directly, with retries, timeouts, idempotency and a dead letter channel. The agent says *what*; the
route handles *how*, including the failures.
> "Refund order 4411 and tell the customer" → `platform-http` → idempotent consumer → payments API
> with redelivery → `slack` or `aws-ses`, dead letter on failure.

[Direct tool calls vs tool calls through Cameleer](docs/guides/agent-tool-calls.md) compares the two
on a damaged-order refund across four systems, with the tested route.

**⚡ Feed agents events, not polling loops.** Turn Kafka, SQS, MQTT, webhooks, mailboxes and file
drops into model calls. Filter and enrich first, so you pay for fewer tokens. Route on the answer.
> New ticket on `kafka` → drop spam with a filter → `aws-bedrock` (Claude) classifies → urgent ones
> page on-call, the rest go to a queue.

**📚 Ground agents in your data.** Pull documents from S3, Google Drive, Gmail, SFTP or a database,
split them, embed them with OpenAI, Bedrock or Vertex AI, and index them where you search. The same
route keeps the index fresh.
> New file in `aws2-s3` → `split` by paragraph → `openai:embeddings` → `elasticsearch`, with the
> source as metadata.

**🛡 Keep models in check.**
- a circuit breaker with fallback between Bedrock, Azure OpenAI and Vertex AI;
- a throttle to stay under rate limits;
- Bedrock guardrails on inputs and outputs;
- a wire tap of every prompt and answer to an audit log.

> `circuitBreaker` → `aws-bedrock` (primary) → on failure `azure-openai` (fallback) → `wireTap` to
> `kafka:llm-audit`.

Every model component also takes `maxRetries`, `fallbackModels`, `responseCache=exact` and
`coalesce`, plus prompt caching (`anthropic` by default, `aws-bedrock` with `promptCaching=true`),
in the same process as the route, so there is no gateway hop
([model providers](docs/guides/model-providers.md)).

**🔁 Run the agent loop in a route.** The `agent` step runs a model and its tools until the model
answers or a turn, token or time limit is hit; tool calls go through the envelope above, a risky
call pauses the conversation for approval, and `cameleer test` replays recorded model responses so
an agent route is tested without a model ([the agent step](docs/guides/agent-step.md)).

**Models today:** 23 providers, with tool calling in all of them: OpenAI and any OpenAI-compatible
server, including the Responses API, Ollama and `provider=` presets for 19 services (`openai`,
also `llm`), Anthropic (`anthropic`), Gemini (`gemini`), Azure OpenAI (`azure-openai`), AWS Bedrock
with Converse, streaming, guardrails and knowledge bases (`aws-bedrock`), and Google Vertex AI
(`google-vertexai`).

---

## Rebuilt for serverless and the edge

Integrations sit idle, then get busy all at once: exactly the load serverless is priced for. A JVM
fights that model: it takes most of a second to boot and hundreds of megabytes to idle.

- **Scale to zero, wake up instantly.** A 3 ms start fits inside a request's latency budget, and
  `cameleer` is its own [AWS Lambda runtime](docs/DEPLOYMENT.md#aws-lambda).
- **Pack tens of services onto one node.** At 8 MB at startup and tens of MB under load, Cameleer fits on edge gateways
  and small devices that can't host a JVM.
- **One executable, nothing to install.** No JRE to patch and no `-Xmx` to tune. `cameleer export`
  turns YAML routes into a standalone Rust project.
- **Ship only what the routes use.** Every build includes everything by default; with
  [build features](docs/guides/build-features.md) a function keeps only its component families
  (`--features cloud,jq`), down to a 5.9 MB binary with `core`.

| Run it as | Status |
|---|---|
| a library inside your Rust service or agent | ✅ |
| a single binary running YAML or XML routes (`cameleer run`, with hot reload in dev mode) | ✅ |
| a container on Cloud Run, Fargate, Azure Container Apps, Knative or Kubernetes | ✅ |
| a standalone project generated from your routes (`cameleer export`) | ✅ |
| a native AWS Lambda function (built-in custom runtime, `provided.al2023`) | ✅ [AWS Lambda](docs/DEPLOYMENT.md#aws-lambda) |
| an MCP server exposing routes as agent tools (`cameleer run --mcp`) | ✅ [MCP server](docs/guides/mcp-server.md) |

---

## Real uses

| | Trigger | Route | Outcome |
|---|---|---|---|
| **Payments** | payment events on `kafka` | idempotent consumer → enrich from `sql` → model risk score → `choice` | duplicates dropped, risky payments held, the rest settled |
| **Healthcare** (health pack) | HL7 v2 over `mllp` | parse → transform → `http` to the EHR, with redelivery | results delivered exactly once, failures to a dead letter queue |
| **Support** | `aws2-sqs`, `mail` or `telegram` | filter → `aws-bedrock` classifies and drafts → `choice` | urgent issues paged, routine ones answered with a draft |
| **E-commerce** | order webhooks on `platform-http` | `split` line items → route per warehouse → `aggregate` | one fulfilment message per warehouse, stock in `aws2-ddb` |
| **IoT and the edge** | sensor readings on `paho-mqtt5` | `throttle` → `aggregate` per device per minute | rollups to `aws2-timestream`, raw data stays on site |
| **AI ops** | `github` events and alerts on `platform-http` | model summary → dedupe → route by severity | one Slack thread per incident, not a hundred alerts |
| **File exchange** | partner files on `sftp` | unmarshal CSV, XML or EDI → validate → `split` | rows in PostgreSQL, rejects back to the partner |

Every component named here ships today; each row is a pattern you assemble from them. The
[use-case pages](https://cameleer.ing/use-cases/) walk through the tested routes
for AI agent tools, HL7 over MLLP, DICOM imaging, MQTT edge gateways, AWS Lambda and file/SFTP pipelines.

**Healthcare is a pack (next release).** HL7 v2 and MLLP, FHIR and DICOM live in the
[health pack](docs/packs/health/index.md) (`cameleer-health`), not in the default build or image:
use `pmsudhi/cameleer:health` or `cargo install cameleer-main --features health`. Up to 0.5.1,
`mllp`, `hl7` and `fhir` are in the default image. The pack adds DICOM, which Apache Camel does not
have: `dicom` (DIMSE as SCU and SCP: C-ECHO, C-STORE, C-FIND, C-MOVE, C-GET, over TCP or TLS),
`dicomweb` (STOW-RS, QIDO-RS, WADO-RS, client and server), `${dicom.Modality}` in Simple, and
`dicomDeidentify` (the PS3.15 Annex E basic profile and options, without the pixel options). Its
live tests pass 10/10 against DCMTK 3.6.7, Orthanc 1.12.2, python-hl7 and HAPI FHIR; the cloud
DICOM services are not tested yet
([DICOM imaging gateway](https://cameleer.ing/use-cases/dicom-imaging-gateway/),
[DICOM Conformance Statement](docs/packs/health/conformance/dicom-conformance-statement-0.6.0.md)).

---

## Not packaged yet? Build it on the engine

| You want | Build it from today's parts | Packaged version |
|---|---|---|
| vector search for RAG | embeddings → pgvector through `sql`, OpenSearch k-NN, or Qdrant and Pinecone through `http` | 🚧 vector components |
| token streaming to users | model streaming chunks → `websocket` or a server-sent-events endpoint | ✅ parts ship |
| per-tenant token budgets | model usage headers → counters in `spring-redis` → `filter` before the call | ✅ parts ship |
| human approval outside an agent tool | `slack` or `mail` link → `platform-http` callback → resume the parked action | ✅ parts ship (tools have [approvals](docs/guides/mcp-server.md#approvals) built in) |

Sketches for each, plus the planned improvements to agents, serverless, performance, operations and
verification, are in **[docs/ROADMAP.md](docs/ROADMAP.md)**.

---

## Quick start

### With Docker: run a Camel YAML route

Save this as `hello-api.camel.yaml` (or take [examples/hello-api.camel.yaml](examples/hello-api.camel.yaml)):

<!-- example: readme-hello-api -->
```yaml
- rest:
    path: /orders
    get:
      - path: /{id}
        to: direct:getOrder
- route:
    from:
      uri: direct:getOrder
      steps:
        - setBody:
            simple: '{"id": "${header.id}", "status": "shipped"}'
```

Run it and call it:

```sh
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/hello-api.camel.yaml
curl localhost:8080/orders/42      # {"id": "42", "status": "shipped"}
```

The image is a single native binary on distroless, for linux/amd64 and linux/arm64, on
[Docker Hub](https://hub.docker.com/r/pmsudhi/cameleer) and as `ghcr.io/pmsudhi/cameleer`. It runs
as non-root and works with `--read-only`. Any route written for Apache Camel's YAML DSL runs the
same way. Kubernetes, Docker Compose and Cloud Run setups are in the
[deployment guide](docs/DEPLOYMENT.md).

### As a Rust crate: embed it in your program

```toml
[dependencies]
cameleer-core = "0.5"
cameleer-http = "0.5"
```

```rust
use cameleer_core::prelude::*;
use cameleer_http::rest::RestDslExt;

fn main() -> Result<()> {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    ctx.properties().set("camel.server.port", "8080");
    ctx.add_routes(|b| {
        b.rest("/api/greet").get("/{name}").to("direct:greet");
        b.from("direct:greet")
            .set_header("Content-Type", constant("application/json"))
            .set_body(simple(r#"{"greeting": "Hello, ${header.name}!"}"#));
    })?;
    ctx.start()?;
    loop { std::thread::park(); }
}
```

To run YAML routes with every component from your own program, depend on `cameleer-main` and
call `cameleer_main::new_main()`, then `add_route_file(...)` and `run()`
([examples/embed_yaml.rs](crates/cameleer-main/examples/embed_yaml.rs)).

### The `cameleer` CLI

From crates.io:

```sh
cargo install cameleer-main
cameleer run hello-api.camel.yaml
```

Or from source:

```sh
git clone https://github.com/pmsudhi/cameleer && cd cameleer
cargo build --release -p cameleer-main
target/release/cameleer run examples/rest-api.camel.yaml
```

Camel JBang habits carry over: `cameleer run`, `cameleer ps`, `cameleer stop`.

### A smaller build

The image, `cargo install` and the default build include every component except the
[packs](docs/packs/index.md), so any route runs without setup. For a function or an edge device, keep only the families your routes use:

```sh
cargo install cameleer-main --no-default-features --features cloud,jq,sql
docker build --build-arg FEATURES=cloud,jq,sql -t my-cameleer .
```

A feature list replaces the default, so name every family you use. Each component page names
its feature, and a route that needs a missing one fails at startup with the feature to add. The
[build features guide](docs/guides/build-features.md) lists them all.

### Documentation

Every component, language, data format and EIP has a page with tested examples:
[cameleer.ing/docs](https://cameleer.ing/docs/) (source in [docs/](docs/README.md)).
Start with [getting started](docs/guides/getting-started.md), [build features](docs/guides/build-features.md), the
[agent cookbook](docs/guides/agents.md), the [edge cookbook](docs/guides/edge.md) or the
[component index](https://cameleer.ing/docs/components/).

### Connect your coding agent

Give Cursor, Claude, Windsurf, VS Code or any MCP-capable assistant Cameleer's real docs, so it
writes routes from the documentation instead of guessing. The docs MCP server is
`https://gitmcp.io/pmsudhi/cameleer` (it serves this repository's `llms.txt`, docs and README).

| tool | add this |
|---|---|
| Claude Code | `claude mcp add --transport http cameleer-docs https://gitmcp.io/pmsudhi/cameleer` |
| Cursor (`~/.cursor/mcp.json`) | `{"mcpServers": {"cameleer-docs": {"url": "https://gitmcp.io/pmsudhi/cameleer"}}}` |
| Windsurf (`~/.codeium/windsurf/mcp_config.json`) | `{"mcpServers": {"cameleer-docs": {"serverUrl": "https://gitmcp.io/pmsudhi/cameleer"}}}` |
| VS Code (`.vscode/mcp.json`) | `{"servers": {"cameleer-docs": {"type": "http", "url": "https://gitmcp.io/pmsudhi/cameleer"}}}` |
| Claude Desktop (`claude_desktop_config.json`) | `{"mcpServers": {"cameleer-docs": {"command": "npx", "args": ["mcp-remote", "https://gitmcp.io/pmsudhi/cameleer"]}}}` |

The docs are also indexed by [Context7](https://context7.com) (ask your assistant to "use
context7" for Cameleer) and [DeepWiki](https://deepwiki.com/pmsudhi/cameleer). Agents that read
files directly can start from [llms.txt](https://cameleer.ing/llms.txt) or
[llms-full.txt](https://cameleer.ing/llms-full.txt).

### Real use cases, ready to run

Every file below runs end to end in the test suite
([crates/cameleer-main/tests/examples.rs](crates/cameleer-main/tests/examples.rs)) against an
embedded Kafka broker and mock S3, Slack and OpenAI servers. Each file's header lists the
environment variables it needs and the exact `docker run` command.

| use case | route | what it does |
|---|---|---|
| Smallest REST API | [hello-api.camel.yaml](examples/hello-api.camel.yaml) | `GET /orders/{id}` returns JSON: the Quick start above. |
| REST API | [rest-api.camel.yaml](examples/rest-api.camel.yaml) | JSON REST endpoints with validation, a content-based router and error handling. Needs nothing else. |
| AI ticket triage | [ai-ticket-triage.camel.yaml](examples/ai-ticket-triage.camel.yaml) | Kafka in, an LLM classifies each ticket P1 or P3, urgent tickets go to their own topic. Works with OpenAI or any OpenAI-compatible server (Ollama, vLLM, Azure OpenAI). |
| Alerts to Slack | [alerts-to-slack.camel.yaml](examples/alerts-to-slack.camel.yaml) | An HTTP webhook takes monitoring alerts, keeps the critical ones and posts them to Slack. |
| Files to S3 | [files-to-s3.camel.yaml](examples/files-to-s3.camel.yaml) | Watches a folder and uploads each new CSV file to S3, then moves it to `.done`. |
| Rust REST service | [rest_service.rs](crates/cameleer-main/examples/rest_service.rs) | The Rust DSL with two crates: REST endpoints, JSON, an echo service. |
| Embed YAML in Rust | [embed_yaml.rs](crates/cameleer-main/examples/embed_yaml.rs) | Your own binary that runs Camel YAML routes with every component. |

The AI triage route, for example:

<!-- example: readme-ai-ticket-triage -->
```yaml
- route:
    id: ticket-triage
    from:
      uri: "kafka:support-tickets?brokers={{env:KAFKA_BROKERS:localhost:9092}}&groupId=triage&autoOffsetReset=earliest"
      steps:
        - setProperty:
            name: ticket
            simple: "${body}"
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&temperature=0&systemMessage=Classify the support ticket. Reply P1 for outages, data loss or security issues, otherwise P3. Reply with P1 or P3 only."
        - setHeader:
            name: priority
            simple: "${body}"
        - setBody:
            simple: "${exchangeProperty.ticket}"
        - choice:
            when:
              - simple: "${header.priority} contains 'P1'"
                steps:
                  - to: "kafka:tickets-urgent?brokers={{env:KAFKA_BROKERS:localhost:9092}}"
            otherwise:
              steps:
                - to: "kafka:tickets-routine?brokers={{env:KAFKA_BROKERS:localhost:9092}}"
```

On Linux (with Docker Desktop, use `KAFKA_BROKERS=host.docker.internal:9092` instead of
`--network host`):

```sh
docker run --rm --network host -e KAFKA_BROKERS=localhost:9092 -e OPENAI_API_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/ai-ticket-triage.camel.yaml
```

---

## FAQ

**Is it a drop-in replacement for Apache Camel?**
For routes, mostly: URIs, options, EIPs, headers and Simple follow Camel, and Camel YAML and XML
routes run. Java code doesn't: processors, beans and custom components are written in Rust.

**Does it have every Camel connector?**
No. It has about 230 components, languages and data formats, including all of Camel's AWS, Azure and
Google Cloud components, but not the long tail of vendor connectors.
[docs/FEATURES.md](docs/FEATURES.md) lists them row by row, including known gaps.

**Does it run my LLM or store vectors?**
No. It calls models and moves embeddings into the stores you already run. It isn't a model runtime
or a vector database.

**Does it have an MCP server?**
Yes. `ai-tool`, `ai-resource` and `ai-prompt` routes are served over Streamable HTTP or stdio
with `cameleer run --mcp` or `--mcp-stdio`, using Apache Camel's `camel.server.mcp-*` property
names, and every call goes through the reliability envelope
([MCP server guide](docs/guides/mcp-server.md)).

**Do I need a JVM or Maven?**
No. You need Rust to build it, and nothing at run time.

**Why do management statistics default differently from Camel?**
Camel collects statistics only when `camel-management` is loaded. Cameleer always has management,
so by default it keeps route and context counters and makes per-step timing opt-in
(`ManagementStatisticsLevel::Default`, or `camel.main.jmxManagementStatisticsLevel=Default`).

**How is it verified?**
- 2,800+ hermetic tests against embedded servers;
- the official MCP conformance suite: 39/39 (2025-11-25) and 26/26 (2025-06-18);
- live suites against real PostgreSQL, Redis, RabbitMQ, ActiveMQ, Mosquitto, vsftpd,
  Postfix/Dovecot and nginx;
- official clients: librdkafka, pymongo, the DataStax driver, boto3, the Azure SDK, gcloud and
  grpcio;
- emulators: moto, Azurite, fake-gcs-server and FerretDB.

See [docs/INTEROP.md](docs/INTEROP.md).

---

## Crates

| crate | contents |
|---|---|
| `cameleer-core` | `CamelContext`, `Exchange`/`Message`/`Value`, routes and the Rust DSL, EIPs, error handling, languages, data formats, core components |
| `cameleer-http` | HTTP/1.1 server and client, `platform-http`, `http`, REST DSL with OpenAPI, `netty` TCP/UDP |
| `cameleer-yaml` | YAML parser, Camel YAML DSL, XML DSL, route templates |
| `cameleer-protocols` | ftp, smtp/pop3/imap, websocket, syslog (with embedded test servers) |
| `cameleer-health` | the health pack, not in the default build: HL7 v2 and MLLP, FHIR R4, DICOM (DIMSE, DICOMweb), de-identification |
| `cameleer-tls` | from-scratch TLS 1.3/1.2, X.509, `SslContextParameters` (used by all network components) |
| `cameleer-kafka-mqtt` | kafka, paho / paho-mqtt5 (with embedded test brokers) |
| `cameleer-amqp` | spring-rabbitmq / rabbitmq (AMQP 0-9-1), stomp, jms / activemq / amqp-stomp (with embedded test brokers) |
| `cameleer-amqp10` | AMQP 1.0 client (SASL, TLS, CBS), amqp (JMS over AMQP 1.0), azure-servicebus, azure-eventhubs |
| `cameleer-sql` | embedded SQL engine, PostgreSQL client, sql / jdbc components, JDBC repositories |
| `cameleer-cloud` | AWS (incl. Bedrock), Azure (incl. OpenAI), Google Cloud (incl. Vertex AI), with verifying emulators |
| `cameleer-ssh` | SSH-2 + SFTP: sftp and ssh components |
| `cameleer-grpc` | HTTP/2 + HPACK, protobuf, grpc component and protobuf data format |
| `cameleer-datastores` | spring-redis, mongodb, elasticsearch/opensearch, cql (Cassandra), couchdb (with embedded test servers) |
| `cameleer-xml` | XML 1.0 parser + DOM, XPath 1.0, XSLT 1.0, XML Schema validation, `xj`, `xmlsecurity-sign`/`-verify` |
| `cameleer-ai` | `ai-tool`, `ai-resource`, `ai-prompt`, the tool registry and reliability envelope, the MCP server, client and gateway, the `mcp` component |
| `cameleer-saas` | openai/llm, anthropic, gemini, slack, telegram, github, twilio, jira, webhook, graphql, rss/atom (with embedded mock servers) |
| `cameleer-main` | `Main` runner (application.properties, route discovery) and the `cameleer` CLI; each family crate above is one of its [cargo features](docs/guides/build-features.md) |
| `cameleer-bench` | benchmark harness, sampling profiler and HTTP load generator |

Internals: [ARCHITECTURE.md](ARCHITECTURE.md).

## Build, test and check the numbers yourself

```text
cargo build --release
cargo test --workspace          # hermetic: every network component runs against embedded servers
scripts/bench.sh                # Cameleer vs Apache Camel (Linux or Git Bash; .\scripts\bench.ps1 on Windows)
```

The live interop suites run against real servers and are opt-in:

```text
sudo scripts/live-interop.sh all
CAMEL_LIVE_INTEROP=1 cargo test --workspace live_
```

## Contributing, security and citing

Contributions are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md) and the
[Code of Conduct](CODE_OF_CONDUCT.md). Report vulnerabilities privately as described in
[SECURITY.md](SECURITY.md). To cite Cameleer, use the "Cite this repository" button
([CITATION.cff](CITATION.cff)).

## Supported by

Cameleer is supported by [DataHQ](https://datahq.ai).

<a href="https://datahq.ai"><img src="site/brand/datahq-logo.png" alt="DataHQ" width="200"></a>

## License and credits

Cameleer is licensed under the [Apache License 2.0](LICENSE); see [NOTICE](NOTICE).

It exists because of [Apache Camel](https://camel.apache.org/)
([github.com/apache/camel](https://github.com/apache/camel)) and its community, whose design,
documentation and Enterprise Integration Patterns catalogue it follows. Apache, Apache Camel and
Camel are trademarks of the Apache Software Foundation; Cameleer is not affiliated with or
endorsed by the ASF.
