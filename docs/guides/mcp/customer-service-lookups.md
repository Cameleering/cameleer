# MCP use case: customer-service lookups

> Staff ask Claude "where is order 1007?" or "do we have the Trailblazer tent?" and get the facts from the back office in one or two read-only tool calls. In the field test, Claude Opus 5.5 answered all 32 lookup runs correctly, with 2.0 tool calls per request on average; the tools spent 4.4 ms per request in total, the rest of the time was the model.

**Cargo feature:** [`ai`](../build-features.md) · **Example:** [`examples/agents/mcp-shop`](https://github.com/cameleering/cameleer/tree/main/examples/agents/mcp-shop) · **Part of:** [Cameleer MCP](../mcp.md)

## What people ask, and what happens

Requests typed into Claude with the shop's MCP server connected (sample date: Saturday
7 November 2026). The tool calls are the ones Claude made in the recorded run.

| request | tool calls | what Claude answered |
|---|---|---|
| Where is order 1007? | `track_shipment` | with DPD, tracking DPD15501007, every scan, due tomorrow, Sunday 8 November |
| where is ordr 1007 lol | `track_shipment` | the same answer: the typo and the tone do not matter |
| Do we have the Trailblazer 2 tent in stock? | `search_products` | out of stock; 20 arriving on 14 November |
| What's the email address on file for Priya Natarajan? | `search_customers` | priya.n@example.com, and that she is the only customer with that name |
| Show me everything Marco Rossi has ordered this year. | `search_customers`, `list_orders`, `get_order` ×3 | three orders, £569 in total, in a table |
| Which orders are still waiting to ship? | `list_orders` ×2 | five paid orders, oldest first, and one more waiting for payment |
| Is order 1013 delivered yet? The customer says it never arrived. | `get_order`, `track_shipment`, `read_document` | delivered but left in the porch without a signature, and the shipping policy's next steps |
| How much did we sell last week, and what was the top product? | `sales_report` | £1,479 from 4 orders, the top product, and which week it counted |
| Where's order 9999? | `track_shipment` | it does not exist; asks for another way to find the customer |

## A recorded conversation

**User:** Where is order 1007?

Claude called `track_shipment` with `{"orderId": 1007}` and got the shipment and its events
(2.9 ms in the server). It answered:

> Order 1007 has shipped with DPD and is on its way. It's due to arrive tomorrow, Sunday
> 8 November. Tracking number: DPD15501007. 4 Nov, 17:20: DPD collected it from our warehouse
> in Swindon. [...]

Two turns, one tool call.

## How the tool is built

`track_shipment` is a route. `direct:load-order` (shared by several tools) reads the order
into headers; the route then answers with the shipment and its tracking events, or with a
readable error:

<!-- example: mcp-shop-customer-service-lookups-track-shipment -->
```yaml
- route:
    id: track-shipment
    from:
      uri: ai-tool:track_shipment
      parameters:
        title: Track a shipment
        description: >-
          Where an order is: carrier, tracking number, delivery status, expected delivery date and
          the tracking events so far. Orders that have not shipped yet have no shipment.
        tags: acme
        parameter.orderId: integer
        parameter.orderId.description: The order number, such as 1007
        parameter.orderId.required: true
        readOnlyHint: true
        idempotentHint: true
      steps:
        - to: direct:load-order
        - choice:
            when:
              - simple: "${header.order} == null"
                steps:
                  - setHeader:
                      name: CamelAiToolIsError
                      constant: true
                  - setBody:
                      simple: "Order ${header.orderId} does not exist. Order numbers run from 1001; ask the customer to check the number."
              - simple: "${header.orderStatus} in 'pending,paid,cancelled'"
                steps:
                  - setBody:
                      simple: '{"orderId": ${header.orderId}, "orderStatus": "${header.orderStatus}", "shipment": null, "note": "Order ${header.orderId} has not shipped (status ${header.orderStatus})."}'
            otherwise:
              steps:
                - to: "sql:select carrier, tracking, status, eta from shipments where order_id = :#orderId?dataSource=jdbc:rustsql:mem:acme&outputType=SelectOne"
                - marshal:
                    json: {}
                - setHeader:
                    name: shipment
                    simple: "${bodyAs(String)}"
                - to: "sql:select at, description from shipment_events where order_id = :#orderId order by at?dataSource=jdbc:rustsql:mem:acme"
                - marshal:
                    json: {}
                - setBody:
                    simple: '{"orderId": ${header.orderId}, "orderStatus": "${header.orderStatus}", "deliveredOn": "${header.orderDelivered}", "shipment": ${header.shipment}, "events": ${body}}'
```

What makes lookups work well with a model:

- **`readOnlyHint` and `idempotentHint`** tell the client the tool changes nothing, so it can
  call it without asking the user.
- **The description says what comes back and what to use next.** Claude picks
  `track_shipment` for "where is" and `get_order` for "what's in".
- **Return the facts the next question needs.** `search_products` includes the incoming stock
  and its date, so "do we have it?" is one call, not two.
- **Refuse in words.** For an unknown order the route sets `CamelAiToolIsError` and says what to
  do instead; the model passes that on rather than guessing. Exceptions are hidden from clients
  by default, so a readable refusal is the route's job.

## Try it

```bash
cd examples/agents/mcp-shop
cameleer run shop.camel.yaml --properties application.properties
```

Then connect Claude Desktop, the MCP Inspector or any MCP client to
`http://127.0.0.1:18190/mcp`, or use stdio as the example's
[README](https://github.com/cameleering/cameleer/tree/main/examples/agents/mcp-shop) shows. The
test `shop_use_cases` in `crates/cameleer-main/tests/mcp_shop.rs` makes these calls over MCP
and checks the answers.

## Related

- [Questions that take several steps](multi-step-questions.md)
- [Cameleer MCP](../mcp.md), [MCP server](../mcp-server.md), [ai-tool](../../components/ai-tool.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/cameleering/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
