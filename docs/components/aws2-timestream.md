# AWS Timestream component

> The `aws2-timestream` component writes time series records to Amazon Timestream and runs queries on them.

**Scheme:** `aws2-timestream:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Timestream](https://camel.apache.org/components/next/aws2-timestream-component.html)

## When to use it

- Store sensor readings from IoT gateways and edge devices.
- Record operational metrics with dimensions (device, site, region).
- Query recent values for dashboards, alerts or an AI agent that answers questions about devices.

## URI format

```text
aws2-timestream://write:label[?options]
aws2-timestream://query:label[?options]
```

`write:` endpoints use Timestream Write, `query:` endpoints Timestream Query. Without an endpoint
override the client discovers the service endpoint (`DescribeEndpoints`) and caches it.

## Example

Write each sensor reading of a queue as a record. The messages are already Timestream records:

<!-- example: aws2-timestream-write-readings -->
```yaml
- route:
    id: readings-to-timestream
    from:
      uri: "aws2-sqs://sensor-readings?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: CamelAwsTimestreamDatabaseName
            constant: iot
        - setHeader:
            name: CamelAwsTimestreamTableName
            constant: readings
        - to: "aws2-timestream://write:sensors?region={{env:AWS_REGION:us-east-1}}&operation=writeRecords"
        - log: "records ingested: ${body}"
```

A reading looks like
`{"Dimensions":[{"Name":"device","Value":"edge-7"}],"MeasureName":"temperature","MeasureValue":"21.5","MeasureValueType":"DOUBLE","Time":"1730800000000"}`.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://sensor-readings?region={{env:AWS_REGION:us-east-1}}")
        .unmarshal("json")
        .set_header("CamelAwsTimestreamDatabaseName", constant("iot"))
        .set_header("CamelAwsTimestreamTableName", constant("readings"))
        .to("aws2-timestream://write:sensors?region={{env:AWS_REGION:us-east-1}}&operation=writeRecords")
        .log("records ingested: ${body}");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `query` on `query:` endpoints | Write: `describeEndpoints`, `createDatabase`, `deleteDatabase`, `describeDatabase`, `listDatabases`, `createTable`, `deleteTable`, `describeTable`, `listTables`, `writeRecords`. Query: `query`, `cancelQuery`, `describeEndpoints`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsTimestreamOperation` | in | Overrides `operation`. |
| `CamelAwsTimestreamDatabaseName`, `CamelAwsTimestreamTableName` | in | Database and table. |
| `CamelAwsTimestreamRecord`, `CamelAwsTimestreamRecordList` | in | Records of `writeRecords` (otherwise the body: a record map or a list). |
| `CamelAwsTimestreamCommonAttributes` | in | Attributes shared by all records. |
| `CamelAwsTimestreamMemoryStoreRetentionPeriodInHours`, `CamelAwsTimestreamMagneticStoreRetentionPeriodInDays` | in | Retention of `createTable`. |
| `CamelAwsTimestreamQueryString` | in | SQL of `query` (otherwise the body). |
| `CamelAwsTimestreamQueryId` | in/out | Query (set by `query`, input of `cancelQuery`). |

`query` returns all pages as a list of rows (maps of column name to value).

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/readings.camel.yaml
```

## Common questions

### How do I query the last hour?

Send `SELECT * FROM "iot"."readings" WHERE time > ago(1h)` to a `query:` endpoint
(`aws2-timestream://query:sensors`); the body becomes the list of rows.

## Related

- [AWS Kinesis](aws2-kinesis.md), [AWS CloudWatch](aws2-cw.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Timestream](https://camel.apache.org/components/next/aws2-timestream-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
