# MCP server

> Cameleer serves your `ai-tool`, `ai-resource` and `ai-prompt` routes to MCP clients (Claude Desktop, VS Code, Cursor and agent frameworks) over Streamable HTTP or stdio, in both protocol eras: 2026-07-28 and 2025-11-25. Every call goes through the same envelope: argument checks, deduplication, approvals, timeouts that cancel and an audit log.

**Cargo feature:** [`ai`](build-features.md) · **Apache Camel:** [MCP Server](https://camel.apache.org/components/next/others/mcp-server.html) (same `camel.server.mcp-*` property names)

## Start it

Write the routes ([examples/agents/mcp-server.camel.yaml](../../examples/agents/mcp-server.camel.yaml)):

<!-- example: mcp-server-lookup-order -->
```yaml
- route:
    from:
      uri: ai-tool:lookup_order
      parameters:
        description: Look up an order by id
        tags: support
        parameter.orderId: string
        parameter.orderId.required: true
        readOnlyHint: true
      steps:
        - setBody:
            simple: '{"orderId": "${header.orderId}", "status": "shipped"}'
```

Serve them over HTTP at `http://localhost:8080/mcp`:

```bash
cameleer run mcp-server.camel.yaml --mcp
```

Or over stdio, for a client that starts the server itself:

```bash
cameleer run mcp-server.camel.yaml --mcp-stdio
```

A client configuration for stdio (Claude Desktop's `claude_desktop_config.json`, and similar
files in other clients):

```json
{
  "mcpServers": {
    "orders": {
      "command": "cameleer",
      "args": ["run", "/path/to/mcp-server.camel.yaml", "--mcp-stdio"]
    }
  }
}
```

On the command line, `--mcp` and `--mcp-stdio` expose every tool, resource and prompt, tagged
or not (they default `camel.server.mcp-tags` to `*,untagged`), unless `camel.server.mcp-tags`
says otherwise. In a configuration file nothing is exposed until you choose:

```properties
camel.server.mcp-enabled=true
camel.server.mcp-tags=support
```

## Configuration

| property | default | description |
|---|---|---|
| `camel.server.mcp-enabled` | `false` | Start the MCP server |
| `camel.server.mcp-transport` | `http` | `http` or `stdio` |
| `camel.server.mcp-tags` | none | Tag patterns of what is exposed: `*`, `support`, `crm-*`, `untagged` (comma separated). Without it nothing is exposed; see [Tags](#tags) |
| `camel.server.mcp-path` | `/mcp` | The HTTP path |
| `camel.server.port`, `camel.server.host` | `8080`, `0.0.0.0` | Shared with `platform-http` routes and the management endpoints |
| `camel.server.mcp-server-name` | the context's name | The `name` in `serverInfo`, as in Camel |
| `camel.server.mcp-server-version` | the Cameleer version | The `version` in `serverInfo` |
| `camel.server.mcp-server-title`, `-server-description`, `-server-website-url` | | More of `serverInfo`: a display name, a description and a web site |
| `camel.server.mcp-server-icons` | | `serverInfo.icons`, a JSON array such as `[{"src": "https://example.com/icon.png", "mimeType": "image/png"}]` |
| `camel.server.mcp-instructions` | | Instructions for the model, sent in `server/discover` and `initialize` |
| `camel.server.mcp-tool-timeout` | `20s` | The default `timeout` of tools that set none |
| `camel.server.mcp-resource-tool` | | A tool name (or `true` for `read_document`): also list a read-only tool that reads any exposed resource by URI; its description lists them. Most clients attach resources only when the user picks one, so without it the model cannot read your policies or documents by itself |
| `camel.server.mcp-expose-errors` | `false` | Send route exceptions to clients as they are. By default a client gets `Tool NAME failed (error ID). The cause is in the server log.` and the cause is logged with the same id |
| `camel.server.mcp-resource-timeout` | `20s` | How long a resource read or a prompt may take |
| `camel.server.mcp-page-size` | `100` | Items per page of the lists |
| `camel.server.mcp-cache-ttl` | `0` | `ttlMs` on lists, `server/discover` and reads (2026-07-28) |
| `camel.server.mcp-allowed-origins` | localhost | Origins allowed to call the endpoint from a browser (`*` for any); other origins get `403` |
| `camel.server.mcp-session-keep-alive-interval` | `30s` | Keep-alive comments on event streams |
| `camel.server.mcp-session-idle-ttl` | `300s` | When an idle legacy session expires |
| `camel.server.mcp-request-state-key` | random per process | 64 hex characters: the AES-256-GCM key of `requestState`; set it when several instances serve one endpoint |
| `camel.server.mcp-request-state-ttl` | `10m` | How long an approval request stays valid |

Authentication properties are in [Authentication](#authentication).

## Tags

`camel.server.mcp-tags` decides which tools, resources and prompts clients see. Each pattern
matches tags, with `*` as a wildcard: `support` matches the tag `support`, `crm-*` matches
`crm-orders`. As in Camel, `*` alone matches every item that has a tag, but not items without
tags. The pattern `untagged` (a Cameleer extension) exposes the items without tags. So
`camel.server.mcp-tags=*,untagged` exposes everything, which is what `--mcp` and `--mcp-stdio`
use by default.

Tools imported from other MCP servers have the tags `mcp` and `mcp:NAME`, so `mcp:*` exposes
all of them (see [MCP client: gateway](mcp-client.md#gateway)).

## Protocol versions

Both eras are served on the same endpoint. The server tells them apart by how the client
opens, as the specification defines:

- **2026-07-28.** Every request carries `_meta` with
  `io.modelcontextprotocol/protocolVersion` and `io.modelcontextprotocol/clientCapabilities`,
  and over HTTP the `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` headers. The server
  keeps no session. Every result has `resultType`, and lists and reads carry `ttlMs` and
  `cacheScope`. Clients learn about changes with `subscriptions/listen`.
- **2025-11-25, 2025-06-18 and 2025-03-26.** `initialize` opens a session (`Mcp-Session-Id`),
  `GET` opens its event stream, `DELETE` ends it, and there are `ping` and
  `resources/subscribe`.

| | 2026-07-28 | 2025-11-25 and earlier |
|---|---|---|
| Discovery | `server/discover` | `initialize` |
| Tools, resources, templates, prompts, completion | yes | yes |
| Approvals | `input_required` with an `elicitation/create` form | `elicitation/create` request on the response stream |
| Change notifications | `subscriptions/listen` | `GET` event stream |
| Errors | `-32020` header mismatch, `-32021` missing capability, `-32022` unsupported version | JSON-RPC errors, `-32002` unknown resource |

## Approvals

A tool that needs approval (`destructiveHint=true`, or an `approval` rule, see
[ai-tool](../components/ai-tool.md#a-call-step-by-step)) doesn't run until someone says yes:

- If the tool has an `approvalEndpoint`, approval happens out of band. The client gets a result
  that says the call is waiting, with its handle.
- Otherwise the client's user is asked. In 2026-07-28 the result is `input_required` with an
  `elicitation/create` form, and a `requestState` sealed with AES-256-GCM and bound to the
  principal, the tool, a digest of the arguments and an expiry. The client retries with the
  answer. A tampered state, changed arguments or a replay run nothing. In legacy sessions the
  server sends `elicitation/create` on the call's response stream (HTTP) or on stdout (stdio).
  The form has one field, `approve`, ticked by default: pressing Accept approves, Decline (or
  unticking the box) refuses.
- A client that didn't declare the `elicitation` capability gets `-32021` and the call doesn't
  run.

Out-of-band approvals can also be resolved from the command line: `cameleer tool approvals`
lists the waiting calls of a running integration, and `cameleer tool approve <handle>` or
`cameleer tool deny <handle>` decides one. `cameleer tool disable <name>` (or
`POST /q/dev/tools/{name}/disable` on the dev console) withdraws a tool while the server runs:
connected clients get `notifications/tools/list_changed` and the tool's calls are refused. See
[ai-tool: kill switch](../components/ai-tool.md#kill-switch).

## Progress, logs, elicitation and sampling

A tool route can talk back to the client while it runs, with steps such as
`to: ai-tool:current?operation=progress`:

- `progress` sends `notifications/progress` when the request carried a `progressToken`.
- `log` sends `notifications/message` at or above the level the client asked for
  (`io.modelcontextprotocol/logLevel` in `_meta` in 2026-07-28, `logging/setLevel` in legacy
  sessions).
- `elicit` asks the client's user a question with a JSON Schema for the answer
  (`elicitation/create`).
- `sample` asks the client's model for a completion (`sampling/createMessage`).

In legacy sessions the questions are requests to the client on the call's response stream or on
stdout. In 2026-07-28 the call returns `input_required` with the questions, and the client
calls again with the answers; the route then runs again from the start. A client that didn't
declare the `elicitation` or `sampling` capability can't be asked, and the step fails with
`InteractionUnavailableException`. The options, headers and an example are in
[ai-tool: talking to the client](../components/ai-tool.md#talking-to-the-client-progress-logs-elicitation-and-sampling).

## Rich results

A tool route can return MCP content blocks (images, audio, embedded resources) by setting the
`CamelAiToolContent` header to a list of them. A prompt route can return messages whose
`content` is such a block.

<!-- example: mcp-server-screenshot -->
```yaml
- route:
    from:
      uri: ai-tool:chart
      parameters:
        description: The sales chart as an image
        readOnlyHint: true
      steps:
        - setHeader:
            name: CamelAiToolContent
            simple: '[{"type": "text", "text": "Sales this week"}, {"type": "image", "mimeType": "image/png", "data": "${body}"}]'
```

## Authentication

| property | description |
|---|---|
| `camel.server.mcp-auth` | `none` (default), `api-key`, `jwt` or `introspection` |
| `camel.server.mcp-api-keys` | `name=key,...` for `api-key` (keys of 16 characters or more; use `{{env:...}}`). Clients send `Authorization: Bearer <key>` or `X-API-Key` |
| `camel.server.mcp-resource-url` | The endpoint's public URL, such as `https://tools.example.com/mcp`. Tokens must name it as their audience (RFC 8707) |
| `camel.server.mcp-jwt-jwks-url` | The JWKS to verify JWTs with (RS256/384/512, PS256/384/512, ES256/384); keys are cached and refetched when a new `kid` appears |
| `camel.server.mcp-jwt-secret` | An HMAC secret instead (HS256/384/512) |
| `camel.server.mcp-jwt-issuer` | The required `iss` |
| `camel.server.mcp-jwt-audience` | Accepted audiences, if not the resource URL |
| `camel.server.mcp-introspection-url`, `-client-id`, `-client-secret` | RFC 7662 introspection |
| `camel.server.mcp-authorization-servers` | Published in the protected-resource metadata |
| `camel.server.mcp-required-scopes` | Scopes every token must have; others get `403` with `insufficient_scope` |
| `camel.server.mcp-principal-claim` | The claim that names the principal (default `sub`) |

With `jwt` or `introspection` the server publishes RFC 9728 metadata at
`/.well-known/oauth-protected-resource` and `/.well-known/oauth-protected-resource/mcp`. A
request without a valid token gets `401` with
`WWW-Authenticate: Bearer resource_metadata="..."`, so clients can find the authorization
server.

The client's token is never passed to the routes or downstream. Tool routes see the principal
in `CamelMcpSecurityPrincipal` and `CamelMcpSecurityPrincipalName` (Camel's name), its scopes
as a list in `CamelMcpSecurityPrincipalRoles` and as one space-separated string in
`CamelMcpSecurityPrincipalScopes`, and the method in `CamelMcpSecurityMethod`. The same
headers are visible to a tool's `authorize` predicate. Rate limits, the audit log and
approvals use the same principal. Over stdio the principal is `stdio`.

## What a call carries

| `_meta` key on `tools/call` | becomes |
|---|---|
| `io.cameleer/toolCallId` | `CamelAiToolCallId`, the idempotency key |
| `io.cameleer/conversationId` | `CamelAiToolConversationId`, for compensation (legacy sessions use the session id) |
| `traceparent`, `tracestate`, `baggage` | headers of the tool's exchange |

Without a call id, a tool with side effects is deduplicated on its principal, the conversation
(the session in 2025-11-25 and earlier, `io.cameleer/conversationId` in 2026-07-28) and its
arguments for `idempotencyWindow`: a client's retry runs nothing twice, and two conversations
that ask for the same thing both run. Declare an `idempotency_key` argument, or set `idempotency=false`, when the
same arguments may legitimately repeat.

## Verification

- The hermetic tests (`crates/cameleer-ai/tests/mcp.rs`) drive both eras over real HTTP and
  stdio connections, including approvals, subscriptions and authentication.
- `scripts/mcp-conformance.sh` runs the official conformance suite
  (`@modelcontextprotocol/conformance` 0.1.16, pinned and checksum-verified) against
  [scripts/mcp-conformance/fixtures.camel.yaml](../../scripts/mcp-conformance/fixtures.camel.yaml).
  It passes all 39 scenarios for 2025-11-25 and all 26 for 2025-06-18, including log and
  progress notifications during a call, sampling and elicitation. The baseline of known
  failures, `scripts/mcp-conformance/expected-failures.yaml`, is empty (`server: []`); a
  scenario that fails makes the run fail. This suite version predates 2026-07-28, so that era
  is covered by the hermetic tests.

## Related

- [ai-tool](../components/ai-tool.md), [ai-resource](../components/ai-resource.md), [ai-prompt](../components/ai-prompt.md)
- [MCP client and gateway](mcp-client.md): using the tools of other MCP servers, and serving
  them again from this server
- [Cameleer for AI agents](agents.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
