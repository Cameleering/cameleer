# Paho (MQTT) component

> The `paho` component publishes and subscribes to MQTT 3.1.1 topics, for IoT sensors, edge gateways and any MQTT broker such as Mosquitto, EMQX, HiveMQ or AWS IoT Core.

**Scheme:** `paho:` · **Crate:** `cameleer-kafka-mqtt` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Paho](https://camel.apache.org/components/next/paho-component.html)

## When to use it

- Collect readings from sensors that publish to MQTT (`sensors/+/temperature`) and forward them to Kafka,
  a database or an HTTP API.
- Send commands or configuration to devices on an edge network.
- Bridge MQTT and another broker (Kafka, AMQP) on a gateway, with QoS 1 or 2 for reliable delivery.

For MQTT 5 features (user properties, session expiry) use [paho-mqtt5](paho-mqtt5.md). The MQTT client is
built into Cameleer (no Eclipse Paho library).

## URI format

```text
paho:topic[?options]
```

Consumers may use the wildcards `+` (one level) and `#` (all remaining levels). In a URI query write `#`
as `%23`; in the YAML `parameters` block or the path it can be written as is.

## Example

<!-- example: paho-sensors-to-kafka -->
```yaml
- route:
    id: sensors-to-kafka
    from:
      uri: paho:sensors/+/temperature
      parameters:
        brokerUrl: "{{env:MQTT_URL:tcp://localhost:1883}}"
        clientId: edge-gw-1
        qos: 1
      steps:
        - setHeader:
            name: kafka.KEY
            simple: "${header.CamelMqttTopic}"
        - to: kafka:temperatures?brokers={{env:KAFKA_BROKERS:localhost:9092}}
```

A reading published to `sensors/room1/temperature` reaches the Kafka topic `temperatures` with the MQTT
topic as record key. The MQTT message is acknowledged after the Kafka send succeeded.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("paho:sensors/+/temperature?brokerUrl=tcp://localhost:1883&clientId=edge-gw-1&qos=1")
            .route_id("sensors-to-kafka")
            .set_header("kafka.KEY", simple("${header.CamelMqttTopic}"))
            .to("kafka:temperatures?brokers=localhost:9092");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `brokerUrl` | `tcp://localhost:1883` | Broker address. `ssl://`, `mqtts://` or `tls://` connect with TLS (default port 8883). `ws://` and `wss://` are not supported. |
| `serverURIs` | | List of brokers; the first one is used. |
| `clientId` | generated | MQTT client id. Use a fixed id for persistent sessions. |
| `qos` | `2` | Quality of service: `0`, `1` or `2`. |
| `retained` | `false` | Producer: publish as a retained message. |
| `userName`, `password` | | Credentials. |
| `cleanSession` | `true` | Start a clean session. `false` keeps subscriptions and queued messages while the route is down. |
| `keepAliveInterval` | `60` | Keep-alive in seconds. |
| `connectionTimeout` | `30` | Connect timeout in seconds. |
| `automaticReconnect` | `true` | Reconnect after a lost connection. |
| `maxReconnectDelay` | `128000` | Longest wait (ms) between reconnect attempts. |
| `willTopic`, `willPayload`, `willQos`, `willRetained` | | Last will message, published by the broker if the client disappears. |
| `httpsHostnameVerificationEnabled` | `true` | `false` skips the host name check of the broker certificate. |
| `sslContextParameters`, `useGlobalSslContextParameters` | | TLS trust settings. |
| `persistence`, `filePersistenceDirectory`, `maxInflight`, `executorServiceTimeout`, ... | | Accepted and ignored. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelMqttTopic` | out | Topic the message was published to. |
| `CamelMqttQoS` | out | QoS of the delivery. |
| `CamelMqttRetained` | out | Whether it was a retained message. |
| `CamelPahoDuplicate` | out | The broker's duplicate flag. |
| `CamelPahoMessageId` | out | MQTT packet id. |
| `CamelPahoOverrideTopic` | in | Producer: publish to this topic instead. |
| `CamelPahoQos` | in | Producer: override `qos`. |
| `CamelPahoRetained` | in | Producer: override `retained`. |

The consumer's body is the payload as bytes. Producers send byte bodies as is and other values as UTF-8
text; QoS 1 and 2 sends wait for the broker's acknowledgement.

## Run it with Docker

```sh
docker run --rm -e MQTT_URL=tcp://mosquitto:1883 -e KAFKA_BROKERS=kafka:9092 \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/sensors.camel.yaml
```

The image is small and starts in milliseconds, so it fits on gateways such as a Raspberry Pi class device.

## Common questions

### How do I subscribe to all topics under a prefix?

Use `#` at the end: `paho:sensors/#`. In a URI query string encode it as `%23`.

### How do I keep messages while my route is offline?

Use a fixed `clientId`, `cleanSession=false` and `qos=1` or `2`. The broker queues messages for the session.

### How do I connect to AWS IoT Core or another TLS broker?

Use `brokerUrl=ssl://endpoint:8883`. Trust comes from `sslContextParameters=#bean`,
`useGlobalSslContextParameters=true`, the global `camel.ssl.*` parameters, or else the system trust store.

## Related

- [paho-mqtt5](paho-mqtt5.md), [kafka](kafka.md), [seda](seda.md)
- [Apache Camel Paho component](https://camel.apache.org/components/next/paho-component.html)
