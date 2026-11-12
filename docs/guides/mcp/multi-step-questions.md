# MCP use case: questions that take several steps

> "Which out-of-stock products have open orders waiting on them?" needs stock, orders and order lines. Claude chains the shop's read-only tools itself, 6.6 calls per request on average and up to 26, and answers in two to four turns. In the field test Claude Opus 5.5 got 21 of 24 such runs right; the three misses were a miscounted total and an unchecked aside, not wrong tool calls.

**Cargo feature:** [`ai`](../build-features.md) · **Example:** [`examples/agents/mcp-shop`](https://github.com/cameleering/cameleer/tree/main/examples/agents/mcp-shop) · **Part of:** [Cameleer MCP](../mcp.md)

## What people ask, and what happens

| request | tool calls (recorded run) | what Claude answered |
|---|---|---|
| A customer emailed from jordan.k@example.com asking where their stuff is. What do I tell them? | `search_customers`, `list_orders`, `track_shipment`, `get_order` | which order is on its way, when it arrives, and a reply to send; it sent nothing |
| Which out-of-stock products have open orders waiting on them? | `check_inventory`, `list_orders` ×2, `get_order` ×6 | two products, the three orders waiting on them and the incoming dates |
| Compare this month's sales with last month's by category. | `sales_report` ×3 | November so far against all of October and against the same 7 days of October |
| Find our three best customers by lifetime value and tell me what they bought most. | `list_orders`, `get_customer` ×4, `get_order` ×10 | the three customers, their lifetime value and what each bought |
| Sam Lee wants to know about his order. | `search_customers` | two customers share the name: it lists both and asks which one |

## A recorded conversation

**User:** Which out-of-stock products have open orders waiting on them?

Claude called `check_inventory` with `{"lowStockOnly": true}`, then `list_orders` for paid
and for pending orders, then `get_order` for each of the six open orders: nine calls in three
turns. It answered:

> Two products are out of stock, and 3 open orders are waiting on them. Beam 400 Headlamp
> (LAMP-HD): 0 in stock. 30 units are due on 13 Nov 2026. #1012 Liam O'Connor [...] #1022
> Priya Natarajan [...] Trailblazer 2 Tent (TENT-TB2): 0 in stock. 20 units are due on
> 14 Nov 2026. #1016 Carlos Mendez [...] I checked all 6 open orders (5 paid, 1 pending).

**User:** Sam Lee wants to know about his order.

One call, `search_customers` with `{"query": "Sam Lee"}`, returned two people. Claude did not
pick one:

> There are two customers named Sam Lee, so I need to know which one before I look up the
> order: 1. Sam Lee in Manchester: sam.lee@example.com [...] 2. Sam Lee in Glasgow:
> samuel.lee@example.org [...]

## How the tools are built

Chained questions work when each tool returns enough to choose the next call. The customer
search returns every match with the details that tell people apart, and says so in its
description:

<!-- example: mcp-shop-multi-step-questions-search-customers -->
```yaml
- route:
    id: search-customers
    from:
      uri: ai-tool:search_customers
      parameters:
        title: Search customers
        description: >-
          Find customers by part of their name, email address or phone number. Returns id, name,
          email, phone, city and tier for every match (several people can share a name, so check
          the email or city before acting for one of them). Use the id with get_customer or
          list_orders.
        tags: acme
        parameter.query: string
        parameter.query.description: Part of a name, email or phone number, such as "Priya", "jordan.k@" or "900103"
        parameter.query.required: true
        readOnlyHint: true
        idempotentHint: true
      steps:
        - setHeader:
            name: pattern
            simple: "%${header.query}%"
        - to: "sql:select id, name, email, phone, city, tier from customers where name ilike :#pattern or email ilike :#pattern or phone like :#pattern order by name, id?dataSource=jdbc:rustsql:mem:acme"
        - choice:
            when:
              - simple: "${body.size()} == 0"
                steps:
                  - setBody:
                      simple: '{"matches": [], "note": "No customer matches \"${header.query}\". Try part of the name, the email or the phone number."}'
            otherwise:
              steps:
                - marshal:
                    json: {}
```

`list_orders` filters by customer, status and date, and returns what the next step needs (ids,
status, totals), leaving the items to `get_order`:

<!-- example: mcp-shop-multi-step-questions-list-orders -->
```yaml
- route:
    id: list-orders
    from:
      uri: ai-tool:list_orders
      parameters:
        title: List orders
        description: >-
          List orders, newest first, filtered by customer, status and/or creation date. Statuses:
          pending (not paid), paid (waiting to ship), shipped, delivered, cancelled, returned.
          Returns id, customer, status, dates and total; use get_order for the items.
        tags: acme
        parameter.customerId: integer
        parameter.customerId.description: Only this customer's orders (id from search_customers)
        parameter.status: string
        parameter.status.description: Only orders with this status
        parameter.status.enum: pending,paid,shipped,delivered,cancelled,returned
        parameter.createdFrom: string
        parameter.createdFrom.description: Only orders created on or after this date, YYYY-MM-DD
        parameter.createdTo: string
        parameter.createdTo.description: Only orders created on or before this date, YYYY-MM-DD
        readOnlyHint: true
        idempotentHint: true
      steps:
        - setHeader:
            name: cid
            simple: "${header.customerId ?: 0}"
        - setHeader:
            name: st
            simple: "${header.status ?: ''}"
        - setHeader:
            name: dfrom
            simple: "${header.createdFrom ?: '1900-01-01'}"
        - setHeader:
            name: dto
            simple: "${header.createdTo ?: '2999-12-31'}"
        - to: "sql:select o.id, o.customer_id, c.name as customer, o.status, o.created, o.shipped, o.delivered, o.total, o.refunded from orders o join customers c on c.id = o.customer_id where (:#cid = 0 or o.customer_id = :#cid) and (:#st = '' or o.status = :#st) and o.created >= :#dfrom and o.created <= :#dto order by o.created desc, o.id desc?dataSource=jdbc:rustsql:mem:acme"
        - marshal:
            json: {}
```

- **Filters as optional parameters** (`status` with an `enum`, dates as `YYYY-MM-DD`) let the
  model ask for exactly the rows it needs. A value outside the enum is rejected by the schema
  before the route runs.
- **All matches, not the first.** Returning both Sam Lees, with city and email, is what lets the
  model notice the ambiguity.
- **Every step is cheap.** In the field test the tools spent 48 ms per multi-step request in
  total; the time goes to the model.

## Where it went wrong

Three of 24 runs were graded as misses. In one, a summary said 3 tents where the orders showed
4. In two, Claude stated that an order "isn't even his order" without looking it up. The tool
calls were right in all three; the slips were in the prose. Asking the model to state only what
the tools returned, and giving it the facts in the result, are the remedies the test pointed to
([lessons](../mcp-shop-example.md#lessons-for-your-own-mcp-server)).

## Try it

```bash
cd examples/agents/mcp-shop
cameleer run shop.camel.yaml --properties application.properties
```

Connect a client to `http://127.0.0.1:18190/mcp`. `shop_use_cases` in
`crates/cameleer-main/tests/mcp_shop.rs` checks the two Sam Lees, the low-stock list and the
paid orders over MCP.

## Related

- [Customer-service lookups](customer-service-lookups.md), [Prompts and resources](prompts-and-resources.md)
- [Cameleer MCP](../mcp.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/cameleering/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
