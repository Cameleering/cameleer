# Ref component

> The `ref` component uses an endpoint looked up by name in the registry, so the real endpoint can be chosen at startup instead of in the route.

**Scheme:** `ref:` · **Crate:** `cameleer-core` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Ref](https://camel.apache.org/components/next/ref-component.html)

## When to use it

- Keep routes independent of the environment: bind `outbound` to `seda:outbound` in tests and to a
  broker URI in production.
- Share one configured endpoint object between several routes.

## URI format

```text
ref:name
```

The registry entry is either an endpoint (`Arc<dyn Endpoint>`) or a `String` holding an endpoint URI.

## Example

<!-- example: ref-outbound -->
```yaml
- route:
    id: publish-events
    from:
      uri: direct:publish
      steps:
        - setHeader:
            name: source
            constant: edge-gw-1
        - to: ref:outbound
```

Bind `outbound` before starting:

```rust
ctx.registry().bind("outbound", "seda:outbound".to_string());
```

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.registry().bind("outbound", "seda:outbound".to_string());
    ctx.add_routes(|b| {
        b.from("direct:publish")
            .route_id("publish-events")
            .set_header("source", constant("edge-gw-1"))
            .to("ref:outbound");
    })
}
```

## Options

None. Options belong to the bound endpoint.

## Message headers

None added.

## Run it with Docker

The stock image has no way to bind registry entries from YAML, so `ref:` is used from Rust programs. In
a container, put the URI in a property placeholder instead: `to: "{{outbound.uri}}"`.

## Common questions

### What happens if nothing is bound under the name?

Creating the endpoint fails with a `NoSuchEndpointException`: "No bean could be found in the registry
for: name of type: Endpoint".

### Should I use ref or property placeholders?

Placeholders (`{{name}}`) work everywhere, including YAML-only deployments. `ref:` is useful when Rust
code builds or chooses the endpoint object.

## Related

- [bean](bean.md), [direct](direct.md)
- [Apache Camel Ref component](https://camel.apache.org/components/next/ref-component.html)
