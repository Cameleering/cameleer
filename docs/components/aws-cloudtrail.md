# AWS CloudTrail component

> The `aws-cloudtrail` component polls AWS CloudTrail for new management events (who did what, when) and turns each event into a message.

**Scheme:** `aws-cloudtrail:` · **Crate:** `cameleer-cloud` · **Consumer:** yes · **Producer:** no ·
**Apache Camel:** [AWS CloudTrail](https://camel.apache.org/components/next/aws-cloudtrail-component.html)

## When to use it

- Security alerting: react when someone deletes a bucket, changes IAM policies or disables logging.
- Audit trails: copy selected API calls into a SIEM, a database or a ticket system.
- Trigger automation when a resource is created (tagging, compliance checks).

## URI format

```text
aws-cloudtrail://label[?options]
```

## Example

Watch S3 API calls and forward each event:

<!-- example: aws-cloudtrail-s3-audit -->
```yaml
- route:
    id: s3-audit
    from:
      uri: "aws-cloudtrail://s3-audit?region={{env:AWS_REGION:us-east-1}}&eventSource=s3.amazonaws.com&maxResults=10&delay=60000"
      steps:
        - convertBodyTo: String
        - log: "${header.CamelAwsCloudTrailEventUsername} called ${header.CamelAwsCloudTrailEventName}"
        - to: "log:s3-audit"
```

Each poll calls `LookupEvents` from the newest event already seen (all events of the trail on the
first poll), oldest first, and skips events already delivered. The body is the CloudTrail event
JSON document.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws-cloudtrail://s3-audit?region={{env:AWS_REGION:us-east-1}}&eventSource=s3.amazonaws.com&maxResults=10&delay=60000")
        .convert_body_to("String")
        .log("${header.CamelAwsCloudTrailEventUsername} called ${header.CamelAwsCloudTrailEventName}")
        .to("log:s3-audit");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `eventSource` | | Only events of this source, for example `s3.amazonaws.com` or `iam.amazonaws.com`. |
| `maxResults` | `1` | Events per poll. |

The consumer accepts the scheduled poll options (`delay` default 500 ms, `initialDelay`,
`scheduler.cron`, ...). CloudTrail limits `LookupEvents` to two calls per second per account, so a
longer `delay` is a good idea.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsCloudTrailEventId` | out | Event id. |
| `CamelAwsCloudTrailEventSource` | out | Event source (service). |
| `CamelAwsCloudTrailEventName` | out | API call, for example `DeleteBucket`. |
| `CamelAwsCloudTrailEventUsername` | out | User or role that made the call. |
| `CamelAwsCloudTrailEventTime` | out | Event time (epoch millis). |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/audit.camel.yaml
```

## Common questions

### How fast are events delivered?

CloudTrail itself delivers events to `LookupEvents` with a delay of several minutes; the consumer
adds at most one poll `delay`.

### How do I alert only on some calls?

Add a `filter` on `${header.CamelAwsCloudTrailEventName}`, for example `in 'DeleteBucket,PutBucketPolicy'`.

## Related

- [AWS Config](aws-config.md), [AWS Secrets Manager](aws-secrets-manager.md) (secret refresh uses CloudTrail)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS CloudTrail](https://camel.apache.org/components/next/aws-cloudtrail-component.html)
