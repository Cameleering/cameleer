# AMQP-STOMP component

> The `amqp-stomp` component is Cameleer's JMS-over-STOMP component under a third name, for routes that used Cameleer's former STOMP-based `amqp` scheme.

**Scheme:** `amqp-stomp:` · **Crate:** `cameleer-amqp` · **Cargo feature:** [`amqp`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** none (Cameleer only; see [JMS](https://camel.apache.org/components/next/jms-component.html))

## When to use it

- You have routes from an earlier Cameleer version where `amqp:` meant JMS over STOMP. Rename the scheme to
  `amqp-stomp:` and keep the rest.
- You want JMS semantics through a STOMP connector and also use [amqp](amqp.md) (AMQP 1.0) in the same
  application, with separate component settings.

For new routes use [jms](jms.md) (STOMP) or [amqp](amqp.md) (AMQP 1.0, Camel's meaning of `amqp:`).

## URI format

```text
amqp-stomp:[queue:|topic:|temp-queue:|temp-topic:]destinationName[?options]
```

## Example

<!-- example: amqp-stomp-work-queue -->
```yaml
- route:
    id: work-queue
    from:
      uri: amqp-stomp:queue:jobs
      parameters:
        brokerURL: "{{env:STOMP_BROKER_URL:tcp://localhost:61613}}"
        concurrentConsumers: 3
      steps:
        - log: "job ${body}"
        - to: log:jobs
```

Three consumers compete for the messages on the queue `jobs`.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("amqp-stomp:queue:jobs?brokerURL=tcp://localhost:61613&concurrentConsumers=3")
            .route_id("work-queue")
            .log("job ${body}")
            .to("log:jobs");
    })
}
```

## Options

The same as [jms](jms.md#options). Component-wide settings use the prefix `camel.component.amqp-stomp.`;
the broker URL may also be given as `camel.component.amqp-stomp.remoteURI`.

## Message headers

The same as [jms](jms.md#message-headers).

## Run it with Docker

```sh
docker run --rm -e STOMP_BROKER_URL=tcp://broker:61613 -v "$PWD:/routes:ro" cameleering/cameleer run /routes/jobs.camel.yaml
```

## Common questions

### Why is there an amqp-stomp scheme at all?

Apache Camel's `amqp:` is JMS over AMQP 1.0. When Cameleer gained an AMQP 1.0 stack, `amqp:` took that
meaning and the older STOMP flavour moved to `amqp-stomp:`.

## Related

- [jms](jms.md), [amqp](amqp.md), [stomp](stomp.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
