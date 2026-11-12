# MCP client and gateway

> Cameleer connects to remote MCP servers, over Streamable HTTP or stdio, and imports their tools into its own tool registry. The imported tools go through the same envelope as `ai-tool` routes: argument checks, approvals for destructive tools, deduplication, timeouts, audit and pinning. Models use them from the `agent` step or the model components, routes call them with the `mcp` component, and Cameleer's own MCP server can serve them again as a gateway.

**Overview and use cases:** [Cameleer MCP](mcp.md) · **Cargo feature:** [`ai`](build-features.md) · **Apache Camel:** none (Cameleer extension)

## Connect a server

One block of properties per server. The block's name (`github` below) names the server
everywhere else.

```properties
# Streamable HTTP
camel.ai.mcp.github.url=https://mcp.example.com/github
camel.ai.mcp.github.bearerToken={{env:GITHUB_TOKEN}}
camel.ai.mcp.github.include=issue_*,pr_*

# stdio: Cameleer starts the program and talks to it over stdin and stdout
camel.ai.mcp.files.command=npx
camel.ai.mcp.files.args=-y,@modelcontextprotocol/server-filesystem,/data
camel.ai.mcp.files.env.NODE_ENV=production
```

When Cameleer starts, after the routes, it connects to each server, detects the protocol era
and imports the tools. The log says how many:

```text
Imported 4 tools from MCP server github
```

A server that is down doesn't stop the integration. Cameleer logs
`MCP server github is not reachable (...); retrying every 30s` and connects in the background
until it succeeds. Until then, that server's tools are not in the registry.

## Configuration

| property | default | description |
|---|---|---|
| `camel.ai.mcp.NAME.url` | | The Streamable HTTP endpoint. Either `url` or `command` is required |
| `camel.ai.mcp.NAME.command` | | stdio: the program to start |
| `camel.ai.mcp.NAME.args` | | stdio: its arguments, comma separated |
| `camel.ai.mcp.NAME.env.VAR` | | stdio: an environment variable of the program |
| `camel.ai.mcp.NAME.header.H` | | HTTP: an extra request header, such as `header.X-Tenant=acme` |
| `camel.ai.mcp.NAME.bearerToken` | | HTTP: sends `Authorization: Bearer TOKEN` |
| `camel.ai.mcp.NAME.toolPrefix` | `NAME_` | The prefix of the imported tools' names; set it empty for none |
| `camel.ai.mcp.NAME.tags` | | Extra tags of the imported tools, comma separated |
| `camel.ai.mcp.NAME.include`, `.exclude` | | Which tools to import: name patterns with `*`, comma separated. `exclude` wins |
| `camel.ai.mcp.NAME.timeout` | `30s` | How long one request may take; also the imported tools' `timeout` |
| `camel.ai.mcp.NAME.protocol` | `auto` | `auto`, `2026-07-28` or `2025-11-25` |
| `camel.ai.mcp.NAME.refreshInterval` | | Re-list the tools this often, in addition to `list_changed` |
| `camel.ai.mcp.NAME.OPTION` | as on `ai-tool` | An envelope option for every imported tool of the server: `rateLimit`, `rateLimitPeriod`, `circuitBreakerThreshold`, `circuitBreakerDelay`, `circuitBreakerFallback`, `approval`, `approvalEndpoint`, `approvalTimeout`, `idempotency`, `idempotencyWindow`, `idempotentRepository`, `compensation`, `audit`, `authorize`, `redact`, `cacheTtl`, `authorizationPolicy` ([ai-tool](../components/ai-tool.md)) |
| `camel.ai.mcp.NAME.tools.TOOL.OPTION` | | The same options for one tool (`TOOL` is the remote server's name for it); they override the server-wide ones |

Use `{{env:...}}` placeholders for tokens. A stdio program's standard error is discarded.

## Protocol eras

With `protocol=auto`, the client sends `server/discover` first. If the server lists
`2026-07-28`, the client uses that era: no session, `_meta` on every request. Any other answer,
including an error, falls back to `initialize` with `2025-11-25`, and the client keeps the
`Mcp-Session-Id` the server returns. A legacy session that expires (`HTTP 404`) is reopened
once, and the request is sent again. `protocol=2026-07-28` fails if `server/discover` fails;
`protocol=2025-11-25` skips it.

On stop, the client ends a legacy HTTP session with `DELETE` and stops a stdio program.

## Imported tools

Each remote tool becomes a local tool named `PREFIX` + its remote name: `create_issue` on
`github` becomes `github_create_issue`. The local tool keeps the remote title, description,
input and output schemas and annotations, and gets these tags:

- `mcp`, on every imported tool,
- `mcp:NAME`, the server's own tag,
- the tags in `camel.ai.mcp.NAME.tags`.

A call to an imported tool runs through the envelope (see
[ai-tool: a call, step by step](../components/ai-tool.md#a-call-step-by-step)) and then goes
to the remote server:

- The arguments are validated against the remote schema locally. Invalid arguments never
  leave the process.
- A tool with `destructiveHint: true` needs approval (`approval=always`). Other tools need
  none.
- A tool that is neither `readOnlyHint` nor `idempotentHint` is deduplicated: a repeated call
  with the same call id, or the same principal and arguments, returns the recorded result.
- The call is cancelled after the server's `timeout`.
- The kill switch and the audit log work as for local tools:
  `cameleer tool disable github_create_issue` withdraws an imported tool too.
- Every other envelope option of `ai-tool` can be set for all the server's tools or for one
  tool: a rate limit per principal, a circuit breaker, an `authorize` rule, `redact` on what
  comes back, a result cache. For example:

  ```properties
  camel.ai.mcp.github.rateLimit=30
  camel.ai.mcp.github.redact=secret,email
  camel.ai.mcp.github.tools.delete_repository.authorize=${header.CamelAiToolPrincipal} == 'admin'
  ```

  A re-sync (`list_changed`, a reconnect, `refreshInterval`) keeps tools whose definition and
  options didn't change, so their rate-limit counters, breaker state and cache survive it.

A remote result with `isError` is a failed call. A structured result (`structuredContent`)
stays structured; content blocks other than text (images, audio, resources) are passed on as
`CamelAiToolContent`.

A remote server that asks for input during a call (2026-07-28 `input_required`, or an
`elicitation/create` or `sampling/createMessage` request) gets a refusal: the client has no
user to ask on that connection.

### Changes and rug-pull protection

The client follows the server's `notifications/tools/list_changed`: over stdio on the
program's output, over HTTP with `subscriptions/listen` (2026-07-28) or the session's `GET`
stream (legacy), reconnecting when the stream drops. With `refreshInterval` it also re-lists
on a timer. On each change it imports the list again: new tools appear, removed tools
disappear, and connected MCP clients of Cameleer's own server get `list_changed` in turn.

Imported tools are pinned like local ones (see [ai-tool: pinning](../components/ai-tool.md#pinning)).
If a server changes a tool's description, schema or annotations after it was imported, the
new definition is refused and the tool is withdrawn. The log says why:

```text
Tool weather of MCP server remote not imported: Tool remote_weather changed (definition hash 3f2a9c1d04be is now 9b71e0aa52c3) but its version is still 1: set a new version= to publish the change
```

This stops a server that was trusted when it was added from rewriting a tool into an
instruction for the model later ("a rug pull"). In memory the pins last for the process; with
`camel.ai.manifest=tools.json` they also survive restarts. To accept a changed tool, remove its
entry from the manifest (or restart, without a manifest).

## Use the tools from a model

The agent step offers a server's tools with `tools: mcp:NAME`, next to local tags:

<!-- example: mcp-client-agent -->
```yaml
- route:
    from:
      uri: direct:triage
      steps:
        - agent:
            model: "openai:chat-completion?model=gpt-5.4-mini"
            tools: mcp:github,support          # the github server's tools and local support tools
            systemMessage: You triage bug reports. Search for duplicates before you open an issue.
            maxTurns: 8
```

The model components take the same tags in `tags`:

<!-- example: mcp-client-tags -->
```yaml
- route:
    from:
      uri: direct:ask
      steps:
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&apiKey={{env:OPENAI_API_KEY}}&tags=mcp:github"
```

A destructive remote tool pauses the conversation for approval like a local one (see the
[agent step](agent-step.md)).

To call a remote tool from a route without a model, use the [mcp component](../components/mcp.md):
`to: mcp:github/create_issue`.

## Gateway

Cameleer's [MCP server](mcp-server.md) serves imported tools like local ones. So one Cameleer
process can sit between your MCP clients and several third-party servers:

```properties
camel.ai.mcp.github.url=https://mcp.example.com/github
camel.ai.mcp.github.bearerToken={{env:GITHUB_TOKEN}}
camel.ai.mcp.files.command=files-server

camel.server.mcp-enabled=true
camel.server.mcp-tags=mcp:*,support
camel.server.mcp-auth=jwt
camel.server.mcp-jwt-jwks-url=https://login.example.com/.well-known/jwks.json
camel.server.mcp-resource-url=https://tools.example.com/mcp
```

What that gives you:

- **One endpoint.** Clients configure `https://tools.example.com/mcp` instead of every server.
  They see `github_*` and `files_*` next to your own `support` tools.
- **One authentication.** Clients authenticate to Cameleer. The third-party credentials stay
  in Cameleer's configuration and are never sent to clients. The remote servers see Cameleer's
  credentials, not the end user's.
- **One audit log.** Every call, local or remote, gets a hash-chained audit record with the
  principal of the client.
- **Your rules on their tools.** Destructive remote tools need approval; the kill switch,
  deduplication, schema checks and timeouts apply, and you can add rate limits, circuit
  breakers, `authorize` rules and redaction per server or per tool. Clients in either protocol era can use servers of either
  era.
- **Pinned definitions.** A third-party server can't change a tool under your clients.

`cameleer run --mcp` exposes every tagged tool, so it re-exports all imported tools; with
`camel.server.mcp-tags` you choose. Remote errors reach the gateway's clients as a generic
message with an error id unless `camel.server.mcp-expose-errors=true`; the cause is in the
log.

## Verification

- `crates/cameleer-ai/tests/mcp_client.rs`: tools imported into the envelope in both eras
  (schema checks before anything is sent, a destructive tool parked for approval), Cameleer as
  a gateway (a client of the gateway lists and calls upstream tools, the gateway's audit log
  records them), a tool that changes upstream is withdrawn, the configuration properties, and
  the `mcp` component.
- `crates/cameleer-main/tests/mcp_main.rs`, `an_agent_uses_tools_of_a_stdio_mcp_server`: `Main`
  starts the real `cameleer` binary as a stdio MCP server, imports one of its tools and an
  agent step calls it.

## Related

- [mcp component](../components/mcp.md): calling remote tools, resources and prompts from routes.
- [MCP server](mcp-server.md): serving tools to MCP clients.
- [ai-tool](../components/ai-tool.md): the envelope.
- [Agent step](agent-step.md): tool calling with `tools` and `tags`.

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/cameleering/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
