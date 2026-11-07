# MCP use case: refunds with the business rules in the route

> The refund policy (30 days for any reason, damaged items at any time, a £500 limit, never more than was paid) is enforced by the `issue_refund` route, not by the prompt. When a rule says no, the route refuses in plain words and Claude passes the reason on; it did not split a £900 refund to get under the limit. In the field test, all 20 refund runs were correct.

**Cargo feature:** [`ai`](../build-features.md) · **Example:** [`examples/agents/mcp-shop`](https://github.com/pmsudhi/cameleer/tree/main/examples/agents/mcp-shop) · **Part of:** [Cameleer MCP](../mcp.md)

## What people ask, and what happens

Every refund also asks the user to approve it ([approvals](approvals.md)).

| request | tool calls (recorded run) | what happened |
|---|---|---|
| Refund order 1011, the jacket was the wrong size. | `get_order`, `issue_refund` | approved, £159 refunded (RF-1): delivered 10 days ago, inside the window |
| Refund order 1002, they didn't like it. | `get_order`, `read_document` | not refunded: delivered 45 days ago; Claude offered store credit or a repair, as the policy says |
| The hiking boots on 1015 fell apart after a week, refund just the boots. | `get_order`, `issue_refund` | £179 for the boots only, marked defective; the socks stay paid |
| Refund $900 on order 1020. | `get_order`, `read_document` | not refunded: over the £500 limit, a manager must do it; Claude also asked whether "$" meant pounds |
| Aisha Bello complained that her stove arrived with a cracked burner. Refund her latest order. | `search_customers`, `list_orders`, `get_order` | the order also has a pot: Claude asked whether to refund the stove (£119) or all of it (£164) |

## A recorded conversation

**User:** The hiking boots on 1015 fell apart after a week, refund just the boots.

Claude read the order, then called `issue_refund` with
`{"orderId": 1015, "amount": 179, "damagedOrDefective": true, "reason": "Defective: Ridgeline
Hiking Boots (BOOT-HK) fell apart after a week of use"}`. The user approved the form and the
route wrote the refund:

> I refunded £179.00 for the Ridgeline Hiking Boots on order 1015, which is Hana Sato's order.
> The refund ID is RF-1. [...] The 3 pairs of Merino Hiking Socks (£54.00) weren't refunded,
> so £54.00 of the £233.00 order total is still paid.

When Claude does call the tool against a rule, the route answers with an error result, for
example:

> Order 1002 was delivered 45 days ago, outside the 30-day refund window. Only damaged or
> defective items can be refunded after 30 days (policy://refunds); otherwise offer store
> credit (issue_store_credit, needs a manager) or a repair.

## How the tool is built

The route loads the order, computes the days since delivery in the embedded SQL engine, and
checks each rule in order. Each refusal sets `CamelAiToolIsError` and says what to do instead:

<!-- example: mcp-shop-refunds-with-business-rules-issue-refund -->
```yaml
- route:
    id: issue-refund
    from:
      uri: ai-tool:issue_refund
      parameters:
        title: Refund an order
        description: >-
          Refund a delivered order, in full or in part. Follows the refund policy (resource
          policy://refunds): within 30 days of delivery for any reason; damaged or defective items
          at any time; refunds over 500 need a manager and are refused here. Give the amount for a
          partial refund (for example the price of one item); without it the remaining amount is
          refunded. Needs the user's approval.
        tags: acme
        parameter.orderId: integer
        parameter.orderId.description: The order number
        parameter.orderId.required: true
        parameter.reason: string
        parameter.reason.description: Why, such as "wrong size" or "damaged on arrival"
        parameter.reason.required: true
        parameter.damagedOrDefective: boolean
        parameter.damagedOrDefective.description: True when the item arrived damaged or turned out defective
        parameter.amount: number
        parameter.amount.description: Amount to refund for a partial refund; omit for everything not yet refunded
        destructiveHint: true
      steps:
        - to: direct:load-order
        - setHeader:
            name: amt
            simple: "${header.amount ?: ${header.remaining}}"
        - to: "sql:select case when :#orderDelivered is null or :#orderDelivered = '' then -1 else cast(date '{{acme.today}}' - cast(:#orderDelivered as date) as int) end?dataSource=jdbc:rustsql:mem:acme&outputType=SelectOne"
        - setHeader:
            name: daysSince
            simple: "${body}"
        - choice:
            when:
              - simple: "${header.order} == null"
                steps:
                  - setHeader:
                      name: CamelAiToolIsError
                      constant: true
                  - setBody:
                      simple: "Order ${header.orderId} does not exist."
              - simple: "${header.orderStatus} != 'delivered'"
                steps:
                  - setHeader:
                      name: CamelAiToolIsError
                      constant: true
                  - setBody:
                      simple: "Order ${header.orderId} is ${header.orderStatus}; only delivered orders are refunded here. Orders that have not shipped can be cancelled instead (cancel_order)."
              - simple: "${header.remaining} <= 0"
                steps:
                  - setHeader:
                      name: CamelAiToolIsError
                      constant: true
                  - setBody:
                      simple: "Order ${header.orderId} has already been refunded in full."
              - simple: "${header.amt} <= 0 || ${header.amt} > ${header.remaining}"
                steps:
                  - setHeader:
                      name: CamelAiToolIsError
                      constant: true
                  - setBody:
                      simple: "The amount must be more than 0 and at most ${header.remaining}, the part of order ${header.orderId} not yet refunded."
              - simple: "${header.amt} > 500"
                steps:
                  - setHeader:
                      name: CamelAiToolIsError
                      constant: true
                  - setBody:
                      simple: "Refunds over 500 need a manager's approval and cannot be issued here. Tell the user to ask a manager to approve the refund of ${header.amt} on order ${header.orderId} in the back office."
              - simple: "${header.daysSince} > 30 && ${header.damagedOrDefective} != true"
                steps:
                  - setHeader:
                      name: CamelAiToolIsError
                      constant: true
                  - setBody:
                      simple: "Order ${header.orderId} was delivered ${header.daysSince} days ago, outside the 30-day refund window. Only damaged or defective items can be refunded after 30 days (policy://refunds); otherwise offer store credit (issue_store_credit, needs a manager) or a repair."
            otherwise:
              steps:
                - to: "sql:insert into refunds (order_id, amount, reason, created) values (:#orderId, :#amt, :#reason, '{{acme.today}}') returning id?dataSource=jdbc:rustsql:mem:acme&outputType=SelectOne"
                - setHeader:
                    name: refundId
                    simple: "RF-${body}"
                - to: "sql:update orders set refunded = refunded + :#amt where id = :#orderId?dataSource=jdbc:rustsql:mem:acme"
                - setBody:
                    simple: '{"refundId": "${header.refundId}", "orderId": ${header.orderId}, "amount": ${header.amt}, "status": "refunded to the card on file, 3-5 working days"}'
```

- **The amount comes from the order.** Without `amount` the route refunds what has not been
  refunded yet; with it, it must be more than 0 and at most that.
- **The model cannot talk its way past a rule.** The prompt and the policy document help the
  model explain; the `choice` in the route decides.
- **Refusals are results, not exceptions.** An exception would reach the client as a generic
  "Tool issue_refund failed" (errors are hidden by default); `CamelAiToolIsError` with a
  sentence gives the model something to tell the user.
- **The policy is also a document** (`policy://refunds`), so Claude can quote it; see
  [prompts and resources](prompts-and-resources.md).

## Try it

Run the shop, connect a client, and ask for the refunds above. `shop_use_cases` in
`crates/cameleer-main/tests/mcp_shop.rs` runs each rule over MCP: outside the window, over the
limit, one line of an order, a full refund, and a second refund of the same order.

## Related

- [Actions guarded by approvals](approvals.md), [A manager in the loop](manager-approval.md)
- [Direct tool calls vs Cameleer](../agent-tool-calls.md): the same idea across four systems with a saga

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
