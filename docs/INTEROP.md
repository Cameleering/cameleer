# Interop verification against real servers

Most rust-camel components are tested against embedded test servers written in this workspace
(`crates/*/src/testing`). Those can only prove that client and server agree with each other.
The **live interop suite** runs rust-camel routes against real, independent server software
installed from the Ubuntu 24.04 archive, so wire-level misunderstandings show up.

## Running it

```sh
sudo scripts/live-interop.sh all          # apt-get install, configure, start (idempotent)
CAMEL_LIVE_INTEROP=1 cargo test --workspace live_
scripts/live-interop.sh status            # which ports answer
sudo scripts/live-interop.sh stop
```

The suites live in `crates/<crate>/tests/live_*.rs` and do nothing unless
`CAMEL_LIVE_INTEROP=1` is set, so the normal `cargo test` stays hermetic and fast. Shared
helpers (`camel_tls::testing::live`) give the state directory (`CAMEL_INTEROP_DIR`, default
`/var/lib/camel-interop`), the test password and TLS parameters trusting the interop CA.

`scripts/live-interop.sh` keeps everything under the state directory: a test PKI made with
`openssl` (an RSA CA, a `localhost`/`127.0.0.1` server certificate, a client certificate, plus
PKCS#12 stores for Java), a private PostgreSQL cluster, and configuration files for every
server, all listening on 127.0.0.1 on non-default ports (the port table is at the top of the
script). Every account uses the password `camelpw` (system users `camelftp` and `camelmail`).

## Results

Run on 2026-11-01. All 70 live tests pass (`CAMEL_LIVE_INTEROP=1 cargo test --workspace live_`), including
17 against Apache ActiveMQ 5.17; the 6 ActiveMQ AMQP 1.0 tests skip because the broker has no
AMQP module (see below).

| Server (version) | Components | What was verified | Result |
|---|---|---|---|
| PostgreSQL 16.15 | `sql`, `jdbc`, `sql-stored`, `JdbcIdempotentRepository`, `JdbcAggregationRepository`, transaction policy, PG wire client | the shared producer suite (named / positional / IN-list parameters, generated keys, SelectOne / StreamList / outputHeader / noop, batch atomicity, `useMessageBodyForSql`, SQLSTATE → exception kinds, `jdbc` with `readSize` and headers as parameters), `sql-stored` with real PL/pgSQL procedures and functions (OUT / INOUT, `RETURNS TABLE`, `RAISE EXCEPTION`, file templates, batch rollback), polling consumer with `onConsume` + JDBC idempotent repository, aggregation repository with recovery, local transactions (isolation, rollback, aborted-transaction state, server-side rollback of dropped connections), route `policy` / `rollback`, pooled connections reusing the backend, SCRAM-SHA-256 (wrong password → 28P01), types (smallint, char padding, numeric, real, bytea, jsonb, uuid, date, timestamp, UTF-8), 5000-row results, NOTICEs, `sslmode=require` / `verify-ca` / `verify-full` (host name and IP SAN; wrong CA refused) | pass |
| Redis 7.0.15 | `spring-redis` producer and consumer, `RedisIdempotentRepository`, `RedisAggregationRepository`, `RedisClient` | strings / hashes / lists / sets / sorted sets / keys / expiry commands, JSON and binary values, `WRONGTYPE`, `requirepass` (NOAUTH, WRONGPASS), `AUTH default` over RESP3 (`HELLO 3`) with map replies, database selection, 200-command pipelines, MULTI/EXEC, 1 MiB values, SUBSCRIBE / PSUBSCRIBE consumers fed by the server, both repositories, TLS port verified against the CA (untrusted CA refused) incl. a TLS subscriber | pass |
| Eclipse Mosquitto 2.0.18 | `paho`, `paho-mqtt5`, `MqttClient` | QoS 0/1/2 round trips with both protocol versions, broker-side QoS downgrade, 256 KiB payloads, `+` / `#` wildcards, `CamelPahoOverrideTopic`, retained messages to late subscribers and clearing them, username/password listener (bad credentials and anonymous refused), TLS listener verified against the CA, will messages, persistent sessions queueing QoS 1 messages while offline (3.1.1 clean-session=false and 5.0 session expiry), unsubscribe | pass |
| RabbitMQ 3.12.1 (Erlang 25.3) — AMQP 0-9-1 | `spring-rabbitmq`, `rabbitmq`, AMQP client | direct / topic / fanout exchanges, AMQP headers and properties (correlation id, priority, message id, expiration, delivery mode, typed header values), JSON bodies, manual ack with reject + requeue (redelivered flag), retries then dead-lettering through a real DLX with `x-death` / `x-first-death-*`, `prefetchCount` (2 unacked, 3 ready), InOut request/reply over exclusive reply queues with competing consumers, remote failures, reply timeouts, publisher confirms, mandatory returns (312 NO_ROUTE), authentication failure, AMQPS (TLS 1.3 and forced TLS 1.2) verified against the CA | pass |
| RabbitMQ 3.12.1 — STOMP plugin | `stomp`, `jms`, `activemq` | queues with competing / concurrent consumers, topics fan-out, JMS headers and properties (`_DOT_` / `_HYPHEN_` keys), NUL-containing byte bodies, InOut over RabbitMQ temporary queues, fixed `replyTo`, remote failures and timeouts, `transacted=true` redelivery, `CLIENT_ACKNOWLEDGE` NACK redelivery, AUTO acknowledging failures, durable topic subscriptions keeping messages while the route is stopped, bad credentials, STOMP over TLS. RabbitMQ needs `host=/` (the STOMP `host` header names the virtual host) and has no JMS selectors (not tested) | pass |
| RabbitMQ 3.12.1 — AMQP 1.0 plugin | `amqp` (JMS over AMQP 1.0) | RabbitMQ addresses (`amqp:queue:/queue/x`, `amqp:topic:/topic/key`), JMS header mapping, text / bytes / map bodies, competing consumers, topic fan-out, request/reply with fixed `replyTo` queues, remote failures, timeouts, `CLIENT_ACKNOWLEDGE` redelivery with `JMSRedelivered`, bad credentials, AMQPS. Broker limitations (asserted as clear errors): no dynamic reply nodes (`Dynamic sources not supported`), no transactions (`amqp:not-implemented`), no selectors | pass |
| RabbitMQ 3.12.1 — MQTT plugin | `paho` | QoS 0/1 publish and wildcard subscription with credentials | pass |
| vsftpd 3.0.5 | `ftp`, `ftps`, FTP client | passive and active mode, mkdirs / STOR / APPE / RETR (200 KB binary) / SIZE / MDTM / LIST / NLST / RNFR-RNTO / DELE / RMD, ASCII mode, wrong password; consumer with `move`, `antInclude`, `sortBy`, recursive `delete` (active mode), `noop` idempotency; producer `fileExist` Fail / Ignore / Append / Override with `tempPrefix` / Move with `moveExisting`, `fileName` expressions, `disconnect`; explicit FTPS (AUTH TLS, PROT P), implicit FTPS, and a `require_ssl_reuse=YES` server (data connections resume the control session; without resumption the transfer is refused with 522) for the client and the `ftps` component; untrusted CA refused | pass |
| Postfix 3.8.6 + Dovecot 2.3.21 | `smtp`, `smtps`, `imap`, `imaps`, `pop3`, `pop3s` | SMTP into Postfix local delivery (Maildir) with custom headers, Cc, UTF-8 subject, dot-stuffing; POP3 consume with delete; SMTP AUTH through Dovecot SASL over STARTTLS (required) with binary and UTF-8 attachments, wrong password → `AuthenticationFailedException`; IMAP consume with `moveTo=Archive`; SMTPS (implicit TLS) → IMAPS with `\Seen` marking and failed exchanges left unseen (checked in Dovecot's Maildir flags); POP3 STLS and POP3S | pass |
| nginx 1.24.0 | `http`, `https` producer | GET / POST / PUT / PATCH / DELETE, forwarded query, custom headers, 1 MiB binary POST, 404 with and without `throwExceptionOnFailure`, 302 with and without `followRedirects`, gzip content coding decoded (compression proven from nginx's access log), keep-alive pooling (nginx `$connection` stays the same; `connectionClose=true` opens new ones), HTTPS verified against the CA by host name and IP SAN, TLS keep-alive, untrusted CA refused; HTTP/2 over TLS via ALPN (`$server_protocol` = HTTP/2.0) with POST, 300 KB request bodies across flow-control windows, gzip, multiplexing on one connection, 404; h2c with prior knowledge | pass |
| Apache ActiveMQ 5.17.6 — STOMP | `stomp`, `jms`, `activemq` | see [Apache ActiveMQ 5.17](#apache-activemq-517): JMS selectors on queues, topics and durable subscriptions, `/temp-queue/` request/reply (also shared `replyTo` queues read with broker-side `JMSCorrelationID` selectors), durable subscriptions, transactions with client-side redelivery and dead-lettering, priority and expiry, persistent / non-persistent, map / object messages, virtual topics, simple authentication, STOMP+SSL | pass |
| Apache ActiveMQ 5.17.6 — MQTT | `paho` | QoS 0/1/2, broker-side QoS downgrade, 256 KiB payloads, wildcards, retained messages, persistent sessions, simple authentication, MQTT+SSL, MQTT ↔ STOMP topic bridging; MQTT 5 refused (ActiveMQ 5 speaks 3.1.1) | pass |
| Apache ActiveMQ 5.17.6 — AMQP 1.0 | `amqp` | not verified: Ubuntu's package has no `activemq-amqp` module (suite ready, skipped) | n/a |
| OpenJDK 21 JSSE (`java` single-file launcher, keytool stores) | `https` producer, `jetty:https` consumer | the https producer against `com.sun.net.httpserver.HttpsServer` restricted to TLS 1.3 and to TLS 1.2 (PKCS#12 key store from openssl, negotiated protocol and cipher echoed back), 300 KB bodies, untrusted CA refused; Java's `HttpClient` (offering HTTP/2 via ALPN, and HTTP/1.1) against a rust-camel `jetty:https` route with a keytool trust store, GET and UTF-8 POST (ALPN falls back to HTTP/1.1, which the camel server speaks) | pass |

Not installable / not verified: nothing from the requested list was missing from the archive.
Apache ActiveMQ 5.17.6 *is* packaged (universe) and is covered over STOMP and MQTT (see
[Apache ActiveMQ 5.17](#apache-activemq-517) below), which now verifies JMS selectors,
`/temp-queue/` request/reply, ActiveMQ durable subscriptions and ActiveMQ's transaction and
redelivery semantics against the real broker. Ubuntu's package leaves out the `activemq-amqp`
module, so AMQP 1.0 selectors, dynamic temporary AMQP 1.0 nodes and AMQP 1.0 transactions are
still only covered by the embedded AMQP 1.0 broker (a ready suite runs as soon as the module is
added). Artemis is not packaged.

## Bugs found and fixed

Every fix has a regression test in the hermetic suite as well.

| # | Component | Bug (found against) | Fix | Regression test |
|---|---|---|---|---|
| 1 | `sql-stored` | INTEGER template parameters were bound as `int8`, and PostgreSQL resolves routines by argument type: `procedure subnumbers(bigint, bigint, unknown) does not exist` (PostgreSQL 16) | narrow declared types (INTEGER, SMALLINT, REAL, NUMERIC) are bound as `CAST(? AS type)`, like pgJDBC's `setInt` (`crates/camel-sql/src/sql_stored.rs` `call_sql`) | `sql_stored::tests::templates`, `tests/postgres.rs` |
| 2 | `spring-rabbitmq`, `rabbitmq` | a component-wide host/port overrode an endpoint's `addresses=amqps://...`, so the endpoint connected to the plain port (RabbitMQ) | `addresses` takes precedence over host/port (Spring Boot / Camel semantics) (`crates/camel-amqp/src/amqp/component.rs`) | `tests/rabbitmq.rs` `endpoint_addresses_take_precedence_over_component_host_and_port` |
| 3 | `stomp`, `jms` | RabbitMQ's `ERROR` for a bad login (`message:Bad CONNECT`, reason in the body) became `StompException` instead of `AuthenticationFailureException` | the frame body is checked too (`crates/camel-amqp/src/stomp/client.rs`) | `tests/jms.rs` `rabbitmq_flavoured_broker_conventions` |
| 4 | `jms` (STOMP) | InOut SUBSCRIBEd to `/temp-queue/...`, which RabbitMQ refuses (`not a valid destination for 'SUBSCRIBE'`) | on RabbitMQ (`server:RabbitMQ/...`) the reply queue is only named in `reply-to` and replies are routed by their `subscription:/temp-queue/...` header (`StompConnection::subscribe_reply_queue`) | same |
| 5 | `jms` (STOMP) | NON_PERSISTENT sends carried `persistent:false`, which RabbitMQ reads as the queue's `durable` flag and refuses for the existing durable queue (`RESOURCE_LOCKED` / precondition failure) | the header is only sent for persistent messages (absence = non-persistent for ActiveMQ, Artemis and RabbitMQ) (`crates/camel-amqp/src/stomp/component.rs`) | same |
| 6 | `jms` (STOMP) | durable topic subscriptions were not durable on RabbitMQ (ActiveMQ / Artemis headers only) | also `durable:true`, `auto-delete:false` and a stable subscription id (`clientId:name`) | same |
| 7 | `jms` (STOMP) | `transacted=true`: after a failed exchange the aborted transactional ACK left the message unacknowledged on RabbitMQ — never redelivered | on RabbitMQ the message is NACKed (requeued) after the ABORT; ActiveMQ redelivers by itself | same |
| 8 | `amqp` | `amqp:queue:/queue/x` lost its leading slash (`queue:/` was trimmed like `queue://`), so RabbitMQ answered `unknown_destination` | only the `//` of the URL form is stripped (`crates/camel-amqp10/src/jms.rs` `parse_destination`) | `jms::tests::destinations_and_keys` |
| 9 | `amqp` | consumers always attached an anonymous-relay reply link; RabbitMQ 3.x does not offer `ANONYMOUS-RELAY` and refused the attach (`address_not_utf8_string`), so no consumer could start | anonymous relay only when the broker offers it, else one reply link per reply address (`ReplySenders`) | `tests/amqp_component.rs` `consumers_work_without_anonymous_relay` (embedded broker option `anonymous_relay(false)`) |
| 10 | `amqp` | redeliveries had `JMSRedelivered=false`: RabbitMQ marks them with `first-acquirer=false` and keeps `delivery-count` at 0 | an explicit `first-acquirer=false` also means redelivered (`Header::acquired_before`, `crates/camel-amqp10/src/message.rs`) | `message::tests::explicit_first_acquirer_false_marks_redelivery` |
| 11 | `stomp`, `jms`, `activemq` | ActiveMQ's refused login (`User name [camel] or password is invalid.`, repeated in the body) became a `StompException` with the text twice (ActiveMQ 5.17) | recognised as `AuthenticationFailureException`; a body repeating `message` is not appended (`crates/camel-amqp/src/stomp/client.rs` `error_frame_to_error`) | `tests/jms.rs` `activemq_flavoured_broker_conventions` |
| 12 | `jms` (STOMP) | `transacted=true`: after a failure the consumer sent `ABORT` and expected the broker to redeliver; ActiveMQ only restores the pending ack, so the message was never redelivered and blocked the subscription's prefetch window (ActiveMQ 5.17) | on ActiveMQ 5 (`server:ActiveMQ/...`) the consumer redelivers locally like ActiveMQ's JMS client: `RedeliveryPolicy` (`jms.redeliveryPolicy.maximumRedeliveries` (6) / `initialRedeliveryDelay` / `redeliveryDelay` broker-url options), `JMSRedelivered=true`, `JMSXDeliveryCount`, then a `NACK` (poison ack → DLQ) (`crates/camel-amqp/src/stomp/component.rs` `Worker::handle`) | same, `redelivery_policy_from_broker_url` |
| 13 | `jms` (STOMP) | `CLIENT_ACKNOWLEDGE`: a failed message was `NACK`ed for redelivery; ActiveMQ treats `NACK` as a poison ack and dead-letters it at once (non-persistent messages are dropped) (ActiveMQ 5.17) | same client-side redelivery; `NACK` only once the policy is exhausted | same |
| 14 | `jms` (STOMP) | sends made by a route consuming with `transacted=true` were committed on their own, so every failed attempt left a duplicate (found against ActiveMQ 5.17, affected every broker) | InOnly sends to the same broker (address and login) from inside a transacted consumer join its STOMP transaction (exchange property `CamelJmsStompTransaction`), as with Spring's thread-bound JMS session: committed or rolled back with the consumed message | same, and `transacted_rollback_redelivers_with_jms_redelivered` |
| 15 | `jms` (STOMP) | `subscriptionShared=true` silently duplicated messages on ActiveMQ 5, whose STOMP adapter ignores the shared subscription headers (every member got every message) (ActiveMQ 5.17) | refused at route start with `UnsupportedOperationException` naming ActiveMQ's virtual topics, as ActiveMQ's (JMS 1.1) client refuses shared consumers | same; live test with virtual topics |

The embedded STOMP broker gained a RabbitMQ flavour (`StompBrokerBuilder::rabbitmq()`) that
reproduces the conventions behind bugs 3-7, and the embedded AMQP 1.0 broker can run without
`ANONYMOUS-RELAY`. An ActiveMQ flavour (`StompBrokerBuilder::activemq()`) reproduces what bugs
11-15 depend on: `server:ActiveMQ/5.17.6`, ActiveMQ's login error, `ABORT` restoring the pending
ack without redelivery and `NACK` as a poison ack.

## Server-side notes

* Mosquitto 2.0 drops messages queued for offline persistent sessions when
  `per_listener_settings true` is used, so the script runs the anonymous and the authenticated
  listeners as two broker processes.
* RabbitMQ's STOMP adapter needs the `host` header to be an existing virtual host (`host=/`
  on rust-camel endpoints or `camel.component.jms.host=/`); rust-camel sends the broker host
  name by default, as Spring's STOMP relay does.
* With RabbitMQ (no selectors) several producers must not share one fixed AMQP 1.0 `replyTo`
  queue: they would compete for each other's replies.

## Apache ActiveMQ 5.17

Apache ActiveMQ 5.17.6 from Ubuntu 24.04 (`activemq`, universe; OpenJDK 21), run on 2026-11-01.
`scripts/live-interop.sh` runs a dedicated instance under `$CAMEL_INTEROP_DIR/activemq` (the
package's `activemq.jar` with its own `activemq.xml`; the Debian init script and instances are
not used):

| Port | Transport connector |
|---|---|
| 16615 / 16616 | STOMP / STOMP+SSL |
| 11886 / 18886 | MQTT / MQTT+SSL |
| 16617 | OpenWire (not used by rust-camel) |
| 15675 / 15676 | AMQP / AMQPS — only when the `activemq-amqp` module is present (see below) |

* `simpleAuthenticationPlugin` with user `camel` / `camelpw`, anonymous access refused;
* TLS with the interop CA: the openssl PKCS#12 server store converted to a JKS key store by
  `keytool -importkeystore`, plus a JKS trust store with the CA (`<sslContext>`);
* per-destination dead letter queues (`individualDeadLetterStrategy`, `DLQ.<queue>`) so tests
  can inspect them, `prioritizedMessages` on queues, KahaDB persistence;
* Ubuntu's package leaves libraries out of the broker's class path, which the script links
  into the instance's `lib/`: the MQTT codec (`libmqtt-client-java`, without it every MQTT
  connection fails with `NoClassDefFoundError: org/fusesource/mqtt/codec/PINGRESP`), and
  XStream / Jettison (`libxstream-java`, `libjettison-java`, needed for STOMP
  `transformation: jms-map-json` / `jms-object-json`, without them the broker drops the
  connection);
* the package does not include the `activemq-amqp` module at all (Maven Central is not
  reachable from the test environment, and building it from the upstream sources was not
  allowed there). Putting `activemq-amqp-5.17.x.jar` into `$CAMEL_INTEROP_DIR/activemq/lib`
  and re-running `setup` + `restart` adds the AMQP connectors (proton-j comes from
  `libapache-qpid-proton-j-java`), and `crates/camel-amqp10/tests/live_activemq_amqp.rs`
  (selectors via `apache.org:selector-filter:string`, AMQP 1.0 transactions, dynamic
  temporary reply queues, durable subscriptions, SASL PLAIN, AMQPS) then runs instead of
  skipping. **These AMQP 1.0 tests have not been run against ActiveMQ.**

Suites: `crates/camel-amqp/tests/live_activemq_stomp.rs` (12 tests),
`crates/camel-kafka-mqtt/tests/live_activemq_mqtt.rs` (5),
`crates/camel-amqp10/tests/live_activemq_amqp.rs` (6, skipped without the AMQP module).

What the STOMP suite verifies with `stomp`, `jms` and `activemq` endpoints:

* the `stomp` component; bad and missing credentials → `AuthenticationFailureException`;
* competing consumers, topic fan-out, `JMSDestination` `queue://` / `topic://`, ActiveMQ
  message ids, `JMSCorrelationID`, `JMSType`, priority, `timeToLive` (`JMSExpiration` -
  `JMSTimestamp`), persistent (default, `JMSDeliveryMode=2`) vs `deliveryPersistent=false`
  (1), dotted / hyphenated property keys, NUL-containing bytes and UTF-8 bodies, MapMessage
  and ObjectMessage through ActiveMQ's JSON transformations;
* **JMS selectors evaluated by ActiveMQ**: on queues (unmatched messages stay on the queue),
  on topics (`JMSPriority >= 7 OR severity IN (...)`) and on a durable subscription while it is
  offline;
* request/reply over ActiveMQ temporary queues (4 concurrent requesters), a fixed shared
  `replyTo` queue read with per-request `JMSCorrelationID` selectors by concurrent producers,
  `replyToType=Exclusive`, `useMessageIDAsCorrelationID`, remote exceptions, timeouts, and no
  stray replies left behind;
* `transacted=true`: redelivery with `JMSRedelivered=true`, a send made inside the transaction
  committed only with the successful attempt, an always-failing message dead-lettered to
  `DLQ.<queue>` with its transactional send rolled back; `CLIENT_ACKNOWLEDGE` redelivery;
  `AUTO_ACKNOWLEDGE` acknowledging failures; `jms.redeliveryPolicy.*` options (maximum
  redeliveries, delays, `JMSXDeliveryCount`);
* the raw broker semantics the emulation relies on: an aborted transactional `ACK` only
  restores the pending ack, `NACK` is a poison ack (DLQ for persistent messages), a disconnect
  redelivers with `redelivered:true` (ActiveMQ sends the header only when true);
* durable subscriptions (`durableSubscriptionName` + `clientId`, and `subscriptionDurable` +
  `subscriptionName` with a selector) keeping messages while the route is stopped;
* priority ordering of waiting messages and expiry of messages whose TTL passed;
* virtual topics (`VirtualTopic.x` → `Consumer.<sub>.VirtualTopic.x` with concurrent
  consumers), and `subscriptionShared=true` refused (ActiveMQ 5 has no shared subscriptions);
* STOMP+SSL by host name and IP SAN, untrusted CA refused.

ActiveMQ notes:

* ActiveMQ 5 does not redeliver after an aborted transaction or a NACK; rust-camel emulates its
  JMS client's redelivery policy on the consumer (bugs 12 and 13). Its defaults are ActiveMQ's
  (6 redeliveries, 1 s delays); set `jms.redeliveryPolicy.*` options on the broker url
  (`camel.component.jms.brokerUrl=tcp://host:61613?jms.redeliveryPolicy.redeliveryDelay=100`).
  Exponential back-off options are not emulated.
* ActiveMQ dead-letters only persistent messages (`processNonPersistent=false`); a poisoned
  non-persistent message is discarded.
* ActiveMQ 5's STOMP adapter has no JMS 2.0 shared subscriptions; use virtual topics.
* ActiveMQ bridges protocols: the MQTT topic `a/b/c` is the STOMP / JMS topic `a.b.c` (verified
  both ways).

## Official clients

The live suite checks rust-camel *clients* against real servers. Many areas, though, are only
served by rust-camel's own emulators and embedded servers (Kafka broker, MongoDB / Cassandra /
Elasticsearch / Redis servers, the AWS / Azure / GCP mocks, the Azure flavour of the AMQP 1.0
broker), or are protocol stacks written from scratch (TLS, HTTP/2, gRPC). The **official-client
suite** drives those with independently written, officially published clients, so emulator and
client cannot simply agree with each other.

```sh
sudo scripts/official-clients.sh install      # apt-get the clients (Ubuntu 24.04 python3-*, kcat)
scripts/official-clients.sh versions
scripts/official-clients.sh test              # CAMEL_OFFICIAL_CLIENTS=1 cargo test --workspace --test 'official_*'
```

The suites are `crates/<crate>/tests/official_*.rs`; they do nothing unless
`CAMEL_OFFICIAL_CLIENTS=1` is set. The client programs (Python scripts, a bash driver for the
Google Cloud CLIs, a Go program) are in `scripts/official-clients/`; helpers are in
`camel_tls::testing::official`. Each test starts the rust-camel side on an ephemeral port, runs
the client as a subprocess and checks the client's `key: value` output. The Google Cloud SDK and
a Go toolchain come from outside apt (see the script header). Every bug fixed along the way also
has a hermetic regression test: `official_regressions.rs` / `official_*_regressions.rs`,
`kafka_wire_compat.rs`, and unit tests.

### Results

Run on 2026-11-01. All 34 official-client tests pass
(`CAMEL_OFFICIAL_CLIENTS=1 cargo test --workspace --test 'official_*'`), together with the 24 hermetic regression tests in the same targets.

| Area | Client (version) | What was exercised | Result |
|---|---|---|---|
| Kafka broker (`testing::KafkaBroker`), `kafka` component | confluent-kafka 1.7.0 on librdkafka 2.3.0, kcat 1.7.1 | produce/consume with headers and keys; murmur2 partitioning matching the Java client; gzip, snappy and lz4 batches from librdkafka decoded by rust-camel, and rust-camel's compressed batches decoded by librdkafka; zstd falling back cleanly; a consumer group mixing librdkafka and rust-camel members (join/sync/heartbeat, rebalance, committed offsets); the admin client (create/describe/delete topics); kcat round trip; SASL PLAIN and SCRAM-SHA-256/512, TLS and mutual TLS | pass |
| MongoDB server | pymongo 3.11.0 | CRUD, bulk writes, aggregation pipelines, indexes (create/list/drop, unique violations → `DuplicateKeyError`), change streams, cursors and `getMore`; SCRAM-SHA-256, TLS and X.509 authentication; documents shared with the `mongodb` component | pass |
| Cassandra server | DataStax cassandra-driver 3.29.0 (protocol v4) | cluster metadata (`system.local`, `system.peers`, `system_schema` keyspaces/tables/columns), prepared and batch statements, paging, typed values, PlainText auth, TLS, data shared with the `cql` component | pass |
| Elasticsearch server | elasticsearch-py 7.17.6 | product check, index/get/update/delete, bulk, search with queries and aggregations (terms, avg/sum/min/max, value_count, cardinality, stats), paging, `_delete_by_query`, error mapping | pass |
| Redis server | redis-py 4.3.4 | strings/hashes/lists/sets/zsets, TTLs, BLPOP, `WRONGTYPE`, SCAN/HSCAN/SSCAN/ZSCAN, pipelines, MULTI/EXEC with WATCH (aborted transactions), pub/sub, username/password AUTH (failures refused), values shared with the `spring-redis` component | pass |
| AWS mock (`AwsMock`), AWS components | boto3 / botocore 1.34.46 | SigV4 (botocore's own signatures compared byte for byte, unknown keys and bad signatures rejected with the services' error codes); S3 incl. multipart (ListParts, ListMultipartUploads), presigned URLs, HTTPS; SQS in botocore's Query protocol and the JSON protocol (message attributes with MD5, FIFO deduplication, batches, error codes); SNS incl. `MessageStructure=json` to SQS; DynamoDB (tables, items, queries, batch write, transactions); Kinesis; Lambda; KMS (encrypt/decrypt with encryption context, aliases, data keys, random); Secrets Manager; STS; data exchanged with rust-camel's AWS components | pass |
| Azure mocks (`AzuriteMock`, `AzureMock`), Azure components | azure-core 1.28.0, azure-storage-blob 12.17.0, -queue 12.7.0b1, -file-datalake 12.12.0, -file-share 12.13.0, azure-identity 1.14.0b2, azure-keyvault-secrets 4.8.0b2, -keys 4.9.0b2, azure-cosmos 3.1.1 | Shared Key and account / service / container SAS; blobs (ranges, content type, case-preserving metadata, delimiter walk, block lists, chunked uploads, append blobs, leases, snapshots, Blob Batch); queues (peek, receive, update, metadata, SAS); Data Lake (paths, renames); file shares; client-credential tokens from the mock's Entra endpoint, Key Vault secrets (versions, properties, soft delete) and keys over HTTPS; Cosmos; data exchanged with the storage components | pass |
| AMQP 1.0 broker in Azure mode, Service Bus / Event Hubs components | azure-servicebus 7.11.1, azure-eventhub 5.11.3 (pyamqp) | CBS SAS authentication; Service Bus peek-lock and receive-and-delete, abandon with delivery counts, peek, scheduled messages, dead-lettering, batches, binary bodies, application properties, sessions with session state; Event Hubs properties and partition management requests, batched sends, partition receive; messages exchanged with rust-camel's components | pass |
| GCP mock (`GcpMock`) | Google Cloud SDK 570.0.0 (gcloud, gsutil), bq 2.1.32 | service-account JWT login against the mock's token endpoint over HTTPS; `gcloud storage` (buckets, multipart and resumable uploads, cat, ls/-r, describe, custom metadata, cp/mv/rm, missing objects); gsutil (JSON API over HTTPS, md5/crc32c verification); `gcloud pubsub` (topics, subscriptions, publish with attributes, pull/ack, describe, delete); bq (datasets, tables, insert, show, standard-SQL query jobs, head, rm) | pass |
| GCP mock Pub/Sub gRPC | grpcio 1.51.1, protobuf 4.21.12 | StreamingPull with a bearer token from the service-account JWT grant (unauthenticated streams refused), attributes, acks over the stream | pass |
| gRPC (`grpc` consumer and producer) | grpcio 1.51.1 | grpcio calling a rust-camel consumer route: unary with metadata, server-, client- and bidirectional streaming, status codes (NOT_FOUND with details), deadlines, 300 KB messages; plaintext h2c, gzip, TLS and mutual TLS (untrusted / missing client certificates refused). The rust-camel producer calling a grpcio server: all four call kinds, headers as metadata, PERMISSION_DENIED, 200 KB messages, plaintext, gzip and TLS (untrusted server certificate refused) | pass |
| TLS (`camel-tls`) | Go 1.24.7 crypto/tls | both directions: TLS 1.3 with each of AES-128-GCM, AES-256-GCM, ChaCha20-Poly1305; TLS 1.2 with all six ECDHE-ECDSA / ECDHE-RSA AEAD suites; x25519, P-256 and P-384 in both versions, Go's default hybrid X25519MLKEM768 key share (camel picks x25519); RSA certificates (PSS in 1.3); ALPN; session resumption (TLS 1.3 PSK tickets, TLS 1.2 tickets) seen by both sides; mutual TLS incl. resumed sessions keeping the client identity, missing and untrusted client certificates refused; untrusted server and host name mismatch refused | pass |
| HTTP/2 and HTTPS (camel-grpc `h2`, `camel-http`) | Go 1.24.7 net/http (bundled x/net/http2) | Go's HTTP/2 client against camel's H2 server over TLS (ALPN h2, 1 MiB upload across flow-control windows, 16 concurrent streams on one connection, 404; an HTTP/1.1-only client gets 505), Go's client against a `jetty:https` route (ALPN falls back to HTTP/1.1; echo, 404); camel's HTTP/2 client (3 MB echo, streamed responses, 12 concurrent streams) and the `https` producer with `httpVersion=HTTP/2` (headers sent) against Go's HTTP/2 server, plus HTTP/1.1 | pass |

Not verified: CouchDB (no packaged official client); Kafka zstd (librdkafka uses it only with
Produce v7+, which the embedded broker does not advertise, so librdkafka falls back to
uncompressed batches, verified); the Google Cloud Python client libraries (not packaged; the
CLIs and grpcio stand in); the Azure Cosmos SDK's newer versions (only 3.1.1 is packaged).

### Bugs found and fixed

| # | Area | Bug (found with) | Fix | Regression test |
|---|---|---|---|---|
| 1 | Kafka broker | librdkafka never joined consumer groups or compressed: its ApiVersions feature probes need the v0 ranges of FindCoordinator/JoinGroup/SyncGroup/Heartbeat/LeaveGroup and Produce v0-v2 / Fetch v0-v2 (librdkafka 2.3.0) | v0 ranges advertised, legacy request/response shims and magic 0/1 message sets (`crates/camel-kafka-mqtt/src/testing/kafka_broker.rs`, `kafka/protocol.rs`) | `kafka_wire_compat.rs` `api_versions_cover_librdkafka_feature_probes`, `group_coordination_v0`, `legacy_message_sets_produce_and_fetch` |
| 2 | Kafka broker | produced batches were re-encoded, losing the producer's compression and CRC | batches are stored and served verbatim (`RawBatch`) | `producer_compression_and_verbatim_batches` |
| 3 | `kafka` component | snappy and lz4 batches could not be decoded or produced (no codecs) | LZ4 frame format with xxHash32 and xerial snappy-java framing (`crates/camel-core/src/dataformat/codecs.rs`), `compressionCodec` option (zstd refused) | `compressed_record_batches_round_trip`, codec unit tests |
| 4 | Kafka broker | SASL after SaslHandshake v0 sends bare tokens; the broker expected SaslAuthenticate frames | bare length-prefixed tokens supported | `sasl_handshake_v0_with_bare_tokens` |
| 5 | SCRAM server | librdkafka's client-final nonce is client nonce + server nonce; Kafka accepts a suffix match, the broker refused it | `ScramServer::accept_nonce_suffix` (`crates/camel-tls/src/sasl.rs`) | `sasl::tests::kafka_style_nonce_suffix` |
| 6 | MongoDB server | `createIndexes` / `listIndexes` / `dropIndexes` missing, unique indexes not enforced (pymongo) | index commands and unique enforcement (`crates/camel-datastores/src/mongodb/server.rs`) | `official_regressions.rs` `mongo_indexes_are_listed_dropped_and_unique` |
| 7 | Cassandra server | the driver failed on `Keyspace system_schema does not exist`, then on missing `schema_version` / `host_id` columns | `system_schema` tables kept in sync with the schema, full `system.local` / `system.peers(_v2)` (`cassandra/engine.rs`, `cassandra/server.rs`) | `cassandra_system_schema_and_peer_metadata` |
| 8 | Elasticsearch server | elasticsearch-py raised `UnsupportedProductError` (no `X-Elastic-Product` header); aggregations and `_delete_by_query` missing; scroll responses without `_shards` | header (not in OpenSearch mode), aggregations, delete-by-query (`elasticsearch/server.rs`) | `elasticsearch_product_header_aggregations_and_delete_by_query` |
| 9 | Redis server | SCAN family and WATCH missing (redis-py) | cursors over keys/hashes/sets/zsets, WATCH with key versions aborting EXEC (`redis/server.rs`) | `redis_scan_family_and_watch` |
| 10 | AWS mock | botocore speaks SQS's Query protocol: JSON error bodies broke it; unknown keys answered with the wrong code | protocol-aware errors (`x-amzn-query-error`, `InvalidClientTokenId` / `UnrecognizedClientException`) (`crates/camel-cloud/src/testing/aws.rs`) | `official_regressions.rs` `sqs_attribute_md5_fifo_dedup_and_error_codes`, `tests/aws.rs` |
| 11 | AWS mock | SQS `MD5OfMessageAttributes`, FIFO deduplication, `SequenceNumber` missing; S3 ListParts / ListMultipartUploads missing; SNS `MessageStructure=json` delivered the whole JSON; DynamoDB ListTables / BatchWrite / TransactWrite gaps; KMS ignored the encryption context and lacked aliases and data keys | implemented (`aws.rs`, `aws_gaps.rs`, `aws_more.rs`; KMS blobs are AES-GCM with the context as AAD) | `s3_list_parts_and_multipart_uploads`, `sns_message_structure_json_delivers_the_sqs_entry`, `dynamodb_list_tables_batch_write_and_transactions`, `kms_context_aliases_and_data_keys` |
| 12 | `aws-secrets-manager` | `updateSecret` called PutSecretValue; Camel (and boto3 users) expect UpdateSecret | `UpdateSecret` in component and mock (`crates/camel-cloud/src/aws/secretsmanager.rs`, `aws_ext.rs`) | `secrets_manager_update_secret` |
| 13 | HTTP server helper | `Response::with_header("Content-Type")` added a second Content-Type | replaces the existing one (`crates/camel-cloud/src/http.rs`) | covered by the above |
| 14 | Azure mocks | blob ranges (206/416), `x-ms-blob-content-type`, metadata case, delimiter listings, snapshots, Blob Batch, container and queue SAS, queue metadata, DFS endpoint serving the Blob API, file ranges, Key Vault properties / versions / soft delete / deleted keys, OpenID discovery, HTTPS Key Vault ids | implemented (`crates/camel-cloud/src/testing/azure*.rs`); the `azure-storage-blob` component now sends `x-ms-blob-content-type` (`azure/blob.rs`) | `official_azure_regressions.rs` (5 tests) |
| 15 | AMQP 1.0 codec | performatives were sent with trailing nulls trimmed; pyamqp indexes fields positionally (`IndexError`), and a null `handle_max` broke it | all fields kept, limits explicit (`crates/camel-amqp10/src/codec.rs`, `types.rs`, `message.rs`) | `amqp10 official_regressions.rs` `performatives_keep_all_fields_and_explicit_limits` |
| 16 | AMQP 1.0 broker (Azure mode) | SAS tokens sent with CBS type `jwt`, absolute-URL link addresses, rcv-settle-mode second from producers, batch message format `0x80013700`, plain-value session filters, `locked-until-utc` in .NET ticks, management requests without `reply-to` | all accepted / produced as Service Bus does (`testing/broker.rs`, `azure.rs`, `servicebus.rs`; `Sender::send_azure_batch`) | `cbs_jwt_typed_sas_tokens_absolute_addresses_and_settlement`, `plain_session_filters_and_tick_lock_expiry`, `management_replies_without_reply_to` |
| 17 | GCP mock | gsutil only speaks HTTPS; gcloud signs JWTs for Google's token audience; multipart uploads with single-quoted boundaries and LF line endings; objects lacked `crc32c` / `metadata` / `mediaLink` (gcloud crashed), no metadata PATCH/PUT, delimiter listings, ranged downloads, `moveTo` / `rewriteTo`, `storageLayout`; Pub/Sub list endpoints; BigQuery `datasets.get`, `tabledata.list`, query jobs, and error bodies without the `errors` list bq reads | `GcpMock::start_tls`, all of the above (`crates/camel-cloud/src/testing/gcp.rs`, `gcp_ext.rs`), checksum verification of uploads | `official_gcp_regressions.rs` (4 tests) |
| 18 | Google components | exceptions dropped BigQuery's `errors[].reason` (`invalidQuery`, `notFound`) once the status is canonical | the reason is appended to the message (`crates/camel-cloud/src/gcp/mod.rs` `google_error`) | `tests/gcp_more.rs` `bigquery_sql_queries_dml_parameters_and_paging` |
| 19 | `camel-tls` client | TLS 1.3 sessions never resumed against Go's crypto/tls: full-handshake ClientHellos lacked `psk_key_exchange_modes`, and Go (like BoringSSL) issues NewSessionTickets only to clients listing psk_dhe_ke | listed whenever resumption is enabled (`crates/camel-tls/src/tls/client.rs` `client_hello`) | `camel-tls official_regressions.rs` `first_client_hello_lists_psk_modes_when_resumption_is_enabled` |

Notes:

* TLS 1.2 with ECDSA certificates: RFC 8422 lets the client's supported groups constrain the
  certificate's curve too, so a Go client restricted to x25519 is refused by a P-256 certificate
  (Go and OpenSSL servers behave the same); the suite checks this explicitly.
* pyamqp (azure-servicebus 7.11) mis-encodes two-character session ids; the script uses longer
  ones.
