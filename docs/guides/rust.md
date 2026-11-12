# Rust developer guide: embed Cameleer, write processors and components

> Use Cameleer as a Rust library: build a `CamelContext`, add routes with the Rust DSL or load YAML route files, write processors, aggregation strategies, data formats and your own components, bind them so YAML routes can use them by name, and send messages with a `ProducerTemplate`. Everything runs on standard threads with no async runtime and no external crates.

**Applies to:** the Rust crates (`cameleer-core` and the component crates, `cameleer-main`) · **Apache Camel:** [Writing components](https://camel.apache.org/manual/writing-components.html), [Processor](https://camel.apache.org/manual/processor.html), [Registry](https://camel.apache.org/manual/registry.html)

Camel's Java extension points have Rust counterparts with the same names:

| Camel (Java) | Cameleer (Rust) | where |
|---|---|---|
| `CamelContext` | `CamelContext` | `cameleer_core::prelude` |
| `RouteBuilder.configure()` | `ctx.add_routes(\|b\| { ... })` | `cameleer_core::prelude` |
| `Processor` | trait `Processor`, or any `Fn(&mut Exchange) -> Result<()>` | `cameleer_core::spi` |
| `AggregationStrategy` | trait `AggregationStrategy`, or any `Fn(Option<Exchange>, Exchange) -> Result<Exchange>` | `cameleer_core::spi` |
| `Component`, `Endpoint`, `Producer`, `Consumer` | traits with the same names | `cameleer_core::spi` |
| `DataFormat` | trait `DataFormat` | `cameleer_core::dataformat` |
| `Registry` (`bindToRegistry`) | `ctx.registry().bind(...)` | `cameleer_core::context::Registry` |
| `ProducerTemplate`, `ConsumerTemplate` | the same, Rust casing | `cameleer_core::prelude` |
| `EventNotifier` | trait `EventNotifier`, `ctx.add_event_notifier(...)` | `cameleer_core::spi` |
| Camel Main | `cameleer_main::Main`, `cameleer_main::new_main()` | `cameleer-main` |
| `SSLContextParameters` | `cameleer_tls::SslContextParameters` | `cameleer-tls` |
| exception classes | `CamelError` with a `kind` (`"IOException"`) | `cameleer_core::error` |

## Which crates

| you want | depend on |
|---|---|
| the engine, every EIP, the Rust DSL, the core components (`direct`, `seda`, `timer`, `mock`, `log`, `file`, ...) and the core languages and data formats | `cameleer-core = "0.5"` |
| a component family | its crate, then call its `register(&ctx)`: `cameleer-http` (`cameleer_http::register`), `cameleer-kafka-mqtt`, `cameleer-sql`, `cameleer-cloud`, ... |
| YAML and XML route files, `application.properties`, every component, the management server: what the `cameleer` binary does | `cameleer-main = "0.5"` |

`cameleer-main` has a cargo feature per component family (`full` by default), so a program can
leave out what it does not use: see [Build features](build-features.md). The API reference for
every crate is on docs.rs ([cameleer-core](https://docs.rs/cameleer-core),
[cameleer-main](https://docs.rs/cameleer-main)).

## Routes in the Rust DSL

```rust
use cameleer_core::prelude::*;

fn main() -> Result<()> {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:orders?period=1000")
            .route_id("orders")
            .set_body(constant(r#"{"id": 1, "priority": "high"}"#))
            .choice()
            .when(simple("${body} contains 'high'"))
            .to("log:express")
            .otherwise()
            .to("log:standard")
            .end();
    })?;
    ctx.start()?;
    std::thread::sleep(std::time::Duration::from_secs(5));
    ctx.stop()
}
```

The same route in YAML runs with the CLI or the container image, with no Rust toolchain:

<!-- example: rust-orders-yaml -->
```yaml
- route:
    id: orders
    from:
      uri: timer:orders?period=1000
      steps:
        - setBody:
            constant: '{"id": 1, "priority": "high"}'
        - choice:
            when:
              - simple: "${body} contains 'high'"
                steps:
                  - to: log:express
            otherwise:
              steps:
                - to: log:standard
```

```bash
cameleer run orders.camel.yaml
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/orders.camel.yaml
```

Every EIP has a Rust method with Camel's name in snake case (`split`, `aggregate`,
`recipient_list`, `circuit_breaker`, `on_exception`, ...); [the EIP pages](eips.md) show each one
in YAML and in Rust, and [Error handling](error-handling.md) shows `error_handler` and
`on_exception`. Expressions come from the prelude: `simple`, `constant`, `header`, `body`,
`exchange_property`, `jsonpath`, `xpath`, `jq`, `tokenize`, `method`, and `language(name, text)`
for any other language.

## A worked example: a component, a processor and a strategy used from YAML

This program adds a `greet:` component, binds a processor named `upper` and an aggregation
strategy named `joinLines`, adds two routes in the Rust DSL, and loads YAML route files that use
the processor by name:

```rust
use std::any::Any;
use std::path::Path;
use std::sync::Arc;

use cameleer_core::prelude::*;

/// `greet:NAME?punctuation=!` replaces the body with "Hello, BODY!".
struct GreetComponent;

struct GreetEndpoint {
    uri: String,
    punctuation: String,
}

struct GreetProducer {
    punctuation: String,
}

impl Component for GreetComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, _remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        // take the options you know; an option left over fails, as in Camel
        let punctuation = params.take_string("punctuation", ".");
        Ok(Arc::new(GreetEndpoint { uri: uri.to_string(), punctuation }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Endpoint for GreetEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(GreetProducer { punctuation: self.punctuation.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Service for GreetProducer {}

impl Processor for GreetProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let greeting = format!("Hello, {}{}", exchange.body(), self.punctuation);
        exchange.set_body(greeting);
        Ok(())
    }
}

/// Every built-in component, plus `greet:`, the `upper` processor that the YAML routes in
/// `routes` refer to by name, and the `joinLines` aggregation strategy.
fn start(routes: &Path) -> Result<cameleer_main::Main> {
    let mut main = cameleer_main::new_main();
    let ctx = main.context().clone();
    ctx.add_component("greet", GreetComponent);
    ctx.registry().bind_processor("upper", |ex: &mut Exchange| -> Result<()> {
        let upper = ex.body().to_string().to_uppercase();
        ex.set_body(upper);
        Ok(())
    });
    ctx.registry().bind_aggregation_strategy("joinLines", |old: Option<Exchange>, mut new: Exchange| -> Result<Exchange> {
        if let Some(old) = old {
            let joined = format!("{}\n{}", old.body(), new.body());
            new.set_body(joined);
        }
        Ok(new)
    });
    // Rust DSL routes next to the YAML ones: split a list, send each item to the YAML route
    // `greet-one`, and join the replies with `joinLines`
    ctx.add_routes(|b| {
        b.from("direct:greetings")
            .split(simple("${body}"))
            .aggregation_strategy_ref("joinLines")
            .to("direct:greet-one")
            .end();
        b.from("direct:hello").to("greet:hello?punctuation=!");
    })?;
    main.add_route_file(routes);
    main.start()?;
    Ok(main)
}
```

The Rust route `direct:greetings` splits a list and sends each item to the YAML route, which runs
`upper` on it and sends it through the second Rust route to `greet:`; `joinLines` joins the
replies:

<!-- example: rust-greetings -->
```yaml
- route:
    id: greet-one
    from:
      uri: direct:greet-one
      steps:
        - process:
            ref: upper
        - to: direct:hello
```

A YAML route can name a Rust aggregation strategy too (`aggregationStrategy: joinLines` on a
`split`), but `cameleer validate` reports a strategy that is not bound as an error, and it
cannot see one bound in code; a processor reference (`process: {ref: ...}`) passes.

This example needs a Rust program: the `cameleer` binary and the container image cannot load a
component, processor or strategy written in Rust. To ship routes that need them as one binary
or image, `cameleer export` writes a Rust project around your route files
([`cameleer export`](cli.md#cameleer-export)), and you add the code there.

`start(Path::new("greetings.camel.yaml"))`, then
`request_body("direct:greetings", vec!["ann", "bob"])` on a producer template returns
`Hello, ANN!` and `Hello, BOB!` on two lines. `greet:x?colour=red` fails to resolve, because the
component did not take `colour`. To block until Ctrl+C instead of returning, call `main.run()`
in place of `main.start()`. The example runs in the test suite as written
(`rust_guide_worked_example` in `crates/cameleer-main/tests/rust_guide/mod.rs`, which also checks
that this page shows the same code).

## Processors

A processor is anything that implements `Processor`; a closure
`|ex: &mut Exchange| -> Result<()>` already does. It reads and changes the exchange in place:
`ex.body()`, `ex.set_body(...)`, `ex.header("name")`, `ex.message.headers`, `ex.property(...)`
and `ex.set_property(...)`. Returning `Err` fails the exchange, and the route's error handler and
`onException` clauses take over, as when a Java processor throws:

```rust
use cameleer_core::prelude::*;

fn check_amount(ex: &mut Exchange) -> Result<()> {
    let amount: i64 = ex.body().to_string().trim().parse()
        .map_err(|e| CamelError::new(kinds::NUMBER_FORMAT, format!("not an amount: {e}")))?;
    if amount <= 0 {
        return Err(CamelError::illegal_argument("the amount must be positive"));
    }
    ex.set_header("amount", amount);
    Ok(())
}
```

Use it in the Rust DSL with `.process(check_amount)`, or bind it with
`ctx.registry().bind_processor("checkAmount", check_amount)` and refer to it from YAML with
`process: {ref: checkAmount}`. Errors carry a kind named like Camel's Java exception classes, so
an `onException` for `NumberFormatException` catches the first error and one for
`IllegalArgumentException` the second; the kinds and their hierarchy are in [Troubleshooting](troubleshooting.md#exception-kinds).
A custom kind gets a parent with `cameleer_core::error::register_kind_parent("OrderRejected",
"IllegalArgumentException")`.

## Beans: what YAML routes can refer to by name

YAML routes cannot create Rust values from a class name, so a program binds them in the
context's registry first, and the routes refer to them by name (a leading `#` or `#bean:` is
accepted):

| bind in Rust | refer to it from YAML |
|---|---|
| `registry().bind_processor("name", p)` | `process: {ref: name}`, `to: bean:name` |
| `registry().bind_aggregation_strategy("name", s)` | `aggregationStrategy: name` on `split`, `aggregate`, `multicast`, `enrich`, ... |
| `registry().bind("name", value)` | any option that takes a bean: `idempotentRepository`, `sslContextParameters=#name`, ... |
| `cameleer_core::dataformat::bind_data_format(registry, "name", df)` | `marshal: {custom: {ref: name}}` and `unmarshal` |
| `cameleer_yaml::register_bean_factory("com.acme.Type", f)` | a `beans:` element or `camel.beans.<name>=#class:com.acme.Type` that names that type |

The built-in aggregation strategies (`useLatest`, `useOriginal`, `groupedBody`, ...) work by name
with nothing bound. A bean factory receives the context and the bean definition (its `name`,
`type_name` and `properties`) and binds whatever it builds, which lets one Rust type be configured
several ways from YAML or from `application.properties`. Without a factory for its type, a
`beans:` entry fails with `No bean factory registered for bean 'name'`.

## Components

A component is a factory of endpoints for a URI scheme. The worked example above shows the whole
shape:

- `Component::create_endpoint` gets the full URI, the part after `scheme:` and the query options
  as `Parameters`. Take each option you support (`take_string`, `take_bool`, `take_u64`,
  `take_duration`, ...). Whatever is left fails the endpoint with Camel's "Unknown parameters"
  error, unless the component returns `true` from `lenient_properties`.
- `Endpoint::create_producer` returns a `Producer`, which is a `Processor` plus a `Service`
  (`start` and `stop`, both optional).
- `Endpoint::create_consumer(processor)` returns a `Consumer` for `from:`. It is a `Service`:
  `start` opens the connection or thread, and each received message becomes an `Exchange`
  passed to `processor.process(...)`. `Endpoint::create_polling_consumer` serves `pollEnrich`
  and `ConsumerTemplate`.
- `ctx.add_component("scheme", component)` registers it; `add_component_factory` creates it on
  first use. `camel.component.<scheme>.<option>` properties become defaults for the options.

The [`spi` module docs](https://docs.rs/cameleer-core/latest/cameleer_core/spi/) describe every
method, and every component in the repository is an example: `crates/cameleer-core/src/component/`
has the simplest ones (`direct`, `seda`, `timer`, `mock`).

## Data formats

A data format implements `DataFormat` (`name`, `marshal`, `unmarshal`, each changing the body of
the exchange). `bind_data_format` registers one instance under a name. To take options from YAML
(`marshal: {avro: {schema: ...}}`), register a factory instead:
`cameleer_core::dataformat::register_data_format_factory(ctx.registry(), "avro", |options| ...)`,
which receives the options as a map and returns the format; a factory takes precedence over a
built-in format of the same name.

## TLS from Rust

Most TLS needs no code: `camel.ssl.*` properties and each component's SSL options are described
in [TLS and security](tls-and-security.md). From Rust, build an `SslContextParameters` and either
make it the global one (used by endpoints with `useGlobalSslContextParameters=true`) or bind it
under a name for `sslContextParameters=#name`:

```rust
use cameleer_tls::{PemSource, SslContextParameters};

let partner = SslContextParameters {
    trust_store: vec![PemSource::File("certs/partner-ca.pem".into())],
    cert_chain: Some(PemSource::File("certs/client.pem".into())),
    private_key: Some(PemSource::File("certs/client.key".into())),
    ..Default::default()
};
ctx.registry().bind("partnerTls", partner.clone());
cameleer_tls::camel_support::set_global_ssl_context_parameters(&ctx, partner);
```

## Configuration and Main

`cameleer_main::new_main()` returns a `Main` with every component of the build registered and the
YAML and XML route loaders installed. Before `start()` or `run()`:

| method | does |
|---|---|
| `add_route_file(path)` | load a route file (`.camel.yaml`, `.xml`, ...) at start |
| `add_route_source(name, text)` | load routes from a string, for example `include_str!`, to ship one binary |
| `configure(\|ctx\| ...)` | run Rust code against the context at start (add routes, bind beans) |
| `load_application_config(dir)`, `load_properties_file(path)` | read `application.properties` or `.yaml` |
| `set_property(key, value)`, `set_default_property(key, value)` | a property that overrides every other source, or one that applies only when nothing else sets it |
| `set_profile("prod")` | activate a profile |
| `context()` | the `CamelContext`, for templates, the registry and routes |
| `start()`, `run()`, `stop()` | start and return; start and block until Ctrl+C or a `camel.main.duration*` limit; stop |

Properties are the same keys as for the binary: see [Configuration](configuration.md). With only
`cameleer-core`, `ctx.properties().set("key", "value")` sets a placeholder value directly.

## Threads, not async

Cameleer uses standard library threads and blocking I/O; it has no async runtime and depends on
no external crates. Consumers run on their own threads, `seda` and `threads` use thread pools,
and a `ProducerTemplate` call blocks until the route returns. From async code (tokio, async-std),
call templates inside `spawn_blocking` or from a dedicated thread, and keep the `CamelContext`
in an `Arc` or a static: it is `Send + Sync` and cheap to clone.

## Faster small allocations

Routing allocates many small, short-lived values for each message. The `cameleer` binary installs
Cameleer's thread-caching allocator, which is opt-in for a program of your own:

```rust
#[global_allocator]
static GLOBAL: cameleer_core::alloc::CamelAllocator = cameleer_core::alloc::CamelAllocator;
```

It keeps freed small blocks for reuse instead of returning them to the operating system; the
measurements are in [the benchmarks](../BENCHMARKS.md).

## Testing

`ProducerTemplate` and `mock:` endpoints are how Rust tests drive and check routes: see
[Testing routes](testing.md#rust-tests).

## Common questions

### Can I use Cameleer from an async (tokio) application?

Yes. Template calls block, so run them with `tokio::task::spawn_blocking` or on a thread of your
own. Cameleer starts its own threads for consumers and does not need, or interfere with, the async
runtime.

### How do I call my Rust code from a YAML route?

Bind a processor in the registry (`ctx.registry().bind_processor("name", ...)`) before the routes
start, and use `process: {ref: name}` or `to: bean:name`. See the
[worked example](#a-worked-example-a-component-a-processor-and-a-strategy-used-from-yaml).

### Is there a Java bean or `@Handler` method binding?

No. Java beans and their methods are not available; a bean is a Rust value bound in the registry,
and a processor is a function. See [What Cameleer does not do](limitations.md).

### How do I write a component that receives messages?

Implement `Endpoint::create_consumer`: return a `Service` whose `start` begins listening (on a
thread of its own), creates an `Exchange` for each message (`Exchange::new(Some(ctx.clone()))`, then
`set_body`) and calls the processor it was given. The `timer` and `seda` components in
`crates/cameleer-core/src/component/` are small examples.

### Does Cameleer pull in dependencies?

No. Every crate uses only the Rust standard library: TLS, HTTP, the protocols, JSON, YAML and XML
are written in the project.

## Related

- [Getting started](getting-started.md), [Configuration](configuration.md),
  [Build features](build-features.md)
- [EIPs](eips.md), [Error handling](error-handling.md), [Testing routes](testing.md)
- [TLS and security](tls-and-security.md), [Troubleshooting](troubleshooting.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
