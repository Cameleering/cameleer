# Google BigQuery SQL component

> The `google-bigquery-sql` component runs standard SQL queries and DML statements on Google BigQuery, with named `@parameters` bound from message headers or the body.

**Scheme:** `google-bigquery-sql:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Google BigQuery Standard SQL](https://camel.apache.org/components/next/google-bigquery-sql-component.html)

## When to use it

- Run a report query on a schedule and send the rows to a file, an API or a chat channel.
- Insert, update or delete rows with DML from incoming messages.
- Give an AI agent a safe, fixed query (only the parameters change) instead of free-form SQL.

## URI format

```text
google-bigquery-sql://projectId:query[?options]
google-bigquery-sql://projectId:file:/path/to/query.sql
```

The query can be written in the URI (URL-encoded where needed) or read from a `file:` or
`classpath:` resource. `@name` parameters are bound from the header `name`, else from the
body map entry `name`. `${header.x}`, `${headers.x}`, `${exchangeProperty.x}` and `${body}`
are substituted into the query text first.

## Example

Every hour, query the large orders and write them to a JSON file:

<!-- example: google-bigquery-sql-hourly-report -->
```yaml
- route:
    id: large-orders-report
    from:
      uri: "timer:report?period=3600000"
      steps:
        - setHeader:
            name: min
            constant: 50
        - to: "google-bigquery-sql://{{env:GCP_PROJECT:my-project}}:SELECT id, customer, total FROM shop.orders WHERE total > @min ORDER BY total DESC"
        - marshal:
            json: {}
        - setHeader:
            name: CamelFileName
            constant: large-orders.json
        - to: "file:{{env:OUT_DIR:out}}"
```

Insert a row for every JSON order posted to an HTTP endpoint; the parameters come from the
body map, and the body becomes the number of inserted rows:

<!-- example: google-bigquery-sql-insert-from-http -->
```yaml
- route:
    id: http-to-bigquery
    from:
      uri: "platform-http:/orders?httpMethodRestrict=POST"
      steps:
        - unmarshal:
            json: {}
        - to: "google-bigquery-sql://{{env:GCP_PROJECT:my-project}}:INSERT INTO shop.orders (id, customer, total) VALUES (@id, @customer, @total)"
        - setBody:
            simple: "inserted ${body} row(s)"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:report?period=3600000")
        .set_header("min", constant(50))
        .to("google-bigquery-sql://my-project:SELECT id, total FROM shop.orders WHERE total > @min")
        .log("${body}");
})?;
```

## Options

| option | default | description |
|---|---|---|
| (credentials) | | `serviceAccountKey`, `GOOGLE_APPLICATION_CREDENTIALS`, `useMetadataServer`, ...: see [credentials](google-storage.md#credentials). |

`connectionFactory` and `projectId` are accepted for Camel compatibility and ignored (the project
is the first part of the URI).

## Message headers

| header | direction | description |
|---|---|---|
| any `name` | in | Value of the `@name` query parameter. |
| `CamelGoogleBigQueryTranslatedQuery` | out | The query after `${...}` substitution. |
| `CamelGoogleBigQueryJobId` | out | Id of the query job. |

The body after the call: the number of affected rows for DML (`INSERT`, `UPDATE`, `DELETE`,
`MERGE`), otherwise a list of rows as maps, with values typed after the result schema
(integers, floats, booleans, strings). All result pages are read.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" -v "$PWD/out:/out" \
  -v "$HOME/keys/sa.json:/keys/sa.json:ro" -e GOOGLE_APPLICATION_CREDENTIALS=/keys/sa.json \
  -e GCP_PROJECT=my-project -e OUT_DIR=/out \
  cameleering/cameleer run /routes/large-orders-report.camel.yaml
```

## Common questions

### How do I pass parameters to a BigQuery query?
Write `@name` in the query and set a header `name` (or a body map entry `name`). Missing
parameters fail the exchange with a clear message.

### How do I keep a long query out of the URI?
Put it in a file and use `google-bigquery-sql://my-project:file:/routes/report.sql`.

### Can I use legacy SQL?
No. Queries run as standard SQL (`useLegacySql=false`), as in Camel.

## Related

- [Google BigQuery](google-bigquery.md) for streaming inserts and load jobs
- [SQL component](sql.md) for relational databases
- [Apache Camel Google BigQuery Standard SQL](https://camel.apache.org/components/next/google-bigquery-sql-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
