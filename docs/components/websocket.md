# WebSocket component

> The `websocket` component runs a WebSocket server: the consumer receives messages from connected clients and the producer sends to one client or broadcasts to all of them.

**Scheme:** `websocket:` · **Crate:** `cameleer-protocols` · **Cargo feature:** [`protocols`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [WebSocket (Jetty)](https://camel.apache.org/components/next/websocket-component.html)

Cameleer implements RFC 6455 itself (no Jetty): text and binary messages, fragmentation,
ping/pong and the closing handshake. To connect to a WebSocket server as a client, use
[ws](ws.md). For a Vert.x-style server with connection events, see
[vertx-websocket](vertx-websocket.md).

## When to use it

- Push live updates (sensor readings, order status, alerts) to browser dashboards.
- Stream an AI agent's progress or tokens to a web UI as they arrive.
- A simple chat or notification hub: every message from one client is broadcast to all.

## URI format

```text
websocket://host:port/path[?options]
```

Defaults: host `0.0.0.0`, port 9292. Consumers and producers on the same host and port share
one listener. A producer also starts the server for its path, so clients can connect to receive
broadcasts even without a consumer route.

## Example

A broadcast hub for an operations dashboard: every message a client sends is stamped with its
sender and sent to every client connected to `/ops`.

<!-- example: websocket-broadcast-hub -->
```yaml
- route:
    id: ops-hub
    from:
      uri: "websocket://0.0.0.0:{{env:WS_PORT:9292}}/ops"
      steps:
        - setBody:
            simple: "${header.CamelWebsocketRemoteAddress}: ${body}"
        - to: "websocket://0.0.0.0:{{env:WS_PORT:9292}}/ops?sendToAll=true"
```

Without `sendToAll`, the producer replies only to the connection named by the
`CamelWebsocketConnectionKey` header, which the consumer sets: `from` and `to` on the same URI
make an echo server.

## Rust DSL

```rust
fn ops_hub_route(b: &mut RouteBuilder, port: u16) {
    b.from(&format!("websocket://0.0.0.0:{}/ops", port))
        .route_id("ops-hub")
        .set_body(simple("${header.CamelWebsocketRemoteAddress}: ${body}"))
        .to(&format!("websocket://0.0.0.0:{}/ops?sendToAll=true", port));
}
```

## Options

| option | default | description |
|---|---|---|
| `sendToAll` | `false` | Producer: send to every connection of the path. |
| `maxTextMessageSize`, `maxBinaryMessageSize`, `maxMessageSize` | 64 MiB | Largest incoming message. |
| `maxFrameSize` | 65536 | Outgoing messages are fragmented into frames of this size. |
| `allowedOrigins` | any | Comma-separated `Origin` values accepted at the handshake (`*` = any). Alias `allowedOriginPattern`. |
| `subProtocol` | | Comma-separated subprotocols to accept. Aliases `subProtocols`, `clientSubProtocols`. |
| `sslContextParameters` | | `#bean` with the server key store: serves `wss`. |
| `useGlobalSslContextParameters` | `false` | Serve `wss` with the global `camel.ssl.*` parameters. |

The first endpoint that binds a host and port decides whether it serves TLS. Jetty options such
as `staticResources`, `sessionSupport`, `minThreads` and `maxThreads` are accepted and ignored.
No compression extensions (`permessage-deflate`) are negotiated.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelWebsocketConnectionKey` | out / in | Consumer: the sending connection. Producer: the connection to send to. |
| `CamelWebsocketRemoteAddress` | out | Client address. |
| `CamelWebsocketSendToAll` | in | `true` broadcasts this message. |

Text messages arrive as strings, binary messages as bytes. A byte body is sent as a binary
message, anything else as text. A send failure raises `WebsocketSendException`.

## Run it with Docker

```sh
docker run --rm -p 9292:9292 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/ops-hub.camel.yaml
# in two terminals (websocat or any WebSocket client):
websocat ws://localhost:9292/ops
```

## Common questions

### How do I send to one client only?
Keep the `CamelWebsocketConnectionKey` header the consumer set and send to the same URI without
`sendToAll`.

### How do I push data that does not come from a client?
Send to `websocket://0.0.0.0:9292/path?sendToAll=true` from any route (a timer, a Kafka
consumer). The producer starts the server, so clients can connect before anything is sent.

### How do I serve wss?
Add `sslContextParameters=#tls` with a key store, or `useGlobalSslContextParameters=true`.

## Related

- [ws](ws.md): WebSocket client.
- [vertx-websocket](vertx-websocket.md): server with open/close events.
- [platform-http](platform-http.md): plain HTTP endpoints.
- Apache Camel: [WebSocket component](https://camel.apache.org/components/next/websocket-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
