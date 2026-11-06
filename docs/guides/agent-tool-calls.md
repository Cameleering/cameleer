# AI agent tool calls: direct vs through Cameleer

> Direct tool calls leave retries, duplicates, half-done work and business rules to the model;
> routing them through Cameleer makes each tool a tested route that validates, runs once, rolls
> back, retries and audits, so the model only decides what to do.

The short version, with the comparison table and FAQ, is on the
[AI agent tool calls: direct vs Cameleer](https://pmsudhi.github.io/cameleer/compare/ai-agent-tool-calls-direct-vs-cameleer/)
page.

Function calling (also called tool use) is how an AI agent acts: the model answers with the
name of a tool and its arguments as JSON, and your code executes it. This page compares two
ways to execute those calls, on a real multi-step job, and shows the complete Cameleer route
for it. The route runs end to end in the test suite
(`crates/cameleer-main/tests/docs_guides.rs`) against mock order, payment, shipping and email
APIs.

![Direct tool calls: an agent with API keys calls the orders, payments, shipping and email APIs itself in four tool calls and five model turns. Through Cameleer: the agent makes one tool call to a route that validates, runs once, checks policy, runs a saga with rollback, retries and audits, then calls the same APIs.](agent-tool-calls.svg)

## The difference in one table

| concern | direct tool calls | through Cameleer |
|---|---|---|
| who runs a multi-step job | the model, one tool call per step, one model turn per call | a route; the model makes one call |
| wrong or invented arguments | reach the API unless your glue code checks them | rejected by a JSON schema with every error listed, so the model can correct itself (`400`) |
| the agent retries a call | the action runs again: a second refund, a second email | `idempotentConsumer` runs each `requestId` once; the repeat gets `duplicate` |
| a step fails after an earlier one succeeded | half-done work the model has to notice and undo | a `saga` calls the compensation (here: cancel the refund) |
| flaky APIs | the model sees the error and may retry, give up or improvise | retries with exponential back-off at the failing step, then a clear answer |
| business rules (amount, limits, approvals) | written in the prompt and trusted to the model | code in the route: the amount comes from the order, large refunds wait for a person |
| credentials | the agent process holds every API key | the route holds them; the agent can only call the tool |
| audit | whatever the agent framework logs | every accepted call appended to an audit log |
| rate limits and outages | per agent, if at all | `throttle` and `circuitBreaker` around any API ([agent cookbook](agents.md#rate-limits-and-a-circuit-breaker-around-a-model-api)) |
| tokens and latency | every step is a model turn that re-sends the conversation and tool results | one tool call, one final turn; the steps run at code speed |
| changing an API or vendor | change the tool, the prompt and maybe re-test the agent | change the route; the tool the model sees stays the same |
| testing | needs the model in the loop | the route is tested without a model (as this page's route is) |

## The use case: a damaged order

A customer writes to a support agent:

> My order EU-4411 arrived broken. I want my money back.

Resolving it takes four systems: read the order (is it delivered, what did it cost, who is the
customer), refund it with the payment provider, create a return label with the carrier, and
email the customer the label. Company rules: only delivered orders, the refund is the order
total and not a number someone typed, and refunds over 200 need a person.

### Direct: the model orchestrates

The agent gets four tools, roughly:

```json
[
  {"name": "get_order", "parameters": {"orderId": "string"}},
  {"name": "issue_refund", "parameters": {"orderId": "string", "amount": "number", "reason": "string"}},
  {"name": "create_return_label", "parameters": {"orderId": "string"}},
  {"name": "send_email", "parameters": {"to": "string", "template": "string", "data": "object"}}
]
```

and a system prompt with the rules. A good run is four tool calls and five model turns:
`get_order`, then `issue_refund` with the total the model read, then `create_return_label`,
then `send_email`, then the reply to the customer. Each turn sends the conversation, the tool
definitions and every tool result so far back to the model.

What goes wrong in production:

- **The refund is issued twice.** The payments API times out after it has refunded; the model,
  or the agent framework, retries `issue_refund`. Nothing in the call says it is the same
  request.
- **Half-done work.** The refund succeeds, then the carrier API is down. The customer has the
  money and no label, and the model has to notice it and decide what to do, every time,
  without a rule.
- **The model sets the amount.** `issue_refund` takes whatever `amount` the model passes: a
  misread total, a currency mix-up, or a customer's "refund me 1,299" injected into the chat.
  The 200 limit is a sentence in the prompt.
- **Every key in one place.** The agent process holds the payment, carrier and email
  credentials, and the tool definitions show the model how to use them.
- **No record.** Unless you add one, there is no log of which tool calls were made with which
  arguments, separate from the model's transcript.

Each of these can be fixed in glue code around every tool, in every agent, in every language
the team uses. That glue code is an integration layer; Cameleer is one.

### Through Cameleer: the model decides, a route executes

The agent gets one tool. Its parameters are the facts only the conversation knows:

```json
{
  "name": "resolve_damaged_order",
  "description": "Refund a delivered order that arrived damaged and send the customer a return label. The amount is taken from the order.",
  "parameters": {
    "type": "object",
    "required": ["requestId", "orderId", "reason"],
    "additionalProperties": false,
    "properties": {
      "requestId": {"type": "string", "pattern": "^[A-Za-z0-9-]{8,64}$"},
      "orderId": {"type": "string", "pattern": "^[A-Z]{2}-[0-9]+$"},
      "reason": {"type": "string", "enum": ["damaged", "missing_parts", "wrong_item"]}
    }
  }
}
```

The same file, [schemas/damaged-order-request.json](../../examples/agents/schemas/damaged-order-request.json),
validates the call in the route, so the tool the model sees and the check that runs can't drift
apart. A good run is one tool call and two model turns. The route does the four steps:

1. **Validate** the arguments against the schema. An invented `amount` or a free-text reason
   gets `400` with every error.
2. **Run once.** `idempotentConsumer` on `requestId`: a retried call answers `duplicate` and
   does nothing.
3. **Read the order** and take the amount and the customer's email from it.
4. **Apply the rules.** Not delivered: `409`. Over `APPROVAL_LIMIT` (200): the request is
   parked as a file for a person and the agent gets `202 pending_approval`, with a sentence it
   can pass on. The [human approval pattern](agents.md#human-in-the-loop-approval) shows the
   Slack message and the approval endpoint.
5. **Refund and label as a saga.** The sub-route `refund-with-label` retries each call twice
   with exponential back-off. If the label still fails, the saga calls `cancel-refund` with the
   `requestId`, and the agent gets `502` with "nothing was changed".
6. **Email and audit** through wire taps, so they don't slow the answer down; the audit log has
   one JSON line per accepted call.

The answer is a small JSON object the model can turn into a reply:
`{"status": "resolved", "refundId": "r-1", "amount": 129.9, "returnLabel": "https://..."}`.

<!-- example: agent-tool-calls-damaged-order -->
```yaml
- rest:
    path: /tools
    post:
      - path: /resolve-damaged-order
        to: direct:resolve-damaged-order
- routeConfiguration:
    id: tool-errors
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
          handled:
            constant: true
          steps:
            - setHeader:
                name: CamelHttpResponseCode
                constant: 502
            - setHeader:
                name: Content-Type
                constant: application/json
            - setBody:
                simple: '{"status": "failed", "requestId": "${header.requestId}", "detail": "a backend did not answer; nothing was changed, retry later with a new requestId"}'
- route:
    id: resolve-damaged-order
    routeConfigurationId: tool-errors
    from:
      uri: direct:resolve-damaged-order
      steps:
        - convertBodyTo:
            type: java.lang.String
        - to: "json-validator:file:{{env:SCHEMA_DIR:schemas}}/damaged-order-request.json"
        - unmarshal:
            json: {}
        - setHeader:
            name: requestId
            simple: "${body[requestId]}"
        - setHeader:
            name: orderId
            simple: "${body[orderId]}"
        - setHeader:
            name: reason
            simple: "${body[reason]}"
        - wireTap:
            uri: direct:audit-order-tool
        - idempotentConsumer:
            header: requestId
            steps:
              - removeHeaders:
                  pattern: "CamelHttp*"
              - toD: "{{env:ORDERS_URL:http://localhost:9001}}/orders/${header.orderId}?httpMethod=GET"
              - unmarshal:
                  json: {}
              - setHeader:
                  name: amount
                  simple: "${body[total]}"
              - setHeader:
                  name: email
                  simple: "${body[email]}"
              - choice:
                  when:
                    - simple: "${body[status]} != 'delivered'"
                      steps:
                        - setHeader:
                            name: CamelHttpResponseCode
                            constant: 409
                        - setBody:
                            simple: '{"status": "rejected", "requestId": "${header.requestId}", "detail": "order ${header.orderId} is ${body[status]}; only delivered orders can be refunded"}'
                    - simple: "${header.amount} > {{env:APPROVAL_LIMIT:200}}"
                      steps:
                        - setBody:
                            simple: '{"requestId": "${header.requestId}", "orderId": "${header.orderId}", "amount": ${header.amount}, "reason": "${header.reason}"}'
                        - to: "file:{{env:PENDING_DIR:pending}}?fileName=${header.requestId}.json"
                        - setHeader:
                            name: CamelHttpResponseCode
                            constant: 202
                        - setBody:
                            simple: '{"status": "pending_approval", "requestId": "${header.requestId}", "detail": "refunds over {{env:APPROVAL_LIMIT:200}} need a person; tell the customer they will hear back within one business day"}'
                  otherwise:
                    steps:
                      - doTry:
                          steps:
                            - to: direct:refund-with-label
                            - wireTap:
                                uri: direct:notify-customer
                            - setHeader:
                                name: CamelHttpResponseCode
                                constant: 200
                            - setBody:
                                simple: '{"status": "resolved", "requestId": "${header.requestId}", "refundId": "${header.refundId}", "amount": ${header.amount}, "returnLabel": "${header.labelUrl}"}'
                          doCatch:
                            - exception:
                                - org.apache.camel.http.base.HttpOperationFailedException
                                - java.io.IOException
                              steps:
                                - setHeader:
                                    name: CamelHttpResponseCode
                                    constant: 502
                                - setBody:
                                    simple: '{"status": "failed", "requestId": "${header.requestId}", "detail": "the return label failed, so the refund was cancelled; nothing was changed. To try again later, call with a new requestId"}'
        - filter:
            simple: "${exchangeProperty.CamelDuplicateMessage} == true"
            steps:
              - setBody:
                  simple: '{"status": "duplicate", "requestId": "${header.requestId}"}'
        - setHeader:
            name: Content-Type
            constant: application/json
- route:
    id: refund-with-label
    errorHandler:
      defaultErrorHandler:
        redeliveryPolicy:
          maximumRedeliveries: 2
          redeliveryDelay: 200
          useExponentialBackOff: true
    from:
      uri: direct:refund-with-label
      steps:
        - saga:
            compensation:
              uri: direct:cancel-refund
            option:
              - key: requestId
                expression:
                  simple: "${header.requestId}"
            steps:
              - setBody:
                  simple: '{"requestId": "${header.requestId}", "orderId": "${header.orderId}", "amount": ${header.amount}, "reason": "${header.reason}"}'
              - enrich:
                  uri: "{{env:PAYMENTS_URL:http://localhost:9002}}/refunds?httpMethod=POST"
              - unmarshal:
                  json: {}
              - setHeader:
                  name: refundId
                  simple: "${body[refundId]}"
              - setBody:
                  simple: '{"orderId": "${header.orderId}", "requestId": "${header.requestId}"}'
              - enrich:
                  uri: "{{env:SHIPPING_URL:http://localhost:9003}}/return-labels?httpMethod=POST"
              - unmarshal:
                  json: {}
              - setHeader:
                  name: labelUrl
                  simple: "${body[labelUrl]}"
- route:
    id: cancel-refund
    from:
      uri: direct:cancel-refund
      steps:
        - removeHeaders:
            pattern: "CamelHttp*"
        - setBody:
            simple: '{"requestId": "${header.requestId}"}'
        - to: "{{env:PAYMENTS_URL:http://localhost:9002}}/refunds/cancel?httpMethod=POST"
- route:
    id: notify-customer
    from:
      uri: direct:notify-customer
      steps:
        - removeHeaders:
            pattern: "CamelHttp*"
        - setBody:
            simple: '{"to": "${header.email}", "template": "refund-issued", "orderId": "${header.orderId}", "amount": ${header.amount}, "returnLabel": "${header.labelUrl}"}'
        - to: "{{env:EMAIL_URL:http://localhost:9004}}/messages?httpMethod=POST"
- route:
    id: audit-order-tool
    from:
      uri: direct:audit-order-tool
      steps:
        - marshal:
            json: {}
        - setBody:
            simple: "${body}\n"
        - to: "file:{{env:AUDIT_DIR:audit}}?fileName=tool-calls.jsonl&fileExist=Append"
```

Three details:

- The refund and the label are in their own route because the error handling has to be in the
  right place. A `handled` `onException` around the saga would clear the error before the saga
  sees it, so nothing would be compensated (Apache Camel behaves the same way). Inside
  `refund-with-label`, the default error handler retries and then lets the error through the
  saga, which compensates; the `doTry` in the main route turns it into the `502` answer.
- A failed call is still an answered call, so its `requestId` is used up, and the `502` tells
  the agent to retry with a new one. The other answers are final, so a repeat is correctly a
  `duplicate`.
- `removeHeaders: CamelHttp*` comes before the backend calls, because the incoming request's
  HTTP headers would otherwise change where the `http` producer sends them, as in Apache Camel.
  The calls with side effects use `enrich`, so a retry sends the original request again (see
  [the tool pattern](agents.md#expose-a-tool-to-an-agent-over-rest)).

### What the test checks

| the agent sends | the route answers | what happened |
|---|---|---|
| `EU-4411`, `damaged` | `200 resolved`, amount 129.9 from the order | payments failed once with `503` and was retried; label created; customer emailed |
| the same call again | `200 duplicate` | nothing executed a second time |
| `orderId: "4411"`, a free-text reason and `amount: 1299` | `400` naming `orderId`, `reason` and `amount` | nothing executed, nothing audited |
| `EU-1200`, not yet delivered | `409 rejected` | nothing executed |
| `EU-9000`, total 899 | `202 pending_approval` | parked for a person, no refund |
| `EU-5000`, carrier down | `502 failed` | refund issued once, label tried three times, refund cancelled, no email |

## When a direct call is enough

Not every tool needs a route. Call the API directly when the call:

- only reads, such as a search or a lookup, and a repeat does no harm;
- is a single step with no other system to keep consistent;
- is a prototype, and you will move the tool behind a route before real users touch it.

Use a route as soon as a call changes something (money, stock, messages, tickets), spans
several systems, needs a rule the model must not override, or must be audited. Read-only
tools can still go through Cameleer to hide credentials and add limits.

## Cameleer's advantages for agent tools

- **Fewer model turns.** A multi-step job is one tool call instead of one call per step, which
  cuts tokens, latency and the chances for the model to go off script.
- **Reliability is code, not prompt.** Idempotency, retries, back-off, sagas, throttling and
  circuit breakers are Enterprise Integration Patterns, configured in a few lines and tested
  without a model.
- **Rules the model can't override.** Amounts, limits and approvals are decided in the route
  from system data; the model only supplies what it learned in the conversation.
- **Least privilege.** API keys stay in the route's environment; the agent can call the tool and
  nothing else.
- **One audit trail** for every agent, framework and model.
- **230 components.** The same route style reaches databases, queues, SaaS APIs, files and
  cloud services ([components](../components/)), and the routes run as a small sidecar or
  inside a Rust agent with `cameleer_main::new_main()`.
- **Model and framework neutral.** A tool is an HTTP endpoint with a JSON schema, so it works
  with any model provider and agent framework that supports function calling.

## Common questions

### Is Cameleer an AI agent framework?

No. The agent framework runs the model loop and decides which tool to call. Cameleer executes
the tool: it is the integration layer between the agent and your systems.

### Does routing tool calls through Cameleer replace function calling?

No. The model still uses function calling; the tool it calls is a Cameleer route behind an
HTTP endpoint, declared with the same JSON schema the route validates against.

### Which models and agent frameworks does it work with?

Any that support function calling or tool use and can make an HTTP request: OpenAI,
Anthropic, Google, AWS Bedrock and Azure OpenAI models, open models on Ollama or vLLM, and
frameworks that let you define a tool as an HTTP call. An MCP server is on the
[roadmap](../ROADMAP.md).

### What happens when an agent retries a tool call?

The route sees the same `requestId` and answers `duplicate` without executing anything. Keep the
ids across restarts and instances with the JDBC or Redis idempotent repository.

### Can the agent still handle a multi-step job itself?

Yes, and for open-ended tasks it should. Put a step behind a route when its order and rules are
known in advance, as in a refund: the model decides that the customer gets a refund, the route
decides how.

### Does an extra hop make tool calls slower?

A route adds microseconds to a call; a model turn takes hundreds of milliseconds to seconds. A
multi-step job through one route saves model turns, so it is usually faster end to end.

## Related

- [Cameleer for AI agents](agents.md): eight tested patterns, including the single-step
  [tool over REST](agents.md#expose-a-tool-to-an-agent-over-rest) and
  [human approval](agents.md#human-in-the-loop-approval)
- [Error handling](error-handling.md), [EIPs](eips.md), [AI components](ai-components.md)
- The route file: [examples/agents/damaged-order-tool.camel.yaml](../../examples/agents/damaged-order-tool.camel.yaml)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
