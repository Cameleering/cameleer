# AWS DynamoDB Streams component

> The `aws2-ddbstream` component consumes the change records (inserts, updates, deletes) of an Amazon DynamoDB table's stream.

**Scheme:** `aws2-ddbstream:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** yes · **Producer:** no ·
**Apache Camel:** [AWS DynamoDB Streams](https://camel.apache.org/components/next/aws2-ddbstream-component.html)

## When to use it

- Change data capture: react to every new or changed item without polling the table.
- Replicate a table into a search index, a cache or another region.
- Trigger notifications or workflows when an item is inserted.

## URI format

```text
aws2-ddbstream://tableName[?options]
```

The stream must be enabled on the table. Its ARN is read with `DescribeTable`.

## Example

Forward every new order item to a queue:

<!-- example: aws2-ddbstream-new-orders -->
```yaml
- route:
    id: new-orders-cdc
    from:
      uri: "aws2-ddbstream://orders?region={{env:AWS_REGION:us-east-1}}&streamIteratorType=FROM_START"
      steps:
        - filter:
            simple: "${header.CamelAwsDdbStreamEventName} == 'INSERT'"
            steps:
              - marshal:
                  json: {}
              - to: "aws2-sqs://new-orders?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true"
```

The body is the stream record as a map: `eventID`, `eventName` (`INSERT`, `MODIFY`, `REMOVE`),
`eventSource`, `awsRegion` and `dynamodb` with `Keys`, `NewImage` and `OldImage` already converted to
plain maps, plus `SequenceNumber`, `SizeBytes` and `StreamViewType`.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-ddbstream://orders?region={{env:AWS_REGION:us-east-1}}&streamIteratorType=FROM_START")
        .filter(simple("${header.CamelAwsDdbStreamEventName} == 'INSERT'"))
        .marshal("json")
        .to("aws2-sqs://new-orders?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `streamIteratorType` | `FROM_LATEST` | `FROM_LATEST`, `FROM_START` or `FROM_SEQUENCE_NUMBER`. |
| `sequenceNumberProvider` | | Start sequence number for `FROM_SEQUENCE_NUMBER`. |
| `maxResultsPerRequest` | `100` | Records per `GetRecords` call. |

The consumer also accepts the scheduled poll options (`delay` default 500 ms, ...). New child
shards after a split are picked up and read from their start; expired iterators are re-created
after the last sequence number seen.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsDdbStreamEventName` | out | Cameleer: `INSERT`, `MODIFY` or `REMOVE`. |
| `CamelAwsDdbStreamSequenceNumber` | out | Cameleer: sequence number of the record. |
| `CamelAwsDdbStreamShardId` | out | Cameleer: shard of the record. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/cdc.camel.yaml
```

## Common questions

### Why do I get no records?

The table needs a stream (`StreamSpecification`), and with the default `FROM_LATEST` only changes
made after the consumer started are delivered. Use `FROM_START` to read the retained records.

### Which images are in the record?

That depends on the table's stream view type: `KEYS_ONLY`, `NEW_IMAGE`, `OLD_IMAGE` or
`NEW_AND_OLD_IMAGES`. You can change it with `aws2-ddb` `UpdateTable` and the
`CamelAwsDdbStreamViewType` header.

## Related

- [AWS DynamoDB](aws2-ddb.md), [AWS Kinesis](aws2-kinesis.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS DynamoDB Streams](https://camel.apache.org/components/next/aws2-ddbstream-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
