# The agent step and tool calling

> The `agent` step runs a model and its tools in a loop as one step of a route: the model asks for tools, Cameleer runs them through the `ai-tool` envelope (argument checks, deduplication, approvals, timeouts, audit), and the loop stops on the model's answer or a limit. The same loop runs inside every model component with `tags`. Recorded model responses let `cameleer test` check an agent route without a model.

**Cargo feature:** [`ai`](build-features.md) for the tools; the model components are in `saas` (`openai`, `llm`, `anthropic`, `gemini`) and `cloud` (`azure-openai`, `aws-bedrock`, `google-vertexai`) · **Apache Camel:** no agent EIP; the `tags` options mirror Camel 4.23's `openai`/`llm` component

## The agent step

<!-- example: agent-step-support -->
```yaml
- route:
    from:
      uri: direct:support
      steps:
        - agent:
            model: "openai:chat-completion?model=gpt-5.4-mini"
            tools: support                 # ai-tool routes with this tag
            systemMessage: You are a support agent. Look orders up before you refund them.
            maxTurns: 8
            maxTokens: 40000
            deadline: 60s
            memory: memory                 # keep conversations (CamelAiConversationId)
            onApproval: direct:approvals   # told when a tool call waits for approval
```

The body is the user's message: text, or a list of `{"role", "content"}` messages. After the
step the body is the model's answer.

| option | default | description |
|---|---|---|
| `model` | required | A model endpoint: `openai:` / `llm:` (`chat-completion`, `responses`, `apiFormat=ollama`, or any [`provider`](model-providers.md) preset), `anthropic:`, `gemini:`, `azure-openai:`, `aws-bedrock:` (Converse), `google-vertexai:` (Gemini on Vertex AI) |
| `tools` | none | Comma-separated `ai-tool` tags; `*` offers every tool; `mcp:SERVER` offers the tools imported from an MCP server ([MCP client](mcp-client.md)) |
| `systemMessage` | the model endpoint's | The system prompt |
| `maxTurns` | `10` | Model calls before the loop stops |
| `maxTokens` | none | Total tokens (input and output) before the loop stops |
| `deadline` | none | How long the loop may run |
| `memory` | none | `memory` (in memory) or `#bean` (an `AggregationRepository`): keeps each conversation by `CamelAiConversationId` |
| `maxHistoryMessages` | `100` | History sent to the model; older messages are dropped, never between a tool call and its result |
| `onApproval` | none | An endpoint told when a tool call waits for approval |
| `parallelToolCalls` | `4` | Tools of one turn run in parallel on at most this many threads |
| `maxResultChars` | `20000` | Longer tool results are cut, with a note to the model |
| `failOnUnknownTool` | `false` | Fail the exchange when the model names a tool that doesn't exist, instead of telling it |
| `record` | | `file:PATH`: append every model response to a recording |
| `replay` | | `file:PATH`: answer from a recording instead of calling the model |

In the Rust DSL:

```rust
use cameleer_core::processor::agent::AgentDefinition;
b.from("direct:support").agent(
    AgentDefinition::new("openai:chat-completion?model=gpt-5.4-mini").tools("support").memory("memory").max_turns(8),
);
```

### What the loop guarantees

- Every tool call goes through the `ai-tool` envelope: the arguments are validated, a retried
  call with the same call id runs once, destructive calls wait for approval, slow calls are
  cancelled, and each call is audited.
- Every tool call gets a result in the conversation, including calls to tools that don't exist
  and calls that failed. Every provider requires one result per call id.
- The loop stops on the model's answer, `maxTurns`, `maxTokens`, `deadline`, the same call
  failing three times, or a rejected approval.

| header | description |
|---|---|
| `CamelAiConversationId` | in: the conversation, for `memory`, approvals and compensation |
| `CamelAiAgentStopReason` | `answer`, `max-turns`, `max-tokens`, `deadline`, `repeated-failure`, `approval-required`, `approval-pending`, `approval-denied` |
| `CamelAiAgentTurns`, `CamelAiAgentToolCalls` | Model calls and tool calls made |
| `CamelAiAgentInputTokens`, `CamelAiAgentOutputTokens`, `CamelAiAgentTotalTokens` | Token usage across turns |
| `CamelAiApprovalHandle` | The handle of a tool call that waits for approval |

### Approvals pause the conversation

When a tool call needs approval (a `destructiveHint` tool, or an `approval` rule on the
`ai-tool`), the agent stops with `approval-required`. `onApproval` receives the handle
(`CamelAiApprovalHandle`), the tool's name and its arguments. With `memory`, the conversation
waits: calling the agent again answers `approval-pending` until a person decides. Once they
do, the next call resumes the conversation with the tool's result.

A person approves or rejects with an `ai-tool` producer:

<!-- example: agent-step-approve -->
```yaml
- route:
    from:
      uri: platform-http:/approvals/approve
      steps:
        - setHeader:
            name: CamelAiToolApprovalHandle
            simple: "${header.handle}"
        - setHeader:
            name: CamelAiToolApprovedBy
            simple: "${header.user}"
        - to: ai-tool:refund?operation=approve    # operation=deny rejects (the body is the reason)
```

## Tool calling in the model components

Every model component takes the same tool options, Camel's names on `openai`/`llm`. With
`tags` it offers those tools and runs the loop itself. The body is the answer, as with the
agent step, but nothing is remembered between calls.

<!-- example: agent-step-tags -->
```yaml
- route:
    from:
      uri: direct:ask
      steps:
        - to: "aws-bedrock://claude?modelId=anthropic.claude-sonnet-4-5-v1:0&operation=converse&tags=support&maxToolIterations=10"
```

| option | default | description |
|---|---|---|
| `tags` | none | `ai-tool` tags to offer |
| `autoToolExecution` | `true` | Run the tools and loop; `false` makes one call and returns the requested calls (`CamelOpenAIToolCalls` on `openai`, `CamelAiRequestedToolCalls` elsewhere) |
| `maxToolIterations` | `50` | Model calls in the loop |
| `maxAgenticTokens` | `0` (unlimited) | Total tokens before the loop stops |
| `parallelToolExecution` | `false` | Run a turn's tools in parallel (on at most 4 threads); by default they run one after another, as in Camel |
| `hallucinatedToolNameStrategy` | `repromptModel` | `repromptModel` tells the model the tool doesn't exist and lets it answer; `failExchange` fails the exchange (Camel's names; `report` and `fail` also work) |
| `record`, `replay` | | `file:PATH`, as on the agent step |
| `maxRetries`, `retryDelay`, `fallbackModels`, `responseCache`, `responseCacheTtl`, `responseCacheSize`, `coalesce` | | Retries, fallback models, the response cache and coalescing ([model providers](model-providers.md#reliability-and-cost)) |

| component | wire format |
|---|---|
| `openai`, `llm` | Chat Completions `tools` and `tool_calls` (also every [`provider`](model-providers.md) preset, Ollama, vLLM, LM Studio; missing call ids are generated; DeepSeek, Qwen and Kimi `reasoning_content` is sent back with the calls); with `streaming=true` every turn is streamed, tool call deltas are joined by `index` and text deltas go to `streamingChunkProcessor` |
| `openai:responses`, `llm:responses` | Responses API `tools`, `function_call` and `function_call_output` items matched by `call_id`, `instructions` |
| `openai`/`llm` with `apiFormat=ollama` | Ollama's `/api/chat`: `tool_calls` with object arguments, `tool` messages with `tool_name`, generated call ids |
| `azure-openai` | the same as `openai`, on Azure deployments |
| `aws-bedrock` (Converse) | `toolConfig`, `toolUse`, `toolResult`; `cachePoint`s with `promptCaching=true` |
| `google-vertexai`, `gemini` | `functionDeclarations`, `functionCall` (its `thoughtSignature` is sent back), `functionResponse` |
| `anthropic` | Messages API `tools`, `tool_use`, `tool_result`; `cache_control` breakpoints (`promptCaching=auto`) |

## Testing without a model

Record a conversation once (`record: file:support.jsonl`), or write the responses by hand: one
JSON line per model response. Then replay it in a test with `cameleer test`:

<!-- not-camel -->
```yaml
# support-agent.test.yaml
- test:
    name: a refund waits for approval, runs once, and the agent answers
    routes: [support-agent.camel.yaml]
    properties:
      agent.replay: file:refund.jsonl     # the route has  replay: "{{agent.replay:}}"
      payments.endpoint: mock:payments
    steps:
      - send:
          to: direct:support
          body: My order A1 arrived broken. Please refund it.
          headers: {CamelAiConversationId: case-1}
        expect:
          headers: {CamelAiAgentStopReason: approval-required}
          mocks: {"mock:payments": 0}
      - approve: {tool: refund, by: alice}
        expect: {mocks: {"mock:payments": 1}}
      - send:
          to: direct:support
          body: continue
          headers: {CamelAiConversationId: case-1}
        expect:
          body: Order A1 arrived damaged, so I refunded it in full.
      - callTool: {name: refund, arguments: {orderId: B7}, callId: retry-1, approvedBy: alice}
      - callTool: {name: refund, arguments: {orderId: B7}, callId: retry-1, approvedBy: alice}
        expect: {duplicate: true, mocks: {"mock:payments": 2}}
```

```bash
cameleer test support-agent.test.yaml
```

Steps are `send` (`to`, `body`, `headers`), `callTool` (`name`, `arguments`, `callId`,
`conversation`, `principal`, `approvedBy`), `approve` / `deny` (`tool`, `by`, `reason`) and
`compensate` (`conversation`). `expect` checks `body`, `bodyContains`, `headers`, `error`,
`status`, `isError`, `duplicate` and `mocks` (messages per mock endpoint). Test files end in
`.test.yaml`, so `cameleer run` and `cameleer validate` skip them. `file:` paths are relative
to the test file. The command exits 1 when a test fails. The complete example is
[examples/agents/support-agent.camel.yaml](../../examples/agents/support-agent.camel.yaml)
with [its test](../../examples/agents/tests/support-agent.test.yaml).

## Not supported yet

- Streaming with Ollama's native API (`apiFormat=ollama`); stream from Ollama through its
  OpenAI-compatible `/v1` API (the default format) instead.
- Streaming in the tool loop of `azure-openai`, `aws-bedrock`, `google-vertexai`, `gemini` and
  `anthropic`: with `tags` they wait for each complete answer. `openai`/`llm` stream every
  turn with `streaming=true`.
- Server-side conversation state of the Responses API (`previous_response_id`): Cameleer
  sends the whole conversation each turn.

## Related

- [ai-tool](../components/ai-tool.md): what a tool is and the envelope every call goes through
- [MCP server](mcp-server.md): the same tools for MCP clients
- [MCP client](mcp-client.md): tools of other MCP servers in the loop
- [Model providers and call optimizations](model-providers.md)
- [Cameleer for AI agents](agents.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/cameleering/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
