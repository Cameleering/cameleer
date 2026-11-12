# MCP use case: actions guarded by approvals

> When Claude wants to cancel an order or issue a refund, the Cameleer MCP server stops and shows the user an approval form in the client. Nothing runs until they accept; a decline tells the model the call did not run and nothing changed. Rules such as "only unshipped orders can change address" are checked in the route either way. In the field test, all 32 runs of the change requests were correct.

**Cargo feature:** [`ai`](../build-features.md) · **Example:** [`examples/agents/mcp-shop`](https://github.com/cameleering/cameleer/tree/main/examples/agents/mcp-shop) · **Part of:** [Cameleer MCP](../mcp.md)

## What people ask, and what happens

| request | tool calls (recorded run) | what happened |
|---|---|---|
| Cancel order 1021, the customer changed their mind. | `get_order`, `cancel_order` | the user saw an approval form, accepted, and the order was cancelled |
| Refund order 1011. *(the user declines the form)* | `get_order`, `read_document`, `issue_refund` | nothing was refunded; Claude said the refund was declined and offered to resubmit |
| Change the delivery address on order 1018 to 22 Harbour Road, Bristol BS1 5TT. | `get_order`, `update_shipping_address` | changed: the order had not shipped |
| Change the address on order 1007 to 9 Elm St, Leeds. | `get_order`, `read_document` | refused: it has shipped; Claude explained how the customer can redirect it with DPD |
| Cancel order 1003. | `get_order` | refused: delivered; Claude offered the refund policy's options instead |
| Open a ticket for Aisha Bello: her stove arrived with a cracked burner, order 1009. High priority. | `search_customers`, `get_order`, `create_support_ticket` | ticket T-106, high priority |
| Email Liam O'Connor that his backorder for the headlamp ships Friday. | 5 lookups, `send_customer_email` | one email, to the right Liam |
| We're running low on the 1L insulated bottle, order 200 more. | `search_products`, `request_restock` | restock request R-1 for 200 units |
| Mark ticket T-104 as resolved, I called the customer back. | `update_ticket` | resolved, with a note about the call |

## A recorded conversation

**User:** Cancel order 1021, the customer changed their mind.

Claude looked the order up, then called `cancel_order` with
`{"orderId": 1021, "reason": "Customer changed their mind"}`. Instead of running it, the
server sent the client a form:

> Allow the tool cancel_order to run with {"orderId":1021,"reason":"Customer changed their
> mind"}? It needs approval because the tool is destructive.

The user accepted, the route ran, and Claude answered:

> I've cancelled order 1021. It was Liam O'Connor's order for one Compact Camp Chair (£69.00),
> placed today. No refund was needed: the order hadn't been paid yet [...] I haven't emailed
> Liam.

When a user declined the same kind of form for a refund, the tool result said:

> The call to issue_refund was rejected by mcp-client:anonymous: the user declined it in the
> approval form. It did not run, and nothing was changed.

and Claude told the user exactly that, without trying again.

## How it is built

One option makes a tool ask: `destructiveHint: true`. With the default `approval: auto`, a
destructive tool needs approval for every call. The route still checks its own rules after
the user says yes:

<!-- example: mcp-shop-approvals-cancel-order -->
```yaml
- route:
    id: cancel-order
    from:
      uri: ai-tool:cancel_order
      parameters:
        title: Cancel an order
        description: >-
          Cancel an order that has not shipped yet (status pending or paid); a paid order is
          refunded in full automatically. Shipped or delivered orders cannot be cancelled: use a
          refund (issue_refund) after the return instead. Needs the user's approval.
        tags: acme
        parameter.orderId: integer
        parameter.orderId.description: The order number
        parameter.orderId.required: true
        parameter.reason: string
        parameter.reason.description: Why the order is cancelled
        parameter.reason.required: true
        destructiveHint: true
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
                      simple: "Order ${header.orderId} does not exist."
              - simple: "${header.orderStatus} not in 'pending,paid'"
                steps:
                  - setHeader:
                      name: CamelAiToolIsError
                      constant: true
                  - setBody:
                      simple: "Order ${header.orderId} is ${header.orderStatus} and can no longer be cancelled. Delivered orders can be returned and refunded under the refund policy (resource policy://refunds)."
            otherwise:
              steps:
                - setHeader:
                    name: refund
                    simple: "${header.orderStatus == 'paid' ? ${header.orderTotal} : 0}"
                - to: "sql:update orders set status = 'cancelled', refunded = :#refund where id = :#orderId?dataSource=jdbc:rustsql:mem:acme"
                - setBody:
                    simple: '{"orderId": ${header.orderId}, "status": "cancelled", "refunded": ${header.refund}}'
```

How the approval reaches the user depends on the client and the protocol era:

- **2026-07-28:** the call returns `input_required` with an `elicitation/create` form and a
  `requestState` sealed with AES-256-GCM, bound to the caller, the tool, the arguments and an
  expiry. The client retries with the answer. A changed argument, a tampered state or a replay
  runs nothing.
- **2025-11-25 sessions and stdio:** the server sends `elicitation/create` on the call's
  response stream or on stdout.
- **No client in the loop:** give the tool an `approvalEndpoint` and an operator decides
  later; see [a manager in the loop](manager-approval.md).

A client that cannot show forms (it did not declare the `elicitation` capability) gets an
error and the call does not run. Tools that only read (`readOnlyHint`) and changes that are safe
to repeat (`update_shipping_address`, `idempotentHint`) need no approval; you choose per tool,
or with a rule such as `approval: "${header.amount} > 500"`.

## Try it

Run the shop and connect Claude Desktop over stdio (the example's
[README](https://github.com/cameleering/cameleer/tree/main/examples/agents/mcp-shop) has the
configuration), then ask it to cancel order 1021. `shop_use_cases` in
`crates/cameleer-main/tests/mcp_shop.rs` accepts and declines the forms over MCP and checks that
a declined refund writes nothing.

## Related

- [Refunds with the rules in the route](refunds-with-business-rules.md), [A manager in the loop](manager-approval.md)
- [MCP server: approvals](../mcp-server.md#approvals), [ai-tool](../../components/ai-tool.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/cameleering/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
