# AWS SNS component

> The `aws2-sns` component publishes messages to Amazon SNS topics, including FIFO topics and batches.

**Scheme:** `aws2-sns:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Simple Notification System (SNS)](https://camel.apache.org/components/next/aws2-sns-component.html)

## When to use it

- Fan out an event to many subscribers (SQS queues, Lambda functions, e-mail, SMS, HTTPS).
- Send operational alerts from a route to an on-call topic.
- Connect a Cameleer route on an edge device to a serverless backend that subscribes to the topic.

## URI format

```text
aws2-sns://topicNameOrArn[?options]
```

When the path is a topic ARN it is used as is. Otherwise the topic is looked up by name
(`ListTopics`) or created with `autoCreateTopic=true`.

## Example

Forward alerts from an SQS queue to an SNS topic with a subject:

<!-- example: aws2-sns-publish-alerts -->
```yaml
- route:
    id: alerts-to-sns
    from:
      uri: "aws2-sqs://alerts?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - to: "aws2-sns://ops-alerts?region={{env:AWS_REGION:us-east-1}}&subject=Ops alert&autoCreateTopic=true"
        - log: "published ${header.CamelAwsSnsMessageId}"
```

Credentials come from the default AWS chain; see [Cloud credentials](../guides/cloud-credentials.md).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://alerts?region={{env:AWS_REGION:us-east-1}}")
        .to("aws2-sns://ops-alerts?region={{env:AWS_REGION:us-east-1}}&subject=Ops alert&autoCreateTopic=true")
        .log("published ${header.CamelAwsSnsMessageId}");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `subject` | | Message subject (`CamelAwsSnsSubject` wins). |
| `messageStructure` | | `json` to send a different message per protocol. |
| `autoCreateTopic` | `false` | Create the topic when it does not exist. |
| `messageGroupIdStrategy` | | FIFO topics: `useConstant`, `useExchangeId` or `usePropertyValue`. |
| `messageDeduplicationIdStrategy` | `useExchangeId` | FIFO topics: `useExchangeId` or `useContentBasedDeduplication`. |
| `subscribeSNStoSQS` | `false` | Subscribe the SQS queue `queueArn` to the topic when the endpoint starts. |
| `queueArn` | | Queue to subscribe with `subscribeSNStoSQS`. |
| `policy` | | Topic policy (JSON text, or a `file:` / `classpath:` resource). |
| `serverSideEncryptionEnabled`, `kmsMasterKeyId` | `false` | Topic encryption. |
| `batchEnabled` | `false` | Publish a list body with `PublishBatch` (10 per request); body = `{successful, failed}`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsSnsMessageId` | out | Id of the published message. |
| `CamelAwsSnsSubject` | in | Subject. |
| `CamelAwsSnsMessageStructure` | in | Message structure. |
| `CamelAwsSnsMessageGroupId` | in (property) | Group id with `messageGroupIdStrategy=usePropertyValue`. |

Other message headers are published as SNS message attributes.

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/alerts.camel.yaml
```

## Common questions

### How do I fan out to an SQS queue?

Add `subscribeSNStoSQS=true&queueArn=arn:aws:sqs:<region>:<account>:<queue>`; the subscription is
created when the endpoint starts (it is idempotent). The queue policy must allow the topic to send.

### Can I consume from SNS?

No. SNS pushes to subscribers; subscribe an SQS queue and consume it with [aws2-sqs](aws2-sqs.md).

### How do I publish many messages in one call?

Send a list body with `batchEnabled=true`.

## Related

- [AWS SQS](aws2-sqs.md), [AWS EventBridge](aws2-eventbridge.md), [AWS SES](aws2-ses.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS SNS](https://camel.apache.org/components/next/aws2-sns-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
