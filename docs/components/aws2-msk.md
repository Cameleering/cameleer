# AWS MSK component

> The `aws2-msk` component manages Amazon MSK (Managed Streaming for Apache Kafka) clusters, and Cameleer adds the `AWS_MSK_IAM` SASL mechanism so the `kafka` component can connect to MSK with IAM authentication.

**Scheme:** `aws2-msk:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Managed Streaming for Apache Kafka (MSK)](https://camel.apache.org/components/next/aws2-msk-component.html)

## When to use it

- Look up the bootstrap brokers of a cluster, then configure the `kafka` component with them.
- List, create or delete clusters from automation routes.
- Connect to MSK with IAM roles instead of SCRAM passwords (`saslMechanism=AWS_MSK_IAM` on `kafka:`).

## URI format

```text
aws2-msk://label?operation=...[&options]
```

## Example

Print the bootstrap broker strings of a cluster:

<!-- example: aws2-msk-bootstrap-brokers -->
```yaml
- route:
    id: msk-bootstrap
    from:
      uri: "timer:msk?repeatCount=1"
      steps:
        - setHeader:
            name: CamelAwsMSKClusterArn
            constant: "{{env:MSK_CLUSTER_ARN:arn:aws:kafka:us-east-1:123456789012:cluster/events/abcd}}"
        - to: "aws2-msk://ops?region={{env:AWS_REGION:us-east-1}}&operation=getBootstrapBrokers"
        - log: "IAM bootstrap: ${body[bootstrapBrokerStringSaslIam]}"
        - to: "log:bootstrap"
```

`getBootstrapBrokers` (Cameleer) returns the map of broker strings (`bootstrapBrokerStringTls`,
`bootstrapBrokerStringSaslIam`, ...).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:msk?repeatCount=1")
        .set_header("CamelAwsMSKClusterArn", constant("{{env:MSK_CLUSTER_ARN:arn:aws:kafka:us-east-1:123456789012:cluster/events/abcd}}"))
        .to("aws2-msk://ops?region={{env:AWS_REGION:us-east-1}}&operation=getBootstrapBrokers")
        .log("IAM bootstrap: ${body[bootstrapBrokerStringSaslIam]}")
        .to("log:bootstrap");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | | Required: `listClusters`, `createCluster`, `describeCluster`, `deleteCluster`, and the Cameleer operation `getBootstrapBrokers`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsMSKOperation` | in | Overrides `operation`. |
| `CamelAwsMSKClusterFilter` | in | Name prefix of `listClusters`. |
| `CamelAwsMSKClusterName`, `CamelAwsMSKClusterKafkaVersion`, `CamelAwsMSKBrokerNodesNumber`, `CamelAwsMSKBrokerNodesGroupInfo` | in | `createCluster` settings (the group info is a map with `clientSubnets`, `instanceType`, ...). |
| `CamelAwsMSKClusterArn` | in/out | Cluster; set by `createCluster`. |
| `CamelAwsMSKClusterState` | out | Cluster state. |

## Connecting the kafka component with IAM

```text
kafka:orders?brokers=b-1.events.abcd.c2.kafka.eu-west-1.amazonaws.com:9098&securityProtocol=SASL_SSL&saslMechanism=AWS_MSK_IAM&saslJaasConfig=software.amazon.msk.auth.iam.IAMLoginModule required;
```

Credentials come from the default AWS chain (or `awsProfileName="..."` in the JAAS config); the region
from the broker host name or `AWS_REGION`.

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e MSK_CLUSTER_ARN \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/msk.camel.yaml
```

## Common questions

### Do I need the aws-msk-iam-auth jar?

No. The `AWS_MSK_IAM` mechanism is built into Cameleer; the JAAS line is accepted for compatibility
with Camel and Kafka client configurations.

## Related

- [AWS Kinesis](aws2-kinesis.md), [AWS MQ](aws2-mq.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS MSK](https://camel.apache.org/components/next/aws2-msk-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
