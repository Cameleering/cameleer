# OpenAI component

> The `openai` component calls any OpenAI-compatible API (OpenAI, Ollama, vLLM, LM Studio, LiteLLM and other gateways) for chat completions and OpenAI's Responses API, with tool calling and streaming, Ollama's native API, and embeddings.

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
openai:responses[?options]
openai:embeddings[?options]
```

`chat`, `chat-completions`, `response` and `embedding` are accepted as aliases. Any other
operation fails when the endpoint is created. `chat-completion` uses the Chat Completions API
(`POST /chat/completions`), or Ollama's native API with `apiFormat=ollama`; `responses` uses
the [Responses API](#the-responses-api) (`POST /responses`).

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

## Streaming

`streaming=true` (or the `CamelOpenAIStreaming` header for one call) sends the request with
`"stream": true` and `stream_options.include_usage`, and decodes the event stream as it
arrives: text deltas are appended, tool call deltas are put together by their `index` (id and
name first, the arguments in pieces). Each text delta goes to the processor named by
`streamingChunkProcessor=#bean` (the body is the delta, `CamelOpenAIStreamingChunkIndex` its
index), so a client can see the answer while it is written. The body after the step is the
whole answer, as without streaming, and `CamelOpenAIStreamingChunkCount` tells how many deltas
came.

Streaming works with `tags`: each turn of the tool loop is streamed, the tools run between the
turns, and the deltas of every turn go to the same processor (indexes keep counting), so the
final answer streams once the tools are done. It also works for the agent step and the
Responses API (`response.output_text.delta` events).

<!-- example: openai-streaming -->
```yaml
- route:
    id: stream-answer
    from:
      uri: "direct:ask-stream"
      steps:
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&apiKey={{env:OPENAI_API_KEY}}&streaming=true&streamingChunkProcessor=#sendDelta&tags=orders"
        - log: "answer=${body} deltas=${header.CamelOpenAIStreamingChunkCount}"
```

`sendDelta` is any `Processor` bean, for example one that writes each delta to a server-sent
events connection. Without `streamingChunkProcessor` the call is still streamed (useful
behind gateways that time out long requests) and the deltas are only counted. A named bean
that does not exist fails the exchange when the first delta arrives. A server that answers
a streamed request with plain JSON is accepted; its whole answer is one delta.

## The Responses API

`openai:responses` talks to OpenAI's Responses API: the system message becomes
`instructions`, the conversation becomes `input` items (messages, `function_call` and
`function_call_output` items matched by `call_id`), tools are sent as
`{type: function, name, description, parameters}` and `maxTokens` as `max_output_tokens`. The
answer is the `output_text` of the `message` items; `CamelOpenAIFinishReason` is the response
`status` (`completed`, or `incomplete: max_output_tokens`), and the token headers come from
`usage.input_tokens` and `usage.output_tokens`. Every option of `chat-completion` works the
same way, including `tags`, `autoToolExecution=false`, `streaming`, `record`/`replay` and the
agent step. A map body is sent as the whole `/responses` request.

<!-- example: openai-responses -->
```yaml
- route:
    id: responses-api
    from:
      uri: "direct:ask-responses"
      steps:
        - to: "openai:responses?model={{env:OPENAI_MODEL:gpt-4o-mini}}&apiKey={{env:OPENAI_API_KEY}}&systemMessage=You are a support agent.&tags=orders&maxTokens=500"
```

Cameleer keeps the conversation itself and sends it in full each turn; it does not use
`previous_response_id` or stored responses.

## Ollama's native API

`apiFormat=ollama` on `chat-completion` calls Ollama's own `POST {baseUrl}/api/chat` instead of
its OpenAI-compatible `/v1` API. Set `baseUrl` to the server root (`http://localhost:11434`;
a trailing `/v1` is dropped). Requests go with `"stream": false`, tool call arguments are JSON
objects, tool results are `tool` messages with `tool_name`, and `temperature`, `maxTokens` and
`topP` go in `options` (`temperature`, `num_predict`, `top_p`). Ollama gives no call ids, so
they are generated (`call_1`, `call_2`, ...); `CamelOpenAIFinishReason` is `done_reason` and the
token headers come from `prompt_eval_count` and `eval_count`. No API key is needed.

<!-- example: openai-ollama-native -->
```yaml
- route:
    id: ollama-native
    from:
      uri: "direct:ask-ollama"
      steps:
        - to: "openai:chat-completion?model={{env:OLLAMA_MODEL:llama3.2}}&apiFormat=ollama&baseUrl={{env:OLLAMA_HOST:http://localhost:11434}}&tags=orders"
```

`streaming=true` with `apiFormat=ollama` is refused when the endpoint is created; to stream
from Ollama, use the default format with `baseUrl=http://localhost:11434/v1`. `apiFormat`
applies to `chat-completion` only.

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
| `provider` | `openai` | A preset for an OpenAI-compatible provider: `gemini`, `mistral`, `deepseek`, `xai`, `groq`, `cerebras`, `together`, `fireworks`, `openrouter`, `perplexity`, `huggingface`, `dashscope`, `moonshot`, `cohere`, `nvidia`, `ollama`, `vllm`, `lmstudio`, `llamacpp`. It sets the default `baseUrl` and the environment variable the key is read from ([the table](../guides/model-providers.md#provider-presets)). |
| `apiKey` | the preset's env variable (`OPENAI_API_KEY`) | Sent as `Authorization: Bearer ...`. |
| `organization` | | Sent as the `OpenAI-Organization` header. |
| `baseUrl` | the preset's (`https://api.openai.com/v1`) | API base URL: any OpenAI-compatible server (for `apiFormat=ollama`, the Ollama server root). |
| `apiFormat` | `openai` | `chat-completion` only: `ollama` calls Ollama's native `/api/chat` ([details](#ollamas-native-api)). |
| `streaming` | `false` | Chat and Responses: stream the answer ([details](#streaming)); the `CamelOpenAIStreaming` header overrides it. |
| `streamingChunkProcessor` | | `#bean`: a `Processor` that receives each streamed text delta as its body. |
| `systemMessage` | | Chat: system prompt (the `CamelOpenAISystemMessage` header overrides it; `instructions` for the Responses API). |
| `temperature` | | Chat: sampling temperature. |
| `maxTokens` | | Chat: `max_tokens` (`max_completion_tokens` for OpenAI's o-series and GPT-5 models, `max_output_tokens` for the Responses API, `options.num_predict` for Ollama). |
| `topP` | | Chat: `top_p`. |
| `user` | | Chat: end-user id sent as `user`. |
| `outputType` | | `full` keeps the whole JSON response as the body instead of the answer text (or the vectors). |
| `tags`, `autoToolExecution`, `maxToolIterations`, `maxAgenticTokens`, `parallelToolExecution`, `hallucinatedToolNameStrategy`, `record`, `replay` | | Tool calling: offer the `ai-tool` routes with these tags and run the loop ([the options](../guides/agent-step.md#tool-calling-in-the-model-components)). |
| `maxRetries` | `2` | Retries of 429, 5xx, timeouts and resets, with exponential backoff from `retryDelay` (`500ms`). |
| `fallbackModels`, `responseCache`, `responseCacheTtl`, `responseCacheSize`, `coalesce` | | Fallback models, the exact response cache and coalescing of identical requests ([details](../guides/model-providers.md#reliability-and-cost)). |
| `timeout` | `120s` | HTTP timeout. |

Values in a request-map body win over the options (an option is only added when the key is
missing).

## Message headers

| header | direction | description |
|---|---|---|
| `CamelOpenAISystemMessage` | in | Chat: system prompt. |
| `CamelOpenAIModel` | in | Model for this call. |
| `CamelOpenAIStreaming` | in | Stream this call (`true` / `false`), overriding `streaming`. |
| `CamelOpenAIStreamingChunkIndex` | chunk | On each exchange given to `streamingChunkProcessor`: the delta's index, counted across the turns of a tool loop (`CamelAiConversationId` is copied too). |
| `CamelOpenAIStreamingChunkCount` | out | Streaming: the number of text deltas. |
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
docker run --rm -e OPENAI_API_KEY -v "$PWD:/routes:ro" cameleering/cameleer run /routes/classify-ticket.camel.yaml
```

With Ollama on the host (Docker Desktop):

```sh
docker run --rm -e OLLAMA_URL=http://host.docker.internal:11434/v1 -v "$PWD:/routes:ro" cameleering/cameleer run /routes/local-llm.camel.yaml
```

## Common questions

### Does it support streaming?

Yes, with `streaming=true`, also together with tool calling: see [Streaming](#streaming). The
deltas go to a `streamingChunkProcessor` bean; the body is still the whole answer. `timeout`
(default 120 seconds) is the time allowed between two reads of the stream.

### Should I use Chat Completions or the Responses API?

Either works with every option here. Chat Completions (`chat-completion`) is served by almost
every OpenAI-compatible server (Ollama, vLLM, LM Studio, gateways); the Responses API
(`responses`) is OpenAI's newer API and what its newer models are documented with.

### Does it support tool calls (function calling)?

Yes. `tags=support` offers the [ai-tool](ai-tool.md) routes tagged `support`; the component runs
the tools the model asks for (through the `ai-tool` envelope) and loops until the model answers
(`maxToolIterations`, default 50). `autoToolExecution=false` makes one call and returns the
requested calls in `CamelOpenAIToolCalls`. The endpoint is also a model for the
[agent step](../guides/agent-step.md), which adds memory and approvals. Missing call ids (some
Ollama versions) are generated. The same works with `openai:responses` and with
`apiFormat=ollama`.

### How do I get JSON output?

Put `"response_format": {"type": "json_object"}` in a request-map body (or ask for JSON in the
system message) and `unmarshal: json` the answer.

### What happens on an API error?

A non-2xx response fails with `HttpOperationFailedException`; the message contains the status
and the API's `error.message` (for example `Incorrect API key provided`). Rate limits (429),
server errors and timeouts are retried first (`maxRetries`, default 2), then `fallbackModels`
are tried in order. A streamed call is not retried once text has arrived.

### Which providers does it work with?

Every server with an OpenAI-compatible Chat Completions API. `provider=groq` (or `mistral`,
`deepseek`, `openrouter`, `ollama`, ...) fills in the base URL and the key's environment
variable; see [model providers](../guides/model-providers.md). Anthropic and Gemini also have
native components ([anthropic](anthropic.md), [gemini](gemini.md)) that keep provider features
the compatible APIs drop.

## Related

- [AI components guide](../guides/ai-components.md)
- [Model providers and call optimizations](../guides/model-providers.md)
- [anthropic](anthropic.md), [gemini](gemini.md), [azure-openai](azure-openai.md), [aws-bedrock](aws-bedrock.md), [google-vertexai](google-vertexai.md)
- [slack](slack.md), [jira](jira.md), [telegram](telegram.md) for agents that post their results
- [Apache Camel LangChain4j Chat](https://camel.apache.org/components/next/langchain4j-chat-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
