# Micrometer component

> The `micrometer` component counts, times and measures messages from a route: counters, timers and distribution summaries with tags, in the context's metric registry, served in the Prometheus format at `/q/metrics` (and `/observe/metrics`) and exported over OTLP.

**Scheme:** `micrometer:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Micrometer](https://camel.apache.org/components/next/micrometer-component.html)

## When to use it

- Count business events (orders placed, payments refused) with tags you choose.
- Time a part of a route (`action=start`, then `action=stop`).
- Record amounts, sizes or scores as a distribution (`summary`).

For Camel's own route and exchange meters, set `camel.metrics.enabled=true` instead
([Observability](../guides/observability.md#camel-metrics)).

## URI format

```text
micrometer:[counter|summary|timer]:name[?options]
```

Without a type the endpoint is a counter (`micrometer:orders` counts `orders`). The type is case
sensitive, as in Camel.

## Example

<!-- example: micrometer-orders -->
```yaml
- route:
    id: orders
    from:
      uri: platform-http:/orders
      steps:
        - to: micrometer:counter:orders.placed?tags.region=${header.region}&tags.channel=web
        - to: micrometer:summary:order.amount?value=${body}
        - to: micrometer:timer:order.processing?action=start
        - to: mock:store
        - to: micrometer:timer:order.processing?action=stop
```

With `camel.server.metricsEnabled=true`, `/q/metrics` then has:

```text
orders_placed_total{camelContext="camel-1",channel="web",region="eu"} 2.0
order_amount_count{camelContext="camel-1"} 2.0
order_amount_sum{camelContext="camel-1"} 74.5
order_processing_seconds_count{camelContext="camel-1"} 2.0
order_processing_seconds_sum{camelContext="camel-1"} 0.0042
order_processing_seconds_max{camelContext="camel-1"} 0.0031
```

## Options

Every option is a [Simple](../languages/simple.md) expression, evaluated for each message.

| option | applies to | meaning |
|---|---|---|
| `tags.<key>=<value>` | all | a tag; key and value may use Simple (`tags.region=${header.region}`) |
| `metricsDescription` | all | the description (Prometheus `# HELP`) |
| `increment` | counter | the amount to add (default 1) |
| `decrement` | counter | the amount to subtract (a Micrometer counter only goes up, so it is ignored, as in Camel) |
| `value` | summary | the value to record; a null value records nothing |
| `action` | timer | `start` or `stop` |

The tag `camelContext` (the context name) is always added. The older `tags=k1=v1,k2=v2` form
still works and logs a warning, as in Camel.

## Message headers

Headers win over the options and are removed after the step.

| header | meaning |
|---|---|
| `CamelMetricsName` | the meter name |
| `CamelMetricsDescription` | the description |
| `CamelMetricsTags` | more tags: an object, or `k=v,k2=v2` |
| `CamelMetricsCounterIncrement`, `CamelMetricsCounterDecrement` | counter amounts |
| `CamelMetricsHistogramValue` | the summary value |
| `CamelMetricsTimerAction` | `start` or `stop` |

A timer keeps its start in the exchange property `timer:<name>` between `start` and `stop`. A
value that is not a number fails the exchange with a `TypeConversionException`.

## Prometheus and OTLP names

As Micrometer's Prometheus registry writes them: dots become underscores, counters end in
`_total`, timers are in seconds with `_seconds_count`, `_seconds_sum` and `_seconds_max`,
summaries have `_count`, `_sum` and `_max`. Over OTLP (`OTEL_METRICS_EXPORTER=otlp`) a counter is
a monotonic sum, a timer a histogram in seconds and a summary a histogram, all cumulative
([OpenTelemetry](../guides/opentelemetry.md#metrics)).

## Differences from Apache Camel

- There is no Micrometer library underneath: the registry is Cameleer's own; `metricsRegistry`
  (the component option and the bean) is not read.
- A summary has no percentiles; the OTLP histogram has bucket counts.

## Related

- [metrics](metrics.md) (Dropwizard) and [opentelemetry-metrics](opentelemetry-metrics.md), on
  the same registry
- [Observability](../guides/observability.md): `/q/metrics` and `camel.metrics.*`

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
