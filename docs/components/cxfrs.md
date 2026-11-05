# CXF-RS component

> The `cxfrs` component serves and calls JAX-RS style REST resources: operations are matched by HTTP method and path template, and the matching operation name is passed to the route.

**Scheme:** `cxfrs:` · **Crate:** `cameleer-soap` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [CXF-RS](https://camel.apache.org/components/next/cxfrs-component.html)

Java resource classes do not exist in Cameleer. The resource model is given inline with
`operations=name:METHOD:/path;...`, or with `ResourceDescriptor` beans (`resourceClasses=#bean`)
in Rust. For new APIs the [REST DSL](rest.md) is usually simpler; `cxfrs` is for routes migrated
from camel-cxf-rest that branch on `operationName`.

## When to use it

- Migrate Camel routes that use `cxfrs:` and switch on `operationName`.
- One route serving several REST operations of a resource.
- Call a REST service with method, path and query from headers.

## URI format

```text
cxfrs:http://host:port/base[?options]
cxfrs:/base[?options]          (consumer on the platform HTTP port)
```

## Example

A customer resource with two operations in one route. The route branches on `operationName`;
path parameters (`id`) are headers.

<!-- example: cxfrs-customer-resource -->
```yaml
- route:
    id: customer-resource
    from:
      uri: "cxfrs:http://0.0.0.0:{{env:API_PORT:9000}}/api?operations=getCustomer:GET:/customers/{id};addCustomer:POST:/customers"
      steps:
        - choice:
            when:
              - simple: "${header.operationName} == 'getCustomer'"
                steps:
                  - setBody:
                      simple: '{"id": "${header.id}", "name": "Ada"}'
                  - unmarshal:
                      json: {}
              - simple: "${header.operationName} == 'addCustomer'"
                steps:
                  - setHeader:
                      name: CamelHttpResponseCode
                      constant: 201
                  - setBody:
                      simple: '{"created": "${body[name]}"}'
                  - unmarshal:
                      json: {}
```

JSON request bodies arrive parsed, so `${body[name]}` reads a field. A map or array result is
sent as JSON; a request that matches no operation gets `404`.

## Rust DSL

```rust
fn customer_resource_route(b: &mut RouteBuilder, port: u16) {
    b.from(&format!("cxfrs:http://0.0.0.0:{}/api?operations=getCustomer:GET:/customers/{{id}};addCustomer:POST:/customers", port))
        .route_id("customer-resource")
        .choice()
        .when(simple("${header.operationName} == 'getCustomer'"))
        .set_body(simple(r#"{"id": "${header.id}", "name": "Ada"}"#))
        .unmarshal_json()
        .when(simple("${header.operationName} == 'addCustomer'"))
        .set_header("CamelHttpResponseCode", constant(201))
        .set_body(simple(r#"{"created": "${body[name]}"}"#))
        .unmarshal_json()
        .end_choice();
}
```

## Options

| option | default | description |
|---|---|---|
| `operations` | | Inline resource model: `name:METHOD:/path/{param};...`. |
| `resourceClasses` | | `#bean` list of `ResourceDescriptor` beans (Java class names are ignored). |
| `providers` | | `#bean` list of `EntityProvider` beans (readers/writers for media types other than JSON). |
| `bindingStyle` | `SimpleConsumer` | `Default` stops mapping query parameters to headers. |
| `produces` | | Default response media type for string bodies. |
| `address` | URI address | Base address. |
| `throwExceptionOnFailure` | `true` | Producer: fail on status 300 and above. |
| `parseJson` | `false` | Producer: parse JSON responses into values. |
| `username`, `password` | | Producer: basic authentication. |
| `receiveTimeout`, `connectionTimeout` | 60000, 30000 | Producer timeouts in ms. |
| `sslContextParameters` | | `#bean` for `https`. |

## Message headers

| header | direction | description |
|---|---|---|
| `operationName` | out | Consumer: the matched operation (else the HTTP method). |
| `CamelHttpMethod` | out / in | Request method; producer: method to send (default GET, POST with a body). |
| `CamelHttpPath` | out / in | Path relative to the base; producer: appended to the base. |
| `CamelHttpUri`, `CamelHttpQuery` | out / in | Request URI and query. |
| `CamelHttpResponseCode` | in / out | Response status. |
| `Content-Type`, `Accept` | in / out | Media types; a `415` or `406` answers mismatches with declared `consumes` / `produces`. |

Path template parameters and query parameters become headers.

## Run it with Docker

```sh
docker run --rm -p 9000:9000 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/customer-resource.camel.yaml
curl localhost:9000/api/customers/7
curl -XPOST -H 'Content-Type: application/json' -d '{"name": "Grace"}' localhost:9000/api/customers
```

## Common questions

### Do I need JAX-RS annotated classes?
No. Describe the operations with `operations=` or `ResourceDescriptor` beans.

### What does a string result look like?
It is sent as `text/plain` (or the `produces` type); maps and arrays are sent as JSON; a null
body answers `204`.

### What is not supported?
The JAX-RS proxy client API and sub-resource locators.

## Related

- [rest](rest.md), [platform-http](platform-http.md), [cxf](cxf.md).
- Apache Camel: [CXF-RS component](https://camel.apache.org/components/next/cxfrs-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
