# AWS Bedrock component

> The `aws-bedrock` component calls foundation models on Amazon Bedrock (Claude, Llama, Titan, Mistral and others) with the Converse and InvokeModel APIs, including streaming and guardrails.

**Scheme:** `aws-bedrock:` · **Also:** `aws2-bedrock:` (same component) · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [AWS Bedrock](https://camel.apache.org/components/next/aws-bedrock-component.html)

## When to use it

- Summarize, classify or translate messages with a large language model inside a route.
- Build AI agents: the Converse API takes a message list, a system prompt and tool definitions, and
  returns the model's answer, stop reason and token usage.
- Create embeddings for retrieval (`invokeEmbeddingsModel`) or images (`invokeImageModel`).
- Check prompts and answers against a Bedrock guardrail before they reach users.

For knowledge bases and Bedrock Agents, use [aws-bedrock-agent-runtime](aws-bedrock-agent-runtime.md).

## URI format

```text
aws-bedrock://label?modelId=...[&options]
```

The path is a free label. `modelId` is the Bedrock model id or an inference profile id.

## Example

Summarize support tickets from a queue with Claude and publish the summary to a topic:

<!-- example: aws-bedrock-summarize-tickets -->
```yaml
- route:
    id: summarize-tickets
    from:
      uri: "aws2-sqs://support-tickets?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - setHeader:
            name: CamelAwsBedrockConverseSystem
            constant: "Summarize the support ticket in one sentence."
        - to: "aws-bedrock://summarizer?region={{env:AWS_REGION:us-east-1}}&modelId={{env:BEDROCK_MODEL_ID:anthropic.claude-3-haiku-20240307-v1:0}}&operation=converse"
        - log: "summary (${header.CamelAwsBedrockConverseStopReason}): ${body}"
        - to: "aws2-sns://ticket-summaries?region={{env:AWS_REGION:us-east-1}}&autoCreateTopic=true"
```

With `converse`, a text body becomes one user message and the body after the call is the model's
answer text. The full output message, the stop reason and the token usage are in headers.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://support-tickets?region={{env:AWS_REGION:us-east-1}}")
        .set_header("CamelAwsBedrockConverseSystem", constant("Summarize the support ticket in one sentence."))
        .to("aws-bedrock://summarizer?region={{env:AWS_REGION:us-east-1}}&modelId={{env:BEDROCK_MODEL_ID:anthropic.claude-3-haiku-20240307-v1:0}}&operation=converse")
        .log("summary (${header.CamelAwsBedrockConverseStopReason}): ${body}")
        .to("aws2-sns://ticket-summaries?region={{env:AWS_REGION:us-east-1}}&autoCreateTopic=true");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `modelId` | | Model or inference profile id (`CamelAwsBedrockModelId` overrides it). |
| `operation` | `invokeTextModel` | `invokeTextModel`, `invokeImageModel`, `invokeEmbeddingsModel`, `converse`, `invokeTextModelStreaming`, `invokeImageModelStreaming`, `invokeEmbeddingsModelStreaming`, `converseStream`, `applyGuardrail`. |
| `streamOutputMode` | `complete` | Streaming operations: `complete` (body = whole text) or `chunks` (body = list of chunks). |
| `streamingChunkProcessor` | | `#bean` of a processor called for every chunk while the response streams (body = chunk text). |
| `guardrailIdentifier`, `guardrailVersion` | | Guardrail used by `applyGuardrail` and applied to invoke and converse requests. |
| `guardrailTrace` | `false` | Ask for the guardrail trace. |
| `promptCaching` | `false` | Converse (tool loop and agent step): add `cachePoint` blocks after the tools, the system prompt and the last message, so the stable prefix is read from Bedrock's prompt cache. Only for models that support prompt caching; others reject the request. |

The invoke operations post the body as is (a map is sent as JSON): you write the request in the
model's own format (for Anthropic models the Messages API body, for Titan `{"inputText": ...}`),
and the parsed JSON response becomes the body.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsBedrockOperation`, `CamelAwsBedrockModelId` | in | Override `operation` and `modelId`. |
| `CamelAwsBedrockContentType`, `CamelAwsBedrockAcceptContentType` | in | Content types of invoke requests (default `application/json`). |
| `CamelAwsBedrockConverseMessages` | in | Converse message list (`{role, content}` maps); otherwise the body is used. |
| `CamelAwsBedrockConverseSystem` | in | System prompt (text or content blocks). |
| `CamelAwsBedrockConverseInferenceConfig` | in | Map such as `{maxTokens, temperature, topP}`. |
| `CamelAwsBedrockConverseToolConfig` | in | Tool definitions for agentic tool use. |
| `CamelAwsBedrockConverseStopReason` | out | `end_turn`, `max_tokens`, `tool_use`, ... |
| `CamelAwsBedrockConverseUsage` | out | Token usage (map). |
| `CamelAwsBedrockConverseOutputMessage` | out | The assistant message, including tool use blocks. |
| `CamelAwsBedrockStreamOutputMode` | in | Overrides `streamOutputMode`. |
| `CamelAwsBedrockStreamingChunkCount`, `CamelAwsBedrockStreamingCompletionReason`, `CamelAwsBedrockStreamingTokenCount` | out | Streaming results. |
| `CamelAwsBedrockStreamingChunkIndex` | out | Index of the chunk given to `streamingChunkProcessor`. |
| `CamelAwsBedrockGuardrailIdentifier`, `CamelAwsBedrockGuardrailVersion` | in | Guardrail. |
| `CamelAwsBedrockGuardrailSource` | in | `INPUT` (default) or `OUTPUT` for `applyGuardrail`. |
| `CamelAwsBedrockGuardrailAction` | out | Result of `applyGuardrail` (`NONE`, `GUARDRAIL_INTERVENED`). |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e BEDROCK_MODEL_ID \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/summarize.camel.yaml
```

## Common questions

### How do I give the model tools (function calling)?

Use `operation=converse` with `tags`: the model gets the [ai-tool](ai-tool.md) routes with those
tags, and the component runs the calls (through the `ai-tool` envelope, as `toolUse` and
`toolResult` blocks) and loops until the model answers. The endpoint is also a model for the
[agent step](../guides/agent-step.md). To handle the calls yourself, set `CamelAwsBedrockConverseToolConfig` to a map with `tools` (each a `toolSpec` with name,
description and JSON schema). When the model wants a tool, the stop reason is `tool_use` and
`CamelAwsBedrockConverseOutputMessage` holds the tool use block; run the tool in the route and send
the next turn with `CamelAwsBedrockConverseMessages`.

### How do I stream the answer?

Use `operation=converseStream` (or an `...Streaming` invoke operation). The response is decoded as it
arrives; with `streamingChunkProcessor=#myProcessor` each chunk is handed to your processor while the
rest is still streaming.

### Which model id should I use?

Any id enabled in your account and region, for example
`anthropic.claude-3-haiku-20240307-v1:0` or a cross-region inference profile such as
`us.anthropic.claude-3-5-sonnet-20241022-v2:0`.

## Related

- [AWS Bedrock Agent Runtime](aws-bedrock-agent-runtime.md), [Azure OpenAI](azure-openai.md)
- [AWS Comprehend](aws2-comprehend.md), [AWS Textract](aws2-textract.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Bedrock](https://camel.apache.org/components/next/aws-bedrock-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
