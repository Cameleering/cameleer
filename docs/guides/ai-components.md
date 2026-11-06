# AI components

> Cameleer calls large language models from routes with seven components: `openai`/`llm` (OpenAI, the Responses API, Ollama and any OpenAI-compatible server, with `provider=` presets for 19 services), `anthropic`, `gemini`, `azure-openai`, `aws-bedrock` and `google-vertexai`; they cover chat, embeddings, tool calling and, depending on the provider, streaming and images.

Every AI component is a producer: a step in a route that sends the message body to a model and
replaces the body with the answer. Everything else (where the text comes from, what happens with
the answer, retries, filtering, splitting) is ordinary Camel routing, so an "AI agent" in
Cameleer is a route: a trigger, one or more model calls, and the systems the agent acts on.

## Which component to use

| | [openai](../components/openai.md) | [azure-openai](../components/azure-openai.md) | [aws-bedrock](../components/aws-bedrock.md) | [google-vertexai](../components/google-vertexai.md) |
|---|---|---|---|---|
| Providers | OpenAI (Chat Completions and the Responses API), Ollama (also its native `/api/chat`), vLLM, LM Studio, LiteLLM, any `/v1/chat/completions` server | Azure OpenAI deployments | Models on Amazon Bedrock (Anthropic, Amazon Titan, Meta, Mistral, ...) | Gemini; partner models (Anthropic, Meta, Mistral) via `rawPredict` |
| Chat | `chat-completion`, `responses` | `chatCompletion` | `converse`, `invokeTextModel` | `generateText`, `generateChat`, `generateCode`, `generateMultimodal` |
| Embeddings | `embeddings` | `embeddings` | `invokeEmbeddingsModel` | `generateEmbeddings` |
| Streaming | yes (`streaming=true`, also in the tool loop) | yes (`streaming=true`) | yes (`converseStream`, `invoke*Streaming`) | yes (`generate*Streaming`, `streamRawPredict`) |
| Tool calls | `tags`: the tool loop over your `ai-tool` routes; `autoToolExecution=false` returns the calls in `CamelOpenAIToolCalls` | the model's tool calls in the `CamelAzureOpenAIToolCalls` header | `CamelAwsBedrockConverseToolConfig` header; tool-use blocks in `CamelAwsBedrockConverseOutputMessage` | pass-through only: send `tools` in a request map, read the raw response in `CamelGoogleVertexAIResponse` |
| Images | no | `imageGeneration` (DALL-E) | `invokeImageModel` | `generateImage` (Imagen), image input with `generateMultimodal` |
| Extras | | Assistants API (`runAssistant`, threads, runs) | Guardrails, knowledge bases and agents (`aws-bedrock-agent-runtime`) | token counting (`countTokens`) |
| Auth | API key (Bearer) or none | API key or Entra ID client credentials | AWS credentials (SigV4) | Google service account / workload identity |

All four give the answer text as the body and token usage and finish reason as headers. Check
each component page for the exact option and header names.

## What is not supported

Be aware of these limits before you design around them:

- **Tool calling and the agent loop** work in `openai`/`llm` (and every
  [`provider`](model-providers.md) preset), `anthropic`, `gemini`, `azure-openai`,
  `aws-bedrock` (Converse) and `google-vertexai` (Gemini): with `tags` they run your
  [`ai-tool`](../components/ai-tool.md) routes for the model, and the
  [agent step](agent-step.md) adds memory, approvals and limits. Server-side agents also work:
  Bedrock Agents (`aws-bedrock-agent-runtime`) and Azure OpenAI Assistants (`runAssistant`).
- **No vector store component.** Embeddings are lists of numbers in the body; store and search
  them with the data store you already run ([mongodb](../components/mongodb.md),
  [elasticsearch](../components/elasticsearch.md), [spring-redis](../components/spring-redis.md),
  files).
- **No LangChain4j components** (`langchain4j-chat`, `langchain4j-embeddings`, ...). Claude
  and Gemini are available directly with [`anthropic`](../components/anthropic.md) and
  [`gemini`](../components/gemini.md), or through Bedrock and Vertex AI; tools of other MCP
  servers come in through the [MCP client](mcp-client.md). Routes are published as tools, resources and prompts
  with [`ai-tool`](../components/ai-tool.md), [`ai-resource`](../components/ai-resource.md) and
  [`ai-prompt`](../components/ai-prompt.md), and the [MCP server](mcp-server.md) serves them.

## Classify and summarise with an LLM, then act

An agent that triages incidents: the model writes a one-line summary, which becomes the Jira
issue summary, while the original text stays the issue description.

<!-- example: ai-triage-to-jira -->
```yaml
- route:
    id: ai-triage-to-jira
    from:
      uri: "direct:incident"
      steps:
        - setProperty:
            name: report
            simple: "${body}"
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&apiKey={{env:OPENAI_API_KEY}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&temperature=0&maxTokens=40&systemMessage=Write a one-line Jira summary of this incident report."
        - setHeader:
            name: IssueSummary
            simple: "${body}"
        - setBody:
            simple: "${exchangeProperty.report}"
        - setHeader:
            name: ProjectKey
            constant: "OPS"
        - setHeader:
            name: IssueTypeName
            constant: "Bug"
        - to: "jira:addIssue?jiraUrl={{env:JIRA_URL:https://your-site.atlassian.net}}&username={{env:JIRA_USER}}&password={{env:JIRA_API_TOKEN}}"
        - log: "opened ${header.IssueKey}: ${header.IssueSummary}"
```

The same shape works with any target: post the answer to [Slack](../components/slack.md), reply
on [Telegram](../components/telegram.md), comment a [GitHub](../components/github.md) pull
request, or use the answer in a `choice` to route the message (see the
[ticket triage example](../../examples/ai-ticket-triage.camel.yaml)).

## Embeddings into a store

Embed every text file dropped into a folder and write the vector next to it as JSON, ready to be
loaded into a vector index. Swap the last step for a database component to store it directly.

<!-- example: ai-embeddings-to-files -->
```yaml
- route:
    id: embed-documents
    from:
      uri: "file:{{env:DOCS_DIR:docs-in}}?include=.*\\.txt&delay=2000"
      steps:
        - convertBodyTo:
            type: String
        - to: "openai:embeddings?model={{env:OPENAI_EMBEDDING_MODEL:text-embedding-3-small}}&apiKey={{env:OPENAI_API_KEY}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}"
        - marshal:
            json: {}
        - to: "file:{{env:VECTOR_DIR:vectors}}?fileName=${file:onlyname.noext}.json"
```

## Local models: Ollama, vLLM, LM Studio

Point `baseUrl` at the server's OpenAI-compatible endpoint; nothing else changes. This keeps the
data on the machine, which matters on edge devices and in regulated networks.

| server | `baseUrl` |
|---|---|
| Ollama | `http://localhost:11434/v1` |
| vLLM | `http://localhost:8000/v1` |
| LM Studio | `http://localhost:1234/v1` |

The [openai page](../components/openai.md) has a complete local-model route.

## Tool calls

With Azure OpenAI, send the request as a map with `tools`; when the model decides to call a tool,
the finish reason is `tool_calls` and the calls (function name and JSON arguments) are in the
`CamelAzureOpenAIToolCalls` header. Your route runs the tool and sends a follow-up request.

<!-- example: ai-azure-tool-calls -->
```yaml
- route:
    id: weather-tool-call
    from:
      uri: "direct:ask-weather"
      steps:
        - setBody:
            constant: '{"messages":[{"role":"user","content":"What is the weather in Oslo?"}],"tools":[{"type":"function","function":{"name":"get_weather","parameters":{"type":"object","properties":{"city":{"type":"string"}}}}}]}'
        - unmarshal:
            json: {}
        - to: "azure-openai://{{env:AZURE_OPENAI_RESOURCE:my-resource}}/{{env:AZURE_OPENAI_DEPLOYMENT:gpt-4o}}?apiKey=RAW({{env:AZURE_OPENAI_KEY}})"
        - log: "finish=${header.CamelAzureOpenAIFinishReason} calls=${header.CamelAzureOpenAIToolCalls}"
```

With `openai:` the same request map is sent unchanged; add `outputType=full` to get the whole
response, including `choices[0].message.tool_calls`, as the body:

<!-- example: ai-openai-tools-passthrough -->
```yaml
- route:
    id: openai-tools-passthrough
    from:
      uri: "direct:ask-with-tools"
      steps:
        - setBody:
            constant: '{"messages":[{"role":"user","content":"What is the weather in Oslo?"}],"tools":[{"type":"function","function":{"name":"get_weather","parameters":{"type":"object","properties":{"city":{"type":"string"}}}}}]}'
        - unmarshal:
            json: {}
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&apiKey={{env:OPENAI_API_KEY}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&outputType=full"
        - log: "finish reason: ${header.CamelOpenAIFinishReason}"
```

With Bedrock, put the tool configuration map in the `CamelAwsBedrockConverseToolConfig` header
of a `converse` call; see [aws-bedrock](../components/aws-bedrock.md).

## Google Vertex AI (Gemini)

<!-- example: ai-vertex-gemini -->
```yaml
- route:
    id: gemini-answer
    from:
      uri: "direct:ask-gemini"
      steps:
        - to: "google-vertexai:{{env:GCP_PROJECT:my-project}}:us-central1:gemini-1.5-flash?temperature=0.2&maxOutputTokens=256&systemInstruction=Answer in one paragraph."
        - log: "finish=${header.CamelGoogleVertexAIFinishReason} tokens=${header.CamelGoogleVertexAITotalTokenCount}"
```

Credentials come from `serviceAccountKey` or the `GOOGLE_APPLICATION_CREDENTIALS` environment
variable. Embeddings use the same component with an embedding model:

<!-- example: ai-vertex-embeddings -->
```yaml
- route:
    id: gemini-embeddings
    from:
      uri: "direct:embed-gemini"
      steps:
        - to: "google-vertexai:{{env:GCP_PROJECT:my-project}}:us-central1:text-embedding-004?operation=generateEmbeddings&taskType=RETRIEVAL_DOCUMENT"
```

More options (streaming, multimodal input, Imagen, Claude on Vertex) are on the
[google-vertexai page](../components/google-vertexai.md).

## Amazon Bedrock

The `converse` operation works the same way for every Bedrock chat model:

<!-- example: ai-bedrock-converse -->
```yaml
- route:
    id: bedrock-converse
    from:
      uri: "direct:ask-bedrock"
      steps:
        - setHeader:
            name: CamelAwsBedrockConverseSystem
            constant: "Answer in one sentence."
        - to: "aws-bedrock://chat?modelId=anthropic.claude-3-haiku-20240307-v1:0&operation=converse&region={{env:AWS_REGION:us-east-1}}&useDefaultCredentialsProvider=true"
        - log: "stop=${header.CamelAwsBedrockConverseStopReason}"
```

See [aws-bedrock](../components/aws-bedrock.md) for streaming, embeddings, guardrails,
knowledge bases and Bedrock Agents.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:summarise")
        .to("openai:chat-completion?model=gpt-4o-mini&systemMessage=Summarise in one sentence.")
        .to("slack:#ops?webhookUrl={{env:SLACK_WEBHOOK_URL}}");
})?;
```

## Run it with Docker

```sh
export OPENAI_API_KEY=sk-... JIRA_USER=bot@example.com JIRA_API_TOKEN=... JIRA_URL=https://your-site.atlassian.net
docker run --rm -e OPENAI_API_KEY -e JIRA_URL -e JIRA_USER -e JIRA_API_TOKEN \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/ai-triage-to-jira.camel.yaml
```

## Common questions

### How do I keep the original message after calling the model?

Save it first (`setProperty` with `${body}`), call the model, move the answer into a header,
then restore the body from the property, as in the triage example above.

### How do I make model calls reliable?

Treat them like any remote call: `onException` with a redelivery policy for timeouts and 429
responses, a `circuitBreaker` around the call, and `temperature=0` for classification so the
answers are stable.

### Can the same route run against OpenAI in the cloud and Ollama at the edge?

Yes. Put `baseUrl`, `model` and `apiKey` in `{{env:...}}` placeholders, as in the examples, and
set them per deployment.

### How are the examples tested?

Every example on this page runs in the test suite against embedded mock servers of the OpenAI,
Azure OpenAI, Bedrock, Vertex AI and Jira APIs, so the option names and the request formats are
checked on every build.

## Related

- [openai](../components/openai.md), [azure-openai](../components/azure-openai.md), [aws-bedrock](../components/aws-bedrock.md), [google-vertexai](../components/google-vertexai.md)
- [slack](../components/slack.md), [jira](../components/jira.md), [github](../components/github.md), [telegram](../components/telegram.md)
- [EIPs](eips.md)
- [Apache Camel AI components](https://camel.apache.org/components/next/langchain4j-chat-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
