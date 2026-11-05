# Data Format component

> The `dataformat` component marshals or unmarshals the message body with a data format (JSON, CSV, Base64, ...) as an endpoint, so it can be used anywhere a URI is accepted.

**Scheme:** `dataformat:` · **Crate:** `cameleer-core` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Data Format](https://camel.apache.org/components/next/dataformat-component.html)

## When to use it

- Parse JSON from an MQTT sensor or an HTTP call into a map, then read fields with Simple.
- Encode or compress a payload before sending it (`base64`, `gzipDeflater`).
- Use a data format where only a URI fits, such as `recipientList`, `toD` or `enrich`.

Inside a route, the `marshal` and `unmarshal` steps do the same thing.

## URI format

```text
dataformat:name:marshal|unmarshal[?options]
```

`name` is a data format bound in the registry under that name, or a built-in one (`json`, `csv`,
`base64`, `gzipDeflater`, ...) created with the endpoint options.

## Example

<!-- example: dataformat-parse-sensor-json -->
```yaml
- route:
    id: parse-reading
    from:
      uri: direct:reading
      steps:
        - to: dataformat:json:unmarshal
        - setBody:
            simple: "${body[device]}=${body[temperature]}"
```

Sending `{"device":"t1","temperature":21.5}` returns `t1=21.5`.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:reading")
            .route_id("parse-reading")
            .to("dataformat:json:unmarshal")
            .set_body(simple("${body[device]}=${body[temperature]}"));
    })
}
```

## Options

The options are those of the chosen data format, for example `prettyPrint=true` for `json` or
`delimiter=;` for `csv`. See the data format pages in [../dataformats/](../dataformats/). When the name
refers to a data format bound in the registry, endpoint options are ignored.

## Message headers

Whatever the data format reads or sets.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/readings.camel.yaml
```

## Common questions

### What does unmarshal json give me?

A map (or list) value. Read fields with `${body[name]}` in Simple, or with JSONPath.

### Why do I get "Operation must be marshal or unmarshal"?

The last part of the URI must be `marshal` or `unmarshal`, for example `dataformat:base64:marshal`.

## Related

- [language](language.md)
- [Apache Camel Data Format component](https://camel.apache.org/components/next/dataformat-component.html)
