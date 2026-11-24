# Platform HTTP component

> The `platform-http` component serves HTTP requests on Cameleer's embedded HTTP server (HTTP/1.1 and HTTP/2): each route binds a path and its reply becomes the HTTP response.

**Scheme:** `platform-http:` · **Crate:** `cameleer-http` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Platform HTTP](https://camel.apache.org/components/next/platform-http-component.html)

All `platform-http` routes share one server and one port (`camel.server.port`, default 8080).
[jetty](jetty.md), [undertow](undertow.md) and [netty-http](netty-http.md) are the same server
with the host and port in the URI. The [REST DSL](rest.md) generates `platform-http` routes.

## When to use it

- Expose an HTTP API or a webhook receiver from a route.
- Give an AI agent a tool endpoint it can call over HTTP.
- Receive telemetry from devices on an edge gateway and hand it to a queue.
- Serve health or status endpoints next to other routes.

## URI format

```text
platform-http:/path[?options]
platform-http:/path/{param}[?options]
```

`{param}` path segments become message headers. The host comes from `camel.server.host`
(default `0.0.0.0`), the port from the `port` option, `camel.component.platform-http.port`,
`camel.server.port` or 8080. Port `0` picks a free port.

## Example

An edge gateway endpoint that accepts sensor readings for a device, answers `202 Accepted` and
forwards the reading to an in-memory queue for processing.

<!-- example: platform-http-device-readings -->
```yaml
- route:
    id: device-readings
    from:
      uri: platform-http:/devices/{id}/readings?httpMethodRestrict=POST
      steps:
        - setHeader:
            name: deviceId
            simple: "${header.id}"
        - wireTap: seda:readings
        - setHeader:
            name: CamelHttpResponseCode
            constant: 202
        - setHeader:
            name: Content-Type
            constant: application/json
        - setBody:
            simple: '{"device": "${header.id}", "accepted": true}'
- route:
    id: store-readings
    from:
      uri: seda:readings
      steps:
        - log: "device ${header.deviceId}: ${body}"
```

A `GET` on the same path answers `405 Method Not Allowed`; an unknown path answers `404`.

## Rust DSL

```rust
fn device_readings_routes(b: &mut RouteBuilder) {
    b.from("platform-http:/devices/{id}/readings?httpMethodRestrict=POST")
        .route_id("device-readings")
        .set_header("deviceId", simple("${header.id}"))
        .wire_tap("seda:readings")
        .set_header("CamelHttpResponseCode", constant(202))
        .set_header("Content-Type", constant("application/json"))
        .set_body(simple(r#"{"device": "${header.id}", "accepted": true}"#));
    b.from("seda:readings").route_id("store-readings").log("device ${header.deviceId}: ${body}");
}
```

## Options

| option | default | description |
|---|---|---|
| `httpMethodRestrict` | any | Comma-separated methods, e.g. `GET,POST`. Other methods get `405`. |
| `matchOnUriPrefix` | `false` | Also match every path below this one. |
| `consumes` | | Accepted request media type. |
| `produces` | | Response media type. |
| `port` | `camel.server.port` or 8080 | Port of this endpoint (`0` = free port). |
| `host` | `camel.server.host` or `0.0.0.0` | Interface to bind. |
| `muteException` | `false` | On failure, answer 500 without the exception text. |
| `chunked` | `false` | Send responses with chunked transfer coding. |
| `mapHttpMessageBody` | `true` | Put the request body in the message body. |
| `mapHttpMessageHeaders` | `true` | Map request headers to message headers. |
| `mapHttpMessageFormUrlEncodedBody` | `true` | Map form fields of `application/x-www-form-urlencoded` bodies to headers. |
| `disableStreamCache` | `false` | Pass the request body as a stream handle read lazily from the connection. |
| `maxInflightRequests` | `camel.server.maxInflightRequests` or 10000 | Cameleer extension. The most requests a route that waits without holding a thread (a route that calls `http`) works on at once. Beyond it the endpoint answers `503` with `Retry-After: 1` straight away. `0` means no limit. |
| `ssl` | `false` | Serve HTTPS (certificate from `camel.server.ssl.*` or `sslContextParameters`). |
| `sslContextParameters` | | `#bean` with the server key store; also enables HTTPS. |
| `useGlobalSslContextParameters` | `false` | Use the global `camel.ssl.*` parameters (with `camel.ssl.enabled=true`). |
| `needClientAuth` | `false` | Require a client certificate (mutual TLS). `clientAuth=NONE\|WANT\|REQUIRE` is the long form. |

### Asynchronous routes

A route that calls the `http` or the `kafka` producer answers its requests without holding a server thread
while it waits for the reply (for Kafka, the acknowledgement of the broker). This is on by default.

- **Where it runs:** the route continues on the event loop that read the reply.
  - Steps that only compute run there too: `setHeader`, `setBody` or `transform` with constant, header,
    JsonPath, jq or Simple expressions (Simple without `bean:`, `ref:` or `properties:`).
  - Before any other step (a processor, a bean, a blocking component), the route moves to a worker thread.
- **Always on the server's threads, as before:** endpoints with `consumes`, `produces` or
  `disableStreamCache`, and routes with `interceptFrom`.
- **The limit:** the server's thread count no longer bounds how many such requests run at once, so
  `maxInflightRequests` does. Beyond it the endpoint answers `503` with `Retry-After: 1`.
- **Switches:**
  - `CAMEL_ASYNC_ROUTING=false` turns asynchronous routes off for the whole process.
  - `CAMEL_HTTP_CLIENT_LOOPS` sets the number of event loops of the `http` producer's client (default half
    the CPUs, at most 64).

`camel.server.sslEnabled=true` turns on HTTPS for every `platform-http` endpoint. Apache Camel
options such as `transferException`, `useStreaming`, `headerFilterStrategy` and
`sessionSupport` are accepted and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelHttpMethod` | out | Request method. |
| `CamelHttpUri` | out | Request target as received. |
| `CamelHttpPath` | out | Request path. |
| `CamelHttpQuery` | out | Decoded query string. |
| `CamelHttpRawQuery` | out | Raw query string. |
| `CamelHttpRemoteAddress` | out | Client address. |
| `Content-Type` | in / out | Request media type; set it to choose the response media type. |
| `CamelHttpResponseCode` | in | Response status (default 200). |
| `CamelHttpResponseText` | in | Response reason phrase. |
| `CamelHttpChunked` | in | `true` sends this response chunked. |
| `CamelHttpSslClientCert` | out | Verified client certificate (PEM) with mutual TLS; also `...SubjectName`, `...IssuerName`, `...SerialNo`, `...NotBefore`, `...NotAfter`. |

Path parameters, query parameters and request headers become message headers. A failed exchange
answers 500 with the exception text; a handled exception answers 200 with the handled body (set
`CamelHttpResponseCode` in the handler to change it).

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" cameleering/cameleer run /routes/device-readings.camel.yaml
curl -XPOST -d '{"temp": 21.5}' localhost:8080/devices/sensor-7/readings
```

## Common questions

### How do I change the port?
Set `camel.server.port` (for example `--prop camel.server.port=9000` on `run`, or
`CAMEL_SERVER_PORT=9000` in the environment), or add `port=9000` to one endpoint.

### How do I return a different status code?
Set the `CamelHttpResponseCode` header before the route ends, e.g. 201 or 404.

### How do I serve HTTPS?
Set `camel.server.ssl.keyStore` (or `certFile` and `keyFile`) and `camel.server.sslEnabled=true`,
or add `ssl=true` / `sslContextParameters=#tls` to an endpoint.

### How many connections and requests can it serve?

The server runs on [simoom](https://crates.io/crates/simoom), one event loop per CPU, so an
idle keep-alive connection costs a few hundred bytes and no thread: tens of thousands of open
connections are fine. A route runs on the loop thread while it is fast, and on a pool of at most 200
threads while it blocks (a call to another service, a database); Cameleer learns which per route, by
timing the route's calls. A route that blocks only briefly can look fast while the server is idle; when its
calls keep more than half of the loop threads' time busy over 100 ms, it goes back to the pool for a minute.
A keep-alive connection stays open for any number of requests (as in Jetty and Vert.x); the server
closes it after 15 s idle.
Header names in responses are lower case, as HTTP allows.

### Can I serve every path below a prefix?
Yes: `platform-http:/files?matchOnUriPrefix=true`; `CamelHttpPath` holds the full path.

## Related

- [rest](rest.md): the REST DSL with OpenAPI generation.
- [jetty](jetty.md), [undertow](undertow.md), [netty-http](netty-http.md): the same server with the address in the URI.
- [http](http.md): call HTTP services.
- [webhook](webhook.md): register webhooks with SaaS services.
- Apache Camel: [Platform HTTP component](https://camel.apache.org/components/next/platform-http-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
