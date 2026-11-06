# Anthropic component

> The `anthropic` component calls Claude models through the Anthropic Messages API, with tool calling: with `tags` it runs your `ai-tool` routes for the model, and it is a model for the `agent` step.

**Scheme:** `anthropic:` · **Crate:** `cameleer-saas` · **Cargo feature:** [`saas`](../guides/build-features.md) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** no direct equivalent (Camel reaches Claude through LangChain4j)

## When to use it

- Call Claude directly, without Bedrock or Vertex AI.
- Give Claude your routes as tools, with argument checks, approvals and an audit log
  ([ai-tool](ai-tool.md)).
- Use Claude as the model of an [agent step](../guides/agent-step.md).

## URI format

```text
anthropic:messages[?options]
```

## Example

<!-- example: anthropic-tools -->
```yaml
- route:
    from:
      uri: direct:ask
      steps:
        - to: "anthropic:messages?model=claude-sonnet-4-5&apiKey={{env:ANTHROPIC_API_KEY}}&systemMessage=You are a support agent.&tags=support"
```

A text body is the user's message and becomes the answer. With `tags`, the model can call the
`ai-tool` routes with those tags; the loop runs them and sends the results back until Claude
answers.

## Options

| option | default | description |
|---|---|---|
| `model` | required | The model, such as `claude-sonnet-4-5` (the `CamelAnthropicModel` header overrides it) |
| `apiKey` | `ANTHROPIC_API_KEY` env | Sent as `x-api-key` |
| `baseUrl` | `https://api.anthropic.com` | The API base URL (a gateway or proxy) |
| `anthropicVersion` | `2023-06-01` | The `anthropic-version` header |
| `systemMessage` | | The system prompt (the `CamelAnthropicSystemMessage` header overrides it) |
| `maxTokens` | `1024` | `max_tokens` (required by the API) |
| `temperature` | | Sampling temperature |
| `promptCaching` | `auto` | `auto` puts `cache_control` breakpoints on the last tool, the system prompt and the last message, so the next turn reads the stable prefix from Anthropic's cache (about a tenth of the input price); `off` sends the request unchanged |
| `timeout` | `120s` | HTTP timeout |
| `tags`, `autoToolExecution`, `maxToolIterations`, `maxAgenticTokens`, `parallelToolExecution`, `hallucinatedToolNameStrategy`, `record`, `replay` | | Tool calling, as on every model component ([the options](../guides/agent-step.md#tool-calling-in-the-model-components)) |
| `maxRetries`, `retryDelay`, `fallbackModels`, `responseCache`, `responseCacheTtl`, `responseCacheSize`, `coalesce` | | Retries, fallbacks, response cache and coalescing ([model providers](../guides/model-providers.md#reliability-and-cost)) |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAnthropicSystemMessage` | in | The system prompt |
| `CamelAnthropicModel` | in | The model for this call |
| `CamelAnthropicStopReason` | out | `end_turn`, or why the tool loop stopped |
| `CamelAnthropicInputTokens`, `CamelAnthropicOutputTokens` | out | Token usage (all turns of a tool loop) |
| `CamelAiAgentTurns`, `CamelAiAgentToolCalls`, `CamelAiAgentStopReason`, `CamelAiAgentTotalTokens` | out | The tool loop |
| `CamelAiRequestedToolCalls` | out | With `autoToolExecution=false`: the calls Claude asked for |
| `CamelAiCacheReadTokens`, `CamelAiCacheWriteTokens` | out | Input tokens read from and written to the prompt cache (when there were any) |

Bodies: a text body is one user message; a list body is the message list
(`{"role", "content"}`); a map body is sent as the whole request (`model` and `max_tokens` are
added when missing) and the whole response becomes the body.

## Common questions

### What happens on an API error?

The exchange fails with `HttpOperationFailedException` and the API's error message.

### Can I test routes that use it without calling Claude?

Yes. Record the responses once (`record=file:claude.jsonl`) and replay them (`replay=...`), or
write the responses by hand; `cameleer test` runs the route against them
([testing without a model](../guides/agent-step.md#testing-without-a-model)).
`cameleer_saas::testing::AnthropicMock` is a mock of the Messages API for Rust tests.

## Related

- [The agent step and tool calling](../guides/agent-step.md)
- [openai](openai.md), [aws-bedrock](aws-bedrock.md), [google-vertexai](google-vertexai.md)
- [ai-tool](ai-tool.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
