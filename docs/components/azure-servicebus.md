# Azure Service Bus component

> The `azure-servicebus` component sends messages to and receives messages from Azure Service Bus queues and topic subscriptions over AMQP 1.0, with peek-lock, sessions, scheduling, dead-lettering and transactions.

**Scheme:** `azure-servicebus:` · **Crate:** `cameleer-amqp10` · **Cargo feature:** [`amqp10`](../guides/build-features.md) · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [Azure ServiceBus](https://camel.apache.org/components/next/azure-servicebus-component.html)

## When to use it

- Reliable enterprise messaging between services on Azure (orders, payments, commands).
- Publish to a topic and let several subscriptions process the same message.
- Ordered processing per customer or device with sessions.
- Schedule messages for later delivery, or defer them and fetch them by sequence number.

## URI format

```text
azure-servicebus:queueOrTopicName[?options]
```

Authenticate with a `connectionString`, with `fullyQualifiedNamespace` + `sharedAccessName` +
`sharedAccessKey`, or with Microsoft Entra ID (`credentialType=AZURE_IDENTITY`).

## Example

Send an order to a queue:

<!-- example: azure-servicebus-send-order -->
```yaml
- route:
    id: send-order
    from:
      uri: "timer:new-order?repeatCount=1"
      steps:
        - setBody:
            constant: '{"orderId":"o-1001","item":"camel saddle"}'
        - setHeader:
            name: CamelAzureServiceBusContentType
            constant: application/json
        - to: "azure-servicebus:orders?connectionString=RAW({{env:SERVICEBUS_CONNECTION_STRING:Endpoint=sb://my-namespace.servicebus.windows.net/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=change-me}})"
```

Process the queue with peek-lock. A message is completed when the route succeeds, and abandoned
(redelivered) when it fails:

<!-- example: azure-servicebus-process-orders -->
```yaml
- route:
    id: process-orders
    from:
      uri: "azure-servicebus:orders?connectionString=RAW({{env:SERVICEBUS_CONNECTION_STRING:Endpoint=sb://my-namespace.servicebus.windows.net/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=change-me}})&maxConcurrentCalls=4"
      steps:
        - log: "order ${header.CamelAzureServiceBusMessageId} (delivery ${header.CamelAzureServiceBusDeliveryCount}): ${body}"
        - to: "log:orders"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("azure-servicebus:orders?connectionString=RAW({{env:SERVICEBUS_CONNECTION_STRING:Endpoint=sb://my-namespace.servicebus.windows.net/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=change-me}})&maxConcurrentCalls=4")
        .log("order ${header.CamelAzureServiceBusMessageId} (delivery ${header.CamelAzureServiceBusDeliveryCount}): ${body}")
        .to("log:orders");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `connectionString` | | Namespace connection string (`Endpoint=sb://...;SharedAccessKeyName=...;SharedAccessKey=...`; `UseDevelopmentEmulator=true` for the emulator). |
| `fullyQualifiedNamespace`, `sharedAccessName`, `sharedAccessKey` | | Instead of a connection string. |
| `credentialType` | `CONNECTION_STRING` | `CONNECTION_STRING`, `AZURE_IDENTITY` (environment, client secret or managed identity; options `tenantId`, `clientId`, `clientSecret`, `authorityHost`, `managedIdentityEndpoint`, `managedIdentityClientId`) or `TOKEN_CREDENTIAL` (`tokenCredential=#bean`). |
| `amqpTransportType` | `AMQP` | `AMQP` (port 5671) or `AMQP_WEB_SOCKETS` (port 443). |
| `serviceBusType` | `queue` | `queue` or `topic`. |
| `subscriptionName` | | Subscription of a topic consumer. |
| `receiveMode` | `PEEK_LOCK` | `PEEK_LOCK` or `RECEIVE_AND_DELETE`. |
| `consumerOperation` | `receiveMessages` | `receiveMessages` or `peekMessages` (without locking; `peekNumMaxMessages` per poll, default 10). |
| `producerOperation` | `sendMessages` | `sendMessages`, `scheduleMessages`, and the Cameleer operations `receiveDeferredMessages`, `getSessionState`, `setSessionState`. |
| `scheduledEnqueueTime` | | Enqueue time of `scheduleMessages` (epoch millis or ISO-8601). |
| `prefetchCount` | `0` | Messages to prefetch. |
| `maxConcurrentCalls` | `1` | Parallel receivers. |
| `subQueue` | `NONE` | `DEAD_LETTER_QUEUE` reads the dead-letter queue. |
| `enableDeadLettering` | `false` | Dead-letter failed messages instead of abandoning them. |
| `disableAutoComplete` | `false` | Do not settle automatically (use `CamelAzureServiceBusDisposition`). |
| `maxAutoLockRenewDuration` | `PT5M` | Renew message and session locks while processing (0 disables). |
| `sessionEnabled`, `sessionId`, `sessionIdleTimeout` | `false`, , 1 s | Sessions: accept any or one session; release it after the idle timeout (Cameleer). |
| `transacted` | `false` | Cameleer: send the messages of an exchange in one AMQP transaction. |
| `binary` | `false` | Send the body as binary. |
| `sasTokenTtl`, `idleTimeout`, `requestTimeout`, `recoveryInterval`, `delay` | | Connection and polling timings. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAzureServiceBusMessageId`, `CamelAzureServiceBusCorrelationId`, `CamelAzureServiceBusContentType`, `CamelAzureServiceBusSubject`, `CamelAzureServiceBusTo`, `CamelAzureServiceBusReplyTo`, `CamelAzureServiceBusSessionId`, `CamelAzureServiceBusPartitionKey`, `CamelAzureServiceBusTimeToLive` | in/out | Message properties. |
| `CamelAzureServiceBusApplicationProperties` | in/out | Application properties (map). |
| `CamelAzureServiceBusReplyToSessionId` | out | Reply-to session. |
| `CamelAzureServiceBusEnqueuedTime`, `CamelAzureServiceBusLockedUntil`, `CamelAzureServiceBusExpiresAt` | out | Times (epoch millis). |
| `CamelAzureServiceBusSessionState` | out | `getSessionState`: the session state (also the body). |
| `CamelAzureServiceBusSequenceNumber` | in/out | Sequence number (set by `scheduleMessages`, input of `receiveDeferredMessages`). |
| `CamelAzureServiceBusDeliveryCount`, `CamelAzureServiceBusLockToken` | out | Delivery count and lock token. |
| `CamelAzureServiceBusDeadLetterReason`, `CamelAzureServiceBusDeadLetterErrorDescription`, `CamelAzureServiceBusDeadLetterSource` | out | Dead-letter details. |
| `CamelAzureServiceBusScheduledEnqueueTime`, `CamelAzureServiceBusProducerOperation` | in | Producer overrides. |
| `CamelAzureServiceBusDisposition` | in | Cameleer: settle explicitly with `complete`, `abandon`, `deadLetter` or `defer`. |

## Run it with Docker

```sh
docker run --rm -e SERVICEBUS_CONNECTION_STRING \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml
```

## Common questions

### How do I consume a topic subscription?

`azure-servicebus:events?serviceBusType=topic&subscriptionName=billing&connectionString=...`.

### How do I read the dead-letter queue?

Add `subQueue=DEAD_LETTER_QUEUE`; the dead-letter reason and description are in headers.

### Does it work with the Service Bus emulator?

Yes: a connection string with `UseDevelopmentEmulator=true` uses plain AMQP to the emulator.

## Related

- [Azure Event Hubs](azure-eventhubs.md), [Azure Storage Queue](azure-storage-queue.md)
- [AWS SQS](aws2-sqs.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel Azure Service Bus](https://camel.apache.org/components/next/azure-servicebus-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
