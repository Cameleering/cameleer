# AWS SQS component

> The `aws2-sqs` component sends messages to and receives messages from Amazon SQS queues, including FIFO queues.

**Scheme:** `aws2-sqs:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [AWS Simple Queue Service (SQS)](https://camel.apache.org/components/next/aws2-sqs-component.html)

## When to use it

- Decouple services: one route sends work to a queue, workers on other machines consume it.
- Process S3, SNS or EventBridge notifications that AWS delivers to a queue.
- Feed serverless pipelines: a Lambda or Step Functions workflow writes to SQS, a Cameleer route on
  an edge device or a container reads from it.
- Give an AI agent a durable task queue: the agent enqueues jobs, a route executes them and
  deletes each message only after it was processed.

## URI format

```text
aws2-sqs://queueNameOrArn[?options]
```

The path is the queue name (or the queue ARN; the last `:` segment is used). The queue URL is
looked up with `GetQueueUrl`, or set it directly with `queueUrl`.

## Example

Send an order to a queue (user headers become SQS message attributes):

<!-- example: aws2-sqs-send-order -->
```yaml
- route:
    id: send-order
    from:
      uri: "timer:new-order?repeatCount=1"
      steps:
        - setBody:
            constant: '{"orderId":"o-1001","item":"camel saddle","qty":2}'
        - setHeader:
            name: orderType
            constant: express
        - to: "aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true"
        - log: "sent order ${header.CamelAwsSqsMessageId}"
```

Consume the queue. Each message becomes one exchange; it is deleted from the queue only when the
route finishes without an exception, otherwise it becomes visible again after the visibility timeout:

<!-- example: aws2-sqs-consume-orders -->
```yaml
- route:
    id: process-orders
    from:
      uri: "aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}&maxMessagesPerPoll=10&visibilityTimeout=30"
      steps:
        - log: "order ${header.CamelAwsSqsMessageId} (${header.orderType}): ${body}"
        - to: "log:orders"
```

Credentials are not in the uri: they come from the default AWS chain (environment variables,
`~/.aws/credentials`, web identity, ECS or EC2 instance roles). See
[Cloud credentials](../guides/cloud-credentials.md).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}&maxMessagesPerPoll=10&visibilityTimeout=30")
        .log("order ${header.CamelAwsSqsMessageId} (${header.orderType}): ${body}")
        .to("log:orders");
})?;
```

## Options

Common AWS options (`region`, `accessKey`, `secretKey`, `sessionToken`, `profileCredentialsName`,
`useDefaultCredentialsProvider`, `useProfileCredentialsProvider`, `uriEndpointOverride`,
`overrideEndpoint`, `proxyHost`, `proxyPort`, `proxyProtocol`, `trustAllCertificates`) are described in
[Cloud credentials](../guides/cloud-credentials.md).

| option | default | description |
|---|---|---|
| `operation` | `sendMessage` | Producer operation: `sendMessage`, `sendBatchMessage`, `deleteMessage`, `listQueues`, `purgeQueue`, `deleteQueue`, `receiveMessage`, and the Cameleer operations `changeMessageVisibility`, `deleteMessageBatch`, `getQueueAttributes`, `setQueueAttributes`. The `CamelAwsSqsOperation` header overrides it. |
| `queueUrl` | | Use this queue URL instead of looking it up. |
| `autoCreateQueue` | `false` | Create the queue when it does not exist. |
| `queueOwnerAWSAccountId` | | Account that owns the queue (cross-account access). |
| `maxMessagesPerPoll` | `1` | Messages per poll. Above 10, several `ReceiveMessage` calls are made. |
| `waitTimeSeconds` | | Long polling wait of `ReceiveMessage` (seconds). |
| `visibilityTimeout` | | Visibility timeout of received messages (seconds). |
| `extendMessageVisibility` | `false` | Keep extending the visibility of a message while it is processed (needs `visibilityTimeout`). |
| `deleteAfterRead` | `true` | Delete each message after successful processing. |
| `deleteIfFiltered` | `true` | Also delete messages whose exchange was filtered out. |
| `messageAttributeNames` | `All` | Message attributes to receive (comma separated). |
| `attributeNames` | `All` | System attributes to receive (comma separated). |
| `concurrentConsumers` | `1` | Parallel receive and process threads. |
| `concurrentRequestLimit` | `50` | Maximum parallel `ReceiveMessage` calls when `maxMessagesPerPoll` is above 10. |
| `sortAttributeName` | | Sort each received batch by this attribute. |
| `delaySeconds` | | Delay of sent messages (seconds). With `delayQueue=true` it becomes the queue's `DelaySeconds` attribute instead. |
| `delayQueue` | `false` | See `delaySeconds`. |
| `messageGroupIdStrategy` | | FIFO queues: `useConstant`, `useExchangeId` or `usePropertyValue` (property `CamelAwsSqsMessageGroupId`). |
| `messageDeduplicationIdStrategy` | `useExchangeId` | FIFO queues: `useExchangeId` or `useContentBasedDeduplication`. |
| `batchSeparator` | `,` | Separator used to split a string body for `sendBatchMessage`. |
| `defaultVisibilityTimeout`, `messageRetentionPeriod`, `maximumMessageSize`, `receiveMessageWaitTimeSeconds`, `policy`, `redrivePolicy` | | Queue attributes applied when the queue is created, or with `SetQueueAttributes` when the endpoint starts. |
| `serverSideEncryptionEnabled`, `kmsMasterKeyId`, `kmsDataKeyReusePeriodSeconds` | `false` | Server-side encryption of the queue. |
| `protocol` | `auto` | Cameleer: `json` (AWS JSON 1.0), `query` (legacy Query protocol, for older SQS-compatible servers) or `auto` (JSON first, Query when the server rejects JSON). |

The consumer also accepts the scheduled poll options (`delay` default 500 ms, `initialDelay`,
`greedy`, `sendEmptyMessageWhenIdle`, `backoffMultiplier`, `scheduler.cron`, ...).

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsSqsMessageId` | out | Id of the received or sent message. |
| `CamelAwsSqsReceiptHandle` | in/out | Receipt handle (consumer); input of `deleteMessage` and `changeMessageVisibility`. |
| `CamelAwsSqsMD5OfBody` | out | MD5 of the message body. |
| `CamelAwsSqsAttributes` | out | System attributes (map). |
| `CamelAwsSqsMessageAttributes` | out | Message attributes (map); each attribute is also set as its own header. |
| `CamelAwsSqsDelaySeconds` | in | Per-message delay. |
| `CamelAwsSqsOperation` | in | Overrides `operation`. |
| `CamelAwsSqsPrefix` | in | Queue name prefix of `listQueues`. |
| `CamelAwsSqsMaxNumberOfMessages` | in | Messages to receive with `receiveMessage`. |
| `CamelAwsSqsVisibilityTimeout` | in | New timeout of `changeMessageVisibility`. |
| `CamelAwsSqsMessageGroupId` | in (property) | Group id with `messageGroupIdStrategy=usePropertyValue`. |

On send, other message headers become SQS message attributes.

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml
```

## Common questions

### How do I read from a FIFO queue?

Use the queue name with its `.fifo` suffix, for example `aws2-sqs://orders.fifo`. When sending, set
`messageGroupIdStrategy` (for example `useConstant`) and, if the queue has content-based
deduplication, `messageDeduplicationIdStrategy=useContentBasedDeduplication`.

### What happens when processing fails?

The message is not deleted. It becomes visible again after the visibility timeout and is received
again; configure a `redrivePolicy` on the queue to move it to a dead-letter queue after a number of
attempts.

### How do I test against LocalStack or ElasticMQ?

Add `uriEndpointOverride=http://localhost:4566&overrideEndpoint=true`. For servers that only speak
the old Query protocol, add `protocol=query` (or keep the default `auto`).

## Related

- [AWS SNS](aws2-sns.md), [AWS EventBridge](aws2-eventbridge.md), [AWS Lambda](aws2-lambda.md)
- [Azure Storage Queue](azure-storage-queue.md), [Azure Service Bus](azure-servicebus.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS SQS](https://camel.apache.org/components/next/aws2-sqs-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
