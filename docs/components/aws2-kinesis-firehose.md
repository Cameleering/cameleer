# AWS Kinesis Firehose component

> The `aws2-kinesis-firehose` component sends records to Amazon Data Firehose (Kinesis Data Firehose) delivery streams, which deliver them to S3, Redshift, OpenSearch or HTTP endpoints.

**Scheme:** `aws2-kinesis-firehose:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Kinesis Firehose](https://camel.apache.org/components/next/aws2-kinesis-firehose-component.html)

## When to use it

- Ship application logs or metrics to S3 without managing batching and retries yourself.
- Load events into Redshift or OpenSearch through a delivery stream.
- Forward records consumed from Kafka, MQTT or SQS to a data lake.

## URI format

```text
aws2-kinesis-firehose://deliveryStreamName[?options]
```

## Example

Ship log lines from an SQS queue to a delivery stream:

<!-- example: aws2-kinesis-firehose-ship-logs -->
```yaml
- route:
    id: logs-to-firehose
    from:
      uri: "aws2-sqs://app-logs?region={{env:AWS_REGION:us-east-1}}&maxMessagesPerPoll=10"
      steps:
        - to: "aws2-kinesis-firehose://app-logs-to-s3?region={{env:AWS_REGION:us-east-1}}"
        - log: "record ${header.CamelAwsKinesisFirehoseRecordId}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://app-logs?region={{env:AWS_REGION:us-east-1}}&maxMessagesPerPoll=10")
        .to("aws2-kinesis-firehose://app-logs-to-s3?region={{env:AWS_REGION:us-east-1}}")
        .log("record ${header.CamelAwsKinesisFirehoseRecordId}");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `sendRecord` | `sendRecord`, `putRecordBatch` (default for an array body), `createDeliveryStream` (DirectPut; a map body is merged into the request), `describeDeliveryStream`, `deleteDeliveryStream`. |

`cborEnabled` is accepted for compatibility and has no effect.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsKinesisFirehoseOperation` | in | Overrides `operation`. |
| `CamelAwsKinesisFirehoseRecordId` | out | Id of the record of `sendRecord`. |
| `CamelAwsKinesisFirehoseDeliveryStreamName` | in | Overrides the stream name. |
| `CamelAwsKinesisFirehoseFailedPutCount` | out | Failed records of `putRecordBatch`. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/logs.camel.yaml
```

## Common questions

### How do I send a batch?

Send an array body: one record per element, with `PutRecordBatch`. The body becomes the list of
record ids or errors, and `CamelAwsKinesisFirehoseFailedPutCount` is set.

### Does Firehose add newlines between records?

No. Firehose concatenates records as sent; add a newline to each body if the destination expects
one record per line.

## Related

- [AWS Kinesis](aws2-kinesis.md), [AWS S3](aws2-s3.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Kinesis Firehose](https://camel.apache.org/components/next/aws2-kinesis-firehose-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
