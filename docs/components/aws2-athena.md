# AWS Athena component

> The `aws2-athena` component runs SQL queries on data in S3 with Amazon Athena, waits for them to finish and returns the result rows.

**Scheme:** `aws2-athena:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Athena](https://camel.apache.org/components/next/aws2-athena-component.html)

## When to use it

- Produce scheduled reports from a data lake (orders per region, error counts).
- Feed query results into another system (e-mail, a database, an API).
- Let an AI agent answer analytical questions with a fixed, reviewed set of queries.

## URI format

```text
aws2-athena://label[?options]
```

## Example

Run a query, wait for it, and fetch all result rows:

<!-- example: aws2-athena-daily-report -->
```yaml
- route:
    id: orders-per-region
    from:
      uri: "timer:athena-report?repeatCount=1"
      steps:
        - setBody:
            constant: "SELECT region, count(*) AS orders FROM orders GROUP BY region"
        - to: "aws2-athena://report?region={{env:AWS_REGION:us-east-1}}&database=analytics&outputLocation=s3://{{env:ATHENA_RESULTS_BUCKET:athena-results}}/&waitTimeout=60000&operation=startQueryExecution"
        - to: "aws2-athena://report?region={{env:AWS_REGION:us-east-1}}&operation=getQueryResults"
        - log: "rows: ${body}"
        - to: "log:report"
```

`startQueryExecution` takes the SQL from the body, polls the query until it ends (up to
`waitTimeout` ms) and sets `CamelAwsAthenaQueryExecutionId`, which `getQueryResults` then uses. With
the default `outputType=StreamList` the body is a list of rows (each a list of strings), the header
row first.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:athena-report?repeatCount=1")
        .set_body(constant("SELECT region, count(*) AS orders FROM orders GROUP BY region"))
        .to("aws2-athena://report?region={{env:AWS_REGION:us-east-1}}&database=analytics&outputLocation=s3://{{env:ATHENA_RESULTS_BUCKET:athena-results}}/&waitTimeout=60000&operation=startQueryExecution")
        .to("aws2-athena://report?region={{env:AWS_REGION:us-east-1}}&operation=getQueryResults")
        .log("rows: ${body}")
        .to("log:report");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `startQueryExecution` | `startQueryExecution`, `getQueryExecution`, `getQueryResults`, `listQueryExecutions`. |
| `queryString` | | SQL (otherwise `CamelAwsAthenaQueryString` or the body). |
| `database` | | Database of the query. |
| `outputLocation` | | S3 location of the results. |
| `workGroup` | | Work group. |
| `encryptionOption`, `kmsKey` | | Encryption of the results. |
| `clientRequestToken` | | Idempotency token. |
| `waitTimeout` | `0` | Wait (ms) for the query to finish; 0 returns at once. |
| `initialDelay`, `delay` | `1000`, `2000` | First and next status checks while waiting (ms). |
| `maxAttempts` | `1` | Attempts for failed queries. |
| `retry` | `never` | When to retry: `never`, `always`, `retryable`, `exhausted`, `generic` (comma separated). |
| `outputType` | `StreamList` | `getQueryResults`: `StreamList` (all pages, list of rows), `SelectList` (one page, raw response) or `S3Pointer` (the S3 location). |
| `maxResults`, `nextToken` | | Paging of `SelectList` and `listQueryExecutions`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsAthenaOperation` | in | Overrides `operation`. |
| `CamelAwsAthenaQueryString`, `CamelAwsAthenaDatabase`, `CamelAwsAthenaWorkGroup`, `CamelAwsAthenaOutputLocation`, `CamelAwsAthenaEncryptionOption`, `CamelAwsAthenaKmsKey`, `CamelAwsAthenaClientRequestToken` | in | Override the options. |
| `CamelAwsAthenaWaitTimeout`, `CamelAwsAthenaInitialDelay`, `CamelAwsAthenaDelay`, `CamelAwsAthenaMaxAttempts`, `CamelAwsAthenaRetry` | in | Override the waiting options. |
| `CamelAwsAthenaQueryExecutionId` | in/out | Query execution. |
| `CamelAwsAthenaQueryExecutionState` | out | `SUCCEEDED`, `FAILED`, `CANCELLED`, ... |
| `CamelAwsAthenaOutputLocation` | out | Where the results are. |
| `CamelAwsAthenaStartQueryExecutionAttempts`, `CamelAwsAthenaStartQueryExecutionElapsedMillis` | out | Attempts and time spent. |
| `CamelAwsAthenaOutputType`, `CamelAwsAthenaMaxResults`, `CamelAwsAthenaNextToken` | in/out | Result paging. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e ATHENA_RESULTS_BUCKET \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/report.camel.yaml
```

## Common questions

### Why is the first row the column names?

Athena returns the header as the first row of the first page. To skip it, `split` on `${body}`
and `filter` on `${exchangeProperty.CamelSplitIndex} > 0`.

### How do I handle large results?

Use `outputType=S3Pointer` and read the CSV result file from S3 with [aws2-s3](aws2-s3.md), or
`SelectList` with `maxResults` to page through the results.

## Related

- [AWS S3](aws2-s3.md), [AWS Redshift Data](aws2-redshift-data.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Athena](https://camel.apache.org/components/next/aws2-athena-component.html)
