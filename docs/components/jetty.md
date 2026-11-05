# Jetty component

> The `jetty` component serves HTTP requests on an address given in the URI, for routes written for Apache Camel's camel-jetty.

**Scheme:** `jetty:` · **Crate:** `cameleer-http` ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Jetty](https://camel.apache.org/components/next/jetty-component.html)

Cameleer has no Jetty: `jetty:` endpoints run on the same embedded HTTP/1.1 server as
[platform-http](platform-http.md), so routes that use `jetty:http://...` keep working
unchanged. The difference from `platform-http` is that the host and port are part of the URI.
[undertow](undertow.md) and [netty-http](netty-http.md) behave the same way.

## When to use it

- Run an existing Camel route that uses `jetty:` without changes.
- Listen on a specific interface and port per route, e.g. an admin endpoint on `127.0.0.1` only.
- Serve HTTPS on a given port with `jetty:https://`.

## URI format

```text
jetty:http://host:port/path[?options]
jetty:https://host:port/path[?options]
```

## Example

A status endpoint for an edge device, served on its own port and answering every path below
`/status` (`matchOnUriPrefix=true`).

<!-- example: jetty-status-endpoint -->
```yaml
- route:
    id: device-status
    from:
      uri: "jetty:http://0.0.0.0:{{env:STATUS_PORT:8081}}/status?matchOnUriPrefix=true&httpMethodRestrict=GET"
      steps:
        - setHeader:
            name: Content-Type
            constant: application/json
        - setBody:
            simple: '{"device": "{{env:DEVICE_ID:gateway-1}}", "path": "${header.CamelHttpPath}", "status": "up"}'
```

## Rust DSL

```rust
fn device_status_route(b: &mut RouteBuilder, port: u16) {
    b.from(&format!("jetty:http://0.0.0.0:{}/status?matchOnUriPrefix=true&httpMethodRestrict=GET", port))
        .route_id("device-status")
        .set_header("Content-Type", constant("application/json"))
        .set_body(simple(r#"{"device": "gateway-1", "path": "${header.CamelHttpPath}", "status": "up"}"#));
}
```

## Options

The options are those of [platform-http](platform-http.md#options): `httpMethodRestrict`,
`matchOnUriPrefix`, `consumes`, `produces`, `muteException`, `chunked`, `mapHttpMessageBody`,
`mapHttpMessageHeaders`, `mapHttpMessageFormUrlEncodedBody`, `disableStreamCache`,
`sslContextParameters`, `useGlobalSslContextParameters`, `needClientAuth` / `clientAuth`. The
host and port come from the URI (`port` and `host` options override them). Jetty-specific
options such as `handlers`, `sessionSupport` and `httpBinding` are accepted and ignored.

## Message headers

The same as [platform-http](platform-http.md#message-headers): `CamelHttpMethod`,
`CamelHttpUri`, `CamelHttpPath`, `CamelHttpQuery`, `CamelHttpRawQuery`,
`CamelHttpRemoteAddress` on the way in, `CamelHttpResponseCode` and `Content-Type` on the way
out.

## Run it with Docker

```sh
docker run --rm -p 8081:8081 -e DEVICE_ID=gateway-7 -v "$PWD:/routes:ro" \
  pmsudhi/cameleer run /routes/device-status.camel.yaml
curl localhost:8081/status/disk
```

## Common questions

### Do I need to install Jetty?
No. The scheme is kept for compatibility with Apache Camel routes; the server is built into
Cameleer.

### Can two jetty routes share a port?
Yes. Routes on the same host and port share one listener and are told apart by path.

### How do I serve HTTPS?
Use `jetty:https://0.0.0.0:8443/path` with `sslContextParameters=#tls`, or set the
`camel.server.ssl.*` properties.

## Related

- [platform-http](platform-http.md), [undertow](undertow.md), [netty-http](netty-http.md).
- [rest](rest.md): `restConfiguration` with `component: jetty`.
- Apache Camel: [Jetty component](https://camel.apache.org/components/next/jetty-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
