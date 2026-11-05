# Paho MQTT 5 component

> The `paho-mqtt5` component publishes and subscribes to MQTT 5.0 topics, with user properties, clean start and session expiry.

**Scheme:** `paho-mqtt5:` · **Crate:** `cameleer-kafka-mqtt` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Paho MQTT 5](https://camel.apache.org/components/next/paho-mqtt5-component.html)

## When to use it

- Publish device status from an edge gateway as a retained message, so new subscribers see the latest value.
- Use MQTT 5 user properties as message headers (trace ids, device metadata).
- Keep a session for a limited time with `sessionExpiryInterval`.

Everything in [paho](paho.md) applies; this page lists the differences.

## URI format

```text
paho-mqtt5:topic[?options]
```

## Example

<!-- example: paho-mqtt5-retained-status -->
```yaml
- route:
    id: publish-gateway-status
    from:
      uri: timer:status
      parameters:
        period: 30000
        delay: 0
      steps:
        - setBody:
            simple: '{"gateway":"edge-gw-1","status":"online","seq":${exchangeProperty.CamelTimerCounter}}'
        - to: paho-mqtt5:devices/edge-gw-1/status?brokerUrl={{env:MQTT_URL:tcp://localhost:1883}}&qos=1&retained=true
```

Every 30 seconds the gateway publishes its status as a retained QoS 1 message. A dashboard that subscribes
later gets the last status at once.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:status?period=30000&delay=0")
            .route_id("publish-gateway-status")
            .set_body(simple(r#"{"gateway":"edge-gw-1","status":"online","seq":${exchangeProperty.CamelTimerCounter}}"#))
            .to("paho-mqtt5:devices/edge-gw-1/status?brokerUrl=tcp://localhost:1883&qos=1&retained=true");
    })
}
```

## Options

The options of [paho](paho.md#options), with these differences:

| option | default | description |
|---|---|---|
| `cleanStart` | `true` | MQTT 5 name of `cleanSession`. |
| `sessionExpiryInterval` | `-1` | Seconds the broker keeps the session after a disconnect. `-1` does not send the property. |

## Message headers

The headers of [paho](paho.md#message-headers). In addition, MQTT 5 user properties of received messages
become headers, and the producer also reads `CamelPahoMqtt5OverrideTopic`.

## Run it with Docker

```sh
docker run --rm -e MQTT_URL=tcp://mosquitto:1883 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/status.camel.yaml
```

## Common questions

### Should I use paho or paho-mqtt5?

Use `paho-mqtt5` when the broker supports MQTT 5 and you need user properties or session expiry. Use
`paho` for MQTT 3.1.1 brokers and devices.

### How do I clear a retained message?

Publish an empty body with `retained=true` to the same topic.

## Related

- [paho](paho.md), [timer](timer.md), [kafka](kafka.md)
- [Apache Camel Paho MQTT 5 component](https://camel.apache.org/components/next/paho-mqtt5-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
