# Stub component

> The `stub` component stands in for any endpoint: it accepts any URI and options and queues the messages in memory like [seda](seda.md), so you can run a route without the real system.

**Scheme:** `stub:` · **Crate:** `cameleer-core` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Stub](https://camel.apache.org/components/next/stub-component.html)

## When to use it

- Run a route locally when a downstream system (mail server, cloud queue, payment API) is not reachable.
- Try a route in a test or a demo without credentials: prefix the real URI with `stub:`.
- Check what a route would send: consume from the same `stub:` URI and inspect the messages.

## URI format

```text
stub:anyScheme:anything[?anyOptions]
```

Everything after `stub:` is kept as the queue name and is never validated.

## Example

<!-- example: stub-mail-server -->
```yaml
- route:
    id: notify-shipping
    from:
      uri: direct:notify
      steps:
        - setBody:
            simple: "Order ${body} shipped"
        - to: stub:smtp://mail.example.com?to=ops@example.com
```

The real mail server is never contacted. The message waits on an in-memory queue, where another route
or a consumer template can read it.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:notify")
            .route_id("notify-shipping")
            .set_body(simple("Order ${body} shipped"))
            .to("stub:smtp://mail.example.com?to=ops@example.com");
    })
}
```

## Options

None. All options are accepted and ignored, and the queue uses the [seda](seda.md) defaults
(1000 messages, one consumer).

## Message headers

None added.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/notify.camel.yaml
```

## Common questions

### How do I see what was sent to a stub endpoint?

Consume from the exact same URI, for example `from: stub:smtp://mail.example.com?to=ops@example.com`
followed by `log:`, or poll it with a consumer template in a test.

### Is there a way to switch between stub and the real endpoint?

Put the scheme in a property placeholder, for example `to: "{{mail.prefix:stub:}}smtp://..."`, and set
`mail.prefix` to an empty value in production.

## Related

- [seda](seda.md), [mock](mock.md)
- [Apache Camel Stub component](https://camel.apache.org/components/next/stub-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
