# OpenTelemetry tracing: design

_2026-11-25. Status: phase 1 built (this document's "In scope now"); internal design note, not on the site._

A reader of the 0.8.0 announcement pointed out that Camel is more than components and routes:
observability, tracing, logging and error handling. Error handling and management were done
(`docs/guides/observability.md`, FEATURES rows 120, 127 to 129, 341). This note closes the
remaining gap with Camel: distributed tracing with OpenTelemetry, trace ids in logs, and JSON
log lines.

Sources: Apache Camel `main` (components `camel-tracing`, `camel-opentelemetry`,
`camel-telemetry`, `camel-opentelemetry2`, `camel-observation`, `camel-jbang`), the
OpenTelemetry specification and opentelemetry-proto, W3C Trace Context Level 1 and W3C Baggage.
The research notes, with a source path for every fact, are kept outside the repository.

## 1. What Camel does

### 1.1 The modules

| module | since | state | what it is |
|---|---|---|---|
| `camel-tracing` | 3.x | deprecated in 4.19 | the tracer core: span decorators, header adapters |
| `camel-opentelemetry` | 3.5 | deprecated in 4.19 | `camel-tracing` on the OpenTelemetry API |
| `camel-observation` | 3.21 | deprecated in 4.19 | `camel-tracing` on Micrometer Observation |
| `camel-telemetry` | 4.11 | preview | the new tracer core, same decorators |
| `camel-opentelemetry2` | 4.11 | stable | `camel-telemetry` on OpenTelemetry; the one Camel recommends |

Cameleer follows **`camel-opentelemetry2`** where the two generations differ, and takes the
option names of both (they are the same five options, `camel.opentelemetry.*`).

### 1.2 Where spans come from

Both generations hook the same four places:

| hook | span | Cameleer |
|---|---|---|
| `RoutePolicy.onExchangeBegin` / `onExchangeDone` | one span per exchange and route (the consumer side) | a wrapper of the route processor, installed by a `RouteFactoryHook` |
| `EventNotifier` on `ExchangeSendingEvent` / `ExchangeSentEvent` | one span per send to an endpoint (`to`, `toD`, `wireTap`, `enrich`, recipient list, templates) | a wrapper of every producer the context creates (`CamelContext::add_producer_wrapper`) |
| `InterceptStrategy` | one span per processor, with `traceProcessors=true` | a channel wrapper |
| `LogListener` | the `log` EIP's message as a span event named `log` | not done (see 5) |

The producer wrapper replaces Camel's sending events because Cameleer's `to` does not emit
`ExchangeSending` events (only the producer template does); wrapping the producer covers every
path in one place (`to` and `wireTap` create producers through `CamelContext::create_producer`,
everything else through `get_producer`).

### 1.3 Span names and kinds

| decorator | name | kind (otel2) | kind (legacy) |
|---|---|---|---|
| default (any component) | the component name: `timer`, `sql`, `mock` | INTERNAL | CLIENT / SERVER |
| internal (`direct`, `seda`, `disruptor`, `vm`) | the URI without scheme and options, at most 250 characters | INTERNAL | INTERNAL |
| HTTP (`http`, `platform-http`, `netty-http`, `jetty`, `undertow`, `servlet`, `vertx-http`, `rest`) | the method: `CamelHttpMethod`, else GET with a query, else POST with a body, else GET | CLIENT sending, SERVER receiving | same |
| messaging (`kafka`, `jms`, `activemq`, `amqp`, `sjms`, `rabbitmq`, `paho`, `mqtt`, `stomp`, `nats`, `aws2-sqs`, `aws2-sns`, `azure-servicebus`, `google-pubsub`, ...) | the destination: Kafka `kafka.OVERRIDE_TOPIC` or the topic, JMS `CamelJmsDestinationName` or the queue/topic name | PRODUCER sending, CONSUMER receiving | same |
| processor spans | `<id>-<shortName>`, e.g. `log1-log` | INTERNAL | INTERNAL |

### 1.4 Attributes

| on | attributes |
|---|---|
| every span | `component` = `camel-<scheme>`, `url.scheme`, `camel.uri` (secrets masked), `url.path`, `url.query`, `camel.route.id`, `op` (`EVENT_RECEIVED`, `EVENT_SENT`, `EVENT_PROCESS`), `exchangeId` |
| HTTP | `http.method`, `http.url`, `http.status_code` (from `CamelHttpResponseCode` after the call) |
| messaging | `messaging.destination.name`, `messaging.message.id` (JMS `JMSMessageID`) |
| Kafka | `kafka.partition`, `kafka.offset`, `kafka.key`, `kafka.partition.key` |
| a failed exchange | `error` = true, status ERROR with the message, an event `error` with `error.kind` = `Exception` and `message` |

Camel's `server.address`, `server.protocol`, `user.name` (from the endpoint's service location)
and `db.system`/`db.statement` (JDBC and SQL) are not set yet. otel2 writes `error` as the
string `"true"`; Cameleer writes a boolean, as `camel-opentelemetry` did, because backends filter
on booleans. The `error` event also carries `exception.type` (Camel's exception class name in
Cameleer's error kinds).

### 1.5 Propagation

Camel has **no per-protocol propagation code**: the configured OpenTelemetry propagators inject
into and extract from the **Camel message headers**, and each component maps headers to its wire
as it always does. The injector skips keys starting with `Camel`; the extractors are
case-insensitive and accept `byte[]` values (decoded as UTF-8), which is how Kafka and AMQP
headers arrive.

| carrier | how the context travels in Cameleer |
|---|---|
| HTTP (`http`, `platform-http`, `netty-http`, `jetty`, `rest`) | request headers become message headers on the consumer; the producer sends non-`Camel*` headers |
| Kafka | record headers (bytes) both ways; `kafka.*` headers stay out |
| JMS, ActiveMQ, AMQP 1.0, STOMP | message properties both ways |
| gRPC | Cameleer's producer sends non-`Camel*` scalar headers as call metadata and the consumer maps metadata to headers, so the context travels; Camel's gRPC component does not copy metadata, so this is more than Camel does |
| MCP | the server already passed `traceparent`, `tracestate` and `baggage` from `_meta` to the route as headers |

`camel.opentelemetry.encoding=true` encodes header keys for carriers that forbid `-` (JMS):
legacy Camel writes `_$dash$_`, otel2 `__dash__` for JMS only and always. The W3C header names
have no `-`, so this matters only for other propagators (B3); Cameleer implements the legacy
encoding (`_$dash$_`, and `_$dot$_` for `.`).

The active span of an exchange is kept in the exchange property `CamelOpenTelemetryActiveSpan`
(a `traceparent` value), with `CamelOpenTelemetryActiveTraceState` and
`CamelOpenTelemetryBaggage`. Exchange copies (split, multicast, wire tap, seda) carry it, so
spans on other threads keep their parent. A route span takes the exchange's active span as its
parent; without one it extracts the headers; without those it starts a trace. A producer span's
parent is the exchange's active span only (as Camel's sending span: no extraction).

### 1.6 Configuration

| Camel | default | Cameleer |
|---|---|---|
| `camel.opentelemetry.enabled` | false | same; as in Camel Main, setting any other option without `enabled=false` enables it |
| `camel.opentelemetry.instrumentationName` | `camel` | same (the instrumentation scope) |
| `camel.opentelemetry.excludePatterns` | none | same: comma separated, matched against endpoint URIs (also with `://` written as `:`), route ids and processor ids, with wildcards and regular expressions |
| `camel.opentelemetry.traceProcessors` | false | same |
| `camel.opentelemetry.encoding` | false | same |
| otel2 `includePatterns`, `traceCustomIdOnly`, `disableCoreProcessors`, `traceHeadersInclusion`, `exportTarget` | | not done |

Camel leaves the SDK to the OpenTelemetry Java agent or autoconfiguration, which read the
`OTEL_*` environment variables (or `otel.*` system properties). Cameleer reads them itself, in
both spellings (a property `otel.exporter.otlp.endpoint` wins over `OTEL_EXPORTER_OTLP_ENDPOINT`):

| variable | default | supported |
|---|---|---|
| `OTEL_SDK_DISABLED` | false | yes |
| `OTEL_SERVICE_NAME` | `unknown_service:<executable>` | yes, wins over `service.name` in the attributes |
| `OTEL_RESOURCE_ATTRIBUTES` | none | yes, `k=v,...`, percent-decoded |
| `OTEL_TRACES_EXPORTER` | `otlp` | `otlp`, `console`/`logging`, `none` |
| `OTEL_EXPORTER_OTLP_PROTOCOL`, `_TRACES_PROTOCOL` | `http/protobuf` | `http/protobuf` only; `grpc` and `http/json` are refused with a message |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `http://localhost:4318` | base URL, `/v1/traces` appended |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | | used as is |
| `OTEL_EXPORTER_OTLP_HEADERS`, `_TRACES_HEADERS` | | yes (`Authorization=Bearer%20...`), the specific one wins per key |
| `OTEL_EXPORTER_OTLP_TIMEOUT`, `_TRACES_TIMEOUT` | 10000 ms | yes: the budget of one batch, retries included |
| `OTEL_EXPORTER_OTLP_COMPRESSION`, `_TRACES_COMPRESSION` | none | `gzip`, `none` |
| `OTEL_EXPORTER_OTLP_CERTIFICATE`, `_CLIENT_CERTIFICATE`, `_CLIENT_KEY` (and `_TRACES_*`) | system store | yes: a CA file replaces the system store; client certificates for mutual TLS |
| `OTEL_EXPORTER_OTLP_INSECURE` | | ignored (gRPC only); certificate verification cannot be turned off |
| `OTEL_TRACES_SAMPLER`, `_ARG` | `parentbased_always_on` | `always_on`, `always_off`, `traceidratio`, `parentbased_*` |
| `OTEL_PROPAGATORS` | `tracecontext,baggage` | `tracecontext`, `baggage`, `none`; `b3`, `jaeger` and others are refused |
| `OTEL_BSP_SCHEDULE_DELAY`, `_EXPORT_TIMEOUT`, `_MAX_QUEUE_SIZE`, `_MAX_EXPORT_BATCH_SIZE` | 5000, 30000, 2048, 512 | yes |
| `OTEL_SPAN_ATTRIBUTE_COUNT_LIMIT`, `OTEL_ATTRIBUTE_COUNT_LIMIT`, `OTEL_SPAN_EVENT_COUNT_LIMIT` | 128 | yes |

### 1.7 Logs and MDC

Camel's MDC keys (`camel.exchangeId`, `camel.routeId`, `camel.messageId`, `camel.breadcrumbId`,
`camel.correlationId`, `camel.contextId`, `camel.transactionKey`, `camel.stepId`) are set with
`camel.main.useMdcLogging=true`. The legacy tracer put `trace_id` and `span_id` in the MDC (also
the OpenTelemetry Java agent's keys); Micrometer and Spring Boot use `traceId`/`spanId`. Camel
JBang's `--logging-json` writes log4j2's ECS layout: `@timestamp`, `log.level`, `message`,
`ecs.version`, `process.thread.name`, `log.logger`, MDC as labels, `error.*`. Camel Main has no
JSON option; the layout belongs to the logging backend.

## 2. In scope now (phase 1, built)

1. **The tracer in `cameleer-core`** (`cameleer_core::telemetry`): route, producer and
   processor spans with otel2's names, kinds and attributes; W3C Trace Context and Baggage on
   the message headers; head sampling; span limits; exclusion; the active span on the exchange;
   the current span of the thread for log lines (also across `.await` points of asynchronous
   routes). The SPI is `SpanProcessor` (`on_end`), so the core has no exporter and no network
   code. Nothing is installed unless asked: without `telemetry::install`, no hook, wrapper or
   per-exchange check runs.
2. **The exporter in a new crate `cameleer-opentelemetry`** (cargo feature `opentelemetry`, in
   `full`): OTLP/HTTP with protobuf (hand-written encoder, about 150 lines), gzip from
   `cameleer-core`'s deflate, the shared HTTP client (keep-alive, TLS with full verification,
   CA and client certificates), headers for auth; the batch span processor with a bounded queue
   that drops and counts when full; retries of 429, 502, 503, 504 and transport errors with
   exponential backoff (1 s, 1.5x, at most 5 s, 20% jitter, 5 attempts) that honours
   `Retry-After` (seconds or HTTP date) within the export budget; partial success logged, never
   retried; a `console` exporter.
3. **`cameleer run`**: `camel.opentelemetry.*` and `OTEL_*`; installed before routes are
   created; flushed at stop; flushed after each AWS Lambda invocation (the process freezes
   between invocations).
4. **Logs**: trace and span id in every record logged inside a span (`LogRecord::trace`); text
   lines get `[trace_id,span_id]` after the thread; `camel.main.log-format` /
   `CAMEL_LOG_FORMAT` / `--log-format` = `text`, `ecs` (Camel JBang's `--logging-json`, also
   accepted as a flag) or `logstash` (logstash-logback-encoder's names, which Spring Boot's and
   Quarkus' `logstash` formats share); `trace.id`/`span.id` in ECS, `trace_id`/`span_id` in
   logstash.
5. **Tests**: W3C parsing rules, the protobuf encoding against known bytes and a decoder, the
   batch processor's limits and failure handling, the exporter's retries against an embedded
   OTLP receiver, span trees through `direct`, `seda` and `wireTap`, propagation across
   HTTP → Kafka (embedded broker) → HTTP over three services, over JMS (STOMP), AMQP 1.0 and
   gRPC, and `Main` from properties. An official-clients test against the OpenTelemetry
   Collector (`scripts/otel-collector.sh`, pinned v0.165.0 by SHA-256, gated by
   `CAMEL_OFFICIAL_CLIENTS=1`).

## 3. Not in scope now (phase 2 candidates)

| item | why not now | instead |
|---|---|---|
| OTLP metrics (`camel-micrometer`, `camel-opentelemetry-metrics`) | the route statistics already exist; a periodic OTLP metrics reader is a separate piece | the Collector's Prometheus receiver scrapes `/q/metrics` |
| OTLP logs | needs a log record exporter and a bounded queue of its own | JSON lines with trace ids, collected by the Collector's `filelog` receiver |
| OTLP over gRPC, OTLP/HTTP JSON | `http/protobuf` is the spec's default and every collector speaks it | point the exporter at port 4318 |
| B3 and Jaeger propagators | not W3C; Camel only has them through the SDK | the Collector translates between formats |
| Camel's MDC keys (`camel.exchangeId`, `camel.routeId`, ...) in log lines | needs per-step thread-local strings even when tracing is off | trace and span ids in log lines |
| the `log` EIP as span events | needs a per-span event buffer reachable from the logger | the log line carries the span id |
| otel2's `includePatterns`, `traceCustomIdOnly`, `traceHeadersInclusion` | rarely used | `excludePatterns` |
| span links, exemplars | Camel does not set links | |

## 4. Overhead

Measured with `cameleer-bench` on one Windows machine (`CAMEL_BENCH_OTEL`: `off`, `spans` with a
discarding span processor, `otlp` to an in-process receiver that answers 200). The results and
the method are in `docs/guides/opentelemetry.md` ("What it costs") and
`benchmarks/results/2026-11-25-windows-opentelemetry/`.

## 5. Risks and decisions

- **The hot path when off is unchanged**: the producer wrapper list is read when a producer is
  created, not per message; `log::emit` reads one relaxed atomic (`INSTALLED`) before touching
  the thread-local.
- **A slow or absent collector never slows a route**: `on_end` takes a mutex and pushes or drops;
  export runs on its own thread.
- **Ids** come from a per-thread splitmix64 seeded from time, process id, stack address and a
  global counter: unique, not secret (the spec asks for randomness, not unpredictability).
- **Kind of the default decorator**: otel2's INTERNAL rather than legacy's CLIENT/SERVER, so a
  `timer` route is INTERNAL; HTTP and messaging keep their kinds.
