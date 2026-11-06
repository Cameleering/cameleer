# Syslog component

> The `syslog` component receives and sends syslog messages (RFC 5424 and RFC 3164) over UDP, TCP or TLS and parses them into structured fields.

**Scheme:** `syslog:` · **Crate:** `cameleer-protocols` · **Cargo feature:** [`protocols`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Syslog data format](https://camel.apache.org/components/next/dataformats/syslog-dataformat.html)

## When to use it

- Run a syslog gateway on an edge site: collect logs from routers, switches and devices and forward only
  the important ones upstream (Kafka, HTTP, a chat alert).
- Turn syslog into structured JSON for a log pipeline or for an AI agent that triages alerts.
- Send syslog messages from a route to an existing syslog server.

Difference from Apache Camel: Camel has a syslog data format that you combine with `netty:udp://`.
Cameleer adds this `syslog:` component, and also provides the `syslog` data format.

## URI format

```text
syslog:udp://host:port[?options]
syslog:tcp://host:port[?options]
syslog:tls://host:port[?options]
```

The default port is 514 (6514 for `tls`). A consumer listens on `host:port`; a producer sends to it.

## Example

<!-- example: syslog-edge-gateway -->
```yaml
- route:
    id: syslog-gateway
    from:
      uri: syslog:udp://0.0.0.0:{{env:SYSLOG_PORT:5514}}
      steps:
        - filter:
            simple: "${header.CamelSyslogSeverity} in 'EMERG,ALERT,CRIT,ERR'"
            steps:
              - setBody:
                  simple: "${header.CamelSyslogHostname}: ${body[logMessage]}"
              - to: log:alerts
```

`<34>1 2026-11-05T10:00:00Z router1 sshd - - - authentication failure` (severity `CRIT`) becomes
`router1: authentication failure`; `INFO` messages are dropped.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("syslog:udp://0.0.0.0:5514")
            .route_id("syslog-gateway")
            .filter(simple("${header.CamelSyslogSeverity} in 'EMERG,ALERT,CRIT,ERR'"))
            .set_body(simple("${header.CamelSyslogHostname}: ${body[logMessage]}"))
            .to("log:alerts")
            .end();
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `unmarshal` | `true` | Consumer: the body is the parsed message (a map). `false` keeps the raw text. |
| `ssl` | `false` | Use TLS with `syslog:tcp://` (RFC 5425). |
| `sslContextParameters` | | `#bean` with TLS settings; the consumer needs key material. |
| `useGlobalSslContextParameters` | `false` | Use the global `camel.ssl.*` parameters. |
| `charset`, `sync`, `textline`, `delimiter`, `receiveBufferSize`, `sendBufferSize` | | Accepted and ignored. |

The parsed body has the fields `format` (`RFC5424` / `RFC3164`), `priority`, `facility` (name such as
`AUTH`), `facilityCode`, `severity` (`EMERG`, `ALERT`, `CRIT`, `ERR`, `WARNING`, `NOTICE`, `INFO`,
`DEBUG`), `severityCode`, `version`, `timestamp`, `timestampMillis`, `hostname`, `appName`, `procId`,
`msgId`, `structuredData` and `logMessage`.

TCP accepts octet-counted (RFC 6587) and line-terminated frames. The producer sends one datagram (UDP) or
one line (TCP), or an octet-counted frame over TLS. A map body is formatted as RFC 5424 first.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelSyslogFacility` | out | Facility name. |
| `CamelSyslogSeverity` | out | Severity name. |
| `CamelSyslogHostname` | out | Host name from the message. |
| `CamelSyslogTimestamp` | out | Timestamp from the message. |
| `CamelSyslogRemoteAddress`, `CamelSyslogLocalAddress` | out | Socket addresses. |

## Run it with Docker

```sh
docker run --rm -p 5514:5514/udp -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/syslog.camel.yaml
```

Point devices at UDP port 5514 of the host. Use port 514 only if the container may bind privileged ports.

## Common questions

### How do I forward the parsed message as JSON?

Add a `marshal` step with `json` after the consumer, then send it on (for example to [kafka](kafka.md)).

### Does it understand old BSD syslog lines?

Yes. RFC 3164 lines are parsed too; `format` tells you which kind arrived.

## Related

- [netty](netty.md), [mllp](mllp.md), [kafka](kafka.md), [stream](stream.md)
- [Apache Camel Syslog data format](https://camel.apache.org/components/next/dataformats/syslog-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
