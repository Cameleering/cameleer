# Google Vertex AI component

> The `google-vertexai` component calls Gemini and other models on Google Cloud Vertex AI: text and chat generation, streaming, multimodal prompts, embeddings, Imagen images, token counts and partner models such as Claude.

**Scheme:** `google-vertexai:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Google Vertex AI](https://camel.apache.org/components/next/google-vertexai-component.html)

## When to use it

- Summarize, classify or extract data from messages with Gemini inside a route.
- Create embeddings for documents and store them for semantic search (RAG).
- Describe images or documents (`generateMultimodal`) from a folder or a `gs://` bucket.
- Call Anthropic Claude or other partner models through Vertex AI (`rawPredict`) when your
  data must stay in your Google Cloud project.

## URI format

```text
google-vertexai:projectId:location:modelId[?options]
```

For example `google-vertexai:my-project:us-central1:gemini-2.0-flash`.

## Example

Summarize every support ticket that lands in a folder and write the summary to another folder:

<!-- example: google-vertexai-summarize-tickets -->
```yaml
- route:
    id: summarize-tickets
    from:
      uri: "file:{{env:INBOX_DIR:tickets}}?include=.*\\.txt&move=.done"
      steps:
        - to: "google-vertexai:{{env:GCP_PROJECT:my-project}}:{{env:VERTEX_LOCATION:us-central1}}:{{env:VERTEX_MODEL:gemini-2.0-flash}}?temperature=0.2&maxOutputTokens=256&systemInstruction=Summarize the support ticket in one sentence."
        - log: "summary of ${file:name}: ${body} (${header.CamelGoogleVertexAITotalTokenCount} tokens)"
        - to: "file:{{env:OUT_DIR:summaries}}"
```

Create an embedding for each document and save the vector as JSON:

<!-- example: google-vertexai-embeddings -->
```yaml
- route:
    id: embed-documents
    from:
      uri: "file:{{env:INBOX_DIR:docs}}?include=.*\\.txt&move=.done"
      steps:
        - convertBodyTo:
            type: String
        - to: "google-vertexai:{{env:GCP_PROJECT:my-project}}:{{env:VERTEX_LOCATION:us-central1}}:text-embedding-004?operation=generateEmbeddings&taskType=RETRIEVAL_DOCUMENT"
        - marshal:
            json: {}
        - setHeader:
            name: CamelFileName
            simple: "${file:onlyname.noext}.json"
        - to: "file:{{env:OUT_DIR:vectors}}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("file:tickets?include=.*\\.txt")
        .to("google-vertexai:my-project:us-central1:gemini-2.0-flash?systemInstruction=Summarize in one sentence.")
        .to("file:summaries");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `operation` | `generateText` | See the operations below. Header `CamelGoogleVertexAIOperation`. |
| `temperature`, `topP`, `topK` | | Sampling settings (`generationConfig`). |
| `maxOutputTokens` | | Maximum tokens in the answer (`rawPredict`: `max_tokens`, default 1024). |
| `candidateCount` | | Number of candidates. |
| `systemInstruction` | | System prompt (header `CamelGoogleVertexAISystemInstruction` wins). |
| `publisher` | `google` | Model publisher: `google`, or a partner such as `anthropic`, `meta`, `mistralai`. |
| `streamOutputMode` | `complete` | Streaming: `complete` (body = whole text) or `chunks` (body = list of chunks). |
| `streamingChunkProcessor` | | Streaming: `#bean` processor called with each chunk. |
| `taskType` | | `generateEmbeddings`: for example `RETRIEVAL_DOCUMENT`, `RETRIEVAL_QUERY`. |
| `outputDimensionality` | | `generateEmbeddings`: vector size. |
| `sampleCount` | `1` | `generateImage`: number of images. |
| `aspectRatio` | | `generateImage`: for example `16:9`. |

Credentials are shared by all Google components: see [credentials](google-storage.md#credentials)
(scope `cloud-platform`).

### Operations

- `generateText` (default), `generateChat`, `generateCode`: a text body is one user turn; a list
  body is a conversation of `{role, content}` (or `{role, parts}`) maps, where role `assistant`
  becomes `model`; a map body is sent as the request as it is (use this for `tools`,
  `responseSchema` or safety settings). Body = the text of the first candidate.
- `generateTextStreaming`, `generateChatStreaming`, `generateCodeStreaming`: the same with
  `streamGenerateContent` (server-sent events, decoded as they arrive).
- `generateMultimodal`: the body bytes (MIME type `CamelGoogleVertexAIMediaMimeType`, default
  `image/png`) or a `gs://` URI in `CamelGoogleVertexAIMediaGcsUri`, with the prompt in
  `CamelGoogleVertexAIPrompt`.
- `countTokens`: body = total token count.
- `generateEmbeddings`: a text body gives one vector, a list body a list of vectors.
- `generateImage` (Imagen): body = list of image bytes.
- `rawPredict` / `streamRawPredict` (partner models): a map body is sent as it is (for example an
  Anthropic Messages request); a text body is wrapped into an Anthropic Messages request
  (`anthropic_version` `vertex-2023-10-16`). The answer text is in
  `CamelGoogleVertexAIResponseText` (`rawPredict`) or the body (`streamRawPredict`).

### What is not supported

- Function calling runs for Gemini models with `tags` (see below); partner models through
  `rawPredict` get no tool loop.
- No Live API (bidirectional audio / video), no context caching, no batch prediction jobs.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGoogleVertexAIOperation` | in | Operation. |
| `CamelGoogleVertexAISystemInstruction` | in | System prompt. |
| `CamelGoogleVertexAIPrompt` | in | Text prompt of `generateMultimodal`. |
| `CamelGoogleVertexAIMediaMimeType` | in | MIME type of the media body. |
| `CamelGoogleVertexAIMediaGcsUri` | in | `gs://` URI of the media. |
| `CamelGoogleVertexAIFinishReason` | out | `STOP`, `MAX_TOKENS`, `SAFETY`, ... |
| `CamelGoogleVertexAIPromptTokenCount` | out | Prompt tokens. |
| `CamelGoogleVertexAICandidatesTokenCount` | out | Answer tokens. |
| `CamelGoogleVertexAITotalTokenCount` | out | Total tokens. |
| `CamelGoogleVertexAISafetyRatings` | out | Safety ratings of the first candidate. |
| `CamelGoogleVertexAIResponse` | out | The raw response. |
| `CamelGoogleVertexAIResponseText` | out | Text of a partner model answer. |
| `CamelGoogleVertexAIStreamingChunkCount` | out | Streaming: number of chunks. |
| `CamelGoogleVertexAIStreamingChunkIndex` | out | Streaming: index of the chunk given to the chunk processor. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" -v "$PWD/tickets:/in" -v "$PWD/summaries:/out" \
  -v "$HOME/keys/sa.json:/keys/sa.json:ro" -e GOOGLE_APPLICATION_CREDENTIALS=/keys/sa.json \
  -e GCP_PROJECT=my-project -e INBOX_DIR=/in -e OUT_DIR=/out \
  pmsudhi/cameleer run /routes/summarize-tickets.camel.yaml
```

## Common questions

### How do I let Gemini call tools?

Set `tags`: Gemini gets the [ai-tool](ai-tool.md) routes with those tags as `functionDeclarations`,
and the component runs each `functionCall` (through the `ai-tool` envelope), sends back the
`functionResponse` and the call's `thoughtSignature`, and loops until Gemini answers. Gemini has no
call ids, so they are generated. The endpoint is also a model for the
[agent step](../guides/agent-step.md).

### How do I send a chat history to Gemini?
Send a list of `{role, content}` maps with `operation=generateChat`; use roles `user` and
`assistant` (or `model`).

### How do I get JSON output from Gemini?
Send a map body with `contents` and `generationConfig.responseMimeType=application/json`
(and a `responseSchema`), then `unmarshal: json` the body.

### How do I call Claude on Vertex AI?
Use the Claude model id with `publisher=anthropic&operation=rawPredict` and a text body, or a
map body with an Anthropic Messages request.

### How does this compare to the openai component?
[`openai`](openai.md) talks to any OpenAI-compatible server (OpenAI, Ollama, vLLM). See the
[AI components guide](../guides/ai-components.md) for a side-by-side view.

## Related

- [OpenAI](openai.md), [AWS Bedrock](aws-bedrock.md), [Azure OpenAI](azure-openai.md)
- [AI components guide](../guides/ai-components.md)
- [Apache Camel Google Vertex AI](https://camel.apache.org/components/next/google-vertexai-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
