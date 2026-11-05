# REST component

> The `rest` component defines REST endpoints (the REST DSL, with a generated OpenAPI 3 document) and calls REST APIs with path templates filled from headers.

**Scheme:** `rest:` · **Crate:** `cameleer-http` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [REST](https://camel.apache.org/components/next/rest-component.html)

Most people use REST through the REST DSL: `rest` and `restConfiguration` blocks in YAML (or
`b.rest(...)` in Rust). Each verb becomes a [platform-http](platform-http.md) route (or a
[jetty](jetty.md) / [undertow](undertow.md) / [netty-http](netty-http.md) route, per
`restConfiguration.component`). The `rest:` URI scheme is the lower-level form of the same thing.

## When to use it

- Publish a JSON API with an OpenAPI document.
- Give an AI agent a set of tools: the agent reads the OpenAPI document to learn the
  operations and calls them over HTTP.
- Call a REST API whose path has parameters (`/orders/{id}`), filled from message headers.

## URI format

```text
rest:method:path[:uriTemplate][?options]
```

For example `rest:get:/orders/{id}` (consumer) or `rest:get:/orders/{id}?host=api.example.com`
(producer).

## Example: tools for an AI agent

Two inventory tools under `/tools`, with an OpenAPI 3 document at `/tools/openapi.json` that an
agent framework can load to discover them. `bindingMode: json` turns the route's result into
JSON.

<!-- example: rest-agent-tools -->
```yaml
- restConfiguration:
    contextPath: /tools
    apiContextPath: /openapi.json
    bindingMode: json
- rest:
    path: /inventory
    get:
      - path: /{sku}
        id: getStock
        description: Current stock level of a product
        to: direct:getStock
    post:
      - path: /{sku}/reserve
        id: reserveStock
        description: Reserve units of a product for an order
        to: direct:reserveStock
- route:
    from:
      uri: direct:getStock
      steps:
        - setBody:
            simple: '{"sku": "${header.sku}", "inStock": 12}'
        - unmarshal:
            json: {}
- route:
    from:
      uri: direct:reserveStock
      steps:
        - setBody:
            simple: '{"sku": "${header.sku}", "reserved": ${body[qty]}}'
        - unmarshal:
            json: {}
```

With `bindingMode: json` the JSON request body arrives already parsed, so `${body[qty]}` reads
a field.

## Example: call a REST API

The `rest:` producer fills `{id}` from the `id` header and sends the request to `host`.

<!-- example: rest-producer-get-order -->
```yaml
- route:
    id: fetch-order
    from:
      uri: timer:fetch-order?repeatCount=1
      steps:
        - setHeader:
            name: id
            constant: "42"
        - to: "rest:get:/orders/{id}?host={{env:ORDERS_API:http://localhost:8080}}"
        - log: "order 42: ${body}"
```

## Rust DSL

```rust
fn agent_tools_routes(b: &mut RouteBuilder) {
    b.rest_configuration().context_path("/tools").api_context_path("/openapi.json").binding_mode(RestBindingMode::Json);
    b.rest("/inventory")
        .get("/{sku}").route_id("getStock").description("Current stock level of a product").to("direct:getStock")
        .post("/{sku}/reserve").route_id("reserveStock").description("Reserve units of a product for an order").to("direct:reserveStock");
    b.from("direct:getStock").set_body(simple(r#"{"sku": "${header.sku}", "inStock": 12}"#)).unmarshal_json();
    b.from("direct:reserveStock").set_body(simple(r#"{"sku": "${header.sku}", "reserved": ${body[qty]}}"#)).unmarshal_json();
}
```

## Options

`rest:` endpoint options:

| option | default | description |
|---|---|---|
| `host` | | Producer: target `host:port`, or `https://host` for TLS. Required for producers. |
| `producerComponentName` | `http` | Component that sends producer requests. |
| `componentName` | `camel.rest.component` or `platform-http` | Consumer: HTTP server component. |
| `port` | | Consumer port. |
| `consumes`, `produces` | | Media types. |

`inType`, `outType`, `routeId`, `description`, `apiDoc` and `bindingMode` are accepted on the
endpoint and ignored (use the REST DSL for binding).

`restConfiguration` keys in YAML: `component`, `host`, `port`, `scheme` (`https` for TLS),
`contextPath`, `apiContextPath` (default `/api-doc`), `bindingMode` (`off`, `auto`, `json`),
`enableCORS` and `corsHeaders`, `skipBindingOnErrorCode`, `endpointProperty`,
`consumerProperty`, `apiProperty`. Verbs take `path`, `id` (route id and OpenAPI
`operationId`), `description`, `consumes`, `produces`, `param`, `responseMessage`, `deprecated`,
`apiDocs`, `outType`, `tag`, and `to` / `toD` or inline `steps`.

Not supported: XML binding (`bindingMode: xml`) and `clientRequestValidation`.

## Message headers

| header | direction | description |
|---|---|---|
| path parameters (`sku`, `id`, ...) | out / in | Consumer: set from the request path. Producer: fill the `{name}` placeholders. |
| query parameters | out | Consumer: each becomes a header. |
| `CamelHttpResponseCode` | in | Consumer: response status. |
| `CamelHttpMethod`, `CamelHttpPath`, `CamelHttpQuery` | out | As for [platform-http](platform-http.md#message-headers). |

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/agent-tools.camel.yaml
curl localhost:8080/tools/inventory/A-100
curl -XPOST -H 'Content-Type: application/json' -d '{"qty": 3}' localhost:8080/tools/inventory/A-100/reserve
curl localhost:8080/tools/openapi.json
```

## Common questions

### Where is the OpenAPI document?
At `<contextPath><apiContextPath>`, by default `/api-doc` (here `/tools/openapi.json`). It is an
OpenAPI 3.0.3 JSON document with one operation per verb.

### How do I return 201 or 400?
Set `CamelHttpResponseCode` in the route that handles the verb; with an `onException` handler
you can map errors to 400.

### How do I call an HTTPS API with `rest:`?
Use `host=https://api.example.com`; the request is sent with the `https` component.

### Which server does the REST DSL use?
`platform-http` unless `restConfiguration.component` names `jetty`, `undertow` or `netty-http`.

## Related

- [platform-http](platform-http.md), [http](http.md).
- [cxfrs](cxfrs.md): JAX-RS style REST endpoints.
- Apache Camel: [REST component](https://camel.apache.org/components/next/rest-component.html),
  [REST DSL](https://camel.apache.org/manual/rest-dsl.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
