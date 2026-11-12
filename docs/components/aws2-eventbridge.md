# AWS EventBridge component

> The `aws2-eventbridge` component publishes events to Amazon EventBridge buses and manages rules and targets.

**Scheme:** `aws2-eventbridge:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Eventbridge](https://camel.apache.org/components/next/aws2-eventbridge-component.html)

## When to use it

- Emit business events (order placed, device offline) that serverless consumers react to.
- Bridge events from on-premises systems, MQTT or Kafka into an AWS event-driven architecture.
- Create and wire rules from an automation route (`putRule`, `putTargets`).

## URI format

```text
aws2-eventbridge://eventbusNameOrArn[?options]
```

Use `default` for the account's default event bus.

## Example

Turn every order message into an `OrderPlaced` event on the default bus:

<!-- example: aws2-eventbridge-put-order-event -->
```yaml
- route:
    id: orders-to-eventbridge
    from:
      uri: "aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - setHeader:
            name: CamelAwsEventbridgeEventSource
            constant: com.example.shop
        - setHeader:
            name: CamelAwsEventbridgeEventDetailType
            constant: OrderPlaced
        - to: "aws2-eventbridge://default?region={{env:AWS_REGION:us-east-1}}&operation=putEvent"
        - log: "event entry ids: ${body}"
```

The body is the event detail and must be JSON.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}")
        .set_header("CamelAwsEventbridgeEventSource", constant("com.example.shop"))
        .set_header("CamelAwsEventbridgeEventDetailType", constant("OrderPlaced"))
        .to("aws2-eventbridge://default?region={{env:AWS_REGION:us-east-1}}&operation=putEvent")
        .log("event entry ids: ${body}");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `putRule` | `putRule`, `putTargets`, `removeTargets`, `deleteRule`, `enableRule`, `disableRule`, `describeRule`, `listRules`, `listTargetsByRule`, `listRuleNamesByTarget`, `putEvent`. The `CamelAwsEventbridgeOperation` header overrides it. |
| `eventPatternFile` | | Event pattern of `putRule`, read from a file (`file:` prefix or a plain path). |

Note that the default operation is `putRule`, as in Apache Camel: set `operation=putEvent` to publish.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsEventbridgeOperation` | in | Overrides `operation`. |
| `CamelAwsEventbridgeEventSource` | in | Source of `putEvent`. |
| `CamelAwsEventbridgeEventDetailType` | in | Detail type of `putEvent`. |
| `CamelAwsEventbridgeEventResources` | in | Resource ARNs of `putEvent` (list or comma separated). |
| `CamelAwsEventbridgeFailedEntryCount` | out | Failed entries of `putEvent`. |
| `CamelAwsEventbridgeRuleName` | in | Rule of the rule operations. |
| `CamelAwsEventbridgeRuleNamePrefix` | in | Prefix of `listRules`. |
| `CamelAwsEventbridgeEventPattern` | in | Event pattern of `putRule`. |
| `CamelAwsEventbridgeTargets` | in | Targets of `putTargets` (list of `{id, arn, input}` maps). |
| `CamelAwsEventbridgeTargetsIds` | in | Ids of `removeTargets`. |
| `CamelAwsEventbridgeTargetArn` | in | Target of `listRuleNamesByTarget`. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/events.camel.yaml
```

## Common questions

### How do I consume EventBridge events in a route?

Create a rule whose target is an SQS queue, and consume the queue with [aws2-sqs](aws2-sqs.md). The
event arrives as the JSON envelope (`source`, `detail-type`, `detail`, ...).

### Why was my event not delivered?

Check `CamelAwsEventbridgeFailedEntryCount`, then the rule's event pattern: `source` and
`detail-type` must match exactly.

## Related

- [AWS SQS](aws2-sqs.md), [AWS SNS](aws2-sns.md), [AWS Lambda](aws2-lambda.md)
- [Azure Event Grid](azure-eventgrid.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS EventBridge](https://camel.apache.org/components/next/aws2-eventbridge-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
