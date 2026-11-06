# OpenAI component

> The `openai` component calls any OpenAI-compatible API (OpenAI, Ollama, vLLM, LM Studio, LiteLLM and other gateways) for chat completions, with tool calling, and embeddings.

**Scheme:** `openai:` · **Also:** `llm:` (Camel 4.23's name, same component) · **Crate:** `cameleer-saas` · **Cargo feature:** [`saas`](../guides/build-features.md) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** no direct equivalent; closest are [LangChain4j Chat](https://camel.apache.org/components/next/langchain4j-chat-component.html) and [LangChain4j Embeddings](https://camel.apache.org/components/next/langchain4j-embeddings-component.html)

## When to use it

- Classify, route or tag messages with an LLM (ticket priority, sentiment, language, intent).
- Summarise or rewrite text inside a route (incident reports, e-mails, logs) before sending it on.
- Create embeddings for semantic search or retrieval-augmented generation (RAG) and store them.
- Run against a local model (Ollama, vLLM) on an edge box or an air-gapped network with the same route.

## URI format

```text
openai:chat-completion[?options]
openai:embeddings[?options]
```

`chat`, `chat-completions` and `embedding` are accepted as aliases. Any other operation fails
when the endpoint is created.

## Example

Classify a support ticket. A text body is the user message; `systemMessage` is the system
prompt. The body becomes the assistant's answer; the token usage is in headers.

<!-- example: openai-classify -->
```yaml
- route:
    id: classify-ticket
    from:
      uri: "direct:classify"
      steps:
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&apiKey={{env:OPENAI_API_KEY}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&temperature=0&maxTokens=5&systemMessage=Classify the ticket as billing, bug or question. Reply with one word."
        - setHeader:
            name: category
            simple: "${body}"
        - log: "category=${header.category} tokens=${header.CamelOpenAITotalTokens}"
```

Create an embedding. A text body becomes one vector (a list of numbers); a list body becomes a
list of vectors, in the same order.

<!-- example: openai-embeddings -->
```yaml
- route:
    id: embed-text
    from:
      uri: "direct:embed"
      steps:
        - to: "openai:embeddings?model={{env:OPENAI_EMBEDDING_MODEL:text-embedding-3-small}}&apiKey={{env:OPENAI_API_KEY}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}"
        - log: "vector with ${body.size()} dimensions"
```

Use a local model with Ollama (or vLLM, LM Studio): only `baseUrl` and `model` change. Ollama
does not check the API key, so none is set. A list body is a conversation: maps with `role`
and `content`, plain strings become user messages.

<!-- example: openai-ollama-local -->
```yaml
- route:
    id: local-llm
    from:
      uri: "direct:ask-local"
      steps:
        - to: "openai:chat-completion?model={{env:OLLAMA_MODEL:llama3.2}}&baseUrl={{env:OLLAMA_URL:http://localhost:11434/v1}}&systemMessage=Answer briefly."
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:classify")
        .to("openai:chat-completion?model=gpt-4o-mini&temperature=0&systemMessage=Classify the ticket as billing, bug or question. Reply with one word.")
        .set_header("category", simple("${body}"));
})?;
```

## Options

| option | default | description |
|---|---|---|
| `model` | | Model name (`gpt-4o-mini`, `text-embedding-3-small`, `llama3.2`, ...). The `CamelOpenAIModel` header overrides it. |
| `apiKey` | `OPENAI_API_KEY` env | Sent as `Authorization: Bearer ...`. |
| `organization` | | Sent as the `OpenAI-Organization` header. |
| `baseUrl` | `https://api.openai.com/v1` | API base URL: any OpenAI-compatible server. |
| `systemMessage` | | Chat: system prompt (the `CamelOpenAISystemMessage` header overrides it). |
| `temperature` | | Chat: sampling temperature. |
| `maxTokens` | | Chat: `max_tokens`. |
| `topP` | | Chat: `top_p`. |
| `user` | | Chat: end-user id sent as `user`. |
| `outputType` | | `full` keeps the whole JSON response as the body instead of the answer text (or the vectors). |
| `tags`, `autoToolExecution`, `maxToolIterations`, `maxAgenticTokens`, `parallelToolExecution`, `hallucinatedToolNameStrategy`, `record`, `replay` | | Tool calling: offer the `ai-tool` routes with these tags and run the loop ([the options](../guides/agent-step.md#tool-calling-in-the-model-components)). |
| `timeout` | `120s` | HTTP timeout. |

Values in a request-map body win over the options (an option is only added when the key is
missing).

## Message headers

| header | direction | description |
|---|---|---|
| `CamelOpenAISystemMessage` | in | Chat: system prompt. |
| `CamelOpenAIModel` | in | Model for this call. |
| `CamelOpenAIResponseId` | out | Chat: response id. |
| `CamelOpenAIResponseModel` | out | Chat: model that answered. |
| `CamelOpenAIFinishReason` | out | Chat: `stop`, `length`, `tool_calls`, ... |
| `CamelOpenAIPromptTokens`, `CamelOpenAICompletionTokens`, `CamelOpenAITotalTokens` | out | Chat: token usage (all turns of a tool loop). |
| `CamelOpenAIToolIterations` | out | Tool loop: model calls made. |
| `CamelOpenAIToolStopReason` | out | Tool loop: `answer`, `max-turns`, `max-tokens`, ... |
| `CamelOpenAIToolCalls` | out | Tool loop: the calls made (`[{id, name, arguments, isError}]`); with `autoToolExecution=false`, the calls the model asked for. |

Chat bodies: a text body is one user message (after the system message); a list body is the
message list; a map body is sent as the whole request (`model` and the options are added when
missing), which lets you pass fields the component has no option for, such as
`response_format`, `seed` or `tools`.

## Run it with Docker

```sh
export OPENAI_API_KEY=sk-...
docker run --rm -e OPENAI_API_KEY -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/classify-ticket.camel.yaml
```

With Ollama on the host (Docker Desktop):

```sh
docker run --rm -e OLLAMA_URL=http://host.docker.internal:11434/v1 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/local-llm.camel.yaml
```

## Common questions

### Does it support streaming?

No. The component sends one request and waits for the complete answer (`timeout`, default 120
seconds). For streaming, use [azure-openai](azure-openai.md) (`streaming=true`),
[aws-bedrock](aws-bedrock.md) or [google-vertexai](google-vertexai.md).

### Does it support tool calls (function calling)?

Yes. `tags=support` offers the [ai-tool](ai-tool.md) routes tagged `support`; the component runs
the tools the model asks for (through the `ai-tool` envelope) and loops until the model answers
(`maxToolIterations`, default 50). `autoToolExecution=false` makes one call and returns the
requested calls in `CamelOpenAIToolCalls`. The endpoint is also a model for the
[agent step](../guides/agent-step.md), which adds memory and approvals. Missing call ids (some
Ollama versions) are generated.

### How do I get JSON output?

Put `"response_format": {"type": "json_object"}` in a request-map body (or ask for JSON in the
system message) and `unmarshal: json` the answer.

### What happens on an API error?

A non-2xx response fails with `HttpOperationFailedException`; the message contains the status
and the API's `error.message` (for example `Incorrect API key provided`). Rate limits (429) can
be retried with `onException` and a redelivery policy.

## Related

- [AI components guide](../guides/ai-components.md)
- [azure-openai](azure-openai.md), [aws-bedrock](aws-bedrock.md), [google-vertexai](google-vertexai.md)
- [slack](slack.md), [jira](jira.md), [telegram](telegram.md) for agents that post their results
- [Apache Camel LangChain4j Chat](https://camel.apache.org/components/next/langchain4j-chat-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
