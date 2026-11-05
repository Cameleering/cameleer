# RabbitMQ component

> The `rabbitmq` component sends and consumes RabbitMQ messages over AMQP 0-9-1 with the options of Apache Camel's older `camel-rabbitmq` component.

**Scheme:** `rabbitmq:` · **Crate:** `cameleer-amqp` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [RabbitMQ (Camel 3)](https://camel.apache.org/components/3.20.x/rabbitmq-component.html)

## When to use it

- Run routes written for Camel 2 or 3 `rabbitmq:` endpoints without rewriting them.
- Consume a RabbitMQ queue with manual acknowledgement (`autoAck=false`).
- Publish to an exchange with a routing key, publisher confirms or guaranteed delivery.

Apache Camel 4 replaced this component with [spring-rabbitmq](spring-rabbitmq.md); Cameleer keeps both.

## URI format

```text
rabbitmq:exchangeName[?options]
rabbitmq://host:port/exchangeName[?options]     (Camel 2 style)
```

## Example

<!-- example: rabbitmq-orders-queue -->
```yaml
- route:
    id: eu-orders
    from:
      uri: rabbitmq:orders
      parameters:
        hostname: "{{env:RABBITMQ_HOST:localhost}}"
        portNumber: "{{env:RABBITMQ_PORT:5672}}"
        username: "{{env:RABBITMQ_USER:guest}}"
        password: "{{env:RABBITMQ_PASSWORD:guest}}"
        queue: eu-orders
        routingKey: eu
        autoAck: false
        autoDelete: false
      steps:
        - log: "EU order: ${body}"
        - to: log:orders
```

The route declares the direct exchange `orders` and the queue `eu-orders` bound with key `eu`, and
acknowledges each message after the route has processed it.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("rabbitmq:orders?hostname=localhost&portNumber=5672&queue=eu-orders&routingKey=eu&autoAck=false&autoDelete=false")
            .route_id("eu-orders")
            .log("EU order: ${body}")
            .to("log:orders");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `hostname`, `portNumber` | `localhost`, `5672` | Broker address. |
| `addresses` | | `host:port` list; takes precedence over hostname and port. |
| `username`, `password`, `vhost` | `guest`, `guest`, `/` | Credentials and virtual host. |
| `queue` | | Queue to consume (comma separated for several). |
| `routingKey` | | Binding key / routing key. |
| `exchangeType` | `direct` | `direct`, `fanout`, `topic` or `headers`. |
| `declare` | `true` | Declare the exchange, queue and binding. |
| `durable` | `true` | Durable exchange and queue. |
| `autoDelete` | `true` | Auto-delete exchange and queue. |
| `exclusive`, `exclusiveConsumer` | `false` | Exclusive queue / consumer. |
| `autoAck` | `true` | Broker auto-ack. `false`: ack after success, reject on failure (requeued only when the exchange has header `CamelRabbitmqRequeue=true`). |
| `concurrentConsumers` | `1` | Consumer channels. |
| `prefetchEnabled`, `prefetchCount`, `prefetchGlobal` | `false`, `0`, `false` | `basic.qos` settings. |
| `deadLetterExchange`, `deadLetterQueue`, `deadLetterRoutingKey`, `deadLetterExchangeType` | | Dead letter topology. |
| `requestTimeout` | `20000` | InOut producer: wait (ms) for the reply. |
| `publisherAcknowledgements` | `false` | Wait for publisher confirms. |
| `publisherAcknowledgementsTimeout` | `5000` | ms to wait for a confirm. |
| `mandatory` | `false` | Publish with the mandatory flag. |
| `guaranteedDeliveries` | `false` | Confirms plus mandatory. |
| `bridgeEndpoint` | `false` | Ignore the `CamelRabbitmqRoutingKey` / `CamelRabbitmqExchangeName` headers of incoming messages. |
| `skipQueueDeclare`, `skipQueueBind`, `skipExchangeDeclare` | `false` | Skip parts of the declaration. |
| `arg.queue.*`, `arg.exchange.*`, `arg.binding.*` | | Declaration arguments. |
| `requestedHeartbeat` | `60` | Heartbeat in seconds. |
| `sslProtocol` | | `true`, `TLS`, `TLSv1.2` or `TLSv1.3` enables TLS (amqps). |
| `sslContextParameters`, `useGlobalSslContextParameters` | | TLS trust settings. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelRabbitmqRoutingKey` | in/out | Routing key (producer override unless `bridgeEndpoint`). |
| `CamelRabbitmqExchangeName` | in/out | Exchange (producer override unless `bridgeEndpoint`). |
| `CamelRabbitmqExchangeOverrideName` | in | Producer: exchange for this message. |
| `CamelRabbitmqDeliveryTag`, `CamelRabbitmqRedeliveryTag` | out | Consumer delivery information. |
| `CamelRabbitmqRequeue` | in | With `autoAck=false`: requeue a failed message. |
| `CamelRabbitmqRequestTimeout` | in | Producer: reply timeout for this message. |
| `CamelRabbitmqContentType`, `CamelRabbitmqCorrelationId`, `CamelRabbitmqMessageId`, `CamelRabbitmqPriority`, `CamelRabbitmqDeliveryMode`, `CamelRabbitmqExpiration`, `CamelRabbitmqTimestamp`, `CamelRabbitmqType`, `CamelRabbitmqUserId`, `CamelRabbitmqAppId`, `CamelRabbitmqReplyTo` | in/out | AMQP message properties. |

## Run it with Docker

```sh
docker run --rm -e RABBITMQ_HOST=rabbitmq -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml
```

## Common questions

### Why does my queue disappear when the route stops?

`autoDelete` defaults to `true` in this component, as in Camel's. Set `autoDelete=false` for a lasting queue.

### How do I requeue a message that failed?

With `autoAck=false`, set the header `CamelRabbitmqRequeue` to `true` in the error handling; otherwise a
failed message is rejected without requeue.

## Related

- [spring-rabbitmq](spring-rabbitmq.md), [amqp](amqp.md)
- [Apache Camel RabbitMQ component (Camel 3)](https://camel.apache.org/components/3.20.x/rabbitmq-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
