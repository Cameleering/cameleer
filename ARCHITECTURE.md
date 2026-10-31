# rust-camel architecture

rust-camel is a Rust implementation of the [Apache Camel](https://camel.apache.org/) integration
framework. It is written **without any external crates** (std only): the build environment has
no access to crates.io. Never add dependencies to `Cargo.toml` other than workspace crates.

## Workspace layout

| crate | purpose |
|---|---|
| `crates/camel-core` | Exchange/Message/Value, CamelContext, routes, Rust DSL, EIPs, languages, data formats, core components |
| `crates/camel-http` | HTTP/1.1 server + client (std::net), `http`, `platform-http`, `rest` DSL, `netty` (tcp/udp) |
| `crates/camel-yaml` | YAML 1.2 parser + Camel YAML DSL loader |
| `crates/camel-protocols` | ftp, mail (smtp/pop3/imap), websocket, mllp/hl7, syslog — plus embedded test servers in `testing` |
| `crates/camel-main` | `camel` CLI binary, application.properties, route loading, dev console |
| `crates/camel-tls` | from-scratch TLS 1.3/1.2 client+server (`TlsStream<S: Read+Write>`), crypto primitives, X.509/PEM/PKCS#8, trust store, self-signed cert generator; no deps |
| `crates/camel-grpc` | HPACK + HTTP/2 client/server (`h2`, public for reuse), `.proto` parser + dynamic protobuf codec (`proto`), gRPC framing/status/client/server (`grpc`), Camel `grpc` component and `protobuf` data format |
| `crates/camel-cloud` | AWS (aws2-s3/sqs/sns/ddb/lambda, SigV4), Azure Storage (blob/queue, Shared Key/SAS), Google (storage/pubsub, JWT→OAuth2) over REST with a small HTTPS client (`http.rs`, camel-tls) + in-memory emulators in `testing` that verify signatures/tokens |

## Key concepts (camel-core)

- `value::Value` — dynamic type for bodies, headers, properties (Null/Bool/Int/Float/String/Bytes/Array/Object).
  `Display` renders strings raw and null as `""`; `to_json()` for JSON. `FromValue` = type converter
  (`ex.body_as::<String>()`). `compare_values`/`values_equal` give Camel-like coercing comparisons.
- `error::CamelError { kind, message, cause }` — `kind` is a Java-like exception class name
  (`"IOException"`). `kind_is_a(kind, parent)` models the hierarchy (used by onException/doCatch).
  Constants in `error::kinds`. `register_kind_parent` adds custom kinds.
- `message::Exchange` — `message` (headers are case-insensitive `Headers`, `body: Value`),
  `properties`, `exception`, `pattern` (InOnly/InOut), `context: Option<CamelContext>`,
  `on_completions`. `Clone` copies everything except on-completion callbacks.
  `correlated_copy()` gives a new id. Property names in `message::props`.
- `spi` — traits: `Processor` (closures `Fn(&mut Exchange)->Result<()>` implement it),
  `Expression`, `Predicate`, `Language`, `Endpoint`, `Producer` (= Processor + Service),
  `Consumer` (= Service), `PollingConsumer`, `Component`, `AggregationStrategy`, `EventNotifier`, `Service`.
- **Failure semantics**: a processor returning `Err(e)` marks the exchange failed. Composite
  processors (pipeline etc.) store it with `exchange.set_exception(e)` and stop routing when
  `!exchange.should_continue()` (failed, `stop()`ed, or rollback-only).
- `context::CamelContext` — cheap-clone handle. Components (`add_component`, lazy
  `add_component_factory`), endpoint cache (`get_endpoint(uri)` resolves `{{placeholders}}`,
  parses `scheme:path?options`, fails on unknown options unless the component is lenient),
  `get_producer(uri)` (cached+started), `registry()`, `properties()`, languages, event notifiers,
  routes lifecycle (`start`, `stop`, `start_route`, `stop_route`, `remove_route`...),
  `create_producer_template()`, `mock_endpoint(uri)`.
  Extension points: `add_route_factory_hook(RouteFactoryHook)` and `add_channel_wrapper(ChannelWrapper)`.
- `model` — `ProcessorDefinition` trait (one struct per EIP, `create_processor(&RouteContext)`).
  Block EIPs implement `is_block`/`add_output`/`outputs`. `RouteDefinition` holds the outputs and a
  stack of open blocks (`push_block`, `add`, `end`, `current_block_mut::<T>()`, `unwind_to_block::<T>()`).
  `RouteContext::create_child_processor(&outputs)` builds a pipeline with each child wrapped by the
  channel wrappers (error handler etc.). Register stateful processors/producers with
  `rc.add_service(...)` so they start/stop with the route. `impl_definition_boilerplate!()` macro.
  `RouteDefinition.extensions` and `RouteBuilder.extensions/customizers/generators` hold
  extension-specific config without touching core structs.
- `builder` — `ExpressionDefinition` (language text resolved at route creation, or ready-made
  expression/predicate), DSL helpers `simple()`, `constant()`, `header()`, `body()`,
  `exchange_property()`, `expr(closure)`, `pred(closure)`, `and/or/not`, `ValueBuilder` predicate
  methods (`is_equal_to`, `is_greater_than`, `contains`, `regex`...). DSL methods take `impl IntoExpression`.
- `dsl` — `RouteBuilder::from(uri) -> &mut RouteDefinition`; DSL methods are `impl RouteDefinition`
  blocks split across `dsl/core.rs`, `dsl/routing.rs`, `dsl/errors.rs`, `dsl/transform.rs`.
  Methods take `&mut self` and return `&mut Self`. Block EIPs are closed with `.end()`.
- `executor::ThreadPool` / `Future` — shared thread pool for parallel processing.
- `log` — std-only logging (`camel_info!`, `camel_debug!`...; `CAMEL_LOG_LEVEL`; `add_sink` for tests).
- `json` — JSON parser; `regex` — regex engine; `util` — URI parsing, `Parameters` (`take_bool`,
  `take_duration`...), `parse_duration`, `matches_pattern`; `util_time` — date formatting (`format_in_zone`);
  `tz` — time zones (TZif reader for `/usr/share/zoneinfo`, POSIX TZ rules, `TimeZone::local()` from `TZ`);
  `async_delay` — releasing seda/timer consumer threads during `asyncDelayed` waits.

## Module map (camel-core)

| module | contents |
|---|---|
| `processor/basic.rs` | to, toD, setBody/Header/Property/Variable, remove*, process, log, convertBodyTo, stop, step |
| `processor/routing/` | filter, choice, split, aggregate (+repository, strategies), multicast, recipientList, routingSlip, dynamicRouter, wireTap, enrich/pollEnrich, loop, loadBalance, resequence, sort, pipeline, claimCheck |
| `processor/errors/` | error handlers (default, dead letter channel, none) with redelivery, onException, doTry, onCompletion, intercepts, throttle, delay, sample, idempotentConsumer, circuitBreaker, validate, rollback, threads, policies, route policies; installed by `ErrorsRouteHook` |
| `processor/transform/`, `dataformat/` | marshal/unmarshal; json, csv, base64, string, hex, gzip/zip (own DEFLATE), jacksonXml, urlencoded |
| `language/` | simple, constant, header, exchangeProperty, variable, jsonpath, xpath (+ small XML parser), tokenize, jq, ref, method/bean |
| `component/` | direct, seda, vm, stub, mock, log, timer, scheduler, file, bean/class, ref, controlbus, browse, exec, stream, language, dataset, dataset-test, dataformat, cron/quartz (`cron_expr.rs` = cron parser), mustache, string-template/velocity/freemarker (`templating.rs`, shared `template_support.rs`), json-validator, validator (XML) |
| `management.rs`, `health.rs`, `tracer.rs`, `history.rs` | statistics/operations/Prometheus, health checks, tracer + backlog, message history (one `ManagementState` per context, installed via a route hook) |
| `template_routes.rs`, `supervising.rs` | route templates in the Rust DSL; supervising route controller |
| `regex.rs` | backtracking regex engine with Java syntax |

Other crates: `camel-http` (HTTP/1.1 server and client, platform-http/jetty/undertow/netty-http,
http producer, REST DSL + OpenAPI, netty TCP/UDP), `camel-yaml` (YAML 1.2 parser/emitter, YAML
DSL step registry in `src/dsl.rs`, `src/dsl/eips.rs`, `src/dsl/config.rs`, XML DSL, Kamelets + `kamelet:` component in `src/kamelet.rs`), `camel-main`
(`Main`, `application.properties`, dev console in `console.rs`, YAML REST glue in
`yaml_rest.rs`, `camel` CLI). Feature coverage versus Apache Camel: `docs/FEATURES.md`.

## Conventions

- Mirror Apache Camel names and semantics (EIP names, endpoint options, header/property names,
  exception kinds, Simple language syntax). When in doubt, follow the Camel documentation.
- Every feature needs tests (unit tests in-module and/or integration tests in `tests/`).
  Tests must be deterministic and fast (avoid long sleeps; use mock `assert_is_satisfied`).
- `cargo build` must produce no warnings; `cargo test` must pass for the whole workspace.
- Keep public APIs documented with `///` doc comments.
