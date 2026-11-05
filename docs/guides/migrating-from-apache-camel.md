# Migrating from Apache Camel

> Camel YAML and XML routes, endpoint URIs, EIPs, the Simple language, headers and error
> handling carry over to Cameleer unchanged; Java code (processors, beans, custom components)
> must be rewritten in Rust, and a few behaviours differ, listed on this page.

Cameleer follows Apache Camel 4's names and semantics, so most of a migration is running your
existing route files and checking that every component and option you use is supported. This
page lists what carries over, what differs, a checklist, and the known deviations.

## What carries over unchanged

- **Route files.** Camel YAML DSL (`*.camel.yaml`) and XML DSL (`<routes>`) files, including
  `routeConfiguration`, `routeTemplate`/`templatedRoute`, `rest`/`restConfiguration`, Kamelets,
  Pipes and Camel K `Integration` resources.
- **Endpoint URIs and options**, with the same names and defaults for the supported components.
  Unknown options fail at startup, as in Camel.
- **EIPs**: names, options and semantics, including the edge cases (a split without a strategy
  continues with the original message, `completionTimeout` is an inactivity timeout, duplicates
  skip only the idempotent block, the last reply wins in a multicast without a strategy).
- **Languages**: Simple (operators, functions, OGNL navigation, ternary), JSONPath, XPath,
  XQuery, jq (with camel-jq's `header()` and `property()`), constant, header, exchangeProperty.
- **Headers and properties**: `CamelFileName`, `CamelHttpResponseCode`, `CamelSplitIndex`,
  `CamelRedeliveryCounter`, `kafka.KEY`, `CamelMqttTopic` and the rest.
- **Error handling**: `errorHandler` (default, dead letter channel, none), redelivery policies,
  `onException` with `handled`/`continued`/`onWhen`/`retryWhile`, `doTry`/`doCatch`/`doFinally`,
  `onCompletion`. Java exception class names in routes are matched by their simple name.
- **Configuration**: `application.properties` and `application.yaml`, `{{placeholders}}` with
  defaults, `{{env:NAME}}`, profiles, `camel.component.<scheme>.<option>`, Spring Boot and Quarkus
  key aliases, `CAMEL_*` environment variables.
- **Tooling**: the `camel` command (`run`, `init`, `ps`, `stop`, `get`, `log`, `trace`,
  `export`, `bind`, `catalog`), health checks at `/q/health`, metrics, the dev console, message
  history and tracing.

This route uses Camel-specific spellings (fully qualified exception and strategy class names, a
`parameters` map, a `routeConfiguration`, a placeholder with a default) and runs as written:

<!-- example: migrate-camel-yaml -->
```yaml
- routeConfiguration:
    onException:
      - onException:
          exception:
            - java.lang.IllegalArgumentException
          handled:
            constant: true
          steps:
            - setBody:
                simple: "rejected: ${exception.message}"
            - to: mock:rejected
- route:
    id: order-lines
    from:
      uri: direct:orders
      steps:
        - unmarshal:
            json:
              library: Jackson
        - filter:
            simple: "${body[lines]} == null"
            steps:
              - throwException:
                  exceptionType: java.lang.IllegalArgumentException
                  message: "order ${body[id]} has no lines"
        - split:
            simple: "${body[lines]}"
            aggregationStrategy: "#class:org.apache.camel.processor.aggregate.GroupedBodyAggregationStrategy"
            steps:
              - setBody:
                  simple: "${body[sku]} x ${body[qty]}"
        - to:
            uri: mock:order-summary
            parameters:
              retainFirst: "{{summary.retain:10}}"
```

An order `{"id": 7, "lines": [{"sku": "A1", "qty": 2}, {"sku": "B2", "qty": 1}]}` reaches
`mock:order-summary` as `["A1 x 2", "B2 x 1"]`; an order without lines goes to `mock:rejected`.

## What differs

| area | Apache Camel | Cameleer |
|---|---|---|
| runtime | JVM, Maven or Gradle | one native binary, or Rust crates |
| custom processors | Java classes or lambdas | Rust closures or structs (`Processor` trait), bound in the registry for YAML/XML |
| beans | Java beans with reflection-based parameter binding | processor beans and named bean methods; no reflection |
| message bodies | Java objects, POJO binding (`unmarshalType`) | dynamic values: null, bool, numbers, text, bytes, lists, maps; JSON maps onto them, no POJOs |
| exceptions | Java exception classes | string kinds with a parent hierarchy; `register_kind_parent` adds your own |
| scripts | Groovy, JavaScript (GraalJS), Python (Jython), Java (jOOR) on the JVM | Cameleer's own interpreters for the same syntaxes, without Java libraries |
| components | about 300 | about 230 components, languages and data formats; see [FEATURES.md](../FEATURES.md) |
| statistics | collected only with `camel-management` | always on; route and context counters by default, per-step timing opt-in (`camel.main.jmxManagementStatisticsLevel=Default`) |
| management | JMX | JSON and Prometheus export, health checks, dev console; no JMX |
| transactions | Spring or JTA transaction managers | a `TransactionManager` trait with Camel's propagation behaviours; no JTA/XA |
| variables | a variable repository | stored as exchange properties named `CamelVariable.<name>` |
| aggregation and idempotent repositories | memory, JDBC, LevelDB, Infinispan, Redis... | memory, JDBC and Redis |
| deployment to Kubernetes | Camel K operator, Kubernetes traits | `kind: Integration` files run in-process; deploy the container image yourself |

## Migration checklist

1. **Inventory the components.** List every scheme your routes use and check it in
   [FEATURES.md](../FEATURES.md) or the [component pages](../components/), including the options.
   `cameleer catalog components` prints the list.
2. **Run the routes as they are.** `cameleer run routes/` (or the Docker image). Startup fails on
   an unknown component, an unknown endpoint option or an unsupported YAML element, with the file
   and line.
3. **Replace Java code.** Rewrite processors and beans as Rust processors and bind them in the
   registry under the same names, or replace them with EIPs and Simple, jq or JSONPath
   expressions (often possible for mapping code), or with a `script` step.
4. **Check type-dependent logic.** Steps that relied on POJO types (`bodyAs(Order.class)`,
   `unmarshalType`, typed beans) work on maps instead: `${body[field]}`, JSONPath, jq.
5. **Move configuration.** Keep `application.properties`; replace Spring XML beans with
   `camel.beans.*` entries or Rust code; Spring Boot keys (`server.port`,
   `spring.profiles.active`) are understood.
6. **Test with the same tests.** Camel's `mock:` endpoint, `ProducerTemplate`,
   `interceptSendToEndpoint` with `skipSendToOriginalEndpoint`, and the embedded test servers
   (Kafka, MQTT, AMQP, SQL, Redis, MongoDB, S3, SQS, OpenAI...) make hermetic tests possible.
7. **Review the deviations below**, then load-test: throughput and memory differ a lot (see the
   [benchmarks](../BENCHMARKS.md)), so thread pools and queue sizes tuned for a JVM may be too
   large.

## Known deviations

These differences are documented on purpose (from the conformance suite in
[FEATURES.md](../FEATURES.md#known-deviations-found-by-the-conformance-suite)) or found while
writing these guides:

1. In Simple text, `$${` is an escape that produces a literal `${`.
2. TLS comes from Cameleer's own `cameleer-tls` crate, not a mature, audited library.
3. Asynchronous delays (`asyncDelayed`, `asyncDelayedRedelivery`) hand the waiting exchange to a
   released thread and start a replacement consumer thread, instead of scheduling a continuation
   on a scheduler pool. The effect on the route is the same.
4. When an `http` call fails with an HTTP error status, Cameleer's `http` producer has already put
   the response body and headers into the message (Apache Camel leaves the message unchanged and
   puts the response in the `HttpOperationFailedException`). A redelivery of that step then sends
   the error response instead of the original request. Until this is fixed, call the API with
   `enrich` (which sends a copy of the message) when the call is retried; see the
   [tool API](agents.md#expose-a-tool-to-an-agent-over-rest) and
   [store and forward](edge.md#store-and-forward-when-the-uplink-is-down) examples.
5. `throttle` uses Camel's classic time-period mode (`timePeriodMillis`); the maximum must be a
   constant.
6. Management statistics default to route and context counters (see the table above).

## Common questions

### Is Cameleer a drop-in replacement for Apache Camel?

For route files, mostly: Camel YAML and XML routes run unchanged when their components and
options are supported. Java code is not: processors, beans and custom components must be
rewritten in Rust.

### Can I run Camel and Cameleer side by side?

Yes. They talk through the same brokers, APIs and files, so you can move routes one at a time,
for example the high-volume or edge routes first.

### Do Camel Kamelets work?

Yes. YAML Kamelets (`spec.definition`, `spec.template`), Pipes and KameletBindings run, loaded
from `camel.main.kamelet-locations` (default `./kamelets`). Remote `github:` locations are not
fetched.

### What about Camel's Java DSL?

The Rust DSL mirrors it method by method (`from`, `filter`, `split`, `aggregate`,
`on_exception`...), so a Java route translates line by line. See [concepts](concepts.md).

## Related

- [Concepts](concepts.md), [FAQ](faq.md), [feature list](../FEATURES.md)
- Apache Camel: [YAML DSL](https://camel.apache.org/components/next/others/yaml-dsl.html),
  [Camel 4 migration guide](https://camel.apache.org/manual/camel-4-migration-guide.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
