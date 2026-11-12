# AWS EKS component

> The `aws2-eks` component lists, creates, describes and deletes Amazon EKS (Kubernetes) clusters.

**Scheme:** `aws2-eks:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Elastic Kubernetes Service (EKS)](https://camel.apache.org/components/next/aws2-eks-component.html)

## When to use it

- Report the EKS clusters of an account from an inventory route.
- Create or delete clusters as part of environment automation.

## URI format

```text
aws2-eks://label?operation=...[&options]
```

## Example

List the cluster names when the route starts:

<!-- example: aws2-eks-list-clusters -->
```yaml
- route:
    id: eks-inventory
    from:
      uri: "timer:eks-inventory?repeatCount=1"
      steps:
        - to: "aws2-eks://inventory?region={{env:AWS_REGION:us-east-1}}&operation=listClusters"
        - log: "EKS clusters: ${body}"
        - to: "log:eks-clusters"
```

The body after `listClusters` is the list of cluster names (all pages).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:eks-inventory?repeatCount=1")
        .to("aws2-eks://inventory?region={{env:AWS_REGION:us-east-1}}&operation=listClusters")
        .log("EKS clusters: ${body}")
        .to("log:eks-clusters");
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
| `CamelAwsEKSOperation` | in | Overrides `operation`. |
| `CamelAwsEKSMaxResults` | in | Page size of `listClusters`. |
| `CamelAwsEKSClusterName` | in | Cluster of the create, describe and delete operations (body = the cluster map). |
| `CamelAwsEKSRoleARN` | in | Cluster role of `createCluster`. |
| `CamelAwsEKSVPCConfig` | in | `createCluster` network: map of `subnetIds` and `securityGroupIds`. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/eks.camel.yaml
```

## Common questions

### Can it deploy to Kubernetes?

No. It manages EKS clusters through the AWS API; it does not talk to the Kubernetes API server.

## Related

- [AWS ECS](aws2-ecs.md), [AWS EC2](aws2-ec2.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS EKS](https://camel.apache.org/components/next/aws2-eks-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
