# DataSet Test component

> The `dataset-test` component checks a route's output against expected messages read from another endpoint, such as a file of expected lines.

**Scheme:** `dataset-test:` · **Also:** `test:` (see [test](test.md)) · **Crate:** `cameleer-core` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [DataSet Test](https://camel.apache.org/components/next/dataset-test-component.html)

## When to use it

- Golden-file tests: compare what a transformation route produces with a file of expected results.
- Regression tests for message mappings, where the expected output is kept next to the routes.
- Compare against messages waiting on a queue, such as `seda:expected`.

## URI format

```text
dataset-test:expectedEndpointUri[?options]
```

Options this component does not know are passed on to the expected endpoint URI.

## Example

<!-- example: dataset-test-golden-file -->
```yaml
- route:
    id: normalize-codes
    from:
      uri: direct:normalize
      steps:
        - setBody:
            simple: "${body.toUpperCase()}"
        - to: dataset-test:file:{{env:EXPECTED_DIR:expected}}?noop=true&split=true
```

With a file `expected/codes.txt` that contains `AB-1` and `CD-2` on two lines, sending `ab-1` and `cd-2`
satisfies the endpoint. Check it from Rust:

```rust
use cameleer_core::component::dataset::dataset_test_endpoint;

let uri = "dataset-test:file:expected?noop=true&split=true";
dataset_test_endpoint(&ctx, uri)?.assert_is_satisfied()?;
```

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:normalize")
            .route_id("normalize-codes")
            .set_body(simple("${body.toUpperCase()}"))
            .to("dataset-test:file:expected?noop=true&split=true");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `timeout` | `2000` | How long (ms) to wait for the first expected message when loading them. |
| `anyOrder` | `false` | Accept the messages in any order. |
| `split` | `false` | Split each expected message into lines (or by `delimiter`), one expected message per part. |
| `delimiter` | `\n` | Separator used with `split`. |

## Message headers

None added.

## Run it with Docker

The expectations are checked from Rust test code, so this component is used in tests.

## Common questions

### When are the expected messages read?

On the first `assert_is_satisfied` call: the endpoint polls the expected URI until nothing arrives for
the `timeout`, then compares.

### Can I see what was received?

Yes. `dataset_test_endpoint(&ctx, uri)?.mock()` is a regular [mock](mock.md) endpoint with the received
exchanges.

## Related

- [test](test.md), [dataset](dataset.md), [mock](mock.md)
- [Apache Camel DataSet Test component](https://camel.apache.org/components/next/dataset-test-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
