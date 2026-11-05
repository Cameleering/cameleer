# Cameleer

[![CI](https://github.com/pmsudhi/cameleer/actions/workflows/ci.yml/badge.svg)](https://github.com/pmsudhi/cameleer/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![Dependencies: none](https://img.shields.io/badge/dependencies-std%20only-brightgreen.svg)](Cargo.toml)
[![Apache Camel compatible](https://img.shields.io/badge/follows-Apache%20Camel%204.x-d4523f.svg)](https://camel.apache.org/)

### Apache Camel, rewritten in Rust. If you know Camel, you already know Cameleer.

The same endpoint URIs. The same Enterprise Integration Patterns. The same Simple expressions, the
same YAML routes, the same headers and the same error handling. About 230 components, languages and
data formats, ported one by one and verified against the real systems.

**But rebuilt for the agent era:** a single native binary that starts in **10 ms**, runs in about
**10 MB**, and routes messages **4–8x faster** than Camel on the JVM.

[Website](https://pmsudhi.github.io/cameleer/) · [Quick start](#quick-start) · [Benchmarks](#rebuilt-for-speed-memory-and-scale) ·
[Agents](#rebuilt-for-agents) · [Serverless](#rebuilt-for-serverless-and-the-edge) ·
[Roadmap](docs/ROADMAP.md) · [FAQ](#faq)

_Cameleer was called rust-camel up to v0.1.0._

> Based on [Apache Camel](https://camel.apache.org/) ([source](https://github.com/apache/camel)), the
> integration framework by the Apache Software Foundation. Cameleer follows its names, options and
> semantics but contains no Camel source code. It is an independent project, not affiliated with or
> endorsed by the ASF. Apache, Apache Camel and Camel are trademarks of the Apache Software Foundation.

---

## Rebuilt for speed, memory and scale

The same routes run on both, measured on the same machine against Apache Camel 4.23 on Java 21:

```text
Startup (spawn → routes running)                      lower is better
  Cameleer  ▏ 10 ms
  Camel       ████████████████████████████████████████ 751 ms              77x faster

Memory (peak, direct route)                           lower is better
  Cameleer  █ 9 MB
  Camel       ████████████████████████████████████████ 366 MB              42x less

Throughput, direct route                              higher is better
  Cameleer  ████████████████████████████████████████ 4.96M msgs/s
  Camel       █████████▍ 1.16M msgs/s                                      4.3x

Throughput, seda request-reply                        higher is better
  Cameleer  ████████████████████████████████████████ 2.09M msgs/s
  Camel       ████▊ 250k msgs/s                                            8.4x

p99 latency, request-reply                            lower is better
  Cameleer  ██████▋ 5.1 µs
  Camel       ████████████████████████████████████████ 30.6 µs             6x lower
```

| scenario | Cameleer | Apache Camel | faster by |
|---|---:|---:|---:|
| `direct` route: set a header, count | 4.96M msgs/s | 1.16M msgs/s | **4.27x** |
| `seda`: 4 producers, 4 concurrent consumers | 5.65M msgs/s | 1.35M msgs/s | **4.18x** |
| `seda` request-reply | 2.09M msgs/s | 250k msgs/s | **8.37x** |
| content-based router + filter + transform | 2.10M msgs/s | 604k msgs/s | **3.48x** |
| JSON split + aggregate, 10 items per order | 113k orders/s | 35.6k orders/s | **3.18x** |
| HTTP consumer, 16 keep-alive connections | 273k req/s | 112k req/s | **2.45x** |
| memory, all scenarios | 9–15 MB | 263–491 MB | **25–46x less** |

_AMD Ryzen 5 5600X, Windows 11, idle machine, default settings on both sides. Each scenario runs in
a fresh process, with the same driver and load generator. All six scenarios are reported, including
the three under 4x. HTTP is capped by the test machine: a server that does nothing peaks near
400k req/s there. Reproduce it with one command, `scripts/bench.sh`. Full report:
[docs/BENCHMARKS.md](docs/BENCHMARKS.md)._

**How, without changing what Camel means:**
- **Nothing but the Rust standard library.** TLS, SSH, HTTP/2, gRPC, AMQP, MQTT, the Kafka
  protocol, the PostgreSQL, Redis, MongoDB and Cassandra wire protocols, XML and JSON are all
  written from scratch. There's no dependency tree, no build-time downloads and no supply-chain
  surprise.
- **No JVM, no garbage collector.** The compiler guarantees memory safety, and routing never pauses
  for GC.
- **Engineered hot paths:**
  - a thread-caching allocator;
  - exchange ids created only when read;
  - copy-on-write JSON objects;
  - a two-lock seda queue that spins before it parks;
  - statistics on the CPU's time-stamp clock with per-thread counters.

  The details are in [benchmarks/analysis.md](benchmarks/analysis.md).

---

## Same Camel. Same signature.

Here is a route that sends big orders from Kafka to SQS and taps every order into an audit topic,
written for Apache Camel and for Cameleer:

<table>
<tr><th>Apache Camel (Java)</th><th>Cameleer</th></tr>
<tr><td>

```java
from("kafka:orders")
    .routeId("big-orders")
    .filter(simple("${header.amount} > 100"))
        .to("aws2-sqs:big-orders")
    .end()
    .wireTap("kafka:audit");
```

</td><td>

```rust
b.from("kafka:orders")
    .route_id("big-orders")
    .filter(simple("${header.amount} > 100"))
        .to("aws2-sqs:big-orders")
    .end()
    .wire_tap("kafka:audit");
```

</td></tr>
</table>

Same URIs, same EIPs, same expression. Only the casing follows Rust. If you write Camel YAML, you
don't change anything at all:

<!-- example: readme-big-orders -->
```yaml
- route:
    id: big-orders
    from:
      uri: kafka:orders
      steps:
        - wireTap: kafka:audit
        - filter:
            simple: "${header.amount} > 100"
            steps:
              - to: aws2-sqs:big-orders
```

```text
$ camel run big-orders.camel.yaml        # the Camel JBang workflow, as one native binary
```

**What carries over from Camel:**
- endpoint URIs and their options;
- EIP names and semantics: split, aggregate, multicast, circuit breaker, saga, idempotent consumer
  and the rest;
- the Simple, JSONPath, XPath and jq languages;
- `Camel*` headers and exchange properties;
- `onException` and the dead letter channel;
- the YAML and XML DSLs, the REST DSL, Kamelets and route templates;
- health checks, metrics, tracing;
- the `camel` CLI (`run`, `init`, `export`, `get`, `trace`).

Your team's Camel experience, designs and routes come with you.

---

## Rebuilt for agents

An agent decides; something still has to *do* it:
- read from Kafka, call the model, write to Postgres;
- open the ticket, post to Slack;
- retry when the API times out, and never charge a card twice.

That's what Camel has done for enterprises for years. Cameleer does it for agents, as a library
inside your agent or as a 10 ms sidecar.

**🖐 Give agents hands.** Every action becomes a route the agent can call over HTTP, a queue or
directly, with retries, timeouts, idempotency and a dead letter channel. The agent says *what*; the
route handles *how*, including the failures.
> "Refund order 4411 and tell the customer" → `platform-http` → idempotent consumer → payments API
> with redelivery → `slack` or `aws-ses`, dead letter on failure.

**⚡ Feed agents events, not polling loops.** Turn Kafka, SQS, MQTT, webhooks, mailboxes and file
drops into model calls. Filter and enrich first, so you pay for fewer tokens. Route on the answer.
> New ticket on `kafka` → drop spam with a filter → `aws-bedrock` (Claude) classifies → urgent ones
> page on-call, the rest go to a queue.

**📚 Ground agents in your data.** Pull documents from S3, Google Drive, Gmail, SFTP or a database,
split them, embed them with OpenAI, Bedrock or Vertex AI, and index them where you search. The same
route keeps the index fresh.
> New file in `aws2-s3` → `split` by paragraph → `openai:embeddings` → `elasticsearch`, with the
> source as metadata.

**🛡 Keep models in check.**
- a circuit breaker with fallback between Bedrock, Azure OpenAI and Vertex AI;
- a throttle to stay under rate limits;
- Bedrock guardrails on inputs and outputs;
- a wire tap of every prompt and answer to an audit log.

> `circuitBreaker` → `aws-bedrock` (primary) → on failure `azure-openai` (fallback) → `wireTap` to
> `kafka:llm-audit`.

**Models today:** OpenAI and any OpenAI-compatible server (`openai`), AWS Bedrock with Converse,
streaming, guardrails and knowledge bases (`aws-bedrock`), Azure OpenAI with tool calls and
Assistants (`azure-openai`), and Google Vertex AI with Gemini plus Anthropic and Meta models
(`google-vertexai`).

---

## Rebuilt for serverless and the edge

Integrations sit idle, then get busy all at once: exactly the load serverless is priced for. A JVM
fights that model: it takes most of a second to boot and hundreds of megabytes to idle.

- **Scale to zero, wake up instantly.** A 10 ms start fits inside a request's latency budget.
- **Pack tens of services onto one node.** At about 10 MB each, Cameleer fits on edge gateways
  and small devices that can't host a JVM.
- **One executable, nothing to install.** No JRE to patch and no `-Xmx` to tune. `camel export`
  turns YAML routes into a standalone Rust project.

| Run it as | Status |
|---|---|
| a library inside your Rust service or agent | ✅ |
| a single binary running YAML or XML routes (`camel run`, with hot reload in dev mode) | ✅ |
| a container on Cloud Run, Fargate, Azure Container Apps, Knative or Kubernetes | ✅ |
| a standalone project generated from your routes (`camel export`) | ✅ |
| a native AWS Lambda function | 🚧 [build it today](docs/ROADMAP.md#14-aws-lambda-function), adapter planned |
| an MCP server exposing routes as agent tools | 🚧 [build it today](docs/ROADMAP.md#11-agent-tool-server-mcp-style), component planned |

---

## Real uses

| | Trigger | Route | Outcome |
|---|---|---|---|
| **Payments** | payment events on `kafka` | idempotent consumer → enrich from `sql` → model risk score → `choice` | duplicates dropped, risky payments held, the rest settled |
| **Healthcare** | HL7 v2 over `mllp` | parse → transform → `http` to the EHR, with redelivery | results delivered exactly once, failures to a dead letter queue |
| **Support** | `aws2-sqs`, `mail` or `telegram` | filter → `aws-bedrock` classifies and drafts → `choice` | urgent issues paged, routine ones answered with a draft |
| **E-commerce** | order webhooks on `platform-http` | `split` line items → route per warehouse → `aggregate` | one fulfilment message per warehouse, stock in `aws2-ddb` |
| **IoT and the edge** | sensor readings on `paho-mqtt5` | `throttle` → `aggregate` per device per minute | rollups to `aws2-timestream`, raw data stays on site |
| **AI ops** | `github` events and alerts on `platform-http` | model summary → dedupe → route by severity | one Slack thread per incident, not a hundred alerts |
| **File exchange** | partner files on `sftp` | unmarshal CSV, XML or EDI → validate → `split` | rows in PostgreSQL, rejects back to the partner |

Every component named here ships today; each row is a pattern you assemble from them.

---

## Not packaged yet? Build it on the engine

| You want | Build it from today's parts | Packaged version |
|---|---|---|
| an agent tool server (MCP-style) | `platform-http` + `toD` to one `direct:` route per tool | 🚧 MCP component |
| a tool-calling agent loop | `loop` + `azure-openai` tool calls + `recipientList` to tool routes | 🚧 agent EIP |
| vector search for RAG | embeddings → pgvector through `sql`, OpenSearch k-NN, or Qdrant and Pinecone through `http` | 🚧 vector components |
| an AWS Lambda function | a boot route looping over the Lambda Runtime API with `http` | 🚧 Lambda adapter |
| token streaming to users | model streaming chunks → `websocket` or a server-sent-events endpoint | ✅ parts ship |
| per-tenant token budgets | model usage headers → counters in `spring-redis` → `filter` before the call | ✅ parts ship |
| human approval of risky actions | `slack` or `mail` link → `platform-http` callback → resume the parked action | ✅ parts ship |

Sketches for each, plus the planned improvements to agents, serverless, performance, operations and
verification, are in **[docs/ROADMAP.md](docs/ROADMAP.md)**.

---

## Quick start

### With Docker: run a Camel YAML route

Save this as `hello-api.camel.yaml` (or take [examples/hello-api.camel.yaml](examples/hello-api.camel.yaml)):

<!-- example: readme-hello-api -->
```yaml
- rest:
    path: /orders
    get:
      - path: /{id}
        to: direct:getOrder
- route:
    from:
      uri: direct:getOrder
      steps:
        - setBody:
            simple: '{"id": "${header.id}", "status": "shipped"}'
```

Run it and call it:

```sh
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/hello-api.camel.yaml
curl localhost:8080/orders/42      # {"id": "42", "status": "shipped"}
```

The image is a single native binary on distroless, for linux/amd64 and linux/arm64, on
[Docker Hub](https://hub.docker.com/r/pmsudhi/cameleer) and as `ghcr.io/pmsudhi/cameleer`. It runs
as non-root and works with `--read-only`. Any route written for Apache Camel's YAML DSL runs the
same way. Kubernetes, Docker Compose and Cloud Run setups are in the
[deployment guide](docs/DEPLOYMENT.md).

### As a Rust crate: embed it in your program

```toml
[dependencies]
cameleer-core = "0.2"
cameleer-http = "0.2"
```

```rust
use cameleer_core::prelude::*;
use cameleer_http::rest::RestDslExt;

fn main() -> Result<()> {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    ctx.properties().set("camel.server.port", "8080");
    ctx.add_routes(|b| {
        b.rest("/api/greet").get("/{name}").to("direct:greet");
        b.from("direct:greet")
            .set_header("Content-Type", constant("application/json"))
            .set_body(simple(r#"{"greeting": "Hello, ${header.name}!"}"#));
    })?;
    ctx.start()?;
    loop { std::thread::park(); }
}
```

To run YAML routes with every component from your own program, depend on `cameleer-main` and
call `cameleer_main::new_main()`, then `add_route_file(...)` and `run()`
([examples/embed_yaml.rs](crates/cameleer-main/examples/embed_yaml.rs)).

### The `cameleer` CLI

From crates.io:

```sh
cargo install cameleer-main
cameleer run hello-api.camel.yaml
```

Or from source:

```sh
git clone https://github.com/pmsudhi/cameleer && cd cameleer
cargo build --release -p cameleer-main
target/release/cameleer run examples/rest-api.camel.yaml
```

The CLI also installs as `camel`, so Camel JBang habits work: `camel run`, `camel ps`, `camel stop`.

### Real use cases, ready to run

Every file below runs end to end in the test suite
([crates/cameleer-main/tests/examples.rs](crates/cameleer-main/tests/examples.rs)) against an
embedded Kafka broker and mock S3, Slack and OpenAI servers. Each file's header lists the
environment variables it needs and the exact `docker run` command.

| use case | route | what it does |
|---|---|---|
| Smallest REST API | [hello-api.camel.yaml](examples/hello-api.camel.yaml) | `GET /orders/{id}` returns JSON: the Quick start above. |
| REST API | [rest-api.camel.yaml](examples/rest-api.camel.yaml) | JSON REST endpoints with validation, a content-based router and error handling. Needs nothing else. |
| AI ticket triage | [ai-ticket-triage.camel.yaml](examples/ai-ticket-triage.camel.yaml) | Kafka in, an LLM classifies each ticket P1 or P3, urgent tickets go to their own topic. Works with OpenAI or any OpenAI-compatible server (Ollama, vLLM, Azure OpenAI). |
| Alerts to Slack | [alerts-to-slack.camel.yaml](examples/alerts-to-slack.camel.yaml) | An HTTP webhook takes monitoring alerts, keeps the critical ones and posts them to Slack. |
| Files to S3 | [files-to-s3.camel.yaml](examples/files-to-s3.camel.yaml) | Watches a folder and uploads each new CSV file to S3, then moves it to `.done`. |
| Rust REST service | [rest_service.rs](crates/cameleer-main/examples/rest_service.rs) | The Rust DSL with two crates: REST endpoints, JSON, an echo service. |
| Embed YAML in Rust | [embed_yaml.rs](crates/cameleer-main/examples/embed_yaml.rs) | Your own binary that runs Camel YAML routes with every component. |

The AI triage route, for example:

<!-- example: readme-ai-ticket-triage -->
```yaml
- route:
    id: ticket-triage
    from:
      uri: "kafka:support-tickets?brokers={{env:KAFKA_BROKERS:localhost:9092}}&groupId=triage&autoOffsetReset=earliest"
      steps:
        - setProperty:
            name: ticket
            simple: "${body}"
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&temperature=0&systemMessage=Classify the support ticket. Reply P1 for outages, data loss or security issues, otherwise P3. Reply with P1 or P3 only."
        - setHeader:
            name: priority
            simple: "${body}"
        - setBody:
            simple: "${exchangeProperty.ticket}"
        - choice:
            when:
              - simple: "${header.priority} contains 'P1'"
                steps:
                  - to: "kafka:tickets-urgent?brokers={{env:KAFKA_BROKERS:localhost:9092}}"
            otherwise:
              steps:
                - to: "kafka:tickets-routine?brokers={{env:KAFKA_BROKERS:localhost:9092}}"
```

On Linux (with Docker Desktop, use `KAFKA_BROKERS=host.docker.internal:9092` instead of
`--network host`):

```sh
docker run --rm --network host -e KAFKA_BROKERS=localhost:9092 -e OPENAI_API_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/ai-ticket-triage.camel.yaml
```

---

## FAQ

**Is it a drop-in replacement for Apache Camel?**
For routes, mostly: URIs, options, EIPs, headers and Simple follow Camel, and Camel YAML and XML
routes run. Java code doesn't: processors, beans and custom components are written in Rust.

**Does it have every Camel connector?**
No. It has about 230 components, languages and data formats, including all of Camel's AWS, Azure and
Google Cloud components, but not the long tail of vendor connectors.
[docs/FEATURES.md](docs/FEATURES.md) lists them row by row, including known gaps.

**Does it run my LLM or store vectors?**
No. It calls models and moves embeddings into the stores you already run. It isn't a model runtime
or a vector database.

**Do I need a JVM or Maven?**
No. You need Rust to build it, and nothing at run time.

**Why do management statistics default differently from Camel?**
Camel collects statistics only when `camel-management` is loaded. Cameleer always has management,
so by default it keeps route and context counters and makes per-step timing opt-in
(`ManagementStatisticsLevel::Default`, or `camel.main.jmxManagementStatisticsLevel=Default`).

**How is it verified?**
- 2,300+ hermetic tests against embedded servers;
- live suites against real PostgreSQL, Redis, RabbitMQ, ActiveMQ, Mosquitto, vsftpd,
  Postfix/Dovecot and nginx;
- official clients: librdkafka, pymongo, the DataStax driver, boto3, the Azure SDK, gcloud and
  grpcio;
- emulators: moto, Azurite, fake-gcs-server and FerretDB.

See [docs/INTEROP.md](docs/INTEROP.md).

---

## Crates

| crate | contents |
|---|---|
| `cameleer-core` | `CamelContext`, `Exchange`/`Message`/`Value`, routes and the Rust DSL, EIPs, error handling, languages, data formats, core components |
| `cameleer-http` | HTTP/1.1 server and client, `platform-http`, `http`, REST DSL with OpenAPI, `netty` TCP/UDP |
| `cameleer-yaml` | YAML parser, Camel YAML DSL, XML DSL, route templates |
| `cameleer-protocols` | ftp, smtp/pop3/imap, websocket, mllp/hl7, syslog (with embedded test servers) |
| `cameleer-tls` | from-scratch TLS 1.3/1.2, X.509, `SslContextParameters` (used by all network components) |
| `cameleer-kafka-mqtt` | kafka, paho / paho-mqtt5 (with embedded test brokers) |
| `cameleer-amqp` | spring-rabbitmq / rabbitmq (AMQP 0-9-1), stomp, jms / activemq / amqp-stomp (with embedded test brokers) |
| `cameleer-amqp10` | AMQP 1.0 client (SASL, TLS, CBS), amqp (JMS over AMQP 1.0), azure-servicebus, azure-eventhubs |
| `cameleer-sql` | embedded SQL engine, PostgreSQL client, sql / jdbc components, JDBC repositories |
| `cameleer-cloud` | AWS (incl. Bedrock), Azure (incl. OpenAI), Google Cloud (incl. Vertex AI), with verifying emulators |
| `cameleer-ssh` | SSH-2 + SFTP: sftp and ssh components |
| `cameleer-grpc` | HTTP/2 + HPACK, protobuf, grpc component and protobuf data format |
| `cameleer-datastores` | spring-redis, mongodb, elasticsearch/opensearch, cql (Cassandra), couchdb (with embedded test servers) |
| `cameleer-xml` | XML 1.0 parser + DOM, XPath 1.0, XSLT 1.0, XML Schema validation, `xj`, `xmlsecurity-sign`/`-verify` |
| `cameleer-saas` | openai, slack, telegram, github, twilio, jira, webhook, graphql, rss/atom (with embedded mock servers) |
| `cameleer-main` | `Main` runner (application.properties, route discovery) and the `camel` CLI |
| `cameleer-bench` | benchmark harness, sampling profiler and HTTP load generator |

Internals: [ARCHITECTURE.md](ARCHITECTURE.md).

## Build, test and check the numbers yourself

```text
cargo build --release
cargo test --workspace          # hermetic: every network component runs against embedded servers
scripts/bench.sh                # Cameleer vs Apache Camel (Linux or Git Bash; .\scripts\bench.ps1 on Windows)
```

The live interop suites run against real servers and are opt-in:

```text
sudo scripts/live-interop.sh all
CAMEL_LIVE_INTEROP=1 cargo test --workspace live_
```

## Contributing, security and citing

Contributions are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md) and the
[Code of Conduct](CODE_OF_CONDUCT.md). Report vulnerabilities privately as described in
[SECURITY.md](SECURITY.md). To cite Cameleer, use the "Cite this repository" button
([CITATION.cff](CITATION.cff)).

## License and credits

Cameleer is licensed under the [Apache License 2.0](LICENSE); see [NOTICE](NOTICE).

It exists because of [Apache Camel](https://camel.apache.org/)
([github.com/apache/camel](https://github.com/apache/camel)) and its community, whose design,
documentation and Enterprise Integration Patterns catalogue it follows. Apache, Apache Camel and
Camel are trademarks of the Apache Software Foundation; Cameleer is not affiliated with or
endorsed by the ASF.
