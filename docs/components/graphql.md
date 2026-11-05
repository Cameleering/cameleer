# GraphQL component

> The `graphql` component sends GraphQL queries and mutations to a GraphQL server over HTTP or HTTPS and returns the JSON response.

**Scheme:** `graphql:` · **Crate:** `cameleer-saas` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [GraphQL](https://camel.apache.org/components/next/graphql-component.html)

## When to use it

- Query a GraphQL API (GitHub, Shopify, Hasura, your own) from a route.
- Run a mutation for each incoming message, with the message supplying the variables.
- Let an AI agent fetch structured data through one fixed, reviewed query.

## URI format

```text
graphql://http://host[:port]/path[?options]
graphql://host/path[?options]      (https is assumed without a scheme)
```

## Example

Look up a book by id: the `id` header becomes the query variable, and the response JSON is
reduced to the book's name.

<!-- example: graphql-book-lookup -->
```yaml
- route:
    id: book-lookup
    from:
      uri: direct:book
      steps:
        - setHeader:
            name: gqlVariables
            simple: '{"id": "${header.bookId}"}'
        - to: "graphql://{{env:GRAPHQL_URL:http://localhost:4000/graphql}}?query=query Book($id: ID) { book(id: $id) { id name author } }&variablesHeader=gqlVariables&accessToken={{env:GRAPHQL_TOKEN:changeme}}"
        - unmarshal:
            json: {}
        - setBody:
            simple: "${body[data][book][name]}"
```

The variables are taken from the header named by `variablesHeader` (JSON text or an object),
else from the `variables` option, else from the body when the body is an object (for example
after `unmarshal: json`).

## Rust DSL

```rust
fn book_lookup_route(b: &mut RouteBuilder, url: &str, token: &str) {
    b.from("direct:book")
        .route_id("book-lookup")
        .set_header("gqlVariables", simple(r#"{"id": "${header.bookId}"}"#))
        .to(&format!("graphql://{}?query=query Book($id: ID) {{ book(id: $id) {{ id name author }} }}&variablesHeader=gqlVariables&accessToken={}", url, token))
        .unmarshal_json()
        .set_body(simple("${body[data][book][name]}"));
}
```

## Options

| option | default | description |
|---|---|---|
| `query` | | The query or mutation text. |
| `queryFile` | | File holding the query (`classpath:` / file path). |
| `queryHeader` | | Header holding the query. |
| `operationName` | | Operation to run when the document has several. |
| `variables` | | Variables as a JSON object, or `#bean` holding a map. |
| `variablesHeader` | | Header holding the variables. |
| `accessToken` | | Token sent as `Authorization: <jwtAuthorizationType> <token>`. |
| `jwtAuthorizationType` | `Bearer` | Authorization scheme for `accessToken`. |
| `username`, `password` | | Basic authentication. |
| `throwExceptionOnFailure` | `true` | Fail on HTTP errors. |

Without `query`, `queryFile` or `queryHeader` the body is the query. With one of them, an object
body supplies the variables when neither `variablesHeader` nor `variables` gives them.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGraphqlResponseCode` | out | HTTP status of the response. |

The result body is the response JSON as text (`{"data": ..., "errors": ...}`), as in Apache
Camel; unmarshal it with `json` to read fields.

## Run it with Docker

```sh
docker run --rm -e GRAPHQL_URL=https://api.example.com/graphql -e GRAPHQL_TOKEN \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/book-lookup.camel.yaml
```

The example starts from `direct:book`; in practice, put a REST endpoint, a Kafka consumer or a
timer in front of it.

## Common questions

### How do I pass variables?
Put JSON text or an object in a header named by `variablesHeader`, fix them with
`variables={"id":"1"}`, or send an object body (not JSON text) together with `query`.

### Why is the response a string?
Apache Camel returns the response text too. Add `unmarshal: json` to work with fields.

### Do GraphQL errors fail the exchange?
Only HTTP errors do. A `200` response with an `errors` array is returned as is; check
`${body[errors]}` after unmarshalling.

## Related

- [http](http.md), [rest](rest.md).
- Apache Camel: [GraphQL component](https://camel.apache.org/components/next/graphql-component.html).
