# Azure OpenAI component

> The `azure-openai` component calls models deployed on Azure OpenAI: chat completions (with streaming and tool calls), embeddings, image generation and the Assistants API.

**Scheme:** `azure-openai:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** none (Apache Camel reaches Azure OpenAI through LangChain4j; this is a Cameleer component)

## When to use it

- Answer, summarize or classify messages with GPT models deployed in your Azure tenant.
- Build AI agents: send a message list with tool definitions and handle the tool calls in the route.
- Create embeddings for search and retrieval.
- Run an Assistants API thread from a route (`runAssistant`).

## URI format

```text
azure-openai://resourceName/deploymentName[?options]
```

Requests go to `https://{resourceName}.openai.azure.com/openai/deployments/{deploymentName}/...`.

## Example

Answer questions from a Storage queue with a system prompt:

<!-- example: azure-openai-answer-questions -->
```yaml
- route:
    id: answer-questions
    from:
      uri: "azure-storage-queue://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/questions?accessKey=RAW({{env:AZURE_STORAGE_KEY:}})"
      steps:
        - to: "azure-openai://{{env:AZURE_OPENAI_RESOURCE:my-openai}}/{{env:AZURE_OPENAI_DEPLOYMENT:gpt-4o-mini}}?apiKey=RAW({{env:AZURE_OPENAI_API_KEY:}})&operation=chatCompletion&temperature=0.2&systemMessage=You answer customer questions about shipping in two sentences."
        - log: "answer (${header.CamelAzureOpenAIFinishReason}): ${body}"
        - to: "log:answers"
```

A text body becomes the user message (after the `systemMessage`); the body after the call is the
assistant's answer. Token usage and the model are in headers.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("azure-storage-queue://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/questions?accessKey=RAW({{env:AZURE_STORAGE_KEY:}})")
        .to("azure-openai://{{env:AZURE_OPENAI_RESOURCE:my-openai}}/{{env:AZURE_OPENAI_DEPLOYMENT:gpt-4o-mini}}?apiKey=RAW({{env:AZURE_OPENAI_API_KEY:}})&operation=chatCompletion&temperature=0.2&systemMessage=You answer customer questions about shipping in two sentences.")
        .log("answer (${header.CamelAzureOpenAIFinishReason}): ${body}")
        .to("log:answers");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `apiKey` | | API key (`api-key` header). |
| `credentialType`, `tenantId`, `clientId`, `clientSecret` | | Microsoft Entra ID client credentials instead of a key (scope `https://cognitiveservices.azure.com/.default`). |
| `apiVersion` | `2024-06-01` | REST API version. |
| `operation` | `chatCompletion` | `chatCompletion`, `chatCompletionStream`, `embeddings`, `imageGeneration`, `createAssistant`, `getAssistant`, `listAssistants`, `deleteAssistant`, `createThread`, `createMessage`, `listMessages`, `createRun`, `getRun`, `runAssistant`. |
| `systemMessage` | | System prompt for a text body. |
| `temperature`, `maxTokens`, `topP` | | Sampling settings. |
| `streaming` | `false` | Stream the chat answer (`text/event-stream`). |
| `streamOutputMode` | `complete` | `complete` (body = whole text) or `chunks` (list of deltas). |
| `streamingChunkProcessor` | | `#bean` of a processor called for every delta while streaming. |
| `imageSize`, `imageCount`, `imageQuality`, `imageResponseFormat` | `1024x1024`, , , `url` | `imageGeneration` settings (`url` or `b64_json`). |
| `assistantsApiVersion` | `2024-05-01-preview` | API version of the Assistants operations. |
| `runPollInterval`, `runTimeout` | `200`, `60000` | Polling of `runAssistant` (ms). |
| `uriEndpointOverride`, `proxyHost`, `proxyPort`, `trustAllCertificates` | | Connection settings (for example an API gateway in front of Azure OpenAI). |

A list body is sent as the message list (`{role, content}` maps); a map body is sent as the request.
For `embeddings` a text body gives one vector and a list body a list of vectors.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAzureOpenAIOperation` | in | Overrides `operation`. |
| `CamelAzureOpenAISystemMessage` | in | Overrides `systemMessage`. |
| `CamelAzureOpenAIStreaming` | in | Overrides `streaming`. |
| `CamelAzureOpenAIFinishReason` | out | `stop`, `length`, `tool_calls`, ... |
| `CamelAzureOpenAIUsage` | out | Token usage (map). |
| `CamelAzureOpenAIModel` | out | Model that answered. |
| `CamelAzureOpenAIToolCalls` | out | Tool calls requested by the model. |
| `CamelAzureOpenAIStreamingChunkCount`, `CamelAzureOpenAIStreamingChunkIndex` | out | Streaming. |
| `CamelAzureOpenAIImageSize`, `CamelAzureOpenAIRevisedPrompts` | in/out | Image generation. |
| `CamelAzureOpenAIAssistantId`, `CamelAzureOpenAIAssistantName`, `CamelAzureOpenAIThreadId`, `CamelAzureOpenAIRunId`, `CamelAzureOpenAIRunStatus` | in/out | Assistants API. |

## Run it with Docker

```sh
docker run --rm -e AZURE_STORAGE_ACCOUNT -e AZURE_STORAGE_KEY -e AZURE_OPENAI_RESOURCE -e AZURE_OPENAI_DEPLOYMENT -e AZURE_OPENAI_API_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/answers.camel.yaml
```

## Common questions

### How do I let the model call tools?

Send a map body with `messages` and `tools` (OpenAI function definitions). When the model calls a tool,
`CamelAzureOpenAIFinishReason` is `tool_calls` and `CamelAzureOpenAIToolCalls` lists the calls; run
them in the route and send the next request with the tool results.

### Can I use OpenAI or a local model instead?

Use the `openai` component for OpenAI-compatible APIs (OpenAI, Ollama, vLLM); `azure-openai` speaks
the Azure deployment URLs and authentication.

## Related

- [AWS Bedrock](aws-bedrock.md), [AWS Bedrock Agent Runtime](aws-bedrock-agent-runtime.md)
- [Cloud credentials](../guides/cloud-credentials.md)
