# AWS Redshift Data component

> The `aws2-redshift-data` component runs SQL on Amazon Redshift clusters and Redshift Serverless workgroups through the Redshift Data API, without a database driver or connection pool.

**Scheme:** `aws2-redshift-data:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Redshift Data](https://camel.apache.org/components/next/aws2-redshift-data-component.html)

## When to use it

- Run scheduled reports or maintenance statements on a warehouse from a route.
- Load small batches of rows (`INSERT`) from events into Redshift.
- Query Redshift from serverless or edge environments where JDBC-style connections are impractical.

## URI format

```text
aws2-redshift-data://label?operation=...[&options]
```

## Example

Run a query, wait for it and fetch the rows:

<!-- example: aws2-redshift-data-sales-report -->
```yaml
- route:
    id: sales-report
    from:
      uri: "timer:sales-report?repeatCount=1"
      steps:
        - setBody:
            constant: "SELECT * FROM sales"
        - to: "aws2-redshift-data://warehouse?region={{env:AWS_REGION:us-east-1}}&database=dev&clusterIdentifier={{env:REDSHIFT_CLUSTER:analytics-cluster}}&dbUser=reporter&operation=executeStatement&waitTimeout=60000"
        - to: "aws2-redshift-data://warehouse?region={{env:AWS_REGION:us-east-1}}&operation=getStatementResult"
        - log: "${header.CamelAwsRedshiftDataTotalNumRows} rows: ${body}"
        - to: "log:sales"
```

`executeStatement` returns the statement id (also in `CamelAwsRedshiftDataStatementId`); with
`waitTimeout` it waits until the statement finished. `getStatementResult` then returns the rows as a
list of maps (column name to value), all pages.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:sales-report?repeatCount=1")
        .set_body(constant("SELECT * FROM sales"))
        .to("aws2-redshift-data://warehouse?region={{env:AWS_REGION:us-east-1}}&database=dev&clusterIdentifier={{env:REDSHIFT_CLUSTER:analytics-cluster}}&dbUser=reporter&operation=executeStatement&waitTimeout=60000")
        .to("aws2-redshift-data://warehouse?region={{env:AWS_REGION:us-east-1}}&operation=getStatementResult")
        .log("${header.CamelAwsRedshiftDataTotalNumRows} rows: ${body}")
        .to("log:sales");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | | Required: `executeStatement`, `batchExecuteStatement`, `describeStatement`, `getStatementResult`, `cancelStatement`, `listStatements`, `listDatabases`, `listSchemas`, `listTables`, `describeTable`. |
| `database` | | Database. |
| `clusterIdentifier` | | Provisioned cluster (or use `workgroupName`). |
| `workgroupName` | | Redshift Serverless workgroup. |
| `secretArn` | | Secrets Manager secret with the database credentials. |
| `dbUser` | | Database user (temporary credentials, provisioned clusters). |
| `waitTimeout` | `0` | Cameleer: wait (ms) for `executeStatement` to finish; a failed statement raises an error. |
| `delay` | `200` | Status check interval while waiting (ms). |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsRedshiftDataOperation` | in | Overrides `operation`. |
| `CamelAwsRedshiftDataDatabase`, `CamelAwsRedshiftDataClusterIdentifier`, `CamelAwsRedshiftDataWorkgroupName`, `CamelAwsRedshiftDataSecretArn`, `CamelAwsRedshiftDataDbUser` | in | Override the connection options. |
| `CamelAwsRedshiftDataSql` | in | SQL (otherwise the body). |
| `CamelAwsRedshiftDataSqlStatementList` | in | Statements of `batchExecuteStatement` (or a list body). |
| `CamelAwsRedshiftDataStatementName`, `CamelAwsRedshiftDataParameters` | in | Statement name and parameters (list of `{name, value}`). |
| `CamelAwsRedshiftDataStatementId` | in/out | Statement. |
| `CamelAwsRedshiftDataStatus` | out | Statement status. |
| `CamelAwsRedshiftDataSchema`, `CamelAwsRedshiftDataTable` | in | Patterns of `listSchemas`, `listTables`, `describeTable`. |
| `CamelAwsRedshiftDataTotalNumRows` | out | Rows of `getStatementResult`. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e REDSHIFT_CLUSTER \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/sales.camel.yaml
```

## Common questions

### How do I pass parameters safely?

Use `:name` placeholders in the SQL and set `CamelAwsRedshiftDataParameters` to a list of
`{name, value}` maps instead of building SQL strings.

## Related

- [AWS Athena](aws2-athena.md), [AWS Secrets Manager](aws-secrets-manager.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Redshift Data](https://camel.apache.org/components/next/aws2-redshift-data-component.html)
