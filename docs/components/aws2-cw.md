# AWS CloudWatch component

> The `aws2-cw` component publishes custom metrics to Amazon CloudWatch.

**Scheme:** `aws2-cw:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS CloudWatch](https://camel.apache.org/components/next/aws2-cw-component.html)

## When to use it

- Count business events (orders received, failed payments) and alarm on them.
- Publish sensor or gateway readings from edge devices as metrics.
- Track route health (queue depth, processing time) next to your AWS dashboards.

## URI format

```text
aws2-cw://namespace[?options]
```

## Example

Count every order received, with the order's sales channel as a dimension. The queue messages are
small JSON objects such as `{"channel":"web"}`; after `unmarshal` the map is used as the
dimensions:

<!-- example: aws2-cw-count-orders-by-channel -->
```yaml
- route:
    id: order-metrics-by-channel
    from:
      uri: "aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: CamelAwsCwMetricDimensions
            simple: "${body}"
        - to: "aws2-cw://Shop/Orders?region={{env:AWS_REGION:us-east-1}}&name=OrdersReceived&value=1&unit=Count"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}")
        .unmarshal("json")
        .set_header("CamelAwsCwMetricDimensions", simple("${body}"))
        .to("aws2-cw://Shop/Orders?region={{env:AWS_REGION:us-east-1}}&name=OrdersReceived&value=1&unit=Count");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `name` | | Metric name. |
| `value` | `1.0` | Metric value. |
| `unit` | `Count` | Unit (`Count`, `Seconds`, `Milliseconds`, `Bytes`, `Percent`, ...). |
| `timestamp` | now | Timestamp (epoch millis or ISO 8601). |

Each exchange is one `PutMetricData` call. An array body sends several data, each an object
`{name, value, unit, timestamp, dimensions}`.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsCwMetricNamespace` | in | Overrides the namespace. |
| `CamelAwsCwMetricName`, `CamelAwsCwMetricValue`, `CamelAwsCwMetricUnit`, `CamelAwsCwMetricTimestamp` | in | Override the options. |
| `CamelAwsCwMetricDimensions` | in | Dimensions (map name to value). |
| `CamelAwsCwMetricDimensionName`, `CamelAwsCwMetricDimensionValue` | in | A single dimension. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/metrics.camel.yaml
```

## Common questions

### How do I publish a measured value?

Set `CamelAwsCwMetricValue` from the message, for example `${header.durationMs}`, with
`unit=Milliseconds`.

### Can I read metrics or alarms?

No. The component only publishes metrics (`PutMetricData`), as in Apache Camel.

## Related

- [AWS CloudTrail](aws-cloudtrail.md), [AWS SNS](aws2-sns.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS CloudWatch](https://camel.apache.org/components/next/aws2-cw-component.html)
