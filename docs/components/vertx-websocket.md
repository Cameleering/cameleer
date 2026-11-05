# Vert.x WebSocket component

> The `vertx-websocket` component runs a WebSocket server (or client) with Vert.x-style headers, including events when a client connects or disconnects.

**Scheme:** `vertx-websocket:` · **Crate:** `cameleer-protocols` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Vert.x WebSocket](https://camel.apache.org/components/next/vertx-websocket-component.html)

Cameleer has no Vert.x; the scheme runs on the same RFC 6455 implementation as
[websocket](websocket.md) and uses the `CamelVertxWebsocket.*` header names, so Camel routes
written for camel-vertx-websocket run unchanged.

## When to use it

- Know when clients connect and disconnect (`fireWebSocketConnectionEvents=true`), e.g. to
  track which AI agents or devices are online.
- Answer each client individually over its own connection.
- Consume from a remote WebSocket server as a client (`consumeAsClient=true`).

## URI format

```text
vertx-websocket:host:port/path[?options]
```

The default port is `0` (a free port), so give one explicitly. A producer whose URI matches a
server endpoint sends to that server's clients; otherwise it connects as a client.

## Example

A presence-aware endpoint for agents: connections and disconnections are logged, and every
message is acknowledged to the client that sent it.

<!-- example: vertx-websocket-agent-presence -->
```yaml
- route:
    id: agent-presence
    from:
      uri: "vertx-websocket:0.0.0.0:{{env:WS_PORT:8090}}/agents?fireWebSocketConnectionEvents=true"
      steps:
        - choice:
            when:
              - simple: "${header[CamelVertxWebsocket.event]} == 'MESSAGE'"
                steps:
                  - setBody:
                      simple: "ack: ${body}"
                  - to: "vertx-websocket:0.0.0.0:{{env:WS_PORT:8090}}/agents"
            otherwise:
              steps:
                - log: "${header[CamelVertxWebsocket.event]} ${header[CamelVertxWebsocket.remoteAddress]}"
```

## Rust DSL

```rust
fn agent_presence_route(b: &mut RouteBuilder, port: u16) {
    let endpoint = format!("vertx-websocket:0.0.0.0:{}/agents", port);
    b.from(&format!("{}?fireWebSocketConnectionEvents=true", endpoint))
        .route_id("agent-presence")
        .choice()
        .when(simple("${header[CamelVertxWebsocket.event]} == 'MESSAGE'"))
        .set_body(simple("ack: ${body}"))
        .to(&endpoint)
        .otherwise()
        .log("${header[CamelVertxWebsocket.event]} ${header[CamelVertxWebsocket.remoteAddress]}")
        .end_choice();
}
```

## Options

| option | default | description |
|---|---|---|
| `fireWebSocketConnectionEvents` | `false` | Also route `OPEN` and `CLOSE` events (header `CamelVertxWebsocket.event`). |
| `consumeAsClient` | `false` | The consumer connects to `host:port/path` as a client instead of serving it. |
| `sendToAll` | `false` | Producer: send to every connection of the path. |
| `connectTimeout` | `10s` | Client connect timeout. |
| `reconnectInterval` | `0` | Client consumer: reconnect delay (`0` = no reconnect). |
| `allowedOrigins` | any | Accepted `Origin` values. Alias `allowedOriginPattern`. |
| `subProtocol` | | Subprotocols. Aliases `subProtocols`, `clientSubProtocols`. |
| `maxTextMessageSize`, `maxBinaryMessageSize`, `maxMessageSize` | 64 MiB | Largest incoming message. |
| `maxFrameSize` | 65536 | Outgoing frame size. |
| `sslContextParameters` | | `#bean` with a key store: serve `wss`. |
| `useGlobalSslContextParameters` | `false` | Serve `wss` with the global `camel.ssl.*` parameters. |

`router`, `serverOptions`, `clientOptions` and `handshakeHeaders` are accepted and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelVertxWebsocket.connectionKey` | out / in | Consumer: the sending connection. Producer: comma-separated connections to send to. |
| `CamelVertxWebsocket.remoteAddress` | out | Client address. |
| `CamelVertxWebsocket.event` | out | `MESSAGE`, `OPEN` or `CLOSE`. |
| `CamelVertxWebsocket.sendToAll` | in | `true` broadcasts this message. |

## Run it with Docker

```sh
docker run --rm -p 8090:8090 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/agent-presence.camel.yaml
websocat ws://localhost:8090/agents
```

## Common questions

### How do I know when a client disconnects?
Set `fireWebSocketConnectionEvents=true`; a `CLOSE` exchange arrives with the connection key.

### How do I reply only to the sender?
Send to the same server URI with the `CamelVertxWebsocket.connectionKey` header the consumer
set, as in the example.

### Can it connect to another server?
Yes: `consumeAsClient=true` on the consumer, and a producer whose URI has no local server
connects as a client.

## Related

- [websocket](websocket.md), [ws](ws.md).
- Apache Camel: [Vert.x WebSocket component](https://camel.apache.org/components/next/vertx-websocket-component.html).
