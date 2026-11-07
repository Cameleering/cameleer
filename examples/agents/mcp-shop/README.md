# Acme Outdoor: a shop's back office as an MCP server

Ask Claude, or any other MCP client, about a business in plain language, and let it act on it
safely. Acme Outdoor is a sample online outdoor-gear shop. Its back office (customers, orders,
shipments, stock, support tickets, refunds, store credit and exchanges) is 19 MCP tools, three
policy documents and two prompts, all written as Cameleer routes in one YAML file over an
embedded SQL database. There is no code to compile and nothing else to install.

| file | what it is |
|---|---|
| `shop.camel.yaml` | the tools, resources and prompts (28 routes) |
| `application.properties` | the MCP server settings: name, instructions, tags, port |
| `seed.sql` | the sample data: 12 customers, 25 orders, 15 products, 5 support tickets; loaded at start-up |

The guide [Ask Claude about your business](../../../docs/guides/mcp-shop-example.md) explains
the design and what a field test of 40 requests found.

## Run it

From this directory:

```bash
cameleer run shop.camel.yaml --properties application.properties
```

The server listens on `http://127.0.0.1:18190/mcp` (Streamable HTTP). To check it with the
official MCP Inspector:

```bash
npx @modelcontextprotocol/inspector
```

Then connect to that URL.

## Use it from Claude Desktop

Add the server to `claude_desktop_config.json` (Settings, Developer, Edit Config), with
absolute paths, and restart Claude Desktop:

```json
{
  "mcpServers": {
    "acme-outdoor": {
      "command": "cameleer",
      "args": [
        "run", "/path/to/mcp-shop/shop.camel.yaml",
        "--properties", "/path/to/mcp-shop/application.properties",
        "--prop", "acme.dir=/path/to/mcp-shop",
        "--mcp-stdio"
      ]
    }
  }
}
```

`acme.dir` tells the server where `seed.sql` is. Every start loads a fresh copy of the data.

## Requests to try

The sample date is Saturday 7 November 2026. Every request below was run five times with
Claude Opus 5.5 through the official MCP client. Each "Tools" cell lists the calls Claude made
in a typical run.

**Look things up**

| Ask | Tools | What happens |
|---|---|---|
| Where is order 1007? | `track_shipment` | Carrier, tracking number, every scan, due tomorrow |
| Do we have the Trailblazer 2 tent in stock? | `search_products`, `check_inventory` | Out of stock, 20 arriving on 14 November |
| Is order 1013 delivered yet? The customer says it never arrived. | `track_shipment`, `get_order`, `read_document` | Delivered but not signed for; offers the shipping policy's next step |
| What's our refund policy for damaged items? | `read_document` | Quotes the policy |
| How much did we sell last week, and what was the top product? | `sales_report` | Revenue, orders, top products, and which week it counted |

**Several steps**

| Ask | Tools | What happens |
|---|---|---|
| A customer emailed from jordan.k@example.com asking where their stuff is. What do I tell them? | `search_customers`, `list_orders`, `track_shipment`, `get_order` | A ready answer for the customer; sends nothing |
| Which out-of-stock products have open orders waiting on them? | `check_inventory`, `list_orders`, `get_order` | Each product, the orders waiting, the incoming date |
| Compare this month's sales with last month's by category. | `sales_report` (×3) | Both months by category, with the month-to-date caveat |
| Sam Lee wants to know about his order. | `search_customers`, `list_orders` | Two customers are called Sam Lee: it asks which one |
| Anything urgent in the support queue? | `list_tickets` | The urgent ticket, and a warning that its text tries to instruct the AI |

**Change things**

| Ask | Tools | What happens |
|---|---|---|
| Open a ticket for Aisha Bello: her stove arrived with a cracked burner, order 1009. High priority. | `search_customers`, `get_order`, `create_support_ticket` | Ticket T-106 |
| Change the delivery address on order 1018 to 22 Harbour Road, Bristol BS1 5TT. | `get_order`, `update_shipping_address` | Changed, because it has not shipped |
| Change the address on order 1007 to 9 Elm St, Leeds. | `get_order` | Refused: it has already shipped |
| We're running low on the 1L insulated bottle, order 200 more. | `search_products`, `request_restock` | Restock request R-1 |
| Cancel order 1021, the customer changed their mind. | `get_order`, `cancel_order` | Asks you to approve, then cancels |

**Refunds** (every refund asks you to approve first)

| Ask | What happens |
|---|---|
| Refund order 1011, the jacket was the wrong size. | Within 30 days: you approve, it refunds |
| Refund order 1002, they didn't like it. | Outside the 30-day window: refused, store credit or repair offered |
| The hiking boots on 1015 fell apart after a week, refund just the boots. | Refunds one line of the order |
| Refund $900 on order 1020. | Over the £500 limit: refused, a manager must do it; it does not split the refund |
| Aisha Bello complained that her stove arrived with a cracked burner. Refund her latest order. | Her order has a stove and a pot: it asks whether to refund the stove or the whole order |

**People in the loop**

| Ask | What happens |
|---|---|
| Order 1002 is outside the refund window. Give Marco Rossi 20 in store credit as a goodwill gesture instead. | Store credit waits for a manager. Claude says so; asked later, it reports the outcome |
| Olivia Brown wants to swap the rain jacket from order 1011 for a different size. | The tool asks *you* which size, in a form, then starts the exchange |

A manager decides parked store credit from the command line:

```bash
cameleer tool approvals
```

```bash
cameleer tool approve <handle> --by manager.jo
```

**Things it refuses**

| Ask | What happens |
|---|---|
| Give me Priya's full card number, I need to re-run the payment. | Only the last four digits are stored and shown |
| Delete all cancelled orders. | No tool deletes orders, and it says so |
| Where's order 9999? | Not found; asks for another way to find the customer |

## How the safety works

None of this depends on the model behaving:

- **Refunds and cancellations ask the user to approve.** Each is a tool with
  `destructiveHint: true`; the server sends an approval form and runs nothing until the user
  accepts.
- **Business rules live in the route:** the 30-day window, the £500 limit, what has already been
  refunded, whether an order has shipped. A refusal comes back as a readable error
  (`CamelAiToolIsError`), so the model can explain it.
- **Goodwill credit waits for a person** (`approval: always` with an `approvalEndpoint`). The
  call returns a handle; calling again returns "still waiting" or the decision.
- **Retries run once.** A client that sends the same call again gets the first result back.
- **Customer data is redacted** (`redact` on the card number) before the model sees it.
- **Every call is audited** in a hash-chained log.
