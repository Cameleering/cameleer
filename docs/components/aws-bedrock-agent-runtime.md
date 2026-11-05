# AWS Bedrock Agent Runtime component

> The `aws-bedrock-agent-runtime` component queries Amazon Bedrock knowledge bases (retrieve, retrieve-and-generate) and invokes Bedrock Agents.

**Scheme:** `aws-bedrock-agent-runtime:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Bedrock Agent Runtime](https://camel.apache.org/components/next/aws-bedrock-agent-runtime-component.html)

## When to use it

- Answer questions from your own documents (retrieval-augmented generation) inside a route.
- Fetch the most relevant passages for a query and pass them to another model or service.
- Call a Bedrock Agent (with its action groups and memory) from an integration, keeping the session
  id between calls.

## URI format

```text
aws-bedrock-agent-runtime://label[?options]
```

## Example

Answer questions from a queue with a knowledge base and a Claude model:

<!-- example: aws-bedrock-agent-runtime-rag-answers -->
```yaml
- route:
    id: answer-questions
    from:
      uri: "aws2-sqs://questions?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - to: "aws-bedrock-agent-runtime://kb?region={{env:AWS_REGION:us-east-1}}&knowledgeBaseId={{env:KNOWLEDGE_BASE_ID:KB12345678}}&modelId=anthropic.claude-3-haiku-20240307-v1:0&operation=retrieveAndGenerate"
        - log: "answer (session ${header.CamelAwsBedrockAgentRuntimeSessionId}): ${body}"
        - to: "log:answers"
```

The body text is the question; the body after the call is the generated answer. The citations are
in the `CamelAwsBedrockAgentRuntimeCitations` header. A model id that is not an ARN is expanded to
`arn:aws:bedrock:{region}::foundation-model/{id}`.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://questions?region={{env:AWS_REGION:us-east-1}}")
        .to("aws-bedrock-agent-runtime://kb?region={{env:AWS_REGION:us-east-1}}&knowledgeBaseId={{env:KNOWLEDGE_BASE_ID:KB12345678}}&modelId=anthropic.claude-3-haiku-20240307-v1:0&operation=retrieveAndGenerate")
        .log("answer (session ${header.CamelAwsBedrockAgentRuntimeSessionId}): ${body}")
        .to("log:answers");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `retrieve` | `retrieve`, `retrieveAndGenerate` or `invokeAgent`. |
| `knowledgeBaseId` | | Knowledge base of `retrieve` and `retrieveAndGenerate`. |
| `modelId` | | Model of `retrieveAndGenerate` (id or ARN). |
| `agentId`, `agentAliasId` | | Agent of `invokeAgent`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsBedrockAgentRuntimeOperation` | in | Overrides `operation`. |
| `CamelAwsBedrockAgentRuntimeKnowledgeBaseId` | in | Overrides `knowledgeBaseId`. |
| `CamelAwsBedrockAgentRuntimeNumberOfResults` | in | Results of `retrieve`. |
| `CamelAwsBedrockAgentRuntimeSessionId` | in/out | Session of `retrieveAndGenerate` and `invokeAgent` (generated when missing). |
| `CamelAwsBedrockAgentRuntimeCitations` | out | Citations of the answer. |
| `CamelAwsBedrockAgentRuntimeAgentId`, `CamelAwsBedrockAgentRuntimeAgentAliasId` | in | Override the agent. |
| `CamelAwsBedrockAgentRuntimeEnableTrace`, `CamelAwsBedrockAgentRuntimeEndSession` | in | `invokeAgent` flags. |
| `CamelAwsBedrockAgentRuntimeSessionAttributes`, `CamelAwsBedrockAgentRuntimePromptSessionAttributes` | in | `invokeAgent` session state (maps). |
| `CamelAwsBedrockAgentRuntimeTraces`, `CamelAwsBedrockAgentRuntimeReturnControl` | out | Agent traces and return-control requests. |

`retrieve` returns a list of results (`{content: {text}, location, score}`) as the body.

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e KNOWLEDGE_BASE_ID \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/rag.camel.yaml
```

## Common questions

### How do I keep a conversation going?

Copy `CamelAwsBedrockAgentRuntimeSessionId` from the first answer into the header of the next
question (for example by keeping it per user in a cache).

### How do I invoke a Bedrock Agent?

Use `operation=invokeAgent&agentId=...&agentAliasId=...`. The agent's event stream is decoded: the
body is its answer; traces and return-control requests (when the agent asks your code to run an
action) are in headers.

### Retrieve only, without generation?

Use the default `operation=retrieve` and pass the passages to any model, for example
[aws-bedrock](aws-bedrock.md) or [azure-openai](azure-openai.md).

## Related

- [AWS Bedrock](aws-bedrock.md), [Azure OpenAI](azure-openai.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Bedrock Agent Runtime](https://camel.apache.org/components/next/aws-bedrock-agent-runtime-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
