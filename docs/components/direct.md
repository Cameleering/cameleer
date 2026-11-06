# Direct component

> The `direct` component calls another route synchronously, in the same thread, inside the same Cameleer process.

**Scheme:** `direct:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Direct](https://camel.apache.org/components/next/direct-component.html)

## When to use it

- Split a long route into small named sub-routes (`direct:validate`, `direct:enrich`) and call them like functions.
- Share one piece of logic between several routes, for example one validation route used by a REST API and a Kafka consumer.
- Give tests and tools an entry point: send a message to `direct:...` with a producer template and read the reply.
- Expose a tool to an AI agent as a request/reply route: the caller blocks until the route returns its answer.

## URI format

```text
direct:name[?options]
```

`name` identifies the route that consumes from it. Only one route can consume from a given `direct:` name.

## Example

<!-- example: direct-subroute -->
```yaml
- route:
    id: orders
    from:
      uri: direct:orders
      steps:
        - to: direct:validate
        - setBody:
            simple: "accepted ${body}"
- route:
    id: validate
    from:
      uri: direct:validate
      steps:
        - filter:
            simple: "${body} == ''"
            steps:
              - throwException:
                  exceptionType: java.lang.IllegalArgumentException
                  message: empty order
        - setHeader:
            name: validated
            constant: true
```

Sending `o-1` to `direct:orders` returns `accepted o-1`. Sending an empty body fails the exchange with
`IllegalArgumentException`, and the error travels back to the caller because the call is synchronous.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:orders")
            .route_id("orders")
            .to("direct:validate")
            .set_body(simple("accepted ${body}"));
        b.from("direct:validate")
            .route_id("validate")
            .filter(simple("${body} == ''"))
            .throw_exception("java.lang.IllegalArgumentException", "empty order")
            .end()
            .set_header("validated", constant(true));
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `block` | `true` | When no consumer is running, wait up to `timeout` for one to start instead of failing at once. |
| `timeout` | `30000` | How long (ms, or a duration such as `5s`) a blocked producer waits for a consumer. |
| `failIfNoConsumers` | `true` | Fail with `DirectConsumerNotAvailableException` when there is no consumer. With `false` the message is dropped silently. |
| `synchronous` | | Accepted for compatibility; direct is always synchronous. |

## Message headers

The direct component does not add headers. Headers, properties and the body pass to the called
route unchanged, and the called route's changes are visible to the caller.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml
```

A `direct:` route only runs when something calls it, so combine it with a consumer such as
[timer](timer.md), [kafka](kafka.md) or a REST endpoint in the same file.

## Common questions

### What is the difference between direct and seda?

`direct` runs the called route in the caller's thread and returns its result. [seda](seda.md) puts
the message on an in-memory queue and a different thread processes it later.

### Why do I get DirectConsumerNotAvailableException?

No route consumes from that `direct:` name, or the route is stopped. Check the name for typos, or set
`failIfNoConsumers=false` if dropping the message is acceptable.

### Can I call a direct route in another Cameleer process?

No. `direct` works inside one process (one `CamelContext`). Use [vm](vm.md) for other contexts in the
same process, and a broker such as [kafka](kafka.md) or a network component between processes.

## Related

- [seda](seda.md), [vm](vm.md): asynchronous in-process queues
- [EIPs](../guides/eips.md)
- [Apache Camel Direct component](https://camel.apache.org/components/next/direct-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
