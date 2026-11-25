# Observability: health checks, Prometheus metrics, logs and tracing

> Cameleer exposes Camel's health checks at `/q/health`, Prometheus metrics at `/q/metrics` and developer consoles at `/q/dev/*` on its management server (`camel.server.*`, or `cameleer run --console`), logs to standard output in text or JSON lines with trace ids, exports OpenTelemetry traces over OTLP (`camel.opentelemetry.enabled`), and traces messages with `--trace`, the backlog tracer and message history. The `cameleer ps`, `get`, `log` and `trace` commands show the same information from the command line.

**Applies to:** the `cameleer` CLI, the Docker image and `cameleer_main::Main` · **Apache Camel:** [Camel Main HTTP server](https://camel.apache.org/components/next/others/main.html), [Health checks](https://camel.apache.org/manual/health-check.html), [Tracer](https://camel.apache.org/manual/tracer.html)

| you want | use |
|---|---|
| liveness and readiness probes | `/q/health/live`, `/q/health/ready` (200 when UP, 503 when DOWN); also at `/observe/health/live` and `/observe/health/ready` |
| metrics in Prometheus or Grafana | `/q/metrics`, Prometheus text format; also at `/observe/metrics` |
| which version, features and packs a running instance has | `/q/info` (`camel.server.infoEnabled=true`, or with the dev console) |
| route state, statistics, endpoints | `/q/dev/routes`, `/q/dev/endpoints`, or `cameleer get routes` |
| to start, stop or suspend a route at run time | `POST /q/dev/route/{id}/suspend`, or `cameleer cmd suspend-route` |
| logs | standard output; `CAMEL_LOG_LEVEL`, `--log-level`; JSON lines with `--logging-json` or `camel.main.log-format=ecs` / `logstash` |
| distributed traces in Jaeger, Tempo, Honeycomb, Datadog | `camel.opentelemetry.enabled=true` and `OTEL_EXPORTER_OTLP_ENDPOINT` ([OpenTelemetry](opentelemetry.md)) |
| to see each message move through a route | `--trace`, or the backlog tracer and `cameleer trace` |
| to see where a failed message went | message history (`camel.main.message-history=true`) |

## Turn on the management server

The management server is off by default. Turn it on with the endpoints you want:

```properties
camel.server.enabled = true
camel.server.healthCheckEnabled = true
camel.server.metricsEnabled = true
camel.server.devConsoleEnabled = true
# camel.server.port = 8080      (the default; shared with REST and platform-http routes)
# camel.server.host = 0.0.0.0   (the default bind address)
```

`cameleer run --console` sets all four. In Docker, pass the flags or the environment variables:

```bash
cameleer run orders.camel.yaml --console
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" cameleering/cameleer run /routes/orders.camel.yaml --console
docker run --rm -p 8080:8080 -e CAMEL_SERVER_ENABLED=true -e CAMEL_SERVER_HEALTH_CHECK_ENABLED=true \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/orders.camel.yaml
```

The management endpoints have no authentication. `/q/dev` can stop and suspend routes, so keep
the port on a private network, or move the endpoints to a separate management port bound to the
loopback interface (below).

On `camel.server.port` the endpoints speak what the port speaks: with
`camel.server.sslEnabled=true` they are HTTPS, next to your HTTPS routes
([TLS and security](tls-and-security.md#https-for-your-routes)).

### The `/observe` paths of camel-observability-services

Camel's `camel-observability-services` (Camel 4.9+) serves the same probes and metrics under
`/observe`: `/observe/health`, `/observe/health/live`, `/observe/health/ready` and
`/observe/metrics`. Cameleer answers those paths too, on the same server and with the same
switches (`camel.server.healthCheckEnabled`, `camel.server.metricsEnabled`), so probes and scrape
configurations written for it work unchanged. Since Camel 4.12 those endpoints are on a
management port, 9876 by default; Cameleer serves them on `camel.server.port` (8080), or on its
own [management port](#a-separate-management-port) when one is set: set it to 9876 to match.

## A separate management port

Camel's separate management server is supported too. With `camel.management.enabled=true` the
`/q/*` endpoints move off the routes' port to their own plain-HTTP port, so probes and scrapers
don't share a port (or TLS) with your API:

```properties
camel.management.enabled = true
camel.management.port = 9876             # the default
camel.management.host = 127.0.0.1        # default: camel.server.host (0.0.0.0)
camel.management.healthCheckEnabled = true
camel.management.metricsEnabled = true
camel.management.devConsoleEnabled = true
```

| key | default | effect |
|---|---|---|
| `camel.management.enabled` | `false` | serve the management endpoints on their own port instead of `camel.server.port` |
| `camel.management.port` | `9876` | the management port |
| `camel.management.host` | `camel.server.host` | the bind address; `127.0.0.1` keeps `/q/dev` local |
| `camel.management.healthCheckEnabled`, `metricsEnabled`, `devConsoleEnabled` | `false` | which endpoints; the `camel.server.*` switches count as well, so `--console` works here too |

The management port is always plain HTTP. `/q/attestation` and the MCP server stay on
`camel.server.port`.

## Health checks

| endpoint | checks | use it for |
|---|---|---|
| `GET /q/health` | all | a dashboard or `cameleer get health --url` |
| `GET /q/health/live` | `context` | a liveness probe: restart the process when this fails |
| `GET /q/health/ready` | `context`, `routes`, `consumers` | a readiness probe: send traffic only when this passes |

| check | UP when |
|---|---|
| `context` | the Camel context is started |
| `routes` | every route that starts automatically is started |
| `consumers` | every started route has a running consumer |

The answer is JSON, with status 200 when every check is UP and 503 otherwise:

```json
{"status":"UP","checks":[
  {"name":"context","status":"UP","data":{"context.name":"camel-1","context.status":"Started"}},
  {"name":"routes","status":"UP","data":{"route.ticks":"Started"}},
  {"name":"consumers","status":"UP","data":{"consumer.ticks":"UP"}}]}
```

A suspended or stopped route makes readiness DOWN (`"message":"Routes not started: ticks"`),
while liveness stays UP. In Rust, add your own checks with
`CamelContext::register_health_check`.

Kubernetes probes:

<!-- not-camel -->
```yaml
livenessProbe:
  httpGet: {path: /q/health/live, port: 8080}
  periodSeconds: 10
readinessProbe:
  httpGet: {path: /q/health/ready, port: 8080}
  periodSeconds: 5
```

The image is distroless and has no shell or `curl`, so use `httpGet` probes rather than `exec`.
The [deployment guide](../DEPLOYMENT.md) has a full manifest.

## Prometheus metrics

`GET /q/metrics` returns the Prometheus text format, with Camel's metric names and the
`camelContext` and `routeId` labels:

| metric | type | labels |
|---|---|---|
| `camel_exchanges_total`, `camel_exchanges_succeeded_total`, `camel_exchanges_failed_total` | counter | `camelContext`, `routeId` |
| `camel_exchanges_inflight` | gauge | `camelContext`, `routeId` |
| `camel_route_policy_seconds` (`_count`, `_sum`), `camel_route_policy_seconds_max` | summary, gauge | `camelContext`, `routeId` |
| `camel_routes_running_routes`, `camel_routes_added_routes` | gauge | `camelContext` |
| `camel_context_exchanges_total`, `camel_context_exchanges_failed_total` | counter | `camelContext` |
| `camel_context_uptime_seconds`, `camel_context_status` (1 = started) | gauge | `camelContext` |
| `camel_processor_exchanges_total`, `camel_processor_exchanges_failed_total`, `camel_processor_seconds_sum` | counter | `camelContext`, `routeId`, `nodeId` |

```text
camel_exchanges_total{camelContext="camel-1",routeId="ticks"} 6
camel_route_policy_seconds_count{camelContext="camel-1",routeId="ticks"} 6
camel_routes_running_routes{camelContext="camel-1"} 1
```

**Per-step metrics are opt-in.** Cameleer collects route-level statistics by default
(`RoutesOnly`), which costs almost nothing. The `camel_processor_*` metrics and the per-step
statistics in `/q/dev/routes` need `camel.main.jmxManagementStatisticsLevel=Default` (or
`Extended`); `Off` collects nothing. Apache Camel without `camel-management` collects nothing
either.

A Prometheus scrape job:

<!-- not-camel -->
```yaml
scrape_configs:
  - job_name: cameleer
    metrics_path: /q/metrics
    static_configs:
      - targets: ["orders:8080"]
```

## Camel metrics

`camel.metrics.enabled=true` adds Camel's own meters, as `camel-micrometer-prometheus` does,
with Camel's names and tags, to `/q/metrics` and `/observe/metrics`:

| meter | what | tags |
|---|---|---|
| `camel.route.policy` (timer) | processing time per route, and for the context | `camelContext`, `kind=CamelRoute`, `routeId` (`""` for the context), `eventType` (`route`, `context`) |
| `camel.exchanges.total`, `.succeeded`, `.failed`, `.failures.handled`, `.external.redeliveries` (counters) | exchanges per route and for the context | the same |
| `camel.exchange.event.notifier` (timer) | exchange processing (`ExchangeCompletedEvent`, `ExchangeFailedEvent`) and sends to endpoints (`ExchangeSentEvent`) | `camelContext`, `kind=CamelExchangeEvent`, `eventType`, `routeId`, `endpointName`, `failed` |
| `camel.exchanges.inflight` (gauge) | inflight exchanges per route | `camelContext`, `kind`, `routeId` |
| `camel.exchanges.last.timestamp` (gauge) | when the last exchange finished, epoch milliseconds | |
| `camel.routes.added`, `camel.routes.running` (gauges, unit `routes`) | routes | `camelContext`, `kind=CamelRoute`, `eventType=RouteEvent` |
| `camel.message.history` (timer, with `enableMessageHistory`) | time per step | `camelContext`, `kind=CamelMessageHistory`, `routeId`, `nodeId` |
| `app.info` | the version and runtime | `camel.version`, `camel.context`, `camel.runtime.provider`, `camel.runtime.version` |

In Prometheus they read as Micrometer writes them: `camel_route_policy_seconds_count`,
`camel_exchanges_total`, `camel_exchange_event_notifier_seconds_sum`, `camel_routes_added_routes`.
Without `camel.metrics.enabled` the endpoint still has the route statistics it always had
(`camel_exchanges_total{camelContext,routeId}` and the others); with it, Camel's families replace
those of the same name.

| option | default | |
|---|---|---|
| `camel.metrics.enabled` | `false` | turn the meters on |
| `camel.metrics.namingStrategy` | `default` | `legacy`: Camel 3 names (`CamelRoutePolicy`, `CamelExchangesSucceeded`, ...) |
| `camel.metrics.enableRoutePolicy` | `true` | the route policy meters |
| `camel.metrics.routePolicyLevel` | `all` | `context` (only the context), `route` (only the routes) |
| `camel.metrics.enableExchangeEventNotifier` | `true` | the exchange event notifier meters |
| `camel.metrics.baseEndpointURIExchangeEventNotifier` | `true` | `endpointName` without the options |
| `camel.metrics.recordLastExchangeTimestamp` | `true` | the last-exchange gauge |
| `camel.metrics.enableRouteEventNotifier` | `true` | the route gauges |
| `camel.metrics.enableMessageHistory` | `false` | a timer per step (turns message history on) |
| `camel.metrics.skipCamelInfo` | `false` | leave out `app.info` |
| `camel.metrics.logMetricsOnShutdown` | `false` | log the meters at shutdown, as `json` or `prometheus` (`logMetricsOnShutdownFormat`), filtered by name (`logMetricsOnShutdownFilters`, wildcards) |
| `camel.metrics.textFormatVersion` | `0.0.4` | only `0.0.4`; `1.0.0` (OpenMetrics) is refused |

Routes add their own meters with the [micrometer](../components/micrometer.md),
[metrics](../components/metrics.md) and
[opentelemetry-metrics](../components/opentelemetry-metrics.md) components; all are on the same
endpoint and, with `OTEL_METRICS_EXPORTER=otlp`, exported over OTLP
([OpenTelemetry](opentelemetry.md#metrics)). `camel.metrics.binders` (JVM, thread pool and
logback binders) does not apply: there is no JVM.

What it costs: with the options off nothing is installed. The default meters cost about 0.1 to
0.26 µs per message on one Windows desktop (`direct` 3.08 M → 1.84 M messages/s; `seda`
4.80 M → 3.20 M; `cbr` 1.41 M → 1.04 M; measured with `cameleer-bench run` and
`CAMEL_BENCH_METRICS=micrometer`, see `benchmarks/results/2026-11-25-windows-metrics/`).

## Developer consoles and route actions

With `camel.server.devConsoleEnabled=true`:

| endpoint | what it returns |
|---|---|
| `GET /q/dev` | the list of consoles |
| `GET /q/dev/context` | the context: name, version, state, uptime, totals |
| `GET /q/dev/routes`, `/q/dev/route/{id}` | each route's state, source endpoint, statistics, steps and a dump of its structure |
| `GET /q/dev/endpoints` | the endpoints the routes use |
| `GET /q/dev/health` | the health checks |
| `GET /q/dev/trace` | the backlog tracer's state and recent events |
| `GET /q/dev/route-controller` | the supervising route controller (routes being retried) |
| `GET /q/dev/templates` | registered route templates |
| `GET /q/dev/tools` | AI tools, the kill switch and pending approvals ([MCP server](mcp-server.md)) |
| `POST /q/dev/route/{id}/start`, `stop`, `suspend`, `resume`, `reset-stats` | change a route at run time; the answer is `{"routeId":..,"action":..,"state":..}` |
| `POST /q/dev/tools/{name}/disable`, `enable` | turn an AI tool off or on |

`cameleer get routes --url http://host:8080` prints the routes as a table from these endpoints,
which works for a container or another host.

## Logs

Cameleer logs to standard output in one line per event, in Camel's layout:

```text
2026-11-09 12:07:56.596 INFO  [main] Main : Stopping CamelContext: duration max seconds (5) reached
```

| setting | effect |
|---|---|
| `CAMEL_LOG_LEVEL` | the level: `TRACE`, `DEBUG`, `INFO` (default), `WARN`, `ERROR`, `OFF` |
| `--log-level`, `camel.main.log-level` | the same, from the command line or a properties file |
| `CAMEL_LOG_FORMAT`, `--log-format`, `camel.main.log-format` | `text` (default), `ecs` (Elastic Common Schema JSON, as Camel JBang's `--logging-json`; `json` is the same), `logstash` (logstash-logback-encoder JSON) |
| `--logging-json` | Camel JBang's flag: the same as `--log-format ecs` |
| the `log` EIP and the `log:` component | log from a route (`log: "${body}"`, `to: log:orders?level=DEBUG&showHeaders=true`) |

A failed exchange is logged with the error handler's report: the exception, the exchange and, with
message history on, the steps it went through. In a container, logs go to the container
runtime; `cameleer run` outside a container also writes them to `$CAMEL_HOME`, where
`cameleer log --follow` reads them.

With [OpenTelemetry tracing](opentelemetry.md) on, every line logged inside a span carries its
trace and span id: after the thread in text lines
(`INFO  [main] [0af7651916cd43dd8448eb211c80319c,b7ad6b7169203331] orders : ...`), as
`trace.id`/`span.id` in ECS lines and `trace_id`/`span_id` in logstash lines:

```json
{"@timestamp":"2026-11-25T10:00:00.123Z","log.level":"INFO","message":"stored order 42","ecs.version":"1.2.0","process.thread.name":"main","log.logger":"orders","trace.id":"0af7651916cd43dd8448eb211c80319c","span.id":"b7ad6b7169203331"}
```

## Distributed tracing with OpenTelemetry

`camel.opentelemetry.enabled=true` traces every route as Camel's `camel-opentelemetry2` does: a
span per exchange and route, a span per send to an endpoint, the W3C `traceparent` carried
through HTTP headers, Kafka record headers, JMS and AMQP properties and gRPC metadata, and the
spans exported over OTLP/HTTP to the collector named by `OTEL_EXPORTER_OTLP_ENDPOINT`. Options,
span names, attributes and costs are on the [OpenTelemetry](opentelemetry.md) page.

```properties
camel.opentelemetry.enabled = true
otel.service.name = orders
otel.exporter.otlp.endpoint = http://otel-collector:4318
```

## Log pipelines

Cameleer can also carry other programs' logs and metrics, the job Logstash or Fluentd often does in
front of a log store. A route receives the events, filters and enriches them with the EIPs, and sends
them on:

| receive | send |
|---|---|
| [lumberjack](../components/lumberjack.md): Filebeat and the other Beats, Logstash (port 5044) | [splunk-hec](../components/splunk-hec.md): Splunk's HTTP Event Collector |
| [syslog](../components/syslog.md): RFC 5424 and 3164 over UDP, TCP or TLS | [elasticsearch](../components/elasticsearch.md), [opensearch](../components/opensearch.md) |
| [kafka](../components/kafka.md), [platform-http](../components/platform-http.md) (JSON over HTTP) | [kafka](../components/kafka.md), [aws2-cw](../components/aws2-cw.md) (CloudWatch metrics), [file](../components/file.md) |

Beats events to Splunk, dropping debug messages:

<!-- example: observability-beats-to-splunk -->
```yaml
- route:
    id: beats-to-splunk
    from:
      uri: lumberjack:0.0.0.0:5044
      steps:
        - filter:
            simple: "${body[log][level]} != 'debug'"
            steps:
              - to: "splunk-hec:{{env:SPLUNK_HEC:splunk.example.com:8088}}?token={{env:SPLUNK_HEC_TOKEN}}&index=main&sourceType=_json&bodyOnly=true"
```

[examples/logs-to-splunk.camel.yaml](https://github.com/cameleering/cameleer/blob/main/examples/logs-to-splunk.camel.yaml)
adds syslog to the same pipeline. Delivery is at-least-once: the Lumberjack server acknowledges a
window only after every event in it went through the route, and a failure makes the shipper send the
window again. Make the route idempotent (an [idempotent consumer](eips/flow-control.md#idempotent-consumer) on an event id) when
duplicates in the store matter.

## Tracing a message

**The tracer** (`--trace`, `camel.main.tracing=true`) logs each message at each step, as Camel's
`Tracing` logger does:

```text
INFO  [Camel timer://tick] org.apache.camel.Tracing : >>> (ticks) setBody[constant{tick}] <<< Exchange[Id: ID-..., ExchangePattern: InOnly, Headers: {}, BodyType: null, Body: [Body is null]]
```

**The backlog tracer** (`camel.main.backlog-tracing=true`) keeps recent events in memory instead
of logging them. Read them at `/q/dev/trace`, or with `cameleer trace`, which also turns it on
for a running integration (`cameleer trace --disable` turns it off).

**Message history** (`camel.main.message-history=true`) records the steps each exchange went
through. When an exchange fails, the error log shows them as a table, so you can see where it
failed:

```text
Message History
RouteId              ProcessorId          Processor                                            Elapsed (ms)
[fails             ] [fails             ] [from[timer://once?repeatCount=1]                  ] [         0]
[fails             ] [setBody1          ] [setBody[constant{hello}]                          ] [         0]
[fails             ] [throwException2   ] [throwException[IllegalArgumentException, bad order]] [         0]
```

All three cost time on every message; turn them on to investigate, not by default in production.

## From the command line

For integrations started with `cameleer run` on the same machine:

| command | shows |
|---|---|
| `cameleer ps` | every running integration with its age, routes, total and failed messages |
| `cameleer get routes [name]` | routes with state and statistics |
| `cameleer get health [name]` | health checks; exits with 1 when DOWN |
| `cameleer log [name] --follow` | the log as it is written |
| `cameleer trace [name]` | traced messages |

Every command is in [the cameleer command line](cli.md).

## What is not there yet

- **Logs over OTLP**: collect the JSON log lines (the Collector's `filelog` receiver).
- **`camel-observation`** (Micrometer Observation): there is no Micrometer; the OpenTelemetry
  tracer covers the same spans.
- **JMX**: there is no JVM, so there are no MBeans; the same data is in `/q/dev` and `/q/metrics`.
- **Camel's MDC keys** (`camel.exchangeId`, `camel.routeId`, ...) in log lines; the trace and span
  id are there.

## A worked example

This runs in the test suite (`observability_guide_worked_example` in
`crates/cameleer-main/tests/docs_reference.rs`):

<!-- example: observability-ticks -->
```yaml
- route:
    id: ticks
    from:
      uri: timer:tick?period=200
      steps:
        - setBody:
            constant: tick
        - to: log:ticks?level=DEBUG
```

```bash
cameleer run ticks.camel.yaml --console --port 8080
curl localhost:8080/q/health/ready                 # 200, three checks UP
curl localhost:8080/q/metrics                      # camel_exchanges_total{...,routeId="ticks"} grows
curl -X POST localhost:8080/q/dev/route/ticks/suspend
curl -i localhost:8080/q/health/ready              # 503: Routes not started: ticks
curl localhost:8080/q/health/live                  # still 200
```

## Common questions

### How do I add a health check endpoint for Kubernetes?

Set `camel.server.enabled=true` and `camel.server.healthCheckEnabled=true` (or run with
`--console`), and point the liveness probe at `/q/health/live` and the readiness probe at
`/q/health/ready` on port 8080.

### How do I get Prometheus metrics?

Set `camel.server.enabled=true` and `camel.server.metricsEnabled=true`, and scrape
`/q/metrics` (or `/observe/metrics`). For Camel's metric names and tags add
`camel.metrics.enabled=true` ([Camel metrics](#camel-metrics)); for a timer per step,
`camel.metrics.enableMessageHistory=true`.

### Does Cameleer support OpenTelemetry?

Yes, for traces: set `camel.opentelemetry.enabled=true` and `OTEL_EXPORTER_OTLP_ENDPOINT`, and
spans go to the collector over OTLP/HTTP, with the context propagated in W3C `traceparent`
headers ([OpenTelemetry](opentelemetry.md)). Metrics are in the Prometheus format at
`/q/metrics`, which the Collector can scrape with its Prometheus receiver.

### How do I see why a message failed?

Turn on message history (`camel.main.message-history=true`): the error log then lists the steps
the message went through. For a live view, run with `--trace`.

### Can I change the log format?

Yes: `camel.main.log-format` (or `CAMEL_LOG_FORMAT`, or `cameleer run --log-format`) is `text`,
`ecs` or `logstash`; `cameleer run --logging-json` is `ecs`, as in Camel JBang. The JSON
formats write one object per line, with the trace and span id when tracing is on.

## Related

- [Configuration](configuration.md), [the cameleer command line](cli.md),
  [Error handling](error-handling.md)
- [Deployment](../DEPLOYMENT.md): Docker, Kubernetes, Cloud Run, AWS Lambda
- [log](../components/log.md) component
- [OpenTelemetry](opentelemetry.md): distributed tracing over OTLP
- [Attestation in confidential VMs](attestation.md): `/q/attestation`, evidence of what runs

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
