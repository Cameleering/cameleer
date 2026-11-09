# Testing routes: validate, cameleer test and Rust tests

> Test Cameleer routes at three levels: `cameleer validate` checks route files without running them, `cameleer test` runs routes from a YAML test file and checks bodies, headers and mock endpoints without writing code, and Rust tests drive routes with a `ProducerTemplate` and assert on `mock:` endpoints, as Camel's Java tests do. All three run without the real Kafka, database or cloud service.

**Applies to:** YAML and XML routes (`cameleer validate`, `cameleer test`) and the Rust DSL (`cargo test`) · **Apache Camel:** [Testing](https://camel.apache.org/manual/testing.html), [Mock](https://camel.apache.org/components/next/mock-component.html)

| level | command | what it catches | needs |
|---|---|---|---|
| static | `cameleer validate routes/` | YAML and XML syntax, unknown EIPs and options, bad endpoint URIs, broken expressions, unresolved placeholders, dangling `direct:` endpoints, duplicate route ids | nothing runs |
| behaviour, no code | `cameleer test routes/orders.test.yaml` | what a route does with a message: the body it returns, headers, which mock endpoints received what, errors; tool calls and approvals for AI routes | the routes run in-process |
| behaviour, in Rust | `cargo test` | everything the YAML tests catch, plus any assertion you can write in Rust | a Rust project |

## Validate every route file

`cameleer validate` loads route files the way `cameleer run` would, resolves every endpoint URI
against the real components (an unknown option is an error, as in Camel), and starts nothing.
It exits with 0 when everything is valid, 1 on errors and 2 on a usage error, so it fits a CI
step or a pre-commit hook. `--format json` gives a report a coding agent can act on. The codes
and the format are in [Validate route files](validate.md).

```bash
cameleer validate routes/
cameleer validate routes/ --format json --strict    # warnings fail too
```

## cameleer test: YAML tests without code

A test file (`*.test.yaml`) lists tests. Each test loads route files, sets properties, sends
messages and checks what came back. `cameleer run` and `cameleer validate` skip test files, so they
can live next to the routes.

### A worked example

This route sends high-priority orders to one Kafka topic and the rest to another. The topics are
placeholders with the real endpoint as the default, so a test can swap them for `mock:`
endpoints without editing the route:

<!-- file: orders.camel.yaml -->
<!-- example: testing-order-lanes -->
```yaml
- route:
    id: order-lanes
    from:
      uri: direct:orders
      steps:
        - choice:
            when:
              - simple: "${body[priority]} == 'high'"
                steps:
                  - setHeader:
                      name: lane
                      constant: express
                  - to: "{{orders.express:kafka:orders-express?brokers=localhost:9092}}"
            otherwise:
              steps:
                - setHeader:
                    name: lane
                    constant: standard
                - to: "{{orders.standard:kafka:orders?brokers=localhost:9092}}"
        - setBody:
            simple: "accepted ${body[id]}"
```

The test sets the two placeholders to mock endpoints and sends two orders:

<!-- file: orders.test.yaml -->
<!-- not-camel -->
```yaml
- test:
    name: a high-priority order takes the express lane
    routes: [orders.camel.yaml]
    properties:
      orders.express: mock:express
      orders.standard: mock:standard
    steps:
      - send:
          to: direct:orders
          body: {id: A1, priority: high}
        expect:
          body: accepted A1
          headers: {lane: express}
          mocks: {"mock:express": 1, "mock:standard": 0}
      - send:
          to: direct:orders
          body: {id: A2, priority: low}
        expect:
          body: accepted A2
          headers: {lane: standard}
          mocks: {"mock:express": 1, "mock:standard": 1}
```

```bash
cameleer test orders.test.yaml
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer test /routes/orders.test.yaml
```

It prints `PASS a high-priority order takes the express lane` and `1 passed, 0 failed`, and
exits with 0; a failing check prints `FAIL`, the step number and what differed, and exits with 1.
Mock counts add up over the steps of one test, and each test starts with fresh routes. This
example runs in the test suite (`testing_guide_worked_example` in
`crates/cameleer-main/tests/docs_reference.rs`).

### The test file

| key | what it does |
|---|---|
| `name` | printed with `PASS` or `FAIL` |
| `routes` | route files to load, relative to the test file (a list, or one file) |
| `properties` | configuration keys and placeholder values for this test; a `file:` value is relative to the test file. `application.properties` is not read, so the test says everything it depends on |
| `steps` | run in order; a step without `expect` fails if the route throws |

| step | what it does |
|---|---|
| `send` | `to` an endpoint, with `body` (text or any YAML value: a map arrives as an object, as JSON would) and `headers`; the body and headers at the end of the route are what `expect` checks |
| `callTool` | call an [`ai-tool`](../components/ai-tool.md) through the tool envelope: `name`, `arguments`, `callId`, `conversation`, `principal`, `approvedBy` |
| `approve`, `deny` | decide the oldest call of `tool` waiting for approval, with `by` and (for `deny`) `reason` |
| `compensate` | run the compensations registered in a `conversation` |

| `expect` key | checks |
|---|---|
| `body` | the body equals this (objects and lists compared as JSON) |
| `bodyContains` | the body contains this text |
| `headers` | each header has this value |
| `error` | the route failed with a message containing this text |
| `mocks` | how many messages each mock endpoint received so far |
| `status`, `isError`, `duplicate` | tool results: the envelope's status, whether the result is an error, whether a repeated call returned the first result |

`callTool`, `approve`, `deny` and `compensate` need the `ai` feature, which default builds have.
For agent routes, the model's responses can be recorded once and replayed, so the test needs no
model and no API key: see [the agent step](agent-step.md).

### Replace real endpoints in a test

Two ways, neither edits the route under test:

- **Placeholders**, as above: write the endpoint as `{{orders.express:kafka:...}}` and set
  `orders.express: mock:express` in the test's `properties`. Clear and explicit, and it also
  makes the endpoint configurable per environment.
- **An intercept file** listed in the test's `routes` before the routes it applies to. Camel's
  `interceptSendToEndpoint` with `skipSendToOriginalEndpoint` catches every message sent to the
  matching endpoints and sends it to a mock instead:

<!-- file: intercept-kafka.camel.yaml -->
<!-- example: testing-intercept-kafka -->
```yaml
- interceptSendToEndpoint:
    uri: "kafka:*"
    skipSendToOriginalEndpoint: true
    steps:
      - to: mock:kafka
```

  With `routes: [intercept-kafka.camel.yaml, orders.camel.yaml]`, the orders above reach
  `mock:kafka` and nothing connects to a broker. The intercept applies only to route files
  loaded after it, so list it first.

## Rust tests

In Rust, a test builds a `CamelContext`, adds routes, sends messages with a `ProducerTemplate`
and asserts on `MockEndpoint`s, the same pattern as Camel's `CamelTestSupport`:

```rust
use cameleer_core::prelude::*;

#[test]
fn high_priority_orders_take_the_express_lane() -> Result<()> {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:orders")
            .choice()
            .when(header("priority").is_equal_to("high"))
            .to("mock:express")
            .otherwise()
            .to("mock:standard")
            .end();
    })?;
    ctx.start()?;

    let express = ctx.mock_endpoint("mock:express")?;
    express.expected_message_count(1);
    express.expected_bodies_received(vec!["A1"]);

    let template = ctx.create_producer_template();
    template.send_body_and_header("direct:orders", "A1", "priority", "high")?;
    template.send_body_and_header("direct:orders", "A2", "priority", "low")?;

    express.assert_is_satisfied()?;
    assert_eq!(ctx.mock_endpoint("mock:standard")?.received_counter(), 1);
    ctx.stop()
}
```

This test runs in the suite as written (`testing_guide_rust_example`, which also checks that this
page shows the same code).

**Sending.** `ProducerTemplate` has Camel's methods with Rust casing: `send_body`,
`send_body_and_header`, `send_body_and_headers` (one-way), `request_body` and
`request_body_and_header` (request-reply, returns the reply body), `request` (build the exchange
in a closure, get the whole reply exchange back), and `async_request_body`. `ConsumerTemplate`
(`ctx.create_consumer_template()`) reads from a polling endpoint: `receive_body("seda:out",
timeout)`.

**Mock expectations.** `expected_message_count`, `expected_minimum_message_count`,
`expected_bodies_received` (in order) and `expected_bodies_received_in_any_order`,
`expected_header_received`, `expected_property_received`, `expects_no_duplicate_bodies`,
`message(i, predicate)`, `all_messages(predicate)` and `expects(closure)` set expectations;
`assert_is_satisfied()` waits up to the result wait time (`set_result_wait_time`) for them and
returns an error that says what differed, in Camel's wording. `received_bodies()`,
`received_exchanges()` and `wait_for_messages(n, timeout)` read what arrived. A mock can also
answer like the real service: `return_reply_body(...)`, `when_any_exchange_received(processor)`.
Every option is on the [mock component](../components/mock.md) page.

**YAML routes from Rust.** To test YAML route files with Rust assertions, load them with
`cameleer_main::new_main()`, `set_property` the placeholders to mock endpoints, then
`add_route_file(...)` and `start()`, and use `main.context()` as above.

**No real servers.** Cameleer's own suite (2,800+ tests) runs hermetically: the components are
tested against embedded servers written for the tests, never the network. For your routes,
`mock:`, `seda:` and `direct:` endpoints stand in for the outside world, and placeholders
switch endpoints between the test and production.

## In CI

A job needs only the binary or the Docker image:

<!-- not-camel -->
```yaml
# .github/workflows/routes.yml
name: routes
on: [push, pull_request]
jobs:
  routes:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Validate route files
        run: docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer validate /routes/routes
      - name: Run route tests
        run: docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer test /routes/routes/orders.test.yaml
```

Both commands exit with a non-zero code on failure, which fails the job. Pin the image to a
version tag (`pmsudhi/cameleer:0.5.1`) for repeatable builds.

## Common questions

### How do I test a route that calls Kafka, a database or a cloud service?

Replace the endpoint with a mock in the test, through a placeholder or an intercept file (see
[Replace real endpoints](#replace-real-endpoints-in-a-test)). The route itself stays as it runs in
production.

### Does Cameleer have Camel's `adviceWith`?

No. Use placeholders for the endpoints you want to swap, an `interceptSendToEndpoint` with
`skipSendToOriginalEndpoint` for whole families of endpoints, or `direct:` starts that a test can
send to directly.

### Can I test a route that starts from a timer, a file or a queue?

Give the consumer a placeholder too (`from: uri: "{{orders.in:file:inbox}}"`) and set it to
`direct:orders-in` in the test, then `send` to that. Or keep the real consumer when it is local:
a `file:` route can be tested by writing files into a temporary folder.

### How do I test an AI agent without calling a model?

Record the model's responses once with `record: file:...` on the agent step or the model
component, then replay them in `cameleer test` with `replay: file:...` (set through a test
property); tool calls, approvals and compensation are checked
with `callTool`, `approve` and `compensate`. See [the agent step](agent-step.md).

### How do I check that my routes load before deploying?

`cameleer validate` in CI or a pre-commit hook; it starts nothing and needs no credentials. A
coding agent can read its `--format json` output and fix what it reports.

## Related

- [Validate route files](validate.md), [the cameleer command line](cli.md),
  [Configuration](configuration.md)
- [mock](../components/mock.md), [dataset](../components/dataset.md) and
  [test](../components/test.md) components
- [The agent step](agent-step.md): record and replay for agent routes

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
