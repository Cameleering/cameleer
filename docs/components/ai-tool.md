# AI Tool component

> The `ai-tool` component turns a route into a tool an AI agent can call. Every call goes through a reliability envelope: the arguments are checked against a JSON Schema, a retried call runs once, destructive calls wait for approval, a slow call is cancelled, and each call is written to a tamper-evident audit log.

**Scheme:** `ai-tool:` · **Crate:** `cameleer-ai` · **Cargo feature:** [`ai`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes (Cameleer extension) · **Apache Camel:** [AI Tool](https://camel.apache.org/components/next/ai-tool-component.html)

## When to use it

- Give an agent an action it can take in your systems (issue a refund, create a ticket, book a
  slot) and enforce the rules in the route, not in the prompt.
- Make an existing integration route callable by a model, with the same error handling,
  retries and connectors it already has.
- Keep a record of what an agent did, for whom, and with which approval.

## URI format

```text
ai-tool:name[?options]
```

`name` is the tool's name as the model sees it. A route that consumes from `ai-tool:name`
publishes the tool when it starts and withdraws it when it stops; two routes can't publish the
same name. A producer (`to: ai-tool:name`) calls the tool through the envelope, which is how
routes and tests call tools.

## Example

A refund tool. The agent sends `{"orderId": "EU-4411", "amount": 25.5}`. The arguments arrive
as headers, and the route's body becomes the result.

<!-- example: ai-tool-refund -->
```yaml
- route:
    from:
      uri: ai-tool:refund
      parameters:
        description: Refund part or all of an order
        parameter.orderId: string
        parameter.orderId.description: The order to refund
        parameter.orderId.required: true
        parameter.amount: number
        parameter.amount.required: true
        destructiveHint: true                      # waits for approval (approval=auto)
        approvalEndpoint: direct:ask-a-human       # who is asked
        compensation: direct:reverse-refund        # undo it if the conversation fails later
        timeout: 10s
      steps:
        - toD: "https://payments.internal.example.com/orders/${header.orderId}/refund?amount=${header.amount}"
        - setBody:
            simple: "Refunded ${header.amount} on order ${header.orderId}"
- route:
    from:
      uri: direct:ask-a-human
      steps:
        - to: log:approvals   # or slack, e-mail, a ticket queue
```

The same tool in the Rust DSL, called with a retry:

```rust
use cameleer_ai::{AiRegistry, ToolCall};
use cameleer_core::prelude::*;

let ctx = CamelContext::new();
cameleer_ai::register(&ctx);
ctx.add_routes(|b| {
    b.from("ai-tool:weather?description=Weather for a city&parameter.city=string&parameter.city.required=true&readOnlyHint=true")
        .set_body(simple("${header.city}: 21 degrees"));
})?;
ctx.start()?;
let tools = AiRegistry::of(&ctx)?;
let result = tools.call(ToolCall::new("weather", cameleer_core::json::parse(r#"{"city":"Oslo"}"#)?).with_call_id("call-1"));
assert_eq!(result.text, "Oslo: 21 degrees");
```

## Options

### Definition (Camel names)

| option | default | description |
|---|---|---|
| `description` | | What the tool does; the model reads it to decide when to call the tool |
| `title` | | A human-readable name |
| `tags` | | Comma-separated tags; an agent or server can offer only the tools with given tags |
| `parameter.NAME` | `string` | Declares an argument and its type: `string`, `integer`, `number`, `boolean`, `object`, `array` |
| `parameter.NAME.description` | | The argument's description |
| `parameter.NAME.required` | `false` | Whether the argument is required |
| `parameter.NAME.enum` | | Comma-separated allowed values |
| `argSchema` | | A full JSON Schema for the arguments, inline or `file:PATH`, instead of `parameter.*` (never fetched over the network) |
| `outputSchema` | | A JSON Schema for the structured result, inline or `file:PATH` |
| `outputParameter.NAME` | | Declares a field of the result, like `parameter.NAME` |
| `argumentsAsBody` | `false` | Also pass the arguments as the body, as a JSON string |
| `readOnlyHint` | | The tool changes nothing |
| `destructiveHint` | | The tool may destroy or overwrite something |
| `idempotentHint` | | Calling it twice with the same arguments has the same effect as once |
| `openWorldHint` | | The tool reaches outside systems |
| `searchable` | `true` | Whether tool search may return it |
| `returnDirect` | `false` | The result goes straight to the user, not back to the model |
| `authorizationPolicy` | | A bean (`#name`) implementing `ToolAuthorizationPolicy` that decides per call and principal |

### Reliability envelope (Cameleer extensions)

In Camel the hints are advisory. Here they set the defaults of an envelope that every caller
goes through.

| option | default | description |
|---|---|---|
| `idempotency` | `auto` | `auto` deduplicates tools that are neither `readOnlyHint` nor `idempotentHint`; `true` or `false` forces it |
| `idempotencyWindow` | `1h` | How long a call's result is remembered |
| `idempotentRepository` | in memory | A bean (`#name`, an `IdempotentRepository`) that records keys across processes |
| `approval` | `auto` | `auto` asks for approval when `destructiveHint=true`; `always`; `never`; or a Simple predicate over the arguments, such as `${header.amount} > 500` |
| `approvalEndpoint` | | An endpoint told about each call that waits (arguments as headers, `CamelAiToolApprovalHandle`) |
| `approvalTimeout` | `1h` | How long a call waits for approval |
| `timeout` | `20s` | How long a caller waits; then the call's route is cancelled. `0` runs on the caller's thread with no limit |
| `rateLimit` | | Calls allowed per principal in `rateLimitPeriod` |
| `rateLimitPeriod` | `1m` | The rate limit's period |
| `circuitBreakerThreshold` | `0` (off) | Consecutive failures that open the breaker |
| `circuitBreakerDelay` | `30s` | How long the breaker stays open before one trial call |
| `circuitBreakerFallback` | | The text the model gets while the breaker is open |
| `compensation` | | An endpoint (usually `direct:`) that undoes the call; see [Compensation](#compensation) |
| `audit` | `true` | Write this tool's calls to the audit log |
| `version` | `1` | The definition's version, for pinning |
| `pin` | `true` | Refuse a changed definition that keeps its version |
| `throwOnError` | `false` | Producer: raise `ToolCallException` instead of returning an error result |

## Message headers

| header | direction | description |
|---|---|---|
| each argument, by name | in (consumer) | The call's arguments; `camel*` and undeclared arguments are dropped |
| `CamelAiToolCallId` | in | The model's or protocol's id for the call |
| `CamelAiToolConversationId` | in | The conversation or saga the call belongs to |
| `CamelAiToolName` | in | The tool's name |
| `CamelAiToolPrincipal` | in | Who the call is made for |
| `CamelAiToolApprovedBy` | in (producer) | Marks the call as approved by this approver |
| `CamelAiToolStatus` | out (producer) | `completed`, `error`, `approval-required` or `denied` |
| `CamelAiToolIsError` | out (producer) | `true` when the call failed or was refused |
| `CamelAiToolDuplicate` | out (producer) | `true` when the recorded result of an earlier call was returned |
| `CamelAiToolApprovalHandle` | out (producer) | The handle of a call that waits for approval |
| `CamelAiToolReturnDirect` | out (producer) | The tool's `returnDirect` flag |
| `CamelAiToolStructuredContent` | out (producer) | The structured result (an object or array) |
| `CamelAiToolContent` | out (tool route) | Set by the route: MCP content blocks for the result (images, audio, embedded resources) |
| `CamelMcpSecurityPrincipal`, `CamelMcpSecurityPrincipalScopes`, `CamelMcpSecurityMethod` | in (over MCP) | The authenticated caller |

## Behavior

### A call, step by step

1. **Kill switch.** A tool named in `camel.ai.disabled-tools`, or disabled with
   `AiRegistry::disable`, is no longer listed and every call is refused.
2. **Authorization.** The `authorizationPolicy` bean may refuse the call; its reason goes back
   to the model.
3. **Arguments.** Arguments whose name starts with `camel` (any case) are dropped, as in Camel,
   and so are arguments the schema doesn't declare, unless it allows them
   (`additionalProperties`). What is left is validated against the full JSON Schema (2020-12,
   see [json-validator](json-validator.md)). Camel only checks required fields. A failure
   returns an error that names each field and shows valid arguments:

   ```text
   Invalid arguments for tool weather:
   - $.city: is missing but it is required
   - $.unit: does not have a value in the enumeration [celsius, fahrenheit]
   Example of valid arguments: {"city":"text"}
   ```

4. **Rate limit**, per principal.
5. **Circuit breaker.** While it is open, calls get the fallback text and the route doesn't run.
6. **Approval.** A call that needs approval doesn't run. It is parked with a handle and the
   result says so (`CamelAiToolStatus=approval-required`). An approver resolves it with
   `AiRegistry::approve(handle, approver)`, which runs it and returns its result, or
   `AiRegistry::deny(handle, approver, reason)`, which runs nothing. Over MCP, a tool without
   an approval endpoint asks the client's user instead (see
   [MCP server: approvals](../guides/mcp-server.md#approvals)).
7. **Idempotency.** The key is the `idempotency_key` argument if the schema declares one, else
   the call id, else a digest of the principal and the arguments. A repeated call returns the
   recorded result with `CamelAiToolDuplicate=true`. A repeat that arrives while the first is
   running waits for it. A failed call is not recorded, so a retry runs it.
8. **The route** runs on a new exchange that keeps only the caller's properties and variables
   (`CamelVariable.*`), as in Camel. Headers and the body don't leak into the tool.
9. **Timeout.** When the caller stops waiting, the call is cancelled: the route stops before its
   next step (a step that is already running is not interrupted), and the exchange is released.
   In Camel the call keeps running.
10. **Result.** An object or array body is the structured result, with its JSON as the text. An
    empty body is `No result`, and any other body is its text. With an `outputSchema`, a result
    that doesn't match it is an error.
11. **Audit.** One record per call, see [Audit log](#audit-log).

### Compensation

`compensation=direct:undo-x` registers the undo step **before** the call runs, under the
call's conversation id (`CamelAiToolConversationId`; calls without one register nothing). If
the conversation fails later, `AiRegistry::compensate(conversation)` runs every registered
undo step, newest first. Each undo step receives the original arguments as headers, the call's
result as the body and `CamelAiToolCompensating=true`.
`AiRegistry::complete(conversation)` ends a conversation and drops its undo steps.

### Audit log

Each call appends a record: time, tool, principal, call and conversation ids, a SHA-256 digest
of the arguments and of the result (never the data itself), the decision (`allowed`,
`invalid`, `approval-required`, `approved`, `rejected`, `rate-limited`, `circuit-open`,
`disabled`, `denied`, `compensate`), the approver, the duplicate flag, the outcome and the
duration. Records are hash-chained: each one holds the SHA-256 of the previous record plus its
own content, so an edited, removed or reordered record fails
`cameleer_ai::audit::verify_file`.

| property | default | description |
|---|---|---|
| `camel.ai.audit.enabled` | `true` | Keep audit records |
| `camel.ai.audit.file` | | Also append them to this JSON lines file; an existing file is verified and continued |
| `camel.ai.audit.maxRecords` | `1000` | Records kept in memory (`AiRegistry::audit().records()`) |

### Pinning

A tool's definition hash covers its name, title, description, schemas and hints. If a route
changes a tool's definition but keeps its `version`, the route fails to start:

```text
Tool transfer changed (definition hash 3f2a9c1d04be is now 9b71e0aa52c3) but its version is still 1: set a new version= to publish the change
```

Within one process the hashes are kept in memory. With `camel.ai.manifest=tools.json` they
are kept in that file, so the check also covers restarts and deployments. A tool's description
or schema can't change under a client that already trusts it.

### Kill switch

`camel.ai.disabled-tools=a,b` withdraws tools at startup. `AiRegistry::disable(name)` and
`enable(name)` do it at runtime. Either way the tool disappears from every list, its calls are
refused, and `AiRegistry::on_change` listeners are told, so MCP clients get
`notifications/tools/list_changed`.

An operator does the same from the command line, against an integration started with
`cameleer run` (it finds the process by name or pid, like `cameleer stop`):

```text
$ cameleer tool list orders
NAME    TAGS    READ-ONLY  DESTRUCTIVE  VERSION  STATUS
lookup  orders  yes                     1        enabled
refund  orders             yes          2        enabled
Pending approvals: 1
$ cameleer tool disable refund orders
tool refund disabled
$ cameleer tool enable refund orders
tool refund enabled
```

`cameleer tool approvals` lists the calls waiting for approval (handle, tool, principal, age,
reason). `cameleer tool approve <handle> [--by NAME]` runs one and prints its result, and
`cameleer tool deny <handle> [--by NAME] [--reason TEXT]` rejects it; both go to the audit log
with the approver (`--by`, default the user name). `tool list` and `tool approvals` take
`--json`. A name that no route publishes is refused, so a typo doesn't go unnoticed.

With the dev console (`camel.server.dev-console-enabled=true`), `GET /q/dev/tools` returns the
same list as JSON, and `POST /q/dev/tools/{name}/disable` and `/enable` flip the switch. Both
need the `ai` feature in the running binary; without it they say so.

## Differences from Apache Camel

- The hints and policies are enforced, not advisory: deduplication, approval, timeouts that
  cancel, rate limits, circuit breakers, compensation, audit, pinning and the kill switch are
  Cameleer extensions.
- Arguments are validated against the full schema, not only the required fields.
- Results keep their structure: a map or list is JSON plus structured content, not
  `toString()`.
- The producer (calling a tool from a route) is a Cameleer extension.
- The [MCP server](../guides/mcp-server.md) serves these tools to MCP clients in both protocol
  eras (2026-07-28 and 2025-11-25), with the same envelope.
- A route can return images, audio or embedded resources by setting `CamelAiToolContent` to a
  list of MCP content blocks.

## Related

- [MCP server](../guides/mcp-server.md): serving these tools to MCP clients.
- [ai-resource](ai-resource.md) and [ai-prompt](ai-prompt.md): the other two things an agent
  can use.
- [json-validator](json-validator.md): the JSON Schema support used for arguments.
- [Cameleer for AI agents](../guides/agents.md): patterns built on these components.
- [Build features](../guides/build-features.md): the `ai` feature.

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
