# The agent step and tool calling

> The `agent` step runs a model and its tools in a loop as one step of a route: the model asks for tools, Cameleer runs them through the `ai-tool` envelope (argument checks, deduplication, approvals, timeouts, audit), and the loop stops on the model's answer or a limit. The same loop runs inside every model component with `tags`. Recorded model responses let `cameleer test` check an agent route without a model.

**Cargo feature:** [`ai`](build-features.md) for the tools; the model components are in `saas` (`openai`, `llm`, `anthropic`) and `cloud` (`azure-openai`, `aws-bedrock`, `google-vertexai`) · **Apache Camel:** no agent EIP; the `tags` options mirror Camel 4.23's `openai`/`llm` component

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
| `model` | required | A model endpoint: `openai:` / `llm:`, `anthropic:`, `azure-openai:`, `aws-bedrock:` (Converse), `google-vertexai:` (Gemini) |
| `tools` | none | Comma-separated `ai-tool` tags; `*` offers every tool |
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
| `maxAgenticTokens` | none | Total tokens before the loop stops |
| `parallelToolExecution` | `true` | Run a turn's tools in parallel |
| `hallucinatedToolNameStrategy` | `report` | `report` tells the model the tool doesn't exist; `fail` fails the exchange |
| `record`, `replay` | | `file:PATH`, as on the agent step |

| component | wire format |
|---|---|
| `openai`, `llm` | Chat Completions `tools` and `tool_calls` (also Ollama, vLLM, LM Studio; missing call ids are generated) |
| `azure-openai` | the same, on Azure deployments |
| `aws-bedrock` (Converse) | `toolConfig`, `toolUse`, `toolResult` |
| `google-vertexai` (Gemini) | `functionDeclarations`, `functionCall` (its `thoughtSignature` is sent back), `functionResponse` |
| `anthropic` | Messages API `tools`, `tool_use`, `tool_result` |

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

- Streaming together with the tool loop: a model component with `tags` waits for each complete
  answer.
- The OpenAI Responses API and Ollama's native `/api/chat` (use the Chat Completions API,
  which Ollama also serves).
- Tools from MCP servers (`mcp:` references in `tools`), which need the MCP client.

## Related

- [ai-tool](../components/ai-tool.md): what a tool is and the envelope every call goes through
- [MCP server](mcp-server.md): the same tools for MCP clients
- [Cameleer for AI agents](agents.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
