# Netty component

> The `netty` component runs TCP and UDP servers and clients for plain socket protocols: text lines or raw bytes, one-way or request/reply.

**Scheme:** `netty:` · **Crate:** `cameleer-http` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Netty](https://camel.apache.org/components/next/netty-component.html)

## When to use it

- Accept line-based messages from devices, PLC gateways or legacy systems on a TCP port and answer each one.
- Receive UDP datagrams, such as telemetry from sensors on an edge network.
- Talk to a TCP service that speaks a simple text protocol (send a line, read a line back).

There is no Netty library underneath: Cameleer implements the same endpoint options with the Rust
standard library.

## URI format

```text
netty:tcp://host:port[?options]
netty:udp://host:port[?options]
```

A consumer binds `host:port`; port `0` picks a free port. A producer connects to `host:port`.

## Example

<!-- example: netty-tcp-line-server -->
```yaml
- route:
    id: tcp-line-server
    from:
      uri: netty:tcp://0.0.0.0:{{env:TCP_PORT:5150}}
      parameters:
        textline: true
        sync: true
      steps:
        - setBody:
            simple: "ACK ${body}"
```

A client that sends the line `reading-42` gets `ACK reading-42` back on the same connection.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("netty:tcp://0.0.0.0:5150?textline=true&sync=true")
            .route_id("tcp-line-server")
            .set_body(simple("ACK ${body}"));
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `textline` | `false` | Messages are delimited text lines. Otherwise each read (TCP) or datagram (UDP) is one message of raw bytes. |
| `sync` | `true` | Request/reply: the consumer writes the result back; the producer waits for one reply. |
| `delimiter` | `LINE` | `LINE` (`\n`) or `NULL` (`\0`). |
| `autoAppendDelimiter` | `true` | Add the delimiter to outgoing text lines. |
| `decoderMaxLineLength` | `1024` | Longest accepted line. |
| `encoding` | `UTF-8` | `UTF-8`, `US-ASCII` or `ISO-8859-1`. |
| `allowDefaultCodec` | `true` | Accepted; the default codec is raw bytes. |
| `disconnect` | `false` | Close the connection after each exchange. |
| `disconnectOnNoReply` | `true` | Close the connection when `sync` is on and there is no reply. |
| `keepAlive` | `true` | TCP keep-alive. |
| `reuseAddress` | `true` | `SO_REUSEADDR` on the server socket. |
| `tcpNoDelay` | `true` | Disable Nagle's algorithm. |
| `requestTimeout` | `0` | Producer: how long (ms) to wait for a reply. `0` waits forever on TCP. |
| `connectTimeout` | `10000` | Producer: connect timeout (ms). |
| `workerCount` | `16` | Consumer: worker threads for connections. |
| `ssl` | `false` | TCP only: use TLS. |
| `sslContextParameters` | | `#bean` with the TLS settings. |
| `keyStoreFile` / `keyStoreResource` | | Server certificate and key: PEM, PKCS#12 or JKS (with `passphrase`). |
| `trustStoreFile` / `trustStoreResource` | | CA certificates to trust. |
| `keyStoreFormat`, `passphrase` | | Store format (`JKS`, `PKCS12`, `PEM`) and password. |
| `hostnameVerification` | `false` | Producer: check the server certificate's host name (the chain is always verified). |
| `encoders`, `decoders`, `encoder`, `decoder` | | Not supported (custom Netty codecs); ignored with a warning. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelNettyRemoteAddress` | out (consumer) | The client's address. |
| `CamelNettyLocalAddress` | out (consumer) | The server's address. |
| `CamelNettyCloseChannelWhenComplete` | in | `true` closes the connection after the reply. |

## Run it with Docker

```sh
docker run --rm -p 5150:5150 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/tcp.camel.yaml
```

Add `/udp` to the published port for `netty:udp://` (`-p 5150:5150/udp`).

## Common questions

### How do I send a line to a TCP server and read the answer?

`to: netty:tcp://server:5150?textline=true&sync=true`. The reply line becomes the body.

### How do I receive UDP without replying?

`from: netty:udp://0.0.0.0:5150?sync=false`. Each datagram becomes one message.

### Does netty support DTLS for UDP?

No. TLS works with TCP only.

## Related

- [mllp](../packs/health/components/mllp.md), [syslog](syslog.md)
- [Apache Camel Netty component](https://camel.apache.org/components/next/netty-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
