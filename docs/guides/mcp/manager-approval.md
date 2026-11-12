# MCP use case: a manager in the loop

> Some decisions belong to someone who is not in the chat. Goodwill store credit in the shop waits for a manager: the call is parked with a handle, the manager approves or rejects it from the command line, and when Claude asks again it gets the decision, never a second request. In the field test, all 8 runs of the two manager cases were correct, approvals and rejections alike.

**Cargo feature:** [`ai`](../build-features.md) · **Example:** [`examples/agents/mcp-shop`](https://github.com/cameleering/cameleer/tree/main/examples/agents/mcp-shop) · **Part of:** [Cameleer MCP](../mcp.md)

## What people ask, and what happens

| request | what happened |
|---|---|
| Order 1002 is outside the refund window. Give Marco Rossi 20 in store credit as a goodwill gesture instead. | Claude asked for the credit and said it was waiting for a manager. *The manager approved.* Asked "Did the credit go through?", Claude called again and reported SC-1, approved by manager.jo |
| Give Hana Sato 50 store credit for the trouble with her boots. | the credit waited for a manager. *The manager rejected it.* Asked "Any news on Hana's credit?", Claude reported the rejection and the manager's reason, and did not try another way |

## A recorded conversation

**User:** Order 1002 is outside the refund window. Give Marco Rossi 20 in store credit as a
goodwill gesture instead.

Claude checked the order and the customer, then called `issue_store_credit`. The call did not
run; the result carried a handle, and Claude said:

> I've asked for the £20 store credit for Marco Rossi, but it hasn't gone through yet. Every
> store credit needs a manager to approve it, and this one is waiting for that sign-off
> (approval handle `approval-169-15625474038170fb`).

A manager, outside the chat:

```bash
cameleer tool approvals
```

```bash
cameleer tool approve approval-169-15625474038170fb --by manager.jo
```

**User:** The manager has just approved it. Did the credit go through?

Claude called `issue_store_credit` again with the same arguments and got the outcome of the
approved call:

> Yes, the £20 store credit went through. It's been added to Marco Rossi's account
> (customer 2). Credit reference: SC-1. Approved by: manager.jo.

In the rejected case the second call returned:

> The call to issue_store_credit was rejected by manager.jo: Goodwill credit is not justified
> for this order. It did not run, and nothing was changed.

## How it is built

`approval: always` with an `approvalEndpoint` parks every call and tells the endpoint about it.
Here the endpoint is a route that writes the request to a manager's queue table; in practice it
could post to Slack or open a ticket. The approved call runs with `CamelAiToolApprovedBy` set,
so the route records who approved it:

<!-- example: mcp-shop-manager-approval-issue-store-credit -->
```yaml
- route:
    id: issue-store-credit
    from:
      uri: ai-tool:issue_store_credit
      parameters:
        title: Give store credit
        description: >-
          Give a customer store credit (a goodwill gesture, or instead of a refund outside the
          policy). Every credit needs a manager's approval: the call waits until a manager approves
          or rejects it in the back office, which can take a while. If it is still waiting, call
          again later with exactly the same arguments to get the outcome.
        tags: acme
        parameter.customerId: integer
        parameter.customerId.description: The customer id from search_customers
        parameter.customerId.required: true
        parameter.amount: number
        parameter.amount.description: Amount of credit, at most 200
        parameter.amount.required: true
        parameter.reason: string
        parameter.reason.description: Why the credit is given
        parameter.reason.required: true
        approval: always
        approvalEndpoint: direct:manager-queue
        approvalTimeout: 1h
      steps:
        - choice:
            when:
              - simple: "${header.amount} <= 0 || ${header.amount} > 200"
                steps:
                  - setHeader:
                      name: CamelAiToolIsError
                      constant: true
                  - setBody:
                      constant: "Store credit must be more than 0 and at most 200."
            otherwise:
              steps:
                - to: "sql:insert into store_credits (customer_id, amount, reason, approved_by, created) values (:#customerId, :#amount, :#reason, :#CamelAiToolApprovedBy, '{{acme.today}}') returning id?dataSource=jdbc:rustsql:mem:acme&outputType=SelectOne"
                - setBody:
                    simple: '{"storeCredit": "SC-${body}", "customerId": ${header.customerId}, "amount": ${header.amount}, "approvedBy": "${header.CamelAiToolApprovedBy}", "status": "credited"}'
```

<!-- example: mcp-shop-manager-approval-manager-queue -->
```yaml
- route:
    id: manager-queue
    from:
      uri: direct:manager-queue
      steps:
        - to: "sql:insert into manager_queue values (:#CamelAiToolApprovalHandle, :#CamelAiToolName, :#customerId, :#amount, :#reason, '{{acme.today}}')?dataSource=jdbc:rustsql:mem:acme"
```

- **Asking again is safe.** A call with the same arguments in the same conversation returns
  "still waiting" with the same handle, or the decision once there is one. The manager is asked
  once.
- **The description tells the model what to expect.** "The call waits until a manager approves
  or rejects it [...] call again later with exactly the same arguments" is what made Claude
  check back instead of giving up or retrying with other arguments.
- **The route still checks its rules after approval.** A credit over 200 is refused even if a
  manager approves it.
- **Decisions are audited** with the approver's name, in the same hash-chained log as every
  call.

Operators can also decide from a program (`AiRegistry::approve` and `deny`), from another route
(`ai-tool:x?operation=approve`), and inside a container with `docker exec`; see
[MCP server: approvals](../mcp-server.md#approvals).

## Try it

```bash
cd examples/agents/mcp-shop
cameleer run shop.camel.yaml --properties application.properties
```

Ask a connected client for goodwill credit, then run `cameleer tool approvals` and
`cameleer tool approve <handle> --by <name>` in another terminal (or
`cameleer tool deny <handle> --reason <text>`). `shop_use_cases` in
`crates/cameleer-main/tests/mcp_shop.rs` approves one credit, rejects another and checks what
the next call returns.

## Related

- [Actions guarded by approvals](approvals.md): the user approves in the client instead
- [The tool asks the user](the-tool-asks-the-user.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/cameleering/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
