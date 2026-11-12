# Interceptors, completions, sagas and reusable routes

> These patterns act around routes rather than on one message: interceptors that see every
> send or every input, `onCompletion` callbacks, sagas with compensation, route templates and
> Kamelets for reuse, the `poll` and `stop` EIPs.

Every example runs in the test suite (YAML, and Rust DSL where shown). See
[the EIP guide](../eips.md) for the other patterns.

## Intercept sends to an endpoint

`interceptSendToEndpoint` runs its steps before every send to endpoints matching a pattern
(wildcards allowed). With `skipSendToOriginalEndpoint: true` the original endpoint is not
called at all, which is how you swap a real payment or model API for a sandbox in a test
profile.

<!-- example: eip-intercept-send -->
```yaml
- routeConfiguration:
    interceptSendToEndpoint:
      - interceptSendToEndpoint:
          uri: "mock:payment-api*"
          skipSendToOriginalEndpoint: true
          steps:
            - setBody:
                simple: "sandbox charge for ${body}"
            - to: mock:sandbox
- route:
    from:
      uri: direct:checkout
      steps:
        - to: mock:payment-api
        - to: mock:receipts
```

```rust
b.intercept_send_to_endpoint("mock:payment-api*")
    .skip_send_to_original_endpoint()
    .set_body(simple("sandbox charge for ${body}"))
    .to("mock:sandbox");
b.from("direct:checkout")
    .to("mock:payment-api")
    .to("mock:receipts");
```

`mock:payment-api` receives nothing; `mock:sandbox` and `mock:receipts` get
`sandbox charge for order-9`. `afterUri` sends to another endpoint after the original one, and
`onWhen` limits the interception to matching messages.

## Intercept every input

`interceptFrom` runs before the first step of every route whose input matches the pattern
(`*` for all). `intercept` runs before every step of every route.

<!-- example: eip-intercept-from -->
```yaml
- routeConfiguration:
    interceptFrom:
      - interceptFrom:
          uri: "direct:*"
          steps:
            - setHeader:
                name: receivedBy
                constant: edge-gateway-1
- route:
    from:
      uri: direct:sensor
      steps:
        - to: mock:stamped
```

```rust
b.intercept_from("direct:*")
    .set_header("receivedBy", constant("edge-gateway-1"));
b.from("direct:sensor")
    .to("mock:stamped");
```

Every message reaching `mock:stamped` carries the `receivedBy` header.

## On completion

`onCompletion` runs its steps after the exchange is done, on a copy, without changing the
reply. `onCompleteOnly` and `onFailureOnly` choose when it runs, `onWhen` adds a predicate, and
`mode: BeforeConsumer` runs it before the consumer answers.

<!-- example: eip-on-completion -->
```yaml
- route:
    from:
      uri: direct:place-order
      steps:
        - onCompletion:
            onFailureOnly: true
            steps:
              - setBody:
                  simple: "order failed: ${body}"
              - to: mock:failed-orders
        - filter:
            simple: "${body} contains 'invalid'"
            steps:
              - throwException:
                  exceptionType: java.lang.IllegalArgumentException
                  message: bad order
        - to: mock:orders
```

```rust
b.from("direct:place-order")
    .on_completion()
        .on_failure_only()
        .set_body(simple("order failed: ${body}"))
        .to("mock:failed-orders")
    .end()
    .filter(simple("${body} contains 'invalid'"))
        .throw_exception("IllegalArgumentException", "bad order")
    .end()
    .to("mock:orders");
```

The order `invalid-1` fails, and `mock:failed-orders` receives `order failed: invalid-1`; the
order `ok-1` goes to `mock:orders` and triggers nothing.

## Saga

A saga groups steps that call different services and registers a compensation for them. If
the exchange fails, the compensation endpoints are called (in reverse order of the steps that
joined), so you can undo bookings, charges or reservations without a distributed transaction.
On success, the optional `completion` endpoints are called.

<!-- example: eip-saga -->
```yaml
- route:
    from:
      uri: direct:book-trip
      steps:
        - saga:
            compensation:
              uri: direct:cancel-flight
            steps:
              - to: mock:flight-booked
              - filter:
                  simple: "${header.hotelFull} == 'true'"
                  steps:
                    - throwException:
                        exceptionType: java.lang.IllegalStateException
                        message: no hotel rooms
- route:
    from:
      uri: direct:cancel-flight
      steps:
        - to: mock:flight-cancelled
```

```rust
b.from("direct:book-trip")
    .saga()
        .compensation("direct:cancel-flight")
        .to("mock:flight-booked")
        .filter(simple("${header.hotelFull} == 'true'"))
            .throw_exception("IllegalStateException", "no hotel rooms")
        .end()
    .end();
b.from("direct:cancel-flight")
    .to("mock:flight-cancelled");
```

A booking with `hotelFull=true` fails and `mock:flight-cancelled` is called once. The default
saga service is in memory; `LraSagaService` talks to a Narayana LRA coordinator over plain HTTP.
Options: `propagation`, `completionMode` (`AUTO`, `MANUAL` with `saga:complete` and
`saga:compensate`), `timeout`, `option` (values passed to the compensation as headers).

## Route templates

A route template is a route with `{{parameters}}`; `templatedRoute` creates routes from it.
Use templates to stamp out the same integration for many tenants, devices or models.

<!-- example: eip-route-template -->
```yaml
- routeTemplate:
    id: ask-model
    parameters:
      - name: name
      - name: prefix
        defaultValue: "Answer briefly:"
    route:
      from:
        uri: "direct:{{name}}"
        steps:
          - setBody:
              simple: "{{prefix}} ${body}"
          - to: "mock:{{name}}"
- templatedRoute:
    routeTemplateRef: ask-model
    parameters:
      - name: name
        value: support-bot
- templatedRoute:
    routeTemplateRef: ask-model
    parameters:
      - name: name
        value: sales-bot
      - name: prefix
        value: "Answer politely:"
```

```rust
ctx.add_route_template(
    "ask-model",
    &[TemplateParam::required("name"), TemplateParam::optional("prefix", "Answer briefly:")],
    |b, p| {
        b.from(format!("direct:{}", p.get("name")))
            .set_body(simple(format!("{} ${{body}}", p.get("prefix"))))
            .to(format!("mock:{}", p.get("name")));
    },
);
TemplatedRouteBuilder::new(ctx, "ask-model").parameter("name", "support-bot").add()?;
TemplatedRouteBuilder::new(ctx, "ask-model").parameter("name", "sales-bot").parameter("prefix", "Answer politely:").add()?;
```

`direct:support-bot` prefixes messages with `Answer briefly:`, `direct:sales-bot` with
`Answer politely:`. Templates can declare beans (`beans:`), and `prefixId` keeps node ids apart.

## Kamelets

A Kamelet is a route template packaged as a component: `kamelet:name?param=value` as a source,
a sink or an action. Built-in Kamelets: `timer-source`, `log-sink`, `log-action`,
`insert-header-action` and `set-body-action`; more are loaded from `camel.main.kamelet-locations`
(default `./kamelets`). The `kamelet` EIP calls one as a step.

<!-- example: eip-kamelet -->
```yaml
- route:
    from:
      uri: direct:tag-reading
      steps:
        - kamelet:
            name: insert-header-action
            parameters:
              name: site
              value: plant-7
        - to: mock:tagged
```

```rust
b.from("direct:tag-reading")
    .kamelet("insert-header-action?name=site&value=plant-7")
    .to("mock:tagged");
```

Every message reaching `mock:tagged` has the header `site=plant-7`. Pipes (`kind: Pipe`) and
KameletBindings connecting Kamelets run in-process too.

## Poll

`poll` consumes one message from an endpoint in the middle of a route and replaces the body
with it (Camel 4's `poll` EIP). The timeout is in milliseconds (default 20 seconds).

<!-- example: eip-poll -->
```yaml
- route:
    from:
      uri: direct:next-job
      steps:
        - poll:
            uri: seda:jobs
            timeout: 1000
        - to: mock:job
```

```rust
b.from("direct:next-job")
    .poll("seda:jobs", 1000)
    .to("mock:job");
```

## Stop

`stop` ends the routing of the current message: no later steps run. Inside a `split`, it stops
only the current part.

<!-- example: eip-stop -->
```yaml
- route:
    from:
      uri: direct:events
      steps:
        - filter:
            simple: "${header.type} == 'heartbeat'"
            steps:
              - stop: {}
        - to: mock:events
```

```rust
b.from("direct:events")
    .filter(simple("${header.type} == 'heartbeat'"))
        .stop()
    .end()
    .to("mock:events");
```

Heartbeats are dropped; every other event reaches `mock:events`.

## Common questions

### How do I mock an external API in a test without changing the route?

Add a `routeConfiguration` with `interceptSendToEndpoint` for the API's URI and
`skipSendToOriginalEndpoint: true`, and load it only in the test profile.

### How do I run something after a message is processed, even if it failed?

Use `onCompletion` (with `onFailureOnly` or `onCompleteOnly` to narrow it), or `doFinally` in a
`doTry` block for code that must run in the same flow.

### How do I reuse one route for many tenants or devices?

Write it once as a `routeTemplate` with parameters and add one `templatedRoute` per tenant, or
package it as a Kamelet.

## Related

- [All EIPs](../eips.md), [error handling](../error-handling.md)
- Apache Camel: [Intercept](https://camel.apache.org/manual/intercept.html),
  [Saga](https://camel.apache.org/components/next/eips/saga-eip.html),
  [Route templates](https://camel.apache.org/manual/route-template.html),
  [Kamelets](https://camel.apache.org/manual/kamelets.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
