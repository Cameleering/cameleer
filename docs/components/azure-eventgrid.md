# Azure Event Grid component

> The `azure-eventgrid` component publishes events to an Azure Event Grid topic, as CloudEvents or in the Event Grid schema.

**Scheme:** `azure-eventgrid:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [Azure Event Grid](https://camel.apache.org/components/next/azure-eventgrid-component.html)

## When to use it

- Publish business events (order placed, device offline) that Azure Functions, Logic Apps or
  webhooks subscribe to.
- Bridge events from MQTT, Kafka or on-premises systems into Azure's event routing.

## URI format

```text
azure-eventgrid:topicEndpoint[?options]
```

The path is the topic endpoint URL, for example
`https://mytopic.westeurope-1.eventgrid.azure.net/api/events`.

## Example

Publish an `OrderPlaced` CloudEvent:

<!-- example: azure-eventgrid-publish-order -->
```yaml
- route:
    id: publish-order-event
    from:
      uri: "timer:order-event?repeatCount=1"
      steps:
        - setBody:
            constant: '{"orderId":"o-1001","total":99.5}'
        - unmarshal:
            json: {}
        - setHeader:
            name: CamelAzureEventGridEventType
            constant: Shop.OrderPlaced
        - setHeader:
            name: CamelAzureEventGridSubject
            constant: orders/o-1001
        - to: "azure-eventgrid:{{env:EVENTGRID_TOPIC_ENDPOINT:https://mytopic.westeurope-1.eventgrid.azure.net/api/events}}?accessKey=RAW({{env:EVENTGRID_ACCESS_KEY:}})"
        - log: "published event ${header.CamelAzureEventGridEventId}"
```

The body is the event `data`; a list body publishes one event per element.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:order-event?repeatCount=1")
        .set_body(constant(r#"{"orderId":"o-1001","total":99.5}"#))
        .unmarshal("json")
        .set_header("CamelAzureEventGridEventType", constant("Shop.OrderPlaced"))
        .set_header("CamelAzureEventGridSubject", constant("orders/o-1001"))
        .to("azure-eventgrid:{{env:EVENTGRID_TOPIC_ENDPOINT:https://mytopic.westeurope-1.eventgrid.azure.net/api/events}}?accessKey=RAW({{env:EVENTGRID_ACCESS_KEY:}})")
        .log("published event ${header.CamelAzureEventGridEventId}");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `eventSchema` | `CloudEvent` | `CloudEvent` (CloudEvents 1.0 JSON batch) or `EventGrid` (Event Grid schema). |
| `accessKey` | | Topic access key (`aeg-sas-key` header). |
| `credentialType` | `ACCESS_KEY` with a key | `ACCESS_KEY`, `AZURE_SAS` (an `aeg-sas-token`: `sharedAccessSignature`, or one signed with `accessKey`), or Entra ID (`tenantId`, `clientId`, `clientSecret`, `authorityHost`; `AZURE_IDENTITY` reads `AZURE_*` variables). |
| `sharedAccessSignature` | | Pre-built SAS token. |
| `sasTokenTtl` | `3600` | Lifetime (seconds) of tokens signed with `accessKey`. |
| `topicEndpoint` | | Alternative to the path. |
| `uriEndpointOverride`, `proxyHost`, `proxyPort`, `trustAllCertificates` | | Connection settings. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAzureEventGridEventType` | in | Event type (default `org.apache.camel.event`). |
| `CamelAzureEventGridSubject` | in | Subject. |
| `CamelAzureEventGridSource` | in | CloudEvents source (default: the topic endpoint). |
| `CamelAzureEventGridEventId` | in/out | Event id (random UUID by default); the ids sent (a list for list bodies). |
| `CamelAzureEventGridDataVersion` | in | Event Grid schema data version (default `1.0`). |
| `CamelAzureEventGridDataContentType` | in | CloudEvents data content type (default `application/json`). |

## Run it with Docker

```sh
docker run --rm -e EVENTGRID_TOPIC_ENDPOINT -e EVENTGRID_ACCESS_KEY \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/events.camel.yaml
```

## Common questions

### How do I receive Event Grid events in a route?

Subscribe an Event Hub, a Storage queue or a Service Bus queue to the topic and consume it with
[azure-eventhubs](azure-eventhubs.md), [azure-storage-queue](azure-storage-queue.md) or
[azure-servicebus](azure-servicebus.md).

### Which schema should I use?

`CloudEvent` for new topics; `EventGrid` when the topic was created with the Event Grid schema.

## Related

- [Azure Event Hubs](azure-eventhubs.md), [Azure Service Bus](azure-servicebus.md)
- [AWS EventBridge](aws2-eventbridge.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel Azure Event Grid](https://camel.apache.org/components/next/azure-eventgrid-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
