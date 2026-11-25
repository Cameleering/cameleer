# Lumberjack component

> The `lumberjack` component is a Lumberjack (Beats protocol) server: Filebeat, Winlogbeat and the other Beats, Logstash and logstash-forwarder send it log events, and each event becomes one exchange whose body is the event as a map.

**Scheme:** `lumberjack:` · **Crate:** `cameleer-protocols` · **Cargo feature:** [`protocols`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Lumberjack](https://camel.apache.org/components/next/lumberjack-component.html)

## When to use it

- Take the place of Logstash in front of a log store: Beats send to Cameleer, a route filters,
  enriches and routes the events to [splunk-hec](splunk-hec.md), [elasticsearch](elasticsearch.md),
  [kafka](kafka.md) or a file.
- Collect logs at an edge site and forward only what matters over a slow link.
- Turn log lines into actions: alert in a chat, open a ticket, call an API.

## URI format

```text
lumberjack:host[:port][?options]
```

`host` is the interface to listen on (`0.0.0.0` for all). The default port is 5044, the port Beats'
`output.logstash` uses.

## Example

Receive Filebeat events and log the line and the file it came from:

<!-- example: lumberjack-filebeat -->
```yaml
- route:
    id: filebeat-in
    from:
      uri: lumberjack:0.0.0.0:{{env:BEATS_PORT:5044}}
      steps:
        - setBody:
            simple: "${body[log][file][path]}: ${body[message]}"
        - to: log:beats
```

Point Filebeat at it with `output.logstash: { hosts: ["cameleer-host:5044"] }`. A line
`GET /index.html 200` from `/var/log/nginx/access.log` is logged as
`/var/log/nginx/access.log: GET /index.html 200`.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("lumberjack:0.0.0.0:5044")
            .route_id("filebeat-in")
            .set_body(simple("${body[log][file][path]}: ${body[message]}"))
            .to("log:beats");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `sslContextParameters` | | `#bean` with the server key pair; the server then speaks TLS only. Also `camel.component.lumberjack.sslContextParameters`. Client certificates are requested when the parameters ask for them. |
| `useGlobalSslContextParameters` | `false` | Use the global `camel.ssl.*` parameters (they need key material). |
| `bridgeErrorHandler`, `exceptionHandler`, `exchangePattern`, `autowiredEnabled` | | Accepted; they have no effect. A failed exchange is logged at WARN by the route's error handler. |

The body is the decoded event: a map for a JSON event (nested objects are maps, arrays are lists), a
map of strings for a version 1 data frame. No headers are set, as in Camel.

How the server talks to the shipper (as Camel's `LumberjackFrameDecoder`): protocol versions 1 and 2;
window (`W`), JSON (`J`), data (`D`) and zlib-compressed (`C`) frames. The events of a window go
through the route in order, one at a time. When all of them succeeded, the server sends one ACK
(`A` with the window size). When one fails, the server sends no ACK and closes the connection; the
events after it are not routed, and the shipper sends the whole window again (at-least-once delivery,
so make the route idempotent when duplicates matter). A malformed frame closes the connection too.

## Message headers

None. Everything the shipper sends is in the body (`@timestamp`, `message`, `host`, `log`, `agent`,
`fields`, ...).

## Run it with Docker

```sh
docker run --rm -p 5044:5044 -v "$PWD:/routes:ro" cameleering/cameleer run /routes/filebeat-in.camel.yaml
```

## Common questions

### How do I enable TLS for Filebeat?

Bind an `SslContextParameters` bean with the server certificate and key (or set `camel.ssl.keyStore`
and use `useGlobalSslContextParameters=true`), and give Filebeat the CA:
`output.logstash.ssl.certificate_authorities: ["ca.crt"]`. For mutual TLS, add a trust store and
`clientAuthentication=REQUIRE` to the parameters, and give Filebeat `ssl.certificate` and `ssl.key`.

### Is there a producer to send to Logstash?

No; Camel has none either. Send to Logstash's HTTP input with the [http](http.md) component.

### What does Filebeat do while the route is slow?

It waits for the ACK of the window. Filebeat's `output.logstash.timeout` (30 s by default) must be
longer than the time the route takes for one window (`bulk_max_size` events, 2048 by default).

## Related

- [splunk-hec](splunk-hec.md), [syslog](syslog.md), [elasticsearch](elasticsearch.md), [kafka](kafka.md)
- [Observability: log pipelines](../guides/observability.md#log-pipelines)
- [Apache Camel Lumberjack component](https://camel.apache.org/components/next/lumberjack-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
