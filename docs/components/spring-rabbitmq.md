# Spring RabbitMQ component

> The `spring-rabbitmq` component sends messages to and consumes messages from RabbitMQ exchanges and queues over AMQP 0-9-1, with Apache Camel's `spring-rabbitmq` options.

**Scheme:** `spring-rabbitmq:` · **Crate:** `cameleer-amqp` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Spring RabbitMQ](https://camel.apache.org/components/next/spring-rabbitmq-component.html)

## When to use it

- Consume work from RabbitMQ queues with acknowledgements, retries and dead-lettering.
- Route telemetry by topic routing keys (`sensor.*`) from many devices to the right consumers.
- Request/reply over RabbitMQ: the producer waits for the answer on a temporary reply queue.
- Connect AI agent workers through a RabbitMQ work queue (competing consumers, `prefetchCount`).

The AMQP 0-9-1 client is built into Cameleer; no Spring or Java library is involved.

## URI format

```text
spring-rabbitmq:exchangeName[?options]
```

`default` or an empty name means the AMQP default exchange.

## Example

<!-- example: spring-rabbitmq-telemetry -->
```yaml
- route:
    id: telemetry-consumer
    from:
      uri: spring-rabbitmq:telemetry
      parameters:
        host: "{{env:RABBITMQ_HOST:localhost}}"
        port: "{{env:RABBITMQ_PORT:5672}}"
        username: "{{env:RABBITMQ_USER:guest}}"
        password: "{{env:RABBITMQ_PASSWORD:guest}}"
        exchangeType: topic
        queues: edge-readings
        routingKey: "sensor.*"
      steps:
        - log: "${header.CamelRabbitmqRoutingKey}: ${body}"
        - to: log:telemetry
- route:
    id: telemetry-publisher
    from:
      uri: direct:reading
      steps:
        - to:
            uri: spring-rabbitmq:telemetry
            parameters:
              host: "{{env:RABBITMQ_HOST:localhost}}"
              port: "{{env:RABBITMQ_PORT:5672}}"
              username: "{{env:RABBITMQ_USER:guest}}"
              password: "{{env:RABBITMQ_PASSWORD:guest}}"
              exchangeType: topic
              routingKey: sensor.temperature
```

The consumer declares the topic exchange `telemetry`, the durable queue `edge-readings` and the binding
`sensor.*`. Messages sent to `direct:reading` are published with routing key `sensor.temperature` and
reach it.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("spring-rabbitmq:telemetry?host=localhost&port=5672&exchangeType=topic&queues=edge-readings&routingKey=sensor.*")
            .route_id("telemetry-consumer")
            .log("${header.CamelRabbitmqRoutingKey}: ${body}")
            .to("log:telemetry");
        b.from("direct:reading")
            .route_id("telemetry-publisher")
            .to("spring-rabbitmq:telemetry?host=localhost&port=5672&exchangeType=topic&routingKey=sensor.temperature");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `host`, `port` | `localhost`, `5672` | Broker address (Cameleer addition; Camel uses a `connectionFactory` bean). Also `spring.rabbitmq.host` / `port`. |
| `addresses` | | `host:port` list or `amqps://host:5671`; takes precedence over host and port. |
| `username`, `password`, `vhost` | `guest`, `guest`, `/` | Credentials and virtual host (also `spring.rabbitmq.*`). |
| `routingKey` | | Binding key (consumer) or routing key (producer). |
| `queues` | | Comma separated queues to consume. Without it, an exclusive server-named queue is used. |
| `exchangeType` | `direct` | `direct`, `fanout`, `topic` or `headers`. |
| `autoDeclare` | `true` | Consumer: declare the exchange, queues and bindings. |
| `autoDeclareProducer` | `false` | Producer: declare the exchange. |
| `durable` | `true` | Declare durable exchanges and queues. |
| `exclusive` | `false` | Declare exclusive queues. |
| `exclusiveConsumer` | `false` | Ask for an exclusive consumer. |
| `concurrentConsumers` | `1` | Consumer channels. |
| `acknowledgeMode` | `AUTO` | `AUTO` (ack on success, retry then reject on failure), `MANUAL` or `NONE` (broker auto-ack). |
| `prefetchCount` | `250` | `basic.qos` prefetch per consumer. |
| `maximumRetryAttempts` | `5` | `AUTO` mode: attempts before rejecting. |
| `retryDelay` | `1000` | ms between attempts. |
| `rejectAndDontRequeue` | `true` | Rejected messages are not requeued (they go to the dead letter exchange, if any). |
| `deadLetterExchange`, `deadLetterQueue`, `deadLetterRoutingKey`, `deadLetterExchangeType` | `direct` type | Dead letter topology, declared with the queue. |
| `replyTimeout` | `30000` | InOut producer: wait (ms) for the reply. |
| `disableReplyTo` | `false` | Ignore reply-to (consumer) / do not wait for a reply (producer). |
| `confirm` | `auto` | `enabled` waits for publisher confirms; `auto` / `disabled` do not. |
| `confirmTimeout` | `5000` | ms to wait for a confirm. |
| `allowNullBody` | `false` | Allow sending a null body. |
| `skipQueueDeclare`, `skipQueueBind`, `skipExchangeDeclare` | `false` | Skip parts of the declaration. |
| `arg.queue.*`, `arg.exchange.*`, `arg.binding.*` | | Declaration arguments, for example `arg.queue.x-message-ttl=60000`. |
| `requestedHeartbeat` | `60` | Heartbeat in seconds. |
| `requestedFrameMax`, `requestedChannelMax`, `connectionTimeout` | | Connection tuning. |
| `recoveryInterval` | `5000` | ms between reconnect attempts. |
| `sslContextParameters`, `useGlobalSslContextParameters` | | TLS (also enabled by an `amqps://` address or `spring.rabbitmq.ssl.enabled=true`). |

Spring-specific options with no meaning here (`connectionFactory`, `messageConverter`,
`maxConcurrentConsumers`, `messageListenerContainerType`, ...) are accepted and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelSpringRabbitmqRoutingOverrideKey` | in | Producer: routing key for this message. |
| `CamelSpringRabbitmqExchangeOverrideName` | in | Producer: exchange for this message. |
| `CamelSpringRabbitmqDeliveryTag` | out | Consumer: delivery tag. |
| `CamelRabbitmqRoutingKey`, `CamelRabbitmqExchangeName`, `CamelRabbitmqDeliveryTag`, `CamelRabbitmqRedeliveryTag` | out | Consumer: where the message came from. |
| `CamelRabbitmqContentType`, `CamelRabbitmqCorrelationId`, `CamelRabbitmqMessageId`, `CamelRabbitmqPriority`, `CamelRabbitmqDeliveryMode`, `CamelRabbitmqExpiration`, `CamelRabbitmqTimestamp`, `CamelRabbitmqType`, `CamelRabbitmqUserId`, `CamelRabbitmqAppId`, `CamelRabbitmqReplyTo`, `CamelRabbitmqContentEncoding`, `CamelRabbitmqClusterId` | in/out | AMQP message properties. |

Other headers (not starting with `Camel`) travel in the AMQP header table. String bodies are sent as
UTF-8 `text/plain`; maps and lists as JSON (`application/json`).

## Run it with Docker

```sh
docker run --rm -e RABBITMQ_HOST=rabbitmq -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/telemetry.camel.yaml
```

## Common questions

### How do I send failed messages to a dead letter queue?

Set `deadLetterExchange`, `deadLetterQueue` and optionally `deadLetterRoutingKey` on the consumer. After
`maximumRetryAttempts` the message is rejected and RabbitMQ moves it there.

### How do I do request/reply?

Send an InOut exchange (for example with `setExchangePattern: InOut`). The producer publishes with a
reply-to queue and waits up to `replyTimeout`; the consumer's result is sent back automatically.

### Should I use spring-rabbitmq or rabbitmq?

Use `spring-rabbitmq` for new routes (it is the Camel 4 component). [rabbitmq](rabbitmq.md) keeps routes
written for the older Camel RabbitMQ component working.

## Related

- [rabbitmq](rabbitmq.md), [amqp](amqp.md), [jms](jms.md)
- [Apache Camel Spring RabbitMQ component](https://camel.apache.org/components/next/spring-rabbitmq-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
