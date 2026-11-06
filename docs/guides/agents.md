# Cameleer for AI agents

> Cameleer is the action layer for AI agents: the model decides what, a tested route decides
> how. It feeds events to models, routes on their answers, and turns agent tool calls into
> reliable actions with validation, idempotency, retries, sagas, human approval, rate limits,
> fallbacks and an audit log.

An agent decides what to do; a route does it. This cookbook shows eight patterns, each a
complete route file in [`examples/agents/`](../../examples/agents/) that runs end to end in the
test suite (`crates/cameleer-main/tests/docs_guides.rs`) against an OpenAI-compatible mock, the
embedded Kafka broker, an Elasticsearch emulator and Slack and payment API mocks.

The routes use the [`openai` component](../components/openai.md), which works with OpenAI and
every OpenAI-compatible server: Azure OpenAI gateways, Ollama, vLLM, LM Studio and LiteLLM.
Point it at another server with `OPENAI_BASE_URL` and `OPENAI_MODEL`. The component reads the
API key from the `OPENAI_API_KEY` environment variable, so the key never appears in a route.
AWS Bedrock (`aws-bedrock`), Azure OpenAI (`azure-openai`) and Google Vertex AI
(`google-vertexai`) work the same way in these patterns; see [AI components](ai-components.md).

| pattern | file | what it shows |
|---|---|---|
| [Classify and route](#classify-with-an-llm-and-route-on-the-answer) | `llm-classify-route.camel.yaml` | an LLM label drives a content-based router, with a fixed list of trusted labels |
| [Summarize](#summarization-pipeline) | `llm-summarize-pipeline.camel.yaml` | map-reduce summaries of long documents |
| [RAG ingestion](#rag-ingestion-chunk-embed-and-store) | `rag-ingest.camel.yaml` | chunk, embed and index documents in Elasticsearch |
| [Tools over REST](#expose-a-tool-to-an-agent-over-rest) | `agent-tool-api.camel.yaml` | schema validation, idempotency, retries and an audit log for agent tool calls |
| [Human approval](#human-in-the-loop-approval) | `human-approval.camel.yaml` | park risky actions until a person approves them |
| [Guard LLM output](#guard-llm-output-with-a-json-schema) | `llm-output-guard.camel.yaml` | validate model JSON against a schema, re-ask once |
| [Rate limits and breakers](#rate-limits-and-a-circuit-breaker-around-a-model-api) | `llm-rate-limit-breaker.camel.yaml` | throttle and circuit breaker around a model API |
| [Multi-model fallback](#multi-model-fallback) | `llm-multi-model-fallback.camel.yaml` | fail over from a hosted model to a local one |

## Classify with an LLM and route on the answer

Tickets arrive over HTTP. The model labels each one, and a `choice` sends the original ticket
to a Kafka topic per label. Two details make this safe to run:

- The ticket is kept in an exchange property and put back after the model call, so the
  topic receives the ticket, not the model's answer.
- The router compares the label against a fixed list. Anything else, including a ticket that
  tries "ignore previous instructions", goes to the general topic. Never use the model's
  answer directly in `toD` or as a topic name.

<!-- example: agents-classify-route -->
```yaml
- route:
    id: classify-ticket
    from:
      uri: "platform-http:/tickets?httpMethodRestrict=POST"
      steps:
        - convertBodyTo:
            type: java.lang.String
        - setProperty:
            name: ticket
            simple: "${body}"
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&temperature=0&systemMessage=Label the support ticket with exactly one word. billing for payments and invoices, technical for errors and outages, other for everything else."
        - setHeader:
            name: category
            simple: "${body.trim().toLowerCase()}"
        - setBody:
            simple: "${exchangeProperty.ticket}"
        - choice:
            when:
              - simple: "${header.category} == 'billing'"
                steps:
                  - to: "kafka:tickets-billing?brokers={{env:KAFKA_BROKERS:localhost:9092}}"
              - simple: "${header.category} == 'technical'"
                steps:
                  - to: "kafka:tickets-technical?brokers={{env:KAFKA_BROKERS:localhost:9092}}"
            otherwise:
              steps:
                - setHeader:
                    name: category
                    constant: other
                - to: "kafka:tickets-general?brokers={{env:KAFKA_BROKERS:localhost:9092}}"
        - setHeader:
            name: CamelHttpResponseCode
            constant: 202
        - setHeader:
            name: Content-Type
            constant: application/json
        - setBody:
            simple: '{"category": "${header.category}"}'
```

The test sends three tickets: an invoice question goes to `tickets-billing`, an error report to
`tickets-technical`, and a prompt-injection attempt (the model answers with an invented label)
to `tickets-general`.

```sh
docker run --rm --network host -e KAFKA_BROKERS=localhost:9092 -e OPENAI_API_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/llm-classify-route.camel.yaml
```

To classify events from Kafka instead of HTTP, replace the `from` URI with
`kafka:support-tickets?brokers=...&groupId=triage`, as in
[ai-ticket-triage.camel.yaml](../../examples/ai-ticket-triage.camel.yaml).

## Summarization pipeline

Long documents do not fit in one prompt, and one huge prompt is slow and expensive. The route
splits each document into paragraphs, summarizes each paragraph, and asks the model to combine
the partial summaries. The split's `GroupedBodyAggregationStrategy` collects the summaries into
a list, and a list body is sent to the `openai` component as one user message per item.

<!-- example: agents-summarize-pipeline -->
```yaml
- route:
    id: summarize-documents
    from:
      uri: "file:{{env:INBOX_DIR:inbox}}?include=.*\\.txt&move=.done&moveFailed=.failed"
      steps:
        - convertBodyTo:
            type: java.lang.String
        - split:
            tokenize: "\n\n"
            aggregationStrategy: "#class:org.apache.camel.processor.aggregate.GroupedBodyAggregationStrategy"
            steps:
              - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&temperature=0&systemMessage=Summarize this paragraph in one sentence."
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&temperature=0&systemMessage=The messages are summaries of consecutive paragraphs of one document. Combine them into a summary of at most five sentences."
        - to: "file:{{env:OUTBOX_DIR:outbox}}?fileName=${file:onlyname.noext}.summary.txt"
        - log: "summarized ${file:name} (${header.CamelOpenAITotalTokens} tokens for the last call)"
```

Drop `report.txt` into the inbox and `report.summary.txt` appears in the outbox. The test checks
that a three-paragraph document makes four model calls (three paragraphs, one combine). Add
`parallelProcessing: true` to the split to summarize paragraphs concurrently, and a
[throttle](eips/flow-control.md#throttle) to stay under the provider's rate limit.

## RAG ingestion: chunk, embed and store

Retrieval-augmented generation needs an index of your documents. This route watches a folder,
splits each document into paragraph chunks, embeds each chunk with `openai:embeddings`, and
indexes `{source, chunk, text, embedding}` in Elasticsearch or OpenSearch under the id
`<file>-<chunk>`, so re-dropping a changed file overwrites its chunks.

<!-- example: agents-rag-ingest -->
```yaml
- route:
    id: rag-ingest
    from:
      uri: "file:{{env:INBOX_DIR:docs}}?include=.*\\.(md|txt)&move=.indexed&moveFailed=.failed"
      steps:
        - convertBodyTo:
            type: java.lang.String
        - setHeader:
            name: docId
            simple: "${file:onlyname.noext}"
        - split:
            tokenize: "\n\n"
            steps:
              - filter:
                  simple: "${body.trim()} != ''"
                  steps:
                    - setProperty:
                        name: chunk
                        simple: "${body.trim()}"
                    - to: "openai:embeddings?model={{env:EMBEDDING_MODEL:text-embedding-3-small}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}"
                    - setHeader:
                        name: indexId
                        simple: "${header.docId}-${exchangeProperty.CamelSplitIndex}"
                    - setBody:
                        jq: '{source: header("CamelFileName"), chunk: property("CamelSplitIndex"), text: property("chunk"), embedding: .}'
                    - to: "elasticsearch://rag?hostAddresses={{env:ES_HOSTS:localhost:9200}}&indexName={{env:ES_INDEX:docs}}&operation=Index"
        - log: "indexed ${file:name}"
```

The `jq` step builds the document: `.` is the embedding vector returned by the model, and
`header()` and `property()` read the file name and the chunk text. Empty chunks are skipped
by the filter. The same shape works for other sources (`aws2-s3`, `google-drive`, `sftp`, `mail`)
and other stores: PostgreSQL with pgvector through the [`sql` component](../components/sql.md),
or a vector database's REST API through [`http`](../components/http.md). Create the index with a
`dense_vector` (Elasticsearch) or `knn_vector` (OpenSearch) mapping before you ingest.

## Publish routes as tools with `ai-tool`

A route that consumes from [`ai-tool:name`](../components/ai-tool.md) is a tool. Its
arguments arrive as headers and its body is the result. Every call goes through the same
envelope, whoever makes it:

- the arguments are checked against the tool's JSON Schema, and a bad call gets an error that
  names each field;
- a tool with side effects is deduplicated, so a retried call runs once;
- a destructive tool waits for approval;
- a slow call is cancelled after `timeout`;
- every call goes into a hash-chained audit log.

<!-- example: agents-ai-tool-refund -->
```yaml
- route:
    from:
      uri: ai-tool:refund
      parameters:
        description: Refund part or all of an order
        parameter.orderId: string
        parameter.orderId.required: true
        parameter.amount: number
        parameter.amount.required: true
        approval: "${header.amount} > 100"     # small refunds run, large ones wait for a person
        approvalEndpoint: direct:approvals
        compensation: direct:reverse-refund
      steps:
        - setBody:
            simple: "Refunded ${header.amount} on order ${header.orderId}"
- route:
    from:
      uri: direct:approvals
      steps:
        - to: log:approvals
- route:
    from:
      uri: direct:reverse-refund
      steps:
        - to: log:reversed
```

Routes call tools with `to: ai-tool:refund`, and Rust code with
`cameleer_ai::AiRegistry::of(&ctx)?.call(...)`, and MCP clients through the
[MCP server](mcp-server.md): `cameleer run tools.camel.yaml --mcp` (HTTP) or `--mcp-stdio`. Read-only context goes in
[`ai-resource`](../components/ai-resource.md) routes, and prompt templates in
[`ai-prompt`](../components/ai-prompt.md) routes. A complete file is
[examples/agents/ai-tools.camel.yaml](../../examples/agents/ai-tools.camel.yaml).

## Expose a tool to an agent over REST

Agents call tools; a tool is a route behind an HTTP endpoint. This refund tool shows the four
things every side-effecting tool needs:

1. **Validated arguments.** `json-validator` checks the call against
   [schemas/refund-request.json](../../examples/agents/schemas/refund-request.json), the same
   schema you give the agent in its tool definition. A bad call gets `400` with every error, so
   the agent can correct itself.
2. **Idempotency.** Agents retry. `idempotentConsumer` on the `requestId` argument makes sure a
   repeated call does not refund twice; the repeat gets `{"status": "duplicate"}`.
3. **Retries.** The payments call is retried three times with exponential back-off on HTTP
   errors and I/O errors, then answered with `502`.
4. **An audit log.** A wire tap appends every accepted call to `tool-calls.jsonl` without slowing
   the call down.

<!-- example: agents-tool-api -->
```yaml
- rest:
    path: /tools
    post:
      - path: /refund
        to: direct:refund-tool
- routeConfiguration:
    onException:
      - onException:
          exception:
            - org.apache.camel.component.jsonvalidator.JsonValidationException
          handled:
            constant: true
          steps:
            - setHeader:
                name: CamelHttpResponseCode
                constant: 400
            - setBody:
                simple: "invalid tool arguments: ${exception.message}"
      - onException:
          exception:
            - org.apache.camel.http.base.HttpOperationFailedException
            - java.io.IOException
          redeliveryPolicy:
            maximumRedeliveries: 3
            redeliveryDelay: 200
            useExponentialBackOff: true
          handled:
            constant: true
          steps:
            - setHeader:
                name: CamelHttpResponseCode
                constant: 502
            - setBody:
                simple: "payments API unavailable, the refund was not confirmed; retry later with the same requestId"
- route:
    id: refund-tool
    from:
      uri: direct:refund-tool
      steps:
        - convertBodyTo:
            type: java.lang.String
        - to: "json-validator:file:{{env:SCHEMA_DIR:schemas}}/refund-request.json"
        - unmarshal:
            json: {}
        - setHeader:
            name: requestId
            simple: "${body[requestId]}"
        - wireTap:
            uri: direct:audit
        - idempotentConsumer:
            header: requestId
            steps:
              - removeHeaders:
                  pattern: "CamelHttp*"
              - marshal:
                  json: {}
              - enrich:
                  uri: "{{env:PAYMENTS_URL:http://localhost:9000}}/refunds?httpMethod=POST"
              - setBody:
                  simple: '{"status": "refunded", "requestId": "${header.requestId}", "payment": ${body}}'
        - filter:
            simple: "${exchangeProperty.CamelDuplicateMessage} == true"
            steps:
              - setBody:
                  simple: '{"status": "duplicate", "requestId": "${header.requestId}"}'
        - setHeader:
            name: Content-Type
            constant: application/json
- route:
    id: audit-tool-calls
    from:
      uri: direct:audit
      steps:
        - marshal:
            json: {}
        - setBody:
            simple: "${body}\n"
        - to: "file:{{env:AUDIT_DIR:audit}}?fileName=tool-calls.jsonl&fileExist=Append"
```

Two details in the payments call:

- `removeHeaders: CamelHttp*` comes first, because the incoming request's `CamelHttpUri` and
  `CamelHttpPath` headers would otherwise change where the `http` producer sends the request,
  as in Apache Camel.
- The call is an `enrich`, not a `to`. `enrich` sends a copy of the message, so a retry sends
  the original request again. With `to`, a retry after an HTTP error would send the error
  response instead, because Cameleer's `http` producer puts the response into the message
  before it fails (Apache Camel leaves the message unchanged). The reply of a successful call
  replaces the body, as with `to`.

The test makes the payments mock fail the first call with `503`, and checks the retry, the
duplicate, the `400` for a call without `orderId` and with an amount over the schema's maximum,
and the two audit lines.

For a tool that spans several systems (refund, return label, email) in one call, with a saga
that rolls back half-done work, see
[AI agent tool calls: direct vs through Cameleer](agent-tool-calls.md).

For one endpoint per tool without one route per tool, use `toD: direct:tool-${header.name}` on
a path parameter, with a fixed set of `direct:` routes behind it. A packaged MCP server is on the
[roadmap](../ROADMAP.md).

## Human-in-the-loop approval

Some actions should not run on the model's word alone. Here refunds up to `APPROVAL_LIMIT` run at
once. Larger ones are written to a `pending` folder, and a Slack message asks a person to
approve. The approval endpoint takes the parked action with `pollEnrich` (deleting the file) and
executes it, so an action runs at most once even if the link is clicked twice.

<!-- example: agents-human-approval -->
```yaml
- rest:
    post:
      - path: /actions
        to: direct:propose
      - path: /approvals/{id}
        to: direct:approve
- route:
    id: propose-action
    from:
      uri: direct:propose
      steps:
        - convertBodyTo:
            type: java.lang.String
        - setProperty:
            name: action
            simple: "${body}"
        - unmarshal:
            json: {}
        - setHeader:
            name: actionId
            simple: "${body[actionId]}"
        - choice:
            when:
              - simple: "${body[amount]} <= {{env:APPROVAL_LIMIT:100}}"
                steps:
                  - setBody:
                      simple: "${exchangeProperty.action}"
                  - to: direct:execute
                  - setBody:
                      simple: '{"status": "executed", "actionId": "${header.actionId}"}'
            otherwise:
              steps:
                - setBody:
                    simple: "${exchangeProperty.action}"
                - to: "file:{{env:PENDING_DIR:pending}}?fileName=${header.actionId}.json"
                - setBody:
                    simple: "Agent wants to refund ${jsonpath($.amount)} on order ${jsonpath($.orderId)}. Approve: {{env:PUBLIC_URL:http://localhost:8080}}/approvals/${header.actionId}"
                - to: "slack:#approvals?webhookUrl={{env:SLACK_WEBHOOK_URL}}"
                - setHeader:
                    name: CamelHttpResponseCode
                    constant: 202
                - setBody:
                    simple: '{"status": "pending_approval", "actionId": "${header.actionId}"}'
- route:
    id: approve-action
    from:
      uri: direct:approve
      steps:
        - setProperty:
            name: actionId
            simple: "${header.id}"
        - pollEnrich:
            uri: "file:{{env:PENDING_DIR:pending}}?fileName=${header.id}.json&delete=true"
            timeout: 1000
        - choice:
            when:
              - simple: "${body} == null"
                steps:
                  - setHeader:
                      name: CamelHttpResponseCode
                      constant: 404
                  - setBody:
                      simple: "no pending action ${exchangeProperty.actionId}"
            otherwise:
              steps:
                - to: direct:execute
                - setBody:
                    simple: '{"status": "executed", "actionId": "${exchangeProperty.actionId}"}'
- route:
    id: execute-action
    from:
      uri: direct:execute
      steps:
        - removeHeaders:
            pattern: "CamelHttp*"
        - to: "{{env:PAYMENTS_URL:http://localhost:9000}}/refunds?httpMethod=POST"
```

The test proposes a 40 refund (executed at once) and a 350 refund (parked, Slack message with
the approval link), approves it, checks that the payments API got it, and checks that a second
approval answers `404`. Protect `/approvals` with your SSO or API gateway: anyone who can call it
can approve. To approve from Slack buttons instead of links, point a Slack interactivity URL at
a `platform-http` route that parses the payload.

## Guard LLM output with a JSON schema

When a model's answer feeds another system, check it first. The route asks for JSON, validates
it with `json-validator` against
[schemas/invoice.json](../../examples/agents/schemas/invoice.json), and on failure asks once
more, passing the validation errors back to the model. A second failure becomes `422`, so bad
data never reaches the caller.

<!-- example: agents-output-guard -->
```yaml
- route:
    id: extract-invoice
    from:
      uri: "platform-http:/extract?httpMethodRestrict=POST"
      steps:
        - convertBodyTo:
            type: java.lang.String
        - setProperty:
            name: document
            simple: "${body}"
        - doTry:
            steps:
              - to: direct:ask-for-json
            doCatch:
              - exception:
                  - org.apache.camel.ValidationException
                steps:
                  - setBody:
                      simple: "Your previous answer was rejected. ${exception.message}. Reply with corrected JSON only. Document follows. ${exchangeProperty.document}"
                  - doTry:
                      steps:
                        - to: direct:ask-for-json
                      doCatch:
                        - exception:
                            - org.apache.camel.ValidationException
                          steps:
                            - setHeader:
                                name: CamelHttpResponseCode
                                constant: 422
                            - setBody:
                                simple: "the model did not return a valid invoice: ${exception.message}"
        - setHeader:
            name: Content-Type
            constant: application/json
- route:
    id: ask-for-json
    from:
      uri: direct:ask-for-json
      steps:
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&temperature=0&systemMessage=Extract the invoice as a JSON object with the fields vendor (string), total (number) and currency (ISO 4217 code). Reply with the JSON object only."
        - to: "json-validator:file:{{env:SCHEMA_DIR:schemas}}/invoice.json"
```

The outer `doTry` catches the first `ValidationException` (an answer that is not JSON at all is
a `JsonValidationException`, a subkind of it) and asks again with the errors; the inner `doTry`
turns a second failure into `422`. Both attempts call the same `direct:ask-for-json` route. The
test covers all three outcomes: valid at once, repaired on the second call, and rejected with
`422`.

## Rate limits and a circuit breaker around a model API

Model APIs have rate limits and outages. The `throttle` keeps calls under five per second
(others wait, instead of getting `429` from the provider). The `circuitBreaker` counts failures;
when half of the last ten calls failed, it stops calling the API for 30 seconds and answers
from `onFallback` with `503` and `Retry-After`.

<!-- example: agents-rate-limit-breaker -->
```yaml
- route:
    id: ask-model
    from:
      uri: "platform-http:/ask?httpMethodRestrict=POST"
      steps:
        - convertBodyTo:
            type: java.lang.String
        - throttle:
            constant: 5
            timePeriodMillis: 1000
        - circuitBreaker:
            resilience4jConfiguration:
              slidingWindowSize: 10
              minimumNumberOfCalls: 5
              failureRateThreshold: 50
              waitDurationInOpenState: 30
              timeoutEnabled: true
              timeoutDuration: 60000
            steps:
              - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&maxTokens=300&systemMessage=Answer in at most three sentences."
            onFallback:
              steps:
                - setHeader:
                    name: CamelHttpResponseCode
                    constant: 503
                - setHeader:
                    name: Retry-After
                    constant: 30
                - setBody:
                    constant: "The assistant is unavailable right now. Please retry in 30 seconds."
```

The test runs it twice: against the model mock (six calls take at least a second because of
the throttle), and against an API that is down (every call gets the fallback `503` at once).
For a limit per tenant or per API key, add `correlationExpression: {header: tenant}` to the
throttle.

## Multi-model fallback

Use a hosted model first and a local one when it fails. A failover load balancer tries the
primary endpoint and moves to the fallback on any exception: a connection error, a timeout, or
an HTTP error such as `429` or `401` (the `openai` component fails with
`HttpOperationFailedException` on API errors).

<!-- example: agents-multi-model-fallback -->
```yaml
- route:
    id: chat-with-fallback
    from:
      uri: "platform-http:/chat?httpMethodRestrict=POST"
      steps:
        - convertBodyTo:
            type: java.lang.String
        - loadBalance:
            failover:
              maximumFailoverAttempts: 1
            steps:
              - to: "openai:chat-completion?model={{env:PRIMARY_MODEL:gpt-4o-mini}}&baseUrl={{env:PRIMARY_BASE_URL:https://api.openai.com/v1}}&timeout=30s"
              - to: "openai:chat-completion?model={{env:FALLBACK_MODEL:llama3.1}}&baseUrl={{env:FALLBACK_BASE_URL:http://localhost:11434/v1}}&timeout=60s"
        - setHeader:
            name: X-Answered-By
            simple: "${header.CamelOpenAIResponseModel}"
```

The `X-Answered-By` header comes from `CamelOpenAIResponseModel`, the model name the API
reported. The test checks both paths: primary down (the local model answers) and primary up
(the fallback is never called). To prefer a cheaper model and fall back to a stronger one, swap
the two endpoints; to spread load instead, use `roundRobin` (see
[load balancer](eips/routing.md#load-balancer)).

## Patterns that combine well

- Put a [`filter`](eips/routing.md#message-filter) and an
  [`idempotentConsumer`](eips/flow-control.md#idempotent-consumer) in front of model calls: spam
  and duplicates cost tokens too.
- [`wireTap`](eips/routing.md#wire-tap) every prompt and answer to Kafka or a file for an audit
  trail and evaluation data.
- Use [`enrich`](eips/transformation.md#content-enricher-enrich) to add customer data from SQL
  or a CRM to the prompt.
- Run the routes as a sidecar that starts in milliseconds next to your agent (Docker image `pmsudhi/cameleer`) or embed
  them in a Rust agent with `cameleer_main::new_main()`.

## Common questions

### How do I call an LLM from a Cameleer route?

Send the prompt as the body to `openai:chat-completion?model=...`; the body becomes the
model's answer and the token counts are in `CamelOpenAI*Tokens` headers. Set `baseUrl` for
Ollama, vLLM or another OpenAI-compatible server, or use `aws-bedrock`, `azure-openai` or
`google-vertexai`.

### Can an AI agent call Cameleer routes as tools?

Yes. A route that consumes from `ai-tool:name` is a tool, with argument checks, deduplication,
approvals, timeouts and an audit log built in (see
[Publish routes as tools](#publish-routes-as-tools-with-ai-tool)), and the
[MCP server](mcp-server.md) serves them to MCP clients. Agents without MCP can call a REST
endpoint in front of the tools, as in the [tool pattern](#expose-a-tool-to-an-agent-over-rest).

### How do I stop an agent from executing the same action twice?

With `ai-tool` this is the default for tools that are neither read-only nor idempotent: a
call with the same call id (or the same declared `idempotency_key`) returns the recorded result
and the route does not run again. In a plain route, have the agent send a unique request id
with every tool call and wrap the side effect in `idempotentConsumer` on that id. Use the JDBC or Redis idempotent repository to keep the ids
across restarts and instances.

### How do I test routes that call an LLM without paying for tokens?

Point `baseUrl` at a mock. `cameleer_saas::testing::OpenAiMock` is an OpenAI-compatible mock
for chat and embeddings, and the tests of this guide use it.

### Does Cameleer run models or store vectors?

No. It calls the models and moves embeddings into the stores you already run (Elasticsearch,
OpenSearch, PostgreSQL with pgvector, or any store with an HTTP API).

## Related

- [AI agent tool calls: direct vs through Cameleer](agent-tool-calls.md): a multi-step tool with a saga, compared with letting the model call each API
- [AI components](ai-components.md), [openai component](../components/openai.md)
- [ai-tool](../components/ai-tool.md), [ai-resource](../components/ai-resource.md), [ai-prompt](../components/ai-prompt.md)
- [EIPs](eips.md), [error handling](error-handling.md), [edge cookbook](edge.md)
- [Roadmap: agent tool server and agent loop](../ROADMAP.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
