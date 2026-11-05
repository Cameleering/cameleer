# AMQP component

> The `amqp` component sends and receives messages over AMQP 1.0 with JMS semantics, for ActiveMQ Artemis, ActiveMQ Classic, Qpid, RabbitMQ's AMQP 1.0 support and Azure Service Bus.

**Scheme:** `amqp:` · **Crate:** `cameleer-amqp10` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [AMQP](https://camel.apache.org/components/next/amqp-component.html)

## When to use it

- Integrate with brokers through the open AMQP 1.0 standard (port 5672, or 5671 with TLS).
- Use a broker queue as the work queue or event bus between AI agents and services, with request/reply,
  acknowledgements after processing and transactions.
- Store and forward from an edge site to a central broker over a standard, firewall-friendly protocol
  (also over WebSockets with `amqpws://` / `amqpwss://`).

The AMQP 1.0 stack is built into Cameleer (no Qpid JMS, no JVM). Messages follow the AMQP JMS mapping, so
Java JMS clients on the other side see normal JMS messages.

## URI format

```text
amqp:[queue:|topic:|temp-queue:]destinationName[?options]
```

## Example

<!-- example: amqp-queue-processing -->
```yaml
- route:
    id: process-orders
    from:
      uri: amqp:queue:orders
      parameters:
        remoteAddress: "{{env:AMQP_URL:amqp://localhost:5672}}"
        username: "{{env:AMQP_USER:admin}}"
        password: "{{env:AMQP_PASSWORD:admin}}"
        acknowledgementModeName: CLIENT_ACKNOWLEDGE
      steps:
        - setHeader:
            name: processedBy
            constant: cameleer
        - to: amqp:queue:orders.processed?remoteAddress={{env:AMQP_URL:amqp://localhost:5672}}&username={{env:AMQP_USER:admin}}&password={{env:AMQP_PASSWORD:admin}}
```

Each order is forwarded to `orders.processed` with an extra property. With `CLIENT_ACKNOWLEDGE` a message
whose route fails is returned to the broker and redelivered with `JMSRedelivered=true`.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.properties().set("camel.component.amqp.remoteAddress", "amqp://localhost:5672");
    ctx.add_routes(|b| {
        b.from("amqp:queue:orders?acknowledgementModeName=CLIENT_ACKNOWLEDGE")
            .route_id("process-orders")
            .set_header("processedBy", constant("cameleer"))
            .to("amqp:queue:orders.processed");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `remoteAddress` (aliases `remoteURI`, `brokerURL`, `connectionFactory.remoteURI`) | | `amqp://host:5672`, `amqps://host:5671` (TLS), `amqpws://` / `amqpwss://` (WebSockets). Also `camel.component.amqp.remoteAddress`, or `host` + `port`. |
| `username`, `password` | | Credentials (or user info in the URL). |
| `connectionFactory` | | `#bean` of type `AmqpConnectionFactory` with URI, credentials, client id and TLS. |
| `saslMechanism` | | Force a SASL mechanism. |
| `clientId` | | Container id, needed for durable subscriptions. |
| `concurrentConsumers` | `1` | Receivers on the destination. |
| `prefetchSize` | `100` | Link credit per receiver. |
| `selector` | | JMS selector, evaluated by the broker. |
| `acknowledgementModeName` | `AUTO_ACKNOWLEDGE` | `AUTO_ACKNOWLEDGE`, `DUPS_OK_ACKNOWLEDGE`, `CLIENT_ACKNOWLEDGE` (failed messages are returned for redelivery) or `SESSION_TRANSACTED`. |
| `transacted` | `false` | AMQP local transactions: acknowledge and reply inside a transaction; failures roll back. |
| `replyTo` | | Fixed reply address instead of a dynamic temporary queue. |
| `requestTimeout` | `20000` | InOut producer: wait (ms) for the reply. |
| `disableReplyTo` | `false` | Do not handle reply-to. |
| `deliveryPersistent` | `true` | Persistent sends wait for the broker's `accepted` outcome; others are sent pre-settled. |
| `deliveryMode` | | `1` or `2`. |
| `priority` | | Message priority. |
| `timeToLive` | `-1` | Expiry in ms. |
| `preserveMessageQos` | `false` | Take QoS from the JMS headers. |
| `jmsMessageType` | from the body | `Text`, `Bytes`, `Map` or `Object`. |
| `durableSubscriptionName`, `subscriptionName`, `subscriptionDurable`, `subscriptionShared` | | Durable and shared (JMS 2.0) topic subscriptions. |
| `includeAmqpAnnotations` | `false` | Message annotations become `JMS_AMQP_MA_<key>` headers. |
| `recoveryInterval` | `5000` | ms between reconnect attempts. |
| `idleTimeout` | `60000` | AMQP idle timeout (heartbeats). |
| `maxFrameSize` | `1048576` | Largest frame. |
| `sslContextParameters`, `useGlobalSslContextParameters` | | TLS trust settings. |

## Message headers

| header | direction | description |
|---|---|---|
| `JMSMessageID`, `JMSCorrelationID`, `JMSDestination`, `JMSReplyTo`, `JMSType`, `JMSTimestamp`, `JMSExpiration`, `JMSDeliveryMode`, `JMSPriority`, `JMSRedelivered` | out (consumer), some in (producer) | Mapped from the AMQP message properties and header. |
| `JMSXDeliveryCount`, `JMSXUserID`, `JMSXGroupID`, `JMSXGroupSeq` | out | AMQP delivery count, user id and group. |
| `JMS_AMQP_MA_<key>` | out | Message annotations, with `includeAmqpAnnotations=true`. |

Other headers are AMQP application properties.

## Run it with Docker

```sh
docker run --rm -e AMQP_URL=amqp://artemis:5672 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml
```

## Common questions

### Which brokers work?

Anything that speaks AMQP 1.0: ActiveMQ Artemis, ActiveMQ Classic (with the `queue://` / `topic://`
prefixes it announces), Apache Qpid, RabbitMQ with AMQP 1.0, and Azure Service Bus (see also the
`azure-servicebus` component).

### Is this the same as RabbitMQ's AMQP 0-9-1?

No. AMQP 0-9-1 is a different protocol; use [spring-rabbitmq](spring-rabbitmq.md) or
[rabbitmq](rabbitmq.md) for it.

### My old Cameleer routes used `amqp:` for STOMP. What changed?

`amqp:` now means AMQP 1.0, as in Apache Camel. The STOMP flavour is available as
[amqp-stomp](amqp-stomp.md) (and [jms](jms.md)).

## Related

- [jms](jms.md), [activemq](activemq.md), [amqp-stomp](amqp-stomp.md), [kafka](kafka.md)
- [Apache Camel AMQP component](https://camel.apache.org/components/next/amqp-component.html)
