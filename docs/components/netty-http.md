# Netty HTTP component

> The `netty-http` component serves HTTP requests on an address given in the URI, for routes written for Apache Camel's camel-netty-http.

**Scheme:** `netty-http:` · **Crate:** `cameleer-http` ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Netty HTTP](https://camel.apache.org/components/next/netty-http-component.html)

`netty-http:` endpoints run on Cameleer's embedded HTTP/1.1 server, the same one as
[platform-http](platform-http.md). There is no Netty: the scheme exists so Camel routes run
unchanged. For raw TCP and UDP use the `netty` component; for outgoing HTTP use [http](http.md).

## When to use it

- Run an existing Camel route that uses `netty-http:` without changes.
- A liveness probe for a container orchestrator on its own port.

## URI format

```text
netty-http:http://host:port/path[?options]
netty-http:https://host:port/path[?options]
```

## Example

A liveness probe that answers `pong` and echoes a `probe` query parameter, so a load balancer
or Kubernetes can check the integration.

<!-- example: netty-http-liveness -->
```yaml
- route:
    id: liveness
    from:
      uri: "netty-http:http://0.0.0.0:{{env:PROBE_PORT:8083}}/ping"
      steps:
        - setBody:
            simple: "pong ${header.probe}"
```

## Rust DSL

```rust
fn liveness_route(b: &mut RouteBuilder, port: u16) {
    b.from(&format!("netty-http:http://0.0.0.0:{}/ping", port))
        .route_id("liveness")
        .set_body(simple("pong ${header.probe}"));
}
```

## Options

The options of [platform-http](platform-http.md#options) apply: `httpMethodRestrict`,
`matchOnUriPrefix`, `consumes`, `produces`, `muteException`, `chunked`, `mapHttpMessageBody`,
`mapHttpMessageHeaders`, `mapHttpMessageFormUrlEncodedBody`, `disableStreamCache`,
`sslContextParameters`, `useGlobalSslContextParameters`, `needClientAuth` / `clientAuth`, and
`host` / `port` to override the URI. Netty tuning options (worker groups, codecs) are not
supported.

## Message headers

The same as [platform-http](platform-http.md#message-headers). Query parameters such as
`?probe=lb` become message headers.

## Run it with Docker

```sh
docker run --rm -p 8083:8083 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/liveness.camel.yaml
curl 'localhost:8083/ping?probe=lb'
```

## Common questions

### Does netty-http need Netty?
No. It runs on the built-in server.

### What happens to query parameters?
They become message headers (`${header.probe}`), and the whole query string is in
`CamelHttpQuery`.

### Can I use netty-http as a client?
No. Use [http](http.md).

## Related

- [platform-http](platform-http.md), [jetty](jetty.md), [undertow](undertow.md).
- Apache Camel: [Netty HTTP component](https://camel.apache.org/components/next/netty-http-component.html).
