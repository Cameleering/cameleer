# AWS Kinesis component

> The `aws2-kinesis` component puts records into and consumes records from Amazon Kinesis Data Streams.

**Scheme:** `aws2-kinesis:` · **Crate:** `cameleer-cloud` · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [AWS Kinesis](https://camel.apache.org/components/next/aws2-kinesis-component.html)

## When to use it

- Stream clickstream, telemetry or log records from devices and gateways into AWS.
- Consume a stream shard by shard and forward records to storage, Firehose or analytics.
- Replay a stream from the beginning (`TRIM_HORIZON`) or resume after a stored sequence number.

## URI format

```text
aws2-kinesis://streamName[?options]
```

## Example

Put a click event. The partition key header is required:

<!-- example: aws2-kinesis-put-click -->
```yaml
- route:
    id: put-click
    from:
      uri: "timer:click?repeatCount=1"
      steps:
        - setBody:
            constant: '{"user":"user-42","page":"/pricing"}'
        - setHeader:
            name: CamelAwsKinesisPartitionKey
            constant: user-42
        - to: "aws2-kinesis://clickstream?region={{env:AWS_REGION:us-east-1}}"
        - log: "stored in ${header.CamelAwsKinesisShardId} at ${header.CamelAwsKinesisSequenceNumber}"
```

Consume the stream from its oldest record:

<!-- example: aws2-kinesis-consume-clickstream -->
```yaml
- route:
    id: read-clickstream
    from:
      uri: "aws2-kinesis://clickstream?region={{env:AWS_REGION:us-east-1}}&iteratorType=TRIM_HORIZON&maxResultsPerRequest=100"
      steps:
        - convertBodyTo: String
        - log: "click from ${header.CamelAwsKinesisPartitionKey}: ${body}"
        - to: "log:clicks"
```

Record data arrives as bytes; `convertBodyTo: String` turns it into text.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-kinesis://clickstream?region={{env:AWS_REGION:us-east-1}}&iteratorType=TRIM_HORIZON&maxResultsPerRequest=100")
        .convert_body_to("String")
        .log("click from ${header.CamelAwsKinesisPartitionKey}: ${body}")
        .to("log:clicks");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `putRecord` | Cameleer: producer operation `putRecord`, `putRecords`, `createStream`, `describeStream`, `listShards`, `deleteStream`. An array body is sent with `PutRecords`. |
| `shardCount` | `1` | Shards of `createStream`. |
| `iteratorType` | `TRIM_HORIZON` | Consumer start: `TRIM_HORIZON`, `LATEST`, `AT_SEQUENCE_NUMBER`, `AFTER_SEQUENCE_NUMBER`, `AT_TIMESTAMP`. |
| `sequenceNumber` | | Start sequence number for the `*_SEQUENCE_NUMBER` iterator types. |
| `messageTimestamp` | | Start time (epoch millis) for `AT_TIMESTAMP`. |
| `maxResultsPerRequest` | `1` | Records read per shard and poll. |
| `shardId` | | Consume only this shard. |
| `shardClosed` | `ignore` | What to do at the end of a closed shard: `ignore` (logged), `silent` or `fail`. |
| `resumeStrategy` | | `#bean` of a `KinesisResumeStrategy` that stores and returns the last processed sequence number per shard. |
| `useKclConsumers` | `false` | KCL-style lease table consumers (`applicationName`, `workerIdentifier`, `failoverTimeMillis`); `kclDisableCloudwatchMetricsExport` is accepted, no metrics are published. |
| `enhancedFanOut` | `false` | Cameleer: enhanced fan-out (`SubscribeToShard` over HTTP/2) with consumer `consumerName` (default `camel-{stream}`). |
| `cborEnabled` | `false` | Use the CBOR flavour of the protocol (JSON otherwise). |

The consumer also accepts the scheduled poll options (`delay` default 500 ms, ...).

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsKinesisPartitionKey` | in/out | Partition key (required when putting a record). |
| `CamelAwsKinesisSequenceNumber` | in/out | Sequence number of the record (input: `SequenceNumberForOrdering`). |
| `CamelAwsKinesisShardId` | out | Shard of the record. |
| `CamelAwsKinesisApproximateArrivalTimestamp` | out | Arrival time. |
| `CamelMessageTimestamp` | out | Arrival time as the message timestamp. |
| `CamelAwsKinesisOperation` | in | Overrides `operation`. |
| `CamelAwsKinesisFailedRecordCount` | out | Failed records of `PutRecords`. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/clickstream.camel.yaml
```

## Common questions

### How do I put many records in one call?

Send an array body: each element is a record (or an object `{data, partitionKey}`); the body then
becomes the list of results and `CamelAwsKinesisFailedRecordCount` is set.

### How do I resume after a restart?

Bind a `KinesisResumeStrategy` (for example backed by a database) and set `resumeStrategy=#name`,
or use `useKclConsumers=true` to keep leases and checkpoints in a DynamoDB lease table.

### Does it support resharding?

Yes. Shards are listed on every poll, so child shards are picked up, and closed shards are handled
according to `shardClosed`.

## Related

- [AWS Kinesis Firehose](aws2-kinesis-firehose.md), [AWS DynamoDB Streams](aws2-ddbstream.md)
- [Azure Event Hubs](azure-eventhubs.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Kinesis](https://camel.apache.org/components/next/aws2-kinesis-component.html)
