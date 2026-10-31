# rust-camel feature matrix

This page compares rust-camel with [Apache Camel 4](https://camel.apache.org/). It was written
by reading the code and the conformance test suite (`crates/camel-core/tests/conformance_*.rs`,
`crates/camel-http/tests/conformance_http.rs`), not from the wishlist.

Legend: ✅ implemented, ⚠️ partial (see notes), ❌ not implemented.

rust-camel uses only the Rust standard library, so anything that needs TLS, a database driver,
a broker client or a JVM library is out of scope unless it can be written from scratch.

## Core concepts

| Feature | Status | Notes |
|---|---|---|
| CamelContext lifecycle (start/stop/restart) | ✅ | Routes start by `startupOrder` and stop in reverse order. Default shutdown timeout is 45 s (`set_shutdown_timeout`). |
| Graceful shutdown | ✅ | Waits for in-flight exchanges. Exchanges still queued on a seda consumer are completed first unless `purgeWhenStopping=true`. |
| Adding/removing routes at runtime | ✅ | Routes added after start begin at once. `remove_route` returns `false` until the route is stopped, as in Camel. |
| Route ids, auto-generated ids (`route1`...) | ✅ | |
| `autoStartup`, `startupOrder`, route groups/descriptions | ✅ | |
| Suspend/resume | ⚠️ | A suspended route stops its consumer. Exchanges stay queued (seda) and are consumed after resume. |
| Exchange / Message / headers | ✅ | Headers are case-insensitive and keep the original key spelling. |
| Exchange patterns InOnly/InOut, `inOnly()`/`inOut()`/`setExchangePattern` | ✅ | |
| Exchange properties, variables (`setVariable`, `${variable.x}`) | ✅ | Variables are stored as exchange properties (`CamelVariable.<name>`), not in a separate variable repository. |
| `Exchange.correlated_copy` (`CamelCorrelationId`) | ✅ | |
| Type conversion (`convertBodyTo`, `bodyAs`) | ⚠️ | Bodies are a dynamic `Value` (null/bool/int/float/string/bytes/array/object). Java class names such as `String`, `java.lang.String`, `Integer` and `byte[]` are mapped to these. There is no pluggable `TypeConverter` registry. |
| Unit of work, on-completion synchronizations | ✅ | `useOriginalMessage` follows Camel's unit-of-work scope: it spans direct/seda routes, and split/multicast/recipientList start a child unit of work unless `shareUnitOfWork`. |
| Property placeholders `{{key}}`, `{{key:default}}`, `{{env:X}}` | ✅ | Resolved in uris, endpoint options and Simple expressions when the route is created. |
| Registry / beans | ✅ | A typed registry (`bind`, `lookup`, processors, aggregation strategies, bean methods). |
| Event notifiers | ✅ | Context, route and exchange events. |
| Endpoint uri normalization | ✅ | Like Camel's `URISupport.normalizeUri`: `Endpoint::uri()`, route endpoints, `CamelToEndpoint`, `CamelRecipientListEndpoint`, `CamelSlipEndpoint`, `CamelFailureEndpoint`, `CamelInterceptedEndpoint` and endpoint errors use `scheme://path?sorted-params` (`mock://result`, `seda://foo?blockWhenFull=true&size=10`, values encoded like `URLEncoder`). Lookups and `interceptSendToEndpoint` patterns work with either form. `RAW(...)` values are shown encoded rather than as `RAW(...)`. |
| Value predicate truthiness | ✅ | `ObjectHelper.evaluateValuePredicate` (`Value::is_predicate_true`): a boolean is its value, a string is false only for `"false"` (trimmed, any case), an array is true when not empty, null is false and anything else (`0`, `""`, maps, bytes) is true. Used by Simple, `header`/`constant`/... languages used as predicates and JSONPath. |

## Enterprise Integration Patterns

| EIP | Status | Notes |
|---|---|---|
| Pipeline, `to`, `toD` (Simple uri), `step` | ✅ | |
| Message Filter (+ `CamelFilterMatched`) | ✅ | |
| Content-Based Router (`choice/when/otherwise`, `endChoice`, nested) | ✅ | A choice with no `otherwise` continues after the choice. |
| Splitter (tokenize, arrays, delimiter, streaming, parallel, timeout, stopOnException, shareUnitOfWork, aggregation strategy) | ✅ | Without a strategy the original message continues. `stop()` in a split part stops only that part. Sequential `streaming` is lazy: a string body is split one part at a time and sub-exchanges are created on demand; `CamelSplitSize` is set on the last part only, as in Camel. The tokenize language itself still returns all tokens at once. |
| Aggregator (completionSize/Timeout/Interval/Predicate/FromBatchConsumer/OnNewCorrelationGroup, eager check, forceCompletionOnStop, discardOnCompletionTimeout, closeCorrelationKeyOnCompletion, custom repository) | ✅ | `completionTimeout` is an inactivity timeout. `CamelAggregatedCompletedBy` takes the values `size`, `timeout`, `predicate`, `interval`, `force`... Repositories are in memory only (no JDBC/LevelDB). |
| Multicast (parallel, strategy, stopOnException, timeout) | ✅ | Without a strategy the last reply wins, as in Camel. |
| Recipient List (delimiter, ignoreInvalidEndpoints, parallel, strategy) | ✅ | |
| Routing Slip, Dynamic Router | ✅ | |
| Wire Tap (copy, `newExchangeBody/Header`, `onPrepare`) | ✅ | Asynchronous, on a separate exchange. |
| Content Enricher `enrich` / `pollEnrich` (strategy, timeout, aggregateOnException, dynamic uri) | ✅ | |
| Loop (count, `doWhile`, `copy`) | ✅ | |
| Idempotent Consumer (eager, skipDuplicate, removeOnFailure, completionEager) | ✅ | Memory LRU repository only. Duplicates skip only the idempotent block, as in Camel. |
| Load Balancer (round robin, random, sticky, weighted, topic, failover, custom) | ✅ | |
| Resequencer (batch, stream) | ✅ | |
| Sort, Claim Check, Sampling, Delay, Throttle (incl. correlated) | ✅ | `asyncDelayed` (default true for delay, false for throttle, as in Camel; `sync_delayed()`) releases the consumer thread of asynchronous consumers (seda, timer) while the exchange waits, see `camel_core::async_delay`. Synchronous callers (direct, `ProducerTemplate`) still wait. |
| Validate | ✅ | Throws `PredicateValidationException`. |
| Circuit Breaker (`onFallback`) | ✅ | A resilience4j-style state machine built in. No Resilience4j/MicroProfile integrations. |
| Stop, Rollback, markRollbackOnly(Last), throwException | ✅ | `throwException` messages are Simple expressions. |
| Threads, Policy, Transacted | ⚠️ | `transacted` applies a `Policy`. There is no transaction manager. |
| Route Policy | ✅ | |
| Intercept, interceptFrom, interceptSendToEndpoint (skip, afterUri, onWhen) | ✅ | |
| Process / bean / method | ✅ | Processors are closures or registry beans. There is no reflection-based bean binding. |
| Kamelets, Pipes / KameletBindings | ⚠️ | `camel-yaml::kamelet`: YAML Kamelets (`spec.definition.properties`, `required`, defaults, `spec.template`/`spec.flow`), `kamelet:name?prop=x` as source (`from`) and sink/action (`to`), `{{prop}}`/`{{?prop}}` from uri parameters, `camel.kamelet.<name>[.<routeId>].<prop>` properties and defaults, `add_kamelet`, `load_kamelets_from_dir`, built-in `timer-source`, `log-sink`, `log-action`, `insert-header-action`, `set-body-action`. Pipes/KameletBindings with Kamelet refs or uris become routes. Template local beans (`spec.template.beans`, created per Kamelet route with the registered bean factories, `{{beanName}}` resolves to the route's bean). No data types or Pipe error handlers. |
| Saga, Script (groovy/js), Transformer/Validator (data types), Service Call | ❌ | |
| Route templates | ✅ | Rust DSL (`add_route_template`, `TemplatedRouteBuilder`) and YAML/XML (`routeTemplate`, `templatedRoute`, `{{param:default}}`, `prefixId`, template beans via registered bean factories). Kamelets: see above. |
| Message history, tracer, route supervising | ✅ | `set_message_history` (also printed in the exhausted-failure log), `set_tracing` + backlog tracer, supervising route controller with back-off. |

## Error handling

| Feature | Status | Notes |
|---|---|---|
| DefaultErrorHandler, DeadLetterChannel, NoErrorHandler | ✅ | Redelivery headers `CamelRedelivered`, `CamelRedeliveryCounter` and `CamelRedeliveryMaxCounter`. Redelivery happens at the point of failure. |
| Redelivery policy (delay, back-off, collision avoidance, delayPattern, max delay, retryWhile, onRedelivery, onExceptionOccurred, onPrepareFailure, log levels, asyncDelayedRedelivery) | ✅ | With `asyncDelayedRedelivery` (Rust DSL and YAML `redeliveryPolicy`) the consumer thread of asynchronous consumers (seda, timer) is released during the delay: a replacement consumer thread takes over and the waiting exchange finishes on the released thread (rust-camel processors are synchronous, so this is a thread hand-off rather than a scheduler continuation). Otherwise, and for synchronous callers, the delay blocks the calling thread. |
| `onException` (hierarchy matching, cause chain, onWhen, handled, continued, useOriginalMessage/Body, route, builder and route configuration scope) | ✅ | Exception "classes" are string kinds with a registered parent hierarchy (`register_kind_parent`). Route configurations for Rust DSL routes: `ctx.add_route_configuration(Some("id") / None, \|c\| ...)` with `on_exception`, `on_completion`, `intercept*` and `error_handler`, selected with `route_configuration_id("a,b*")`; a configuration without id applies to every route that names none (context-wide `onException`). |
| Error handler scope across direct sub-routes | ✅ | The failing route's error handler applies. A handled exchange stops the caller too (`CamelErrorHandlerHandled`). An exhausted exchange is not redelivered again by the caller. |
| deadLetterHandleNewException | ✅ | |
| doTry / doCatch (onWhen) / doFinally | ✅ | No error-handler redelivery inside doTry. |
| onCompletion (onCompleteOnly, onFailureOnly, onWhen, mode before/after consumer, route/global scope) | ✅ | |
| ProducerTemplate failures | ✅ | Returned as `CamelExecutionException` with the original exception as the cause. |

## Languages

| Language | Status | Notes |
|---|---|---|
| Simple | ✅ | Operators (`==`, `=~`, `!=`, `>`..., `contains`, `~~`, `regex`, `in`, `range`, `is`, `startsWith`, `endsWith`, `&&`, `||`), ternary, elvis, OGNL navigation and method calls, `bodyAs`/`headerAs`, `date`/`date-with-timezone`, `properties`, `env`/`sys`, `file:*` functions, string/math/collection functions, `jsonpath`/`jq`/`xpath` inline, `bean`, `ref`. `${bodyType}` returns the Java class name Camel would report (`java.lang.String`, `java.lang.Integer`/`Long`, `java.lang.Double`, `java.lang.Boolean`, `byte[]`, `java.util.ArrayList`, `java.util.LinkedHashMap`). `${type:org.apache.camel.Exchange.FILE_NAME}` gives the `Exchange` constants (and `java.lang.Integer/Long.MIN_VALUE/MAX_VALUE`, `Boolean.TRUE/FALSE`). `${messageHistory}` / `${messageHistory(false)}`. `date-with-timezone` accepts region ids (`Europe/Paris`); `${date:...}` uses the default time zone (`TZ`). |
| Constant, Header, ExchangeProperty, Variable, Ref, Bean/Method | ✅ | |
| Tokenize (token, regex, pair, xml) | ✅ | |
| JSONPath | ✅ | A built-in implementation (Jayway-style syntax). |
| XPath | ⚠️ | A built-in XPath 1.0 subset over a built-in XML parser. No XSLT and no XQuery. |
| jq | ⚠️ | A built-in implementation of a jq subset. |
| File language | ✅ | Available in file component options and as Simple `${file:...}` functions. |
| Groovy, JavaScript, Python, OGNL, MVEL, SpEL, DataSonnet, CSimple, JOOR | ❌ | |

## Data formats

| Data format | Status | Notes |
|---|---|---|
| JSON (`json`, aliases `jackson`, `gson`...) | ✅ | Pretty print. No POJO binding: values map to `Value`. |
| CSV | ✅ | RFC 4180. |
| Base64, Hex, String (charset) | ✅ | |
| GZip, Zip/Deflate, ZipFile | ✅ | Built-in deflate implementation. |
| XML (`jacksonXml`, aliases `xml`, `jaxb`) | ⚠️ | A generic value ⇄ XML mapping (no schema binding). |
| URL-encoded form | ✅ | |
| Custom data formats (registry) | ✅ | |
| Avro, Protobuf, Bindy, YAML (as data format), Tar, Crypto, PGP, Fory... | ❌ | |

## Components

| Component | Status | Key options / notes |
|---|---|---|
| ftp | ✅ | `camel-protocols`. Polling consumer (noop/delete/move/preMove/moveFailed, include/exclude/ant, recursive, sortBy, idempotent) and producer (fileExist strategies, tempFileName, doneFileName); passive/active mode, MLSD/LIST parsing. No `readLock`. `ftps`/`sftp` ❌ (no TLS/SSH). |
| smtp / pop3 / imap (mail) | ✅ | `camel-protocols`. SMTP AUTH PLAIN/LOGIN, headers override options, attachments via `CamelAttachments`; POP3/IMAP polling with MIME decoding, delete/copyTo/moveTo. No TLS variants or STARTTLS. |
| websocket / vertx-websocket / ws | ✅ | `camel-protocols`. RFC 6455 server and client, fragmentation, ping/pong, `sendToAll`, connection-key routing. No compression extensions; `wss` ❌. |
| mllp + hl7 data format | ✅ | `camel-protocols`. MLLP framing, auto-ACK (AA/AE), producer ACK validation and exceptions, `CamelMllp*` headers; HL7 v2 parsing with terser paths (`hl7terser` language). No typed HAPI model. |
| syslog | ✅ | `camel-protocols`. RFC 5424/3164 data format, UDP/TCP component. |
| direct | ✅ | Synchronous. Fails with `DirectConsumerNotAvailableException` when no consumer is running. |
| seda / vm / stub | ✅ | `size`, `concurrentConsumers`, `waitForTaskToComplete`, `timeout`, `multipleConsumers`, `blockWhenFull`, `discardWhenFull`, `offerTimeout`, `purgeWhenStopping`, `failIfNoConsumers`, `discardIfNoConsumers`. Supports polling consumers. |
| timer | ✅ | `period`, `delay`, `repeatCount`, `fixedRate`, `includeMetadata`, `time` (local time of the default time zone; a trailing `Z`, offset or `[Europe/Paris]` selects another zone). Sets the `CamelTimerCounter` property. |
| scheduler | ✅ | Scheduled poll consumer options, including `scheduler=quartz&scheduler.cron=...` (also for the file consumer). |
| cron, quartz | ✅ | Built-in cron engine (`component/cron_expr.rs`): Quartz 6/7-field and Unix 5-field syntax, `L`, `W`, `#`, names, macros (`@daily`...), time zones (`timeZone=GMT+02:00` or region ids such as `Europe/Paris` read from the system zoneinfo database, TZif v1-v3 with POSIX footer rules; default: the process time zone). DST gaps and overlaps behave like Quartz: a fire time in a spring-forward gap fires once, shifted forward; one in a fall-back overlap fires once (standard time). `quartz:` cron and simple triggers (`trigger.repeatInterval`/`repeatCount`, `fireNow`, `triggerStartDelay`). Quartz headers (`fireTime`, `scheduledFireTime`, ...) carry epoch millis. No job store, clustering or calendars; missed fires are skipped. |
| mustache | ✅ | Variables, sections, inverted sections, comments, partials, set-delimiter, standalone lines; `allowTemplateFromHeader`, `contentCache`, `startDelimiter`/`endDelimiter`. No lambdas. |
| string-template, velocity, freemarker | ⚠️ | Substitution only (`<name>`, `$name`/`${name}`, `${name!default}`); directives and template expressions fail with "not supported". |
| json-validator | ✅ | Built-in JSON Schema validator (draft-04 to 07 subset incl. `$ref` to local definitions, combinators, formats); `JsonValidationException` lists all errors. No remote `$ref`. |
| validator (XML) | ⚠️ | Well-formedness only, plus the root element must be a global element of the XSD. No XML Schema content validation. |
| log | ✅ | `level`, `showAll`/`showXxx`, `multiline`, `maxChars`, `groupSize`/`groupInterval`, masking. `BodyType:` shows the Java class name Camel would show (`java.lang.String`...). Log timestamps use the default time zone (`TZ`). |
| mock | ✅ | Expectations (count, minimum count, bodies in order or any order, headers, properties, no duplicates, custom), `assertPeriod`, `resultWaitTime`, `whenAnyExchangeReceived`, `returnReplyBody`, reset. |
| file | ✅ | Consumer: `noop` (idempotent), `delete`, `move`/`moveFailed`/`preMove` with file-language expressions, include/exclude/ant filters, `recursive`, depth, sorting, read locks (`changed` follows Camel: marker file, two equal checks or `readLockMinAge`, `readLockMinLength`, `readLockTimeout`), idempotent repositories, done files. Producer: `fileName` expressions, `fileExist` (Override/Append/Fail/Ignore/Move/TryRename), temp files, done files, `CamelFileNameProduced`. |
| controlbus | ✅ | Route actions (start/stop/suspend/resume/restart/status/stats/fail) and `language:` scripts. |
| bean / class, ref, language, browse, exec, stream, dataset/dataset-test/test, dataformat | ✅ | `exec` runs local processes. `stream` handles stdin/stdout/stderr/files. |
| platform-http (aliases jetty, undertow, netty-http) | ✅ | HTTP/1.1 server: path parameters, `matchOnUriPrefix`, `httpMethodRestrict` (405), chunked encoding, keep-alive, `muteException`. `CamelHttpResponseCode` sets the status. A failure is a 500 carrying the exception text. A handled exception is a 200 with the handled body. |
| http | ✅ | HTTP/1.1 client: `httpMethod`, `throwExceptionOnFailure`, `okStatusCodeRange`, timeouts, redirects, Basic auth, `bridgeEndpoint`, `CamelHttpMethod/Path/Query/Uri` headers, `HttpOperationFailedException` with status code and response body. |
| https / TLS | ❌ | Out of scope for a std-only build (no TLS stack). `https:` uris fail with a clear error. |
| rest (REST DSL) | ✅ | Verbs, path/query parameters, `bindingMode` off/auto/json, CORS, OpenAPI 3 document. No XML binding, no `clientRequestValidation`. |
| netty (tcp/udp) | ✅ | Text line and raw codecs, sync request/reply. |
| Kafka, JMS/AMQP, SQL/JDBC, AWS/Azure/GCP, FTP/SFTP, Mail, MQTT, gRPC, WebSocket... | ❌ | They need external protocol stacks or drivers. |

## DSLs

| DSL | Status | Notes |
|---|---|---|
| Rust fluent DSL (equivalent of the Java DSL) | ✅ | `end()` with no open block is tolerated, as in the Java DSL. Route configurations: `ctx.add_route_configuration(...)` + `route_configuration_id(...)`. |
| YAML DSL | ✅ | All EIPs of the Rust DSL incl. marshal/unmarshal, `routeConfiguration` (onException, onCompletion, intercepts, errorHandler), top-level `errorHandler`, beans via registered factories, line/column error messages. Throttle needs a constant maximum; Kamelet, Pipe and KameletBinding resources (and `kamelet:` uris) are supported; options the core does not model (e.g. `executorService`) are ignored. |
| XML DSL | ✅ | `camel-yaml/src/xml.rs` converts XML to the YAML model and reuses the YAML step handlers, so coverage follows the YAML DSL. |
| REST DSL (Rust) | ✅ | See the rest component. |
| REST DSL in YAML | ✅ | `rest`/`restConfiguration` in YAML and XML (registered by `camel-main`), OpenAPI document. `rest` elements loaded before the context starts become routes at startup, so a `restConfiguration` from any file (earlier or later) applies. |

## Management and tooling

| Feature | Status | Notes |
|---|---|---|
| Route statistics, in-flight repository | ✅ | `RouteStats`, `inflight_count`, controlbus `stats`. |
| JMX-style management, health checks, dev console | ✅ | `managed_context()` (route/processor statistics, operations, JSON + Prometheus export), health check registry (context/routes/consumers, readiness/liveness), dev console `/q/health`, `/q/metrics`, `/q/dev/*` with route start/stop/suspend/resume. No JMX (not applicable). |
| `camel-main` runner (`application.properties`, route discovery, `duration-max-*`) | ✅ | |
| CLI `camel run / get / catalog / version / help` | ⚠️ | Subset of Camel JBang: `run` (YAML/XML files, properties, `--console`, `--trace`, `--max-*`), `get routes/context/health`. No `init`, `export`, `bind`. |
| Spring Boot / Quarkus integration, Camel K | ❌ | Not applicable to Rust. |

## Known deviations found by the conformance suite

These differences are kept on purpose (documented, not fixed):

1. In Simple text, `$${` is an escape that produces a literal `${`.
2. No TLS: `https` and secure netty are not available.
3. Maps (`Value::Object`) are sorted by key: JSON objects do not keep the insertion order of their keys.
4. Asynchronous delays (`asyncDelayed`, `asyncDelayedRedelivery`) hand the waiting exchange to its (released) thread and start a replacement consumer thread, instead of scheduling a continuation on a scheduler pool.
