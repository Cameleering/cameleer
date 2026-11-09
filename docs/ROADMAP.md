# Roadmap: what you can build today, and what comes next

Cameleer ships an integration engine (routes, about 230 connectors, the Enterprise Integration
Patterns and the languages) and, since 0.5.0, an action layer for AI agents (tools, the MCP server
and client, the `agent` step). Some things agent and serverless teams want don't exist as packaged
components yet. This page covers two things:

1. **[Build it on the engine today](#1-build-it-on-the-engine-today):** how to assemble each missing
   piece from what already ships. These are patterns, not shipped features; the sketches show the
   route shape, and you fill in names, URIs and credentials.
2. **[Enhancement opportunities](#2-enhancement-opportunities):** what we intend to package or
   improve, with the evidence behind each item.

Status labels: ✅ available · 🧩 build it from existing components (pattern below) · 🚧 not yet.

---

## 1. Build it on the engine today

### 1.1 Agent tool server ✅

**Goal:** an agent calls named tools, and each tool is a route with retries, timeouts and
idempotency.

**Shipped in 0.5.0:** the [MCP server](guides/mcp-server.md) serves
[`ai-tool`](components/ai-tool.md) routes to any MCP client, with schema checks, approvals,
deduplication, timeouts and audit on every call. Start with [MCP](guides/mcp.md).

**Without MCP:** for agents that call plain HTTP, one `platform-http` route receives tool calls
and dispatches them by name to a `direct:` route per tool with `toD`. Each tool route is ordinary
Camel: error handling, idempotent consumer, circuit breaker.

<!-- example: roadmap-agent-tools -->
```yaml
# sketch: a tool endpoint dispatching to one route per tool
- route:
    id: agent-tools
    from:
      uri: platform-http:/tools/call
      steps:
        - setHeader:
            name: tool
            jsonpath: "$.name"
        - setBody:
            jsonpath: "$.arguments"
        - toD: "direct:tool-${header.tool}"
- route:
    id: tool-refund
    from:
      uri: direct:tool-refund
      steps:
        - idempotentConsumer:
            jsonpath: "$.orderId"
            steps:
              - to: "http://payments.internal/refunds?httpMethod=POST"
```

### 1.2 Tool-calling agent loop ✅

**Goal:** let the model choose tools until it has an answer.

**Shipped in 0.5.0:** every model component (`openai`/`llm`, `anthropic`, `gemini`,
`azure-openai`, `aws-bedrock`, `google-vertexai`) runs the tool loop itself when given `tags`,
and the [`agent` step](guides/agent-step.md) makes the loop one step of a route, with turn,
token and time limits, memory and approvals.

### 1.3 Vector store ingestion and retrieval 🧩

**Goal:** store embeddings and query them for RAG.

**How:**
- Embed with `openai:embeddings`, `aws-bedrock` (`invokeEmbeddingsModel`) or `google-vertexai`
  (`generateEmbeddings`).
- Store them with what already ships:
  - **PostgreSQL with pgvector** through the `sql` component: plain SQL with a vector literal
    (`INSERT ... VALUES (:#id, :#embedding::vector)`, and for retrieval
    `ORDER BY embedding <-> :#query::vector LIMIT 5`). The vector operators run in PostgreSQL.
  - **OpenSearch or Elasticsearch** through the `elasticsearch` component: index documents with a
    vector field; k-NN queries go through the search operation.
  - **Qdrant, Pinecone, Weaviate or Milvus** through their REST APIs with the `http` component
    and the `json` data format.
- Bedrock knowledge bases are ✅ through `aws-bedrock-agent-runtime` (`retrieve`,
  `retrieveAndGenerate`).

Dedicated vector components are on the roadmap ([2.1](#21-agents-and-ai)).

### 1.4 AWS Lambda function ✅

**Goal:** run routes as a Lambda function rather than a container.

**How:** `cameleer` is its own Lambda custom runtime (`provided.al2023`). Package the routes with
`scripts/package-lambda.sh`: API Gateway and function URL events go to the `platform-http`
routes, other events to `direct:lambda` (or the endpoint in the function's Handler setting).
See [AWS Lambda](DEPLOYMENT.md#aws-lambda). Containers on Cloud Run, Fargate, Azure Container
Apps or Knative work as before.

### 1.5 Streaming model output to users 🧩

**Goal:** forward tokens to a browser or client as they arrive.

**How:**
- `openai`/`llm` streams with `streaming=true` and a `streamingChunkProcessor` called per delta,
  also inside the tool loop.
- `aws-bedrock`, `azure-openai` and `google-vertexai` support streaming with
  `streamOutputMode=chunks` and a `streamingChunkProcessor` called per chunk.
- In that processor, send each chunk to a `websocket` endpoint, or write it to a seda queue that a
  server-sent-events endpoint drains.

### 1.6 Token and cost budgets 🧩

**Goal:** cap spend per tenant or per agent.

**How:**
- Read the usage headers the model components set (prompt and completion tokens).
- Add them to a counter in `spring-redis` or `sql`, keyed by tenant.
- Put a `filter` or `choice` before the model call that rejects or downgrades the model once the
  budget is spent.
- Use `throttle` for a hard requests-per-second cap.

### 1.7 Human in the loop ✅ / 🧩

**Goal:** an agent's risky action waits for a person.

**Built in since 0.5.0:** an `ai-tool` with `approval` parks the call until someone approves it
(`cameleer tool approve`, `ai-tool:x?operation=approve`, or the MCP client's approval form), and
a tool route can ask the user a question with `ai-tool:current?operation=elicit`. See
[MCP: manager approval](guides/mcp/manager-approval.md).

**Your own flow:**
- Route the proposed action to `slack`, `telegram` or `mail` with an approval link that hits a
  `platform-http` route.
- Park the pending action in a repository (`sql` or `spring-redis`) keyed by an id.
- On approval, continue the action route. A `claimCheck` or the aggregator with
  `completionTimeout` handles expiry.

---

## 2. Enhancement opportunities

### 2.1 Agents and AI

| Item | Why | Status |
|---|---|---|
| **MCP server**: routes as MCP tools, resources and prompts ([ai-tool](components/ai-tool.md), [MCP server guide](guides/mcp-server.md)) | the standard way agents discover and call tools, with the reliability envelope on every call | ✅ 0.5.0 |
| **Tool calling in every model component** (`openai`/`llm`, `anthropic`, `gemini`, `azure-openai`, `aws-bedrock`, `google-vertexai`) | removes the main gap for the agent loop in 1.2 | ✅ 0.5.0 |
| **The `agent` step**: model call → tool dispatch → loop, with turn, token and time limits, memory and approvals ([guide](guides/agent-step.md)) | packages 1.2 so it's one step in a route | ✅ 0.5.0 |
| **Vector store components**: pgvector, Qdrant, OpenSearch k-NN, Pinecone | first-class retrieval instead of SQL/REST in 1.3 | 🚧 |
| **MCP client and gateway**: other MCP servers' tools behind the same envelope ([guide](guides/mcp-client.md)) | one place to govern every tool an agent can call | ✅ 0.5.0 |
| **Streaming in `openai`** | parity with the Bedrock, Azure and Vertex components | ✅ 0.5.0, also in the tool loop |
| **Prompt templates and response caching** | cut cost and latency for repeated prompts | ✅ 0.5.0: `ai-prompt`, `responseCache=exact`, `coalesce`, provider prompt caching ([model providers](guides/model-providers.md)) |
| **Anthropic and Gemini API components** | direct access without Bedrock or Vertex | ✅ 0.5.0: `anthropic`, `gemini`, and 19 OpenAI-compatible presets |

### 2.2 Serverless and edge

| Item | Why | Status |
|---|---|---|
| **AWS Lambda runtime adapter**: route as a Lambda handler, with events mapped to exchanges | packages 1.4; Lambda is the largest serverless platform | ✅ built into `cameleer run` ([AWS Lambda](DEPLOYMENT.md#aws-lambda)) |
| **Published container images and deployment examples** | shows scale-to-zero end to end | ✅ images on Docker Hub and ghcr (amd64, arm64); [deployment guide](DEPLOYMENT.md) for Docker, Compose, Kubernetes, Cloud Run and AWS Lambda. 🚧 worked examples for Fargate, Azure Container Apps and Knative |
| **Static and ARM64 builds tested** (musl, aarch64 Linux) | edge gateways and Graviton or Ampere hosts | ✅ partly: the arm64 image is published and static musl is measured (3–22% slower than glibc, [benchmarks](BENCHMARKS.md#faas-cold-start-and-size)); 🚧 the hermetic suite on aarch64 |
| **Binary size budget and feature flags per crate** | smaller images for functions that need only a few connectors | ✅ a feature per component family, plus `templates`, `jq`, `xpath`, `http2` and `validate` ([build features](guides/build-features.md)) |

### 2.3 Performance

Current results: [BENCHMARKS.md](BENCHMARKS.md); analysis: [../benchmarks/analysis.md](../benchmarks/analysis.md).

| Item | Why | Status |
|---|---|---|
| **HTTP benchmark with the load generator on a second machine** | a fair HTTP comparison | ✅ `scripts/cloud-bench`: Cameleer 1.7x Camel on a fixed reply, 68% of a do-nothing server ([the HTTP ceiling](BENCHMARKS.md#the-http-ceiling)) |
| **Event-driven HTTP server core** | plain HTTP is the one scenario under 2x Camel; the request path, not the work, is the cost | 🚧 next step |
| **Shared JSON object keys** (`ObjectMap` API change) | JSON parsing is the largest cost in split/aggregate (2.6x Camel today) | 🚧 |
| **A smaller `Exchange`** (box `exception`, group `from_*`) | fewer bytes copied per split part and per seda hand-off | 🚧 |
| **Kafka benchmark scenario** | a real broker on both sides | ✅ Kafka 4.3.2 in `scripts/cloud-bench`: 8.7x Camel on stream processing ([benchmarks](BENCHMARKS.md)) |

### 2.4 Operations

| Item | Why | Status |
|---|---|---|
| **OpenTelemetry tracing export** | trace a request across agents, routes and services; today there are Prometheus metrics, tracing and message history | 🚧 |
| **Windows default time zone** (CLDR `windowsZones` map) | `TimeZone::local` is UTC on Windows unless `TZ` is set | 🚧 [#15](https://github.com/pmsudhi/cameleer/issues/15) |
| **Graceful stop for the remaining direct `TcpStream::shutdown` calls** | so `stop()` never hangs on idle peers on Windows | 🚧 |

### 2.5 Verification and trust

| Item | Why | Status |
|---|---|---|
| **Real cloud accounts tier** (AWS, Azure, GCP) | today the cloud components are verified against emulators and official SDKs | 🚧 needs the owner's credentials |
| **Docker-based real servers** (Kafka, Elasticsearch, Cassandra, CouchDB, MongoDB, Artemis) | beyond the protocol emulators | 🚧 |
| **Independent security audit and fuzzing** of the from-scratch TLS, SSH, XML security and protocol parsers | the price of having no dependencies is owning that code | 🚧 |

The verification and operations items are also tracked in [STATUS.md](STATUS.md).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
