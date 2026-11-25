# OpenTelemetry

> Distributed tracing for routes, as Camel's `camel-opentelemetry2`: a span for every exchange in every route and for every send to an endpoint, the W3C `traceparent`, `tracestate` and `baggage` carried in and out through HTTP headers, Kafka record headers, JMS and AMQP properties and gRPC metadata, spans exported over OTLP/HTTP to an OpenTelemetry Collector or any OTLP backend, and trace ids in the log lines.

**Scheme:** none (a tracer, not an endpoint) · **Crate:** `cameleer-opentelemetry` (exporter), `cameleer-core` (tracer) · **Cargo feature:** [`opentelemetry`](build-features.md) ·
**Apache Camel:** [camel-opentelemetry2](https://camel.apache.org/components/next/others/opentelemetry2.html), [camel-opentelemetry](https://camel.apache.org/components/next/others/opentelemetry.html)

## When to use it

- See one request end to end: an HTTP call that goes to Kafka, is consumed by another service and
  calls a third, as one trace in Jaeger, Tempo, Honeycomb, Grafana Cloud or Datadog.
- Find which step of which route is slow, or where a message failed, in production.
- Join Cameleer routes to traces that other services (Java, Go, Python, .NET) already make: the
  context is the W3C standard the OpenTelemetry SDKs use.

## Turn it on

```properties
# application.properties
camel.opentelemetry.enabled = true
```

```bash
export OTEL_SERVICE_NAME=orders
export OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4318
cameleer run orders.camel.yaml
```

Every route is traced from then on; nothing in the routes changes. Spans go to
`$OTEL_EXPORTER_OTLP_ENDPOINT/v1/traces` over OTLP/HTTP with protobuf.

## Options

Camel's options (`camel.opentelemetry.*`, kebab-case accepted):

| option | default | meaning |
|---|---|---|
| `enabled` | `false` | turn tracing on; as in Camel Main, setting any other option below turns it on too |
| `instrumentationName` | `camel` | the instrumentation scope name of the spans |
| `excludePatterns` | | comma separated endpoint URIs, route ids or processor ids not to trace; wildcards (`log:*`) and regular expressions |
| `traceProcessors` | `false` | a span for every processor too, named `<id>-<name>` (`setBody1-setBody`) |
| `encoding` | `false` | encode header keys for carriers that forbid `-` (`_$dash$_`, as Camel's JMS key format) |

The OpenTelemetry SDK settings, as environment variables or as properties (`otel.service.name`;
a property wins):

| variable | default | |
|---|---|---|
| `OTEL_SERVICE_NAME` | `unknown_service:cameleer` | the `service.name` resource attribute |
| `OTEL_RESOURCE_ATTRIBUTES` | | more resource attributes, `deployment.environment=prod,team=billing` |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `http://localhost:4318` | the collector; `/v1/traces` is appended |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | | the full traces URL, used as is |
| `OTEL_EXPORTER_OTLP_HEADERS` | | `Authorization=Bearer%20abc,x-tenant=7` (values percent-encoded) |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | `10000` | milliseconds for one batch, retries included |
| `OTEL_EXPORTER_OTLP_COMPRESSION` | `none` | `gzip` |
| `OTEL_EXPORTER_OTLP_CERTIFICATE` | system store | a PEM file of CA certificates to trust instead |
| `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE`, `OTEL_EXPORTER_OTLP_CLIENT_KEY` | | PEM files for mutual TLS |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/protobuf` | only `http/protobuf`; `grpc` and `http/json` are refused at start-up |
| `OTEL_TRACES_EXPORTER` | `otlp` | `console` (or `logging`) logs one line per span; `none` exports nothing but still propagates |
| `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG` | `parentbased_always_on` | `always_on`, `always_off`, `traceidratio` with a ratio, `parentbased_traceidratio`, `parentbased_always_off` |
| `OTEL_PROPAGATORS` | `tracecontext,baggage` | `tracecontext`, `baggage`, `none` |
| `OTEL_BSP_SCHEDULE_DELAY`, `OTEL_BSP_EXPORT_TIMEOUT`, `OTEL_BSP_MAX_QUEUE_SIZE`, `OTEL_BSP_MAX_EXPORT_BATCH_SIZE` | `5000`, `30000`, `2048`, `512` | the batch span processor |
| `OTEL_SPAN_ATTRIBUTE_COUNT_LIMIT`, `OTEL_SPAN_EVENT_COUNT_LIMIT` | `128` | span limits |
| `OTEL_SDK_DISABLED` | `false` | `true` turns everything off |

The `_TRACES_` variants of the exporter variables (`OTEL_EXPORTER_OTLP_TRACES_HEADERS`, ...) win
over the general ones. TLS certificate verification cannot be turned off;
`OTEL_EXPORTER_OTLP_INSECURE` (gRPC only in the specification) is ignored.

## What the spans look like

For an HTTP request that a route sends to Kafka, consumed by another service that calls a third
one over HTTP:

```text
POST                 SERVER    edge       route ingest       http.method=POST
└─ orders            PRODUCER  edge       to kafka:orders    messaging.destination.name=orders kafka.partition=0 kafka.offset=41
   └─ orders         CONSUMER  billing    route bill         kafka.partition=0 kafka.offset=41
      └─ POST        CLIENT    billing    to http://...      http.url=... http.status_code=200
         └─ POST     SERVER    invoicing  route invoice
```

| component | span name | kind |
|---|---|---|
| HTTP (`platform-http`, `http`, `netty-http`, `jetty`, `rest`, ...) | the method | `SERVER` receiving, `CLIENT` sending |
| messaging (`kafka`, `jms`, `activemq`, `amqp`, `paho`, `aws2-sqs`, `google-pubsub`, ...) | the topic or queue | `CONSUMER` receiving, `PRODUCER` sending |
| `direct`, `seda`, `vm`, `disruptor` | the endpoint name (`direct:audit` is `audit`) | `INTERNAL` |
| anything else (`timer`, `sql`, `file`, `log`, `mock`, ...) | the component name | `INTERNAL` |

Every span has `component` (`camel-kafka`), `url.scheme`, `camel.uri` (passwords and tokens
masked), `url.path`, `url.query`, `camel.route.id`, `op` (`EVENT_RECEIVED`, `EVENT_SENT`,
`EVENT_PROCESS`) and `exchangeId`. A failed exchange sets status `ERROR` with the exception
message, the attribute `error=true` and an event `error` (`error.kind`, `message`,
`exception.type`). These are the names Camel's decorators use, so dashboards built for Camel
work unchanged.

## How the context travels

The tracer writes `traceparent` (and `tracestate` and `baggage` when there are any) into the
message headers before each send, and reads them from the headers when a route receives an
exchange that has no span yet. Each component carries headers on its wire as it always does:

| component | carrier |
|---|---|
| `http`, `platform-http`, `netty-http`, `jetty`, `rest` | HTTP headers |
| `kafka` | record headers |
| `jms`, `activemq`, `amqp` (AMQP 1.0), `amqp-stomp`, `stomp` | message properties / headers |
| `grpc` | call metadata (Camel's own gRPC component does not do this) |
| `direct`, `seda`, `wireTap`, split, multicast | the exchange itself (the copy keeps its parent span) |
| the MCP server | `_meta.traceparent` of a tool call |

A route that receives an exchange without a `traceparent` starts a new trace.

## Trace ids in log lines

Every line logged inside a span carries its trace and span id. In the text layout they follow the
thread:

```text
2026-11-25 10:00:00.123 INFO  [platform-http-1] [0af7651916cd43dd8448eb211c80319c,b7ad6b7169203331] orders : stored order 42
```

For log pipelines, choose JSON lines with `camel.main.log-format` (or `CAMEL_LOG_FORMAT`, or
`cameleer run --log-format`):

| value | layout |
|---|---|
| `text` | the default, as above |
| `ecs` (or `json`, or `cameleer run --logging-json` as in Camel JBang) | Elastic Common Schema, as Camel JBang's `--logging-json`: `@timestamp`, `log.level`, `message`, `ecs.version`, `process.thread.name`, `log.logger`, `trace.id`, `span.id` |
| `logstash` | logstash-logback-encoder's names, also Spring Boot's and Quarkus' `logstash` format: `@timestamp`, `@version`, `message`, `logger_name`, `thread_name`, `level`, `level_value`, `trace_id`, `span_id` |

```json
{"@timestamp":"2026-11-25T10:00:00.123Z","log.level":"INFO","message":"stored order 42","ecs.version":"1.2.0","process.thread.name":"platform-http-1","log.logger":"orders","trace.id":"0af7651916cd43dd8448eb211c80319c","span.id":"b7ad6b7169203331"}
```

The JSON formats work with tracing off too; the ids are then left out.

## Example

<!-- example: opentelemetry-orders -->
```yaml
- route:
    id: ingest
    from:
      uri: platform-http:/orders
      steps:
        - log: "received order"
        - to: kafka:orders?brokers=localhost:9092
        - setBody:
            constant: queued
```

```bash
docker run -d -p 4318:4318 -p 16686:16686 jaegertracing/all-in-one   # Jaeger with an OTLP receiver
CAMEL_OPENTELEMETRY_ENABLED=true OTEL_SERVICE_NAME=edge cameleer run ingest.camel.yaml --logging-json
curl -H 'traceparent: 00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01' -d '{"id":42}' localhost:8080/orders
# open http://localhost:16686: trace 0af76519... has the POST and the orders spans
```

## Reliability

- Spans are queued (`OTEL_BSP_MAX_QUEUE_SIZE`) and sent in batches by one background thread; a
  route never waits for the collector. When the queue is full, spans are dropped and counted.
- `429`, `502`, `503`, `504` and connection errors are retried with exponential backoff (1 s,
  growing 1.5x to 5 s, with jitter, at most 5 attempts) and at least as long as a `Retry-After`
  header asks, within `OTEL_EXPORTER_OTLP_TIMEOUT`; any other status drops the batch. One warning
  is logged when exports start failing, and one line when they work again.
- At shutdown the queue is flushed (at most 5 s). On AWS Lambda each invocation's spans are
  exported before the response is reported, because the process freezes between invocations.

## What it costs

With tracing off (the default) the routing engine runs no tracing code at all: no hook, wrapper
or per-message check is installed. With it on, a span costs about 0.1 to 0.35 µs. Measured with
`cameleer-bench run` and `CAMEL_BENCH_OTEL` (`off`, `spans`: every span made and dropped, `otlp`:
exported to a local receiver) on one Windows desktop (Ryzen 5 5600X), median of three
interleaved rounds, messages per second:

| scenario | 0.8.0 (`main`) | tracing off | tracing on | on, exported over OTLP |
|---|---|---|---|---|
| `direct` (one route, one `to`) | 4,011,814 | 3,707,191 | 923,424 | 622,524 |
| `seda` (4 producers, 4 consumers) | 5,095,791 | 5,472,525 | 2,324,393 | 2,161,027 |
| `cbr` (content-based router) | 1,548,251 | 1,665,587 | 719,423 | 617,865 |
| `splitagg` (split 10 items, aggregate) | 100,357 | 93,828 | 72,106 | 57,431 |

Off is within noise of 0.8.0 (0.92x to 1.08x). On, a `direct` route goes from 0.27 to 1.1 µs per
message (2.5 spans each); a route that does any I/O will not notice. The exporter sent about
0.8 million spans per second over one connection; beyond that the queue drops spans and counts
them, and the routes keep their speed. Method and raw data:
`benchmarks/results/2026-11-25-windows-opentelemetry/`.

## Differences from Apache Camel

- The tracer and exporter are built in; there is no OpenTelemetry Java agent or SDK to add.
- Only W3C Trace Context and Baggage propagate; B3 and Jaeger headers do not.
- Spans are exported over OTLP/HTTP with protobuf only (not gRPC or JSON).
- Metrics are not exported over OTLP; scrape `/q/metrics` (Prometheus) with the Collector's
  `prometheus` receiver. Logs are not exported over OTLP; collect the JSON lines.
- Camel's MDC keys (`camel.exchangeId`, `camel.routeId`) are not in the log lines, only the trace
  and span id; the `log` EIP does not add span events.
- `error` is a boolean attribute (`camel-opentelemetry2` writes the string `"true"`).
- otel2's `includePatterns`, `traceCustomIdOnly`, `traceHeadersInclusion` and `exportTarget` are
  not there.

## Related

- [Observability](observability.md): health checks, Prometheus metrics, the tracer and
  message history
- [kafka](../components/kafka.md), [platform-http](../components/platform-http.md), [http](../components/http.md), [jms](../components/jms.md),
  [amqp](../components/amqp.md), [grpc](../components/grpc.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
