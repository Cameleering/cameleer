# rust-camel

### Apache Camel, rewritten in Rust. If you know Camel, you already know rust-camel.

The same endpoint URIs. The same Enterprise Integration Patterns. The same Simple expressions, the
same YAML routes, the same headers and the same error handling. About 230 components, languages and
data formats, ported one by one and verified against the real systems.

**But rebuilt for the agent era:** a single native binary that starts in **10 ms**, runs in about
**10 MB**, and routes messages **4–8x faster** than Camel on the JVM.

[Quick start](#quick-start) · [Benchmarks](#rebuilt-for-speed-memory-and-scale) ·
[Agents](#rebuilt-for-agents) · [Serverless](#rebuilt-for-serverless-and-the-edge) ·
[Roadmap](docs/ROADMAP.md) · [FAQ](#faq)

---

## Rebuilt for speed, memory and scale

The same routes run on both, measured on the same machine against Apache Camel 4.23 on Java 21:

```text
Startup (spawn → routes running)                      lower is better
  rust-camel  ▏ 10 ms
  Camel       ████████████████████████████████████████ 751 ms              77x faster

Memory (peak, direct route)                           lower is better
  rust-camel  █ 9 MB
  Camel       ████████████████████████████████████████ 366 MB              42x less

Throughput, direct route                              higher is better
  rust-camel  ████████████████████████████████████████ 4.96M msgs/s
  Camel       █████████▍ 1.16M msgs/s                                      4.3x

Throughput, seda request-reply                        higher is better
  rust-camel  ████████████████████████████████████████ 2.09M msgs/s
  Camel       ████▊ 250k msgs/s                                            8.4x

p99 latency, request-reply                            lower is better
  rust-camel  ██████▋ 5.1 µs
  Camel       ████████████████████████████████████████ 30.6 µs             6x lower
```

| scenario | rust-camel | Apache Camel | faster by |
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
written for Apache Camel and for rust-camel:

<table>
<tr><th>Apache Camel (Java)</th><th>rust-camel</th></tr>
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

That's what Camel has done for enterprises for years. rust-camel does it for agents, as a library
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
- **Pack tens of services onto one node.** At about 10 MB each, rust-camel fits on edge gateways
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

```rust
use camel_core::prelude::*;

fn main() -> Result<()> {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:hello?period=200")
            .route_id("hello")
            .set_body(simple("Hello from rust-camel #${exchangeProperty.CamelTimerCounter}"))
            .filter(simple("${exchangeProperty.CamelTimerCounter} > 2"))
                .to("log:greetings")
            .end();
    })?;
    ctx.start()?;
    std::thread::sleep(std::time::Duration::from_millis(2000));
    ctx.stop()
}
```

An agent-style route in YAML, which classifies tickets with a model and routes on the answer:

```yaml
- route:
    id: triage
    from:
      uri: kafka:support-tickets
      steps:
        - to: "openai:chat-completion?model=gpt-4o-mini&systemMessage=Answer URGENT or ROUTINE only"
        - choice:
            when:
              - simple: "${body} contains 'URGENT'"
                steps:
                  - to: kafka:tickets-urgent
            otherwise:
              steps:
                - to: kafka:tickets-routine
```

```text
$ cargo run --release -p camel-main --bin camel -- run triage.camel.yaml
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
Camel collects statistics only when `camel-management` is loaded. rust-camel always has management,
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
| `camel-core` | `CamelContext`, `Exchange`/`Message`/`Value`, routes and the Rust DSL, EIPs, error handling, languages, data formats, core components |
| `camel-http` | HTTP/1.1 server and client, `platform-http`, `http`, REST DSL with OpenAPI, `netty` TCP/UDP |
| `camel-yaml` | YAML parser, Camel YAML DSL, XML DSL, route templates |
| `camel-protocols` | ftp, smtp/pop3/imap, websocket, mllp/hl7, syslog (with embedded test servers) |
| `camel-tls` | from-scratch TLS 1.3/1.2, X.509, `SslContextParameters` (used by all network components) |
| `camel-kafka-mqtt` | kafka, paho / paho-mqtt5 (with embedded test brokers) |
| `camel-amqp` | spring-rabbitmq / rabbitmq (AMQP 0-9-1), stomp, jms / activemq / amqp-stomp (with embedded test brokers) |
| `camel-amqp10` | AMQP 1.0 client (SASL, TLS, CBS), amqp (JMS over AMQP 1.0), azure-servicebus, azure-eventhubs |
| `camel-sql` | embedded SQL engine, PostgreSQL client, sql / jdbc components, JDBC repositories |
| `camel-cloud` | AWS (incl. Bedrock), Azure (incl. OpenAI), Google Cloud (incl. Vertex AI), with verifying emulators |
| `camel-ssh` | SSH-2 + SFTP: sftp and ssh components |
| `camel-grpc` | HTTP/2 + HPACK, protobuf, grpc component and protobuf data format |
| `camel-datastores` | spring-redis, mongodb, elasticsearch/opensearch, cql (Cassandra), couchdb (with embedded test servers) |
| `camel-xml` | XML 1.0 parser + DOM, XPath 1.0, XSLT 1.0, XML Schema validation, `xj`, `xmlsecurity-sign`/`-verify` |
| `camel-saas` | openai, slack, telegram, github, twilio, jira, webhook, graphql, rss/atom (with embedded mock servers) |
| `camel-main` | `Main` runner (application.properties, route discovery) and the `camel` CLI |
| `camel-bench` | benchmark harness, sampling profiler and HTTP load generator |

Internals: [ARCHITECTURE.md](ARCHITECTURE.md).

## Build, test and check the numbers yourself

```text
cargo build --release
cargo test --workspace          # hermetic: every network component runs against embedded servers
scripts/bench.sh                # rust-camel vs Apache Camel (Linux or Git Bash; .\scripts\bench.ps1 on Windows)
```

The live interop suites run against real servers and are opt-in:

```text
sudo scripts/live-interop.sh all
CAMEL_LIVE_INTEROP=1 cargo test --workspace live_
```
