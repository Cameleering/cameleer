# Test component

> The `test` component is the short name of [dataset-test](dataset-test.md): it checks a route's output against expected messages read from another endpoint.

**Scheme:** `test:` · **Crate:** `cameleer-core` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [DataSet Test](https://camel.apache.org/components/next/dataset-test-component.html)

## When to use it

- Move older Camel tests that use `test:file:...` without changing them.
- Compare a route's output with a file of expected lines, in any order.

## URI format

```text
test:expectedEndpointUri[?options]
```

## Example

<!-- example: test-any-order -->
```yaml
- route:
    id: enrich-devices
    from:
      uri: direct:device
      steps:
        - setBody:
            simple: "device:${body}"
        - to: test:file:{{env:EXPECTED_DIR:expected}}?noop=true&split=true&anyOrder=true
```

With a file that lists `device:a` and `device:b`, the messages may arrive in either order.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:device")
            .route_id("enrich-devices")
            .set_body(simple("device:${body}"))
            .to("test:file:expected?noop=true&split=true&anyOrder=true");
    })
}
```

## Options

The same as [dataset-test](dataset-test.md#options): `timeout`, `anyOrder`, `split`, `delimiter`. Other
options go to the expected endpoint URI.

## Message headers

None added.

## Run it with Docker

Used from Rust tests (`cameleer_core::component::dataset::dataset_test_endpoint`).

## Common questions

### Is test the same as dataset-test?

Yes. Both schemes are the same component, as in Apache Camel 2 where `test:` was the original name.

## Related

- [dataset-test](dataset-test.md), [dataset](dataset.md), [mock](mock.md)
- [Apache Camel DataSet Test component](https://camel.apache.org/components/next/dataset-test-component.html)
