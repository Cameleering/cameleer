# rust-camel

A Rust implementation of the [Apache Camel](https://camel.apache.org/) integration framework:
routes, endpoints and components, the Enterprise Integration Patterns, expression languages
(Simple, JSONPath, XPath, tokenize, jq...), data formats, a fluent Rust DSL, the YAML/XML DSLs,
a REST DSL, and a `camel` command-line runner.

It is written in **pure Rust using only the standard library** (no external crates) — including its own TLS, SSH, HTTP/2 and protocol stacks.

```rust
use camel_core::prelude::*;

fn main() -> Result<()> {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:hello?period=200")
            .route_id("hello")
            .set_body(simple("Hello from rust-camel #${exchangeProperty.CamelTimerCounter} at ${date:now:HH:mm:ss}"))
            .filter(simple("${exchangeProperty.CamelTimerCounter} > 2"))
                .to("log:greetings")
            .end();
    })?;
    ctx.start()?;
    std::thread::sleep(std::time::Duration::from_millis(2000));
    ctx.stop()
}
```

Or with the YAML DSL and the CLI:

```yaml
# hello.camel.yaml
- route:
    id: hello
    from:
      uri: timer:hello
      parameters:
        period: 1000
      steps:
        - setBody:
            simple: "Hello ${exchangeProperty.CamelTimerCounter}"
        - log: "${body}"
```

```text
$ cargo run -p camel-main --bin camel -- run hello.camel.yaml --max-seconds 5
```

## Crates

| crate | contents |
|---|---|
| `camel-core` | `CamelContext`, `Exchange`/`Message`/`Value`, routes and the Rust DSL, EIPs, error handling, languages, data formats, core components |
| `camel-http` | HTTP/1.1 server and client, `platform-http`, `http`, REST DSL with OpenAPI, `netty` TCP/UDP |
| `camel-yaml` | YAML parser, Camel YAML DSL, XML DSL, route templates |
| `camel-protocols` | ftp, smtp/pop3/imap, websocket, mllp/hl7, syslog (with embedded test servers) |
| `camel-tls` | from-scratch TLS 1.3/1.2, X.509, `SslContextParameters` (used by all network components) |
| `camel-kafka-mqtt` | kafka, paho / paho-mqtt5 (with embedded test brokers) |
| `camel-amqp` | spring-rabbitmq / rabbitmq (AMQP 0-9-1), stomp, jms / activemq / amqp-stomp (with embedded test brokers) |
| `camel-amqp10` | AMQP 1.0 client (SASL, TLS, CBS), amqp (JMS over AMQP 1.0), azure-servicebus, azure-eventhubs (embedded AMQP 1.0 broker with Azure emulation) |
| `camel-sql` | embedded SQL engine, PostgreSQL client, sql / jdbc components, JDBC repositories |
| `camel-cloud` | AWS S3/SQS/SNS/DynamoDB/Lambda, Azure Blob/Queue, Google Storage/Pub/Sub (with verifying emulators) |
| `camel-ssh` | SSH-2 + SFTP: sftp and ssh components |
| `camel-grpc` | HTTP/2 + HPACK, protobuf, grpc component and protobuf data format |
| `camel-datastores` | spring-redis, mongodb, elasticsearch/opensearch, cql (Cassandra), couchdb (with embedded test servers) |
| `camel-xml` | XML 1.0 parser + DOM, XPath 1.0 (`xpath` language), XSLT 1.0 (`xslt`, `xslt-saxon`), XML Schema (`validator`), `xj`, `xmlsecurity-sign`/`-verify` |
| `camel-saas` | slack, telegram, github, twilio, jira, openai, webhook, graphql, rss/atom (with embedded mock servers) |
| `camel-main` | `Main` runner (application.properties, route discovery) and the `camel` CLI |

See [ARCHITECTURE.md](ARCHITECTURE.md) for the internals, and [docs/FEATURES.md](docs/FEATURES.md) for
the list of what's implemented compared with Apache Camel. [docs/INTEROP.md](docs/INTEROP.md) lists the
real servers the components are verified against (PostgreSQL, Redis, Mosquitto, RabbitMQ with
AMQP 0-9-1 / STOMP / AMQP 1.0 / MQTT, vsftpd, Postfix + Dovecot, nginx, JDK JSSE) and the bugs
that live testing found.

## Building and testing

```text
cargo build
cargo test
```

`cargo test` is hermetic: every network component is tested against embedded servers. The live
interop suite (`crates/*/tests/live_*.rs`) runs against real servers and is opt-in:

```text
sudo scripts/live-interop.sh all          # install, configure and start the servers
CAMEL_LIVE_INTEROP=1 cargo test --workspace live_
```
