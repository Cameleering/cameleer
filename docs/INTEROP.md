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
