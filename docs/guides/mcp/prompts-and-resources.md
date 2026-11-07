# MCP use case: prompts and resources as documents

> Besides tools, an MCP server offers resources (documents the model can read) and prompts (ready-made requests the user picks). The shop serves its refund, shipping and support policies and every order as resources, two prompts for an apology email and a weekly summary, and a `read_document` tool so the model can read the policies on its own. In the field test all 8 runs of the two prompts were correct in sessions; the weekly summary missed once in the stateless run.

**Cargo feature:** [`ai`](../build-features.md) · **Example:** [`examples/agents/mcp-shop`](https://github.com/pmsudhi/cameleer/tree/main/examples/agents/mcp-shop) · **Part of:** [Cameleer MCP](../mcp.md)

## What people ask, and what happens

| request | tool calls (recorded run) | what Claude produced |
|---|---|---|
| the prompt **draft_apology** with Aisha Bello, order 1009, "the stove arrived with a cracked burner" | `get_order`, `read_document` ×2, `search_customers` | a ready-to-send apology naming the Campfire Pro Stove and the refund it qualifies for; shown, not sent |
| the prompt **weekly_ops_summary** | `sales_report` ×2, `list_orders` ×3, `check_inventory`, `list_tickets` ×2, `track_shipment` ×3, `get_order` ×6 | a summary of the week: revenue, orders waiting, a delayed shipment, low stock, urgent tickets |
| What's our refund policy for damaged items? | `read_document` | the shop's own policy, quoted |

## A recorded conversation

The user picked **draft_apology** in the client and filled in its three arguments. The server
turned them into this request for the model:

> Draft a short, warm apology email to Aisha Bello about order 1009: the stove arrived with a
> cracked burner. Look the order up first so the email names the items and what happens next,
> follow the support guide and the refund policy, and show me the draft before sending anything.

Claude read the order and both policies, and answered:

> I looked up order 1009 and drafted the email below. Nothing has been sent. [...] To: Aisha
> Bello. Subject: Sorry about your Campfire Pro Stove (order 1009) [...]

## How it is built

A resource is a route that returns the document. The refund policy is a fixed text; an order
is a template, `order://{orderId}`, filled in from the database:

<!-- example: mcp-shop-prompts-and-resources-policy-refunds -->
```yaml
- route:
    id: policy-refunds
    from:
      uri: ai-resource:refund-policy
      parameters:
        uri: policy://refunds
        title: Refund policy
        description: When Acme refunds, how much, and who must approve
        mimeType: text/markdown
        tags: acme
      steps:
        - setBody:
            constant: "# Refund policy\n\n- Any delivered order can be refunded within 30 days of delivery, for any reason, in full or in part.\n- Damaged or defective items are refunded at any time; ask the customer for a photo.\n- Refunds over 500 need a manager's approval in the back office.\n- Orders that have not shipped are cancelled instead of refunded; a paid order is refunded in full when cancelled.\n- Refunds go back to the card on file within 3-5 working days.\n- Outside the window, offer store credit or a repair.\n"
```

<!-- example: mcp-shop-prompts-and-resources-order-resource -->
```yaml
- route:
    id: order-resource
    from:
      uri: ai-resource:order
      parameters:
        uriTemplate: "order://{orderId}"
        title: Order
        description: One order as a JSON document
        mimeType: application/json
        tags: acme
      steps:
        - to: direct:load-order
        - setBody:
            simple: "${header.order ?: '{\"error\": \"no such order\"}'}"
```

A prompt is a route that turns its arguments into a request:

<!-- example: mcp-shop-prompts-and-resources-prompt-apology -->
```yaml
- route:
    id: prompt-apology
    from:
      uri: ai-prompt:draft_apology
      parameters:
        title: Draft an apology
        description: Draft an apology email to a customer about a problem with an order
        tags: acme
        argument.customer.description: The customer's name or email
        argument.customer.required: true
        argument.orderId.description: The order number
        argument.orderId.required: true
        argument.issue.description: What went wrong
        argument.issue.required: true
      steps:
        - setBody:
            simple: "Draft a short, warm apology email to ${header.customer} about order ${header.orderId}: ${header.issue}. Look the order up first so the email names the items and what happens next, follow the support guide and the refund policy, and show me the draft before sending anything."
```

- **Expose documents as a tool too.** Desktop clients attach a resource only when the user picks
  it, so in the first run of the field test Claude could not read the policies on its own.
  `camel.server.mcp-resource-tool=read_document` adds a read-only tool that reads any exposed
  resource; its description lists them. After that change Claude quoted the policies whenever a
  rule mattered.
- **Prompts encode the house rules.** "Show me the draft before sending anything" is in the
  prompt, so an apology is never sent by accident.
- **Resources can be subscribed to** (`resources/subscribe`, or `subscriptions/listen` in
  2026-07-28): a route signals a change with `ai-resource:x?operation=updated`.

## Try it

Run the shop, connect Claude Desktop, and pick a prompt from its prompt menu, or ask about the
refund policy. `shop_use_cases` in `crates/cameleer-main/tests/mcp_shop.rs` lists and gets the
prompts, lists and reads the resources and reads documents through `read_document`, over MCP.

## Related

- [ai-resource](../../components/ai-resource.md), [ai-prompt](../../components/ai-prompt.md)
- [Refunds with the rules in the route](refunds-with-business-rules.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
