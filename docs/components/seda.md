# SEDA component

> The `seda` component hands messages to another route through a bounded in-memory queue, so the sender does not wait for the processing.

**Scheme:** `seda:` · **Crate:** `cameleer-core` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [SEDA](https://camel.apache.org/components/next/seda-component.html)

## When to use it

- Decouple a fast producer from slow work: accept a request, queue it, answer at once.
- Store and forward on an edge gateway: buffer sensor readings in memory while a slower uplink route
  forwards them (combine with [kafka](kafka.md) or [paho](paho.md) for the uplink).
- Process messages in parallel with `concurrentConsumers`.
- Fan out one message to several routes with `multipleConsumers=true` (publish-subscribe).

## URI format

```text
seda:name[?options]
```

Endpoints with the same `name` share one queue inside a `CamelContext`. Use [vm](vm.md) to share a
queue across contexts.

## Example

<!-- example: seda-store-and-forward -->
```yaml
- route:
    id: ingest-readings
    from:
      uri: direct:reading
      steps:
        - to: seda:readings?size=10000&blockWhenFull=true
        - setBody:
            constant: queued
- route:
    id: store-readings
    from:
      uri: seda:readings
      parameters:
        size: 10000
        concurrentConsumers: 2
      steps:
        - log: "storing ${body}"
        - to: log:stored
```

The caller of `direct:reading` gets `queued` back immediately. Two threads take readings off the queue
and process them. When 10000 readings are waiting, `blockWhenFull=true` makes the sender wait instead
of failing.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:reading")
            .route_id("ingest-readings")
            .to("seda:readings?size=10000&blockWhenFull=true")
            .set_body(constant("queued"));
        b.from("seda:readings?size=10000&concurrentConsumers=2")
            .route_id("store-readings")
            .log("storing ${body}")
            .to("log:stored");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `size` (alias `queueSize`) | `1000` | Maximum number of messages in the queue. Every endpoint that names a size for the same queue must use the same value. |
| `concurrentConsumers` | `1` | Number of threads consuming from the queue. |
| `waitForTaskToComplete` | `IfReplyExpected` | `Always`, `Never` or `IfReplyExpected`: whether the sender waits for the consumer to finish. InOut exchanges wait with the default. |
| `timeout` | `30000` | How long (ms) a waiting sender waits for the consumer. `0` waits forever. |
| `multipleConsumers` | `false` | Publish-subscribe: every consumer gets a copy of each message. Otherwise consumers compete. |
| `limitConcurrentConsumers` | `true` | Reject `concurrentConsumers` above 500. |
| `blockWhenFull` | `false` | When the queue is full, wait instead of failing. |
| `discardWhenFull` | `false` | When the queue is full, drop the message silently. |
| `offerTimeout` | `0` | With `blockWhenFull`, how long (ms) to wait for space. `0` waits forever. |
| `pollTimeout` | `1000` | How long (ms) a consumer thread waits for a message before checking for shutdown. |
| `purgeWhenStopping` | `false` | Empty the queue when the consumer stops. |
| `failIfNoConsumers` | `false` | Fail the send when no consumer is running. |
| `discardIfNoConsumers` | `false` | Drop the message when no consumer is running. |

## Message headers

The queue does not add headers. The consumer receives a copy of the exchange with a new exchange id
and the property `CamelCorrelationId` set to the sender's exchange id.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/readings.camel.yaml
```

## Common questions

### Are seda messages lost when the process stops?

Yes. The queue lives in memory. For durable store and forward, put the messages on a broker such as
[kafka](kafka.md), [amqp](amqp.md) or [jms](jms.md), or write them to files.

### How do I make every route get every message?

Set `multipleConsumers=true` on the consumers. Each consumer then gets its own copy, like a topic.

### What happens when the queue is full?

By default the send fails with an `IllegalStateException` ("Queue full"). Use `blockWhenFull=true` to
apply back pressure or `discardWhenFull=true` to drop new messages.

### Can I read a seda queue on demand?

Yes. seda endpoints support polling consumers, so `pollEnrich` and a consumer template can take one
message at a time.

## Related

- [direct](direct.md), [vm](vm.md), [stub](stub.md)
- [EIPs](../guides/eips.md)
- [Apache Camel SEDA component](https://camel.apache.org/components/next/seda-component.html)
