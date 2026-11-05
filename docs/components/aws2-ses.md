# AWS SES component

> The `aws2-ses` component sends e-mails (text, HTML or raw MIME) with Amazon Simple Email Service.

**Scheme:** `aws2-ses:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Simple Email Service (SES)](https://camel.apache.org/components/next/aws2-ses-component.html)

## When to use it

- Send notifications and alerts from routes without running an SMTP server.
- Send transactional mails (order confirmations, password resets) from events.
- Forward complete MIME messages built by another system (`SendRawEmail`).

## URI format

```text
aws2-ses://from@example.com[?options]
```

The path is the sender address; it must be verified in SES.

## Example

Mail every alert of a queue to the on-call address:

<!-- example: aws2-ses-mail-alerts -->
```yaml
- route:
    id: mail-alerts
    from:
      uri: "aws2-sqs://alerts?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - to: "aws2-ses://{{env:ALERT_SENDER:alerts@example.com}}?region={{env:AWS_REGION:us-east-1}}&to={{env:ONCALL_ADDRESS:oncall@example.com}}&subject=Production alert"
        - log: "sent mail ${header.CamelAwsSesMessageId}"
```

A text body is sent with `SendEmail` (as HTML with `htmlEmail=true`); a bytes body is sent as a
complete MIME message with `SendRawEmail`.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://alerts?region={{env:AWS_REGION:us-east-1}}")
        .to("aws2-ses://{{env:ALERT_SENDER:alerts@example.com}}?region={{env:AWS_REGION:us-east-1}}&to={{env:ONCALL_ADDRESS:oncall@example.com}}&subject=Production alert")
        .log("sent mail ${header.CamelAwsSesMessageId}");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `to`, `cc`, `bcc` | | Recipients (comma separated). |
| `subject` | | Subject. |
| `replyToAddresses` | | Reply-to addresses. |
| `returnPath` | | Bounce address. |
| `configurationSet` | | SES configuration set. |
| `htmlEmail` | `false` | Send the body as HTML. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsSesFrom` | in | Overrides the sender. |
| `CamelAwsSesTo`, `CamelAwsSesCc`, `CamelAwsSesBcc` | in | Recipients (list or comma separated). |
| `CamelAwsSesSubject`, `CamelAwsSesReturnPath`, `CamelAwsSesReplyToAddresses`, `CamelAwsSesConfigurationSet` | in | Override the options. |
| `CamelAwsSesHtmlEmail` | in | Send as HTML. |
| `CamelAwsSesMessageId` | out | Id of the sent mail. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e ALERT_SENDER -e ONCALL_ADDRESS \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/alerts.camel.yaml
```

## Common questions

### How do I send attachments?

Build a MIME message (for example with the `mime-multipart` data format) and send it as bytes:
it then goes out with `SendRawEmail`.

### Which SES API is used?

SES v1 (`SendEmail` / `SendRawEmail` over the Query API), as in Apache Camel's component.

## Related

- [AWS SNS](aws2-sns.md), [AWS SQS](aws2-sqs.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS SES](https://camel.apache.org/components/next/aws2-ses-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
