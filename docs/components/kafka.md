# Kafka component

> The `kafka` component sends messages to and consumes messages from Apache Kafka topics, with consumer groups, keys, partitions, TLS and SASL.

**Scheme:** `kafka:` · **Crate:** `cameleer-kafka-mqtt` · **Cargo feature:** [`kafka-mqtt`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Kafka](https://camel.apache.org/components/next/kafka-component.html)

## When to use it

- Consume events from a topic and process them (billing, enrichment, indexing) with a consumer group.
- Publish events from an API, a database poll or an MQTT bridge to a topic.
- Use Kafka as an AI agent's event bus: agents publish their actions and observations keyed by agent id,
  and other routes or agents react to them.
- Store and forward from the edge: buffer readings locally and push them to a central Kafka cluster.

The Kafka wire protocol client is built into Cameleer (no librdkafka, no JVM). It works with Apache
Kafka, Redpanda and Kafka-compatible cloud services.

## URI format

```text
kafka:topic[?options]
```

## Example

<!-- example: kafka-consume-and-log -->
```yaml
- route:
    id: orders-consumer
    from:
      uri: kafka:orders
      parameters:
        brokers: "{{env:KAFKA_BROKERS:localhost:9092}}"
        groupId: billing
        autoOffsetReset: earliest
      steps:
        - log: "partition ${header[kafka.PARTITION]} offset ${header[kafka.OFFSET]}: ${body}"
        - to: log:orders
```

The route joins the consumer group `billing`, starts at the oldest record when the group has no committed
offset, and commits offsets automatically.

### Agent event bus

<!-- example: kafka-agent-event-bus -->
```yaml
- route:
    id: publish-agent-event
    from:
      uri: direct:agent-event
      steps:
        - setHeader:
            name: kafka.KEY
            simple: "${header.agentId}"
        - to: kafka:agent-events?brokers={{env:KAFKA_BROKERS:localhost:9092}}
```

Events of one agent share a key, so they land on the same partition and stay in order.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("kafka:orders?brokers=localhost:9092&groupId=billing&autoOffsetReset=earliest")
            .route_id("orders-consumer")
            .log("partition ${header[kafka.PARTITION]} offset ${header[kafka.OFFSET]}: ${body}")
            .to("log:orders");
        b.from("direct:agent-event")
            .route_id("publish-agent-event")
            .set_header("kafka.KEY", simple("${header.agentId}"))
            .to("kafka:agent-events?brokers=localhost:9092");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `brokers` | `localhost:9092` | Bootstrap servers, comma separated. |
| `clientId` | generated | Client id sent to the brokers. |
| `groupId` | | Consumer group. Without it, each consumer thread reads all partitions and commits nothing. |
| `consumersCount` | `1` | Consumer threads (group members). |
| `autoOffsetReset` | `latest` | Where to start without a committed offset: `earliest`, `latest` or `none`. |
| `seekTo` | | `beginning` or `end`: seek on start, ignoring committed offsets. |
| `maxPollRecords` | `500` | Maximum records per poll. |
| `autoCommitEnable` | `true` | Commit offsets automatically. |
| `autoCommitIntervalMs` | `5000` | Auto-commit interval. |
| `autoCommitOnStop` | `sync` | Commit on stop: `sync`, `async` or `none`. |
| `allowManualCommit` | `false` | Put a manual committer in the `CamelKafkaManualCommit` header. |
| `breakOnFirstError` | `false` | Stop the batch at the first failed record and retry from it. |
| `topicIsPattern` | `false` | The topic is a regular expression. |
| `pollTimeoutMs` | `5000` | Poll timeout. |
| `fetchWaitMaxMs` | `500` | Broker wait for data. |
| `fetchMaxBytes`, `maxPartitionFetchBytes` | | Fetch size limits. |
| `heartbeatIntervalMs` | `3000` | Group heartbeat interval. |
| `sessionTimeoutMs` | `45000` | Group session timeout. |
| `maxPollIntervalMs` | `300000` | Maximum time between polls. |
| `requestTimeoutMs` | `30000` | Request timeout. |
| `keyDeserializer`, `valueDeserializer` | string | A `ByteArrayDeserializer` (or `BytesDeserializer`) gives byte bodies. |
| `key` | | Producer: record key. |
| `partitionKey` | | Producer: target partition. |
| `requestRequiredAcks` | `all` | Producer acks: `all` / `-1`, `1` or `0`. |
| `maxInFlightRequest` | `5` | Producer: produce requests in flight at once. Concurrent sends are batched into one request per partition, and up to this many requests run in parallel on their own connections (opened only under contention), so a send waits behind at most one request. |
| `compressionCodec` | `none` | Producer batch compression: `none`, `gzip`, `snappy`, `lz4` (`zstd` is not supported). |
| `recordMetadata` | `true` | Producer: set `kafka.RECORD_META` after the send. |
| `securityProtocol` | `PLAINTEXT` | `PLAINTEXT`, `SSL`, `SASL_PLAINTEXT` or `SASL_SSL`. |
| `saslMechanism` | `PLAIN` | `PLAIN`, `SCRAM-SHA-256` or `SCRAM-SHA-512` (and `AWS_MSK_IAM` with `cameleer-cloud`). |
| `saslJaasConfig` | | Credentials: `... required username="u" password="p";`. |
| `sslTruststoreLocation` | | Trusted CA certificates: a PEM file, a PKCS12 file or a Java keystore (JKS). |
| `sslTruststorePassword`, `sslTruststoreType` | | Password and type (`PEM`, `PKCS12`, `JKS`) of the trust store; the type is detected when absent. |
| `sslKeystoreLocation` | | Client certificate for mutual TLS: a PKCS12 file, a Java keystore (JKS) or a PEM file with the certificate chain and key. |
| `sslKeystorePassword`, `sslKeyPassword`, `sslKeystoreType` | | Password of the key store, of the key inside it, and its type (`PEM`, `PKCS12`, `JKS`); detected when absent. |
| `sslEndpointIdentificationAlgorithm` (also `sslEndpointAlgorithm`) | `https` | Empty value disables host name verification. |
| `sslProtocol` (also `sslEnabledProtocols`) | | Restrict the TLS version. |
| `sslContextParameters`, `useGlobalSslContextParameters` | | TLS settings from a bean or the global `camel.ssl.*` parameters (which apply only with `camel.ssl.enabled=true`). |

GSSAPI (Kerberos) and OAUTHBEARER are not supported. Other
Camel Kafka tuning options are accepted and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `kafka.KEY` | in (producer), out (consumer) | Record key. |
| `kafka.PARTITION_KEY` | in | Producer: target partition. |
| `kafka.OVERRIDE_TOPIC` | in | Producer: send to this topic instead (the header is removed). |
| `kafka.OVERRIDE_TIMESTAMP` | in | Producer: record timestamp (epoch ms). |
| `kafka.TOPIC`, `kafka.PARTITION`, `kafka.OFFSET`, `kafka.TIMESTAMP` | out | Where the record came from (consumer) or went (producer: partition and offset). |
| `kafka.LAST_POLL_RECORD`, `kafka.LAST_RECORD_BEFORE_COMMIT` | out | Consumer: last record of the poll / before the commit. |
| `kafka.RECORD_META` | out | Producer: list of `{topic, partition, offset, timestamp}`. |
| `CamelKafkaManualCommit` | out | Consumer with `allowManualCommit=true`: commits the record's offset. |

Other headers become Kafka record headers (strings as UTF-8, integers as 8-byte longs), except `Camel*`,
`org.apache.camel*` and `kafka.*`. Received record headers become message headers (as bytes).

## Run it with Docker

```sh
docker run --rm -e KAFKA_BROKERS=broker:9092 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml
```

Use `--network host` or the broker's network name so the container can reach Kafka.

## Common questions

### How do I read a topic from the beginning?

Set `autoOffsetReset=earliest` with a new `groupId`, or `seekTo=beginning` to ignore committed offsets.

### How do I send one record per list element?

Send a list body: an array body sends one record per element.

### How do I connect to Confluent Cloud or another SASL_SSL cluster?

`securityProtocol=SASL_SSL&saslMechanism=PLAIN&saslJaasConfig=RAW(org.apache.kafka.common.security.plain.PlainLoginModule required username="{{env:KAFKA_KEY}}" password="{{env:KAFKA_SECRET}}";)`.

### How do I connect with a client certificate (mutual TLS)?

Use `securityProtocol=SSL` with a trust store for the broker's CA and a key store holding the
client certificate and key:
`securityProtocol=SSL&sslTruststoreLocation=/certs/ca.pem&sslKeystoreLocation=/certs/client.p12&sslKeystorePassword={{env:KAFKA_KEYSTORE_PASSWORD}}`.
PKCS12, Java keystores (JKS) and PEM files work for both stores; set `sslKeystoreType` or
`sslTruststoreType` (`PKCS12`, `JKS`, `PEM`) if the type is not detected from the file.

### How do I commit offsets only after my processing succeeded?

Set `autoCommitEnable=false&allowManualCommit=true` and call the committer in `CamelKafkaManualCommit`
from a processor, or keep auto-commit with `breakOnFirstError=true`.

## Related

- [paho](paho.md) (MQTT to Kafka bridge), [seda](seda.md), [amqp](amqp.md)
- [Apache Camel Kafka component](https://camel.apache.org/components/next/kafka-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
