# AWS STS component

> The `aws2-sts` component gets temporary AWS credentials (assume role, session and federation tokens) and tells which identity the credentials belong to.

**Scheme:** `aws2-sts:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Security Token Service (STS)](https://camel.apache.org/components/next/aws2-sts-component.html)

## When to use it

- Check at startup which account and role an integration runs as (`getCallerIdentity`).
- Assume a role in another account and use the temporary credentials for other AWS endpoints.
- Issue short-lived federation tokens for clients.

## URI format

```text
aws2-sts://label[?options]
```

## Example

Log the identity of the integration when it starts:

<!-- example: aws2-sts-caller-identity -->
```yaml
- route:
    id: who-am-i
    from:
      uri: "timer:whoami?repeatCount=1"
      steps:
        - to: "aws2-sts://sts?region={{env:AWS_REGION:us-east-1}}&operation=getCallerIdentity"
        - log: "running as ${body[arn]} in account ${body[account]}"
        - to: "log:identity"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:whoami?repeatCount=1")
        .to("aws2-sts://sts?region={{env:AWS_REGION:us-east-1}}&operation=getCallerIdentity")
        .log("running as ${body[arn]} in account ${body[account]}")
        .to("log:identity");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `assumeRole` | `assumeRole`, `getSessionToken`, `getFederationToken`, `getCallerIdentity`. |

The credential operations return the map `{accessKeyId, secretAccessKey, sessionToken, expiration}`
(plus `assumedRoleArn` or `federatedUserArn`), ready to be used as the `accessKey`, `secretKey` and
`sessionToken` options of other AWS endpoints. `getCallerIdentity` returns the identity map.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsStsOperation` | in | Overrides `operation`. |
| `CamelAwsStsRoleArn`, `CamelAwsStsRoleSessionName` | in | Role of `assumeRole`. |
| `CamelAwsStsDurationSeconds` | in | Lifetime of the credentials. |
| `CamelAwsStsFederatedName`, `CamelAwsStsPolicy` | in | `getFederationToken`. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/whoami.camel.yaml
```

## Common questions

### Do I need this component to use IAM roles?

No. Instance roles, ECS task roles and EKS web identity (IRSA) are picked up by the default
credentials chain of every AWS component; see [Cloud credentials](../guides/cloud-credentials.md).

## Related

- [AWS IAM](aws2-iam.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS STS](https://camel.apache.org/components/next/aws2-sts-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
