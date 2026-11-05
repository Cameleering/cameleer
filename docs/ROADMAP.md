# Roadmap: what you can build today, and what comes next

Cameleer ships an integration engine: routes, about 230 connectors, the Enterprise Integration
Patterns and the languages. Some things agent and serverless teams want don't exist as packaged
components yet. This page covers two things:

1. **[Build it on the engine today](#1-build-it-on-the-engine-today):** how to assemble each missing
   piece from what already ships. These are patterns, not shipped features; the sketches show the
   route shape, and you fill in names, URIs and credentials.
2. **[Enhancement opportunities](#2-enhancement-opportunities):** what we intend to package or
   improve, with the evidence behind each item.

Status labels: ✅ available · 🧩 build it from existing components (pattern below) · 🚧 not yet.

---

## 1. Build it on the engine today

### 1.1 Agent tool server (MCP-style) 🧩

**Goal:** an agent calls named tools over HTTP, and each tool is a route with retries, timeouts
and idempotency.

**How:** one `platform-http` route receives tool calls and dispatches them by name to a `direct:`
route per tool with `toD`. Each tool route is ordinary Camel: error handling, idempotent consumer,
circuit breaker. A second endpoint returns the tool list (a static JSON body, or one built from route
descriptions). Full MCP protocol support (JSON-RPC sessions, `tools/list` schemas) is the packaged
version on the roadmap ([2.1](#21-agents-and-ai)).

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

### 1.2 Tool-calling agent loop 🧩

**Goal:** let the model choose tools until it has an answer.

**How:**
- `azure-openai` returns the model's tool calls in headers.
- A `loop` (with `doWhile`) calls the model, uses `choice` to stop when there are no more tool calls,
  and runs the requested tools with `recipientList` or `toD` to their `direct:` routes.
- An aggregation strategy appends each tool result to the message list for the next turn.

Keep the turn count bounded with the loop's count limit, and the cost bounded with `throttle`. The
`openai` and `aws-bedrock` components don't pass tool definitions yet ([2.1](#21-agents-and-ai)).

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

### 1.7 Human in the loop 🧩

**Goal:** an agent's risky action waits for a person.

**How:**
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
| **MCP server component**: expose routes as MCP tools (JSON-RPC, `tools/list` with JSON schemas, `tools/call`) | the standard way agents discover and call tools; today it's the pattern in 1.1 | 🚧 |
| **Tool calls in `openai` and `aws-bedrock`**: pass tool definitions and return tool calls (as `azure-openai` already does) | removes the main gap for the agent loop in 1.2 | 🚧 |
| **An agent EIP / DSL step**: model call → tool dispatch → loop, with turn and token limits | packages 1.2 so it's one step in a route | 🚧 |
| **Vector store components**: pgvector, Qdrant, OpenSearch k-NN, Pinecone | first-class retrieval instead of SQL/REST in 1.3 | 🚧 |
| **Streaming in `openai`** | parity with the Bedrock, Azure and Vertex components | 🚧 |
| **Prompt templates and response caching** | cut cost and latency for repeated prompts; reuse the templating languages | 🚧 |
| **Anthropic API component** | direct access without Bedrock or Vertex | 🚧 (reachable today through `aws-bedrock` and `google-vertexai`) |

### 2.2 Serverless and edge

| Item | Why | Status |
|---|---|---|
| **AWS Lambda runtime adapter**: route as a Lambda handler, with events mapped to exchanges | packages 1.4; Lambda is the largest serverless platform | ✅ built into `cameleer run` ([AWS Lambda](DEPLOYMENT.md#aws-lambda)) |
| **Published container images and deployment examples** (Cloud Run, Fargate, Azure Container Apps, Knative) | shows scale-to-zero end to end | 🚧 |
| **Static and ARM64 builds tested** (musl, aarch64 Linux) | edge gateways and Graviton or Ampere hosts; today the hermetic suite runs on Windows and Linux x86-64 | 🚧 |
| **Binary size budget and feature flags per crate** | smaller images for functions that need only a few connectors | ✅ feature per component family ([smaller builds](DEPLOYMENT.md#smaller-builds)) |

### 2.3 Performance

Current results: [BENCHMARKS.md](BENCHMARKS.md); analysis: [../benchmarks/analysis.md](../benchmarks/analysis.md).

| Item | Why | Status |
|---|---|---|
| **HTTP benchmark with the load generator on a second machine** | on one machine even a do-nothing server tops out near 400k req/s (3.6x Java), so 4x can't be shown there | 🚧 |
| **Event-driven HTTP server** (IOCP on Windows, epoll on Linux), opt-in, with the blocking server as fallback | scalability with many idle connections; on one machine it measured no faster than threads | 🚧 decision pending |
| **Shared JSON object keys** (`ObjectMap` API change) | JSON parsing is the largest cost in split/aggregate (3.18x today) | 🚧 |
| **A smaller `Exchange`** (box `exception`, group `from_*`) | fewer bytes copied per split part and per seda hand-off | 🚧 |
| **Kafka benchmark scenario** | needs a real broker on both sides | 🚧 |

### 2.4 Operations

| Item | Why | Status |
|---|---|---|
| **OpenTelemetry tracing export** | trace a request across agents, routes and services; today there are Prometheus metrics, tracing and message history | 🚧 |
| **Windows default time zone** (CLDR `windowsZones` map) | `TimeZone::local` is UTC on Windows unless `TZ` is set | 🚧 |
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
