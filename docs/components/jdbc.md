# JDBC component

> The `jdbc` component executes the SQL statement in the message body against a data source and returns the rows or the update count.

**Scheme:** `jdbc:` · **Crate:** `cameleer-sql` · **Cargo feature:** [`sql`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [JDBC](https://camel.apache.org/components/next/jdbc-component.html)

## When to use it

- The SQL is built at runtime: by a template, by a previous step, or by an AI agent that
  writes a read-only report query (validate it before you run it).
- Run admin statements or DDL from a route.
- Port a Camel route that already uses `jdbc:` without rewriting it to `sql:`.

For fixed statements with parameters, the [sql](sql.md) component is usually simpler.

## URI format

```text
jdbc:<dataSourceName>[?options]
```

`dataSourceName` is the registry name of a data source. Cameleer binds one named `dataSource`
from the `camel.datasource.url` property (environment variable `CAMEL_DATASOURCE_URL`, plus
`camel.datasource.username` and `camel.datasource.password`), and more with
`camel.datasource.<name>.url`. Supported URLs are `jdbc:rustsql:mem:<name>` (the embedded
in-memory engine) and `jdbc:postgresql://host:port/db`.

With `useHeadersAsParameters=true`, `:?name` placeholders in the SQL are filled from the
`CamelJdbcParameters` header (a map, or a list for positional values) and then from the message
headers.

## Example

`GET /products?maxPrice=2` runs a parameterised query and returns the rows as JSON. The query
parameter becomes the `maxPrice` header, which fills `:?maxPrice`.

<!-- example: jdbc-products-below-price -->
```yaml
- route:
    id: cheap-products
    from:
      uri: "platform-http:/products?httpMethodRestrict=GET"
      steps:
        - setBody:
            constant: "select name, price from products where price < :?maxPrice order by name"
        - to: "jdbc:dataSource?useHeadersAsParameters=true&outputType=SelectList"
        - marshal:
            json: {}
```

Run it with the data source set by a property:

```properties
camel.datasource.url={{env:DB_URL:jdbc:rustsql:mem:shop}}
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:report")
        .set_body(constant("select count(*) as orders from orders"))
        .to("jdbc:dataSource?outputType=SelectOne");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `outputType` | `SelectList` | `SelectList` (list of row maps), `SelectOne` (one row map, or the value when there is one column) or `StreamList` (read fully into a list) |
| `useHeadersAsParameters` | `false` | fill `:?name` placeholders from `CamelJdbcParameters` and the headers |
| `allowNamedParameters` | `true` | recognise `:?name` placeholders |
| `readSize` | `0` | read at most this many rows (0 = all) |
| `statement.maxRows` | | same as `readSize` (the smaller wins) |
| `resetAutoCommit` | `true` | accepted; see below |
| `transacted` | `false` | accepted |
| `parameters.*` | | accepted, ignored |

`outputClass`, `beanRowMapper`, `useJDBC4ColumnNameAndLabelSemantics`, `useGetBytesForBlob`,
`prepareStatementStrategy` and `connectionStrategy` are accepted and ignored: rows are maps.

**Difference from Camel:** `resetAutoCommit=false` does not open a transaction. Outside a JDBC
transaction policy every statement commits on its own.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelJdbcParameters` | in | named (map) or positional (list) parameter values |
| `CamelRetrieveGeneratedKeys` | in | `true` to return the generated keys of an INSERT |
| `CamelJdbcRowCount` | out | rows returned by a SELECT |
| `CamelJdbcColumnNames` | out | column names of a SELECT |
| `CamelJdbcUpdateCount` | out | rows changed by other statements |
| `CamelGeneratedKeysRowCount` | out | number of generated key rows |
| `CamelGeneratedKeysRows` | out | the generated keys, a list of maps |

## Run it with Docker

```sh
docker run --rm -p 8080:8080 \
  -e CAMEL_DATASOURCE_URL='jdbc:postgresql://db.example.com:5432/shop' \
  -e CAMEL_DATASOURCE_USERNAME=camel -e CAMEL_DATASOURCE_PASSWORD \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/products.camel.yaml
```

## Common questions

### Where does the data source come from?

From the registry. Set `camel.datasource.url` (or `CAMEL_DATASOURCE_URL`) to bind `dataSource`,
or bind your own in Rust with `cameleer_sql::bind_datasource(ctx, "name", ds)`.

### How do I pass parameters safely?

Use `useHeadersAsParameters=true` and `:?name` placeholders. Values are sent as bind parameters,
never pasted into the SQL text.

### Is it safe to run SQL written by an LLM?

Only with a read-only database user and a check of the statement before it runs. The component
executes whatever the body contains.

## Related

- [sql](sql.md): fixed statements with parameters, and table polling.
- [sql-stored](sql-stored.md): stored procedures.
- [Apache Camel JDBC component](https://camel.apache.org/components/next/jdbc-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
