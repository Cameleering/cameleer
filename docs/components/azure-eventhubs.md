# Azure Event Hubs component

> The `azure-eventhubs` component sends events to and consumes events from Azure Event Hubs over AMQP 1.0, with partitions, checkpoints and load balancing between consumers.

**Scheme:** `azure-eventhubs:` · **Crate:** `cameleer-amqp10` · **Cargo feature:** [`amqp10`](../guides/build-features.md) · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [Azure Event Hubs](https://camel.apache.org/components/next/azure-eventhubs-component.html)

## When to use it

- Stream telemetry from devices and gateways into Azure.
- Consume high-volume event streams partition by partition, resuming from checkpoints.
- Run several consumer instances that share the partitions (like the Azure SDK's event processor).
- Read Event Grid events (for example Key Vault or Storage events) that are forwarded to an event hub.

## URI format

```text
azure-eventhubs:namespace/eventHubName[?options]
azure-eventhubs:?connectionString=...;EntityPath=eventHubName
```

## Example

Send device telemetry; events with the same partition key go to the same partition:

<!-- example: azure-eventhubs-send-telemetry -->
```yaml
- route:
    id: send-telemetry
    from:
      uri: "timer:telemetry?repeatCount=1"
      steps:
        - setBody:
            constant: '{"device":"edge-7","temperature":21.5}'
        - to: "azure-eventhubs:?connectionString=RAW({{env:EVENTHUBS_CONNECTION_STRING:Endpoint=sb://my-namespace.servicebus.windows.net/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=change-me;EntityPath=telemetry}})&partitionKey=edge-7"
```

Consume the event hub from the oldest retained event:

<!-- example: azure-eventhubs-consume-telemetry -->
```yaml
- route:
    id: read-telemetry
    from:
      uri: "azure-eventhubs:?connectionString=RAW({{env:EVENTHUBS_CONNECTION_STRING:Endpoint=sb://my-namespace.servicebus.windows.net/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=change-me;EntityPath=telemetry}})&initialPosition=earliest"
      steps:
        - convertBodyTo: String
        - log: "partition ${header.CamelAzureEventHubsPartitionId} offset ${header.CamelAzureEventHubsOffset}: ${body}"
        - to: "log:telemetry"
```

The event body arrives as bytes. Checkpoints are kept in memory unless you configure a checkpoint
store (see the options).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("azure-eventhubs:?connectionString=RAW({{env:EVENTHUBS_CONNECTION_STRING:Endpoint=sb://my-namespace.servicebus.windows.net/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=change-me;EntityPath=telemetry}})&initialPosition=earliest")
        .convert_body_to("String")
        .log("partition ${header.CamelAzureEventHubsPartitionId} offset ${header.CamelAzureEventHubsOffset}: ${body}")
        .to("log:telemetry");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `connectionString` | | Connection string; `EntityPath` names the event hub when the uri has none. |
| `sharedAccessName`, `sharedAccessKey`, `fullyQualifiedNamespace` | | Instead of a connection string (with `namespace/eventHubName` in the uri). |
| `credentialType` | `CONNECTION_STRING` | `CONNECTION_STRING`, `AZURE_IDENTITY` or `TOKEN_CREDENTIAL` (see [Azure Service Bus](azure-servicebus.md)). |
| `amqpTransportType` | `AMQP` | `AMQP` or `AMQP_WEB_SOCKETS`. |
| `consumerGroupName` | `$Default` | Consumer group. |
| `partitionId` | | Producer: send to this partition. Consumer: read only this partition. |
| `partitionKey` | | Producer: partition key. |
| `prefetchCount` | `500` | Events to prefetch per partition. |
| `initialPosition` | `latest` | Cameleer: where to start without a checkpoint, `latest` or `earliest`. |
| `eventPosition` | | Start position per partition, for example `0=earliest,1=offset:42,2=sequenceNumber:7:inclusive` (or `#bean`). |
| `checkpointStore` | in memory | `#bean` of a `CheckpointStore`. |
| `blobAccountName`, `blobContainerName`, `blobAccessKey`, `blobEndpoint` | | Checkpoints and ownership in Azure Blob Storage (layout of the Azure SDK's blob checkpoint store). |
| `checkpointBatchSize`, `checkpointBatchTimeout` | `500`, `5000` | Checkpoint every N events or T ms (and when stopping). |
| `loadBalancing` | `true` | Share partitions with other instances using the same checkpoint store. |
| `loadBalancingUpdateInterval`, `partitionOwnershipExpirationInterval` | 10 s, 60 s | Load balancing timings (Cameleer). |
| `ownerId`, `ownerLevel` | random, | Owner id of ownership records and the receiver epoch. |
| `sasTokenTtl`, `idleTimeout`, `requestTimeout`, `recoveryInterval` | | Connection timings. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAzureEventHubsPartitionId` | in/out | Partition (input: target partition). |
| `CamelAzureEventHubsPartitionKey` | in/out | Partition key. |
| `CamelAzureEventHubsOffset`, `CamelAzureEventHubsSequenceNumber` | out | Position of the event. |
| `CamelAzureEventHubsEnqueuedTime` | out | Enqueued time (epoch millis), also `CamelMessageTimestamp`. |

Other headers are sent as application properties, and received application properties become headers.

## Run it with Docker

```sh
docker run --rm -e EVENTHUBS_CONNECTION_STRING \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/telemetry.camel.yaml
```

## Common questions

### How do I resume after a restart?

Configure a persistent checkpoint store, for example Azure Blob Storage with `blobAccountName`,
`blobContainerName` and `blobAccessKey`; the consumer continues after the last checkpoint.

### How do I scale out consumers?

Start more instances with the same consumer group and the same blob checkpoint store: partitions are
balanced between them, and a new owner takes over the partitions of a stopped instance.

### Can I use Kafka clients instead?

Event Hubs also has a Kafka endpoint; the `kafka` component can use it with SASL `PLAIN` and the
connection string as password. This component uses the native AMQP protocol.

## Related

- [Azure Service Bus](azure-servicebus.md), [Azure Schema Registry](azure-schema-registry.md), [Azure Key Vault](azure-key-vault.md)
- [AWS Kinesis](aws2-kinesis.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel Azure Event Hubs](https://camel.apache.org/components/next/azure-eventhubs-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
