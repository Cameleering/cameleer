# MCP use case: the tool asks the user

> A tool route can stop in the middle of a call and ask the person at the client a question, through MCP elicitation, then carry on with the answer. The shop's `exchange_item` asks which size to send; if the user closes the form, nothing changes and the result says so. In the field test, both elicitation cases passed in every run.

**Cargo feature:** [`ai`](../build-features.md) · **Example:** [`examples/agents/mcp-shop`](https://github.com/cameleering/cameleer/tree/main/examples/agents/mcp-shop) · **Part of:** [Cameleer MCP](../mcp.md)

## What people ask, and what happens

| request | what happened |
|---|---|
| Olivia Brown wants to swap the rain jacket from order 1011 for a different size. | the client showed a form, "Which size should we send for JKT-RAIN on order 1011?"; the user picked L; exchange EX-1 was created |
| Exchange the jacket on order 1011 for another size. *(the user closes the form)* | nothing was exchanged; Claude said the size question was cancelled and offered to start again |

## A recorded conversation

**User:** Olivia Brown wants to swap the rain jacket from order 1011 for a different size.

Claude called `get_order`, then `exchange_item` with `{"orderId": 1011, "sku": "JKT-RAIN"}`.
The route asked:

> Which size should we send for JKT-RAIN on order 1011?

with a choice of XS to XXL. The user chose L, the route finished, and Claude answered:

> Olivia Brown's rain jacket exchange is set up. The reference is EX-1, for order 1011.
> New size: L, which is the size you chose when asked. [...] The size L jacket will ship once
> her original is scanned at the post office.

The model never guessed the size: the answer came from the user, through the tool.

## How it is built

The route sets the form's JSON Schema in `CamelAiToolElicitSchema`, puts the question in the
body and sends it to `ai-tool:current?operation=elicit`. The answer comes back as the body and
`CamelAiToolElicitAction` says whether the user accepted, declined or cancelled:

<!-- example: mcp-shop-the-tool-asks-the-user-exchange-item -->
```yaml
- route:
    id: exchange-item
    from:
      uri: ai-tool:exchange_item
      parameters:
        title: Exchange an item
        description: >-
          Exchange one item of a delivered order for another size, within 30 days of delivery. The
          tool asks the user which size to send; the customer returns the original with a prepaid
          label.
        tags: acme
        parameter.orderId: integer
        parameter.orderId.description: The order number
        parameter.orderId.required: true
        parameter.sku: string
        parameter.sku.description: The SKU of the item to exchange (get_order lists them)
        parameter.sku.required: true
      steps:
        - to: direct:load-order
        - to: "sql:select count(*) from order_lines where order_id = :#orderId and sku = :#sku?dataSource=jdbc:rustsql:mem:acme&outputType=SelectOne"
        - setHeader:
            name: hasLine
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
                      simple: "Order ${header.orderId} is ${header.orderStatus}; only delivered items can be exchanged."
              - simple: "${header.hasLine} == 0"
                steps:
                  - setHeader:
                      name: CamelAiToolIsError
                      constant: true
                  - setBody:
                      simple: "Order ${header.orderId} has no item ${header.sku}; get_order lists its SKUs."
            otherwise:
              steps:
                - setHeader:
                    name: CamelAiToolElicitSchema
                    constant: '{"type": "object", "properties": {"size": {"type": "string", "title": "New size", "enum": ["XS", "S", "M", "L", "XL", "XXL"]}}, "required": ["size"]}'
                - setBody:
                    simple: "Which size should we send for ${header.sku} on order ${header.orderId}?"
                - to: ai-tool:current?operation=elicit
                - choice:
                    when:
                      - simple: "${header.CamelAiToolElicitAction} != 'accept'"
                        steps:
                          - setBody:
                              simple: '{"orderId": ${header.orderId}, "status": "not exchanged", "note": "The user did not choose a size (${header.CamelAiToolElicitAction}); nothing was changed."}'
                    otherwise:
                      steps:
                        - setHeader:
                            name: size
                            simple: "${body[size]}"
                        - to: "sql:insert into exchanges (order_id, sku, size, created) values (:#orderId, :#sku, :#size, '{{acme.today}}') returning id?dataSource=jdbc:rustsql:mem:acme&outputType=SelectOne"
                        - setBody:
                            simple: '{"exchange": "EX-${body}", "orderId": ${header.orderId}, "sku": "${header.sku}", "newSize": "${header.size}", "note": "The tool asked the user which size to send, and the user chose ${header.size}.", "status": "replacement ships when the original is scanned at the post office; prepaid return label emailed"}'
```

- **The question is part of the tool,** so every client asks it the same way, and the model
  cannot skip it.
- **Every outcome is handled.** Accept continues; decline or cancel returns "not exchanged ...
  nothing was changed".
- **The result names who decided.** "The tool asked the user which size to send, and the user
  chose L" stopped smaller models from presenting the size as their own choice.
- **Both eras work.** In 2026-07-28 the call returns `input_required` and the client retries
  with the answer; in a 2025-11-25 session or over stdio the server sends `elicitation/create`
  during the call. A client without the `elicitation` capability gets an error result, and
  nothing runs.

A route can also send progress, log messages and sampling requests (ask the client's model)
the same way; see [MCP server](../mcp-server.md#progress-logs-elicitation-and-sampling).

## Try it

Run the shop, connect Claude Desktop (it shows elicitation forms) and ask for the exchange.
`shop_use_cases` in `crates/cameleer-main/tests/mcp_shop.rs` answers the form with L, and
cancels it in a second call, over MCP.

## Related

- [A manager in the loop](manager-approval.md), [Actions guarded by approvals](approvals.md)
- [Cameleer MCP](../mcp.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/cameleering/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
