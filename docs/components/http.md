# HTTP component

> The `http` component calls HTTP and HTTPS endpoints from a route: REST APIs, webhooks and LLM APIs, over HTTP/1.1 or HTTP/2.

**Scheme:** `http:` · **Also:** `https:` · **Crate:** `cameleer-http` · **Cargo feature:** none, always included; HTTP/2 needs [`http2`](../guides/build-features.md) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [HTTP](https://camel.apache.org/components/next/http-component.html)

The `http` component is a client only. To serve HTTP requests, use
[platform-http](platform-http.md) or the [REST DSL](rest.md).

## When to use it

- Call a REST API from a route and work with the JSON response.
- Call an LLM API (OpenAI, or any OpenAI-compatible server such as Ollama or vLLM) from an AI
  agent workflow.
- Post events to a webhook (Slack, Teams, a CI system).
- Proxy or bridge requests from a `platform-http` route to a backend (`bridgeEndpoint=true`).

## URI format

```text
http://host[:port]/path[?options]
https://host[:port]/path[?options]
```

The scheme picks plain HTTP or TLS. Query parameters that are not component options are sent
to the server as query parameters, as in Apache Camel. This also means that a misspelled option
is not rejected: it is sent to the server.

## Example

An endpoint that answers questions with an LLM. `POST /ask` with a question as the body; the
route calls the chat completions API and returns the answer as plain text. The same route works
with any OpenAI-compatible server: point `LLM_BASE_URL` at it.

<!-- example: http-ask-llm -->
```yaml
- route:
    id: ask-llm
    from:
      uri: platform-http:/ask?httpMethodRestrict=POST
      steps:
        - removeHeaders:
            pattern: "*"
        - setHeader:
            name: Authorization
            simple: "Bearer {{env:OPENAI_API_KEY:sk-test-key}}"
        - setHeader:
            name: Content-Type
            constant: application/json
        - setBody:
            simple: '{"model": "{{env:LLM_MODEL:gpt-4o-mini}}", "messages": [{"role": "user", "content": "${body}"}]}'
        - to: "{{env:LLM_BASE_URL:https://api.openai.com/v1}}/chat/completions?httpMethod=POST"
        - unmarshal:
            json: {}
        - setBody:
            simple: "${body[choices][0][message][content]}"
        - removeHeaders:
            pattern: "*"
        - setHeader:
            name: Content-Type
            constant: text/plain
```

The question is placed into the JSON as is; a question containing double quotes needs escaping
first. `removeHeaders` keeps the incoming request headers (and the API's response headers) from
being forwarded.

## Rust DSL

```rust
fn ask_llm_route(b: &mut RouteBuilder, base_url: &str, api_key: &str) {
    b.from("platform-http:/ask?httpMethodRestrict=POST")
        .route_id("ask-llm")
        .remove_headers("*")
        .set_header("Authorization", constant(format!("Bearer {}", api_key)))
        .set_header("Content-Type", constant("application/json"))
        .set_body(simple(r#"{"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "${body}"}]}"#))
        .to(&format!("{}/chat/completions?httpMethod=POST", base_url))
        .unmarshal_json()
        .set_body(simple("${body[choices][0][message][content]}"))
        .remove_headers("*")
        .set_header("Content-Type", constant("text/plain"));
}
```

## Options

| option | default | description |
|---|---|---|
| `httpMethod` | GET, or POST when there is a body | HTTP method. The `CamelHttpMethod` header wins over it. |
| `httpVersion` | `HTTP/1.1` | `HTTP/2` uses HTTP/2 (TLS with ALPN `h2`, or h2c prior knowledge for `http:`), one multiplexed connection per target. Alias `protocolVersion`. |
| `throwExceptionOnFailure` | `true` | Fail with `HttpOperationFailedException` when the status is outside `okStatusCodeRange`. |
| `okStatusCodeRange` | `200-299` | Status codes treated as success, e.g. `200-299,404`. |
| `bridgeEndpoint` | `false` | Ignore the `CamelHttpUri` header and use the endpoint URI (for proxies). |
| `connectTimeout` | `30s` | TCP connect timeout. |
| `socketTimeout` | `60s` | Read timeout. Aliases `soTimeout`, `responseTimeout`. |
| `followRedirects` | `false` | Follow 3xx redirects. |
| `copyHeaders` | `true` | Copy the response headers to the message. |
| `skipRequestHeaders` | `false` | Do not send the message headers as request headers. |
| `skipResponseHeaders` | `false` | Do not map response headers to message headers. |
| `userAgent` | | `User-Agent` header value. |
| `authUsername`, `authPassword` | | Basic authentication. `authMethod` accepts only `Basic`. |
| `chunked` | `false` | Send the request body with chunked transfer coding. |
| `getWithBody`, `deleteWithBody` | `false` | Send a body with GET / DELETE. |
| `ignoreResponseBody` | `false` | Do not read the response body into the message. |
| `customHostHeader` | | Value for the `Host` header. |
| `preserveHostHeader` | `false` | Forward the incoming `Host` header. |
| `contentCompressionDisabled` | `false` | Do not send `Accept-Encoding: gzip, deflate` and do not decode compressed responses. |
| `connectionClose` | `false` | Do not pool keep-alive connections. |
| `connectionsPerRoute` | `20` | Pooled connections per host and port. |
| `disableStreamCache` | `false` | Return the response body as a stream handle instead of bytes. |
| `proxyHost`, `proxyPort` | | HTTP proxy (CONNECT tunnelling for `https`). Aliases `proxyAuthHost`, `proxyAuthPort`. |
| `proxyAuthUsername`, `proxyAuthPassword` | | Proxy credentials (sent as HTTP Basic). |
| `proxyAuthScheme`, `proxyAuthMethod` | | Accepted for compatibility with Camel and ignored: the proxy is always reached with Basic credentials. |
| `useSystemProperties` | `false` | Take the proxy from `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY`. |
| `sslContextParameters` | | `#bean` with trust and key material for `https` (client certificates come from its key store). |
| `useGlobalSslContextParameters` | `false` | Use the global `camel.ssl.*` parameters (with `camel.ssl.enabled=true`). |

Certificates and host names are always verified for `https`. Several Apache Camel options
(`headerFilterStrategy`, `httpClientConfigurer`, `cookieHandler`, `maxTotalConnections` and
others) are accepted and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelHttpMethod` | in | Method for this request. |
| `CamelHttpUri` | in | Full target URI (ignored with `bridgeEndpoint=true`). |
| `CamelHttpPath` | in | Path appended to the endpoint path. |
| `CamelHttpQuery` | in | Query string for this request. |
| `Content-Type` | in / out | Request and response media type. |
| `CamelHttpResponseCode` | out | Response status code. |
| `CamelHttpResponseText` | out | Response reason phrase. |

Other message headers (except `Camel*` ones) are sent as request headers, and response headers
become message headers. A failure raises `HttpOperationFailedException` carrying the status code
and the response body.

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -e OPENAI_API_KEY -v "$PWD:/routes:ro" \
  pmsudhi/cameleer run /routes/ask-llm.camel.yaml
curl -XPOST --data 'What is Apache Camel?' localhost:8080/ask
```

For a local model, add `-e LLM_BASE_URL=http://host.docker.internal:11434/v1 -e LLM_MODEL=llama3.2`.

## Common questions

### How do I send JSON with a POST request?
Set the body to the JSON text, set `Content-Type: application/json`, and use
`httpMethod=POST` (a body alone also makes the request a POST).

### How do I handle a 404 without an exception?
Add `throwExceptionOnFailure=false` and branch on `${header.CamelHttpResponseCode}`, or widen
`okStatusCodeRange`.

### How do I call an API that needs a client certificate?
Bind an `SSLContextParameters` bean with a key store and reference it with
`sslContextParameters=#myTls`. Mutual TLS works with TLS 1.2 and 1.3.

### Why does a typo in an option not fail?
Unknown options are sent as query parameters, as in Apache Camel. Check the request the server
receives.

## Related

- [platform-http](platform-http.md): serve HTTP requests.
- [rest](rest.md): REST DSL and the `rest:` producer.
- [graphql](graphql.md), [webhook](webhook.md).
- Apache Camel: [HTTP component](https://camel.apache.org/components/next/http-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
