# MLLP component

> The `mllp` component sends and receives HL7 v2 healthcare messages over TCP with MLLP framing, and generates HL7 acknowledgements automatically.

**Scheme:** `mllp:` · **Crate:** `cameleer-protocols` · **Cargo feature:** [`protocols`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [MLLP](https://camel.apache.org/components/next/mllp-component.html)

## When to use it

- Build an HL7 interface engine: receive ADT, ORM or ORU messages from a hospital system and forward them.
- Run a small HL7 gateway next to lab devices or in a clinic (edge), forwarding messages to Kafka or HTTP.
- Send HL7 messages to a remote system and check its acknowledgement (`AA`, `AE`, `AR`).

## URI format

```text
mllp://host:port[?options]
```

A consumer listens on `host:port` (`0.0.0.0` for all interfaces); a producer connects to it.

## Example

<!-- example: mllp-hl7-gateway -->
```yaml
- route:
    id: hl7-inbound
    from:
      uri: mllp://0.0.0.0:{{env:MLLP_PORT:2575}}
      steps:
        - log: "HL7 ${header.CamelMllpMessageType} from ${header.CamelMllpSendingApplication}, control id ${header.CamelMllpMessageControlId}"
        - to: log:hl7
```

Each message is answered with `MSA|AA|<control id>` when the route succeeds and `MSA|AE|...` when it
fails. Send a test message with another route: `to: mllp://localhost:2575`.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("mllp://0.0.0.0:2575")
            .route_id("hl7-inbound")
            .log("HL7 ${header.CamelMllpMessageType} from ${header.CamelMllpSendingApplication}, control id ${header.CamelMllpMessageControlId}")
            .to("log:hl7");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `autoAck` | `true` | Consumer: generate the HL7 acknowledgement. |
| `hl7Headers` | `true` | Consumer: set the `CamelMllp*` headers from the `MSH` segment. |
| `stringPayload` | `true` | Body as a string; `false` gives bytes. |
| `validatePayload` | `false` | Check the HL7 structure of received messages. |
| `charsetName` | `UTF-8` | `UTF-8` or `ISO-8859-1`. |
| `receiveTimeout` | `15000` | Producer: how long (ms) to wait for the acknowledgement. |
| `readTimeout` | `5000` | How long (ms) to wait for the rest of a started frame. |
| `connectTimeout` | `30000` | Producer: connect timeout (ms). |
| `idleTimeout` | `0` | Consumer: close idle connections after this many ms. `0` never closes. |
| `keepAlive` | `true` | TCP keep-alive. |
| `tcpNoDelay` | `true` | Disable Nagle's algorithm. |
| `maxBufferSize` | 1 GiB | Largest accepted frame. |
| `sslContextParameters` | | `#bean`: the consumer accepts TLS (needs key material) and the producer connects with TLS. |
| `useGlobalSslContextParameters` | `false` | Use the global `camel.ssl.*` parameters (with `camel.ssl.enabled=true`) for TLS. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelMllpSendingApplication`, `CamelMllpSendingFacility`, `CamelMllpReceivingApplication`, `CamelMllpReceivingFacility` | out (consumer) | From `MSH-3` to `MSH-6`. |
| `CamelMllpTimestamp`, `CamelMllpSecurity`, `CamelMllpMessageType`, `CamelMllpEventType`, `CamelMllpTriggerEvent`, `CamelMllpMessageControlId`, `CamelMllpProcessingId`, `CamelMllpVersionId` | out (consumer) | From the other `MSH` fields. |
| `CamelMllpCharset` | out (consumer) | The character set. |
| `CamelMllpLocalAddress`, `CamelMllpRemoteAddress` | out | Socket addresses. |
| `CamelMllpAcknowledgement` / `CamelMllpAcknowledgementString` | in (consumer), out (producer) | The acknowledgement as bytes / string. A route can set it to send its own. |
| `CamelMllpAcknowledgementType` | in (consumer), out (producer) | `AA`, `AE`, `AR` (or `CA`, `CE`, `CR`). A route can set it to force the code. |

A producer that receives `AE` / `AR` fails with `MllpApplicationErrorAcknowledgementException` /
`MllpApplicationRejectAcknowledgementException`; no acknowledgement gives
`MllpAcknowledgementTimeoutException`.

## Run it with Docker

```sh
docker run --rm -p 2575:2575 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/hl7.camel.yaml
```

## Common questions

### How do I send my own acknowledgement?

Set the header `CamelMllpAcknowledgementString` (or `CamelMllpAcknowledgement` as bytes) in the route.
To only change the code, set `CamelMllpAcknowledgementType` to `AE` or `AR`.

### How do I read HL7 fields?

Use the `hl7terser` language with a terser path, for example a `setHeader` step whose expression is
`hl7terser: PID-5-1` (the patient's family name). The `hl7` data format parses the whole message.

### Is the connection kept open?

Yes. The producer reuses its connection between messages.

## Related

- [netty](netty.md), [syslog](syslog.md), [kafka](kafka.md)
- [Apache Camel MLLP component](https://camel.apache.org/components/next/mllp-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
