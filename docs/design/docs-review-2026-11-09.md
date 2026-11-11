# Documentation review, 2026-11-09

Three read-only reviews of `docs/` against the code: reference completeness (CLI, properties,
YAML DSL, components, Rust API, errors), "how do I X?" coverage (about 115 search-style
questions), and the learning path plus answer-engine readiness. The findings marked
**verified** were re-checked against the code by hand. Internal: `docs/design/` is not
published.

## Summary

Single-component questions are answered well: every component, language and data format has a
page, and the 22 transport pages checked all have a YAML example, a Rust block, a Docker command
and an FAQ. The gaps are the topics that cut across components, and they are exactly what people
and agents ask first: configuration, the CLI, TLS and authentication, observability, testing,
Rust as a library, troubleshooting, and multi-component recipes. A few pages are wrong.

By area, "how do I X?" questions answered: core routing 15/16, error handling 7/8,
transformation 9/10, files 8/10, HTTP/REST 8/14, messaging 13/16, databases 6/9, cloud storage
5/5, scheduling 4/5, AI/MCP 7/7, edge 5/5, deployment 6/9, **configuration and secrets 2/7,
observability 2/7, testing 2/5, migration 3/7, Rust embedding 2/7**.

## 1. Wrong or misleading (fix first)

**Done 2026-11-09** (after merging `main` with 0.5.0 and 0.5.1): Kafka TLS options and a mutual
TLS FAQ; `camel.ssl.enabled=true` on every global-TLS mention; the `cameleer export` caveat in
getting-started; the trust store and `SSL_CERT_FILE` in the FAQ (and `trustAllCertificates`);
`concepts.md` rows; a note on INTEROP's dated counts; the missing headers and options on five
component pages; `STATUS.md` no longer published (links now go to the roadmap and GitHub); the
Read the Docs canonical points at `https://cameleer.ing/docs/` and its toctree lists every guide;
guides ordered by path; `llms.txt` with one documentation section, a project-pages section on
the site, the full CLI list, the counts and the page URL pattern. Not a finding after all: the
MCP server table does list `mcp-server-description` and `mcp-server-website-url`.

| where | says | true (evidence) |
|---|---|---|
| `docs/components/kafka.md:115,120` | Java keystores and client certificates are not supported | **verified**: PKCS12, JKS and PEM key and trust stores are accepted (`crates/cameleer-kafka-mqtt/src/kafka/mod.rs:175-187`); `sslKeystoreLocation`, `sslKeystorePassword`, `sslKeyPassword`, `sslKeystoreType`, `sslTruststorePassword`, `sslTruststoreType`, `sslEndpointAlgorithm`, `sslEnabledProtocols` undocumented; module comment at mod.rs:39 is stale too |
| `docs/components/ftps.md:43,95` and every "global `camel.ssl.*`" mention | set `camel.ssl.trustStore` | **verified**: `camel.ssl.*` applies only with `camel.ssl.enabled=true` (`crates/cameleer-tls/src/camel_support.rs:97`) |
| `docs/guides/getting-started.md:201` | `cameleer export` makes a standalone project | **verified**: the generated `Cargo.toml` has a path dependency to the build machine's crate directory (`cli.rs:447-482`); exports from the Docker image or a crates.io install don't build; `--camel-path` undocumented. Also a code issue |
| (nowhere) | | **verified**: the system trust store is `SSL_CERT_FILE` or Linux bundle paths only (`crates/cameleer-tls/src/x509.rs:360-395`); on Windows HTTPS to public hosts has no roots unless `SSL_CERT_FILE` is set. Document it; reading the Windows store is a code follow-up |
| `docs/guides/concepts.md:241` | "Camel JBang (`cameleer run`) \| `cameleer run`" | rename artefact: the Camel side is `camel run` |
| `docs/guides/concepts.md:242` | every component built in | components are cargo features (`build-features.md`); the default build has all |
| `docs/INTEROP.md:30,251,343` | 72 live / 34 official / 31 simulator tests | STATUS: 74 / 58 / 35 |
| `docs/STATUS.md` | published as `docs/STATUS.html`, in the sitemap and llms-full | **verified**: internal (branches, todos); `INTERNAL` in `scripts/build-docs.py:42` has only `website-plan.md` |
| `docs/conf.py:43` | Read the Docs canonical `cameleer.readthedocs.io` | **verified**: duplicates compete with cameleer.ing; the RTD toctree also omits the MCP and agent guides |
| `scripts/build-docs.py:63` `GUIDE_ORDER` | | **verified**: matches file stems, so `guides/mcp/deployment.md` is listed 6th of all guides; names guides that don't exist (`rust-dsl`, `rest-dsl`, `kamelets`, `testing`); `validate` and `yaml-for-coding-agents` sort last |
| `llms.txt` | | two link sections ("Documentation" and "Docs"); the CLI list omits `test`, `tool`, `cmd`, `log`; "languages (26)" while 22 pages exist |
| `docs/components/rabbitmq.md`, `paho-mqtt5.md`, `http.md`, `elasticsearch.md`, `azure-servicebus.md` | | missing headers/options: `CamelRabbitmqMandatory`, `CamelRabbitmqContentEncoding`, `CamelRabbitmqClusterId`; `CamelPahoMqtt5ContentType`; `proxyAuthScheme`, `proxyAuthMethod`; `snifferInterval`; `CamelAzureServiceBusSessionState` |
| `docs/guides/mcp-server.md` | | `camel.server.mcp-server-description` and `mcp-server-website-url` are read but not in the table |

## 2. Missing cross-cutting guides (biggest gain for people, agents and LLMs)

**Done 2026-11-09:** items 1 and 2, `docs/guides/configuration.md` and `docs/guides/cli.md`, with
their worked examples and the CLI command and flag list checked by
`crates/cameleer-main/tests/docs_reference.rs`; linked from the docs index, getting started, the
Read the Docs toctree and llms.txt.
**Done 2026-11-11:** items 6, 7 and 8, `guides/rust.md` (worked example: a custom component, a
processor and an aggregation strategy used from YAML, run and compared with the page by
`tests/rust_guide/mod.rs`), `guides/troubleshooting.md` (every quoted message checked against the
binary, the kind table against `error.rs`) and `guides/limitations.md` (the missing-component
table checked against the build). The docs harness now skips examples whose loader error is a
bean bound from Rust. Code finding, fixed in 6a8acca: `builtin_parent` gave
`CamelExchangeException` itself as its parent, so `onException`/`doCatch` for `Exception` or
`RuntimeException` missed `HttpOperationFailedException`, `DirectConsumerNotAvailableException` and
every other kind under it (now `Exception`, as in Camel; `troubleshooting_kinds_match_the_code`
checks the table).
Items 3, 4 and 5 done the same day: `guides/testing.md`, `guides/observability.md`,
`guides/tls-and-security.md`, each with a worked example run by the same test file (the TLS one
with Cameleer as both HTTPS server and client). Code findings from writing them, not fixed here:
`/q/metrics` sends two `Content-Type` headers; the readiness `consumers` check says DOWN while
its data says `consumer.<id>: UP` for a suspended route; the management server cannot share a
port with HTTPS routes (`camel.server.sslEnabled=true` plus `--console` fails with "Port is
already used by an https server"), and there is no separate management port; a `routeConfiguration`
or top-level intercept applies only to route files loaded after it (Camel applies them to all).

1. **`guides/configuration.md`**: sources and precedence; relaxed binding; `CAMEL_*` env mapping
   and `CAMEL_COMPONENT_<SCHEME>_*`; profiles (`--profile`, `%dev.key`, YAML on-profile); the
   Spring Boot and Quarkus aliases; every `camel.main.*` key (`name`, `log-level`,
   `shutdown-timeout`, `dev-reload`, `tracing`, `backlog-tracing`, `message-history`,
   `startup-recorder`, `route-controller-supervise-*`, `jmxManagementStatisticsLevel`,
   `duration-max-*`), `camel.server.*` (incl. `metricsEnabled`, `ssl.*`), `camel.ssl.*` (with the
   enabled flag), `camel.datasource.*`, `camel.beans.*`, `camel.component.*`; what a YAML-only user
   can configure without Rust; environment variables (`CAMEL_LOG_LEVEL`, `CAMEL_HOME`,
   `CAMEL_LOCAL_STATUS`, `SSL_CERT_FILE`, `TZ`/`TZDIR`, `CAMEL_DNS_SERVER`).
2. **`guides/cli.md`**: every command and flag: `run` (`--max-messages`, `--max-idle-seconds`,
   `--max-seconds`, `--log-level`, `--background`, `--profile`, `--trace`, `--port`, `--name`,
   `--dev`, `--console`, `--mcp`, what plain `run` picks up), `init`, `export` (`--camel-path`,
   the caveat), `bind` (`--step`, `--property`, `--error-handler`, `--output`), `ps`, `stop`,
   `get routes|health|endpoints`, `log [--follow] [--tail]`, `trace [--disable]`,
   `cmd suspend-route|resume-route|reset-stats|reload|start-route|stop-route`, `validate`,
   `schema`, `catalog`, `test`, `tool`, `version`; exit codes.
3. **`guides/testing.md`**: `cameleer test` for any route (not only agents); Rust tests with
   `ProducerTemplate`, `ConsumerTemplate`, `MockEndpoint`; intercepting external calls; the
   embedded test brokers; `cameleer validate` in CI.
4. **`guides/observability.md`**: health (`/q/health*`), Prometheus `/q/metrics`
   (`camel.server.metricsEnabled`) with a scrape config, the `/q/dev/*` consoles and route
   actions, statistics levels, log levels, `--trace`, message history, backlog tracing,
   `cameleer get|log|trace`; OpenTelemetry stated as not implemented.
5. **`guides/tls-and-security.md`**: server HTTPS (`camel.server.ssl.*`, PEM and keystore),
   client mTLS, custom CA, `camel.ssl.*` vs per-endpoint, one block per transport (http, kafka,
   jms, amqp, mqtt, sql, mongodb, redis, ftps), `SSL_CERT_FILE` on Windows; inbound auth for
   REST APIs (what exists, and the filter/`onException` pattern where nothing is built in);
   outbound OAuth2 client credentials (a token-caching route); CORS with an example; what is
   unsupported.
6. **`guides/rust.md`** (Rust developer guide): the Rust DSL beyond getting started; `Processor`;
   `ProducerTemplate`; binding processors, aggregation strategies and repositories for YAML
   (`register_bean_factory`, registry `bind`); a custom component (`spi.rs`, `add_component`);
   data format factories; `Main` API; `set_global_ssl_context_parameters`; the opt-in allocator;
   threads, not tokio; docs.rs links.
7. **`guides/troubleshooting.md`** plus an exception-kind table in `error-handling.md`: the ~38
   kinds and their hierarchy (`crates/cameleer-core/src/error.rs:12-73`); startup errors (unknown
   component or option, missing feature, unresolved placeholder, missing global SSL, "needs the
   ai feature"), TLS handshake failures, with causes and fixes; a "finding → fix" table in
   `validate.md`.
8. **`guides/limitations.md`** ("what Cameleer does not do"): OpenTelemetry, Excel, CDC and
   Debezium, PostgreSQL LISTEN/NOTIFY, clustering and leader election, Kafka OAUTHBEARER and
   Kerberos, Confluent Schema Registry, XA, OpenWire, Java beans. Answer engines need explicit
   negatives so they don't invent support.

## 3. Learning path and depth

- **A step-by-step tutorial** that builds one realistic integration (REST in, validate, Kafka or
  PostgreSQL, error handling, `cameleer test`, Docker, health), linked from `docs/README.md` row
  one and from getting-started. `docs/README.md` today links the `guides/` folder, not
  getting-started, concepts, yaml-dsl or error-handling.
- **Concepts** missing: transactions, the threading model (`direct` vs `seda`, `threads`,
  `parallelProcessing`), stream caching, route lifecycle and graceful shutdown, InOut vs InOnly;
  a glossary; the Camel-to-Cameleer table linked to pages.
- **Tested multi-component recipes**: SFTP to S3, CSV to PostgreSQL batch insert, HTTP to Kafka,
  Kafka to PostgreSQL upsert, poll an API into a database, mail attachments to S3.
- **Per-transport "errors and retries"** sections (http, platform-http, sql, jdbc, aws2-s3,
  aws2-sns, mongodb, spring-redis, paho-mqtt5, file never mention them) and a by-transport table
  in `error-handling.md`; a Kafka reliability section (at-least-once in YAML, DLQ topic and
  offsets, consumer groups, schema registry status).
- **Transformation hub** phrased as questions; **messaging** and **scheduling** guides
  (run-once jobs and Kubernetes CronJobs, `duration-max-messages`); **performance tuning**.
- **Migration**: a Java DSL / YAML / Rust side-by-side table; a Spring Boot walkthrough; rewriting
  processors and beans.
- **YAML reference gaps** (`crates/cameleer-yaml/src/model.rs`): no example for `inOnly`, `inOut`,
  `removeProperties`, `transformDataType`, `serviceCall`, `intercept`, `rollback`, `transacted`;
  Camel 4 `variableSend`/`variableReceive` on 8 steps undocumented; missing keys on aggregation
  strategies, `split`/`multicast` (`parallelAggregate`, `synchronous`), `aggregate`, route
  (`streamCaching`, `logMask`, ...), error handlers, `bean` factory keys, most of
  `restConfiguration`. EIPs without a worked example: Service Call, Transacted/Policy, Route
  Policy, Transformer/Validator, Rollback, supervising route controller, message history.
- **Deployment**: systemd and bare metal, Windows service, Helm/kustomize, CronJob, shutdown.

## 4. Answer-engine readiness

- Opening answers: present on all pages but DEPLOYMENT, FEATURES, INTEROP, ROADMAP, STATUS; the
  MCP pages' answers are 70-97 words, too long to be quoted (aim for 25-40).
- FAQs missing on 22 pages, all in the newest area: `ai-tool`, `ai-prompt`, `ai-resource`,
  component `mcp`, `mcp.md`, `mcp-server.md`, `mcp-client.md`, `mcp-shop-example.md`,
  `agent-step.md`, `model-providers.md`, the 10 `guides/mcp/*`, `DEPLOYMENT.md`, `BENCHMARKS.md`.
- `llms-full.txt` is about 2.0 MB (257 pages, roughly 500k tokens): too large for most context
  windows. Drop STATUS and INTEROP, cap FEATURES, and publish per-section files
  (`llms-guides.txt`, `llms-components.txt`, `llms-agents.txt`).
- `llms.txt`: one link section; the component URL pattern `https://cameleer.ing/docs/components/<scheme>.html`;
  the corrected counts and CLI list.
- Agent support: a `cameleer catalog component <scheme>` command that prints options and headers
  as JSON (and a data format listing) would let agents look options up offline (code
  follow-up); mention `cameleer schema` and the yaml-language-server modeline in `AGENTS.md` and
  `yaml-for-coding-agents.md`; a short generated "YAML reference card".
- 13 orphan pages (linked only from generated indexes): components `aws2-timestream`, `cql`,
  `google-firestore`, `google-functions`, `google-secret-manager`, `saga`; data formats `asn1`,
  `barcode`, `fory`, `ical`, `syslog`, `urlencoded`; language `datasonnet`. Only 38 of 163
  component pages are linked from a guide.
- The guides are lopsided: 17 of 36 are about AI and MCP, 4 are core.

## 5. Code follow-ups found (for the code session)

- `cameleer export` writes a path dependency to the build machine's source tree; from the image
  or crates.io it should depend on the published `cameleer-main` version.
- The system trust store ignores the Windows certificate store; only `SSL_CERT_FILE` or Linux
  bundles are read.
- `cameleer catalog component <scheme>` with options and headers as JSON (agents).
- Kafka module comment `kafka/mod.rs:39` says keystores are unsupported.
