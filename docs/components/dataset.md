# DataSet component

> The `dataset` component load-tests a route: it sends a known set of messages into the route and checks that the same messages come out.

**Scheme:** `dataset:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [DataSet](https://camel.apache.org/components/next/dataset-component.html)

## When to use it

- Soak-test a route with thousands of messages and check that none are lost or reordered.
- Measure throughput with a fixed message rate (`produceDelay`) or a minimum rate (`minRate`).
- Verify that a route chain (seda queues, brokers) delivers every message exactly once.

## URI format

```text
dataset:name[?options]
```

`name` is a data set bound in the registry: a `SimpleDataSet` (N copies of one body), a `ListDataSet`
(a list of bodies) or your own type implementing the `DataSet` trait.

## Example

<!-- example: dataset-load-test -->
```yaml
- route:
    id: load-source
    from:
      uri: dataset:orders
      parameters:
        produceDelay: 1
        initialDelay: 100
      steps:
        - to: seda:work
- route:
    id: system-under-test
    from:
      uri: seda:work
      steps:
        - setHeader:
            name: processed
            constant: true
        - to: dataset:orders
```

Bind the data set in Rust before starting, then wait for all messages to arrive:

```rust
use cameleer_core::component::dataset::{bind_dataset, dataset_endpoint, SimpleDataSet};

bind_dataset(ctx.registry(), "orders", SimpleDataSet::new(500).with_body("order"));
// ... start the routes ...
dataset_endpoint(&ctx, "dataset:orders")?.assert_is_satisfied_within(std::time::Duration::from_secs(30))?;
```

## Rust DSL

```rust
use cameleer_core::component::dataset::{bind_dataset, SimpleDataSet};
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    bind_dataset(ctx.registry(), "orders", SimpleDataSet::new(500).with_body("order"));
    ctx.add_routes(|b| {
        b.from("dataset:orders?produceDelay=1&initialDelay=100")
            .route_id("load-source")
            .to("seda:work");
        b.from("seda:work")
            .route_id("system-under-test")
            .set_header("processed", constant(true))
            .to("dataset:orders");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `produceDelay` | `3` | Consumer: delay (ms) between sent messages. |
| `consumeDelay` | `0` | Producer: delay (ms) after each received message. |
| `preloadSize` | `0` | Consumer: send this many messages at once before applying `produceDelay`. |
| `initialDelay` | `1000` | Consumer: wait (ms) before sending the first message. |
| `dataSetIndex` | `lenient` | `strict` (every message must carry the right `CamelDataSetIndex`), `lenient` (checked when present) or `off`. |
| `minRate` | `0` | Fail when fewer messages per second arrive. |
| `expectedCount` | data set size | Override the number of expected messages. |
| `assertPeriod`, `resultWaitTime`, `retainFirst`, `retainLast`, `reportGroup`, `log`, `copyOnExchange`, `failFast`, `bridgeErrorHandler` | | Accepted and ignored. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelDataSetIndex` | out (consumer), in (producer) | Index of the message in the data set, starting at 0. |

## Run it with Docker

Data sets are bound from Rust code, so this component is used in Rust tests rather than in a plain
container run.

## Common questions

### Where do the messages come from?

From the `DataSet` bound under the endpoint name. `SimpleDataSet::new(n).with_body(b).with_header(k, v)`
and `ListDataSet::new(vec![...])` cover most cases.

### How do I check the result?

`dataset_endpoint(&ctx, "dataset:name")?.assert_is_satisfied_within(timeout)` waits for all messages and
fails on missing, extra or wrongly indexed ones.

## Related

- [dataset-test](dataset-test.md), [test](test.md), [mock](mock.md), [seda](seda.md)
- [Apache Camel DataSet component](https://camel.apache.org/components/next/dataset-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
