# Google Pub/Sub component

> The `google-pubsub` component publishes messages to Google Cloud Pub/Sub topics and consumes messages from Pub/Sub subscriptions, with gRPC streaming pull by default.

**Scheme:** `google-pubsub:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Google Pubsub](https://camel.apache.org/components/next/google-pubsub-component.html)

## When to use it

- Turn webhooks or HTTP calls into Pub/Sub events for downstream services.
- Consume a subscription and write each message to a database, a file or another system.
- Connect edge gateways to Google Cloud: publish readings from the device, consume commands.
- Feed AI agents: publish tasks to a topic and let several workers consume the subscription.

## URI format

```text
google-pubsub://projectId:destinationName[?options]
```

The destination is a **topic** for the producer and a **subscription** for the consumer.

## Example

Accept JSON events on an HTTP endpoint and publish each one to a topic:

<!-- example: google-pubsub-publish-webhooks -->
```yaml
- route:
    id: webhook-to-pubsub
    from:
      uri: "platform-http:/events?httpMethodRestrict=POST"
      steps:
        - to: "google-pubsub://{{env:GCP_PROJECT:my-project}}:{{env:PUBSUB_TOPIC:events}}"
        - setHeader:
            name: CamelHttpResponseCode
            constant: 202
        - setBody:
            simple: "${header.CamelGooglePubsubMessageId}"
```

Consume a subscription (streaming pull) and write every message to a file named after its
message id. Successful exchanges are acknowledged, failed ones are nacked and redelivered:

<!-- example: google-pubsub-consume-subscription -->
```yaml
- route:
    id: pubsub-to-files
    from:
      uri: "google-pubsub://{{env:GCP_PROJECT:my-project}}:{{env:PUBSUB_SUBSCRIPTION:events-sub}}?concurrentConsumers=2"
      steps:
        - log: "message ${header.CamelGooglePubsubMessageId} published at ${header.CamelGooglePubsubPublishTime}"
        - setHeader:
            name: CamelFileName
            simple: "${header.CamelGooglePubsubMessageId}.json"
        - to: "file:{{env:OUT_DIR:out}}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("google-pubsub://my-project:events-sub")
        .log("got ${body}")
        .to("google-pubsub://my-project:audit");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `ackMode` | `AUTO` | Consumer: `AUTO` acknowledges successful exchanges and nacks failed ones; `NONE` leaves it to the subscription's ack deadline. |
| `synchronousPull` | `false` | Consumer: `false` uses gRPC streaming pull (`Subscriber/StreamingPull`), `true` uses scheduled REST `pull` calls. |
| `concurrentConsumers` | `1` | Streaming pull: number of parallel streams. |
| `maxOutstandingMessages` | `1000` | Streaming pull flow control: unacknowledged messages. |
| `maxOutstandingBytes` | `104857600` | Streaming pull flow control: unacknowledged bytes. |
| `ackDeadlineSeconds` | `10` | Ack deadline of the stream (1 to 600); also the deadline of `createSubscription`. |
| `maxAckExtensionPeriod` | `3600` | Seconds a message's lease is extended while it is still being processed. |
| `maxMessagesPerPoll` | `1` | Synchronous pull: messages per `pull` call. |
| `pubsubEndpoint` | `pubsub.googleapis.com:443` | gRPC target for streaming pull (`host:port`; `https://` for TLS, a bare address is plaintext, as for the emulator). |
| `messageOrderingEnabled` | `false` | Enable message ordering on `createSubscription`. |
| `operation` | `publish` | Cameleer producer operations: `publish`, `createTopic`, `deleteTopic`, `createSubscription`, `getSubscription`, `deleteSubscription`. The header `CamelGooglePubsubOperation` overrides it. |
| `topic` | | Topic of `createSubscription`. |
| `deadLetterTopic` | | `createSubscription`: dead-letter topic. |
| `maxDeliveryAttempts` | | `createSubscription`: delivery attempts before dead-lettering (5 to 100). |
| `enableExactlyOnceDelivery` | `false` | `createSubscription`: exactly-once delivery. The synchronous pull consumer then extends leases while processing and checks each acknowledgement. |
| `delay`, `initialDelay`, ... | `500`, `1000` | Synchronous pull poll schedule (milliseconds) and the other scheduled-poll options. |

Credentials and endpoint options (`serviceAccountKey`, `GOOGLE_APPLICATION_CREDENTIALS`,
`useMetadataServer`, `authenticate=false` and `endpoint` for the Pub/Sub emulator, ...) are
the same for every Google component: see [credentials](google-storage.md#credentials).

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGooglePubsubAttributes` | in / out | Message attributes (a map). |
| `CamelGooglePubsubOrderingKey` | in / out | Ordering key. |
| `CamelGooglePubsubMessageId` | out | Id of the published message (a list for a list body) or of the consumed message. |
| `CamelGooglePubsubAckId` | out | Ack id of a consumed message. |
| `CamelGooglePubsubPublishTime` | out | Publish time of a consumed message. |
| `CamelGooglePubsubDeliveryAttempt` | out | Delivery attempt (subscriptions with a dead-letter policy). |
| `CamelGooglePubsubOperation` | in | Cameleer: operation to run. |
| `CamelGooglePubsubTopic` | in | Cameleer: topic of `createSubscription`. |

A list body is published as one message per element. The consumer's body is the message
data as bytes.

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" \
  -v "$HOME/keys/sa.json:/keys/sa.json:ro" -e GOOGLE_APPLICATION_CREDENTIALS=/keys/sa.json \
  -e GCP_PROJECT=my-project -e PUBSUB_TOPIC=events \
  cameleering/cameleer run /routes/webhook-to-pubsub.camel.yaml
```

## Common questions

### How do I use the Pub/Sub emulator?
Add `authenticate=false&endpoint=localhost:8085`. The REST calls and streaming pull both go to
that address (plaintext); `pubsubEndpoint` overrides the streaming pull target alone.

### How do I set message attributes?
Put a map in the `CamelGooglePubsubAttributes` header before the producer, for example with a
processor or a `setHeader` whose expression returns a map.

### How do I create a subscription with a dead-letter topic?
Send a message to `google-pubsub://project:my-sub?operation=createSubscription&topic=orders&deadLetterTopic=orders-dlq&maxDeliveryAttempts=5`.

### Why does my consumer get each message only once even when it fails?
It does not: with `ackMode=AUTO` a failed exchange is nacked, and Pub/Sub delivers it again.
Use the error handler or a dead-letter policy to stop endless retries.

## Related

- [Google Pub/Sub Lite](google-pubsub-lite.md), [Google BigQuery](google-bigquery.md)
- [Kafka](kafka.md) for self-hosted streaming
- [Apache Camel Google Pubsub](https://camel.apache.org/components/next/google-pubsub-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
