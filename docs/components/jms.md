# JMS component

> The `jms` component sends and receives JMS-style messages (queues, topics, selectors, request/reply, transactions) through any broker that speaks STOMP 1.2, such as ActiveMQ, Artemis or RabbitMQ.

**Scheme:** `jms:` · **Also:** `activemq:`, `amqp-stomp:` (same component, see [activemq](activemq.md), [amqp-stomp](amqp-stomp.md)) · **Crate:** `cameleer-amqp` · **Cargo feature:** [`amqp`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [JMS](https://camel.apache.org/components/next/jms-component.html)

## When to use it

- Move Camel routes that use `jms:queue:...` to Cameleer without a JVM or a JMS client library.
- Request/reply over a queue: a service answers on a temporary reply queue, as with JMS `replyTo`.
- Competing consumers on a queue, publish-subscribe on a topic, durable and shared subscriptions.
- Expose an AI model or agent as a queue-based service that other systems call with request/reply.

Difference from Apache Camel: there is no JMS API in Rust, so Cameleer speaks STOMP 1.2 to the broker
and maps JMS semantics onto it. Use [amqp](amqp.md) for JMS over AMQP 1.0. XA transactions are not
possible over STOMP (`transactionManager` is accepted and ignored); `transacted=true` gives local
transactions.

## URI format

```text
jms:[queue:|topic:|temp-queue:|temp-topic:]destinationName[?options]
```

Without a prefix the destination is a queue.

## Example

<!-- example: jms-request-reply -->
```yaml
- route:
    id: price-service
    from:
      uri: jms:queue:prices
      parameters:
        brokerURL: "{{env:JMS_BROKER_URL:tcp://localhost:61613}}"
        username: "{{env:JMS_USER:admin}}"
        password: "{{env:JMS_PASSWORD:admin}}"
        concurrentConsumers: 2
      steps:
        - setBody:
            simple: "price of ${body}: 42.00 EUR"
```

A caller that sends `ACME` as an InOut message to `jms:queue:prices` gets `price of ACME: 42.00 EUR`
back through a temporary reply queue.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("jms:queue:prices?brokerURL=tcp://localhost:61613&concurrentConsumers=2")
            .route_id("price-service")
            .set_body(simple("price of ${body}: 42.00 EUR"));
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `brokerURL` (alias `brokerUrl`) | `tcp://localhost:61613` | Broker STOMP address: `tcp://`, `stomp://`, `amqp://` or `failover:(...)` (first address used). `ssl://`, `stomp+ssl://` or `stomps://` use TLS. Also `camel.component.jms.brokerUrl`. |
| `username`, `password` | | Credentials (also `camel.component.jms.username` / `password`). |
| `host` | | STOMP virtual host. |
| `clientId` | | Client id, needed for durable subscriptions. |
| `concurrentConsumers` | `1` | Consumers on a queue (one STOMP subscription each). |
| `prefetchSize` | `1` | Unacknowledged messages per consumer. |
| `selector` | | JMS selector, evaluated by the broker, for example `region = 'EU' AND priority > 5`. |
| `acknowledgementModeName` (or `acknowledgementMode` 0-3) | `AUTO_ACKNOWLEDGE` | `AUTO_ACKNOWLEDGE`, `DUPS_OK_ACKNOWLEDGE`, `CLIENT_ACKNOWLEDGE` (failed messages are redelivered) or `SESSION_TRANSACTED`. |
| `transacted` | `false` | Process each message in a STOMP transaction; a failure aborts it and the broker redelivers. |
| `replyTo` | | Fixed reply queue instead of a temporary one. |
| `replyToType` | `Temporary` (`Shared` with `replyTo`) | `Temporary`, `Shared` or `Exclusive`. |
| `requestTimeout` | `20000` | InOut producer: wait (ms) for the reply. |
| `disableReplyTo` | `false` | Do not handle reply-to. |
| `useMessageIDAsCorrelationID` | `false` | Reply with the request's `JMSMessageID` as correlation id. |
| `deliveryPersistent` | `true` | Persistent sends (wait for a broker receipt). |
| `deliveryMode` | | `1` (non-persistent) or `2` (persistent). |
| `priority` | broker default | Message priority, 0 to 9. |
| `timeToLive` | `-1` | Message expiry in ms. `-1` never expires. |
| `explicitQosEnabled` | | Accepted; QoS options always apply. |
| `preserveMessageQos` | `false` | Take priority, delivery mode and expiry from the `JMSPriority`, `JMSDeliveryMode`, `JMSExpiration` headers. |
| `jmsMessageType` | from the body | `Text`, `Bytes`, `Map` or `Object` (map and object bodies travel as JSON). |
| `durableSubscriptionName` | | Durable topic subscription name. |
| `subscriptionName`, `subscriptionDurable`, `subscriptionShared` | | JMS 2.0 durable and shared subscriptions. |
| `recoveryInterval` | `5000` | ms between reconnect attempts. |
| `sslContextParameters`, `useGlobalSslContextParameters` | | TLS trust settings. |

## Message headers

| header | direction | description |
|---|---|---|
| `JMSMessageID`, `JMSCorrelationID`, `JMSDestination`, `JMSReplyTo`, `JMSDeliveryMode`, `JMSPriority`, `JMSTimestamp`, `JMSType`, `JMSRedelivered`, `JMSExpiration` | out (consumer), some in (producer) | JMS message headers. |
| `CamelJmsDestinationName` | in | Producer: send to this destination instead. |
| `CamelJmsRequestTimeout` | in | Producer: reply timeout for this message. |

Other headers travel as message properties (keys encoded with Camel's `_DOT_` / `_HYPHEN_` strategy).

## Run it with Docker

```sh
docker run --rm -e JMS_BROKER_URL=tcp://activemq:61613 -v "$PWD:/routes:ro" cameleering/cameleer run /routes/prices.camel.yaml
```

The broker must have its STOMP connector enabled (port 61613 by default on ActiveMQ and Artemis; the
`rabbitmq_stomp` plugin on RabbitMQ).

## Common questions

### How do I call a JMS service and wait for the answer?

Send an InOut exchange: add a `setExchangePattern: InOut` step before `to: jms:queue:prices`, or call
`request_body` on a producer template. The reply body replaces the message.

### How do I keep topic messages while my route is stopped?

Set `clientId` and `durableSubscriptionName` (or `subscriptionName` with `subscriptionDurable=true`).
A durable subscription that is not shared requires `clientId`.

### Does Cameleer support XA transactions with JMS?

No. STOMP has no two-phase commit. Use `transacted=true` for local transactions and an idempotent consumer
for exactly-once processing.

## Related

- [activemq](activemq.md), [amqp-stomp](amqp-stomp.md), [stomp](stomp.md), [amqp](amqp.md)
- [Apache Camel JMS component](https://camel.apache.org/components/next/jms-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
