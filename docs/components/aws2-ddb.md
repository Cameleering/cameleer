# AWS DynamoDB component

> The `aws2-ddb` component writes, reads, queries, scans and deletes items in Amazon DynamoDB tables, using plain maps instead of DynamoDB attribute values.

**Scheme:** `aws2-ddb:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS DynamoDB](https://camel.apache.org/components/next/aws2-ddb-component.html)

## When to use it

- Store orders, device state or session data from a route.
- Look up an item by key to enrich a message.
- Keep the memory of an AI agent (conversation state keyed by session id) in a serverless table.

For change data capture on a table, use [aws2-ddbstream](aws2-ddbstream.md).

## URI format

```text
aws2-ddb://tableName[?options]
```

## Example

Save each JSON order from a queue as an item:

<!-- example: aws2-ddb-save-order -->
```yaml
- route:
    id: save-orders
    from:
      uri: "aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: CamelAwsDdbItem
            simple: "${body}"
        - to: "aws2-ddb://orders?region={{env:AWS_REGION:us-east-1}}&operation=PutItem"
        - log: "saved order ${header.CamelAwsDdbItem[orderId]}"
```

Items, keys and conditions are plain maps. Strings become `S`, numbers `N`, booleans `BOOL`, null
`NULL`, bytes `B`, lists `L` and maps `M`; results are converted back the same way.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}")
        .unmarshal("json")
        .set_header("CamelAwsDdbItem", simple("${body}"))
        .to("aws2-ddb://orders?region={{env:AWS_REGION:us-east-1}}&operation=PutItem")
        .log("saved order ${header.CamelAwsDdbItem[orderId]}");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `PutItem` | `PutItem`, `GetItem`, `UpdateItem`, `DeleteItem`, `Query`, `Scan`, `BatchGetItems`, `DescribeTable`, `UpdateTable`, `DeleteTable`. The `CamelAwsDdbOperation` header overrides it. |
| `consistentRead` | `false` | Strongly consistent reads. |
| `enabledInitialDescribeTable` | `true` | Describe the table when the endpoint starts. |
| `keyAttributeName` | | When set and the table is missing at start, create it with this hash key. |
| `keyAttributeType` | `S` | Type of the created hash key. |
| `readCapacity`, `writeCapacity` | `1` | Provisioned throughput of the created table. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsDdbItem` | in | Item of `PutItem` (map). |
| `CamelAwsDdbKey` | in | Key of `GetItem`, `UpdateItem`, `DeleteItem` (map). |
| `CamelAwsDdbUpdateValues` | in | `UpdateItem`: attribute to new value (null deletes). |
| `CamelAwsDdbUpdateExpression`, `CamelAwsDdbConditionExpression`, `CamelAwsDdbKeyConditionExpression`, `CamelAwsDdbFilterExpression`, `CamelAwsDdbProjectionExpression` | in | Expressions. |
| `CamelAwsDdbExpressionAttributeNames`, `CamelAwsDdbExpressionAttributeValues` | in | `#name` and `:value` placeholders (plain values). `CamelAwsDdbFilterExpressionAttributeNames` / `...Values` are accepted too. |
| `CamelAwsDdbKeyConditions`, `CamelAwsDdbScanFilter` | in | Legacy `Query` / `Scan` conditions. |
| `CamelAwsDdbReturnValues`, `CamelAwsDdbConsistentRead`, `CamelAwsDdbAttributeNames`, `CamelAwsDdbIndexName`, `CamelAwsDdbLimit`, `CamelAwsDdbStartKey`, `CamelAwsDdbScanIndexForward` | in | Request settings. |
| `CamelAwsDdbBatchItems` | in | `BatchGetItems`: table to `{Keys, ConsistentRead, ProjectionExpression}`. |
| `CamelAwsDdbReadCapacity`, `CamelAwsDdbWriteCapacity`, `CamelAwsDdbStreamViewType` | in | `UpdateTable` (stream view type is a Cameleer extension). |
| `CamelAwsDdbTableName`, `CamelAwsDdbOperation` | in | Override the table and the operation. |
| `CamelAwsDdbAttributes`, `CamelAwsDdbItems`, `CamelAwsDdbCount`, `CamelAwsDdbScannedCount`, `CamelAwsDdbLastEvaluatedKey` | out | Results. |
| `CamelAwsDdbBatchResponse`, `CamelAwsDdbUnprocessedKeys` | out | `BatchGetItems` results. |
| `CamelAwsDdbTableStatus`, `CamelAwsDdbItemCount`, `CamelAwsDdbKeySchema`, `CamelAwsDdbProvisionedThroughput`, `CamelAwsDdbStreamArn` | out | Table information. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml
```

## Common questions

### How do I read an item by key?

Set `CamelAwsDdbKey` to a map such as `{"orderId": "o-1001"}` and use `operation=GetItem`; the item
is returned in the `CamelAwsDdbAttributes` header.

### How do I increment a counter?

Use `UpdateItem` with `CamelAwsDdbUpdateExpression` = `SET hits = hits + :one` and
`CamelAwsDdbExpressionAttributeValues` = `{":one": 1}`.

### Does it work with DynamoDB Local?

Yes: `uriEndpointOverride=http://localhost:8000&overrideEndpoint=true`.

## Related

- [AWS DynamoDB Streams](aws2-ddbstream.md), [AWS SQS](aws2-sqs.md)
- [Azure Cosmos DB](azure-cosmosdb.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS DynamoDB](https://camel.apache.org/components/next/aws2-ddb-component.html)
