# Google BigQuery component

> The `google-bigquery` component streams rows into Google BigQuery tables, runs load and extract jobs between BigQuery and Cloud Storage, and manages tables and datasets.

**Scheme:** `google-bigquery:` · **Crate:** `cameleer-cloud` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Google BigQuery](https://camel.apache.org/components/next/google-bigquery-component.html)

For SQL queries use [`google-bigquery-sql`](google-bigquery-sql.md).

## When to use it

- Stream events from Pub/Sub, Kafka or HTTP into a BigQuery table for analytics.
- Load CSV or JSON files from Cloud Storage into a table on a schedule.
- Export a table to Cloud Storage for another system.
- Log AI agent actions or LLM results (prompt, answer, tokens) to a table for evaluation.

## URI format

```text
google-bigquery://projectId:datasetId[:tableId][?options]
```

Without a table in the URI, the `CamelGoogleBigQueryTableId` header names it.

## Example

Consume JSON events from a Pub/Sub subscription and insert each one as a row. The `id` field is
the insert id, so a redelivered message is not inserted twice:

<!-- example: google-bigquery-stream-from-pubsub -->
```yaml
- route:
    id: pubsub-to-bigquery
    from:
      uri: "google-pubsub://{{env:GCP_PROJECT:my-project}}:{{env:PUBSUB_SUBSCRIPTION:events-sub}}"
      steps:
        - unmarshal:
            json: {}
        - to: "google-bigquery://{{env:GCP_PROJECT:my-project}}:{{env:BQ_DATASET:analytics}}:{{env:BQ_TABLE:events}}?useAsInsertId=id"
```

Load newline-delimited JSON files from Cloud Storage into a table once a day (a load job,
polled until it finishes):

<!-- example: google-bigquery-load-from-gcs -->
```yaml
- route:
    id: nightly-load
    from:
      uri: "timer:nightly-load?period=86400000"
      steps:
        - to: "google-bigquery://{{env:GCP_PROJECT:my-project}}:{{env:BQ_DATASET:analytics}}:{{env:BQ_TABLE:events}}?operation=load&sourceUris=gs://{{env:GCS_BUCKET:my-bucket}}/exports/events.json&sourceFormat=NEWLINE_DELIMITED_JSON&writeDisposition=WRITE_APPEND"
        - log: "load job ${header.CamelGoogleBigQueryJobId} finished"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("google-pubsub://my-project:events-sub")
        .unmarshal(DataFormatDefinition::json())
        .to("google-bigquery://my-project:analytics:events?useAsInsertId=id");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `useAsInsertId` | | Row field used as the insert id (de-duplication). |
| `operation` | `insert` | Cameleer: `insert` (streaming insert), `load`, `extract`, `createTable`, `getTable`, `listTables`, `deleteTable`, `createDataset`, `listDatasets`, `deleteDataset`. Header `CamelGoogleBigQueryOperation`. |
| `createTableIfNotExists` | `false` | Streaming insert: create a missing table with `tableSchema` first. |
| `sourceUris` | | `load`: comma-separated `gs://` URIs (or the header, or a list / text body). |
| `sourceFormat` | `CSV` | `load`: `CSV`, `NEWLINE_DELIMITED_JSON`, `AVRO`, `PARQUET`, ... |
| `skipLeadingRows` | | `load`: header rows to skip (CSV). |
| `fieldDelimiter` | | `load`: CSV delimiter. |
| `writeDisposition` | `WRITE_APPEND` | `load`: `WRITE_APPEND`, `WRITE_TRUNCATE`, `WRITE_EMPTY`. |
| `createDisposition` | | `load`: `CREATE_IF_NEEDED` or `CREATE_NEVER`. |
| `autodetect` | | `load`: detect the schema. |
| `destinationUris` | | `extract`: `gs://` URIs (or the header). |
| `destinationFormat` | `NEWLINE_DELIMITED_JSON` | `extract`: `NEWLINE_DELIMITED_JSON` or `CSV`. |
| `printHeader` | | `extract`: CSV header row. |
| `compression` | | `extract`: compression, for example `GZIP`. |
| `jobPollInterval` | `200` | Milliseconds between job status checks. |
| `jobTimeout` | `60000` | Milliseconds to wait for a job. |
| `tableSchema` | | `createTable`: a list of `{name, type, mode, description}` maps, its JSON text, or `name:TYPE,name:TYPE`. Header `CamelGoogleBigQueryTableSchema`. |
| `timePartitioningField`, `timePartitioningType` | `DAY` | `createTable`: time partitioning. |
| `clusteringFields` | | `createTable`: clustering fields. |
| `tableDescription`, `expirationMs` | | `createTable`: description and expiration. |
| `datasetLocation` | `US` | `createDataset`: location. |
| `deleteContents` | | `deleteDataset`: also delete the tables. |

Credentials are shared by all Google components: see [credentials](google-storage.md#credentials).

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGoogleBigQueryTableId` | in | Table (when the URI has none). |
| `CamelGoogleBigQueryTableSuffix` | in | Template suffix: rows go to `table + suffix`. |
| `CamelGoogleBigQueryPartitionDecorator` | in | Partition, for example `20260101` (`table$20260101`). |
| `CamelGoogleBigQueryInsertId` | in | Insert id of the row. |
| `CamelGoogleBigQueryOperation` | in | Cameleer: operation. |
| `CamelGoogleBigQuerySourceUris` | in | Cameleer: `load` source URIs. |
| `CamelGoogleBigQueryDestinationUris` | in | Cameleer: `extract` destination URIs. |
| `CamelGoogleBigQueryJobId` | out | Id of the load or extract job. |

A map body is one row, a list body several rows. Rejected rows raise a `BigQueryException` that
lists the insert errors; a job that ends with an error raises one too.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" \
  -v "$HOME/keys/sa.json:/keys/sa.json:ro" -e GOOGLE_APPLICATION_CREDENTIALS=/keys/sa.json \
  -e GCP_PROJECT=my-project -e BQ_DATASET=analytics -e BQ_TABLE=events \
  pmsudhi/cameleer run /routes/pubsub-to-bigquery.camel.yaml
```

## Common questions

### How do I insert several rows in one request?
Send a list of maps; the whole list is one `insertAll` call.

### How do I avoid duplicate rows on retries?
Set `useAsInsertId` to a unique field (or the `CamelGoogleBigQueryInsertId` header). BigQuery
de-duplicates rows with the same insert id for a short window.

### How do I query BigQuery?
Use [`google-bigquery-sql`](google-bigquery-sql.md) with a standard SQL query.

### How is this different from Apache Camel?
Streaming inserts match Camel. Load and extract jobs and table and dataset management are
Cameleer additions.

## Related

- [Google BigQuery SQL](google-bigquery-sql.md), [Google Pub/Sub](google-pubsub.md), [Google Storage](google-storage.md)
- [Apache Camel Google BigQuery](https://camel.apache.org/components/next/google-bigquery-component.html)
