# Undertow component

> The `undertow` component serves HTTP requests on an address given in the URI, for routes written for Apache Camel's camel-undertow.

**Scheme:** `undertow:` · **Crate:** `cameleer-http` ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Undertow](https://camel.apache.org/components/next/undertow-component.html)

`undertow:` endpoints run on Cameleer's embedded HTTP/1.1 server, the same one as
[platform-http](platform-http.md) and [jetty](jetty.md). Host and port are part of the URI.
Cameleer's `undertow` is a consumer only: to call HTTP services use [http](http.md). Undertow's
WebSocket support is not part of this scheme; use [websocket](websocket.md).

## When to use it

- Run an existing Camel route that uses `undertow:` without changes.
- Echo or transform request bodies on a dedicated port, e.g. a small adapter in front of a
  legacy device.

## URI format

```text
undertow:http://host:port/path[?options]
undertow:https://host:port/path[?options]
```

## Example

An adapter that takes a plain-text command from a client, normalizes it to upper case and
returns it with the request method.

<!-- example: undertow-command-adapter -->
```yaml
- route:
    id: command-adapter
    from:
      uri: "undertow:http://0.0.0.0:{{env:ADAPTER_PORT:8082}}/command?httpMethodRestrict=POST,PUT"
      steps:
        - setBody:
            simple: "${header.CamelHttpMethod} ${body.toUpperCase()}"
        - setHeader:
            name: Content-Type
            constant: text/plain
```

## Rust DSL

```rust
fn command_adapter_route(b: &mut RouteBuilder, port: u16) {
    b.from(&format!("undertow:http://0.0.0.0:{}/command?httpMethodRestrict=POST,PUT", port))
        .route_id("command-adapter")
        .set_body(simple("${header.CamelHttpMethod} ${body.toUpperCase()}"))
        .set_header("Content-Type", constant("text/plain"));
}
```

## Options

The options of [platform-http](platform-http.md#options) apply: `httpMethodRestrict`,
`matchOnUriPrefix`, `consumes`, `produces`, `muteException`, `chunked`, `mapHttpMessageBody`,
`mapHttpMessageHeaders`, `mapHttpMessageFormUrlEncodedBody`, `disableStreamCache`,
`sslContextParameters`, `useGlobalSslContextParameters`, `needClientAuth` / `clientAuth`, and
`host` / `port` to override the URI.

## Message headers

The same as [platform-http](platform-http.md#message-headers).

## Run it with Docker

```sh
docker run --rm -p 8082:8082 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/command-adapter.camel.yaml
curl -XPOST -d 'restart pump 3' localhost:8082/command
```

## Common questions

### Is this the real Undertow server?
No. The scheme maps to Cameleer's built-in server so Apache Camel routes run unchanged.

### Can I use undertow as an HTTP client?
Not in Cameleer. Use the [http](http.md) component for outgoing calls.

### How do I accept only some methods?
`httpMethodRestrict=POST,PUT`; other methods get `405`.

## Related

- [platform-http](platform-http.md), [jetty](jetty.md), [netty-http](netty-http.md).
- [http](http.md) for outgoing requests.
- Apache Camel: [Undertow component](https://camel.apache.org/components/next/undertow-component.html).
