# Cassandra CQL component

> The `cql` component runs CQL statements against Apache Cassandra and ScyllaDB, and polls a CQL query on a schedule.

**Scheme:** `cql:` · **Crate:** `cameleer-datastores` · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [Cassandra CQL](https://camel.apache.org/components/next/cql-component.html)

## When to use it

- Write time-series or event data (sensor readings at the edge, click streams, agent run
  logs) into a Cassandra table partitioned by day or device.
- Read rows by partition key in the middle of a route.
- Poll a small table on a schedule, for example a configuration or feature-flag table.

## URI format

```text
cql://host1,host2:port/keyspace?cql=<statement>[&options]
cql:bean:<sessionBean>?cql=<statement>[&options]
```

A port on the last host applies to all hosts (`cql://a,b,c:9042/shop`); the default is 9042.
`cql:bean:name` uses a `CqlSession` bound in the registry.

The producer runs `cql` (or the `CamelCqlQuery` header) as a prepared statement with the
message body as bind values: a list for several `?` markers, a single value for one, nothing
for a null body. The result goes to the body according to `resultSetConversionStrategy`.

The client is Cameleer's own implementation of the CQL native protocol v4: PasswordAuthenticator,
prepared statements (re-prepared when the server forgets them), paging, TLS, LZ4 and Snappy
compression and token-aware routing.

## Example

`POST /events` with `{"day":"2026-11-05","seq":1,"msg":"door opened"}` inserts a row. The `jq`
step turns the JSON object into the list of bind values.

<!-- example: cql-insert-events -->
```yaml
- route:
    id: save-event
    from:
      uri: "platform-http:/events?httpMethodRestrict=POST"
      steps:
        - unmarshal:
            json: {}
        - setBody:
            jq: "[.day, .seq, .msg]"
        - to: "cql://{{env:CASSANDRA_HOSTS:localhost:9042}}/iot?cql=INSERT INTO events (day, seq, msg) VALUES (?, ?, ?)&consistencyLevel=QUORUM"
        - setBody:
            constant: "saved"
```

The table:

```sql
CREATE KEYSPACE iot WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1};
CREATE TABLE iot.events (day text, seq int, msg text, PRIMARY KEY ((day), seq));
```

This route polls one partition every 10 seconds and logs each row. Each poll sends the whole
result set as one exchange (a list of row maps), so `split` handles the rows one by one.

<!-- example: cql-poll-events -->
```yaml
- route:
    id: poll-events
    from:
      uri: "cql://{{env:CASSANDRA_HOSTS:localhost:9042}}/iot?cql=SELECT seq, msg FROM events WHERE day = '2026-11-05'&delay=10000"
      steps:
        - split:
            simple: "${body}"
            steps:
              - to: "log:events"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:event")
        .to("cql://localhost:9042/iot?cql=INSERT INTO events (day, seq, msg) VALUES (?, ?, ?)");
    b.from("direct:day")
        .to("cql://localhost:9042/iot?cql=SELECT seq, msg FROM events WHERE day = ?&resultSetConversionStrategy=LIMIT_10");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `cql` | | the statement (header `CamelCqlQuery` overrides it) |
| `consistencyLevel` | `LOCAL_ONE` | `ANY`, `ONE`, `TWO`, `THREE`, `QUORUM`, `ALL`, `LOCAL_QUORUM`, `EACH_QUORUM`, `LOCAL_ONE`, ... |
| `serialConsistencyLevel` | | `SERIAL` or `LOCAL_SERIAL` for lightweight transactions |
| `prepareStatements` | `true` | `false` sends plain QUERY frames with inferred value types |
| `resultSetConversionStrategy` | `ALL` | `ALL` (list of row maps), `ONE` (first row or null) or `LIMIT_<n>` (first n rows) |
| `fetchSize` | `5000` | page size; all pages are read |
| `username`, `password` | | PasswordAuthenticator credentials |
| `connectTimeout` | `10s` | connection timeout |
| `readTimeout` | `30s` | request timeout |
| `ssl` | `false` | TLS (or `sslContextParameters=#bean`, `useGlobalSslContextParameters=true`) |
| `compression` | none | `lz4` or `snappy` frame compression |
| `tokenAware` | `true` | send prepared statements to the node that owns the partition key; `false` uses the contact points in order |
| `delay`, `initialDelay`, ... | `500` ms, `1000` ms | consumer: the scheduled poll options |

`clusterName`, `loadBalancingPolicyClass`, `datacenter`, `extraTypeCodecs` and `session` are
accepted and ignored. There is no datacenter-aware load balancing.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelCqlQuery` | in | statement to run instead of `cql` |

## Values

Rows are maps. Timestamps come back as ISO-8601 text (epoch milliseconds are accepted on input),
UUIDs and decimals as text, dates as `YYYY-MM-DD`, blobs as bytes, durations as Cassandra duration
text (`1mo2d3h`), lists and sets as arrays, maps and user-defined types as objects.

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -e CASSANDRA_HOSTS=cassandra1,cassandra2:9042 \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/events.camel.yaml
```

## Common questions

### How do I pass several bind values?

Make the body a list in the order of the `?` markers, for example with
`jq: "[.day, .seq, .msg]"` after `unmarshal: json`.

### Does the consumer send one exchange per row?

No. As in Apache Camel, each poll sends one exchange with the converted result set. Use `split`
to process rows individually.

### Does it work with ScyllaDB and Amazon Keyspaces?

The client speaks CQL protocol v4, which ScyllaDB supports. It is tested against an embedded CQL
server, not against Cassandra, ScyllaDB or Keyspaces clusters.

## Related

- [sql](sql.md): relational databases.
- [Split EIP](../guides/eips.md)
- [Apache Camel CQL component](https://camel.apache.org/components/next/cql-component.html)
