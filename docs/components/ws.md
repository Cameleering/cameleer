# WS component

> The `ws` component is a WebSocket client: it connects to a WebSocket server, receives the server's messages as exchanges and sends messages on the same connection.

**Scheme:** `ws:` · **Also:** `wss:` · **Crate:** `cameleer-protocols` · **Cargo feature:** [`protocols`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [AHC WebSocket](https://camel.apache.org/components/next/ahc-ws-component.html)

`wss:` connects over TLS and always verifies the server certificate. To run a WebSocket server,
use [websocket](websocket.md) or [vertx-websocket](vertx-websocket.md).

## When to use it

- Subscribe to a streaming API (market data, chat, a realtime events feed) and route each
  message.
- Connect an edge device to a central WebSocket hub and receive commands.
- Send messages to a WebSocket service from any route.

## URI format

```text
ws://host[:port]/path[?options]
wss://host[:port]/path[?options]
```

Default ports: 80 for `ws`, 443 for `wss`. A consumer and a producer with the same URI share
one connection.

## Example

An edge agent that connects to a central hub, receives commands and keeps only those addressed
to its site. `reconnectInterval` makes the client reconnect after the connection drops.

<!-- example: ws-edge-commands -->
```yaml
- route:
    id: hub-commands
    from:
      uri: "ws://{{env:HUB_ADDRESS:localhost:9292}}/commands?reconnectInterval=5s"
      steps:
        - filter:
            simple: "${body} starts with '{{env:SITE_ID:site-1}}:'"
            steps:
              - log: "command for this site: ${body}"
```

## Rust DSL

```rust
fn hub_commands_route(b: &mut RouteBuilder, hub: &str) {
    b.from(&format!("ws://{}/commands?reconnectInterval=5s", hub))
        .route_id("hub-commands")
        .filter(simple("${body} starts with 'site-1:'"))
        .log("command for this site: ${body}");
}
```

## Options

| option | default | description |
|---|---|---|
| `connectTimeout` | `10s` | Connect and handshake timeout. |
| `reconnectInterval` | `0` | Consumer: reconnect after this delay when the connection drops (`0` = no reconnect). |
| `subProtocol` | | Subprotocols to request. Aliases `subProtocols`, `clientSubProtocols`. |
| `maxTextMessageSize`, `maxBinaryMessageSize`, `maxMessageSize` | 64 MiB | Largest incoming message. |
| `maxFrameSize` | 65536 | Fragment outgoing messages into frames of this size. |
| `sslContextParameters` | | `wss`: `#bean` with the trust store (and a key store for client certificates). |
| `useGlobalSslContextParameters` | `false` | `wss`: use the global `camel.ssl.*` parameters. |

Options of Apache Camel's AHC client (`clientOptions`, `reconnectMaxAttempts`, ...) are accepted
and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelWebsocketConnectionKey` | out | Key of the client connection. |

Text messages arrive as strings and binary messages as bytes. The producer sends byte bodies as
binary messages and anything else as text.

## Run it with Docker

```sh
docker run --rm -e HUB_ADDRESS=hub.example.com:9292 -e SITE_ID=site-7 \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/hub-commands.camel.yaml
```

## Common questions

### How do I connect over TLS?
Use `wss://host/path`. The certificate is checked against the system trust store, or the trust
store in `sslContextParameters`.

### How do I send a message to the server?
`to: ws://host:port/path` sends the body on the connection; a consumer on the same URI receives
the replies.

### What happens when the server goes away?
Without `reconnectInterval` the consumer stops receiving. With it, the client reconnects on that
interval.

## Related

- [websocket](websocket.md), [vertx-websocket](vertx-websocket.md): WebSocket servers.
- [http](http.md): request/response HTTP calls.
- Apache Camel: [AHC WebSocket component](https://camel.apache.org/components/next/ahc-ws-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
