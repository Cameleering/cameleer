# What Cameleer does not do

> The parts of Apache Camel, and the integrations people ask about, that Cameleer does not have: Java code and beans, databases other than PostgreSQL, OpenTelemetry, change data capture, clustering and leader election, XA transactions, some Kafka and ActiveMQ options, Excel, and the Camel components people ask about most that are not ported. Each entry says what to use instead, where there is something.

**Applies to:** Cameleer 0.5 · **Apache Camel:** [the component list](https://camel.apache.org/components/next/index.html) · Parity with Camel, row by row: [FEATURES.md](../FEATURES.md)

Cameleer reimplements Camel's routing engine, EIPs, YAML and XML DSLs and about 230 components,
languages and data formats in Rust, with no JVM and no external crates. What it cannot do comes
mostly from that: anything that needs a JVM, or a client library that has not been written from
scratch yet. When something on this page changes, the page changes with it; the component list
below is checked against the build by `limitations_match_the_catalog` in
`crates/cameleer-main/tests/docs_reference.rs`.

## Java and the JVM

| not available | instead |
|---|---|
| the Java DSL (`RouteBuilder` classes), Spring XML `<bean>` definitions, jars on the classpath | YAML or XML routes; the [Rust DSL](rust.md) |
| Java beans with reflection-based method and parameter binding (`bean:`, `@Handler`, `@Body`) | a processor written in Rust and bound in the registry, a `script` step (`js`, `groovy`, `python`, `java` snippets run by Cameleer's own interpreters, without Java libraries), or EIPs with Simple, jq or JSONPath |
| POJO bodies and type conversion to Java classes (`unmarshalType`, `bodyAs(Order.class)`) | bodies are dynamic values (text, bytes, numbers, lists, maps); `${body[field]}`, JSONPath, jq |
| JMX and MBeans, Hawtio | `/q/metrics` (Prometheus), `/q/health`, the dev console, `cameleer get` ([Observability](observability.md)) |
| Camel Quarkus and Camel Spring Boot starters | the `cameleer` binary or container image; Spring Boot and Quarkus property names are understood ([Configuration](configuration.md)) |
| Camel K (the Kubernetes operator) | `kind: Integration` files run in-process; deploy the container image yourself ([deployment guide](../DEPLOYMENT.md)) |

## Databases

| not available | instead |
|---|---|
| MySQL, MariaDB, Oracle, SQL Server, DB2, SQLite, or any other JDBC driver | PostgreSQL over its wire protocol, or the embedded in-memory SQL database ([sql](../components/sql.md)) |
| PostgreSQL `LISTEN`/`NOTIFY` (Camel's `pgevent`) | poll a table with the `sql` consumer |
| change data capture: Debezium (`debezium-postgres`, `debezium-mysql`, ...) | poll with the `sql` consumer and mark rows as processed; MongoDB change streams (`mongodb` consumer, `consumerType=changeStreams`), DynamoDB Streams, Cosmos DB change feed and CouchDB `_changes` where the source has them |
| JPA, Hibernate, MyBatis | SQL in the route (`sql:`), or a Rust processor |
| XA and JTA transactions across several resources | local transactions with Camel's propagation behaviours (`transacted`); for exactly-once across systems, an idempotent consumer or the outbox pattern |

## Messaging

| not available | instead |
|---|---|
| ActiveMQ's OpenWire protocol | `activemq:` speaks STOMP; `amqp:` speaks AMQP 1.0 to ActiveMQ Classic and Artemis |
| Kafka SASL `GSSAPI` (Kerberos) and `OAUTHBEARER` | SASL `PLAIN`, `SCRAM-SHA-256`/`512`, mutual TLS, and `AWS_MSK_IAM` for Amazon MSK ([kafka](../components/kafka.md)) |
| Confluent Schema Registry (the Avro and Protobuf serializers that look schemas up in it) | the `avro` and `protobuf` data formats with a schema given in the route; `azure-schema-registry` for Azure |
| Apache Pulsar, NATS, IBM MQ, Solace | Kafka, MQTT, AMQP 0-9-1 and 1.0, STOMP, JMS over those, the cloud queues |
| AMQP 1.0 link recovery with unsettled-state resume | reconnect with redelivery, and make consumers idempotent |

## Running several instances

| not available | instead |
|---|---|
| clustering and leader election (Camel's `master:` component and cluster services on ZooKeeper, Kubernetes, Consul or JGroups) | run one instance of a polling route, or let the broker share the work: Kafka consumer groups, competing consumers on a queue |
| distributed aggregation and idempotent repositories (Infinispan, Hazelcast, LevelDB) | memory, JDBC (PostgreSQL) and Redis repositories, which several instances can share |

## Observability

| not available | instead |
|---|---|
| OpenTelemetry traces and metrics (`camel-opentelemetry`), Micrometer (`camel-micrometer`), `camel-observation` | Prometheus metrics at `/q/metrics`, which an OpenTelemetry Collector can scrape; the tracer for messages ([Observability](observability.md)). an MCP tool call passes the `traceparent`, `tracestate` and `baggage` of its `_meta` to the route as headers |
| JSON log lines | plain text lines in Camel's format |

## Data formats and files

| not available | instead |
|---|---|
| Excel files (Apache POI: `.xlsx`, `.xls`) | CSV (`csv` data format), or `google-sheets` for Google Sheets |
| SMB/CIFS file shares (`smb:`) | `file:` on a mounted share, `ftp`, `ftps`, `sftp` |

## Camel components that Cameleer does not have

Camel has about 300 components. These are the ones people ask about most that are not in
Cameleer; for anything else, `cameleer catalog components` lists every scheme in your build, and
[FEATURES.md](../FEATURES.md) has the full table.

| Camel component (scheme) | area |
|---|---|
| `debezium-postgres`, `debezium-mysql`, `debezium-mongodb`, `debezium-sqlserver`, `debezium-oracle` | change data capture |
| `pgevent` | PostgreSQL `LISTEN`/`NOTIFY` |
| `jpa`, `mybatis`, `hibernate` | Java persistence |
| `master` | leader election |
| `zookeeper`, `consul`, `etcd3` | coordination services |
| `infinispan`, `hazelcast-map`, `ignite-cache` | data grids |
| `kubernetes-pods`, `kubernetes-deployments` | Kubernetes API |
| `pulsar`, `nats`, `jt400`, `mina`, `ldap`, `smb` | other protocols |
| `salesforce`, `servicenow`, `sap-netweaver` | enterprise applications |
| `opentelemetry`, `micrometer` | telemetry |
| `couchbase` | Couchbase (binary key-value protocol) |

## What has not been verified yet

These work and are tested, but not yet against the real thing:

- **Cloud services.** The AWS, Azure and Google Cloud components are tested against embedded
  mock servers, the official SDKs' clients and third-party emulators (moto, Azurite,
  fake-gcs-server, pstest), not yet against real cloud accounts.
- **AWS Lambda.** The custom runtime is tested end to end in AWS's `provided:al2023` image with
  the Runtime Interface Emulator, not yet on the Lambda service itself.
- **Attestation** has not run on real confidential-computing hardware yet, and it does not
  verify hardware signatures itself; see the limits in [Attestation](attestation.md).
- **Security audit.** The TLS, X.509, SSH and XML signature code is written in the project and
  has not had an independent security audit.

Known differences in behaviour from Apache Camel, such as how a failed `http` call leaves the
message, are listed in [Migrating from Apache Camel](migrating-from-apache-camel.md#known-deviations).

## Common questions

### Does Cameleer support MySQL or Oracle?

No. The `sql` and `jdbc` components work with PostgreSQL and the embedded database only; there
are no drivers for other databases.

### Does Cameleer support OpenTelemetry?

Not yet. Metrics are in the Prometheus format at `/q/metrics`, which an OpenTelemetry Collector
can scrape.

### Can two Cameleer instances share work, or elect a leader?

There is no leader election. Instances share work through the broker (Kafka consumer groups,
competing consumers on a queue) and can share Redis or PostgreSQL idempotent and aggregation
repositories.

### Can I use my existing Java processors?

No. Rewrite them as Rust processors, `script` steps, or EIPs; see
[Migrating from Apache Camel](migrating-from-apache-camel.md#migration-checklist).

### Something I need is missing. What now?

[Open an issue](https://github.com/pmsudhi/cameleer/issues/new) that names the component or
option and what you use it for. The project adds what users need, written from scratch.

## Related

- [Migrating from Apache Camel](migrating-from-apache-camel.md), [FAQ](faq.md)
- [Rust developer guide](rust.md), [Troubleshooting](troubleshooting.md)
- [FEATURES.md](../FEATURES.md): every Camel feature, row by row

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
