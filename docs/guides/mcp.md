# Cameleer MCP: the action layer for AI agents

> The model decides what; a tested route decides how. Cameleer sits between AI agents and your systems: agents call it over MCP from Claude, IDE assistants and agent frameworks, each action runs as a route, and the server enforces what a model can't be trusted with: arguments checked, retries run once, risky actions approved by a person, slow calls cancelled, every call audited. In a field test with 40 real requests, Claude Opus 5.5 completed 196 of 200 runs correctly, and none ran an action twice, broke a rule or ran a risky call without a yes.

**Cargo feature:** [`ai`](build-features.md) · **Examples:** [`examples/agents/mcp-shop`](https://github.com/cameleering/cameleer/tree/main/examples/agents/mcp-shop), [`examples/agents/mcp-gateway`](https://github.com/cameleering/cameleer/tree/main/examples/agents/mcp-gateway)

This page is the map: why agents need an action layer, how Cameleer's MCP server works, where it
runs, how it compares, and one page per use case with the requests people typed, what happened,
and the tested route behind it.

## Why agents need an action layer

Agents are good at deciding and bad at doing things exactly once, within the rules, with a
record of who allowed what. Cameleer enforces those parts on every call, whatever the model does:

| when the model... | without an action layer | with Cameleer | shown by |
|---|---|---|---|
| sends the same call twice | the action runs twice: two refunds, two emails | it runs once per conversation; the repeat gets the first result | field test: a resent ticket made one ticket |
| invents or mangles arguments | they reach your API | rejected by the tool's JSON Schema, every error named; nothing runs | hermetic tests |
| picks a risky action | it runs | it waits for the user's yes in the client, or for an operator | [approvals](mcp/approvals.md), [a manager in the loop](mcp/manager-approval.md) |
| ignores a business rule | it runs if the prompt didn't stop it | the route refuses and says why; the model passes the reason on | [refunds](mcp/refunds-with-business-rules.md): a £900 refund refused, not split |
| reads instructions planted in data | it may act on them | data comes back as data; destructive tools still need a person's yes | [safety](mcp/safety.md): an injected "refund order 1020" reported, not acted on |
| waits on a slow system | the call times out but the work carries on | the timeout cancels the route before its next step | hermetic tests |
| is shown personal data | it sees everything the API returns | redacted before it leaves the server | [safety](mcp/safety.md): no full card number |
| is asked what it did | whatever the framework logged | a hash-chained record per call: caller, argument digest, decision, approver | a test detects an edited record |

On a refund across four systems, an agent calling each API directly needs 4 tool calls and 5
model turns and can refund twice, leave half-done work or choose the amount; through one
Cameleer route it is 1 tool call and 2 model turns, run once, rolled back on failure, with the
amount taken from the order ([direct tool calls vs Cameleer](agent-tool-calls.md)).

MCP is how agents reach this layer. Around it, Cameleer also feeds events to models (Kafka, MQTT,
webhooks, files), calls 23 model providers with retries and fallbacks
([model providers](model-providers.md)), routes on the model's answer, and runs agents itself
with the [agent step](agent-step.md).

## In one minute

The [Model Context Protocol](https://modelcontextprotocol.io) (MCP) is how AI clients find and
call tools. With Cameleer, a tool is a route:

<!-- example: mcp-hub-lookup-order -->
```yaml
- route:
    from:
      uri: ai-tool:lookup_order
      parameters:
        description: Look up an order by id
        parameter.orderId: string
        parameter.orderId.required: true
        readOnlyHint: true
      steps:
        - setBody:
            simple: '{"orderId": "${header.orderId}", "status": "shipped"}'
```

```bash
cameleer run tools.camel.yaml --mcp          # Streamable HTTP on http://localhost:8080/mcp
cameleer run tools.camel.yaml --mcp-stdio    # for a client that starts the server itself
```

**Existing routes become tools without editing them.** `cameleer run orders.camel.yaml
--mcp-expose rest:*` serves every REST DSL operation as a tool with typed arguments taken from its
documented parameters; `direct:*` and `route:*` patterns select other routes. Reads (GET, HEAD)
are read-only tools, and every other exposed tool needs approval by default
([how](mcp-server.md#expose-existing-routes-without-editing-them)).

**Or make a route a tool by changing one line**, when you want to name the tool and its arguments
yourself. Keep its steps and replace the line
it starts from with an `ai-tool` URI that names the tool and its arguments
([examples/agents/route-to-mcp-tool.camel.yaml](../../examples/agents/route-to-mcp-tool.camel.yaml)):

```diff
 - route:
     id: order-lookup
     from:
-      uri: direct:lookup-order
+      uri: "ai-tool:lookup_order?description=Look up an order by id&parameter.orderId=string&parameter.orderId.required=true&readOnlyHint=true"
       steps:
```

Then one command serves it, from the CLI, Docker or your own Rust program:

```bash
cameleer run orders.camel.yaml --mcp
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" cameleering/cameleer run /routes/orders.camel.yaml --mcp
```

```rust
main.set_property("camel.server.mcp-enabled", "true");
b.from("ai-tool:lookup_order?description=Look up an order by id&parameter.orderId=string&parameter.orderId.required=true&readOnlyHint=true")
    .set_body(simple(r#"{"orderId": "${header.orderId}"}"#));
```

A test (`an_existing_route_becomes_a_tool_by_changing_one_line` in
`crates/cameleer-main/tests/mcp_main.rs`) checks that the example differs from the original route
in that one line and serves the tool, from YAML and from the Rust DSL. A route that other routes
or a REST endpoint also call keeps its `direct:` start, with a three-line `ai-tool` route in front
of it that does `to: direct:lookup-order`.

**Cameleer is both an MCP server and an MCP client.** It serves your routes to agents, and it
imports other MCP servers' tools for routes and models to use, or serves them again as a
[gateway](mcp/gateway.md) with the same checks.

In a real tool the `setBody` is a query, an API call, a queue: any of Cameleer's about 230
components, with the route's own error handling and retries. The fastest way to see what that
means is the [shop example](mcp-shop-example.md): a whole back office in one YAML file that you
can ask questions in Claude Desktop.

## How it works

1. **Routes become tools, resources and prompts.** `ai-tool`, `ai-resource` and `ai-prompt`
   endpoints use Apache Camel's names and options; the server uses Camel's
   `camel.server.mcp-*` properties ([MCP server](mcp-server.md)).
2. **Every call goes through one envelope**, in this order: kill switch, authorization, JSON
   Schema 2020-12 argument checks, rate limit, circuit breaker, approval, deduplication,
   compensation, a timeout that cancels the route, output checks and redaction, a hash-chained
   audit record. The model never decides whether these apply ([ai-tool](../components/ai-tool.md)).
3. **Both protocol eras on one endpoint**: the stateless 2026-07-28 era and the 2025-11-25,
   2025-06-18 and 2025-03-26 versions with sessions. Over Streamable HTTP and stdio.
4. **Approvals and questions go to the person.** A risky call shows the user an approval form
   (`input_required` or `elicitation/create`), or waits for an operator; a tool route can ask
   the user a question in the middle of a call.
5. **Cameleer is also a client.** It imports the tools of other MCP servers into the same
   envelope and serves them again, so one endpoint can front many servers
   ([MCP client and gateway](mcp-client.md)).

The official MCP conformance suite passes all 39 scenarios for 2025-11-25 and all 26 for
2025-06-18.

## Use cases

Each page shows requests a shop's staff typed into Claude, the tool calls Claude made, what
came back, and the route that made it safe. The requests come from the
[field test](mcp-shop-example.md#how-it-was-tested); the routes are the
[shop example](https://github.com/cameleering/cameleer/tree/main/examples/agents/mcp-shop) and run
in Cameleer's test suite (`crates/cameleer-main/tests/mcp_shop.rs`).

| use case | what it shows | field test (Opus 5.5) |
|---|---|---|
| [Customer-service lookups](mcp/customer-service-lookups.md) | one question, one or two read-only tools, an answer with the facts | 32 of 32 runs |
| [Questions that take several steps](mcp/multi-step-questions.md) | the model chains up to 26 calls; tools that return the facts the next step needs | 21 of 24 |
| [Actions guarded by approvals](mcp/approvals.md) | cancellations and refunds wait for the user's yes in the client | 32 of 32 |
| [Refunds with the rules in the route](mcp/refunds-with-business-rules.md) | 30-day window, a £500 limit, partial refunds: checked in code, explained to the model | 20 of 20 |
| [A manager in the loop](mcp/manager-approval.md) | store credit parked for a person outside the chat; asking again gets the decision | 16 of 16 (with the elicitation cases) |
| [The tool asks the user](mcp/the-tool-asks-the-user.md) | a route stops mid-call to ask which size, then carries on | included above |
| [Safety](mcp/safety.md) | prompt injection in data, a redacted card number, refusals, unknown ids | 28 of 28 |
| [Prompts and resources](mcp/prompts-and-resources.md) | policies as documents, prompts the user picks, documents through a tool | 8 of 8 |
| [One gateway for several MCP servers](mcp/gateway.md) | import another server's tools, choose which, put your rules on them | tested in the suite |
| [Where to run it](mcp/deployment.md) | desktop, server, Docker and Kubernetes, Cloud Run, AWS Lambda, an edge gateway | tested in the suite |

## Where it runs, and why each matters

| environment | transport | why |
|---|---|---|
| Claude Desktop and other desktop clients | stdio | the client starts the server; nothing to host, data stays on the machine |
| a server or VM | Streamable HTTP | one endpoint for a team, with API keys, JWT or OAuth token checks |
| Docker, Docker Compose, Kubernetes | either | the same image as every other route; health checks; several replicas |
| Cloud Run, Knative, Azure Container Apps | Streamable HTTP | routes run about 3 ms after start, so scaling to zero costs clients little |
| AWS Lambda | Streamable HTTP through a function URL | the binary is its own Lambda runtime; stateless 2026-07-28 requests fit it |
| an edge gateway | either | tools next to the machines; data stays on site; arm64 and amd64 |

Commands and settings for each: [Where to run it](mcp/deployment.md).

## How it compares

Measured on three identical cloud machines (4 dedicated vCPUs each, 64 client connections), one
tool answering a JSON document ([benchmarks](../BENCHMARKS.md#action-layer-and-mcp-2026-11-06-second-run)):

| | Cameleer | Apache Camel `camel-mcp-server` (JVM) | MCP TypeScript SDK (Node.js) |
|---|---:|---:|---:|
| `tools/call` in sessions (2025-06-18) | **41,620 calls/s** | 22,458 calls/s | 1,536 calls/s |
| p99 latency | 5.9 ms | 5.2 ms | 79 ms |
| peak memory | **53 MB** | 317 MB | 214 MB |
| stateless `tools/call` (2026-07-28) | 51,700 calls/s, 26 MB | not offered | not offered |

Each Cameleer call in that run went through the whole envelope (schema check, rate-limit and
breaker bookkeeping, an audit record); the envelope costs about 6 µs per call on that Xeon.

| | Cameleer | Apache Camel `camel-mcp-server` | an official MCP SDK | by hand (direct tool calls) |
|---|---|---|---|---|
| tools as | routes (YAML, XML, Rust) | routes (Java, YAML) | functions in your code | functions in the agent |
| argument checks | JSON Schema 2020-12 | required arguments | the schema | yours to write |
| duplicate calls | run once per conversation | not built in | yours to write | yours to write |
| risky calls | approval form or operator | hints are advisory; no approval step documented | yours to write | yours to write |
| slow calls | timeout cancels the route | the client gets an error; the route keeps running | yours to write | yours to write |
| audit | hash-chained record per call | events and metrics | yours to write | yours to write |
| other MCP servers | imported and served again (gateway) | MCP client options inside some AI producers | a client library | not applicable |
| deploy | one binary, about 3 ms start | JVM, Quarkus or Spring Boot | Node.js or Python service | inside the agent |

The Camel column follows Camel's documentation as summarised in
[Apache Camel's MCP server vs Cameleer](https://cameleer.ing/compare/camel-mcp-server-vs-cameleer/);
[direct tool calls vs Cameleer](agent-tool-calls.md) compares a hand-wired agent with a route on
a refund across four systems.

## What a real field test found

40 requests a shop's staff would type ran through Claude and the official MCP Python client
against the shop example, five times each with Claude Opus 5.5:

| model | runs | correct | turns per request | tool calls per request | cost per request |
|---|---|---|---|---|---|
| Claude Opus 5.5, 2025-11-25 sessions | 160 | 157 (98%) | 2.9 | 3.3 | about $0.026 |
| Claude Opus 5.5, 2026-07-28 stateless | 40 | 39 | 2.9 | 3.3 | about $0.025 |
| Claude Haiku 5.5, 2025-11-25 sessions | 120 | 104 (87%) | 2.8 | 2.9 | about $0.0007 |

Each run is graded on the tool calls made, the database afterwards (for example, that no refund
was written) and a separate judge comparing the answer with the tool results. The four Opus
misses were wording slips, not wrong actions. The first run passed 25 of 36 requests; the
fixes it led to are in [what the test changed](mcp-shop-example.md#what-the-test-changed).

## Reference

- [MCP server](mcp-server.md): every property, protocol versions, approvals, authentication
- [MCP client and gateway](mcp-client.md): importing other servers' tools
- [ai-tool](../components/ai-tool.md), [ai-resource](../components/ai-resource.md), [ai-prompt](../components/ai-prompt.md)
- [The agent step](agent-step.md): Cameleer as the agent, with the same tools
- [Ask Claude about your business](mcp-shop-example.md): the shop example and the field test

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/cameleering/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
