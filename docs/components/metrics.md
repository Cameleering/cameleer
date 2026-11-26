# Metrics component

> The `metrics` component is Camel's Dropwizard Metrics component: counters, meters, histograms, timers and gauges updated from a route, kept in the context's metric registry and served at `/q/metrics` with the other meters.

**Scheme:** `metrics:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Metrics](https://camel.apache.org/components/next/metrics-component.html)

## When to use it

- Routes written for Camel's `metrics:` component move over unchanged.
- For new routes, prefer [micrometer](micrometer.md): it has tags.

## URI format

```text
metrics:[gauge|counter|histogram|meter|timer]:name[?options]
```

Without a type the endpoint is a meter (`metrics:hits` marks `hits`), as in Camel.

## Example

<!-- example: metrics-jobs -->
```yaml
- route:
    id: jobs
    from:
      uri: timer:jobs?period=1000
      steps:
        - to: metrics:counter:jobs.started?increment=1
        - to: metrics:meter:jobs.rate
        - to: metrics:histogram:jobs.size?value=42
        - to: metrics:timer:jobs.time?action=start
        - to: mock:work
        - to: metrics:timer:jobs.time?action=stop
```

## Options

The values are numbers given in the URI (not expressions), as in Camel.

| option | applies to | meaning |
|---|---|---|
| `increment`, `decrement` | counter | the amount to add or subtract (default: add 1) |
| `mark` | meter | how many events to mark (default 1) |
| `value` | histogram | the value to record |
| `action` | timer | `start` or `stop` |
| `subject` | gauge | the gauge's value (a number) |

## Message headers

| header | meaning |
|---|---|
| `CamelMetricsName` | the metric name |
| `CamelMetricsCounterIncrement`, `CamelMetricsCounterDecrement` | counter amounts |
| `CamelMetricsMeterMark` | meter mark |
| `CamelMetricsHistogramValue` | histogram value |
| `CamelMetricsTimerAction` | `start` or `stop` |
| `CamelMetricsGaugeSubject` | the gauge's value |

Headers win over the options and are removed after the step. A missing value, a timer that is
already running or not found, and a missing action log a warning, as in Camel.

## Prometheus and OTLP

Dropwizard metrics have no tags. In `/q/metrics` a counter (it goes up and down) is a gauge, a
meter is a counter (`_total`), a histogram is a summary (`_count`, `_sum`, `_max`) and a timer is
a summary in seconds. Over OTLP the counter is a non-monotonic sum and the histogram and timer
are histograms.

## Differences from Apache Camel

- The meter's 1, 5 and 15 minute rates and the histogram's percentiles are not kept: the mean
  rate, counts, sums, the maximum and bucket counts are.
- There is no JMX or SLF4J reporter; the meters are on the Prometheus endpoint and OTLP.
- A gauge's subject must be a number.
- The `metricRegistry` bean is not read: every metric component shares the context's registry.

## Related

- [micrometer](micrometer.md), [opentelemetry-metrics](opentelemetry-metrics.md)
- [Observability](../guides/observability.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
