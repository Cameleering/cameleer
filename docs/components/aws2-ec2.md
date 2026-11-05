# AWS EC2 component

> The `aws2-ec2` component starts, stops, creates, terminates, tags and describes Amazon EC2 instances.

**Scheme:** `aws2-ec2:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Elastic Compute Cloud (EC2)](https://camel.apache.org/components/next/aws2-ec2-component.html)

## When to use it

- Stop development instances every evening and start them in the morning.
- Launch worker instances when a queue grows, and terminate them afterwards.
- Tag instances or report their state from an operations route.

## URI format

```text
aws2-ec2://label?operation=...[&options]
```

## Example

Stop the development instances once a day (the timer fires at start and then every 24 hours):

<!-- example: aws2-ec2-stop-dev-instances -->
```yaml
- route:
    id: stop-dev-instances
    from:
      uri: "timer:stop-dev?period=86400000"
      steps:
        - setHeader:
            name: CamelAwsEC2InstancesIds
            constant: "{{env:DEV_INSTANCE_IDS:i-0123456789abcdef0}}"
        - to: "aws2-ec2://ops?region={{env:AWS_REGION:us-east-1}}&operation=stopInstances"
        - log: "state changes: ${body}"
```

`CamelAwsEC2InstancesIds` takes a list or comma separated ids. The body after the call is the list
of state changes `{instanceId, previousState, currentState}`.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:stop-dev?period=86400000")
        .set_header("CamelAwsEC2InstancesIds", constant("{{env:DEV_INSTANCE_IDS:i-0123456789abcdef0}}"))
        .to("aws2-ec2://ops?region={{env:AWS_REGION:us-east-1}}&operation=stopInstances")
        .log("state changes: ${body}");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | | Required: `describeInstances`, `describeInstancesStatus`, `createAndRunInstances`, `startInstances`, `stopInstances`, `terminateInstances`, `rebootInstances`, `monitorInstances`, `unmonitorInstances`, `createTags`, `deleteTags`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsEC2Operation` | in | Overrides `operation`. |
| `CamelAwsEC2InstancesIds` | in | Instance ids (list or comma separated). |
| `CamelAwsEC2ImageId`, `CamelAwsEC2InstanceType` | in | AMI and type of `createAndRunInstances`. |
| `CamelAwsEC2InstanceMinCount`, `CamelAwsEC2InstanceMaxCount` | in | Instance count (default 1). |
| `CamelAwsEC2InstanceKeyPair`, `CamelAwsEC2InstanceMonitoring`, `CamelAwsEC2InstanceKernelId`, `CamelAwsEC2InstanceEbsOptimized` | in | Launch settings. |
| `CamelAwsEC2InstanceSecurityGroups` | in | Security group names or `sg-` ids. |
| `CamelAwsEC2InstancesClientToken`, `CamelAwsEC2InstancesPlacement`, `CamelAwsEC2SubnetId` | in | Idempotency token, availability zone, subnet. |
| `CamelAwsEC2InstancesTags` | in | Tags (map); for `deleteTags` a list of keys or a map. |

Bodies: lists of instances (`instanceId`, `imageId`, `instanceType`, `state`, `launchTime`,
`privateIpAddress`, `tags`, ...), of state changes or of monitoring states.

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e DEV_INSTANCE_IDS \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/stop-dev.camel.yaml
```

## Common questions

### How do I launch an instance?

Use `operation=createAndRunInstances` with `CamelAwsEC2ImageId` and `CamelAwsEC2InstanceType`
(plus subnet, security groups and tags as needed); the body is the list of new instances.

## Related

- [AWS ECS](aws2-ecs.md), [AWS EKS](aws2-eks.md), [AWS IAM](aws2-iam.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS EC2](https://camel.apache.org/components/next/aws2-ec2-component.html)
