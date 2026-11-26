# Metrics: design

_2026-11-25. Status: built (second PR after the OpenTelemetry tracing PR); internal design note._

The owner asked for the metrics side after tracing: Camel's `micrometer` component, the
`metrics` (Dropwizard) component and `opentelemetry-metrics` (Camel 4.17), on one std-only
registry, exported through the Prometheus endpoint and over OTLP. Sources: Apache Camel `main`
(`components/camel-micrometer`, `camel-metrics`, `camel-opentelemetry-metrics`,
`camel-micrometer-prometheus`, `core/camel-main` `MetricsConfigurationProperties`), the
camel-spring-boot starter of `camel-opentelemetry-metrics`, Micrometer's Prometheus registry,
opentelemetry-proto and the OpenTelemetry metrics SDK specification. The research notes with a
source path for every fact are kept outside the repository.

## 1. What Camel has

| Camel | what | Cameleer |
|---|---|---|
| `micrometer:[counter\|summary\|timer]:name` | Simple options `metricsDescription`, `tags.k=v`, `action`, `value`, `increment`, `decrement`; headers `CamelMetrics*`; tag `camelContext`; timer sample in the property `timer:<name>` | same (`cameleer_core::component::meters`) |
| `metrics:[gauge\|counter\|histogram\|meter\|timer]:name` (Dropwizard; default type meter) | literal options `action`, `mark`, `value`, `increment`, `decrement`, `subject`; warnings for missing values | same; meter rates and histogram percentiles are not kept |
| `opentelemetry-metrics:[counter\|summary\|timer]:name` | `LongUpDownCounter`, `LongHistogram`; `attributes.k=v`; `unit` (`milliseconds`); header `CamelMetricsAttributes` | same; histograms with the SDK's default bounds |
| Micrometer route policy, exchange and route event notifiers, message history factory, `app.info` | meter names and tags in `MicrometerConstants`; legacy naming | `cameleer_core::metrics::instrument` |
| OpenTelemetry route policy, notifiers, message history | `camel.route.policy` and friends as histograms in a time unit, `camel.exchange.elapsed`, `camel.exchange.sent`, `camel.exchanges.last.time` | same, flavor `OpenTelemetry` |
| `camel.metrics.*` (Camel Main) | `enabled`, `namingStrategy`, `enableRoutePolicy`, `routePolicyLevel`, `enableMessageHistory`, `enableExchangeEventNotifier`, `baseEndpointURIExchangeEventNotifier`, `recordLastExchangeTimestamp`, `enableRouteEventNotifier`, `clearOnReload`, `skipCamelInfo`, `logMetricsOnShutdown*`, `textFormatVersion`, `binders`, `path` | all but `binders` (no JVM; a warning), `clearOnReload` (no route reload events), `textFormatVersion=1.0.0` (refused), `path` (both `/q/metrics` and `/observe/metrics` are served) |
| `camel.opentelemetry.metrics.*` (Spring Boot starter) | `enableRoutePolicy`, `routePolicyLevel`, `routePolicyExcludePattern`, `enableMessageHistory`, `enableExchangeEventNotifier`, `baseEndpointUriExchangeEventNotifier`, `enableRouteEventNotifier` | same, plus `enabled` |
| the OpenTelemetry SDK | `OTEL_METRICS_EXPORTER`, `OTEL_METRIC_EXPORT_INTERVAL`/`_TIMEOUT`, `OTEL_EXPORTER_OTLP_METRICS_*` | same; cumulative temporality only |

## 2. Design

- **One registry per context** (`MeterRegistry::of`, bound in the bean registry): meters keyed by
  name and sorted tags; kinds counter, up-down counter, gauge, function gauge, timer, summary or
  histogram with bucket bounds, Dropwizard meter, long task timer. Recording is atomics only; a
  timer's max is kept over a rotating one-minute window without a lock.
- **Components** cache nothing per message beyond the registry lookup of the evaluated name and
  tags (a read lock and a `BTreeMap` lookup): names and tags may be Simple expressions.
- **Built-in meters** are installed by `instrument::install` (from `Main` when the properties ask):
  a route wrapper (route policy, completed and failed exchanges at the route the exchange started
  in, the last-exchange gauge), a producer wrapper (sent exchanges, the same SPI the tracer uses),
  a channel wrapper (message history) and function gauges (inflight, routes, `app.info`). The
  context-level policy counts an exchange once, in the route whose id is the exchange's
  `from_route_id` (Camel's "route stack level 1").
- **Prometheus**: rendered as Micrometer's registry writes it (`_total`, `_seconds_count`,
  `_seconds_sum`, `_seconds_max`, base units as suffix). The endpoint keeps management's own route
  statistics; a family the registry also has replaces management's (so `camel_exchanges_total`
  carries Camel's `kind` and `eventType` tags once `camel.metrics.enabled` is on), and no name has
  two `# TYPE` lines.
- **OTLP**: `cameleer-opentelemetry::metrics` encodes `ExportMetricsServiceRequest` (one metric per
  name, a data point per tag set, cumulative, start time = registry creation) and a periodic exporter
  thread posts it with the trace exporter's HTTP client, retries and headers.
- **Zero cost when off**: nothing is installed unless `camel.metrics.enabled`,
  `camel.opentelemetry.metrics.enabled` or `OTEL_METRICS_EXPORTER=otlp` is set; the registry is
  created only when a component or those options need it; the endpoint renders it only if it exists.

## 3. Left out

- Micrometer binders (JVM, thread pools, logback), OpenMetrics text, delta and low-memory
  temporality, exemplars, exponential histograms, `clearOnReload`.
- Dropwizard meter rates (1/5/15 minutes) and histogram percentiles; Micrometer percentiles and
  SLO buckets on the Prometheus side (the OTLP histogram has bucket counts).
- The route policy's long task timer (programmatic in Camel, no Main option).
- `camel.routes.reloaded`.
