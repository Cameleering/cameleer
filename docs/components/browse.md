# Browse component

> The `browse` component keeps the messages sent to it in memory so they can be listed later, and passes them on to a consuming route if there is one.

**Scheme:** `browse:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Browse](https://camel.apache.org/components/next/browse-component.html)

## When to use it

- Keep the last rejected or failed messages for inspection while debugging.
- Show recent messages of a route on a status page or to an operator tool.
- Record what passed a point in a route during development.

## URI format

```text
browse:name[?options]
```

## Example

<!-- example: browse-rejected-orders -->
```yaml
- route:
    id: record-rejections
    from:
      uri: direct:reject
      steps:
        - setHeader:
            name: rejectedAt
            simple: "${date:now:yyyy-MM-dd'T'HH:mm:ss}"
        - to: browse:rejected?browseLimit=100
```

The last 100 rejected messages are kept. List them from Rust with
`cameleer_core::component::browse::browse_exchanges(&ctx, "browse:rejected?browseLimit=100")`.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:reject")
            .route_id("record-rejections")
            .set_header("rejectedAt", simple("${date:now:yyyy-MM-dd'T'HH:mm:ss}"))
            .to("browse:rejected?browseLimit=100");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `browseLimit` | unlimited | Maximum number of exchanges kept; older ones are dropped. |
| `synchronous` | | Accepted and ignored. |

## Message headers

None added.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/rejections.camel.yaml
```

## Common questions

### Does browse keep messages after a restart?

No. They are in memory only. Write them to a file or a broker when you need to keep them.

### Can another route consume from a browse endpoint?

Yes. `from: browse:rejected` receives each message synchronously as it is sent, and the message is still
kept for browsing.

## Related

- [mock](mock.md), [log](log.md), [seda](seda.md)
- [Apache Camel Browse component](https://camel.apache.org/components/next/browse-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
