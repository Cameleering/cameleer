# Design: the action layer for AI agents

_Status: proposal for review, 2026-11-06. Branch `agent-action-layer`._

> **The claim we build toward.** Every action an AI agent takes through Cameleer is described by a
> pinned, validated schema; runs once, even when the agent or the network retries; is allowed by a
> policy written as code; waits for a person when it should; rolls back when a later step fails;
> and leaves an audit record. Agents reach it over MCP, or through the tool calling of any model
> Cameleer talks to, with the same guarantees.

This document turns three studies into a feature set and a delivery plan. The studies cover:
- Apache Camel 4.22 and 4.23's AI features
- the competing "action layer" and MCP products
- the MCP specification and tool-calling practice

Sources are listed at the end.

## 1. Where things stand

### What Apache Camel offers

Camel 4.22 and 4.23 (LTS) ship:
- `camel-ai-tool`: routes as tools, with flat `parameter.*` options or a raw schema, tags, and the
  four MCP hints.
- `camel-mcp-server`: Preview, Streamable HTTP and stdio, tools and resources.
- Tool loops inside the `openai`/`llm` and `langchain4j-agent` producers.
- Five vector store components and the `a2a` component.

Its gaps, all confirmed in its docs, JIRA and pull requests:

- **Reliability is left to the user.** The tool hints (`destructiveHint`, `idempotentHint`) are
  documented as advisory and not enforced. Tool calls are not tied to the idempotent consumer, and
  the MCP server drops the tool-call id. There is no human approval, no audit log, and nothing
  backed by a saga.
- **Weak contracts.**
  - Arguments are not validated against their JSON Schema; only required top-level fields are
    checked.
  - Results are the body's `toString()`.
- **MCP gaps.**
  - The protocol revision is not stated.
  - No prompts, no resource subscriptions, no elicitation.
  - A timed-out call keeps running and leaks its exchange.
- **No MCP client component.** Client code is copied into three producers, each with different
  transports.
- **Uneven model support.**
  - Bedrock takes raw SDK objects with no loop.
  - Vertex has no function calling.
  - Streaming can't be combined with the tool loop.
- **No agent EIP.** An agent is always a producer endpoint.
- **Churn.** Two tool components were removed within two releases, and `openai` was renamed `llm`.

### What competitors offer

- **Hosted catalogs:** Composio, Arcade, Paragon ActionKit, Pipedream, Zapier MCP, viaSocket.
  They sell connector count (1,000–30,000 actions) and managed per-user OAuth.
- **iPaaS vendors:** MuleSoft Agent Fabric, Workato, Boomi, Tray. They sell governance in a UI.
- **MCP gateways:** Azure APIM, Kong, agentgateway, Docker, ContextForge. They sell access
  control in front of tools.

**Table stakes in 2026:**
- an MCP server over Streamable HTTP and stdio, with OAuth 2.1
- typed tool schemas and per-tool access control
- retries and rate limits
- approval for write actions
- call logs and OpenTelemetry tracing

**What nobody offers:**
- **Exactly-once side effects.** Durable-execution engines (Temporal, Inngest, LangGraph) leave
  idempotency keys to the user, and practitioners report duplicate emails and invoices.
- **Rollback across tools.**
- **Approval and policy as versioned code, per argument.**
- **Testing tools without a model.**
- **Pinned tool definitions that can't silently change** (the "rug pull").
- **Self-hosting below the Enterprise price tier.**

### What MCP requires

The current revision is **2026-07-28**, and it is stateless:
- no `initialize`, no sessions, and every request carries its protocol version and capabilities
  in `_meta`
- a mandatory `server/discover`
- multi round-trip requests (`resultType: "input_required"`) instead of server-to-client
  requests, which is how a tool asks for confirmation
- `subscriptions/listen`
- caching hints (`ttlMs`, `cacheScope`)
- required `Mcp-Method`/`Mcp-Name` headers

Most clients still speak **2025-11-25**, with the `initialize` handshake and `Mcp-Session-Id`. The
spec defines a dual-era server and client: detect the version from the first message.

### What Cameleer has today

- **Shipped:**
  - REST tool routes, the pattern in roadmap 1.1
  - `json-validator`, which validates against JSON Schema but lacks the 2020-12 keywords
  - idempotent consumer with memory, file, JDBC and Redis repositories
  - saga, throttle, circuit breaker
  - the human-approval example
  - `cameleer validate` and `cameleer schema`
  - streaming HTTP response bodies, the basis for Server-Sent Events
  - HMAC and AES-GCM in `cameleer-tls`
  - SigV4 signing
  - an embedded PostgreSQL client, the basis for pgvector
- **Model components:**
  - `openai`: chat and embeddings, no tools
  - `azure-openai`: returns tool calls in a header
  - `aws-bedrock` `converse`: no tool config
  - `google-vertexai`: no function calling
  - `aws-bedrock-agent-runtime`
- **Missing:** an MCP protocol, an agent step, tool calls in most providers, an MCP client, vector
  stores, an audit log.

## 2. Principles

1. **Camel names first.**
   - Camel users find `ai-tool`, `ai-resource`, the `camel.server.mcp-*` properties,
     `openai:`/`llm:` operations and `CamelOpenAI*` headers where they expect them, with the same
     defaults.
   - Cameleer's additions are new options with Camel-style names, marked as extensions in the docs.
2. **Enforce what Camel only advises.** A hint or policy on a tool changes what happens: it
   validates, deduplicates, asks for approval, audits. It never just annotates.
3. **One tool model, every surface.** The same tool definition serves MCP, every model
   component's tool calling and the agent step, with the same guarantees.
4. **Hermetic and std-only.**
   - Everything is implemented from the specs on the existing HTTP, TLS and JSON code.
   - Every feature has a hermetic test against embedded mock servers.
   - Official conformance tools run in a gated tier, like the simulator tier.
5. **Honest scope.** We don't compete on connector count or a hosted OAuth catalog. We compete on
   reliability, control, footprint and Camel compatibility.

## 3. The feature set

Each item names the Camel or competitor evidence behind it and marks Camel parity versus a
Cameleer advantage.

### A. Tool model (`cameleer-ai`, new crate)

- **`ai-tool:name` consumer.** Camel parity, every option and semantic:
  - Options: `description`, `title`, `tags`, `parameter.NAME` (with `.description`, `.required`,
    `.enum`), `argSchema`, `outputSchema`, `outputParameter.*`, `argumentsAsBody`, `searchable`,
    `returnDirect`, the four `*Hint` annotations, and `authorizationPolicy`.
  - Semantics:
    - Arguments become headers.
    - `camel*` and undeclared arguments are dropped.
    - Each call runs on an exchange copy that keeps only properties and variables, with its own
      unit of work.
    - An empty result is "No result".
    - `CamelAiToolCallId` and `CamelAiToolConversationId` are set.
- **`ai-resource:name`** (Camel parity) and **`ai-prompt:name`** (advantage: Camel has no MCP
  prompts).
- **Tool registry** in the context, selected by tags. It is built when the routes start, so tools
  can't go missing in the startup race Camel fixed in CAMEL-25000.
- **Full JSON Schema 2020-12 validation** of arguments and of output. Advantage: Camel only checks
  required fields.
  - `json-validator` gains `$defs`, `$ref` to local definitions, `prefixItems`,
    `unevaluatedProperties`, `const`, `dependentRequired` and `if`/`then`/`else`.
  - Network `$ref`s are never fetched, and schema depth and size are bounded.
  - A validation failure becomes an error the model can act on: it names each field and gives an
    example.
- **Structured results.** Advantage: a map or list body becomes JSON (`structuredContent` plus its
  text mirror) instead of `toString()`.

### B. The reliability envelope (the differentiator)

Declared on `ai-tool` endpoints, with defaults derived from the hints, and enforced for every
surface that calls the tool.

| option | default | what it does |
|---|---|---|
| `idempotency` | `auto`: on when `readOnlyHint` and `idempotentHint` are both false | Deduplicates on an idempotency key: the `idempotency_key` argument if the schema declares one, else the tool-call id, else a hash of the arguments within `idempotencyWindow`. A repeat returns the recorded result and sets `CamelAiToolDuplicate`. Uses an idempotent repository (`idempotentRepository=#bean`, default in memory). |
| `approval` | `auto`: on when `destructiveHint=true`, off otherwise | A Simple predicate (`approval=${header.amount} > 500`), `always` or `never`. Over MCP, a call that needs approval returns `input_required` with a confirmation form (2026-07-28) or uses `elicitation/create` (2025-11-25). Otherwise it parks the call on an approval endpoint (`approvalEndpoint=direct:approve`) and returns "pending approval" with a handle the agent can poll. |
| `timeout` | 20 s (Camel's `tool-timeout`) | **Cancels** the call's route and releases the exchange. Camel leaks it. |
| `rateLimit` | none | Calls per period, per principal and per tool (the throttle EIP). |
| `circuitBreaker` | none | Per-tool breaker with a fallback text for the model. |
| `compensation` | none | `direct:` route registered before the tool's side effect. Runs if a later step of the same conversation or saga fails. Multi-tool rollback, offered by no competitor. |
| `audit` | on, with `camel.ai.audit.*` | Appends a record per call: time, principal, tool, argument digest, policy decision, approval, duplicate flag, result digest, outcome. The records are hash-chained, so tampering shows. Written to a file (JSON lines) or `sql:`, exported as OpenTelemetry-style span attributes. |
| `pin` | on | The tool's definition hash is part of the server's manifest. A route that changes a published tool's description or schema is rejected unless it bumps `version`, so tools can't change under a client. |

A **kill switch** (`cameleer tool disable NAME`, JMX/management, and a `camel.ai.disabled-tools`
property) withdraws a tool everywhere and sends `list_changed`.

### C. MCP server

Camel parity on names, an advantage on completeness.

- **Properties:** `camel.server.mcp-enabled`, `-transport` (`http`/`stdio`), `-tags` (patterns;
  nothing is exposed by default), `-path` (`/mcp`), `-tool-timeout`, `-resource-timeout`,
  `-server-name`, `-instructions`, `-session-keep-alive-interval` and `-session-idle-ttl` (legacy
  sessions). Also `cameleer run --mcp-stdio`.
- **Dual-era protocol:**
  - **2026-07-28:** `server/discover`; `_meta` versions and capabilities; `resultType`; MRTR with
    an AES-GCM-protected `requestState` bound to the principal, method, argument digest and
    expiry; `subscriptions/listen`; `ttlMs`/`cacheScope`; `Mcp-Method`/`Mcp-Name` checks; error
    codes -32020/-32021/-32022.
  - **2025-11-25 and 2025-06-18:** `initialize`, `Mcp-Session-Id`, the GET SSE stream with
    `Last-Event-ID`, `ping`, elicitation.
  - The era is chosen per request from the first message, as the spec defines.
- **Transports:** Streamable HTTP (JSON or SSE per request, `Origin` check, 202 for
  notifications, keep-alive comments) and stdio (newline-delimited, logs to stderr).
- **Features:**
  - tools, with structured output
  - resources, templates and subscriptions; resources work over stdio too, which Camel lacks
  - prompts
  - completion
  - pagination
  - progress notifications
  - `list_changed`
  - the tool annotations, honored as in B
- **Authentication:**
  - static API keys, JWT verified against a JWKS endpoint, or RFC 7662 introspection
  - RFC 9728 protected-resource metadata and `WWW-Authenticate` challenges, with the token's
    audience checked (RFC 8707)
  - Never passes a client's token downstream.
  - The principal reaches the route as `CamelMcpSecurityPrincipal*` (Camel parity).
- **Trace propagation:** `traceparent` in `_meta` is joined to the route's trace.

### D. Model tool calling and the agent step

- **One internal model:** `ToolCall{id, name, args}`, `ToolResult{id, content, isError}` and
  `ModelTurn`. Each provider converts it on the wire.
- **Tool calling in every model component:**
  - `openai`: Chat Completions and Responses, `tools`, `tool_choice`, strict mode, parallel calls,
    streaming tool calls.
  - `azure-openai`: the same shapes on Azure.
  - `aws-bedrock` `converse` and `converseStream`: `toolConfig`, `toolUse`, `toolResult`.
  - `google-vertexai`: `functionDeclarations` and `functionCall`, keeping the `thoughtSignature`.
  - Ollama, through `openai`'s compatible mode or `/api/chat`, with generated call ids.
  - A new `anthropic:` component for the Messages API: `tool_use` and `tool_result`.
  - Camel parity on `openai`: `tags`, `autoToolExecution` (default true), `maxToolIterations`
    (50), `maxAgenticTokens`, `hallucinatedToolNameStrategy`, `toolExecutionErrorStrategy`,
    `parallelToolExecution`, `conversationMemory`, `maxHistoryMessages`, and the `CamelOpenAI*`
    token and iteration headers.
  - `llm:` as an alias of `openai:` (Camel 4.23).
  - Advantages:
    - Streaming works together with the tool loop.
    - Bedrock and Vertex get the same loop.
- **The agent step** (Cameleer extension; Camel has no agent EIP):

  ```yaml
  - agent:
      model: openai:chat-completion?model=gpt-5.4-mini
      tools: orders            # ai-tool tags, plus mcp:<server> references
      maxTurns: 10
      maxTokens: 40000
      deadline: 60s
      memory: "#conversations" # an aggregation repository, keyed by CamelAiConversationId
      onApproval: direct:ask-a-person
  ```

  The loop:
  - Runs the tools of each turn in parallel on a bounded pool, through the reliability envelope.
  - Truncates large results and keeps the full text behind a handle.
  - Stops on the model's answer, the turn limit, the token budget, the deadline, a repeated
    failing call or a denied approval.
  - Always returns a result for every call id, which every provider requires.
  - Sets headers for turns, tokens, tool calls and the stop reason.
  - Pauses durably for approvals: the pending state is stored, and the loop resumes on the
    approval endpoint's reply.
- **Record and replay:**
  - `agent` and the model components can record model responses to a file and replay them
    (`replay=file:...`), so an agent route runs in the test suite without a model.
  - `cameleer test` replays recorded MCP and tool calls against routes with mocked endpoints and
    asserts the results, deduplication and compensation.
  - No competitor offers this.

### E. MCP client

Advantage: Camel has no MCP client component.

- **An `mcp:serverName` producer** with operations `listTools`, `callTool`, `readResource` and
  `getPrompt`.
- **A shared client registry** (`camel.ai.mcp-servers.NAME.*`): `transport`
  (`stdio`/`streamableHttp`), `command`/`args` or `url`, auth (API key header or the OAuth 2.1
  flow with PKCE, resource indicator and `iss` check), and `toolNames`.
- **Camel parity:** `mcpServer.<name>.*` options on `openai`/`llm`, resolved through the same
  registry.
- **Dual-era detection:** probe with `server/discover`, fall back to `initialize`, and cache the
  result per server.
- **Tool pinning on the client side:** the tool definitions are hashed on first use. A changed
  `tools/list` is refused, or needs approval, before its tools are offered to a model.
- **The agent step and the MCP server can use remote MCP tools**, so Cameleer can front
  third-party MCP servers with its reliability envelope: a gateway with policy, audit and
  deduplication.

### F. Retrieval

Camel parity, later phase.

- `pgvector` (on the embedded PostgreSQL client), `qdrant` and `opensearch` k-NN. `weaviate` and
  `pinecone` can follow.
- Each with Camel's action enums, the `Camel<X>*` headers, and the `<store>:embeddings` and
  `<store>:rag` transformers.
- The `pgvector` filter is parameterized only, closing the SQL injection risk in Camel's raw
  filter.

### Not in scope

- A hosted catalog of thousands of connectors.
- A managed per-user OAuth vault. A per-user credential broker for a handful of providers is a
  possible later phase.
- A2A: Camel's component is Preview. It can follow once A to E land.

## 4. Delivery plan

Five phases. Each phase ends with docs, examples that `cameleer validate` checks, hermetic tests,
zero warnings, and the full suite green on Windows and Linux.

| phase | contents | rough size |
|---|---|---|
| 1. Tool model and envelope | `cameleer-ai` crate; `ai-tool`, `ai-resource`, `ai-prompt`; registry; JSON Schema 2020-12 in `json-validator`; idempotency, approval, timeout with cancellation, rate limit, circuit breaker, audit, pinning, kill switch | large |
| 2. MCP server | dual-era protocol, Streamable HTTP and stdio, tools/resources/prompts/completion/subscriptions, MRTR approvals, auth (keys, JWT, introspection, PRM), `camel.server.mcp-*`, `--mcp-stdio`; gated conformance tier with the official suite and Inspector | large |
| 3. Model tool calling and the agent step | internal tool model; tools in `openai` (both APIs), `azure-openai`, `aws-bedrock`, `google-vertexai`, `anthropic` (new); `agent` step; record and replay; `cameleer test` | large |
| 4. MCP client | `mcp:` producer, client registry, OAuth client, dual-era detection, client-side pinning, MCP tools in `openai` and `agent` | medium |
| 5. Retrieval | `pgvector`, `qdrant`, `opensearch` k-NN, transformers | medium |

Phases 1 to 3 make a complete action layer for a release (0.5.0). Phases 4 and 5 can follow
(0.6.0).

**Website.** The pages already have slots for this. When phase 2 lands, "the action layer for AI
agents" becomes the tagline (`docs/website-plan.md`), and an `/use-cases/mcp-tool-server/` page
and a "Cameleer vs Camel MCP server" comparison are added.

## 5. Testing

- **Hermetic:**
  - embedded mock servers for OpenAI, Anthropic, Bedrock and Vertex (the existing mock-server
    pattern)
  - a scripted MCP client and server
  - recorded model transcripts
  - Each envelope feature gets a test that proves the guarantee: a retried call runs once; a
    denied approval runs nothing; a timeout cancels the route; a failed second step compensates
    the first; a changed tool is refused; the audit chain detects an edited record.
- **Gated interop tier** (`CAMEL_MCP_CONFORMANCE=1`, scripts pinned and checksum-verified like the
  simulator tier):
  - the official MCP conformance suite (`@modelcontextprotocol/conformance`, server and client,
    2026-07-28 and 2025-11-25)
  - MCP Inspector CLI smoke tests
  - the `server-everything` reference server as a peer for the client
- **Live tier** (needs the owner's API keys, never run by default): one real call per provider.

## 6. Decisions for the owner

1. **A new crate, `cameleer-ai`,** with an `ai` cargo feature that is on by default and in
   `full`. Recommended: the tool model, the MCP server and client and the agent step belong
   together. The model components stay where they are and depend on it.
2. **MCP versions:** 2026-07-28 plus 2025-11-25, with 2025-06-18 accepted as legacy. Recommended,
   as the spec defines.
3. **The `agent` step** as a Cameleer extension of the DSL, clearly marked as not in Apache Camel.
   Recommended. Camel users can still use the `openai`/`llm` producer loop, which we match.
4. **Release scope:** phases 1 to 3 as 0.5.0, phases 4 and 5 as 0.6.0. Recommended.

## Sources

- **Apache Camel:**
  - https://camel.apache.org/blog/2026/08/camel-ai-tools-mcp-422/
  - https://camel.apache.org/releases/release-4.23.0/
  - https://camel.apache.org/components/next/ai-tool-component.md
  - https://camel.apache.org/components/next/others/mcp-server.md
  - https://camel.apache.org/components/next/langchain4j-agent-component.html
  - https://camel.apache.org/components/next/openai-component.html
  - https://camel.apache.org/components/next/aws-bedrock-component.html
  - https://camel.apache.org/components/next/pgvector-component.html
  - https://github.com/apache/camel/pull/25309
- **MCP:**
  - https://modelcontextprotocol.io/specification/2026-07-28/changelog and the 2026-07-28
    specification pages (basic, versioning, transports, tools, MRTR, subscriptions, authorization)
  - https://modelcontextprotocol.io/specification/2025-11-25/changelog
  - https://modelcontextprotocol.io/docs/2026-07-28/tutorials/security/security_best_practices
  - https://github.com/modelcontextprotocol/conformance
  - https://github.com/modelcontextprotocol/inspector
- **Tool calling:**
  - https://developers.openai.com/api/docs/guides/function-calling
  - https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools
  - https://docs.ollama.com/capabilities/tool-calling
  - https://aistudio.google.com/docs/function-calling
  - https://www.anthropic.com/engineering/writing-tools-for-agents
  - https://github.com/open-telemetry/semantic-conventions-genai
- **Competitors:**
  - https://docs.mulesoft.com/general/agent-fabric-overview
  - https://docs.workato.com/mcp/verified-user-access
  - https://boomi.com/platform/agentstudio/
  - https://learn.microsoft.com/azure/api-management/mcp-server-overview
  - https://developer.konghq.com/plugins/ai-mcp-proxy/
  - https://agentgateway.dev/docs/local/latest/faqs/
  - https://www.arcade.dev/developers
  - https://docs.zapier.com/mcp/usage.md
  - https://developers.openai.com/api/docs/guides/tools-remote-mcp
  - https://openai.github.io/openai-agents-python/human_in_the_loop
  - https://temporal.io/blog/announcing-openai-agents-sdk-integration
  - https://urandom.io/blog/2026-03-03-agentic-ai-idempotency-retries-and-compensation
