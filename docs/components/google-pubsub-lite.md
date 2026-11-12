# Google Pub/Sub Lite component

> The `google-pubsub-lite` component publishes messages to Google Cloud Pub/Sub Lite topics and consumes Pub/Sub Lite subscriptions over gRPC, partition by partition.

**Scheme:** `google-pubsub-lite:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Google PubSub Lite](https://camel.apache.org/components/next/google-pubsub-lite-component.html)

**Note:** Google deprecated Pub/Sub Lite in 2024 and announced its shutdown for March 2026.
The component is kept for parity with Apache Camel and for existing emulator-based setups. For
new work use [Google Pub/Sub](google-pubsub.md) or [Kafka](kafka.md).

## When to use it

- Keep running an existing Pub/Sub Lite integration while migrating away from it.
- Partitioned, ordered streams: messages with the same ordering key go to the same partition.
- Test partition and cursor handling against a Pub/Sub Lite emulator.

## URI format

```text
google-pubsub-lite:projectId:location:destinationName[?options]
```

`location` is a zone (`us-central1-a`) or region. The destination is a topic for the producer
and a subscription for the consumer. The gRPC target is `{region}-pubsublite.googleapis.com:443`
unless `pubsubEndpoint` (or `endpoint`) points elsewhere.

## Example

Publish one message per second, keyed by device so each device's messages stay in order:

<!-- example: google-pubsub-lite-publish -->
```yaml
- route:
    id: telemetry-to-pubsub-lite
    from:
      uri: "timer:telemetry?period=1000"
      steps:
        - setHeader:
            name: CamelGooglePubsubLiteOrderingKey
            constant: "{{env:DEVICE_ID:device-1}}"
        - setBody:
            simple: "reading ${exchangeProperty.CamelTimerCounter}"
        - to: "google-pubsub-lite:{{env:GCP_PROJECT:my-project}}:{{env:LITE_LOCATION:us-central1-a}}:{{env:LITE_TOPIC:telemetry}}"
        - log: "published as ${header.CamelGooglePubsubLiteMessageId}"
```

Consume a subscription; each processed message commits the partition cursor:

<!-- example: google-pubsub-lite-consume -->
```yaml
- route:
    id: pubsub-lite-to-files
    from:
      uri: "google-pubsub-lite:{{env:GCP_PROJECT:my-project}}:{{env:LITE_LOCATION:us-central1-a}}:{{env:LITE_SUBSCRIPTION:telemetry-sub}}?maxMessagesPerPoll=50"
      steps:
        - setHeader:
            name: CamelFileName
            simple: "p${header.CamelGooglePubsubLitePartition}-${header.CamelGooglePubsubLiteOffset}.txt"
        - to: "file:{{env:OUT_DIR:out}}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("google-pubsub-lite:my-project:us-central1-a:telemetry-sub")
        .log("partition ${header.CamelGooglePubsubLitePartition}: ${body}");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `ackMode` | `AUTO` | Consumer: `AUTO` commits the cursor after each successfully processed message; `NONE` never commits. |
| `maxMessagesPerPoll` | `100` | Consumer flow control: messages per partition per poll. |
| `perPartitionBytes` | `10485760` | Consumer flow control bytes per partition; also the per-partition storage of `createTopic`. |
| `streamIdleTimeout` | `300` | Consumer: milliseconds without a message after which a partition's stream ends for this poll. |
| `pubsubEndpoint` | `{region}-pubsublite.googleapis.com:443` | gRPC target (`host:port`; plaintext for emulators). |
| `operation` | publish | Cameleer admin operations: `createTopic`, `createSubscription`, `deleteTopic`, `deleteSubscription`, `getTopicPartitions` (body = partition count). Header `CamelGooglePubsubLiteOperation`. |
| `partitionCount` | `1` | `createTopic`: number of partitions. |
| `topic` | | `createSubscription`: the topic to subscribe to. |
| `delay`, `initialDelay`, ... | `500`, `1000` | Consumer poll schedule (milliseconds) and the other scheduled-poll options. |

`concurrentConsumers`, `maxAckExtensionPeriod`, `publisherCacheSize`, `publisherCacheTimeout`,
`publisherTerminationTimeout`, `serializer` and `loggerId` are accepted for Camel
compatibility and ignored. Credentials are shared by all Google components: see
[credentials](google-storage.md#credentials).

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGooglePubsubLiteOrderingKey` | in / out | Ordering key; messages with a key go to partition `hash(key) % partitions`, others round robin. |
| `CamelGooglePubsubLiteAttributes` | in / out | Attributes (a map; consumed values are strings). |
| `CamelGooglePubsubLiteEventTime` | in | Event time (epoch milliseconds). |
| `CamelGooglePubsubLiteMessageId` | out | `partition:offset` of the published or consumed message (a list for a list body). |
| `CamelGooglePubsubLitePublishTime` | out | Publish time of a consumed message. |
| `CamelGooglePubsubLitePartition` | out | Partition. |
| `CamelGooglePubsubLiteOffset` | out | Offset in the partition. |
| `CamelGooglePubsubLiteOperation` | in | Cameleer: admin operation. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" \
  -v "$HOME/keys/sa.json:/keys/sa.json:ro" -e GOOGLE_APPLICATION_CREDENTIALS=/keys/sa.json \
  -e GCP_PROJECT=my-project -e LITE_TOPIC=telemetry \
  cameleering/cameleer run /routes/telemetry-to-pubsub-lite.camel.yaml
```

## Common questions

### How do I create a Pub/Sub Lite topic with several partitions?
Send a message to `google-pubsub-lite:project:zone:telemetry?operation=createTopic&partitionCount=3`.

### What happens when processing fails?
The partition stops for that poll without committing, so the failed message (and the ones after
it) are delivered again on the next poll.

### Does the consumer keep a stream open?
No. Each poll opens one `Subscribe` stream per partition from the committed cursor and reads until
the stream is idle. This differs from Camel, which keeps long-lived streams.

## Related

- [Google Pub/Sub](google-pubsub.md), [Kafka](kafka.md)
- [Apache Camel Google PubSub Lite](https://camel.apache.org/components/next/google-pubsub-lite-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
