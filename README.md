# rust-camel

A Rust implementation of the [Apache Camel](https://camel.apache.org/) integration framework:
routes, endpoints and components, the Enterprise Integration Patterns, expression languages
(Simple, JSONPath, XPath, tokenize, jq...), data formats, a fluent Rust DSL, the YAML/XML DSLs,
a REST DSL, and a `camel` command-line runner.

It is written in **pure Rust using only the standard library** (no external crates).

```rust
use camel_core::prelude::*;

fn main() -> Result<()> {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:hello?period=1000")
            .route_id("hello")
            .set_body(simple("Hello from rust-camel at ${date:now:HH:mm:ss}"))
            .filter(header("CamelTimerCounter").is_greater_than(2))
                .to("log:greetings")
            .end();
    })?;
    ctx.start()?;
    std::thread::sleep(std::time::Duration::from_secs(5));
    ctx.stop()
}
```

Or with the YAML DSL and the CLI:

```yaml
# hello.camel.yaml
- route:
    id: hello
    from:
      uri: timer:hello
      parameters:
        period: 1000
      steps:
        - setBody:
            simple: "Hello ${exchangeProperty.CamelTimerCounter}"
        - log: "${body}"
```

```text
$ cargo run -p camel-main --bin camel -- run hello.camel.yaml --max-seconds 5
```

## Crates

| crate | contents |
|---|---|
| `camel-core` | `CamelContext`, `Exchange`/`Message`/`Value`, routes and the Rust DSL, EIPs, error handling, languages, data formats, core components |
| `camel-http` | HTTP/1.1 server and client, `platform-http`, `http`, REST DSL with OpenAPI, `netty` TCP/UDP |
| `camel-yaml` | YAML parser, Camel YAML DSL, XML DSL, route templates |
| `camel-main` | `Main` runner (application.properties, route discovery) and the `camel` CLI |

See [ARCHITECTURE.md](ARCHITECTURE.md) for the internals, and [docs/FEATURES.md](docs/FEATURES.md) for
the list of what's implemented compared with Apache Camel.

## Building and testing

```text
cargo build
cargo test
```
