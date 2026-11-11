# Cameleer at the edge

> Cameleer runs integration routes on gateways and small devices: a single native binary of
> about 10 MB of memory that ingests MQTT, syslog and (with the health pack) HL7 locally, filters and aggregates on
> site, buffers when the uplink is down, and forwards to Kafka or HTTP in the cloud.

Edge gateways sit between machines and the cloud. They need to speak local protocols, keep
working offline, and fit next to other software on small hardware. Apache Camel does all of
this on a JVM; Cameleer runs the same routes without one. This cookbook shows five patterns,
each a complete route file in [`examples/edge/`](../../examples/edge/) that runs end to end in
the test suite (`crates/cameleer-main/tests/docs_guides.rs`) against the embedded MQTT and Kafka
brokers and HTTP mocks.

| pattern | file | what it shows |
|---|---|---|
| [Sensor rollups](#filter-and-aggregate-mqtt-sensor-readings) | `mqtt-sensor-rollup.camel.yaml` | MQTT in, validation, per-sensor aggregation, Kafka out |
| [Store and forward](#store-and-forward-when-the-uplink-is-down) | `store-and-forward.camel.yaml` | retries, a local file buffer and replay when the uplink is down |
| [Syslog to Kafka](#syslog-to-kafka) | `syslog-to-kafka.camel.yaml` | protocol bridging with severity filtering |
| [HL7 over MLLP](#hl7-v2-over-mllp-to-an-http-api) | `hl7-mllp-to-http.camel.yaml` | healthcare messages with acknowledgements after delivery ([health pack](../packs/health/index.md)) |
| [Gateway presence](#gateway-presence-with-retained-messages-and-a-last-will) | `gateway-heartbeat.camel.yaml` | retained status and MQTT last will |

## Filter and aggregate MQTT sensor readings

Sensors publish a reading every few seconds; the cloud needs a summary. The route subscribes
to `sensors/+/temperature`, takes the sensor id from the topic, drops readings that are not
numbers or are outside the sensor's range, and aggregates five readings per sensor (or what
arrived within a minute) into one rollup with count, min, max and average. Only rollups go to
Kafka, keyed by sensor, so a sensor's rollups stay in order on one partition.

<!-- example: edge-sensor-rollup -->
```yaml
- route:
    id: sensor-rollup
    from:
      uri: "paho:sensors/+/temperature?brokerUrl={{env:MQTT_BROKER:tcp://localhost:1883}}&qos=1&clientId={{env:GATEWAY_ID:gateway-1}}-rollup"
      steps:
        - convertBodyTo:
            type: java.lang.String
        - setHeader:
            name: sensor
            simple: "${header.CamelMqttTopic.split('/')[1]}"
        - choice:
            when:
              - simple: "${body} !regex '-?[0-9]+(\\.[0-9]+)?'"
                steps:
                  - log: "dropped malformed reading '${body}' from ${header.sensor}"
                  - stop: {}
        - convertBodyTo:
            type: java.lang.Double
        - choice:
            when:
              - simple: "${body} < -40 || ${body} > 125"
                steps:
                  - log: "dropped out-of-range reading ${body} from ${header.sensor}"
                  - stop: {}
        - aggregate:
            correlationExpression:
              header: sensor
            aggregationStrategy: "#class:org.apache.camel.processor.aggregate.GroupedBodyAggregationStrategy"
            completionSize: 5
            completionInterval: 60000
            steps:
              - setBody:
                  jq: '{gateway: "{{env:GATEWAY_ID:gateway-1}}", sensor: header("sensor"), count: length, min: min, max: max, avg: (add / length)}'
              - marshal:
                  json: {}
              - setHeader:
                  name: kafka.KEY
                  simple: "${header.sensor}"
              - to: "kafka:sensor-rollups?brokers={{env:KAFKA_BROKERS:localhost:9092}}"
```

The test publishes `20`, `garbage`, `21.5`, `999`, `22`, `23.5` and `18` for sensor `t1` and one
reading for `t2`. Kafka receives one rollup for `t1` with count 5, min 18, max 23.5 and average
21; `garbage` and `999` are dropped, and `t2` waits for its minute.

MQTT payloads arrive as bytes, so the route converts them to text first. The `jq` step
computes the statistics on the list the `GroupedBodyAggregationStrategy` collected. To keep a
rollup per minute regardless of the number of readings, drop `completionSize` and keep
`completionInterval`. Aggregation state is in memory: rollups in progress are lost on a restart,
which is usually fine for telemetry; use `forceCompletionOnStop: true` to flush them on a clean
shutdown.

## Store and forward when the uplink is down

Gateways lose their uplink: cellular dead zones, maintenance windows, cloud outages. This route
keeps every message:

1. Telemetry from MQTT goes into a `seda` queue, so the MQTT consumer is never blocked by a slow
   uplink.
2. The uplink route posts each message to the cloud. A dead letter channel retries three times
   with exponential back-off, then writes the original message to a buffer folder.
3. A replay route polls the buffer folder every 30 seconds, posts each file, and moves it to
   `.sent` when the cloud accepts it. While the uplink is still down, the post fails and the file
   stays for the next poll.

<!-- example: edge-store-and-forward -->
```yaml
- route:
    id: telemetry-in
    from:
      uri: "paho:telemetry/#?brokerUrl={{env:MQTT_BROKER:tcp://localhost:1883}}&qos=1&clientId={{env:GATEWAY_ID:gateway-1}}-uplink"
      steps:
        - convertBodyTo:
            type: java.lang.String
        - to: "seda:uplink?size=10000&blockWhenFull=true"
- route:
    id: uplink
    errorHandler:
      deadLetterChannel:
        deadLetterUri: "file:{{env:BUFFER_DIR:buffer}}?fileName=${date:now:yyyyMMddHHmmssSSS}-${exchangeId}.msg"
        useOriginalMessage: true
        redeliveryPolicy:
          maximumRedeliveries: 3
          redeliveryDelay: "{{env:RETRY_DELAY_MS:1000}}"
          useExponentialBackOff: true
    from:
      uri: "seda:uplink?size=10000"
      steps:
        - setHeader:
            name: X-Gateway
            constant: "{{env:GATEWAY_ID:gateway-1}}"
        - enrich:
            uri: "{{env:UPLINK_URL:http://localhost:9000}}/telemetry?httpMethod=POST"
- route:
    id: replay-buffer
    from:
      uri: "file:{{env:BUFFER_DIR:buffer}}?include=.*\\.msg&sortBy=file:name&move=.sent&delay={{env:REPLAY_DELAY_MS:30000}}"
      steps:
        - convertBodyTo:
            type: java.lang.String
        - setHeader:
            name: X-Gateway
            constant: "{{env:GATEWAY_ID:gateway-1}}"
        - setHeader:
            name: X-Replayed
            constant: "true"
        - enrich:
            uri: "{{env:UPLINK_URL:http://localhost:9000}}/telemetry?httpMethod=POST"
```

The test starts with the cloud mock answering `503`: the message is tried four times and lands
in the buffer. When the mock recovers, the replay posts it (with `X-Replayed: true`) and moves
the file to `.sent`, and new messages go straight through.

Why `enrich` and not `to` for the uplink call: `enrich` sends a copy, so every retry posts the
original message. With `to`, Cameleer's `http` producer puts the error response into the message
before it fails, and a retry would post that instead (Apache Camel leaves the message unchanged).

Things to tune for production:

- Mount the buffer on persistent storage (`-v cameleer-buffer:/buffer`); the container root can
  stay read-only.
- `seda` is in memory: messages queued there when the process stops are lost. For zero loss
  across restarts, write to the buffer folder first and let the replay route be the only sender,
  at the cost of disk writes for every message.
- Order is kept per buffer folder (`sortBy=file:name`), not between live and replayed messages.
  Put a timestamp in the payload if the cloud must reorder.

## Protocol bridging

Bridges translate between what the site speaks and what the cloud accepts. The two routes
above already bridge MQTT to Kafka and MQTT to HTTP. Two more common ones follow.

### Syslog to Kafka

Routers, switches, PLCs and Linux hosts all speak syslog. The `syslog` component receives RFC
5424 and RFC 3164 messages over UDP or TCP (and TLS), parses them into a map, and sets headers
such as `CamelSyslogSeverity`, `CamelSyslogFacility` and `CamelSyslogHostname`. This route keeps
warnings and worse and sends them to Kafka as JSON.

<!-- example: edge-syslog-to-kafka -->
```yaml
- route:
    id: syslog-alerts
    from:
      uri: "syslog:udp://{{env:SYSLOG_HOST:0.0.0.0}}:{{env:SYSLOG_PORT:5514}}"
      steps:
        - filter:
            simple: "${header.CamelSyslogSeverity} in 'EMERG,ALERT,CRIT,ERR,WARNING'"
            steps:
              - setHeader:
                  name: kafka.KEY
                  simple: "${header.CamelSyslogHostname}"
              - marshal:
                  json: {}
              - to: "kafka:syslog-alerts?brokers={{env:KAFKA_BROKERS:localhost:9092}}"
```

The test sends a `local4.notice` message (dropped) and an `auth.crit` message (forwarded to the
`syslog-alerts` topic, keyed by `router-1`).

### HL7 v2 over MLLP to an HTTP API

This route needs the [health pack](../packs/health/index.md) (image `pmsudhi/cameleer:health`).

Hospital systems exchange HL7 v2 over MLLP. The `mllp` component frames and acknowledges
messages and sets `CamelMllp*` headers (message type, event, control id, sending application);
the `hl7terser` language reads fields by path (`PID-3-1` is the patient id). The acknowledgement
goes back after the route finishes, so the sender gets `AA` only once the EHR accepted the
message, and `AE` when it still failed after five retries, so it keeps the message and resends.

<!-- example: edge-hl7-mllp -->
```yaml
- route:
    id: hl7-to-ehr
    errorHandler:
      defaultErrorHandler:
        redeliveryPolicy:
          maximumRedeliveries: 5
          redeliveryDelay: "{{env:RETRY_DELAY_MS:2000}}"
    from:
      uri: "mllp://{{env:MLLP_HOST:0.0.0.0}}:{{env:MLLP_PORT:2575}}"
      steps:
        - filter:
            simple: "${header.CamelMllpEventType} == 'ADT'"
            steps:
              - setHeader:
                  name: X-Patient-Id
                  hl7terser: "PID-3-1"
              - setHeader:
                  name: X-Message-Control-Id
                  simple: "${header.CamelMllpMessageControlId}"
              - setHeader:
                  name: Content-Type
                  constant: application/hl7-v2
              - enrich:
                  uri: "{{env:EHR_URL:http://localhost:9000}}/hl7/v2?httpMethod=POST"
```

The test sends an ADT message (posted, `AA`), an ORU message (acknowledged, not posted) and an
ADT message the EHR rejects (six posts with the original message, then `AE`). For TLS between
the systems, add `sslContextParameters=#ssl` to the `mllp` URI.

## Gateway presence with retained messages and a last will

Dashboards need to know which gateways are alive without polling them. The heartbeat route
publishes a retained `online` status every 30 seconds. Its MQTT connection registers a last will
of `offline`, retained too: if the gateway loses power or network, the broker publishes it.
Anyone subscribing to `gateways/+/status` gets the current state of every gateway at once.

<!-- example: edge-gateway-heartbeat -->
```yaml
- route:
    id: gateway-heartbeat
    from:
      uri: "timer:heartbeat?period={{env:HEARTBEAT_MS:30000}}"
      steps:
        - setBody:
            constant: online
        - to: "paho:gateways/{{env:GATEWAY_ID:gateway-1}}/status?brokerUrl={{env:MQTT_BROKER:tcp://localhost:1883}}&clientId={{env:GATEWAY_ID:gateway-1}}-status&qos=1&retained=true&willTopic=gateways/{{env:GATEWAY_ID:gateway-1}}/status&willPayload=offline&willQos=1&willRetained=true"
```

The test checks that the broker holds the retained `online` status and that the gateway is
connected under its own client id. The broker sends the will only on an abnormal disconnect;
a clean stop of the route does not mark the gateway offline, so publish `offline` from an
`onCompletion` or a shutdown route if you need that too.

## MCP tools on the gateway: let an agent read sensors and change setpoints

An AI agent can work with the plant directly when the gateway serves MCP tools: a technician
asks Claude Desktop "which sensors are above 25 °C, and turn fan-1 to 22", or an agent in the
cloud adjusts setpoints overnight. The tools run on the gateway, against the local MQTT broker,
so readings never leave the site and the agent reaches only what the tools expose.
[examples/edge/mcp-gateway-tools.camel.yaml](../../examples/edge/mcp-gateway-tools.camel.yaml):

- `latest_readings` (read only) returns the last reading of every sensor, kept in the embedded
  in-memory SQL database as readings arrive over MQTT;
- `set_setpoint` publishes a setpoint to `devices/<device>/setpoint`. The schema accepts only the
  devices listed in `parameter.device.enum`, and `approval` makes any setpoint above 30 °C wait
  for a person: the call is parked with a handle, `approvalEndpoint` is told (a log here; Slack,
  e-mail or a ticket queue in practice), and an operator decides with
  `cameleer tool approve <handle>` or `cameleer tool deny <handle>`. Without `approvalEndpoint`,
  the MCP client's own user is asked instead ([approvals](mcp-server.md#approvals)).

<!-- example: edge-mcp-gateway-tools -->
```yaml
- route:
    id: create-readings-table
    from:
      uri: timer:create-readings?repeatCount=1&delay=0
      steps:
        - to: "sql:create table if not exists readings (sensor text, celsius double precision, at text)?dataSource=jdbc:rustsql:mem:gateway"
- route:
    id: remember-latest-reading
    from:
      uri: "paho:sensors/+/temperature?brokerUrl={{env:MQTT_BROKER:tcp://localhost:1883}}&qos=1&clientId={{env:GATEWAY_ID:gateway-1}}-readings"
      steps:
        - convertBodyTo:
            type: java.lang.String
        - filter:
            simple: "${body} regex '-?[0-9]+(\\.[0-9]+)?'"
            steps:
              - setHeader:
                  name: sensor
                  simple: "${header.CamelMqttTopic.split('/')[1]}"
              - setHeader:
                  name: celsius
                  simple: "${bodyAs(Double)}"
              - setHeader:
                  name: at
                  simple: "${date:now:yyyy-MM-dd'T'HH:mm:ss'Z'}"
              - to: "sql:delete from readings where sensor = :#sensor?dataSource=jdbc:rustsql:mem:gateway"
              - to: "sql:insert into readings (sensor, celsius, at) values (:#sensor, :#celsius, :#at)?dataSource=jdbc:rustsql:mem:gateway"
- route:
    id: latest-readings
    from:
      uri: ai-tool:latest_readings
      parameters:
        description: The latest temperature reading of every sensor on this gateway
        tags: plant
        readOnlyHint: true
      steps:
        - to: "sql:select sensor, celsius, at from readings order by sensor?dataSource=jdbc:rustsql:mem:gateway"
        - marshal:
            json: {}
- route:
    id: set-setpoint
    from:
      uri: ai-tool:set_setpoint
      parameters:
        description: Set a device's temperature setpoint in °C; above 30 °C a person approves first
        tags: plant
        parameter.device: string
        parameter.device.description: The device id
        parameter.device.required: true
        parameter.device.enum: fan-1,fan-2,heater-1
        parameter.celsius: number
        parameter.celsius.description: The new setpoint in °C
        parameter.celsius.required: true
        approval: "${header.celsius} > 30"
        approvalEndpoint: log:setpoint-approvals
      steps:
        - setBody:
            simple: '{"setpoint": ${header.celsius}, "gateway": "{{env:GATEWAY_ID:gateway-1}}"}'
        - toD: "paho:devices/${header.device}/setpoint?brokerUrl={{env:MQTT_BROKER:tcp://localhost:1883}}&qos=1&clientId={{env:GATEWAY_ID:gateway-1}}-setpoints"
        - setBody:
            simple: '{"device": "${header.device}", "setpoint": ${header.celsius}, "sent": true}'
```

Serve the tools over HTTP on the gateway (an agent elsewhere connects to
`http://<gateway>:8080/mcp`; add API-key or JWT checks from the
[MCP server guide](mcp-server.md#authentication) before you open the port beyond the plant
network):

```bash
docker run -d --name gw --network host --read-only --tmpfs /tmp -e CAMEL_LOCAL_STATUS=true   -e MQTT_BROKER -e GATEWAY_ID -v "$PWD:/routes:ro" pmsudhi/cameleer   run /routes/mcp-gateway-tools.camel.yaml --mcp
```

`CAMEL_LOCAL_STATUS=true` (off by default in the image) and a writable `/tmp` let the operator
commands reach the running process:

```bash
docker exec gw cameleer tool approvals            # the calls that wait, with their handles
docker exec gw cameleer tool approve <handle>     # runs the call: the setpoint is published
docker exec gw cameleer tool deny <handle>        # runs nothing
```

The test publishes readings over the embedded MQTT broker (one malformed), reads them back
through `tools/call`, sets `fan-1` to 22 °C (published to `devices/fan-1/setpoint`), and checks
that 45 °C for `heater-1` waits for approval and that an unknown device is refused, with
nothing published for either. The same steps, plus `cameleer tool approve` publishing the parked
setpoint, were run in the container against Mosquitto 2.

## Small footprint deployment

The Docker image `pmsudhi/cameleer` is one native binary on a distroless base, for `linux/amd64`
and `linux/arm64`, about 20 MB to download (12.5 MB built with `FEATURES=core`). A running route uses about 10 MB of memory (8 MB at startup in the
[benchmarks](../BENCHMARKS.md)) and starts its routes in milliseconds. Nothing is written to
the filesystem unless a route writes files, so the container runs with a read-only root:

```sh
docker run -d --name edge-routes --restart unless-stopped \
  --read-only --memory 64m --cpus 0.5 \
  --network host \
  -e MQTT_BROKER=tcp://localhost:1883 -e KAFKA_BROKERS -e GATEWAY_ID=plant-7 \
  -v "$PWD/routes:/routes:ro" \
  pmsudhi/cameleer run /routes/mqtt-sensor-rollup.camel.yaml
```

- **arm64 boards:** the same image runs on 64-bit Raspberry Pi OS (Pi 3, 4 and 5), NVIDIA
  Jetson, AWS Graviton and other arm64 Linux devices; Docker picks the right platform. There is
  no published 32-bit ARM image.
- **No container runtime:** `cargo install cameleer-main` builds the `cameleer` binary on the
  device (or cross-compile it: Cameleer has no dependencies outside the Rust standard library),
  then run it under systemd, for example `cameleer run /etc/cameleer/rollup.camel.yaml`.
- **Several route files:** pass several files to `run` (or none, to load every route file in
  the working directory); they share one process.
- **Health:** add `--prop camel.server.enabled=true --prop camel.server.healthCheckEnabled=true`
  for `/q/health/live` and `/q/health/ready` on port 8080. See the
  [deployment guide](../DEPLOYMENT.md).

## Offline-first patterns

- **Decide locally.** Filters, thresholds and aggregation run on the gateway, so alarms fire
  without the cloud. A `choice` that writes to a local actuator topic (`paho:...`) keeps working
  when the uplink is gone.
- **Buffer, then forward.** A dead letter channel to `file:` plus a replay route is a durable
  outbox in a few lines of YAML. Use `seda` with `blockWhenFull=true` in front of slow calls.
- **Make delivery idempotent.** Replays and MQTT QoS 1 can deliver a message twice. Give each
  message an id at the edge and use an `idempotentConsumer` at the receiving side.
- **Report presence with the broker.** Retained status messages and a last will tell the cloud
  what is offline without a polling loop.
- **Thin out telemetry.** `sample` and `throttle` keep the uplink bill under control; aggregate
  before sending.

## Common questions

### Can Cameleer run on a Raspberry Pi?

Yes, on a 64-bit OS: the `pmsudhi/cameleer` image is published for `linux/arm64`, and a route
uses about 10 MB of memory. 32-bit ARM images are not published.

### How do I bridge MQTT to Kafka?

Use a route from `paho:topic/#?brokerUrl=tcp://...` to `kafka:topic?brokers=...`, converting the
payload with `convertBodyTo` and setting `kafka.KEY` from the MQTT topic if you need per-device
ordering. The [sensor rollup example](#filter-and-aggregate-mqtt-sensor-readings) shows it with
filtering and aggregation.

### What happens to messages when the internet connection drops?

Without precautions, a failed call fails the exchange. With a dead letter channel to a local
`file:` folder and a replay route, as in [store and forward](#store-and-forward-when-the-uplink-is-down),
messages wait on disk and are sent when the uplink returns.

### Does Cameleer support MQTT 5 and TLS?

Yes. `paho` speaks MQTT 3.1.1 and `paho-mqtt5` MQTT 5.0, both with QoS 0, 1 and 2, retained
messages, wills, persistent sessions, automatic reconnect and `ssl://` broker URLs. WebSocket
transports and shared subscriptions are not supported.

### Which industrial and healthcare protocols are supported?

MQTT, syslog, TCP and UDP (`netty`), HTTP, WebSocket, FTP and SFTP, Kafka, AMQP and STOMP in the
default build; HL7 v2 over MLLP, FHIR and DICOM in the [health pack](../packs/health/index.md)
(image `pmsudhi/cameleer:health`). There are no OPC UA, Modbus or PLC4X components; reach those through an MQTT or HTTP
gateway.

## Related

- [EIPs](eips.md): [aggregate](eips/split-aggregate.md#aggregator),
  [sample and throttle](eips/flow-control.md)
- [Error handling](error-handling.md), [deployment guide](../DEPLOYMENT.md),
  [benchmarks](../BENCHMARKS.md)
- Components: [paho](../components/paho.md), [kafka](../components/kafka.md),
  [syslog](../components/syslog.md), [mllp](../packs/health/components/mllp.md), [file](../components/file.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
