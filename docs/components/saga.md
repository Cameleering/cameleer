# Saga component

> The `saga` component completes or compensates the current saga from a route, for sagas started with the Saga EIP.

**Scheme:** `saga:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Saga](https://camel.apache.org/components/next/saga-component.html)

## When to use it

- Undo earlier steps of a long-running business transaction when a later step fails (book hotel, charge
  card, cancel the hotel if the charge fails).
- End a saga with `completionMode: MANUAL` explicitly once an asynchronous confirmation arrives
  (`to: saga:complete`), or roll it back (`to: saga:compensate`).
- Give multi-step AI agent workflows an undo path for actions with side effects.

## URI format

```text
saga:complete
saga:compensate
```

## Example

<!-- example: saga-book-trip -->
```yaml
- route:
    id: book-trip
    from:
      uri: direct:book
      steps:
        - saga:
            compensation:
              uri: direct:cancel-booking
            option:
              - key: bookingId
                expression:
                  simple: "${body}"
            steps:
              - to: direct:reserve-hotel
              - to: direct:charge-card
- route:
    id: reserve-hotel
    from:
      uri: direct:reserve-hotel
      steps:
        - log: "hotel reserved for ${body}"
- route:
    id: charge-card
    from:
      uri: direct:charge-card
      steps:
        - filter:
            simple: "${body} contains 'declined'"
            steps:
              - throwException:
                  exceptionType: java.lang.IllegalStateException
                  message: card declined
- route:
    id: cancel-booking
    from:
      uri: direct:cancel-booking
      steps:
        - log: "cancelling ${header.bookingId}"
        - to: log:compensations
```

Booking `trip-1` completes. Booking `trip-2-declined` fails in `charge-card`, so the saga is compensated:
`direct:cancel-booking` is called with the header `bookingId: trip-2-declined` and the
`Long-Running-Action` header holding the saga id.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:book")
            .route_id("book-trip")
            .saga()
            .compensation("direct:cancel-booking")
            .option("bookingId", simple("${body}"))
            .to("direct:reserve-hotel")
            .to("direct:charge-card")
            .end();
        b.from("direct:reserve-hotel").route_id("reserve-hotel").log("hotel reserved for ${body}");
        b.from("direct:charge-card")
            .route_id("charge-card")
            .filter(simple("${body} contains 'declined'"))
            .throw_exception("java.lang.IllegalStateException", "card declined")
            .end();
        b.from("direct:cancel-booking")
            .route_id("cancel-booking")
            .log("cancelling ${header.bookingId}")
            .to("log:compensations");
    })
}
```

## Options

The `saga:` endpoint has no options; the path is `complete` or `compensate`. The Saga EIP block takes:

| setting | default | description |
|---|---|---|
| `propagation` | `REQUIRED` | `REQUIRED`, `REQUIRES_NEW`, `MANDATORY`, `SUPPORTS`, `NOT_SUPPORTED`, `NEVER`. |
| `completionMode` | `AUTO` | `AUTO` completes when the block succeeds; `MANUAL` waits for `saga:complete` or a timeout. |
| `compensation` | | Endpoint called (in reverse step order) when the saga is compensated. |
| `completion` | | Endpoint called when the saga completes. |
| `option` | | `key` plus an expression, evaluated when the step starts and passed to compensation and completion as headers. |
| `timeout` | | Compensate the saga when it is not finished in time (for example `5s`). |

The default saga service is in memory. A Narayana LRA compatible coordinator client is available from
Rust (`cameleer_core::processor::saga::lra::LraSagaService`).

## Message headers

| header | direction | description |
|---|---|---|
| `Long-Running-Action` | out | The saga id, on compensation and completion calls. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/trip.camel.yaml
```

## Common questions

### Are compensations retried if they fail?

The in-memory service calls each compensation endpoint once per saga step. Make compensation routes
idempotent and add an error handler with redelivery to them.

### Does the saga survive a restart?

Not with the in-memory service. Use an LRA coordinator for sagas that must survive restarts.

## Related

- [EIPs](../guides/eips.md)
- [direct](direct.md)
- [Apache Camel Saga component](https://camel.apache.org/components/next/saga-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
