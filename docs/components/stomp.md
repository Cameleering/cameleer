# STOMP component

> The `stomp` component sends and receives messages over STOMP 1.2 with brokers such as ActiveMQ, Artemis and RabbitMQ, mapping STOMP headers to message headers.

**Scheme:** `stomp:` · **Crate:** `cameleer-amqp` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Stomp](https://camel.apache.org/components/next/stomp-component.html)

## When to use it

- Exchange plain messages with a STOMP destination (`/queue/...`, `/topic/...`) where JMS semantics are not needed.
- Talk to web or IoT clients that use STOMP (for example browser apps through a broker's STOMP gateway).
- Bridge STOMP destinations to Kafka, MQTT or HTTP on a gateway.

For acknowledgements per message, selectors, request/reply and transactions use [jms](jms.md), which runs
over the same STOMP client.

## URI format

```text
stomp:queue:name[?options]
stomp:topic:name[?options]
```

`stomp:queue:alerts` is the STOMP destination `/queue/alerts`.

## Example

<!-- example: stomp-alerts -->
```yaml
- route:
    id: stomp-alerts
    from:
      uri: stomp:queue:alerts
      parameters:
        brokerURL: "{{env:STOMP_URL:tcp://localhost:61613}}"
        login: "{{env:STOMP_USER:guest}}"
        passcode: "{{env:STOMP_PASSWORD:guest}}"
      steps:
        - log: "alert on ${header.destination}: ${body}"
        - to: log:alerts
```

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("stomp:queue:alerts?brokerURL=tcp://localhost:61613&login=guest&passcode=guest")
            .route_id("stomp-alerts")
            .log("alert on ${header.destination}: ${body}")
            .to("log:alerts");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `brokerURL` (alias `brokerUrl`) | `tcp://localhost:61613` | Broker address. `ssl://`, `stomp+ssl://` or `stomps://` connect with TLS. Also `camel.component.stomp.brokerUrl`. |
| `login`, `passcode` | | Credentials. |
| `host` | | STOMP virtual host (RabbitMQ vhost). |
| `clientId` | | Client id. |
| `sslContextParameters`, `useGlobalSslContextParameters` | | TLS trust settings. |

The [jms](jms.md#options) options are also accepted.

## Message headers

Consumers subscribe with `ack:auto` and turn every STOMP header (`destination`, `message-id`,
`subscription`, `content-type`, custom headers, ...) into a message header. Producers `SEND` the body
with every header that does not start with `Camel`.

## Run it with Docker

```sh
docker run --rm -e STOMP_URL=tcp://rabbitmq:61613 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/alerts.camel.yaml
```

## Common questions

### How do I connect to RabbitMQ over STOMP?

Enable the `rabbitmq_stomp` plugin and use `brokerURL=tcp://host:61613`, with `host=/` for the default vhost.

### What happens to a message when my route fails?

`stomp` consumers use `ack:auto`, so the broker considers it delivered. Use [jms](jms.md) with
`acknowledgementModeName=CLIENT_ACKNOWLEDGE` or `transacted=true` for redelivery.

## Related

- [jms](jms.md), [activemq](activemq.md), [paho](paho.md)
- [Apache Camel Stomp component](https://camel.apache.org/components/next/stomp-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
