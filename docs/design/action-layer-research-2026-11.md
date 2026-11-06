# Action layer research, November 2026

Internal working notes (not published; `docs/design/` is excluded from the site). Three studies done on
2026-11-06 for the action layer: what MCP products offer, Camel 4.22/4.23 parity, and LLM
providers plus the optimizations gateways offer. Sources are the URLs in each section; vendor
numbers are marked as such.

## 1. MCP market: what others offer

Products studied: Apache Camel 4.22/4.23, MuleSoft (MCP Connector, Flex Gateway), Boomi, Workato
(Enterprise MCP, Verified User Access), Zapier MCP, n8n, Pipedream, Composio, Arcade.dev, Kong
(AI MCP Proxy, AI MCP OAuth2), Azure API Management, AWS Bedrock AgentCore Gateway, Cloudflare
(Agents SDK, MCP portals), IBM ContextForge, Docker MCP Gateway, Microsoft MCP Gateway,
agentgateway, LiteLLM, Spring AI 2, Quarkus MCP Server, FastMCP 3.

Gaps where at least two serious competitors offer something, in priority order:

1. **MCP client, gateway and aggregation with namespacing** (AgentCore, Kong, ContextForge,
   agentgateway, Cloudflare portals, Docker, FastMCP, Workato, LiteLLM, Microsoft). Done in
   `cameleer_ai::mcp::client`: imported tools go through the envelope, are prefixed per server and
   re-exposed by Cameleer's MCP server; rug-pull protection through pinning.
2. **Per-user backend credentials and OAuth token exchange (RFC 8693)** (AgentCore 3LO, Workato
   VUA, Arcade, Composio, Pipedream, Kong 3.14, agentgateway). Not built: every tool runs as the
   route's identity. The main enterprise objection.
3. **OpenAPI to MCP tools** (AgentCore, Kong `deck file openapi2mcp`, APIM, ContextForge,
   agentgateway, FastMCP).
4. **Tasks extension, progress and logging** (AgentCore, Quarkus, FastMCP). Progress and logging
   done; tasks not built.
5. **Declarative per-tool policy on claims and arguments** (AgentCore Cedar, agentgateway CEL,
   Kong ACLs, MuleSoft ABAC, Workato RBAC, LiteLLM).
6. **OpenTelemetry spans and metrics per tool call** (AgentCore, ContextForge, MuleSoft, APIM,
   Quarkus).
7. **Content guardrails on arguments and results**: PII and secret redaction, deny lists
   (ContextForge, Docker, Cloudflare DLP, LiteLLM).
8. **Elicitation and sampling from tools** (AgentCore, Quarkus, Cloudflare, MuleSoft, FastMCP).
   Done. 2026-07-28 deprecates sampling, roots and logging with a 12-month window.
9. **Tool search to cut context** (AgentCore semantic search, Cloudflare Code Mode, Zapier,
   Composio).
10. **Tool aliasing and description override** (Cloudflare, MuleSoft Tool Mapping, FastMCP).
11. **Quotas and response caching** (APIM, Workato, agentgateway, FastMCP, ContextForge).
12. **Registry and catalog, admin UI** (Workato, Boomi, APIM, Docker, ContextForge).
13. **CIMD/DCR as an OAuth client** for protected upstream servers (FastMCP, Cloudflare).

Where Cameleer is ahead (no competitor found offering it): exactly-once tool calls for retries,
saga-style compensation per conversation, a hash-chained tamper-evident audit log, definition
pinning, timeouts that cancel the work (Camel documents that a timed-out route keeps running),
four protocol revisions on one endpoint (only AgentCore matches), ~230 integration components in
one self-hosted binary, record/replay and `cameleer test`.

Published footprints: agentgateway (Rust) 30 MB and p99 under 0.2 ms at 30k QPS (vendor);
ContextForge 23 ms median added latency (AIMultiple); Docker MCP Gateway 1.1 ms; Bifrost 0.84 ms;
Quarkus native about 49 ms start and 33–36 MB RSS. AIMultiple (2026-10) found most gateways
either did not block prompt injection in tool results or did not record blocked calls.

## 2. Camel 4.22/4.23 parity

Camel pages: ai-tool, ai-resource (4.23), others/mcp-server, openai (llm since 4.23),
others/openai-mcp, langchain4j-agent and its guardrails, a2a.

Differences found and what to do:

- `hallucinatedToolNameStrategy`: Camel's values are `failExchange` (default) and
  `repromptModel`; Cameleer accepted only `report`/`fail`. Accept Camel's values.
- `toolExecutionErrorStrategy` (`failExchange` default, `repromptModel`): missing.
- `maxAgenticTokens=0` means unlimited in Camel; Cameleer stopped before the first call.
- `parallelToolExecution` defaults to false in Camel.
- `returnDirect`: the loop must return the tool result without another model call
  (`CamelOpenAIMcpReturnDirect`).
- Token headers: Camel's `CamelOpenAIPromptTokens` etc. are the last call;
  `CamelOpenAIAgentic*Tokens` are cumulative; `CamelOpenAIMcpToolCalls` lists tool names.
- MCP `-tags`: `*` never exposes untagged items in Camel.
- MCP server metadata: `-server-title`, `-server-description`, `-server-website-url`,
  `-server-icons`; defaults: keep-alive 30 s, idle TTL 5 min (0 disables), server name = context
  name. Headers `CamelMcpSecurityPrincipalName`, `CamelMcpSecurityPrincipalRoles`.
- Errors: Camel sends MCP clients a generic message and logs the cause.
- openai options still missing: `userMessage`, `developerMessage`, `jsonSchema`, `streaming`,
  `stripThinking`, `additionalBodyProperty.*`, `additionalHeader.*`, `maxRetries` (2),
  `connectTimeout`/`requestTimeout`, embeddings options and similarity score,
  `conversationMemory`, `toolSearch*`, `parallelToolTimeout`, error properties
  `CamelAiErrorCategory`/`CamelAiRetryAfterMillis`, the other operations (moderation, images,
  audio, batch, webhook) and `a2a`.
- ai-tool `description` defaults to the tool name; ai-resource's documented option is
  `resourceUri`.

## 3. Model providers and call optimizations

Providers with an OpenAI-compatible Chat Completions endpoint (preset candidates): OpenAI,
Azure (v1), Gemini (AI Studio, beta), Bedrock (runtime `/openai/v1`), Mistral, DeepSeek, xAI,
Groq, Cerebras, Together, Fireworks, OpenRouter, Perplexity, Hugging Face, DashScope (Qwen),
Moonshot (Kimi), Cohere (compatibility API), Ollama, vLLM, LM Studio, llama.cpp. Quirks a preset
needs: `max_completion_tokens` for o-series and GPT-5; Mistral tool-call ids of exactly 9
alphanumerics; DeepSeek, Qwen and Kimi need `reasoning_content` sent back during tool loops;
Groq, Cerebras and Fireworks reject unknown fields. Native wire formats stay better for
Anthropic (caching, thinking), Bedrock Converse (cachePoint, guardrails) and Gemini (thought
signatures, explicit caches).

What gateways do beyond forwarding: provider prompt caching (Anthropic `cache_control`, reads at
0.1x; OpenAI automatic prefix caching, hit rate depends on byte-stable prefixes; Gemini implicit
caching 90% off; Bedrock `cachePoint`), exact and semantic response caching (Cloudflare,
Portkey, Helicone, Kong, APIM; Cloudflare does not coalesce concurrent identical requests),
retries and fallbacks (LiteLLM, APIM, OpenRouter), token budgets (APIM, Kong), batch APIs (50%
off), connection reuse. Vendor gateway overheads: Bifrost 11–59 µs, TensorZero under 1 ms,
Portkey 20–40 ms, Helicone 1–5 ms, plus a network hop for hosted gateways.

Cameleer's advantages as an embedded runtime: no extra hop; prompts assembled by Cameleer, so
the cached prefix is byte-stable by construction; in-process response cache and single-flight
coalescing; retries inside the envelope, so a retried model turn never repeats a side effect;
record/replay doubles as a deterministic cache for CI.
