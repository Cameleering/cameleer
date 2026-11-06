# Changelog

All notable changes to Cameleer. Versions follow [Semantic Versioning](https://semver.org/); before
1.0, a minor version can change behaviour, and each such change is listed under **Changed**.

## 0.4.0 (2026-11-06)

Measured against Apache Camel with the same routes, Cameleer starts 450 times faster (3 ms vs
1.1 s), uses up to 54 times less memory and handles 1.7 to 8.7 times the throughput, across
routing, REST APIs, Kafka, PostgreSQL and batch jobs
([comparison](https://pmsudhi.github.io/cameleer/compare/apache-camel-vs-cameleer/),
[report](docs/BENCHMARKS.md)). This release also adds a built-in AWS Lambda runtime and builds
that contain only the components your routes use.

### Added

- **AWS Lambda custom runtime** in `cameleer run`, for the `provided.al2023` runtime. API
  Gateway, function URL and ALB events go to your `platform-http` routes in process, without a
  socket; other events go to `direct:lambda` or the endpoint named in the function's Handler.
  Startup failures go to `/init/error` and failed exchanges to `/error`. Package a function
  with `scripts/package-lambda.sh`. See [AWS Lambda](docs/DEPLOYMENT.md#aws-lambda).
- **Build features.** Every component family is a cargo feature of `cameleer-main` (`amqp`,
  `amqp10`, `cloud`, `dataformats`, `datastores`, `grpc`, `kafka-mqtt`, `protocols`, `saas`,
  `script`, `soap`, `sql`, `ssh`, `xml`), and so are five smaller parts: `templates`, `jq`,
  `xpath`, `http2` and `validate`. The default, `full`, includes everything; `core` includes
  none. The Dockerfile takes `--build-arg FEATURES=...`. A route that uses a left-out
  component fails at startup with the feature to add. Every component, language and data
  format page names its feature. See the [build features guide](docs/guides/build-features.md).
- **Startup recorder:** `camel.main.startupRecorder=logging` logs each startup step with its
  offset and duration, like Camel's `StartupStepRecorder`.
- `cameleer-bench coldstart` measures spawn-to-ready, spawn-to-exit and peak memory of the CLI.
- Guide: [AI agent tool calls, direct or through Cameleer](docs/guides/agent-tool-calls.md), with a
  tested saga route in `examples/agents/`.
- **A reproducible cloud benchmark** against Apache Camel on the JVM and on Quarkus native:
  `scripts/cloud-bench` creates three cloud machines, runs every scenario (in-process routing,
  REST APIs, HTTP into Kafka and PostgreSQL, Kafka stream processing, CSV batch) and deletes
  them. Raw results are in `benchmarks/results/`.
- Comparison pages on the website: Apache Camel vs Cameleer, Apache Camel alternatives, and AI
  agent tool calls made directly or through Cameleer.

### Changed

- **A feature list now leaves out the template engines, jq, the core XPath subset, HTTP/2 and
  `cameleer validate`.** If you built with a feature list in 0.3 (for example `--features
  cloud`), add the parts you use: `--features cloud,jq,validate`. The default build is
  unchanged. Rust programs that depend on `cameleer-core` or `cameleer-http` keep these parts
  through the crates' default features.
- **Timer routes that share a timer name share one thread**, as Camel keeps one `java.util.Timer`
  per name: 20 routes on one name use 1 thread instead of 20. A slow route delays the others on
  its name, as in Camel. Routes with different names still get a thread each.
- **XQuery and XSLT run on the caller's thread** and move to a larger stack only when they
  need it: an ordinary evaluation is 10 to 35 times faster (for example 142 µs to 6.5 µs for
  a small XQuery on Linux). XQuery recursion 2000 levels deep is about 30% slower on Linux.
- **Thread pools start their workers on demand** and idle workers exit after 60 s, like core
  threads in Java's `ThreadPoolExecutor`. An idle HTTP route uses 2 threads instead of 34.
  `ThreadPool::prestarted` keeps the old behaviour.
- The supervising route controller's thread exits when no route is left to retry.
- The local status file for `cameleer ps` is rewritten only when its content changes, and
  `cameleer ps` checks that a process is alive through the operating system. No status files
  are written on Lambda, Cloud Run, Knative or Azure Functions.
- Smaller and leaner binaries: the `dist` profile uses fat LTO, log macros format only enabled
  records, the allocator carves memory lazily, and the amd64 image uses packed relocations.
  The full binary is 23.9 MB (was 24.9 MB); the `core` build is 5.8 MB. An HTTP route uses
  about 10 MB of memory (was 15 MB) and is ready in about 1.2 ms on Linux (was 3.3 ms).

### Fixed

- **HTTP server keep-alive under load.** The server had 32 worker threads and closed keep-alive
  connections while all were busy, so more than 32 concurrent clients reconnected on every
  request. The default is now 200 workers, started on demand; REST throughput roughly doubled.
- **Kafka producer throughput.** Concurrent sends waited in line one record at a time. Records
  sent while another send is in flight are now batched into one produce request per broker,
  as Kafka's own producer does; each exchange still waits for its own acknowledgement.
- **PostgreSQL connection pool.** `poolSize` only limited idle connections, so a burst opened
  extra connections (each with a full login) and closed them again. It is now a hard maximum,
  like HikariCP's `maximumPoolSize`; a caller waits up to `poolTimeout` seconds (default 30)
  for a free connection.
- **TLS:** a server that rejected a client certificate could lose its alert to a TCP reset on
  Windows, so the client saw "connection reset" instead of `certificate_required`. Servers now
  close gracefully after a fatal alert. Code that calls `TlsStream::accept` on a raw
  `TcpStream` should use `cameleer_tls::net::accept_tls`.
- The run loop stops as soon as `camel.main.duration-max-messages` is reached, instead of up to
  20 ms later.

## 0.3.0 (2026-11-05)

### Added

- `cameleer validate`: checks YAML and XML route files for syntax, DSL structure, endpoint
  options, expressions, unresolved placeholders, dangling endpoints and duplicate route ids,
  without starting a route. Text or JSON output; exit code 0, 1 or 2.
  See [validate](docs/guides/validate.md).
- `cameleer schema`: a JSON Schema (draft 2020-12) of the YAML DSL for editors and agents.
- A complete [YAML DSL reference](docs/guides/yaml-dsl.md), a
  [YAML guide for coding agents](docs/guides/yaml-for-coding-agents.md) and `AGENTS.md`.

## 0.2.1 (2026-11-05)

### Changed

- The CLI is `cameleer` only. The `camel` binary is gone, since `camel` is Apache Camel's own CLI
  name; the subcommands are unchanged.

### Fixed

- Suspending a polling route waits for a poll in progress, as Apache Camel does.
- `mongodb-gridfs` no longer skips a file stored in the same millisecond as the previous one.
- Property values like `camel.server.port={{env:PORT:8080}}` resolve.
- `cameleer get health` exits with status 1 when the integration is DOWN.
- Status files are best-effort, so a read-only filesystem no longer stops the integration;
  `CAMEL_LOCAL_STATUS=false` turns them off.

## 0.2.0 (2026-11-05)

### Changed

- **rust-camel is now Cameleer.** Crates are `cameleer-*`, module paths `cameleer_*`, and the
  repository is `pmsudhi/cameleer`.

### Added

- The container image `pmsudhi/cameleer` on Docker Hub and `ghcr.io/pmsudhi/cameleer`, for
  linux/amd64 and linux/arm64.
- 19 crates on crates.io.
- Tested real-world examples in `examples/`.

## 0.1.0 (2026-11-05)

The first public release: Apache Camel's routing engine, every EIP, about 230 components,
languages and data formats, the Rust, YAML and XML DSLs, the REST DSL, Kamelets, route
templates, management, health checks and a Camel-JBang-style CLI, written in Rust on the
standard library alone.
