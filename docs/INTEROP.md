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

Run on 2026-11-01. All 45 live tests pass (`CAMEL_LIVE_INTEROP=1 cargo test --workspace live_`).

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
| OpenJDK 21 JSSE (`java` single-file launcher, keytool stores) | `https` producer, `jetty:https` consumer | the https producer against `com.sun.net.httpserver.HttpsServer` restricted to TLS 1.3 and to TLS 1.2 (PKCS#12 key store from openssl, negotiated protocol and cipher echoed back), 300 KB bodies, untrusted CA refused; Java's `HttpClient` (offering HTTP/2 via ALPN, and HTTP/1.1) against a rust-camel `jetty:https` route with a keytool trust store, GET and UTF-8 POST (ALPN falls back to HTTP/1.1, which the camel server speaks) | pass |

Not installable / not verified: nothing from the requested list was missing from the archive.
ActiveMQ / Artemis (the native STOMP/AMQP 1.0 brokers rust-camel's JMS components are modelled
on) are not packaged for Ubuntu 24.04, so JMS selectors, dynamic temporary AMQP 1.0 nodes and
AMQP 1.0 transactions are only covered by the embedded brokers.

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

The embedded STOMP broker gained a RabbitMQ flavour (`StompBrokerBuilder::rabbitmq()`) that
reproduces the conventions behind bugs 3-7, and the embedded AMQP 1.0 broker can run without
`ANONYMOUS-RELAY`.

## Server-side notes

* Mosquitto 2.0 drops messages queued for offline persistent sessions when
  `per_listener_settings true` is used, so the script runs the anonymous and the authenticated
  listeners as two broker processes.
* RabbitMQ's STOMP adapter needs the `host` header to be an existing virtual host (`host=/`
  on rust-camel endpoints or `camel.component.jms.host=/`); rust-camel sends the broker host
  name by default, as Spring's STOMP relay does.
* With RabbitMQ (no selectors) several producers must not share one fixed AMQP 1.0 `replyTo`
  queue: they would compete for each other's replies.
