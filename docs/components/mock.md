# Mock component

> The `mock` component records the messages a route sends to it and checks them against expectations, so you can test routes without real systems.

**Scheme:** `mock:` · **Crate:** `cameleer-core` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Mock](https://camel.apache.org/components/next/mock-component.html)

## When to use it

- Unit-test routing logic: send test messages in, assert what reached each `mock:` endpoint.
- Replace a real endpoint during a test (see the [stub](stub.md) component for a queue instead).
- Check counts, bodies, order, headers and properties, with a wait time for asynchronous routes.

## URI format

```text
mock:name[?options]
```

All endpoints with the same `name` share one recorder in a context.

## Example

<!-- example: mock-temperature-routing -->
```yaml
- route:
    id: route-temperatures
    from:
      uri: direct:temperature
      steps:
        - choice:
            when:
              - simple: "${body} > 80"
                steps:
                  - to: mock:alerts
            otherwise:
              steps:
                - to: mock:normal
```

A test sends `85` and `20` to `direct:temperature` and expects one message at `mock:alerts` and one at
`mock:normal`:

```rust
let alerts = ctx.mock_endpoint("mock:alerts")?;
alerts.expected_message_count(1);
let template = ctx.create_producer_template();
template.send_body("direct:temperature", 85)?;
template.send_body("direct:temperature", 20)?;
alerts.assert_is_satisfied()?;
```

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:temperature")
            .route_id("route-temperatures")
            .choice()
            .when(simple("${body} > 80"))
            .to("mock:alerts")
            .otherwise()
            .to("mock:normal")
            .end();
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `expectedCount` | | Expect exactly this many messages. |
| `retainFirst` | | Keep only the first N exchanges (counts stay exact). |
| `retainLast` | | Keep only the last N exchanges. |
| `assertPeriod` | | After the expectations are met, keep checking for this long (catches extra messages). |
| `resultWaitTime` | | Maximum time `assert_is_satisfied` waits for the expectations. |
| `log`, `reportGroup` | | Accepted and ignored. |

Expectations are set from Rust on the `MockEndpoint` (from `ctx.mock_endpoint(uri)`):
`expected_message_count`, `expected_minimum_message_count`, `expected_bodies_received`,
`expected_bodies_received_in_any_order`, `expected_header_received`, `expected_property_received`,
`expects_no_duplicate_bodies`, `message(i, predicate)`, `all_messages(predicate)`, `expects(fn)`.
Behaviour: `return_reply_body`, `when_any_exchange_received`, `when_exchange_received`. Inspection:
`received_counter`, `received_exchanges`, `received_bodies`, `wait_for_messages`. Asserts:
`assert_is_satisfied`, `assert_is_satisfied_within`, `assert_is_not_satisfied`,
`MockEndpoint::assert_all_satisfied`.

## Message headers

None added. The recorded exchanges keep their headers for inspection.

## Run it with Docker

Mock endpoints are for tests. In a container they accept and record messages in memory:

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/temperatures.camel.yaml
```

## Common questions

### How do I test a YAML route with mocks?

Load the YAML with `cameleer_main::new_main()` and `add_route_source`, start it, send messages with
`ctx.create_producer_template()`, and assert on `ctx.mock_endpoint("mock:...")`.

### How do I make a mock reply like a real service?

Call `return_reply_body(constant("..."))` or `when_any_exchange_received(processor)` on the mock.

### Why does my assertion pass even though extra messages arrive later?

Assertions succeed as soon as the expectations hold. Set `assertPeriod` (or `set_assert_period`) to keep
checking for a while.

## Related

- [stub](stub.md), [dataset](dataset.md), [dataset-test](dataset-test.md), [browse](browse.md)
- [Apache Camel Mock component](https://camel.apache.org/components/next/mock-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
