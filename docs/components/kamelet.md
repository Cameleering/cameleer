# Kamelet component

> The `kamelet` component uses Kamelets, reusable route snippets with typed parameters, as sources, actions and sinks in a route.

**Scheme:** `kamelet:` · **Crate:** `cameleer-yaml` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Kamelet](https://camel.apache.org/components/next/kamelet-component.html)

## When to use it

- Reuse a connector configured once (a "source" or "sink") in many routes with different parameters.
- Hide connection details from route authors: they only set the documented Kamelet properties.
- Run Kamelet Pipes (`kind: Pipe` / `KameletBinding`) from Kubernetes manifests outside Kubernetes.

Built-in Kamelets, mirroring the Apache Camel catalog: `timer-source`, `log-sink`, `log-action`,
`insert-header-action`, `set-body-action`. Add your own by loading `kind: Kamelet` YAML files.

## URI format

```text
kamelet:name[/routeId][?properties]
```

- `from: kamelet:name` uses a source Kamelet; its `kamelet:sink` feeds your route.
- `to: kamelet:name` uses an action or sink Kamelet; the exchange returns to your route afterwards.

## Example

<!-- example: kamelet-timer-to-log -->
```yaml
- route:
    id: greetings
    from:
      uri: kamelet:timer-source
      parameters:
        message: hello from the edge
        period: 10000
      steps:
        - to:
            uri: kamelet:insert-header-action
            parameters:
              name: site
              value: edge-1
        - to: log:greetings
```

Every 10 seconds the route receives `hello from the edge` with the header `site: edge-1`.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    cameleer_yaml::kamelet::register_kamelet_component(ctx);
    ctx.add_routes(|b| {
        b.from("kamelet:timer-source?message=hello from the edge&period=10000")
            .route_id("greetings")
            .to("kamelet:insert-header-action?name=site&value=edge-1")
            .to("log:greetings");
    })
}
```

## Options

The URI options are the properties of the Kamelet. For the built-ins:

| Kamelet | properties |
|---|---|
| `timer-source` | `message` (required), `period` (`1000`), `contentType` (`text/plain`), `repeatCount` |
| `log-sink` | `loggerName` (`log-sink`), `level` (`INFO`), `logMask`, `multiline`, `showAllProperties`, `showBody`, `showBodyType`, `showExchangePattern`, `showHeaders`, `showProperties`, `showStreams`, `showCachedStreams`, `marker` |
| `insert-header-action` | `name`, `value` |
| `set-body-action` | `value` |
| `log-action` | like `log-sink` |

Property values are resolved from, in order: the URI options, the application properties
`camel.kamelet.<name>.<routeId>.<property>` and `camel.kamelet.<name>.<property>`, then the declared
`default`. A missing required property fails endpoint creation. `kamelet:name/myRoute` sets the id of
the generated route (default `name-N`).

## Message headers

Whatever the Kamelet sets. `timer-source` sets `Content-Type`; `insert-header-action` sets the header
you name.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/greetings.camel.yaml
```

Put your own `kind: Kamelet` files in the same directory and pass them to `run` as well.

## Common questions

### How do I write my own Kamelet?

Write a YAML document with `apiVersion: camel.apache.org/v1`, `kind: Kamelet`, `metadata.name`,
`spec.definition.properties` and a `spec.template` route that ends in `kamelet:sink` (sources) or starts
at `kamelet:source` (sinks and actions). Load it with your routes.

### Can Cameleer run a Kamelet Pipe?

Yes. `kind: Pipe` and the older `kind: KameletBinding` become a route named after `metadata.name`.
Strimzi `KafkaTopic` refs become `kafka:` endpoints and Knative `Channel`/`Broker` refs become `vm:`.

### What does `{{?property}}` mean in a Kamelet template?

It is an optional property: when it has no value, the option that uses it is removed.

## Related

- [timer](timer.md), [log](log.md), [vm](vm.md), [kafka](kafka.md)
- [Apache Camel Kamelet component](https://camel.apache.org/components/next/kamelet-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
