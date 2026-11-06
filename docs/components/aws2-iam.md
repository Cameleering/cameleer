# AWS IAM component

> The `aws2-iam` component manages AWS IAM users, groups, access keys, roles, policies and instance profiles.

**Scheme:** `aws2-iam:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Identity and Access Management (IAM)](https://camel.apache.org/components/next/aws2-iam-component.html)

## When to use it

- Onboard and offboard users from an HR or ticket system.
- Rotate access keys on a schedule.
- Create roles and attach policies as part of environment automation.

## URI format

```text
aws2-iam://label?operation=...[&options]
```

IAM is a global service: requests go to `iam.amazonaws.com`, signed for `us-east-1`.

## Example

Create an IAM user for every user name that arrives on an onboarding queue:

<!-- example: aws2-iam-onboard-users -->
```yaml
- route:
    id: onboard-users
    from:
      uri: "aws2-sqs://onboarding?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - setHeader:
            name: CamelAwsIAMUsername
            simple: "${body}"
        - to: "aws2-iam://iam?operation=createUser"
        - log: "created IAM user ${body[userName]} (${body[arn]})"
```

The body after `createUser` is the user map `{userName, userId, arn, path, createDate}`.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://onboarding?region={{env:AWS_REGION:us-east-1}}")
        .set_header("CamelAwsIAMUsername", simple("${body}"))
        .to("aws2-iam://iam?operation=createUser")
        .log("created IAM user ${body[userName]} (${body[arn]})");
})?;
```

## Options

Common AWS options (credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | | Required. Users and keys: `createUser`, `getUser`, `deleteUser`, `listUsers`, `createAccessKey`, `listAccessKeys`, `deleteAccessKey`, `updateAccessKey`. Groups: `createGroup`, `getGroup`, `deleteGroup`, `listGroups`, `addUserToGroup`, `removeUserFromGroup`. Roles and policies: `createRole`, `getRole`, `deleteRole`, `listRoles`, `createPolicy`, `getPolicy`, `deletePolicy`, `listPolicies`, `attachUserPolicy`, `detachUserPolicy`, `listAttachedUserPolicies`, `attachGroupPolicy`, `detachGroupPolicy`, `listAttachedGroupPolicies`, `attachRolePolicy`, `detachRolePolicy`, `listAttachedRolePolicies`. Instance profiles: `createInstanceProfile`, `getInstanceProfile`, `deleteInstanceProfile`, `listInstanceProfiles`, `addRoleToInstanceProfile`, `removeRoleFromInstanceProfile`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsIAMOperation` | in | Overrides `operation`. |
| `CamelAwsIAMUsername` | in | User. |
| `CamelAwsIAMGroupName`, `CamelAwsIAMGroupPath` | in | Group. |
| `CamelAwsIAMAccessKeyID`, `CamelAwsIAMAccessKeyStatus` | in | Access key and its new status (`Active`, `Inactive`). |
| `CamelAwsIAMRoleName`, `CamelAwsIAMAssumeRolePolicyDocument` | in | Role and its trust policy. |
| `CamelAwsIAMPolicyName`, `CamelAwsIAMPolicyDocument`, `CamelAwsIAMPolicyArn` | in | Policy (the document may also be the body). |
| `CamelAwsIAMInstanceProfileName` | in | Instance profile. |
| `CamelAwsIAMPath`, `CamelAwsIAMDescription` | in | Path and description of roles. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/onboarding.camel.yaml
```

## Common questions

### How do I create an access key for a user?

Use `operation=createAccessKey` with `CamelAwsIAMUsername`. The body is
`{accessKeyId, secretAccessKey, status}`: store it right away, for example in
[Secrets Manager](aws-secrets-manager.md), since the secret cannot be read again.

## Related

- [AWS STS](aws2-sts.md), [AWS Secrets Manager](aws-secrets-manager.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS IAM](https://camel.apache.org/components/next/aws2-iam-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
