# Cameleer FAQ

> Short, factual answers to the questions people ask about Cameleer: what it is, how it relates
> to Apache Camel, how fast it is, where it runs, and how to use it with Kafka, LLMs and edge
> devices.

## About Cameleer

### What is Cameleer?

Cameleer is Apache Camel rewritten in Rust: an integration framework with the same routes,
Enterprise Integration Patterns, endpoint URIs, Simple language and YAML DSL, shipped as one
native binary with no dependencies outside the Rust standard library. See the
[README](../../README.md) and [concepts](concepts.md).

### What is cameleering?

Cameleering is the practice of making software carry the same load on less (less time, memory,
size and dependencies) and proving every gain against the original, on the same hardware, with the
method published. Its one hard rule is no claim without a number. Cameleer is the first Cameleering
project: Apache Camel's routes run by one native Rust binary that starts in about 3 ms and runs in
megabytes. The word comes from *cameleer*, the driver of a camel caravan. See
[what is cameleering?](https://cameleer.ing/) and
[the ledger](https://cameleer.ing/ledger/).

### Is Cameleer compatible with Apache Camel?

It follows Camel 4's names and semantics: Camel YAML and XML routes, URIs, options, EIPs, Simple
expressions and `Camel*` headers work unchanged for the supported components. Java code
(processors, beans, custom components) does not run; it is rewritten in Rust. See
[migrating from Apache Camel](migrating-from-apache-camel.md).

### Is Cameleer affiliated with the Apache Software Foundation?

No. It is an independent project that follows Apache Camel's design and documentation but
contains no Camel source code, and it is not endorsed by the ASF. Apache Camel is a trademark of
the ASF.

### What was Cameleer called before?

rust-camel, up to version 0.1.0.

### What license does Cameleer use?

The Apache License 2.0 ([LICENSE](../../LICENSE)).

## Performance and footprint

### How fast is Cameleer?

With the same routes, Cameleer handles 1.7 to 8.7 times the throughput of Apache Camel 4.23 on
Java 21 in every scenario measured: 2.1x on a REST API with JSON, 2.3x writing to PostgreSQL,
1.7x publishing to Kafka, 8.7x on Kafka stream processing and 3.0x on a content-based router.
Every scenario, the method and the raw data are on the
[Apache Camel vs Cameleer](https://cameleer.ing/cameleer/compare/apache-camel-vs-cameleer/)
page and in [BENCHMARKS.md](../BENCHMARKS.md).

### How much memory does it use?

A route uses about 10 MB: 8 MB at startup, and 8 to 68 MB at peak across the benchmark scenarios,
against 213 to 440 MB for Apache Camel on the JVM (up to 54 times less). See
[BENCHMARKS.md](../BENCHMARKS.md).

### How long does it take to start?

3 ms from process start to routes running in the benchmark, against 1.1 s for Apache Camel on
the JVM and 30 ms compiled natively with Quarkus. A function build with only the components it
needs starts in under 1 ms. That makes scale-to-zero and serverless practical
([BENCHMARKS.md](../BENCHMARKS.md#faas-cold-start-and-size)).

### How big is the Docker image?

The `cameleering/cameleer` image is about 20 MB to download: one native binary on a distroless base. Built with only the
component families you need it is smaller: 14.5 MB with `FEATURES=cloud`, 12.5 MB with
`FEATURES=core`.
See the [deployment guide](../DEPLOYMENT.md).

## Running it

### How do I install Cameleer?

Run the Docker image `cameleering/cameleer`, install the CLI with `cargo install cameleer-main`, or add
the `cameleer-core` crate (plus component crates) to a Rust project. See
[getting started](getting-started.md).

### Do I need Java, Maven or a JVM?

No. Cameleer is a native binary. Rust is needed only to build it from source or to embed it in a
Rust program.

### Can I run it on a Raspberry Pi?

Yes, on a 64-bit OS: the image is published for `linux/arm64` (Raspberry Pi 3, 4 and 5 with
64-bit Raspberry Pi OS), and a route uses about 10 MB of memory. There is no 32-bit ARM image.
See [Cameleer at the edge](edge.md#small-footprint-deployment).

### Does it run on Kubernetes, Cloud Run or AWS Lambda?

Kubernetes, Docker Compose, Google Cloud Run, Azure Container Apps, Knative and ECS/Fargate run
the container image as it is ([deployment guide](../DEPLOYMENT.md)). On AWS Lambda, `cameleer`
is its own custom runtime for `provided.al2023`: API Gateway and function URL events go to your
`platform-http` routes, other events to `direct:lambda`
([AWS Lambda](../DEPLOYMENT.md#aws-lambda)).

### Does it run on Windows and macOS?

Yes. The CLI builds and runs on Linux, macOS and Windows (the test suite passes on Windows 11);
the Docker image is Linux (amd64 and arm64). On Windows, set `SSL_CERT_FILE` to a PEM bundle of
CA certificates before calling HTTPS or TLS services on the internet: Cameleer does not read the
Windows certificate store (see the next question).

### Which CA certificates does Cameleer trust?

Unless an endpoint names its own trust store (`sslContextParameters`, `trustStore`, the global
`camel.ssl.*` parameters with `camel.ssl.enabled=true`), Cameleer uses the system trust store:
the file named by the `SSL_CERT_FILE` environment variable, else the first bundle found at
`/etc/ssl/certs/ca-certificates.crt`, `/etc/pki/tls/certs/ca-bundle.crt`,
`/etc/ssl/ca-bundle.pem`, `/etc/ssl/cert.pem` or
`/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem`. The Docker image and most Linux
distributions have one of these, and macOS has `/etc/ssl/cert.pem`. Windows has none, so set
`SSL_CERT_FILE` there (for example to a `cacert.pem` bundle). Certificates are always checked
unless you set Camel's `trustAllCertificates=true` (in `camel.ssl.*` or an `SSLContextParameters`
bean), which is meant for tests only.

### Can it run with a read-only filesystem?

Yes. The container writes nothing unless a route writes files, and it works with `--read-only`
and Kubernetes' `readOnlyRootFilesystem: true`.

### How do I check that it is healthy?

Start it with `--prop camel.server.enabled=true --prop camel.server.healthCheckEnabled=true` and
probe `/q/health/live` and `/q/health/ready` on port 8080, or run
`cameleer get health --url http://localhost:8080`.

## Features

### Which Enterprise Integration Patterns does it support?

All the patterns of Apache Camel's EIP catalogue, including split, aggregate, multicast,
recipient list, routing slip, dynamic router, wire tap, enrich, idempotent consumer, load
balancer, resequencer, throttle, circuit breaker, saga and transacted. See [EIPs](eips.md).

### Does it support Kafka?

Yes. The `kafka` component has its own Kafka wire protocol client: producer and consumer groups,
keys and partitions, offsets and commits, TLS and SASL (PLAIN, SCRAM, AWS MSK IAM). It is
verified with librdkafka, kcat and the kfake Kafka simulator. See the
[kafka component](../components/kafka.md).

### Does it support MQTT?

Yes: `paho` (MQTT 3.1.1) and `paho-mqtt5` (MQTT 5.0) with QoS 0 to 2, retained messages, wills,
persistent sessions and TLS, tested against Mosquitto, RabbitMQ and ActiveMQ. See
[the edge cookbook](edge.md).

### Which databases and clouds does it connect to?

PostgreSQL (`sql`, `jdbc`), Redis, MongoDB, Elasticsearch and OpenSearch, Cassandra and CouchDB,
plus all of Apache Camel's AWS, Azure and Google Cloud components (S3, SQS, SNS, DynamoDB,
Lambda, Bedrock, Blob Storage, Service Bus, Event Hubs, Cosmos DB, Cloud Storage, Pub/Sub,
BigQuery, Vertex AI and more). See [FEATURES.md](../FEATURES.md).

### Does it have every Camel component?

No. It has about 230 components, languages and data formats, chosen from the most used ones, but
not Camel's long tail of vendor connectors. [FEATURES.md](../FEATURES.md) lists what is there.

### Can I write routes in YAML, XML or code?

All three: Camel's YAML DSL, Camel's XML DSL, and a Rust DSL equivalent to Camel's Java DSL.
They support the same EIPs. See [concepts](concepts.md#the-three-dsls).

### Does it support REST APIs and OpenAPI?

Yes. The REST DSL (`rest:` in YAML, `b.rest(...)` in Rust) declares endpoints with path and query
parameters, JSON binding and CORS, and generates an OpenAPI 3 document.

## AI and agents

### How do I use Cameleer with an LLM?

Send the prompt as the message body to a model component: `openai:chat-completion?model=...`
(OpenAI or any OpenAI-compatible server such as Ollama or vLLM, with presets for 19 services),
`anthropic`, `gemini`, `aws-bedrock`, `azure-openai` or `google-vertexai`. The answer becomes
the body, and you route on it with `choice`. Give the component `tags` and it calls your tool
routes itself; the [`agent` step](agent-step.md) runs a whole tool-calling conversation as one
step. See [Cameleer for AI agents](agents.md) and [model providers](model-providers.md).

### Can an AI agent call Cameleer routes as tools?

Yes. Make the route an [`ai-tool`](../components/ai-tool.md) and serve it with the
[MCP server](mcp-server.md) (`cameleer run --mcp`, or `--mcp-stdio` for Claude Desktop): every
call gets schema checks, approvals, deduplication, timeouts and an audit record. Start with
[MCP](mcp.md). For agents that call plain HTTP, the
[tool API pattern](agents.md#expose-a-tool-to-an-agent-over-rest) does the same over REST.

### Does Cameleer run models or store embeddings?

No. It calls model APIs and writes embeddings to the stores you run, such as Elasticsearch,
OpenSearch or PostgreSQL with pgvector. See [RAG ingestion](agents.md#rag-ingestion-chunk-embed-and-store).

## Quality and support

### Is Cameleer production ready?

The feature list is complete against its parity table (every row ✅), with 2,800+ hermetic tests and
verification against real servers, official clients and emulators ([INTEROP.md](../INTEROP.md)).
Two caveats: it is young (version 0.6), and its from-scratch TLS and crypto code has not had an
independent security audit yet (it is on the [roadmap](../ROADMAP.md#25-verification-and-trust)). Evaluate it on your own workload.

### How is it tested?

With 2,800+ hermetic tests against embedded servers, live suites against real PostgreSQL, Redis,
RabbitMQ, ActiveMQ, Mosquitto, vsftpd, Postfix/Dovecot and nginx, official clients (librdkafka,
pymongo, the DataStax driver, boto3, the Azure SDK, gcloud, grpcio) and emulators (moto, Azurite,
fake-gcs-server, FerretDB). Every example in these guides runs in the test suite.

### Why are there no third-party dependencies?

The project was written with only the Rust standard library: TLS, HTTP/2, gRPC, the Kafka, AMQP,
MQTT, PostgreSQL, Redis, MongoDB and Cassandra protocols, XML and JSON are implemented from
scratch. There is no dependency tree to audit or patch, and builds need no downloads.

### How do I report a bug or a security issue?

Open an issue on [GitHub](https://github.com/cameleering/cameleer/issues) for bugs; report
vulnerabilities privately as described in [SECURITY.md](../../SECURITY.md).

## Related

- [Getting started](getting-started.md), [concepts](concepts.md), [EIPs](eips.md)
- [Migrating from Apache Camel](migrating-from-apache-camel.md),
  [What Cameleer does not do](limitations.md), [Troubleshooting](troubleshooting.md)
- [Benchmarks](../BENCHMARKS.md), [features](../FEATURES.md), [roadmap](../ROADMAP.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
