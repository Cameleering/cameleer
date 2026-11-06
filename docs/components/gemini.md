# Gemini component

> The `gemini` component calls Google's Gemini models through the Gemini API (Google AI Studio, API keys), with tool calling: with `tags` it runs your `ai-tool` routes for the model, and it is a model for the `agent` step.

**Scheme:** `gemini:` · **Crate:** `cameleer-saas` · **Cargo feature:** [`saas`](../guides/build-features.md) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** no direct equivalent (Camel reaches Gemini through LangChain4j or `google-vertexai`)

## When to use it

- Call Gemini with an API key, without a Google Cloud project or service account.
- Give Gemini your routes as tools, with argument checks, approvals and an audit log
  ([ai-tool](ai-tool.md)).
- Use Gemini as the model of an [agent step](../guides/agent-step.md).

For Vertex AI (service accounts, regions, VPC) use [google-vertexai](google-vertexai.md). For
Gemini's OpenAI-compatible API use `openai:chat-completion?provider=gemini`; this component keeps
what that API drops, such as thought signatures.

## URI format

```text
gemini:generate[?options]
```

## Example

<!-- example: gemini-tools -->
```yaml
- route:
    from:
      uri: direct:ask
      steps:
        - to: "gemini:generate?model=gemini-2.5-flash&apiKey={{env:GEMINI_API_KEY}}&systemMessage=You are a support agent.&tags=support"
```

A text body is the user's message and becomes the answer. With `tags`, the model can call the
`ai-tool` routes with those tags; each `functionCall` runs through the `ai-tool` envelope and its
`thoughtSignature` is sent back with it, as Gemini requires.

## Options

| option | default | description |
|---|---|---|
| `model` | required | The model, such as `gemini-2.5-flash` (the `CamelGeminiModel` header overrides it) |
| `apiKey` | `GEMINI_API_KEY`, then `GOOGLE_API_KEY` env | Sent as `x-goog-api-key` |
| `baseUrl` | `https://generativelanguage.googleapis.com` | The API base URL (a gateway or proxy) |
| `systemMessage` | | The system instruction (the `CamelGeminiSystemMessage` header overrides it) |
| `maxTokens` | | `generationConfig.maxOutputTokens` |
| `temperature` | | `generationConfig.temperature` |
| `timeout` | `120s` | HTTP timeout |
| `tags`, `autoToolExecution`, `maxToolIterations`, `maxAgenticTokens`, `parallelToolExecution`, `hallucinatedToolNameStrategy`, `record`, `replay` | | Tool calling, as on every model component ([the options](../guides/agent-step.md#tool-calling-in-the-model-components)) |
| `maxRetries`, `retryDelay`, `fallbackModels`, `responseCache`, `responseCacheTtl`, `responseCacheSize`, `coalesce` | | Retries, fallbacks, response cache and coalescing ([model providers](../guides/model-providers.md#reliability-and-cost)) |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGeminiSystemMessage` | in | The system instruction |
| `CamelGeminiModel` | in | The model for this call |
| `CamelGeminiFinishReason` | out | Map bodies: the first candidate's `finishReason` |
| `CamelGeminiPromptTokenCount`, `CamelGeminiCandidatesTokenCount` | out | Token usage (all turns of a tool loop) |
| `CamelAiAgentTurns`, `CamelAiAgentToolCalls`, `CamelAiAgentStopReason`, `CamelAiAgentTotalTokens` | out | The tool loop |
| `CamelAiCacheReadTokens` | out | Input tokens served from Gemini's implicit cache (when there were any) |
| `CamelAiRequestedToolCalls` | out | With `autoToolExecution=false`: the calls Gemini asked for |

Bodies: a text body is one user message; a list body is the message list
(`{"role", "content"}`); a map body is sent as the whole `generateContent` request and the
whole response becomes the body.

## Common questions

### What happens on an API error?

The exchange fails with `HttpOperationFailedException` and the API's error message (for example
`API key not valid`). 429 and 5xx answers are retried first (`maxRetries`).

### Can I test routes that use it without calling Gemini?

Yes. Record the responses once (`record=file:gemini.jsonl`) and replay them (`replay=...`);
`cameleer test` runs the route against them
([testing without a model](../guides/agent-step.md#testing-without-a-model)).
`cameleer_saas::testing::GeminiMock` is a mock of the API for Rust tests: it scripts tool calls
and rejects a `functionCall` sent back without its thought signature.

## Related

- [Model providers and call optimizations](../guides/model-providers.md)
- [The agent step and tool calling](../guides/agent-step.md)
- [google-vertexai](google-vertexai.md), [anthropic](anthropic.md), [openai](openai.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
