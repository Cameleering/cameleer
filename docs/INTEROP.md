# Interop verification against real servers

Most Cameleer components are tested against embedded test servers written in this workspace
(`crates/*/src/testing`). Those can only prove that client and server agree with each other.
The **live interop suite** runs Cameleer routes against real, independent server software
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
helpers (`cameleer_tls::testing::live`) give the state directory (`CAMEL_INTEROP_DIR`, default
`/var/lib/camel-interop`), the test password and TLS parameters trusting the interop CA.

`scripts/live-interop.sh` keeps everything under the state directory: a test PKI made with
`openssl` (an RSA CA, a `localhost`/`127.0.0.1` server certificate, a client certificate, plus
PKCS#12 stores for Java), a private PostgreSQL cluster, and configuration files for every
server, all listening on 127.0.0.1 on non-default ports (the port table is at the top of the
script). Every account uses the password `camelpw` (system users `camelftp` and `camelmail`).

## Results

The sections below record the runs of 2026-11-01. The latest full run, in one container session, passed 74 live, 58 official-client and 35 simulator tests with no failures.

Run on 2026-11-01. All 72 live tests pass (`CAMEL_LIVE_INTEROP=1 cargo test --workspace live_`), including
27 against Apache ActiveMQ 5.17 (12 STOMP, 5 MQTT and 10 AMQP 1.0, the latter with the
`activemq-amqp` module added, see below).

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
| Apache ActiveMQ 5.17.6 — AMQP 1.0 (`activemq-amqp` 5.17.6, proton-j 0.34) | `amqp` (JMS over AMQP 1.0) | see [AMQP 1.0](#amqp-10-transport): queues, topic fan-out to several non-durable subscribers (`queue://` / `topic://` address prefixes), JMS selectors, AMQP 1.0 transactions with redelivery, dynamic temporary reply queues, durable subscriptions, the JMS mapping through ActiveMQ's JMS transformer (text / bytes / map / object bodies, typed properties, `JMSReplyTo` topics, message annotations), 3 MB multi-frame messages, `CLIENT_ACKNOWLEDGE` / rollback redelivery counts, SASL PLAIN, AMQPS; JMS 2.0 shared subscriptions refused (ActiveMQ 5 has none) | pass |
| OpenJDK 21 JSSE (`java` single-file launcher, keytool stores) | `https` producer, `jetty:https` consumer | the https producer against `com.sun.net.httpserver.HttpsServer` restricted to TLS 1.3 and to TLS 1.2 (PKCS#12 key store from openssl, negotiated protocol and cipher echoed back), 300 KB bodies, untrusted CA refused; Java's `HttpClient` (offering HTTP/2 via ALPN, and HTTP/1.1) against a Cameleer `jetty:https` route with a keytool trust store, GET and UTF-8 POST (ALPN falls back to HTTP/1.1, which the camel server speaks) | pass |

Not installable / not verified: nothing from the requested list was missing from the archive.
Apache ActiveMQ 5.17.6 *is* packaged (universe) and is covered over STOMP and MQTT (see
[Apache ActiveMQ 5.17](#apache-activemq-517) below), which now verifies JMS selectors,
`/temp-queue/` request/reply, ActiveMQ durable subscriptions and ActiveMQ's transaction and
redelivery semantics against the real broker. Ubuntu's package leaves out the `activemq-amqp`
module; `scripts/live-interop.sh install` adds it from Maven Central (SHA-1 pinned, or copied
by hand when Maven Central is not reachable), so AMQP 1.0 selectors, dynamic temporary nodes
and transactions are verified against ActiveMQ too. Artemis is not packaged.

## Bugs found and fixed

Every fix has a regression test in the hermetic suite as well.

| # | Component | Bug (found against) | Fix | Regression test |
|---|---|---|---|---|
| 1 | `sql-stored` | INTEGER template parameters were bound as `int8`, and PostgreSQL resolves routines by argument type: `procedure subnumbers(bigint, bigint, unknown) does not exist` (PostgreSQL 16) | narrow declared types (INTEGER, SMALLINT, REAL, NUMERIC) are bound as `CAST(? AS type)`, like pgJDBC's `setInt` (`crates/cameleer-sql/src/sql_stored.rs` `call_sql`) | `sql_stored::tests::templates`, `tests/postgres.rs` |
| 2 | `spring-rabbitmq`, `rabbitmq` | a component-wide host/port overrode an endpoint's `addresses=amqps://...`, so the endpoint connected to the plain port (RabbitMQ) | `addresses` takes precedence over host/port (Spring Boot / Camel semantics) (`crates/cameleer-amqp/src/amqp/component.rs`) | `tests/rabbitmq.rs` `endpoint_addresses_take_precedence_over_component_host_and_port` |
| 3 | `stomp`, `jms` | RabbitMQ's `ERROR` for a bad login (`message:Bad CONNECT`, reason in the body) became `StompException` instead of `AuthenticationFailureException` | the frame body is checked too (`crates/cameleer-amqp/src/stomp/client.rs`) | `tests/jms.rs` `rabbitmq_flavoured_broker_conventions` |
| 4 | `jms` (STOMP) | InOut SUBSCRIBEd to `/temp-queue/...`, which RabbitMQ refuses (`not a valid destination for 'SUBSCRIBE'`) | on RabbitMQ (`server:RabbitMQ/...`) the reply queue is only named in `reply-to` and replies are routed by their `subscription:/temp-queue/...` header (`StompConnection::subscribe_reply_queue`) | same |
| 5 | `jms` (STOMP) | NON_PERSISTENT sends carried `persistent:false`, which RabbitMQ reads as the queue's `durable` flag and refuses for the existing durable queue (`RESOURCE_LOCKED` / precondition failure) | the header is only sent for persistent messages (absence = non-persistent for ActiveMQ, Artemis and RabbitMQ) (`crates/cameleer-amqp/src/stomp/component.rs`) | same |
| 6 | `jms` (STOMP) | durable topic subscriptions were not durable on RabbitMQ (ActiveMQ / Artemis headers only) | also `durable:true`, `auto-delete:false` and a stable subscription id (`clientId:name`) | same |
| 7 | `jms` (STOMP) | `transacted=true`: after a failed exchange the aborted transactional ACK left the message unacknowledged on RabbitMQ — never redelivered | on RabbitMQ the message is NACKed (requeued) after the ABORT; ActiveMQ redelivers by itself | same |
| 8 | `amqp` | `amqp:queue:/queue/x` lost its leading slash (`queue:/` was trimmed like `queue://`), so RabbitMQ answered `unknown_destination` | only the `//` of the URL form is stripped (`crates/cameleer-amqp10/src/jms.rs` `parse_destination`) | `jms::tests::destinations_and_keys` |
| 9 | `amqp` | consumers always attached an anonymous-relay reply link; RabbitMQ 3.x does not offer `ANONYMOUS-RELAY` and refused the attach (`address_not_utf8_string`), so no consumer could start | anonymous relay only when the broker offers it, else one reply link per reply address (`ReplySenders`) | `tests/amqp_component.rs` `consumers_work_without_anonymous_relay` (embedded broker option `anonymous_relay(false)`) |
| 10 | `amqp` | redeliveries had `JMSRedelivered=false`: RabbitMQ marks them with `first-acquirer=false` and keeps `delivery-count` at 0 | an explicit `first-acquirer=false` also means redelivered (`Header::acquired_before`, `crates/cameleer-amqp10/src/message.rs`) | `message::tests::explicit_first_acquirer_false_marks_redelivery` |
| 11 | `stomp`, `jms`, `activemq` | ActiveMQ's refused login (`User name [camel] or password is invalid.`, repeated in the body) became a `StompException` with the text twice (ActiveMQ 5.17) | recognised as `AuthenticationFailureException`; a body repeating `message` is not appended (`crates/cameleer-amqp/src/stomp/client.rs` `error_frame_to_error`) | `tests/jms.rs` `activemq_flavoured_broker_conventions` |
| 12 | `jms` (STOMP) | `transacted=true`: after a failure the consumer sent `ABORT` and expected the broker to redeliver; ActiveMQ only restores the pending ack, so the message was never redelivered and blocked the subscription's prefetch window (ActiveMQ 5.17) | on ActiveMQ 5 (`server:ActiveMQ/...`) the consumer redelivers locally like ActiveMQ's JMS client: `RedeliveryPolicy` (`jms.redeliveryPolicy.maximumRedeliveries` (6) / `initialRedeliveryDelay` / `redeliveryDelay` broker-url options), `JMSRedelivered=true`, `JMSXDeliveryCount`, then a `NACK` (poison ack → DLQ) (`crates/cameleer-amqp/src/stomp/component.rs` `Worker::handle`) | same, `redelivery_policy_from_broker_url` |
| 13 | `jms` (STOMP) | `CLIENT_ACKNOWLEDGE`: a failed message was `NACK`ed for redelivery; ActiveMQ treats `NACK` as a poison ack and dead-letters it at once (non-persistent messages are dropped) (ActiveMQ 5.17) | same client-side redelivery; `NACK` only once the policy is exhausted | same |
| 14 | `jms` (STOMP) | sends made by a route consuming with `transacted=true` were committed on their own, so every failed attempt left a duplicate (found against ActiveMQ 5.17, affected every broker) | InOnly sends to the same broker (address and login) from inside a transacted consumer join its STOMP transaction (exchange property `CamelJmsStompTransaction`), as with Spring's thread-bound JMS session: committed or rolled back with the consumed message | same, and `transacted_rollback_redelivers_with_jms_redelivered` |
| 15 | `jms` (STOMP) | `subscriptionShared=true` silently duplicated messages on ActiveMQ 5, whose STOMP adapter ignores the shared subscription headers (every member got every message) (ActiveMQ 5.17) | refused at route start with `UnsupportedOperationException` naming ActiveMQ's virtual topics, as ActiveMQ's (JMS 1.1) client refuses shared consumers | same; live test with virtual topics |
| 16 | `amqp` | two non-durable `amqp:topic:x` routes: a message published to the topic reached only one of them. ActiveMQ 5 ignores the `topic` terminus capability and takes a bare address for a **queue**, so the "subscribers" were competing consumers of queue `x` (and durable subscriptions were queues too) (ActiveMQ 5.17) | the broker's `queue-prefix` / `topic-prefix` open-frame properties (`queue://`, `topic://`) are applied to link and `to` / `reply-to` addresses, as Qpid JMS does, and stripped again for `JMSDestination` / `JMSReplyTo` (a topic becomes `topic:name`, which can be sent to again) (`crates/cameleer-amqp10/src/jms.rs` `AddressPrefixes`) | `tests/amqp_component.rs` `activemq_flavoured_broker_address_prefixes`, `jms::tests::broker_address_prefixes` |
| 17 | `amqp` | `subscriptionShared=true` on ActiveMQ 5: the `shared` capability is ignored, so every member would receive every message (ActiveMQ 5.17) | refused at route start with `UnsupportedOperationException` naming virtual topics when the broker announces `product=ActiveMQ`, `version=5.x` (as with bug 15) | same; live `live_activemq_amqp_shared_subscriptions_refused` |
| 18 | `cameleer-tls` servers (`NetStream::server`, gRPC/HTTP2, the cloud mock servers) | a rejected TLS 1.3 client certificate could reach the client as "connection reset" instead of the alert (`certificate_required`, `unknown_ca`): the client writes application data right after its Finished, the server dropped the socket right after sending the alert, the late data drew a TCP reset, and on Windows a reset discards the unread alert in the receive buffer (Linux returns it first). Seen as a rare flake (1 in 300 under CPU load) of `mutual_tls_rejects_missing_and_untrusted_certificates` on Windows 11 | after a fatal alert the server closes gracefully: FIN after the alert, then it reads and discards until the peer closes (at most 2 s or 256 KiB), like a lingering HTTP close (`crates/cameleer-tls/src/net.rs` `lingering_close`, `accept_tls`, used by `NetStream::server`) | `mtls_resumption.rs` `alert_survives_a_client_that_reads_late` (the client waits 300 ms before reading; it saw `ConnectionReset` before the fix) |
| 19 | HTTP/2 (`cameleer-grpc` `h2`: the gRPC server, `H2Server`, the cloud mock servers, the Kinesis enhanced fan-out client) | when a peer closed its connection, the reader thread stopped but the writer thread kept the socket and waited for frames forever: every closed connection leaked a thread and two descriptors. On a 4-vCPU Linux droplet (descriptor limit 1024) the Kinesis enhanced fan-out test reached 512 threads and 1018 descriptors in half a second; the mock then refused connections, which the client saw as "connection reset" (found on DigitalOcean, Ubuntu 24.04; it never showed on Windows or WSL2, where the test finished before the limit) | the reader tells the writer to close when it ends, so the socket is shut down on our side too | `cameleer-grpc` `tests/h2.rs` `the_server_closes_a_connection_the_peer_closed`; `cameleer-cloud` `aws_gaps::kinesis_enhanced_fan_out_over_http2` (18 descriptors and 16 threads during the test on the same droplet) |
| 20 | HTTP/2 client (`cameleer-grpc` `h2`: gRPC, `H2Connection`) | RFC 9113 section 8.1 lets a server answer before the request is complete and then send RST_STREAM(NO_ERROR); the client must keep the response. Ours reported `stream reset: NO_ERROR` and dropped the complete response, and a send after the reset failed. Our own server does this when a handler returns before the client ended its side, so `h2c_upgrade_and_route_controlled_stream_observer` failed now and then on Linux CI (GitHub Actions, 2026-11-09) | a complete response followed by RST_STREAM(NO_ERROR) is read to the end, and the rest of the request is dropped without an error; any other reset code is still an error | `cameleer-grpc` `tests/h2.rs` `a_complete_response_survives_rst_stream_no_error` |

The embedded STOMP broker gained a RabbitMQ flavour (`StompBrokerBuilder::rabbitmq()`) that
reproduces the conventions behind bugs 3-7, and the embedded AMQP 1.0 broker can run without
`ANONYMOUS-RELAY`. An ActiveMQ flavour (`StompBrokerBuilder::activemq()`) reproduces what bugs
11-15 depend on: `server:ActiveMQ/5.17.6`, ActiveMQ's login error, `ABORT` restoring the pending
ack without redelivery and `NACK` as a poison ack. The embedded AMQP 1.0 broker has an ActiveMQ
flavour too (`BrokerBuilder::activemq()`, bugs 16-17): ActiveMQ's open-frame properties, nodes
typed only by the `queue://` / `topic://` prefixes (capabilities ignored, a bare address is a
queue) and no shared subscriptions. `CAMEL_AMQP10_TRACE=1` makes the AMQP 1.0 client (and the
embedded broker) print every frame to stderr, which is how bug 16 was diagnosed.

## Server-side notes

* Mosquitto 2.0 drops messages queued for offline persistent sessions when
  `per_listener_settings true` is used, so the script runs the anonymous and the authenticated
  listeners as two broker processes.
* RabbitMQ's STOMP adapter needs the `host` header to be an existing virtual host (`host=/`
  on Cameleer endpoints or `camel.component.jms.host=/`); Cameleer sends the broker host
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
| 16617 | OpenWire (not used by Cameleer) |
| 15675 / 15676 | AMQP / AMQPS (`transport.transformer=jms`) — only when the `activemq-amqp` module is present (see below) |

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
* the package does not include the `activemq-amqp` module at all. `scripts/live-interop.sh
  install` downloads `activemq-amqp-5.17.6.jar` from Maven Central into
  `$CAMEL_INTEROP_DIR/activemq/lib` and keeps it only if its SHA-1 is the pinned
  `cdb7bb9009d36a2d00de970a515976a20cc8ad8e` (a jar already there with another checksum is
  moved aside); its dependency proton-j is linked from `libapache-qpid-proton-j-java` (0.34).
  When Maven Central is not reachable (as from the test environment's proxy) the step prints
  the manual instruction: copy the verified jar there. `setup` + `restart` then add the AMQP
  connectors (`amqp_has_amqp` looks in the package's `lib/`, `lib/optional/` and the instance's
  `lib/`).

Suites: `crates/cameleer-amqp/tests/live_activemq_stomp.rs` (12 tests),
`crates/cameleer-kafka-mqtt/tests/live_activemq_mqtt.rs` (5),
`crates/cameleer-amqp10/tests/live_activemq_amqp.rs` (10, skipped without the AMQP module).

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

### AMQP 1.0 transport

Run on 2026-11-01 against the AMQP connector with `activemq-amqp` 5.17.6 (the jar's SHA-1
verified against Maven Central) and proton-j 0.34.0 from Ubuntu: all 10 tests of
`live_activemq_amqp.rs` pass. They were first run here: 5 of the original 6 passed, the
topic fan-out test failed (bug 16; the durable subscription test only passed because the
"subscription" was a queue). What the suite verifies with `amqp` endpoints:

* queues with JMS headers (`priority`, `JMSCorrelationID`, properties), topic fan-out to two
  non-durable subscribers on separate connections, SASL PLAIN with a wrong password →
  `AuthenticationFailureException`;
* JMS selectors (`apache.org:selector-filter:string`) evaluated by ActiveMQ;
* request/reply over dynamic temporary reply queues (`temp-queue://ID:...` addresses), remote
  exceptions, timeouts;
* AMQP 1.0 transactions (`transacted=true`): a failed exchange rolls back and ActiveMQ
  redelivers with `JMSRedelivered=true`, `JMSXDeliveryCount=2`; `CLIENT_ACKNOWLEDGE`
  (`modified(delivery-failed)`) redelivered with counts 1, 2, 3;
* a durable subscription (`clientId` + `durableSubscriptionName`) keeping a message published
  while its route is stopped;
* the JMS mapping through ActiveMQ's JMS transformer (AMQP → ActiveMQ JMS message → AMQP):
  UTF-8 text, bytes, map and object (`jmsMessageType=Object`) bodies, `JMSType`,
  `JMSCorrelationID`, `JMSXGroupID`, priority, `timeToLive` (`JMSExpiration` - `JMSTimestamp`),
  non-persistent `JMSDeliveryMode=1`, ActiveMQ `ID:` message ids, int / boolean / double /
  dotted / hyphenated properties, the `x-opt-jms-msg-type` / `x-opt-jms-dest` /
  `x-opt-jms-reply-to` annotations (`includeAmqpAnnotations=true`), a `JMSReplyTo` topic
  answered by the consumer and re-used as `CamelJmsDestinationName`;
* a 3 MB bytes and a 600 KB text message (ActiveMQ's max-frame-size is 128 KiB; the consumer
  asks for 16 KiB frames), split into many transfer frames in both directions;
* `subscriptionShared=true` refused (bug 17); AMQPS with the interop CA.

ActiveMQ notes:

* ActiveMQ 5 does not redeliver after an aborted transaction or a NACK; Cameleer emulates its
  JMS client's redelivery policy on the consumer (bugs 12 and 13). Its defaults are ActiveMQ's
  (6 redeliveries, 1 s delays); set `jms.redeliveryPolicy.*` options on the broker url
  (`camel.component.jms.brokerUrl=tcp://host:61613?jms.redeliveryPolicy.redeliveryDelay=100`).
  Exponential back-off options are not emulated.
* ActiveMQ dead-letters only persistent messages (`processNonPersistent=false`); a poisoned
  non-persistent message is discarded.
* ActiveMQ 5's STOMP adapter has no JMS 2.0 shared subscriptions; use virtual topics. Its
  AMQP transport has none either (the `shared` capability is ignored).
* ActiveMQ's AMQP transport types nodes by address prefix only (`queue://`, `topic://`,
  announced as `queue-prefix` / `topic-prefix` in its open frame); the `topic` / `queue`
  terminus capabilities are echoed but ignored, and a bare address is a queue. Received
  messages carry prefixed `to` / `reply-to` addresses.
* ActiveMQ bridges protocols: the MQTT topic `a/b/c` is the STOMP / JMS topic `a.b.c` (verified
  both ways).

## Official clients

The live suite checks Cameleer *clients* against real servers. Many areas, though, are only
served by Cameleer's own emulators and embedded servers (Kafka broker, MongoDB / Cassandra /
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
`cameleer_tls::testing::official`. Each test starts the Cameleer side on an ephemeral port, runs
the client as a subprocess and checks the client's `key: value` output. The Google Cloud SDK and
a Go toolchain come from outside apt (see the script header). Every bug fixed along the way also
has a hermetic regression test: `official_regressions.rs` / `official_*_regressions.rs`,
`kafka_wire_compat.rs`, and unit tests.

### Results

Run on 2026-11-01. All 34 official-client tests pass
(`CAMEL_OFFICIAL_CLIENTS=1 cargo test --workspace --test 'official_*'`), together with the 24 hermetic regression tests in the same targets.

| Area | Client (version) | What was exercised | Result |
|---|---|---|---|
| Kafka broker (`testing::KafkaBroker`), `kafka` component | confluent-kafka 1.7.0 on librdkafka 2.3.0, kcat 1.7.1 | produce/consume with headers and keys; murmur2 partitioning matching the Java client; gzip, snappy and lz4 batches from librdkafka decoded by Cameleer, and Cameleer's compressed batches decoded by librdkafka; zstd falling back cleanly; a consumer group mixing librdkafka and Cameleer members (join/sync/heartbeat, rebalance, committed offsets); the admin client (create/describe/delete topics); kcat round trip; SASL PLAIN and SCRAM-SHA-256/512, TLS and mutual TLS | pass |
| MongoDB server | pymongo 3.11.0 | CRUD, bulk writes, aggregation pipelines, indexes (create/list/drop, unique violations → `DuplicateKeyError`), change streams, cursors and `getMore`; SCRAM-SHA-256, TLS and X.509 authentication; documents shared with the `mongodb` component | pass |
| Cassandra server | DataStax cassandra-driver 3.29.0 (protocol v4) | cluster metadata (`system.local`, `system.peers`, `system_schema` keyspaces/tables/columns), prepared and batch statements, paging, typed values, PlainText auth, TLS, data shared with the `cql` component | pass |
| Elasticsearch server | elasticsearch-py 7.17.6 | product check, index/get/update/delete, bulk, search with queries and aggregations (terms, avg/sum/min/max, value_count, cardinality, stats), paging, `_delete_by_query`, error mapping | pass |
| Redis server | redis-py 4.3.4 | strings/hashes/lists/sets/zsets, TTLs, BLPOP, `WRONGTYPE`, SCAN/HSCAN/SSCAN/ZSCAN, pipelines, MULTI/EXEC with WATCH (aborted transactions), pub/sub, username/password AUTH (failures refused), values shared with the `spring-redis` component | pass |
| AWS mock (`AwsMock`), AWS components | boto3 / botocore 1.34.46 | SigV4 (botocore's own signatures compared byte for byte, unknown keys and bad signatures rejected with the services' error codes); S3 incl. multipart (ListParts, ListMultipartUploads), presigned URLs, HTTPS; SQS in botocore's Query protocol and the JSON protocol (message attributes with MD5, FIFO deduplication, batches, error codes); SNS incl. `MessageStructure=json` to SQS; DynamoDB (tables, items, queries, batch write, transactions); Kinesis; Lambda; KMS (encrypt/decrypt with encryption context, aliases, data keys, random); Secrets Manager; STS; data exchanged with Cameleer's AWS components | pass |
| Azure mocks (`AzuriteMock`, `AzureMock`), Azure components | azure-core 1.28.0, azure-storage-blob 12.17.0, -queue 12.7.0b1, -file-datalake 12.12.0, -file-share 12.13.0, azure-identity 1.14.0b2, azure-keyvault-secrets 4.8.0b2, -keys 4.9.0b2, azure-cosmos 3.1.1 | Shared Key and account / service / container SAS; blobs (ranges, content type, case-preserving metadata, delimiter walk, block lists, chunked uploads, append blobs, leases, snapshots, Blob Batch); queues (peek, receive, update, metadata, SAS); Data Lake (paths, renames); file shares; client-credential tokens from the mock's Entra endpoint, Key Vault secrets (versions, properties, soft delete) and keys over HTTPS; Cosmos; data exchanged with the storage components | pass |
| AMQP 1.0 broker in Azure mode, Service Bus / Event Hubs components | azure-servicebus 7.11.1, azure-eventhub 5.11.3 (pyamqp) | CBS SAS authentication; Service Bus peek-lock and receive-and-delete, abandon with delivery counts, peek, scheduled messages, dead-lettering, batches, binary bodies, application properties, sessions with session state; Event Hubs properties and partition management requests, batched sends, partition receive; messages exchanged with Cameleer's components | pass |
| GCP mock (`GcpMock`) | Google Cloud SDK 570.0.0 (gcloud, gsutil), bq 2.1.32 | service-account JWT login against the mock's token endpoint over HTTPS; `gcloud storage` (buckets, multipart and resumable uploads, cat, ls/-r, describe, custom metadata, cp/mv/rm, missing objects); gsutil (JSON API over HTTPS, md5/crc32c verification); `gcloud pubsub` (topics, subscriptions, publish with attributes, pull/ack, describe, delete); bq (datasets, tables, insert, show, standard-SQL query jobs, head, rm) | pass |
| GCP mock Pub/Sub gRPC | grpcio 1.51.1, protobuf 4.21.12 | StreamingPull with a bearer token from the service-account JWT grant (unauthenticated streams refused), attributes, acks over the stream | pass |
| gRPC (`grpc` consumer and producer) | grpcio 1.51.1 | grpcio calling a Cameleer consumer route: unary with metadata, server-, client- and bidirectional streaming, status codes (NOT_FOUND with details), deadlines, 300 KB messages; plaintext h2c, gzip, TLS and mutual TLS (untrusted / missing client certificates refused). The Cameleer producer calling a grpcio server: all four call kinds, headers as metadata, PERMISSION_DENIED, 200 KB messages, plaintext, gzip and TLS (untrusted server certificate refused) | pass |
| TLS (`cameleer-tls`) | Go 1.24.7 crypto/tls | both directions: TLS 1.3 with each of AES-128-GCM, AES-256-GCM, ChaCha20-Poly1305; TLS 1.2 with all six ECDHE-ECDSA / ECDHE-RSA AEAD suites; x25519, P-256 and P-384 in both versions, Go's default hybrid X25519MLKEM768 key share (camel picks x25519); RSA certificates (PSS in 1.3); ALPN; session resumption (TLS 1.3 PSK tickets, TLS 1.2 tickets) seen by both sides; mutual TLS incl. resumed sessions keeping the client identity, missing and untrusted client certificates refused; untrusted server and host name mismatch refused | pass |
| HTTP/2 and HTTPS (cameleer-grpc `h2`, `cameleer-http`) | Go 1.24.7 net/http (bundled x/net/http2) | Go's HTTP/2 client against camel's H2 server over TLS (ALPN h2, 1 MiB upload across flow-control windows, 16 concurrent streams on one connection, 404; an HTTP/1.1-only client gets 505), Go's client against a `jetty:https` route (ALPN falls back to HTTP/1.1; echo, 404); camel's HTTP/2 client (3 MB echo, streamed responses, 12 concurrent streams) and the `https` producer with `httpVersion=HTTP/2` (headers sent) against Go's HTTP/2 server, plus HTTP/1.1 | pass |

Not verified: CouchDB (no packaged official client); Kafka zstd (librdkafka uses it only with
Produce v7+, which the embedded broker does not advertise, so librdkafka falls back to
uncompressed batches, verified); the Google Cloud Python client libraries (not packaged; the
CLIs and grpcio stand in); the Azure Cosmos SDK's newer versions (only 3.1.1 is packaged).

### Bugs found and fixed

| # | Area | Bug (found with) | Fix | Regression test |
|---|---|---|---|---|
| 1 | Kafka broker | librdkafka never joined consumer groups or compressed: its ApiVersions feature probes need the v0 ranges of FindCoordinator/JoinGroup/SyncGroup/Heartbeat/LeaveGroup and Produce v0-v2 / Fetch v0-v2 (librdkafka 2.3.0) | v0 ranges advertised, legacy request/response shims and magic 0/1 message sets (`crates/cameleer-kafka-mqtt/src/testing/kafka_broker.rs`, `kafka/protocol.rs`) | `kafka_wire_compat.rs` `api_versions_cover_librdkafka_feature_probes`, `group_coordination_v0`, `legacy_message_sets_produce_and_fetch` |
| 2 | Kafka broker | produced batches were re-encoded, losing the producer's compression and CRC | batches are stored and served verbatim (`RawBatch`) | `producer_compression_and_verbatim_batches` |
| 3 | `kafka` component | snappy and lz4 batches could not be decoded or produced (no codecs) | LZ4 frame format with xxHash32 and xerial snappy-java framing (`crates/cameleer-core/src/dataformat/codecs.rs`), `compressionCodec` option (zstd refused) | `compressed_record_batches_round_trip`, codec unit tests |
| 4 | Kafka broker | SASL after SaslHandshake v0 sends bare tokens; the broker expected SaslAuthenticate frames | bare length-prefixed tokens supported | `sasl_handshake_v0_with_bare_tokens` |
| 5 | SCRAM server | librdkafka's client-final nonce is client nonce + server nonce; Kafka accepts a suffix match, the broker refused it | `ScramServer::accept_nonce_suffix` (`crates/cameleer-tls/src/sasl.rs`) | `sasl::tests::kafka_style_nonce_suffix` |
| 6 | MongoDB server | `createIndexes` / `listIndexes` / `dropIndexes` missing, unique indexes not enforced (pymongo) | index commands and unique enforcement (`crates/cameleer-datastores/src/mongodb/server.rs`) | `official_regressions.rs` `mongo_indexes_are_listed_dropped_and_unique` |
| 7 | Cassandra server | the driver failed on `Keyspace system_schema does not exist`, then on missing `schema_version` / `host_id` columns | `system_schema` tables kept in sync with the schema, full `system.local` / `system.peers(_v2)` (`cassandra/engine.rs`, `cassandra/server.rs`) | `cassandra_system_schema_and_peer_metadata` |
| 8 | Elasticsearch server | elasticsearch-py raised `UnsupportedProductError` (no `X-Elastic-Product` header); aggregations and `_delete_by_query` missing; scroll responses without `_shards` | header (not in OpenSearch mode), aggregations, delete-by-query (`elasticsearch/server.rs`) | `elasticsearch_product_header_aggregations_and_delete_by_query` |
| 9 | Redis server | SCAN family and WATCH missing (redis-py) | cursors over keys/hashes/sets/zsets, WATCH with key versions aborting EXEC (`redis/server.rs`) | `redis_scan_family_and_watch` |
| 10 | AWS mock | botocore speaks SQS's Query protocol: JSON error bodies broke it; unknown keys answered with the wrong code | protocol-aware errors (`x-amzn-query-error`, `InvalidClientTokenId` / `UnrecognizedClientException`) (`crates/cameleer-cloud/src/testing/aws.rs`) | `official_regressions.rs` `sqs_attribute_md5_fifo_dedup_and_error_codes`, `tests/aws.rs` |
| 11 | AWS mock | SQS `MD5OfMessageAttributes`, FIFO deduplication, `SequenceNumber` missing; S3 ListParts / ListMultipartUploads missing; SNS `MessageStructure=json` delivered the whole JSON; DynamoDB ListTables / BatchWrite / TransactWrite gaps; KMS ignored the encryption context and lacked aliases and data keys | implemented (`aws.rs`, `aws_gaps.rs`, `aws_more.rs`; KMS blobs are AES-GCM with the context as AAD) | `s3_list_parts_and_multipart_uploads`, `sns_message_structure_json_delivers_the_sqs_entry`, `dynamodb_list_tables_batch_write_and_transactions`, `kms_context_aliases_and_data_keys` |
| 12 | `aws-secrets-manager` | `updateSecret` called PutSecretValue; Camel (and boto3 users) expect UpdateSecret | `UpdateSecret` in component and mock (`crates/cameleer-cloud/src/aws/secretsmanager.rs`, `aws_ext.rs`) | `secrets_manager_update_secret` |
| 13 | HTTP server helper | `Response::with_header("Content-Type")` added a second Content-Type | replaces the existing one (`crates/cameleer-cloud/src/http.rs`) | covered by the above |
| 14 | Azure mocks | blob ranges (206/416), `x-ms-blob-content-type`, metadata case, delimiter listings, snapshots, Blob Batch, container and queue SAS, queue metadata, DFS endpoint serving the Blob API, file ranges, Key Vault properties / versions / soft delete / deleted keys, OpenID discovery, HTTPS Key Vault ids | implemented (`crates/cameleer-cloud/src/testing/azure*.rs`); the `azure-storage-blob` component now sends `x-ms-blob-content-type` (`azure/blob.rs`) | `official_azure_regressions.rs` (5 tests) |
| 15 | AMQP 1.0 codec | performatives were sent with trailing nulls trimmed; pyamqp indexes fields positionally (`IndexError`), and a null `handle_max` broke it | all fields kept, limits explicit (`crates/cameleer-amqp10/src/codec.rs`, `types.rs`, `message.rs`) | `amqp10 official_regressions.rs` `performatives_keep_all_fields_and_explicit_limits` |
| 16 | AMQP 1.0 broker (Azure mode) | SAS tokens sent with CBS type `jwt`, absolute-URL link addresses, rcv-settle-mode second from producers, batch message format `0x80013700`, plain-value session filters, `locked-until-utc` in .NET ticks, management requests without `reply-to` | all accepted / produced as Service Bus does (`testing/broker.rs`, `azure.rs`, `servicebus.rs`; `Sender::send_azure_batch`) | `cbs_jwt_typed_sas_tokens_absolute_addresses_and_settlement`, `plain_session_filters_and_tick_lock_expiry`, `management_replies_without_reply_to` |
| 17 | GCP mock | gsutil only speaks HTTPS; gcloud signs JWTs for Google's token audience; multipart uploads with single-quoted boundaries and LF line endings; objects lacked `crc32c` / `metadata` / `mediaLink` (gcloud crashed), no metadata PATCH/PUT, delimiter listings, ranged downloads, `moveTo` / `rewriteTo`, `storageLayout`; Pub/Sub list endpoints; BigQuery `datasets.get`, `tabledata.list`, query jobs, and error bodies without the `errors` list bq reads | `GcpMock::start_tls`, all of the above (`crates/cameleer-cloud/src/testing/gcp.rs`, `gcp_ext.rs`), checksum verification of uploads | `official_gcp_regressions.rs` (4 tests) |
| 18 | Google components | exceptions dropped BigQuery's `errors[].reason` (`invalidQuery`, `notFound`) once the status is canonical | the reason is appended to the message (`crates/cameleer-cloud/src/gcp/mod.rs` `google_error`) | `tests/gcp_more.rs` `bigquery_sql_queries_dml_parameters_and_paging` |
| 19 | `cameleer-tls` client | TLS 1.3 sessions never resumed against Go's crypto/tls: full-handshake ClientHellos lacked `psk_key_exchange_modes`, and Go (like BoringSSL) issues NewSessionTickets only to clients listing psk_dhe_ke | listed whenever resumption is enabled (`crates/cameleer-tls/src/tls/client.rs` `client_hello`) | `cameleer-tls official_regressions.rs` `first_client_hello_lists_psk_modes_when_resumption_is_enabled` |

Notes:

* TLS 1.2 with ECDSA certificates: RFC 8422 lets the client's supported groups constrain the
  certificate's curve too, so a Go client restricted to x25519 is refused by a P-256 certificate
  (Go and OpenSSL servers behave the same); the suite checks this explicitly.
* pyamqp (azure-servicebus 7.11) mis-encodes two-character session ids; the script uses longer
  ones.

## Third-party simulators

Real AWS / Azure / Google Cloud accounts, Kafka, MongoDB, Cassandra and Elasticsearch are not
reachable from the test environment. The live suite above covers real servers from the Ubuntu
archive and the official-client suite drives Cameleer's own emulators with official clients;
the **simulator suite** closes the remaining gap from the other side: Cameleer's *components*
run against independent, widely used simulators and wire-compatible servers, installed and run
locally.

```sh
scripts/simulators.sh install     # pip (moto), npm (Azurite), Go builds (fake-gcs-server, pstest, FerretDB, kfake)
scripts/live-interop.sh start     # FerretDB needs the live PostgreSQL 16; TLS listeners use its PKI
scripts/simulators.sh start       # idempotent; `status`, `versions`, `stop`
scripts/simulators.sh test        # CAMEL_SIMULATORS=1 cargo test --workspace --test 'sim_*'
```

The suites are `crates/<crate>/tests/sim_*.rs` and do nothing unless `CAMEL_SIMULATORS=1` is
set (helpers: `cameleer_tls::testing::sim`). Every test uses unique bucket / queue / topic /
database names, because the simulators keep their state between runs. Tools live under
`$CAMEL_SIM_HOME` (default `/opt/camel-sim`), state and logs in `$CAMEL_SIM_HOME/state`; the
port table is at the top of the script. TLS listeners serve the live-interop `localhost`
certificate.

Installing offline. pypi.org, registry.npmjs.org and proxy.golang.org are reachable, but
sum.golang.org is not, and proxy.golang.org redirects large module zips to
storage.googleapis.com, which is blocked. The script therefore builds each Go program inside its
own module (`-mod=readonly`, so every dependency is checked against the upstream `go.sum`),
fetches GitHub-hosted modules straight from git (`GONOPROXY=github.com`; hashes still checked),
and packs `google.golang.org/api` from its git tag with `scripts/simulators/modzip` into a file
`GOPROXY`, which the go command accepts only if the zip's hash equals the one in `go.sum`. Only
the top-level modules (fake-gcs-server, FerretDB, kfake, the pubsub module and
`golang.org/x/mod`) cannot be checked against the checksum database: they come from
proxy.golang.org / GitHub over TLS at pinned versions / commits.

### Results

Run on 2026-11-01. All 31 simulator tests pass
(`CAMEL_SIMULATORS=1 cargo test --workspace --test 'sim_*'`), together with the 4 hermetic
regression tests in `sim_regressions.rs`; the hermetic workspace suite (`cargo test --workspace`)
stays green, and `official_kafka.rs` (librdkafka, now joining groups with JoinGroup v4) passes.

| Simulator (version) | Components | What was verified | Result |
|---|---|---|---|
| moto 5.2.5 (server mode, pip `moto[server]`) | `aws2-s3`, `aws2-sqs`, `aws2-sns`, `aws2-ddb`, `aws2-ddbstream`, `aws2-kinesis`, `aws2-kinesis-firehose`, `aws2-eventbridge`, `aws-secrets-manager` (+ `{{aws:...}}`), `aws2-kms`, `aws2-sts`, `aws2-iam`, `aws2-ec2`, `aws2-cw`, `aws2-ses`, `aws2-step-functions`, `aws2-lambda`, `aws2-athena` | S3: buckets, objects with UTF-8 / `+` / space keys, metadata, ranges, prefix listings, copy, delete, `NoSuchKey` / `BucketNotEmpty` / `NoSuchBucket`, presigned download links, multipart uploads with 5 MiB parts (3 parts, `-3` ETag) and moto's `EntityTooSmall` (upload aborted), streamed multipart upload from a file and streamed download into a file, streaming upload mode, consumer with `moveAfterRead`; SQS: send with message attributes and MD5, batches, prefix listing, consumer with attributes, redelivery after the visibility timeout, FIFO with content-based deduplication, purge / delete, errors; SNS: auto-created topic subscribed to SQS (`subscribeSNStoSQS`), subject and attributes in the SNS envelope, `MessageStructure=json`; DynamoDB: table created on first use, typed items (lists, sets), Update / Condition / Filter / Projection expressions with attribute names and values (moto enforces DynamoDB's reserved words), `ConditionalCheckFailedException`, Query, Scan (also legacy `ScanFilter`), BatchGetItems, UpdateTable enabling a stream, the `aws2-ddbstream` consumer reading INSERT / MODIFY / REMOVE with old and new images, DeleteTable; Kinesis: two-shard stream, PutRecord / PutRecords, a consumer per shard; Firehose: delivery stream with an extended S3 destination, single and batch records landing in S3; EventBridge: rule with an event pattern and an SQS target, only matching events delivered, describe / list / disable / remove / delete; Secrets Manager: create / get by name and ARN / update / describe / list / binary secrets / delete / restore, `{{aws:secret/field}}` placeholders; KMS: create / list / describe / disable / enable keys, encrypt and decrypt, schedule deletion; STS: AssumeRole, GetSessionToken, GetFederationToken; IAM: users, groups, access keys, `EntityAlreadyExists`, `NoSuchEntity`; EC2: run / describe / stop / start / tag / terminate instances, `InvalidInstanceID.NotFound`; CloudWatch: PutMetricData (single, dimensions, timestamps, lists) read back with ListMetrics; SES: moto's verified-sender rule (`MessageRejected` until verified), plain, HTML with Cc / Reply-To, raw messages; Step Functions: state machines, executions, `ExecutionAlreadyExists`; Lambda management API (create from a zip, versions, aliases, tags, get / list / delete); Athena: start / get / list query executions. A second moto server enforces IAM authentication and SigV4 verification: header-signed S3 (keys with spaces and `+`), SQS, DynamoDB and IAM requests from Cameleer are accepted; a wrong secret gets `SignatureDoesNotMatch`, an unknown key `InvalidAccessKeyId` / `UnrecognizedClientException`. HTTPS server verified by host name and IP SAN (untrusted certificate refused unless `trustAllCertificates=true`) | pass |
| Azurite 3.37.0 (npm `azurite`) | `azure-storage-blob`, `azure-storage-queue` | containers, block blobs with content type and metadata, names with spaces / `+` / UTF-8 content, paged listings, `BlobNotFound` / `ContainerAlreadyExists` / `ContainerNotFound`, wrong account key refused (403); block lists (stage, uncommitted / committed lists, recommit, `InvalidBlockList`), append blobs, page blobs (ranges, resize, clear), leases (acquire, `LeaseIdMissing` 412, renew, release, `leaseBlob=true`, break), server-side copy, download links (service SAS) checked by Azurite incl. tampering (403); consumer with prefix and `deleteAfterRead`; account SAS made by Cameleer (full, read-only → `AuthorizationPermissionMismatch`, forged, expired); queues: send (XML-special and UTF-8 text), peek, receive with visibility timeout, update (new pop receipt, `PopReceiptMismatch` for the old one), delete, clear, consumer deleting processed messages and redelivering a failing one with a growing dequeue count, SAS, `QueueNotFound`; Blob and Queue over HTTPS | pass |
| fake-gcs-server 1.53.1 | `google-storage` | `authenticate=false` against the emulator; bucket auto-creation, objects with UTF-8 / `+` names and content type, get, list, copy to a created bucket, list / delete buckets, a resumable upload session in 256 KiB chunks, delete, 404; consumer with `moveAfterRead`; HTTPS | pass |
| pstest (cloud.google.com/go/pubsub 1.50.1) | `google-pubsub` streaming-pull consumer | gRPC StreamingPull without authentication: message id, ack id, publish time and attributes, every message acknowledged once (as pstest counts acks), a failing message nacked and redelivered (3 deliveries). pstest has no REST API, so topics, subscriptions and messages are created through a small HTTP control API around pstest's Go methods (`scripts/simulators/pstest`) | pass |
| FerretDB 1.24.2 on PostgreSQL 16.15 | `mongodb`, `mongodb-gridfs` | insert (single / many), `E11000` duplicate key, findById / findOneByQuery / findAll with criteria, sort, limit, skip, projection, `$or` / `$regex`, getMore batches, count, distinct, multi / upsert / `$unset` updates, invalid update operators, save, aggregation (`$match`, `$unwind`, `$group`, `$sort`, `$count`), remove, db / collection stats, commands (`NotImplemented` / unknown command errors), Extended JSON dates, longs and binaries, bulkWrite (ordered / unordered, upserts), dynamicity, `createCollection=false`; SCRAM-SHA-256 and SCRAM-SHA-1 (FerretDB's new authentication), wrong password → `MongoSecurityException`; TLS verified against the interop CA (untrusted CA refused); tailable consumer on a capped collection; GridFS create / findOne / count / listAll / remove of a 600 KB file and the GridFS consumer | pass |
| kfake (franz-go, commit of v1.20.6) | `kafka` | a three-broker cluster: topic creation, murmur2 partitioning, keys, string / long headers, offsets per partition, commits on stop as reported by kfake's OffsetFetch; auto-created topics, explicit / header partitions, `kafka.OVERRIDE_TOPIC`, acks 0 / 1 / all, array bodies, round robin; gzip, snappy and lz4 batches decoded by franz-go; a consumer group of two routes splitting four partitions and rebalancing when one leaves; committed offsets surviving a restart; manual commits; SASL PLAIN, SCRAM-SHA-256 and SCRAM-SHA-512, over plaintext and TLS; TLS by `sslContextParameters` and by `sslTruststoreLocation`; wrong passwords and an untrusted certificate refused | pass |

### Bugs found and fixed

| # | Component | Bug (found against) | Fix | Regression test |
|---|---|---|---|---|
| 1 | `kafka` client | `create_topics` sent CreateTopics to any broker; kfake (like ZooKeeper-mode Kafka) answers `NOT_CONTROLLER` (41, shown as `UNKNOWN_ERROR_CODE`) from the others | sent to the controller named by Metadata (`crates/cameleer-kafka-mqtt/src/kafka/client.rs`); error names for 29, 30, 38, 41, 79 (`protocol.rs`) | `cameleer-kafka-mqtt` `sim_regressions.rs` `create_topics_is_sent_to_the_controller` (fake two-node cluster) |
| 2 | `kafka` consumer groups | consumers joined with JoinGroup v2 and never received records from kfake, which (for v0-v3 joins) answers a first join with an empty member id; modern clients join with v4 and KIP-394's `MEMBER_ID_REQUIRED` | JoinGroup v4: on `MEMBER_ID_REQUIRED` the member rejoins with the assigned id (`kafka/consumer.rs`); the embedded broker answers v4 joins the same way, like Kafka 2.2+ (`testing/kafka_broker.rs`) | `join_group_v4_member_id_required`; `official_kafka.rs` (librdkafka now joins with v4) still passes |
| 3 | `azure-storage-blob` | `listBlobs` with `maxResultsPerPage` returned only the first page (Azurite) | the option is the page size and every page is read, as Camel collects the SDK's PagedIterable (`crates/cameleer-cloud/src/azure/blob.rs`) | `cameleer-cloud` `sim_regressions.rs` `list_blobs_reads_every_page` |
| 4 | `google-storage` (and other Google components) | with `authenticate=false` (emulators) there is no key, so bucket operations failed with "A project id ... is required" (fake-gcs-server) | the default project comes from `GOOGLE_CLOUD_PROJECT` / `GCLOUD_PROJECT`, as in the Google client libraries (`crates/cameleer-cloud/src/gcp/mod.rs` `project_id`) | `storage_bucket_operations_use_the_default_project_without_a_key` |

### Simulator limitations (asserted around, not Cameleer bugs)

* moto: presigned URLs are not checked (any signature is accepted) unless IAM authentication is
  on, and with IAM authentication on a presigned GET fails with a 500 (`KeyError:
  HTTP_AUTHORIZATION`), so presigned links are only checked for content; Lambda invocations need
  Docker; separate moto servers do not share state. moto enforces DynamoDB's reserved words
  (`total`, `status`, ...) where Cameleer's own emulator accepted them.
* Azurite: no Data Lake (DFS), Files, change feed or Entra ID tokens without a real tenant
  (`--oauth basic` validates issuers); its Table endpoint runs but no Cameleer component uses
  it.
* fake-gcs-server does not verify OAuth tokens or V4 signed URLs, so the authenticated paths stay
  covered by the official-client suite (gcloud / gsutil against `GcpMock`).
* pstest speaks gRPC only, so the REST producer and the synchronous-pull consumer are not
  verified against it.
* FerretDB 1.24: no change streams (`$changeStream` is `NotImplemented`), no `$max` group
  accumulator, and `$sum` of a field path after `$unwind` returns 0 (pymongo sees the same);
  the tests avoid these.
* kfake answers JoinGroup v0-v3 first joins with an empty member id (see bug 2).

### Could not run

| Simulator / server | Why |
|---|---|
| Apache Kafka (KRaft), Redpanda | archive.apache.org is reachable only through the WebFetch tool, which refuses downloads over 10 MB (the Kafka 4.3.2 tarball is far larger); dlcdn / downloads.apache.org and Redpanda's hosts are blocked. franz-go's kfake stands in. |
| Elasticsearch, OpenSearch | the official tarballs (also OpenSearch's minimal distribution) are hundreds of MB, far over the 10 MB limit, artifacts.opensearch.org / elastic.co are blocked, nothing is packaged in Ubuntu. ZincSearch (Go) serves its partial ES-compatible API under `/es/`, a path prefix `hostAddresses` cannot express. |
| Apache Cassandra, ScyllaDB | the Cassandra 5.0 tarball exceeds the 10 MB limit (and the Apache CDN is blocked); ScyllaDB is not available offline. |
| LocalStack | needs Docker (host mode was removed in LocalStack 2). |
| Azure Cosmos DB emulator, Service Bus emulator | Windows-only / Docker-only. |
| gcloud Pub/Sub, Firestore and Datastore emulators | `gcloud components install` needs dl.google.com (blocked), the Firebase emulator jars come from storage.googleapis.com (blocked, > 10 MB). pstest replaces the Pub/Sub emulator for the gRPC consumer; Firestore has no offline alternative. |

## DICOM

`scripts/dicom-interop.sh` installs DCMTK 3.6.7 and Orthanc 1.12.2 with the DICOMweb plugin 1.16
(Ubuntu 24.04 packages: `apt-get install` as root, or `apt-get download` plus pinned SHA-256 and
`dpkg -x` into a user directory without root) and starts DCMTK `storescp` (plain and TLS, BCP 195
non-downgrading profile) and Orthanc. `CAMEL_DICOM_INTEROP=1 cargo test -p cameleer-health --test
live_dicom` then runs **7/7** (Ubuntu 24.04 under WSL2, 2026-11-09):

| test | what runs against what |
|---|---|
| `live_cameleer_scu_to_dcmtk_storescp` | Cameleer SCU: C-ECHO, C-STORE in Explicit, Implicit and Deflated syntaxes to `storescp`; the files `storescp` wrote parse to the same data sets; `dcmdump` reads a Cameleer-written deflated file |
| `live_cameleer_scu_tls_to_dcmtk` | Cameleer SCU over TLS to `storescp +tls` |
| `live_dcmtk_tools_to_cameleer_scp` | `echoscu`, `storescu`, `findscu -X`, `getscu` and `movescu` (to `storescp`) against a Cameleer SCP route |
| `live_dcmtk_tls_to_cameleer_scp` | `storescu +tls` against a Cameleer SCP with TLS |
| `live_orthanc_as_pacs` | Cameleer SCU: C-ECHO, C-STORE, C-FIND (study and image level), C-GET and C-MOVE from Orthanc to a Cameleer SCP |
| `live_orthanc_dicomweb` | the `dicomweb` producer against Orthanc's DICOMweb plugin: STOW-RS, QIDO-RS, WADO-RS, metadata |
| `live_orthanc_dicomweb_client_to_cameleer_server` | Orthanc's DICOMweb client against a `dicomweb` consumer: STOW-RS, QIDO-RS, WADO-RS |

The parser is also checked on 33 public pydicom test files (`scripts/fetch-dicom-testdata.sh`,
`tests/dicom_corpus.rs`): every file reads and writes back byte for byte.

Bug found and fixed: a deflated data set of odd length was sent as an odd-length PDV fragment,
and DCMTK aborted the association ("Odd Fragment Length"). Deflated data sets are now padded to
an even length (PS3.5 A.5) and PDV fragments are always even
(`crates/cameleer-health/src/dicom/codec.rs` `dataset_to_bytes`, `assoc.rs` `max_fragment`; regression
test `codec::tests::deflated_data_sets_have_even_length`).

Not verified yet: dcm4che, real modalities, and the cloud DICOM services (Azure Health Data
Services, Google Cloud Healthcare API, AWS HealthImaging), which need accounts from the owner.

## MCP

The MCP server is checked with the official conformance suite, `@modelcontextprotocol/conformance`
0.1.16 (pinned and checksum-verified), by `scripts/mcp-conformance.sh` against
`cameleer run --mcp` serving `scripts/mcp-conformance/fixtures.camel.yaml`: **39/39** scenarios
for protocol 2025-11-25 and **26/26** for 2025-06-18 (2026-11-06). The suite predates 2026-07-28,
so that era is covered by the hermetic tests in `crates/cameleer-ai/tests/mcp.rs`. A field test
with Claude and the official MCP Python SDK (`scripts/mcp-fieldtest`, 40 requests against
`examples/agents/mcp-shop`) is described in
[Ask Claude about your business](guides/mcp-shop-example.md).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
