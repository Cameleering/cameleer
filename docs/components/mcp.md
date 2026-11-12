# MCP component

> The `mcp` component calls a remote MCP server from a route: call one of its tools, list its tools, read a resource or get a prompt. A tool call goes through the local `ai-tool` envelope, so arguments are checked, destructive tools wait for approval and every call is audited.

**Scheme:** `mcp:` · **Crate:** `cameleer-ai` · **Cargo feature:** [`ai`](../guides/build-features.md) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** none (Cameleer extension)

## When to use it

- Call a tool of a third-party MCP server (GitHub, a file server, an internal service) from an
  ordinary route, without a model in between.
- Read a resource or a prompt that an MCP server publishes.
- Check what a server offers, for example in a health check or a startup route.

To let a model call these tools, you don't need this component: the tools of a configured
server are already in the tool registry, see [MCP client](../guides/mcp-client.md).

## URI format

```text
mcp:SERVER/TOOL[?options]
mcp:SERVER[?options]
```

`SERVER` is the name of a client configured with `camel.ai.mcp.SERVER.*` (see
[MCP client: configuration](../guides/mcp-client.md#configuration)). `TOOL` is the tool's name
on the remote server, without the local prefix. `mcp:SERVER/TOOL` calls the tool;
`mcp:SERVER` lists the server's tools.

## Example

The server `github` is configured in the properties:

```properties
camel.ai.mcp.github.url=https://mcp.example.com/github
camel.ai.mcp.github.bearerToken={{env:GITHUB_TOKEN}}
```

A route that opens an issue for each failed build. The body is the tool's arguments, as JSON:

<!-- example: mcp-call-tool -->
```yaml
- route:
    from:
      uri: direct:build-failed
      steps:
        - setBody:
            simple: '{"repo": "acme/shop", "title": "Build ${header.buildId} failed"}'
        - to: mcp:github/create_issue
        - choice:
            when:
              - simple: "${header.CamelMcpIsError} == true"
                steps:
                  - to: log:mcp-errors
```

Reading a resource and getting a prompt:

<!-- example: mcp-resource-prompt -->
```yaml
- route:
    from:
      uri: direct:readme
      steps:
        - setHeader:
            name: CamelMcpResourceUri
            constant: repo://acme/shop/README.md
        - to: mcp:github?operation=readResource
- route:
    from:
      uri: direct:review-prompt
      steps:
        - setBody:
            constant: '{"pr": "42"}'          # the prompt's arguments
        - to: mcp:github?operation=getPrompt&prompt=review_pr
```

## Options

| option | default | description |
|---|---|---|
| `operation` | `callTool` with a tool, else `listTools` | `callTool`, `listTools`, `readResource` or `getPrompt` |
| `toolName` | | The tool, instead of `/TOOL` in the path |
| `uri` | | `readResource`: the resource's URI when the `CamelMcpResourceUri` header is not set |
| `prompt` | | `getPrompt`: the prompt's name (required) |
| `direct` | `false` | `callTool`: send the call straight to the server and skip the local envelope |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelMcpResourceUri` | in | `readResource`: the resource to read; it wins over `uri=` |
| `CamelAiToolCallId` | in | `callTool`: the call id (the idempotency key) |
| `CamelAiToolConversationId` | in | `callTool`: the conversation, for compensation |
| `CamelAiToolPrincipal` | in | `callTool`: who the call is made for (rate limits, audit) |
| `CamelMcpIsError` | out | `callTool`: `true` when the tool failed or the call was refused |
| `CamelAiToolStatus` | out | `callTool` through the envelope: `completed`, `error`, `approval-required` or `denied` |
| `CamelAiToolContent` | out | `callTool` with `direct=true`: the content blocks, when the result has more than text |

## Behavior

### Calling a tool

The body is the arguments: a JSON object as text, or a map. Any other body sends no
arguments (`{}`).

When the server's tools were imported (they are, unless `include` or `exclude` left this one
out), the call goes through the local envelope of the imported tool `PREFIX` + `TOOL`
(`github_create_issue` by default). That means the arguments are validated against the
remote schema before anything is sent, a destructive tool waits for approval, a repeated call
with side effects runs once, and the call is written to the audit log. See
[ai-tool: a call, step by step](ai-tool.md#a-call-step-by-step). Refusals and failures don't
raise an exception: the body is the error text and `CamelMcpIsError` is `true`.

With `direct=true`, or when the tool was not imported, the call goes straight to the server. A
result with `isError` sets `CamelMcpIsError=true` and keeps the text as the body.

After the call, the body is the structured result (`structuredContent`) when the server sent
one, otherwise the text blocks joined by newlines.

### The other operations

- `listTools` sets the body to a list of `{name, description, inputSchema}` maps, following
  pagination. The names are the remote ones, without the local prefix.
- `readResource` sets the body to the text of the resource's contents, joined by newlines. If
  any part is binary, the body is the list of content items as the server sent them.
- `getPrompt` sends the body as the prompt's arguments and sets the body to the prompt's list
  of messages.

### Errors

- An unknown server fails with `IllegalStateException`: `No MCP client named github (configure
  camel.ai.mcp.github.url or .command)`.
- A JSON-RPC error from the server fails the exchange with the kind
  `org.apache.camel.component.mcp.McpException`, and the message names the server and the
  error code. Connection failures are I/O errors.
- A server that answers a call with `input_required` (2026-07-28) or asks for elicitation is
  refused: Cameleer has no user to ask on that connection.

## Related

- [MCP client](../guides/mcp-client.md): configuring servers, importing their tools, and the
  gateway.
- [ai-tool](ai-tool.md): the envelope the calls go through.
- [MCP server](../guides/mcp-server.md): serving tools to MCP clients.

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/cameleering/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
