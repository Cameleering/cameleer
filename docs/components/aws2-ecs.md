# AWS ECS component

> The `aws2-ecs` component lists, creates, describes and deletes Amazon ECS clusters.

**Scheme:** `aws2-ecs:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Elastic Container Service (ECS)](https://camel.apache.org/components/next/aws2-ecs-component.html)

## When to use it

- Report the ECS clusters of an account from an inventory route.
- Create or remove clusters as part of environment automation.

## URI format

```text
aws2-ecs://label?operation=...[&options]
```

## Example

List the clusters when the route starts:

<!-- example: aws2-ecs-list-clusters -->
```yaml
- route:
    id: ecs-inventory
    from:
      uri: "timer:ecs-inventory?repeatCount=1"
      steps:
        - to: "aws2-ecs://inventory?region={{env:AWS_REGION:us-east-1}}&operation=listClusters"
        - log: "ECS clusters: ${body}"
        - to: "log:ecs-clusters"
```

The body after `listClusters` is the list of cluster ARNs (all pages).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:ecs-inventory?repeatCount=1")
        .to("aws2-ecs://inventory?region={{env:AWS_REGION:us-east-1}}&operation=listClusters")
        .log("ECS clusters: ${body}")
        .to("log:ecs-clusters");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | | Required: `listClusters`, `createCluster`, `describeCluster`, `deleteCluster`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsECSOperation` | in | Overrides `operation`. |
| `CamelAwsECSMaxResults` | in | Page size of `listClusters`. |
| `CamelAwsECSClusterName` | in | Cluster of the create, describe and delete operations (body = the cluster map). |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/ecs.camel.yaml
```

## Common questions

### Can it run tasks or update services?

No. Like Apache Camel's component it covers cluster operations only.

## Related

- [AWS EKS](aws2-eks.md), [AWS EC2](aws2-ec2.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS ECS](https://camel.apache.org/components/next/aws2-ecs-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
