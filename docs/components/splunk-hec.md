# Splunk HEC component

> The `splunk-hec` component sends each exchange to Splunk as one event through the HTTP Event Collector (HEC), with the HEC token, index, source, sourcetype, host and time.

**Scheme:** `splunk-hec:` · **Crate:** `cameleer-saas` · **Cargo feature:** [`saas`](../guides/build-features.md) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Splunk HEC](https://camel.apache.org/components/next/splunk-hec-component.html)

## When to use it

- Ship application events, audit records or alerts from a route into Splunk.
- Put Cameleer between log shippers and Splunk: receive Beats events with [lumberjack](lumberjack.md)
  or syslog with [syslog](syslog.md), filter and enrich them, and index only what matters.
- Replace a Camel route that uses `camel-splunk-hec`: the URI, the options, the header and the JSON
  sent are the same.

## URI format

```text
splunk-hec:host:port?token=...[&options]
```

`host:port` is the HEC listener (Splunk's default HEC port is 8088). `token` is required and is a HEC
token (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`), not a user token. The address and the token are
checked when the route starts, as in Camel. The request goes to
`https://host:port/services/collector/event` with the header `Authorization: Splunk <token>`.

## Example

Receive events from Filebeat, keep the warnings and errors, and index the event map in Splunk:

<!-- example: splunk-hec-from-beats -->
```yaml
- route:
    id: beats-to-splunk
    from:
      uri: lumberjack:0.0.0.0:{{env:BEATS_PORT:5044}}
      steps:
        - filter:
            simple: "${body[log][level]} in 'warn,error'"
            steps:
              - to: "splunk-hec:{{env:SPLUNK_HEC:splunk.example.com:8088}}?token={{env:SPLUNK_HEC_TOKEN}}&index=main&sourceType=_json&source=filebeat&bodyOnly=true"
```

With `bodyOnly=true` the Filebeat event (a map) is the Splunk `event`; without it the event is
`{"body": ..., "headers": {...}}`, as in Camel. A Splunk certificate from a private CA is trusted
with `sslContextParameters=#bean` or the global `camel.ssl.trustStore`.

Each exchange is one POST with this JSON:

<!-- not-camel -->
```json
{"sourcetype":"_json","source":"filebeat","index":"main","host":"gateway-01","event":{"message":"...","log":{"level":"error"}}}
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:audit")
        .to("splunk-hec:splunk.example.com:8088?token={{env:SPLUNK_HEC_TOKEN}}&index=audit&bodyOnly=true");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `token` | | **Required.** The HEC token. |
| `index` | `camel` | The Splunk index. |
| `source` | `camel` | The event `source`. |
| `sourceType` | `camel` | The event `sourcetype`. |
| `host` | the local host name | The event `host` field (not the server connected to); `unknown` when the host name cannot be found. |
| `time` | | Epoch time of the event; Splunk uses the arrival time when it is not set. The `CamelSplunkHECIndexTime` header overrides it. |
| `bodyOnly` | `false` | Send only the body as the event. Wins over `headersOnly`. |
| `headersOnly` | `false` | Send only the headers as the event. |
| `splunkEndpoint` | `/services/collector/event` | The HEC path, appended as given (a query string included, for example `/services/collector/event?auto_extract_timestamp=true`). `/services/collector/raw` receives the same JSON envelope, as in Camel. |
| `https` | `true` | Use HTTPS; `false` for plain HTTP. |
| `sslContextParameters` | | `#bean` with the trust store (and a key pair for mutual TLS). Also `camel.component.splunk-hec.sslContextParameters`. |
| `useGlobalSslContextParameters` | `false` | Use the global `camel.ssl.*` parameters. |
| `skipTlsVerify` | `false` | **Refused when `true`.** Cameleer never turns certificate verification off; trust a private Splunk by adding its certificate or CA to the trust store. |
| `lazyStartProducer`, `autowiredEnabled` | | Accepted; they have no effect (the producer opens connections when the first message is sent). |

The body is sent as JSON: a text body as a string, a map or list as an object or array, bytes as text.
A list body is one event whose `event` is the array; the component does not split or batch (use the
[Split EIP](../guides/eips.md) for one event per element). Connections are kept alive and reused.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelSplunkHECIndexTime` | in | Epoch time of the event; overrides the `time` option. |

Only status 200 counts as success. Any other answer fails the exchange with a `RuntimeCamelException`
whose message holds the status line and Splunk's answer, for example
`Splunk HEC request failed: HTTP/1.1 403 Forbidden` and `{"text":"Invalid token","code":4}` on the next line.

## Run it with Docker

```sh
export SPLUNK_HEC=splunk.example.com:8088 SPLUNK_HEC_TOKEN=00000000-0000-0000-0000-000000000000
docker run --rm -p 5044:5044 -e SPLUNK_HEC -e SPLUNK_HEC_TOKEN -v "$PWD:/routes:ro" cameleering/cameleer run /routes/beats-to-splunk.camel.yaml
```

## Common questions

### How do I retry when Splunk is busy?

HEC answers 503 (`Server is busy`) under load. Add an `onException` for `RuntimeCamelException` with
`maximumRedeliveries` and a `redeliveryDelay`, or put a [seda](seda.md) queue in front of the producer.

### Can I send several events in one request?

Not with this component, as in Camel: one exchange is one request with one event. Aggregate the
messages into a list when one event holding the array is what you want.

### Does it support HEC acknowledgement (`X-Splunk-Request-Channel`)?

No; Camel does not either. A 200 answer means Splunk accepted the event for indexing.

## Related

- [lumberjack](lumberjack.md), [syslog](syslog.md), [elasticsearch](elasticsearch.md), [log](log.md)
- [Observability: log pipelines](../guides/observability.md#log-pipelines)
- [Apache Camel Splunk HEC component](https://camel.apache.org/components/next/splunk-hec-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
