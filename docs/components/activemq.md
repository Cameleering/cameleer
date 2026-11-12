# ActiveMQ component

> The `activemq` component sends and receives messages on Apache ActiveMQ (Classic or Artemis) queues and topics with JMS semantics, over the broker's STOMP connector.

**Scheme:** `activemq:` · **Crate:** `cameleer-amqp` · **Cargo feature:** [`amqp`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [ActiveMQ](https://camel.apache.org/components/next/activemq-component.html)

## When to use it

- Keep Camel routes that use `activemq:queue:...` working against an existing ActiveMQ broker.
- Consume only matching messages with a JMS selector evaluated by the broker.
- Use transactions with redelivery and ActiveMQ's dead letter queue.

`activemq` is the [jms](jms.md) component under another name: all options and headers are the same. It
uses the broker's STOMP connector (port 61613), not OpenWire. ActiveMQ 5 does not redeliver after a STOMP
`ABORT` or `NACK`, so Cameleer redelivers failed transacted messages itself, like ActiveMQ's JMS client
(configurable with `jms.redeliveryPolicy.*` options in the broker URL).

## URI format

```text
activemq:[queue:|topic:|temp-queue:|temp-topic:]destinationName[?options]
```

## Example

<!-- example: activemq-selector -->
```yaml
- route:
    id: eu-orders
    from:
      uri: activemq:queue:orders
      parameters:
        brokerURL: "{{env:ACTIVEMQ_URL:tcp://localhost:61613}}"
        username: "{{env:ACTIVEMQ_USER:admin}}"
        password: "{{env:ACTIVEMQ_PASSWORD:admin}}"
        selector: "region = 'EU'"
      steps:
        - log: "EU order ${header.JMSMessageID}: ${body}"
        - to: log:eu-orders
```

Only messages whose `region` property is `EU` are delivered to this route; others stay on the queue for
other consumers.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("activemq:queue:orders?brokerURL=tcp://localhost:61613&selector=region = 'EU'")
            .route_id("eu-orders")
            .log("EU order ${header.JMSMessageID}: ${body}")
            .to("log:eu-orders");
    })
}
```

## Options

The same as [jms](jms.md#options): `brokerURL`, `username`, `password`, `clientId`, `concurrentConsumers`,
`selector`, `transacted`, `acknowledgementModeName`, `replyTo`, `requestTimeout`, `deliveryPersistent`,
`priority`, `timeToLive`, `durableSubscriptionName`, ... Component-wide settings use the prefix
`camel.component.activemq.` (for example `camel.component.activemq.brokerUrl`).

## Message headers

The same as [jms](jms.md#message-headers): `JMSMessageID`, `JMSCorrelationID`, `JMSDestination`,
`JMSReplyTo`, `JMSDeliveryMode`, `JMSPriority`, `JMSTimestamp`, `JMSType`, `JMSRedelivered`,
`JMSExpiration`, `CamelJmsDestinationName`, `CamelJmsRequestTimeout`.

## Run it with Docker

```sh
docker run --rm -e ACTIVEMQ_URL=tcp://activemq:61613 -v "$PWD:/routes:ro" cameleering/cameleer run /routes/orders.camel.yaml
```

## Common questions

### Can Cameleer use ActiveMQ's OpenWire protocol (port 61616)?

No. Point `brokerURL` at the STOMP connector (61613), which ActiveMQ Classic and Artemis enable by default.
For AMQP 1.0 (port 5672) use [amqp](amqp.md).

### Does the selector run in Cameleer or in the broker?

In the broker. Cameleer validates the selector syntax when the endpoint is created.

## Related

- [jms](jms.md), [amqp](amqp.md), [stomp](stomp.md)
- [Apache Camel ActiveMQ component](https://camel.apache.org/components/next/activemq-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
