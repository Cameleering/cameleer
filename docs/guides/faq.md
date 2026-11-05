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

On the published benchmark (AMD Ryzen 5 5600X, Windows 11) against Apache Camel 4.23 on Java 21,
Cameleer is 4.27x faster on a `direct` route (4.96 million messages per second), 8.37x on seda
request-reply, 3.48x on a content-based router, 3.18x on JSON split and aggregate, and 2.45x on
an HTTP consumer. All six scenarios, including the three under 4x, are in
[BENCHMARKS.md](../BENCHMARKS.md).

### How much memory does it use?

A route uses about 10 MB: peak memory was 9 to 15 MB across the benchmark scenarios, against 263
to 491 MB for Camel on the JVM (25 to 46 times less). See [BENCHMARKS.md](../BENCHMARKS.md).

### How long does it take to start?

About 10 ms from process start to routes running, against 751 ms for Camel on the JVM (77 times
faster), which makes scale-to-zero and serverless practical.

### How big is the Docker image?

The `pmsudhi/cameleer` image is under 20 MB to download: one native binary on a distroless base.
See the [deployment guide](../DEPLOYMENT.md).

## Running it

### How do I install Cameleer?

Run the Docker image `pmsudhi/cameleer`, install the CLI with `cargo install cameleer-main`, or add
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
the container image as it is ([deployment guide](../DEPLOYMENT.md)). A native AWS Lambda adapter
is on the [roadmap](../ROADMAP.md).

### Does it run on Windows and macOS?

Yes. The CLI builds and runs on Linux, macOS and Windows (the test suite passes on Windows 11);
the Docker image is Linux (amd64 and arm64).

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

Send the prompt as the message body to `openai:chat-completion?model=...` (OpenAI or any
OpenAI-compatible server such as Ollama or vLLM), `aws-bedrock`, `azure-openai` or
`google-vertexai`; the answer becomes the body, and you route on it with `choice`. See
[Cameleer for AI agents](agents.md).

### Can an AI agent call Cameleer routes as tools?

Yes. Expose a route over REST and add JSON schema validation, an idempotent consumer, retries
and an audit log, as in the [tool API pattern](agents.md#expose-a-tool-to-an-agent-over-rest).
A packaged MCP server is planned ([roadmap](../ROADMAP.md)).

### Does Cameleer run models or store embeddings?

No. It calls model APIs and writes embeddings to the stores you run, such as Elasticsearch,
OpenSearch or PostgreSQL with pgvector. See [RAG ingestion](agents.md#rag-ingestion-chunk-embed-and-store).

## Quality and support

### Is Cameleer production ready?

The feature list is complete against its 228-row parity table, with 2,300+ hermetic tests and
verification against real servers, official clients and emulators ([INTEROP.md](../INTEROP.md)).
Two caveats: it is young (version 0.2), and its from-scratch TLS and crypto code has not had an
independent security audit yet ([STATUS.md](../STATUS.md)). Evaluate it on your own workload.

### How is it tested?

With 2,300+ hermetic tests against embedded servers, live suites against real PostgreSQL, Redis,
RabbitMQ, ActiveMQ, Mosquitto, vsftpd, Postfix/Dovecot and nginx, official clients (librdkafka,
pymongo, the DataStax driver, boto3, the Azure SDK, gcloud, grpcio) and emulators (moto, Azurite,
fake-gcs-server, FerretDB). Every example in these guides runs in the test suite.

### Why are there no third-party dependencies?

The project was written with only the Rust standard library: TLS, HTTP/2, gRPC, the Kafka, AMQP,
MQTT, PostgreSQL, Redis, MongoDB and Cassandra protocols, XML and JSON are implemented from
scratch. There is no dependency tree to audit or patch, and builds need no downloads.

### How do I report a bug or a security issue?

Open an issue on [GitHub](https://github.com/pmsudhi/cameleer/issues) for bugs; report
vulnerabilities privately as described in [SECURITY.md](../../SECURITY.md).

## Related

- [Getting started](getting-started.md), [concepts](concepts.md), [EIPs](eips.md)
- [Migrating from Apache Camel](migrating-from-apache-camel.md)
- [Benchmarks](../BENCHMARKS.md), [features](../FEATURES.md), [roadmap](../ROADMAP.md)
