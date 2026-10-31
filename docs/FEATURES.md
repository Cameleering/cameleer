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
| Endpoint uri normalization | ⚠️ | Uris keep the form they were written in (`mock:a`). Camel normalizes to `mock://a`, which shows in `CamelToEndpoint` and `CamelRecipientListEndpoint`. |
| Value predicate truthiness | ⚠️ | A bare value used as a predicate is false for null, `false`, `"false"`, `0`, `""` and empty collections. Camel treats any non-null value other than `false`/`"false"` as true (so `0` and `""` are true in Camel). |

## Enterprise Integration Patterns

| EIP | Status | Notes |
|---|---|---|
| Pipeline, `to`, `toD` (Simple uri), `step` | ✅ | |
| Message Filter (+ `CamelFilterMatched`) | ✅ | |
| Content-Based Router (`choice/when/otherwise`, `endChoice`, nested) | ✅ | A choice with no `otherwise` continues after the choice. |
| Splitter (tokenize, arrays, delimiter, streaming, parallel, timeout, stopOnException, shareUnitOfWork, aggregation strategy) | ✅ | Without a strategy the original message continues. `stop()` in a split part stops only that part. |
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
| Sort, Claim Check, Sampling, Delay, Throttle (incl. correlated) | ✅ | |
| Validate | ✅ | Throws `PredicateValidationException`. |
| Circuit Breaker (`onFallback`) | ✅ | A resilience4j-style state machine built in. No Resilience4j/MicroProfile integrations. |
| Stop, Rollback, markRollbackOnly(Last), throwException | ✅ | `throwException` messages are Simple expressions. |
| Threads, Policy, Transacted | ⚠️ | `transacted` applies a `Policy`. There is no transaction manager. |
| Route Policy | ✅ | |
| Intercept, interceptFrom, interceptSendToEndpoint (skip, afterUri, onWhen) | ✅ | |
| Process / bean / method | ✅ | Processors are closures or registry beans. There is no reflection-based bean binding. |
| Kamelets, Pipes / KameletBindings | ⚠️ | `camel-yaml::kamelet`: YAML Kamelets (`spec.definition.properties`, `required`, defaults, `spec.template`/`spec.flow`), `kamelet:name?prop=x` as source (`from`) and sink/action (`to`), `{{prop}}`/`{{?prop}}` from uri parameters, `camel.kamelet.<name>[.<routeId>].<prop>` properties and defaults, `add_kamelet`, `load_kamelets_from_dir`, built-in `timer-source`, `log-sink`, `log-action`, `insert-header-action`, `set-body-action`. Pipes/KameletBindings with Kamelet refs or uris become routes. No Kamelet beans, data types or Pipe error handlers. |
| Saga, Script (groovy/js), Transformer/Validator (data types), Service Call | ❌ | |
| Route templates | ✅ | Rust DSL (`add_route_template`, `TemplatedRouteBuilder`) and YAML/XML (`routeTemplate`, `templatedRoute`, `{{param:default}}`, `prefixId`, template beans via registered bean factories). Kamelets: see above. |
| Message history, tracer, route supervising | ✅ | `set_message_history` (also printed in the exhausted-failure log), `set_tracing` + backlog tracer, supervising route controller with back-off. |

## Error handling

| Feature | Status | Notes |
|---|---|---|
| DefaultErrorHandler, DeadLetterChannel, NoErrorHandler | ✅ | Redelivery headers `CamelRedelivered`, `CamelRedeliveryCounter` and `CamelRedeliveryMaxCounter`. Redelivery happens at the point of failure. |
| Redelivery policy (delay, back-off, collision avoidance, delayPattern, max delay, retryWhile, onRedelivery, onExceptionOccurred, onPrepareFailure, log levels) | ✅ | Delays block the calling thread (no asynchronous redelivery scheduler). |
| `onException` (hierarchy matching, cause chain, onWhen, handled, continued, useOriginalMessage/Body, route vs builder scope) | ✅ | Exception "classes" are string kinds with a registered parent hierarchy (`register_kind_parent`). |
| Error handler scope across direct sub-routes | ✅ | The failing route's error handler applies. A handled exchange stops the caller too (`CamelErrorHandlerHandled`). An exhausted exchange is not redelivered again by the caller. |
| deadLetterHandleNewException | ✅ | |
| doTry / doCatch (onWhen) / doFinally | ✅ | No error-handler redelivery inside doTry. |
| onCompletion (onCompleteOnly, onFailureOnly, onWhen, mode before/after consumer, route/global scope) | ✅ | |
| ProducerTemplate failures | ✅ | Returned as `CamelExecutionException` with the original exception as the cause. |

## Languages

| Language | Status | Notes |
|---|---|---|
| Simple | ✅ | Operators (`==`, `=~`, `!=`, `>`..., `contains`, `~~`, `regex`, `in`, `range`, `is`, `startsWith`, `endsWith`, `&&`, `||`), ternary, elvis, OGNL navigation and method calls, `bodyAs`/`headerAs`, `date`/`date-with-timezone`, `properties`, `env`/`sys`, `file:*` functions, string/math/collection functions, `jsonpath`/`jq`/`xpath` inline, `bean`, `ref`. `${bodyType}` returns the dynamic value type (`string`) rather than a Java class name. `${type:...}` is not supported. |
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
| kafka | ✅ | `camel-kafka-mqtt`. Built-in Kafka wire protocol client (ApiVersions, Metadata v5, Produce v3 with v2 record batches/CRC32C/headers, Fetch v4, ListOffsets v1, FindCoordinator, JoinGroup/SyncGroup/Heartbeat/LeaveGroup, OffsetCommit/OffsetFetch, CreateTopics). Producer: `kafka.KEY`/`key`, `kafka.PARTITION_KEY`/`partitionKey`, murmur2 partitioner, `kafka.OVERRIDE_TOPIC`, `requestRequiredAcks`, record headers, `kafka.RECORD_META`. Consumer: `groupId` with range assignor and rebalancing, `consumersCount`, committed offsets (`autoCommitEnable`, `autoCommitIntervalMs`, `allowManualCommit` via `kafka::manual_commit`), `autoOffsetReset`, `seekTo`, `topicIsPattern`, `breakOnFirstError`, `maxPollRecords`; `kafka.TOPIC/PARTITION/OFFSET/KEY/TIMESTAMP` headers. Embedded test broker `testing::KafkaBroker`. No TLS/SASL, transactions, idempotent producer, compression on send (gzip batches are decoded), or Avro/schema registry. |
| paho / paho-mqtt5 (MQTT) | ✅ | `camel-kafka-mqtt`. MQTT 3.1.1 and 5.0 client: username/password, keep-alive, QoS 0/1/2 flows, `+`/`#` wildcards, retained, clean/persistent sessions, will (`willTopic`...), automatic reconnect with re-subscription. Headers `CamelMqttTopic`, `CamelMqttQoS`, `CamelMqttRetained`, `CamelPahoOverrideTopic` (+ `CamelPahoQos`/`CamelPahoRetained` on the producer). Embedded test broker `testing::MqttBroker`. No `ssl://`/`ws://` transports, no MQTT 5 topic aliases, enhanced auth or shared subscriptions. |
| direct | ✅ | Synchronous. Fails with `DirectConsumerNotAvailableException` when no consumer is running. |
| seda / vm / stub | ✅ | `size`, `concurrentConsumers`, `waitForTaskToComplete`, `timeout`, `multipleConsumers`, `blockWhenFull`, `discardWhenFull`, `offerTimeout`, `purgeWhenStopping`, `failIfNoConsumers`, `discardIfNoConsumers`. Supports polling consumers. |
| timer | ✅ | `period`, `delay`, `repeatCount`, `fixedRate`, `includeMetadata`. Sets the `CamelTimerCounter` property. |
| scheduler | ✅ | Scheduled poll consumer options, including `scheduler=quartz&scheduler.cron=...` (also for the file consumer). |
| cron, quartz | ✅ | Built-in cron engine (`component/cron_expr.rs`): Quartz 6/7-field and Unix 5-field syntax, `L`, `W`, `#`, names, macros (`@daily`...), UTC or fixed offsets (`timeZone=GMT+02:00`). `quartz:` cron and simple triggers (`trigger.repeatInterval`/`repeatCount`, `fireNow`, `triggerStartDelay`). Quartz headers (`fireTime`, `scheduledFireTime`, ...) carry epoch millis. No job store, clustering, calendars or regional time zones; missed fires are skipped. |
| mustache | ✅ | Variables, sections, inverted sections, comments, partials, set-delimiter, standalone lines; `allowTemplateFromHeader`, `contentCache`, `startDelimiter`/`endDelimiter`. No lambdas. |
| string-template, velocity, freemarker | ⚠️ | Substitution only (`<name>`, `$name`/`${name}`, `${name!default}`); directives and template expressions fail with "not supported". |
| json-validator | ✅ | Built-in JSON Schema validator (draft-04 to 07 subset incl. `$ref` to local definitions, combinators, formats); `JsonValidationException` lists all errors. No remote `$ref`. |
| validator (XML) | ⚠️ | Well-formedness only, plus the root element must be a global element of the XSD. No XML Schema content validation. |
| log | ✅ | `level`, `showAll`/`showXxx`, `multiline`, `maxChars`, `groupSize`/`groupInterval`, masking. Body type names are rust-camel value types. |
| mock | ✅ | Expectations (count, minimum count, bodies in order or any order, headers, properties, no duplicates, custom), `assertPeriod`, `resultWaitTime`, `whenAnyExchangeReceived`, `returnReplyBody`, reset. |
| file | ✅ | Consumer: `noop` (idempotent), `delete`, `move`/`moveFailed`/`preMove` with file-language expressions, include/exclude/ant filters, `recursive`, depth, sorting, read locks, idempotent repositories, done files. Producer: `fileName` expressions, `fileExist` (Override/Append/Fail/Ignore/Move/TryRename), temp files, done files, `CamelFileNameProduced`. |
| controlbus | ✅ | Route actions (start/stop/suspend/resume/restart/status/stats/fail) and `language:` scripts. |
| bean / class, ref, language, browse, exec, stream, dataset/dataset-test/test, dataformat | ✅ | `exec` runs local processes. `stream` handles stdin/stdout/stderr/files. |
| platform-http (aliases jetty, undertow, netty-http) | ✅ | HTTP/1.1 server: path parameters, `matchOnUriPrefix`, `httpMethodRestrict` (405), chunked encoding, keep-alive, `muteException`. `CamelHttpResponseCode` sets the status. A failure is a 500 carrying the exception text. A handled exception is a 200 with the handled body. |
| http | ✅ | HTTP/1.1 client: `httpMethod`, `throwExceptionOnFailure`, `okStatusCodeRange`, timeouts, redirects, Basic auth, `bridgeEndpoint`, `CamelHttpMethod/Path/Query/Uri` headers, `HttpOperationFailedException` with status code and response body. |
| https / TLS | ❌ | Out of scope for a std-only build (no TLS stack). `https:` uris fail with a clear error. |
| rest (REST DSL) | ✅ | Verbs, path/query parameters, `bindingMode` off/auto/json, CORS, OpenAPI 3 document. No XML binding, no `clientRequestValidation`. |
| netty (tcp/udp) | ✅ | Text line and raw codecs, sync request/reply. |
| sql | ✅ | `camel-sql`. Producer: `:#name` (body map, headers, properties), `:#${simple}`, `:#in:name`, `?`/`#` positional (body array or `separator`-split string), `outputType` SelectList/SelectOne/StreamList, `outputHeader`, `noop`, `batch`, `useMessageBodyForSql`, `parametersCount`, `placeholder`, `CamelSqlQuery`, generated keys (`CamelSqlRetrieveGeneratedKeys` → `CamelSqlGeneratedKeysRows`), `file:`/`classpath:` queries. Consumer: one exchange per row or a list (`useIterator`), `onConsume`/`onConsumeFailed`/`onConsumeBatchComplete`, `maxMessagesPerPoll`, `routeEmptyResultSet`, `transacted`, scheduled poll options. StreamList is materialized as a list; `outputClass` is ignored (rows are maps). |
| jdbc | ✅ | `camel-sql`. SQL from the body, `outputType`, `useHeadersAsParameters` (`:?name`), `readSize`/`statement.maxRows`, `CamelJdbcRowCount`/`UpdateCount`/`ColumnNames`, `CamelGeneratedKeysRows`. `resetAutoCommit=false` is accepted but statements outside a JDBC transaction policy still auto-commit. |
| DataSources: embedded `rustsql` engine, PostgreSQL client | ✅ | `camel-sql`. `jdbc:rustsql:mem:<name>`: in-memory SQL engine (DDL, multi-row INSERT/RETURNING, joins, GROUP BY/HAVING, subqueries, UNION, transactions with undo-log rollback, read-uncommitted isolation). `jdbc:postgresql://host:port/db`: wire protocol v3 client (cleartext/MD5/SCRAM-SHA-256, simple + extended query, text format, pooled); no TLS, COPY or binary format. `camel.datasource[.<name>].url` properties in camel-main. Transactions: `JdbcTransactionPolicy` via `.policy(...)` (the core `transacted()` marker stays a no-op). |
| JDBC idempotent / aggregation repositories | ✅ | `camel-sql`. `JdbcIdempotentRepository` (`CAMEL_MESSAGEPROCESSED`), `JdbcAggregationRepository` (JSON exchange snapshots, `<name>_completed` table, manual `scan`/`recover`, no background recovery). |
| sql-stored | ❌ | Fails with "not supported" (no stored procedures). |
| Kafka, JMS/AMQP, SQL/JDBC, AWS/Azure/GCP, FTP/SFTP, Mail, MQTT, gRPC, WebSocket... | ❌ | They need external protocol stacks or drivers. |

## DSLs

| DSL | Status | Notes |
|---|---|---|
| Rust fluent DSL (equivalent of the Java DSL) | ✅ | `end()` with no open block is tolerated, as in the Java DSL. |
| YAML DSL | ✅ | All EIPs of the Rust DSL incl. marshal/unmarshal, `routeConfiguration` (onException, onCompletion, intercepts, errorHandler), top-level `errorHandler`, beans via registered factories, line/column error messages. Throttle needs a constant maximum; Kamelet, Pipe and KameletBinding resources (and `kamelet:` uris) are supported; options the core does not model (e.g. `executorService`) are ignored. |
| XML DSL | ✅ | `camel-yaml/src/xml.rs` converts XML to the YAML model and reuses the YAML step handlers, so coverage follows the YAML DSL. |
| REST DSL (Rust) | ✅ | See the rest component. |
| REST DSL in YAML | ✅ | `rest`/`restConfiguration` in YAML and XML (registered by `camel-main`), OpenAPI document. A `restConfiguration` loaded in a later file than its `rest` is not applied. |

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

1. Endpoint uris are not normalized (`mock:a`, not `mock://a`).
2. Value truthiness: `0` and `""` are false as predicates (Camel treats them as true).
3. `${bodyType}` and the log component's `BodyType:` show rust-camel value type names.
4. In Simple text, `$${` is an escape that produces a literal `${`.
5. Redelivery delays block the processing thread (Camel can schedule redelivery asynchronously).
6. No TLS: `https` and secure netty are not available.
