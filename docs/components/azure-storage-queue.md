# Azure Storage Queue component

> The `azure-storage-queue` component sends messages to and receives messages from Azure Queue Storage.

**Scheme:** `azure-storage-queue:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [Azure Storage Queue Service](https://camel.apache.org/components/next/azure-storage-queue-component.html)

## When to use it

- A simple, cheap work queue between services on Azure.
- Hand jobs from Azure Functions or Logic Apps to a Cameleer worker, and back.
- Buffer messages from edge devices with an account key or a SAS token.

For topics, sessions and transactions, use [Azure Service Bus](azure-servicebus.md).

## URI format

```text
azure-storage-queue://accountName/queueName[?options]
```

## Example

Send a job (the queue is created when it does not exist):

<!-- example: azure-storage-queue-send-job -->
```yaml
- route:
    id: send-job
    from:
      uri: "timer:new-job?repeatCount=1"
      steps:
        - setBody:
            constant: '{"job":"resize","image":"cat.jpg"}'
        - to: "azure-storage-queue://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/jobs?operation=sendMessage&createQueue=true&accessKey=RAW({{env:AZURE_STORAGE_KEY:}})"
        - log: "queued message ${header.CamelAzureStorageQueueMessageId}"
```

Work through the queue. Each message is deleted after it was processed successfully:

<!-- example: azure-storage-queue-worker -->
```yaml
- route:
    id: job-worker
    from:
      uri: "azure-storage-queue://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/jobs?maxMessages=10&visibilityTimeout=PT60S&accessKey=RAW({{env:AZURE_STORAGE_KEY:}})"
      steps:
        - log: "job ${header.CamelAzureStorageQueueMessageId} (dequeued ${header.CamelAzureStorageQueueDequeueCount}x): ${body}"
        - to: "log:jobs"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("azure-storage-queue://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/jobs?maxMessages=10&visibilityTimeout=PT60S&accessKey=RAW({{env:AZURE_STORAGE_KEY:}})")
        .log("job ${header.CamelAzureStorageQueueMessageId} (dequeued ${header.CamelAzureStorageQueueDequeueCount}x): ${body}")
        .to("log:jobs");
})?;
```

## Options

Common Azure options (credentials, endpoint override, proxy) are described in
[Cloud credentials](../guides/cloud-credentials.md#azure-options).

| option | default | description |
|---|---|---|
| `operation` | `sendMessage` | `listQueues`, `createQueue`, `deleteQueue`, `clearQueue`, `sendMessage`, `receiveMessages`, `peekMessages`, `deleteMessage`, `updateMessage`. |
| `createQueue` | `false` | `sendMessage` creates the queue first. |
| `maxMessages` | `1` | Messages per poll (consumer) or per `receiveMessages` / `peekMessages`. |
| `visibilityTimeout` | | Visibility timeout (ISO-8601 duration such as `PT60S`, or seconds). |
| `timeToLive` | | Time to live of sent messages. |

The consumer also accepts the scheduled poll options. Message text is sent as is (not base64),
like the Azure SDK v12.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAzureStorageQueueOperation` | in | Overrides `operation`. |
| `CamelAzureStorageQueueName` | in | Overrides the queue. |
| `CamelAzureStorageQueueMessageId`, `CamelAzureStorageQueuePopReceipt` | in/out | Message; input of `deleteMessage` and `updateMessage`. |
| `CamelAzureStorageQueueVisibilityTimeout`, `CamelAzureStorageQueueTimeToLive`, `CamelAzureStorageQueueMaxMessages` | in | Override the options. |
| `CamelAzureStorageQueueInsertionTime`, `CamelAzureStorageQueueExpirationTime`, `CamelAzureStorageQueueTimeNextVisible` | out | Message times. |
| `CamelAzureStorageQueueDequeueCount` | out | How often the message was received. |

## Run it with Docker

```sh
docker run --rm -e AZURE_STORAGE_ACCOUNT -e AZURE_STORAGE_KEY \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/jobs.camel.yaml
```

## Common questions

### What happens when a job fails?

The message is not deleted. It becomes visible again after the visibility timeout and
`CamelAzureStorageQueueDequeueCount` grows, so the route can move poison messages away after a few tries.

### Are messages base64 encoded?

No. Text is sent as is. Azure Functions queue triggers expect base64 by default; configure the
function's `messageEncoding` to `none`, or encode the body with the `base64` data format.

## Related

- [Azure Service Bus](azure-servicebus.md), [Azure Storage Blob](azure-storage-blob.md)
- [AWS SQS](aws2-sqs.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel Azure Storage Queue](https://camel.apache.org/components/next/azure-storage-queue-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
