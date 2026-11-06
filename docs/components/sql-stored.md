# SQL Stored Procedure component

> The `sql-stored` component calls stored procedures and functions in PostgreSQL or the embedded database, passing IN parameters and returning OUT values and result sets.

**Scheme:** `sql-stored:` · **Crate:** `cameleer-sql` · **Cargo feature:** [`sql`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [SQL Stored Procedure](https://camel.apache.org/components/next/sql-stored-component.html)

## When to use it

- Keep business rules in the database: call a procedure that validates and inserts an order
  and returns a running total.
- Call a set-returning function (`RETURNS TABLE`) and route the rows.
- Reuse existing PL/pgSQL code from an integration route or an AI agent's tool call without
  rewriting it.

## URI format

```text
sql-stored:<template>[?options]
sql-stored:file:<path-to-template-file>[?options]
```

The template follows Camel's syntax: `NAME(parameter, parameter, ...)`.

| parameter | form | example |
|---|---|---|
| IN | `[IN] TYPE[(scale)] valueSource` | `INTEGER ${header.qty}`, `VARCHAR :#item` |
| OUT | `OUT TYPE name` | `OUT INTEGER total` |
| INOUT | `INOUT TYPE valueSource name` | `INOUT INTEGER ${body[qty]} qty` |

`TYPE` is a `java.sql.Types` name (`INTEGER`, `BIGINT`, `VARCHAR`, `DECIMAL(10)`, `BOOLEAN`, ...)
or its numeric code; IN values are converted to it. A value source is a Simple expression
(`${header.num}`, `${body[qty]}`), `:#name` (a body map entry, then a header, an exchange
property or a variable), `:#${simple}`, or a literal (`5`, `'text'`).

A procedure is called as `CALL name(?, ?, NULL)` (OUT positions are passed as `NULL`, as
PostgreSQL 14 and later expect). With `function=true` it is called as
`SELECT * FROM name(?, ?)` and the first OUT parameter names the return value.

The database is chosen with `dataSource`, exactly as for the [sql](sql.md) component: a
`jdbc:rustsql:mem:<name>` or `jdbc:postgresql://user:password@host:port/db` URL, a `#bean`, or
the only data source in the registry.

## Example

The procedure, in PostgreSQL or the embedded engine:

```sql
create table orders (id serial primary key, item text, qty int);
create procedure add_order(in p_item text, in p_qty integer, out total integer)
language plpgsql as $$
begin
  if p_qty <= 0 then raise exception 'invalid quantity % for %', p_qty, p_item; end if;
  insert into orders (item, qty) values (p_item, p_qty);
  select sum(qty) into total from orders;
end $$;
```

The route takes a JSON order over HTTP, calls the procedure and answers with its OUT value.

<!-- example: sql-stored-add-order -->
```yaml
- route:
    id: add-order
    from:
      uri: "platform-http:/orders?httpMethodRestrict=POST"
      steps:
        - unmarshal:
            json: {}
        - to: "sql-stored:add_order(VARCHAR :#item, INTEGER :#qty, OUT INTEGER total)?dataSource={{env:DB_URL:jdbc:rustsql:mem:shop}}"
        - marshal:
            json: {}
```

Posting `{"item":"camel","qty":3}` answers `{"total":3}`. A `raise exception` in the procedure
fails the exchange with an `SQLException` that carries the message.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:add-order")
        .to("sql-stored:add_order(VARCHAR :#item, INTEGER :#qty, OUT INTEGER total)?dataSource=jdbc:rustsql:mem:shop");
    b.from("direct:big-orders")
        .to("sql-stored:orders_over(INTEGER ${header.min})?dataSource=jdbc:rustsql:mem:shop&function=true");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `dataSource` | the only data source in the registry | a JDBC-style URL or `#bean` (also `dataSourceRef`) |
| `function` | `false` | call a function (`SELECT * FROM name(...)`) instead of a procedure |
| `outputType` | `SelectList` for result sets | `SelectList` (list of row maps) or `SelectOne` (one row map, or the value when there is one column) |
| `outputHeader` | | put the result in this header and keep the body |
| `noop` | `false` | call the procedure but keep the body |
| `batch` | `false` | the body is a list; each element (a map or a value) is the parameter source of one call, all in one transaction |
| `useMessageBodyForTemplate` | `false` | the template is the message body |
| `templateOptions.*` | | accepted for compatibility, ignored |

`outputClass` and `lazyStartProducer` are accepted and ignored.

## Result

- OUT and INOUT values become a map body, `{total=3}` (or the `outputHeader` header).
- Result sets (functions returning `SETOF` or `TABLE`, and procedures whose body runs a plain
  SELECT on the embedded engine) become rows according to `outputType`.
- When a template has OUT parameters and the call also returns rows, the rows are added to the
  map under `#result-set-1`, unless `outputType` is set, which makes the rows the result.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelSqlStoredTemplate` | in | template to use instead of the endpoint's |
| `CamelSqlStoredUpdateCount` | out | rows changed by the call |

Over the PostgreSQL protocol, `CALL` reports no row count, so `CamelSqlStoredUpdateCount` is 0 for
procedures without OUT parameters.

## Run it with Docker

```sh
docker run --rm -p 8080:8080 \
  -e DB_URL='jdbc:postgresql://camel:secret@db.example.com:5432/shop' \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/add-order.camel.yaml
```

## Common questions

### How do I read the OUT value?

The body is a map keyed by the OUT parameter names: `${body[total]}` in Simple. Use
`outputHeader=result` to keep the original body and read `${header.result[total]}`.

### How do I call a function instead of a procedure?

Add `function=true`. Name the return value with a leading `OUT` parameter, for example
`sql-stored:subtract(OUT INTEGER result, INTEGER ${header.a}, INTEGER ${header.b})?function=true`.

### Can the call join the route's transaction?

Yes. Calls use the connection of a JDBC transaction policy bound with
`cameleer_sql::bind_transaction_policy` and roll back with the route.

## Related

- [sql](sql.md): plain SQL statements and table polling.
- [jdbc](jdbc.md): SQL from the message body.
- [Apache Camel SQL Stored Procedure component](https://camel.apache.org/components/next/sql-stored-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
