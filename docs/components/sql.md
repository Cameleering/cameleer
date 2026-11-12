# SQL component

> The `sql` component runs SQL queries and updates against PostgreSQL or the embedded in-memory database, and polls tables for new rows.

**Scheme:** `sql:` · **Crate:** `cameleer-sql` · **Cargo feature:** [`sql`](../guides/build-features.md) · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [SQL](https://camel.apache.org/components/next/sql-component.html)

## When to use it

- Insert or update rows from a route: save orders received over HTTP, keep an audit table of
  the decisions an AI agent made, store an LLM's classification next to the ticket.
- Poll a table as a work queue: pick up rows where `processed = false`, route each one, and
  mark it done with `onConsume`.
- Look up reference data in the middle of a route (`outputType=SelectOne`).
- Prototype without a database server: `jdbc:rustsql:mem:<name>` is an embedded in-memory SQL
  engine with the PostgreSQL dialect, so the same routes later run against PostgreSQL.

## URI format

```text
sql:<statement>[?options]
sql:file:<path-to-sql-file>[?options]
```

The statement uses Camel's parameter syntax:

| placeholder | value |
|---|---|
| `:#name` | `name` from the body (when it is a map), else a header, an exchange property or a variable |
| `:#${simple}` | the result of a Simple expression, for example `:#${header.id}` |
| `:#in:name` | a list, expanded to `?, ?, ...` for `IN (...)` |
| `?` or `#` | positional, taken in order from the body (a list, or a string split by `separator`) |

### Choosing the database

`dataSource` names the database. It is either a JDBC-style URL or a registry bean:

| value | database |
|---|---|
| `jdbc:rustsql:mem:shop` | the embedded in-memory engine; every endpoint with the same name shares one database |
| `jdbc:postgresql://user:password@host:5432/db` | PostgreSQL over its wire protocol (SCRAM-SHA-256, MD5 or cleartext authentication, TLS with `?sslmode=require` or `verify-full`) |
| `#name` | a data source bound in the registry |

Without `dataSource`, the endpoint uses the only data source in the registry. Cameleer binds one
named `dataSource` from the `camel.datasource.url` property (with `camel.datasource.username`
and `camel.datasource.password`), which the container also reads from the
`CAMEL_DATASOURCE_URL` environment variable. `camel.datasource.<name>.url` binds more data
sources by name. Other databases (MySQL, Oracle, SQL Server) are not supported: there are no
JDBC drivers, only these two engines.

A PostgreSQL data source is a connection pool, sized in its URL like HikariCP:
`?poolSize=16` opens at most 16 connections (default 8) and reuses them. When all of them are
busy, a caller waits for one to come back, up to `poolTimeout` seconds (default 30), then fails
with `Connection is not available`. Size the pool for the database, not for the number of
concurrent requests: PostgreSQL handles a few busy connections better than many.

## Example

This route polls a table as a work queue: every new order is logged once and marked as
processed. A one-shot timer route creates the table first, so the file runs as is against the
embedded database or against PostgreSQL.

<!-- example: sql-poll-new-orders -->
```yaml
- route:
    id: create-schema
    from:
      uri: "timer:schema?repeatCount=1&delay=0"
      steps:
        - to: "sql:create table if not exists orders (id serial primary key, item text, qty int, processed boolean default false)?dataSource={{env:DB_URL:jdbc:rustsql:mem:shop}}"
- route:
    id: new-orders
    from:
      uri: "sql:select * from orders where processed = false order by id?dataSource={{env:DB_URL:jdbc:rustsql:mem:shop}}&onConsume=update orders set processed = true where id = :#id&delay=2000"
      steps:
        - log: "new order ${body[id]}: ${body[qty]} x ${body[item]}"
```

Each row arrives as a map (`${body[item]}`), one exchange per row. `onConsume` runs once per row
after the route finished it, with the row's columns as `:#` parameters.

This route saves JSON orders posted over HTTP. The fields of the JSON object fill the `:#item`
and `:#qty` parameters, and the response reports the number of inserted rows.

<!-- example: sql-save-http-orders -->
```yaml
- route:
    id: save-order
    from:
      uri: "platform-http:/orders?httpMethodRestrict=POST"
      steps:
        - unmarshal:
            json: {}
        - to: "sql:insert into orders (item, qty) values (:#item, :#qty)?dataSource={{env:DB_URL:jdbc:rustsql:mem:shop}}"
        - setBody:
            simple: "inserted ${header.CamelSqlUpdateCount}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:save-order")
        .to("sql:insert into orders (item, qty) values (:#item, :#qty)?dataSource=jdbc:rustsql:mem:shop");
    b.from("direct:find-order")
        .to("sql:select item, qty from orders where id = :#${header.id}?dataSource=jdbc:rustsql:mem:shop&outputType=SelectOne");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `dataSource` | the only data source in the registry | a JDBC-style URL or `#bean` (also `dataSourceRef`) |
| `outputType` | `SelectList` | `SelectList` (list of row maps), `SelectOne` (one row map, or the value when there is one column; more than one row is an error) or `StreamList` (read fully into a list) |
| `outputHeader` | | store the query result in this header and keep the body |
| `noop` | `false` | run the statement but keep the body |
| `batch` | `false` | the body is a list; run the statement once per element, in one transaction |
| `useMessageBodyForSql` | `false` | the SQL is the message body; positional values come from the `CamelSqlParameters` header |
| `parametersCount` | `0` | number of positional parameters to bind (0 = as many as the statement has) |
| `placeholder` | `#` | extra positional placeholder character next to `?` |
| `usePlaceholder` | `true` | whether `placeholder` is recognised |
| `separator` | `,` | splits a string body into positional values |
| `allowNamedParameters` | `true` | recognise `:#name` parameters |
| `template.maxRows` | `0` | stop reading after this many rows (0 = all) |
| `useIterator` | `true` | consumer: one exchange per row; `false` sends all rows as one list (header `CamelSqlRowCount`) |
| `onConsume` | | consumer: statement run per row after it was processed successfully |
| `onConsumeFailed` | | consumer: statement run per row when processing failed |
| `onConsumeBatchComplete` | | consumer: statement run once after all rows of a poll |
| `maxMessagesPerPoll` | `0` | consumer: at most this many rows per poll (0 = all) |
| `routeEmptyResultSet` | `false` | consumer: route an empty list when the query returns no rows |
| `transacted` | `false` | consumer: run each poll in one transaction, rolled back when a row fails |
| `breakBatchOnConsumeFail` | `false` | consumer: stop the poll when `onConsume` fails |
| `expectedUpdateCount` | `-1` | consumer: fail when `onConsume` changes a different number of rows |
| `delay`, `initialDelay`, `repeatCount`, ... | `500` ms, `1000` ms | consumer: the scheduled poll options |

`outputClass`, `processingStrategy`, `prepareStatementStrategy`, `alwaysPopulateStatement` and
`rowMapperFactory` are accepted for compatibility and ignored: rows are always maps.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelSqlQuery` | in | SQL to run instead of the endpoint's statement |
| `CamelSqlParameters` | in | positional values when `useMessageBodyForSql=true` |
| `CamelSqlRetrieveGeneratedKeys` | in | `true` to return the generated keys of an INSERT |
| `CamelSqlUpdateCount` | out | rows changed by INSERT, UPDATE or DELETE (the total for `batch`) |
| `CamelSqlRowCount` | out | rows returned by a SELECT |
| `CamelSqlGeneratedKeysRowCount` | out | number of generated key rows |
| `CamelSqlGeneratedKeysRows` | out | the generated keys, a list of maps |

The consumer sets the exchange properties `CamelBatchIndex`, `CamelBatchSize` and
`CamelBatchComplete`.

## Run it with Docker

Against the embedded database (data is lost when the container stops):

```sh
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" cameleering/cameleer run /routes/orders.camel.yaml
```

Against PostgreSQL:

```sh
docker run --rm -p 8080:8080 \
  -e DB_URL='jdbc:postgresql://camel:secret@db.example.com:5432/shop?sslmode=require' \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/orders.camel.yaml
```

## Common questions

### How do I connect to PostgreSQL?

Set `dataSource=jdbc:postgresql://user:password@host:5432/db`, or set `camel.datasource.url`
(environment variable `CAMEL_DATASOURCE_URL`) and leave `dataSource` out. URL-encode special
characters in the password.

### How do I get the id of an inserted row?

Set the header `CamelSqlRetrieveGeneratedKeys` to `true`. The keys come back in
`CamelSqlGeneratedKeysRows`, for example `[{id=7}]`. On PostgreSQL you can also write
`insert ... returning id` and read the result as a query.

### How do I process each row only once?

Use `onConsume` to mark the row (`update ... set processed = true where id = :#id`) and filter on
that column in the polling query. Add `transacted=true` to roll the marks back when a row fails.

### Does it support MySQL or Oracle?

No. Cameleer has no JDBC drivers: it speaks the PostgreSQL wire protocol (tested against
PostgreSQL 16) and embeds its own engine.

## Related

- [sql-stored](sql-stored.md): call stored procedures and functions.
- [jdbc](jdbc.md): run SQL taken from the message body.
- [Apache Camel SQL component](https://camel.apache.org/components/next/sql-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
