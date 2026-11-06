# Getting started with Cameleer

> Install Cameleer (Docker image, `cargo install` or Rust crates), run a first route and a first
> REST API, and configure them with properties and environment variables, in about ten minutes.

Cameleer is Apache Camel rewritten in Rust: the same routes, endpoint URIs, Enterprise
Integration Patterns and Simple language, as one native binary with no JVM. If you know Camel,
you already know how to write Cameleer routes. Every route on this page runs in the test suite
(`crates/cameleer-main/tests/docs_guides.rs`).

## Install

Pick one of three ways. They run the same routes.

### Docker

The image `pmsudhi/cameleer` on Docker Hub is a single binary on a distroless base, for
`linux/amd64` and `linux/arm64`, about 20 MB to download. Mount your route files and run them:

```sh
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/hello.camel.yaml
```

It runs as a non-root user, works with `--read-only`, and stops gracefully on `SIGTERM`. The
[deployment guide](../DEPLOYMENT.md) covers Docker Compose, Kubernetes and Cloud Run.

### The `cameleer` command line

With a Rust toolchain installed:

```sh
cargo install cameleer-main
cameleer run hello.camel.yaml
```

This installs the `cameleer` command. Its subcommands follow Camel JBang (`run`, `init`, `ps`, `stop`, `log`, ...). To build from source:

```sh
git clone https://github.com/pmsudhi/cameleer && cd cameleer
cargo build --release -p cameleer-main
target/release/cameleer run examples/hello-api.camel.yaml
```

### Rust crates

To embed routes in your own Rust program, depend on `cameleer-core` (the engine, EIPs and Rust
DSL) and the crates for the components you need:

```toml
[dependencies]
cameleer-core = "0.3"
cameleer-http = "0.3"
```

`cameleer-main` bundles every component and runs YAML routes from Rust; see
[Embed routes in a Rust program](#embed-routes-in-a-rust-program) below.

## Your first route

A route has one input (`from`) and a list of steps. Save this as `hello.camel.yaml`:

<!-- example: gs-hello-timer -->
```yaml
- route:
    id: hello
    from:
      uri: timer:hello
      parameters:
        period: 500
        repeatCount: 3
      steps:
        - setBody:
            simple: "Hello from Cameleer, message ${exchangeProperty.CamelTimerCounter}"
        - log: "${body}"
```

Run it with `cameleer run hello.camel.yaml` (or the `docker run` line above). The `timer`
endpoint fires three times, half a second apart; `setBody` builds a message with the
[Simple language](../languages/simple.md); `log` prints `Hello from Cameleer, message 1`, then 2 and 3, to standard output.

Without `repeatCount` the timer runs until you stop the process with Ctrl+C.

## Your first REST API

The REST DSL declares HTTP endpoints and hands each request to a route. Save this as
`hello-api.camel.yaml`:

<!-- example: gs-hello-api -->
```yaml
- rest:
    path: /hello
    get:
      - path: /{name}
        to: direct:greet
- route:
    id: greet
    from:
      uri: direct:greet
      steps:
        - setHeader:
            name: Content-Type
            constant: application/json
        - setBody:
            simple: '{"greeting": "Hello, ${header.name}!"}'
```

```sh
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/hello-api.camel.yaml
curl localhost:8080/hello/Ada      # {"greeting": "Hello, Ada!"}
```

Path parameters such as `{name}` become headers. The server listens on port 8080 by default
(`camel.server.port` changes it). For JSON binding, validation and error responses, see
[examples/rest-api.camel.yaml](../../examples/rest-api.camel.yaml) and the
[REST component](../components/rest.md).

## Configuration

Routes read configuration with property placeholders, as in Camel:

- `{{name}}` and `{{name:default}}` come from `application.properties` (or
  `application.yaml`), environment variables and `--prop name=value`.
- `{{env:NAME}}` and `{{env:NAME:default}}` read an environment variable directly.

<!-- example: gs-config -->
```yaml
- route:
    id: configured
    from:
      uri: "timer:tick?period={{tick.period:1000}}&repeatCount=1"
      steps:
        - setBody:
            simple: "{{greeting}} from {{env:SITE_NAME:the lab}}"
        - log: "${body}"
```

With this `application.properties` next to the route:

```properties
greeting=Good morning
tick.period=200
camel.server.port=9090
```

```sh
cameleer run configured.camel.yaml --properties application.properties
SITE_NAME=plant-7 cameleer run configured.camel.yaml --prop greeting=Hi
```

The first command logs `Good morning from the lab`. Settings are applied in this order, the last
one winning: the properties file, `CAMEL_*` environment variables (`CAMEL_SERVER_PORT=9090`
sets `camel.server.port`), then `--prop`. Spring Boot and Quarkus names
(`server.port`, `spring.profiles.active`) and profiles (`application-dev.properties`) work too.
Never put secrets in route files: use `{{env:...}}` or a secrets vault
(`{{aws:...}}`, `{{azure:...}}`, `{{gcp:...}}`).

## Embed routes in a Rust program

The Rust DSL has the same EIPs as the YAML DSL, with Rust casing:

```rust
use cameleer_core::prelude::*;
use cameleer_http::rest::RestDslExt;

fn main() -> Result<()> {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    ctx.properties().set("camel.server.port", "8080");
    ctx.add_routes(|b| {
        b.rest("/api/greet").get("/{name}").to("direct:greet");
        b.from("direct:greet")
            .set_header("Content-Type", constant("application/json"))
            .set_body(simple(r#"{"greeting": "Hello, ${header.name}!"}"#));
    })?;
    ctx.start()?;
    loop { std::thread::park(); }
}
```

To run YAML route files from your own binary with every component, depend on `cameleer-main`
and use `cameleer_main::new_main()`, then `add_route_file("routes/hello.camel.yaml")` and
`run()` ([example](../../crates/cameleer-main/examples/embed_yaml.rs)).

## Useful commands

| command | what it does |
|---|---|
| `cameleer run route.camel.yaml` | run one or more route files (all route files of the current folder when none is given) |
| `cameleer run --dev route.camel.yaml` | reload route files when they change |
| `cameleer init my-route.camel.yaml` | write a starter route |
| `cameleer ps`, `cameleer stop --all` | list and stop running integrations |
| `cameleer get routes`, `cameleer get health` | inspect a running integration |
| `cameleer export --directory my-service` | turn routes into a standalone Rust project |
| `cameleer catalog components` | list the components |

## Where to go next

- [Concepts](concepts.md): routes, endpoints, exchanges, messages, components and the DSLs.
- [Enterprise Integration Patterns](eips.md): every EIP with a YAML and a Rust example.
- [Error handling](error-handling.md): retries, dead letter channels, `onException`.
- [Cameleer for AI agents](agents.md) and [Cameleer at the edge](edge.md): cookbooks.
- [Migrating from Apache Camel](migrating-from-apache-camel.md) and the [FAQ](faq.md).
- [Examples](../../examples/): real routes for Kafka, S3, Slack and OpenAI, each with a
  `docker run` line.

## Common questions

### Do I need Java or Maven to run Cameleer?

No. Cameleer is a native binary with no JVM. You need Rust only to build it from source or to
embed it; the Docker image and the installed binary need nothing else.

### Can I run my existing Apache Camel YAML routes?

Mostly yes: Camel YAML and XML routes run unchanged when they use components and options
Cameleer supports. Java beans and processors must be rewritten in Rust. See
[Migrating from Apache Camel](migrating-from-apache-camel.md).

### How do I change the HTTP port?

Set `camel.server.port`: `--prop camel.server.port=9090`, the environment variable
`CAMEL_SERVER_PORT=9090`, or `camel.server.port=9090` in `application.properties`.

### Where do log messages go?

To standard output. Set the level with the `CAMEL_LOG_LEVEL` environment variable (`TRACE`,
`DEBUG`, `INFO`, `WARN`, `ERROR` or `OFF`).

## Related

- [README](../../README.md), [deployment guide](../DEPLOYMENT.md), [feature list](../FEATURES.md)
- Apache Camel: [Camel JBang](https://camel.apache.org/manual/camel-jbang.html),
  [YAML DSL](https://camel.apache.org/components/next/others/yaml-dsl.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
