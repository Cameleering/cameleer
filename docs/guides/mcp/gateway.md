# MCP use case: one gateway for several MCP servers

> Cameleer can import the tools of other MCP servers and serve them again, next to its own, through one endpoint. Clients configure one URL; the gateway chooses which remote tools to expose and puts the same envelope on them: schema checks, approvals, rate limits, a result cache, deduplication and the audit log. The example fronts the shop's MCP server and imports only its read-only tools.

**Cargo feature:** [`ai`](../build-features.md) · **Example:** [`examples/agents/mcp-gateway`](https://github.com/pmsudhi/cameleer/tree/main/examples/agents/mcp-gateway) · **Part of:** [Cameleer MCP](../mcp.md)

## What it is for

- **A support team gets one endpoint** for the shop's back office, the ticket system and the
  team's own tools, instead of configuring each server in every client.
- **Read-only access for some people.** The gateway imports `search_*`, `get_*` and `list_*` and
  leaves refunds and cancellations out, so the people who use it cannot reach them.
- **Your rules on someone else's tools.** A rate limit, a cache, approvals for destructive
  remote tools, redaction and an audit record per call, configured in properties.
- **Credentials stay in the gateway.** Clients authenticate to Cameleer; the remote servers see
  the gateway's credentials, never the client's token.
- **Definitions are pinned.** If a remote server changes a tool's description, schema or
  annotations after it was imported, the new definition is refused, the tool is withdrawn and
  the log says why (rug-pull protection).

## The example

A local tool, served with the imported ones:

<!-- example: mcp-gateway-support-hours -->
```yaml
- route:
    id: support-hours
    from:
      uri: ai-tool:support_hours
      parameters:
        title: Support opening hours
        description: When the support team answers phone calls and email, in UK time
        tags: support
        readOnlyHint: true
      steps:
        - setBody:
            constant: '{"phone": "Mon-Fri 08:00-18:00", "email": "every day, answered within one working day", "timeZone": "Europe/London"}'
```

The gateway's `application.properties` starts the shop over stdio, imports part of it with the
prefix `shop_`, and serves both:

```properties
camel.server.port=18191
camel.server.host=127.0.0.1
camel.server.mcp-enabled=true
camel.server.mcp-tags=mcp:*,support

camel.ai.mcp.shop.command=cameleer
camel.ai.mcp.shop.args=run,../mcp-shop/shop.camel.yaml,--properties,../mcp-shop/application.properties,--prop,acme.dir=../mcp-shop,--prop,camel.server.port=0,--mcp-stdio
camel.ai.mcp.shop.toolPrefix=shop_
camel.ai.mcp.shop.include=search_*,get_*,list_*,track_shipment,check_inventory,sales_report,read_document
camel.ai.mcp.shop.rateLimit=60
camel.ai.mcp.shop.rateLimitPeriod=1m
camel.ai.mcp.shop.cacheTtl=30s
```

```bash
cd examples/agents/mcp-gateway
cameleer run gateway.camel.yaml --properties application.properties
```

The log says `Imported 10 tools from MCP server shop`. A client connected to
`http://127.0.0.1:18191/mcp` sees `shop_search_customers`, `shop_get_order`,
`shop_track_shipment`, `shop_read_document` and the rest of the read-only tools, plus
`support_hours`; `shop_issue_refund`, `shop_cancel_order` and the other changes are not there.
A remote server can also be reached over Streamable HTTP with `camel.ai.mcp.NAME.url` and a
`bearerToken`.

## What to know

- **Imported tools are tagged `mcp` and `mcp:NAME`**, so `camel.server.mcp-tags=mcp:*` exposes
  all of them and `mcp:shop` one server's.
- **Per-tool rules** use `camel.ai.mcp.NAME.tools.TOOL.OPTION`, for example
  `camel.ai.mcp.shop.tools.sales_report.rateLimit=5`.
- **A cache answers from memory.** With `cacheTtl`, a result for the same arguments is reused
  for that long, including one taken before the remote server was ready; keep it short for data
  that changes.
- **Either era to either era.** Clients on 2026-07-28 can use a remote server on 2025-11-25 and
  the other way round.
- **Remote errors are sanitized** like local ones: clients get a generic message with an error
  id, the cause is in the gateway's log.
- **Models in Cameleer can use the imported tools too,** from the `agent` step or a model
  component (`tools: mcp:shop`), and routes can call them with the `mcp:` component.

## Try it

`gateway_imports_the_shop_over_stdio` in `crates/cameleer-main/tests/mcp_shop.rs` starts the
gateway from this example's files, waits for the import, checks which tools are exposed and
which are not, and calls `shop_track_shipment` and `support_hours` through the gateway.

## Related

- [MCP client and gateway](../mcp-client.md): every `camel.ai.mcp.*` option
- [Where to run it](deployment.md), [Safety](safety.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
