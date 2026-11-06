# AI Resource component

> The `ai-resource` component turns a route into a resource an AI agent can read: a document, a record, a report. The route's body is the content.

**Scheme:** `ai-resource:` · **Crate:** `cameleer-ai` · **Cargo feature:** [`ai`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes (Cameleer extension) · **Apache Camel:** [AI Resource](https://camel.apache.org/components/next/ai-resource-component.html)

## When to use it

- Give an agent read access to context it needs (a handbook, a price list, an order) without
  giving it a tool that can change anything.
- Serve records by URI with a template (`orders://{id}`), backed by any connector.

## URI format

```text
ai-resource:name[?options]
```

## Example

<!-- example: ai-resource-handbook -->
```yaml
- route:
    from:
      uri: ai-resource:handbook
      parameters:
        uri: docs://handbook
        description: The staff handbook
        mimeType: text/markdown
      steps:
        - pollEnrich:                                     # or any endpoint that returns content
            constant: file:docs?fileName=handbook.md&noop=true&idempotent=false
- route:
    from:
      uri: ai-resource:order
      parameters:
        uri: "orders://{id}"
        mimeType: application/json
      steps:
        - setBody:
            simple: '{"id": "${header.id}", "status": "shipped"}'
```

From Rust, `AiRegistry::of(&ctx)?.read_resource("orders://42")?` returns the content. From a
route, `to: ai-resource:order` with the `CamelAiResourceUri` header reads it into the body.

## Options

| option | default | description |
|---|---|---|
| `uri` | `ai-resource://NAME` | The resource's URI. With `{name}` parts it is a template: `orders://{id}` matches `orders://42` and sets the header `id` to `42` |
| `title` | | A human-readable name |
| `description` | | What the resource holds |
| `mimeType` | `text/plain` | The content's type; a route can override it per read with the `CamelAiResourceMimeType` header |
| `tags` | | Comma-separated tags |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAiResourceUri` | in | The URI being read (also selects the resource for the producer) |
| `CamelAiResourceName` | in | The resource's name |
| template variables | in | One header per `{name}` in a URI template |
| `CamelAiResourceMimeType` | out | The content's type |

## Behavior

- A text body (or a byte body with a text, JSON, XML or YAML type) is text content. Other byte
  bodies are binary content. A map or list body is returned as JSON.
- An unknown URI fails with `ResourceNotFoundException`.
- Templates use RFC 6570 level 1: each `{name}` matches up to the next literal part of the
  template.

## Differences from Apache Camel

- URI templates and the producer are Cameleer extensions.
- The [MCP server](../guides/mcp-server.md) lists and serves resources and templates to MCP
  clients, over HTTP and stdio, with subscriptions.
- `to: ai-resource:name?operation=updated` tells subscribed clients that the resource changed
  (MCP `notifications/resources/updated`).
- `values.NAME=a,b,c` lists completion values for a template variable.

## Related

- [ai-tool](ai-tool.md) and [ai-prompt](ai-prompt.md).
- [Cameleer for AI agents](../guides/agents.md).

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
