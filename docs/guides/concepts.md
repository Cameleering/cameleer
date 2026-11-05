# Cameleer concepts

> Cameleer uses Apache Camel's model: a CamelContext runs routes; each route takes exchanges from
> an endpoint, passes their messages (body and headers) through processors and EIPs, and sends
> them to other endpoints created by components.

This page explains each concept, shows how it appears in the YAML, XML and Rust DSLs, and notes
where Cameleer differs from Apache Camel. The examples run in the test suite
(`crates/cameleer-main/tests/docs_guides.rs`).

## The model in one route

<!-- example: concepts-route -->
```yaml
- route:
    id: order-intake
    from:
      uri: direct:orders
      steps:
        - setHeader:
            name: priority
            simple: "${body[total] > 1000 ? 'high' : 'normal'}"
        - setProperty:
            name: receivedAt
            simple: "${date:now:yyyy-MM-dd}"
        - choice:
            when:
              - simple: "${header.priority} == 'high'"
                steps:
                  - to: mock:priority-orders
            otherwise:
              steps:
                - to: mock:orders
```

```rust
b.from("direct:orders")
    .route_id("order-intake")
    .set_header("priority", simple("${body[total] > 1000 ? 'high' : 'normal'}"))
    .set_property("receivedAt", simple("${date:now:yyyy-MM-dd}"))
    .choice()
        .when(simple("${header.priority} == 'high'"))
            .to("mock:priority-orders")
        .otherwise()
            .to("mock:orders")
    .end();
```

- The **CamelContext** loaded this route and runs it.
- `direct:orders` is an **endpoint URI**: the `direct` **component** created the endpoint, and
  the route's **consumer** takes messages from it.
- Each order arrives as an **exchange** holding a **message**. `${body[total]}` reads the
  message **body**; `setHeader` writes a message **header**; `setProperty` writes an
  **exchange property**.
- `setHeader`, `setProperty` and `choice` are **processors**; `choice` is an **EIP**.
- `mock:priority-orders` and `mock:orders` are endpoints too: the route's **producers** send to
  them.

An order with `total` 1500 reaches `mock:priority-orders` with the header `priority: high`.

## CamelContext

The CamelContext is the runtime: it holds the components, the registry of beans, the
properties, the type converters and the routes, and starts and stops them. Routes start in
`startupOrder` and stop in reverse order; stopping waits for in-flight exchanges (45 seconds by
default).

- **YAML and XML:** `cameleer run` (or `cameleer_main::new_main()`) creates the context, registers
  every component and loads the route files.
- **Rust:** `CamelContext::new()` creates an empty context; register the components you need
  (`cameleer_http::register(&ctx)`), add routes with `ctx.add_routes(|b| ...)`, then
  `ctx.start()`. A `CamelContext` is a cheap handle you can clone into threads.

## Routes

A route has one input, `from`, and a list of steps. It has an id (generated as `route1`,
`route2`... when you do not set one), and can be started, stopped, suspended and resumed at run
time (`cameleer cmd stop-route`, the dev console, or `ctx.stop_route(id)`). Options:
`autoStartup`, `startupOrder`, `group`, `description`, `routeConfigurationId`, `errorHandler`.

Route templates and Kamelets create routes from parameters; see
[route templates](eips/system.md#route-templates).

## Endpoints and URIs

An endpoint is addressed by a URI: `scheme:path?option=value&option=value`.

```text
kafka:orders?brokers=localhost:9092&groupId=billing
file:inbox?include=.*\.csv&move=.done
platform-http:/alerts?httpMethodRestrict=POST
```

The scheme picks the component, the path names the resource (a topic, a folder, a URL path), and
the options configure it. Options are checked when the endpoint is created: an unknown or
misspelled option fails the route at startup, as in Camel. URIs accept property placeholders
(`{{env:KAFKA_BROKERS:localhost:9092}}`), and in YAML the options can also go in a `parameters`
map. Endpoints are normalized and cached, so two spellings of the same URI share one endpoint.

`toD` computes the URI per message from a Simple expression (`toD: "kafka:${header.topic}"`).

## Components

A component creates endpoints for one scheme. Cameleer has about 230 components, languages and
data formats, all built in; there is nothing to download. They are grouped in crates:
`cameleer-core` (direct, seda, timer, file, log, mock, bean, rest...), `cameleer-http`,
`cameleer-kafka-mqtt`, `cameleer-cloud` (AWS, Azure, Google Cloud), `cameleer-saas` (OpenAI,
Slack, GitHub...) and others. [FEATURES.md](../FEATURES.md) lists every component with its
supported options; the [component pages](../components/) document them one by one.

An endpoint can be a **consumer** (the `from` of a route: it receives or polls messages) and a
**producer** (a `to`: it sends). Most components are both.

## Exchanges and messages

An **exchange** is one message's trip through a route. It holds:

| part | what it is | in Simple |
|---|---|---|
| message body | the payload: text, bytes, a number, a list or a map (JSON-like) | `${body}`, `${body[field]}` |
| message headers | name/value pairs that travel with the message; components map them to protocol headers (HTTP, Kafka, MQTT, JMS...) | `${header.name}` |
| exchange properties | name/value pairs for the route's own bookkeeping; they are not sent out | `${exchangeProperty.name}` |
| variables | named values set with `setVariable` | `${variable.name}` |
| exception | the error, when a step failed | `${exception.message}` |
| pattern | `InOnly` (fire and forget) or `InOut` (request-reply) | |

Rules worth knowing:

- Header names are case-insensitive and keep their original spelling.
- Headers go out with the message (an `http` call sends them as HTTP headers, `kafka` as record
  headers), so remove what should not leave: `removeHeaders: {pattern: "Camel*"}`. Properties
  stay inside.
- Cameleer has no Java objects: bodies, headers and properties are dynamic values (null, bool,
  integer, float, string, bytes, list, map). JSON maps directly onto them, which is why
  `${body[field]}` works after `unmarshal: json: {}`.
- Camel's names carry over: `CamelFileName`, `CamelHttpResponseCode`, `CamelSplitIndex`,
  `kafka.KEY` and the rest.
- Variables are stored as exchange properties named `CamelVariable.<name>` (Camel has a separate
  variable repository).

## Processors and EIPs

Every step of a route is a processor: something that reads and changes the exchange. The
built-in ones are the EIPs (`filter`, `choice`, `split`, `aggregate`, `circuitBreaker`...) and the
message steps (`setBody`, `setHeader`, `marshal`, `convertBodyTo`...). See
[the EIP guide](eips.md) for all of them.

Custom logic is a processor written in Rust. In the Rust DSL it is a closure:

```rust
b.from("direct:score")
    .process(|ex: &mut Exchange| {
        let text = ex.body().to_string();
        let score = text.split_whitespace().filter(|w| w.eq_ignore_ascii_case("urgent")).count() as i64;
        ex.set_header("urgency", score);
        Ok(())
    })
    .to("mock:scored");
```

To use it from YAML or XML, bind it in the registry (`ctx.registry().bind_processor("scorer",
...)`) and call it with `- process: {ref: scorer}`. Returning
`Err(CamelError::new("IOException", "..."))` fails the exchange and hands it to the
[error handling](error-handling.md). Camel's Java beans with reflection-based parameter binding
do not exist; scripts in Groovy, JavaScript, Python or Java syntax run in Cameleer's own
interpreters ([`script`](eips/transformation.md#script)).

## Expressions, predicates and languages

An **expression** computes a value from the exchange; a **predicate** is an expression used as a
condition. Both are written in a **language**:

- [Simple](../languages/simple.md): `${header.amount} > 100`, `Hello ${body[name]}`, the default
  for most steps.
- JSONPath (`$.items[0].sku`), jq, XPath and XQuery for structured data.
- `constant`, `header`, `exchangeProperty`, `variable` for plain lookups.
- Groovy, JavaScript, Python, MVEL, OGNL, SpEL, DataSonnet and Java-like snippets
  (`cameleer-script`).

In YAML the language is the key (`simple: ...`, `jsonpath: ...`); in Rust it is a function
(`simple("...")`, `jsonpath("$.items")`, `header("id")`, `constant(3)`).

## Data formats and type conversion

A **data format** converts between bytes and structured data: `unmarshal` parses, `marshal`
serializes. JSON, CSV, XML, YAML, Avro, Protobuf, CBOR, Base64, GZip, Zip, PGP, HL7 and others are
built in.

The **type converter** changes a value's type when a step needs it: `convertBodyTo` with Camel's
Java type names (`java.lang.String`, `java.lang.Integer`, `byte[]`), or implicitly when Simple
compares a text with a number.

## The three DSLs

The same route can be written in three ways. All three build the same route model and support
the same EIPs.

**YAML**, Camel's YAML DSL, the usual choice for route files (`*.camel.yaml`): see the route at
the top of this page.

**XML**, Camel's XML DSL (`<routes>`/`<route>` files), converted to the YAML model when loaded:

```xml
<routes xmlns="http://camel.apache.org/schema/spring">
  <route id="order-intake">
    <from uri="direct:orders"/>
    <setHeader name="priority">
      <simple>${body[total] &gt; 1000 ? 'high' : 'normal'}</simple>
    </setHeader>
    <choice>
      <when>
        <simple>${header.priority} == 'high'</simple>
        <to uri="mock:priority-orders"/>
      </when>
      <otherwise>
        <to uri="mock:orders"/>
      </otherwise>
    </choice>
  </route>
</routes>
```

**Rust**, the equivalent of Camel's Java DSL: `RouteBuilder` methods with snake_case names
(`set_header`, `wire_tap`, `to_d`, `loop_`), blocks closed with `.end()`. Use it when routes live
in a Rust program, need custom processors, or should be checked by the compiler.

The REST DSL (`rest:` in YAML, `b.rest(...)` in Rust) declares HTTP APIs in all three.

## How the concepts map to Apache Camel

| Apache Camel | Cameleer | notes |
|---|---|---|
| `CamelContext` | `CamelContext` | a cloneable handle |
| `RouteBuilder.configure()` (Java DSL) | `ctx.add_routes(\|b\| ...)` (Rust DSL) | snake_case method names |
| YAML DSL, XML DSL | YAML DSL, XML DSL | the same files |
| `Exchange`, `Message` | `Exchange`, `Message` | bodies and headers are dynamic `Value`s, not Java objects |
| `Processor` (Java class or lambda) | `Processor` (Rust closure or struct) | |
| exception classes | error kinds (`"IOException"`) with a parent hierarchy | `java.io.IOException` in YAML maps to `IOException` |
| Java beans, `bean:` with parameter binding | processor beans and named bean methods in the registry | no reflection |
| Camel Main, Spring Boot, Quarkus | `cameleer-main` (`application.properties`, profiles, `server.port`) | |
| Camel JBang (`camel run`) | `cameleer run` / `camel run` | the same commands |
| Maven dependencies per component | every component built in | |

## Common questions

### What is the difference between a header and an exchange property?

Headers belong to the message and are sent out by components (as HTTP headers, Kafka record
headers and so on); exchange properties belong to the exchange and never leave the route. Use
properties for intermediate values.

### What is an endpoint URI?

It is the address of an endpoint: `scheme:path?options`, for example
`kafka:orders?brokers=localhost:9092`. The scheme selects the component.

### Do I have to write Rust to use Cameleer?

No. YAML or XML route files cover all EIPs and components. Rust is needed only for custom
processors and for embedding routes in a Rust program.

### How do routes call each other?

With `direct:` (synchronous, in the same thread), `seda:` (asynchronous, through an in-memory
queue) or `vm:` (like seda, across contexts in the same process).

## Related

- [Getting started](getting-started.md), [EIPs](eips.md), [error handling](error-handling.md)
- [Architecture](../../ARCHITECTURE.md): the internals
- Apache Camel: [Architecture](https://camel.apache.org/manual/architecture.html),
  [Exchange](https://camel.apache.org/manual/exchange.html),
  [Endpoint](https://camel.apache.org/manual/endpoint.html)
