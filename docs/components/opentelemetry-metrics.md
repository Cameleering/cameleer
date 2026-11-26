# OpenTelemetry Metrics component

> The `opentelemetry-metrics` component (Camel 4.17) records counters, histograms and timers from a route with OpenTelemetry's instruments and attributes, exported over OTLP to an OpenTelemetry Collector and served at `/q/metrics`.

**Scheme:** `opentelemetry-metrics:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [OpenTelemetry Metrics](https://camel.apache.org/components/next/opentelemetry-metrics-component.html)

## When to use it

- Routes that report to an OpenTelemetry backend (Grafana Mimir, Datadog, Honeycomb, New Relic)
  through the Collector.
- Routes written for Camel's `opentelemetry-metrics` component.

## URI format

```text
opentelemetry-metrics:[counter|summary|timer]:name[?options]
```

Without a type the endpoint is a counter. The type is not case sensitive.

## Example

<!-- example: opentelemetry-metrics-jobs -->
```yaml
- route:
    id: jobs
    from:
      uri: platform-http:/jobs
      steps:
        - to: opentelemetry-metrics:counter:jobs.queued?attributes.queue=${header.queue}
        - to: opentelemetry-metrics:timer:job.time?action=start
        - to: mock:work
        - to: opentelemetry-metrics:timer:job.time?action=stop
        - to: opentelemetry-metrics:counter:jobs.queued?decrement=1&attributes.queue=${header.queue}
```

```bash
OTEL_METRICS_EXPORTER=otlp OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4318 cameleer run jobs.camel.yaml
```

## Options

Every option except `unit` is a [Simple](../languages/simple.md) expression; numbers are whole
numbers (OpenTelemetry's long instruments).

| option | applies to | meaning |
|---|---|---|
| `attributes.<key>=<value>` | all | an attribute (key and value may use Simple) |
| `metricsDescription` | all | the description |
| `increment`, `decrement` | counter | the amount to add or subtract (default: add 1) |
| `value` | summary | the value to record |
| `action` | timer | `start` or `stop` |
| `unit` | timer | the time unit: `NANOSECONDS`, `MICROSECONDS`, `MILLISECONDS` (default), `SECONDS`, `MINUTES`, `HOURS`, `DAYS` |

The attribute `camelContext` is always added.

## Message headers

As [micrometer](micrometer.md#message-headers), with `CamelMetricsAttributes` (an object, or
`k=v,k2=v2`) instead of `CamelMetricsTags`. Headers are removed after the step.

## Instruments

| type | OpenTelemetry instrument | OTLP | `/q/metrics` |
|---|---|---|---|
| `counter` | `LongUpDownCounter` | non-monotonic sum | gauge |
| `summary` | `LongHistogram` | histogram with the SDK's default bounds (0, 5, 10, ... 10000) | summary |
| `timer` | `LongHistogram`, unit `milliseconds` (the `unit` option, lower case, as Camel) | histogram | summary with the unit as suffix |

## Differences from Apache Camel

- There is no OpenTelemetry SDK or Java agent: the meters go to Cameleer's registry and its OTLP
  exporter (`OTEL_METRICS_EXPORTER`, `OTEL_EXPORTER_OTLP_*`); the `meter` component option is not
  read.
- Exemplars and the exponential histogram aggregation are not there.

## Related

- [OpenTelemetry](../guides/opentelemetry.md#metrics): the OTLP metric exporter and
  `camel.opentelemetry.metrics.*`
- [micrometer](micrometer.md), [metrics](metrics.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
