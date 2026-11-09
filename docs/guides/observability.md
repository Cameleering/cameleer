# Observability: health checks, Prometheus metrics, logs and tracing

> Cameleer exposes Camel's health checks at `/q/health`, Prometheus metrics at `/q/metrics` and developer consoles at `/q/dev/*` on its management server (`camel.server.*`, or `cameleer run --console`), logs to standard output, and traces messages with `--trace`, the backlog tracer and message history. The `cameleer ps`, `get`, `log` and `trace` commands show the same information from the command line.

**Applies to:** the `cameleer` CLI, the Docker image and `cameleer_main::Main` · **Apache Camel:** [Camel Main HTTP server](https://camel.apache.org/components/next/others/main.html), [Health checks](https://camel.apache.org/manual/health-check.html), [Tracer](https://camel.apache.org/manual/tracer.html)

| you want | use |
|---|---|
| liveness and readiness probes | `/q/health/live`, `/q/health/ready` (200 when UP, 503 when DOWN) |
| metrics in Prometheus or Grafana | `/q/metrics`, Prometheus text format |
| route state, statistics, endpoints | `/q/dev/routes`, `/q/dev/endpoints`, or `cameleer get routes` |
| to start, stop or suspend a route at run time | `POST /q/dev/route/{id}/suspend`, or `cameleer cmd suspend-route` |
| logs | standard output; `CAMEL_LOG_LEVEL`, `--log-level` |
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
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml --console
docker run --rm -p 8080:8080 -e CAMEL_SERVER_ENABLED=true -e CAMEL_SERVER_HEALTH_CHECK_ENABLED=true \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml
```

The management endpoints have no authentication. `/q/dev` can stop and suspend routes, so keep
the port on a private network, or bind it to the loopback interface with
`camel.server.host=127.0.0.1` when only local tools need it.

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
| the `log` EIP and the `log:` component | log from a route (`log: "${body}"`, `to: log:orders?level=DEBUG&showHeaders=true`) |

A failed exchange is logged with the error handler's report: the exception, the exchange and, with
message history on, the steps it went through. In a container, logs go to the container
runtime; `cameleer run` outside a container also writes them to `$CAMEL_HOME`, where
`cameleer log --follow` reads them. Logs are plain text; there is no JSON log format yet.

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

- **OpenTelemetry** traces and metrics, and Camel's `camel-opentelemetry`, `camel-micrometer`
  and `camel-observation` components: not implemented. Use the Prometheus endpoint for metrics.
- **JMX**: there is no JVM, so there are no MBeans; the same data is in `/q/dev` and `/q/metrics`.
- **JSON logs**: logs are plain text lines.

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
`/q/metrics`. For per-step metrics add `camel.main.jmxManagementStatisticsLevel=Default`.

### Does Cameleer support OpenTelemetry?

Not yet. Metrics are available in the Prometheus format at `/q/metrics`, which an OpenTelemetry
Collector can scrape with its Prometheus receiver.

### How do I see why a message failed?

Turn on message history (`camel.main.message-history=true`): the error log then lists the steps
the message went through. For a live view, run with `--trace`.

### Can I change the log format?

Not yet: the layout is fixed, plain text, one line per event (plus the error report for failed
messages).

## Related

- [Configuration](configuration.md), [the cameleer command line](cli.md),
  [Error handling](error-handling.md)
- [Deployment](../DEPLOYMENT.md): Docker, Kubernetes, Cloud Run, AWS Lambda
- [log](../components/log.md) component
- [Attestation in confidential VMs](attestation.md): `/q/attestation`, evidence of what runs
  (next release)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
