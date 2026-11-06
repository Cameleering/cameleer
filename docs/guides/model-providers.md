# Model providers and call optimizations

> Cameleer talks to 23 model providers out of the box: seven native components and 19 OpenAI-compatible services through `provider=` presets, plus any other OpenAI-compatible server through `baseUrl`. Around each model call it adds what usually needs a separate gateway: retries, fallback models, prompt caching, an exact response cache and coalescing of identical requests. These run in the same process as the routes, so they add no network hop.

**Cargo feature:** [`saas`](build-features.md) (`openai`, `llm`, `anthropic`, `gemini`) and `cloud` (`azure-openai`, `aws-bedrock`, `google-vertexai`) · **Apache Camel:** the `openai`/`llm` options mirror Camel 4.23; presets, caching and the reliability options are Cameleer extensions

## Native components

| component | API | tool calling | notes |
|---|---|---|---|
| [`openai`, `llm`](../components/openai.md) | OpenAI Chat Completions, Responses API, embeddings | yes | streaming in the tool loop |
| `openai` with `apiFormat=ollama` | Ollama's `/api/chat` | yes | |
| [`anthropic`](../components/anthropic.md) | Anthropic Messages API | yes | automatic prompt caching |
| [`gemini`](../components/gemini.md) | Gemini API (AI Studio keys) | yes | thought signatures kept |
| [`azure-openai`](../components/azure-openai.md) | Azure OpenAI deployments | yes | Entra ID or keys |
| [`aws-bedrock`](../components/aws-bedrock.md) | Bedrock Converse (Claude, Llama, Mistral, Nova, ...) | yes | guardrails, opt-in prompt caching |
| [`google-vertexai`](../components/google-vertexai.md) | Gemini on Vertex AI | yes | service accounts |

All of them are models for the [agent step](agent-step.md) and take the same tool options.

## Provider presets

`provider=NAME` on `openai:` / `llm:` sets the base URL and the environment variable the key is
read from. `baseUrl` and `apiKey` still override both.

<!-- example: model-providers-groq -->
```yaml
- route:
    from:
      uri: direct:ask
      steps:
        - to: "llm:chat-completion?provider=groq&model=llama-3.3-70b-versatile&tags=support"
```

| `provider` | base URL | key from |
|---|---|---|
| `openai` (default) | `https://api.openai.com/v1` | `OPENAI_API_KEY` |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta/openai` | `GEMINI_API_KEY` |
| `mistral` | `https://api.mistral.ai/v1` | `MISTRAL_API_KEY` |
| `deepseek` | `https://api.deepseek.com/v1` | `DEEPSEEK_API_KEY` |
| `xai` | `https://api.x.ai/v1` | `XAI_API_KEY` |
| `groq` | `https://api.groq.com/openai/v1` | `GROQ_API_KEY` |
| `cerebras` | `https://api.cerebras.ai/v1` | `CEREBRAS_API_KEY` |
| `together` | `https://api.together.xyz/v1` | `TOGETHER_API_KEY` |
| `fireworks` | `https://api.fireworks.ai/inference/v1` | `FIREWORKS_API_KEY` |
| `openrouter` | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` |
| `perplexity` | `https://api.perplexity.ai` | `PERPLEXITY_API_KEY` |
| `huggingface` | `https://router.huggingface.co/v1` | `HF_TOKEN` |
| `dashscope` (Qwen) | `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` | `DASHSCOPE_API_KEY` |
| `moonshot` (Kimi) | `https://api.moonshot.ai/v1` | `MOONSHOT_API_KEY` |
| `cohere` | `https://api.cohere.ai/compatibility/v1` | `COHERE_API_KEY` |
| `nvidia` (NIM) | `https://integrate.api.nvidia.com/v1` | `NVIDIA_API_KEY` |
| `ollama` | `http://localhost:11434/v1` | `OLLAMA_API_KEY` (usually none) |
| `vllm` | `http://localhost:8000/v1` | `VLLM_API_KEY` |
| `lmstudio` | `http://localhost:1234/v1` | `LMSTUDIO_API_KEY` |
| `llamacpp` | `http://localhost:8080/v1` | `LLAMACPP_API_KEY` |

Any other OpenAI-compatible server works with `baseUrl`. Azure OpenAI's v1 API and Bedrock's
OpenAI-compatible endpoint work the same way; the native components add Entra ID, SigV4 and
guardrails.

What the component adjusts per provider:

- OpenAI's o-series and GPT-5 models get `max_completion_tokens` instead of `max_tokens`.
- DeepSeek, Qwen and Kimi return `reasoning_content` with tool calls and want it back on the
  next turn; Cameleer sends it back.
- Missing tool call ids (some Ollama versions) are generated.

The presets were checked against each provider's documentation, not against the live
services; the tests run against mocks.

## Reliability and cost

Every model component takes these options. They apply to the tool loop and the agent step.

| option | default | description |
|---|---|---|
| `maxRetries` | `2` | Retries of 408, 409, 425, 429, 5xx and 529 answers, timeouts and resets, with exponential backoff |
| `retryDelay` | `500ms` | The first backoff; it doubles each retry, up to 10 s |
| `fallbackModels` | | Model endpoint URIs, comma separated, tried in order when the model still fails. Inside a URI option, write the fallback's own `&` as `%26` |
| `responseCache` | `off` | `exact`: an identical request (same model, messages, tools and system prompt) is answered from memory |
| `responseCacheTtl` | `5m` | How long a cached answer is used |
| `responseCacheSize` | `1000` | Answers kept; the oldest goes first |
| `coalesce` | `false` | Identical requests in flight at the same time share one model call |

<!-- example: model-providers-fallback -->
```yaml
- route:
    from:
      uri: direct:ask
      steps:
        - to: "openai:chat-completion?model=gpt-5.4-mini&tags=support&responseCache=exact&fallbackModels=anthropic:messages?model=claude-sonnet-4-5"
```

What these guarantee:

- A retried, cached or coalesced model turn never repeats a tool call: tools run in the
  [`ai-tool` envelope](../components/ai-tool.md) after the model answers, with their own
  idempotency.
- A cached or coalesced answer reports zero tokens, so the usage headers show what was paid.
- A streamed call is not retried once text has been sent to the chunk processor.
- `responseCache` and `coalesce` are off by default: two users who send the same prompt get
  the same answer. Turn them on for classification, extraction and other deterministic calls.

## Prompt caching

Providers bill cached input at a fraction of the normal price, but only for a prefix that is
byte-for-byte the same as last time. Cameleer assembles the prompt itself (tools in a fixed
order, then the system prompt, then the history), so the prefix is stable from turn to turn.

| component | how | default |
|---|---|---|
| `anthropic` | `cache_control` breakpoints on the last tool, the system prompt and the last message | on (`promptCaching=auto`) |
| `aws-bedrock` | Converse `cachePoint` after the tools, the system prompt and the last message | off (`promptCaching=true`), because models without caching reject it |
| `openai` and presets | the provider's automatic prefix caching | always (provider side) |
| `gemini`, `google-vertexai` | Gemini's implicit caching | always (provider side) |

`CamelAiCacheReadTokens` and `CamelAiCacheWriteTokens` report the cached input tokens of a
call when there were any. A prompt shorter than the provider's minimum (about 1024 tokens for
most models) is not cached and costs nothing extra.

## Why in-process

A hosted gateway adds a network hop to every model call; a sidecar gateway adds a local hop and
another process. Cameleer's model calls go straight from the route to the provider over pooled
keep-alive connections (no TCP and TLS handshake after the first call), and the cache and
coalescing live in the same memory as the routes. The tool loop, approvals and the audit log
are in the same process too, so a retried model turn can't run a tool twice.

## Related

- [The agent step and tool calling](agent-step.md)
- [ai-tool](../components/ai-tool.md): the envelope around every tool call
- [MCP server](mcp-server.md) and [MCP client](mcp-client.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
