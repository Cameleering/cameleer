# AWS Config component

> The `aws-config` component manages AWS Config rules and conformance packs and reads their compliance results.

**Scheme:** `aws-config:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Config Service](https://camel.apache.org/components/next/aws-config-component.html)

## When to use it

- Enable managed compliance rules from an automation route.
- Report rule compliance on a schedule to a chat channel or ticket system.
- Deploy conformance packs as part of an environment setup.

## URI format

```text
aws-config://label?operation=...[&options]
```

## Example

Enable the managed rule that forbids public-read S3 buckets when the route starts:

<!-- example: aws-config-put-rule -->
```yaml
- route:
    id: enable-config-rule
    from:
      uri: "timer:config-rules?repeatCount=1"
      steps:
        - setHeader:
            name: CamelAwsConfigRuleName
            constant: s3-bucket-public-read-prohibited
        - setHeader:
            name: CamelAwsConfigRuleSourceIdentifier
            constant: S3_BUCKET_PUBLIC_READ_PROHIBITED
        - to: "aws-config://rules?region={{env:AWS_REGION:us-east-1}}&operation=putConfigRule"
        - log: "rule ${header.CamelAwsConfigRuleName} is in place"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:config-rules?repeatCount=1")
        .set_header("CamelAwsConfigRuleName", constant("s3-bucket-public-read-prohibited"))
        .set_header("CamelAwsConfigRuleSourceIdentifier", constant("S3_BUCKET_PUBLIC_READ_PROHIBITED"))
        .to("aws-config://rules?region={{env:AWS_REGION:us-east-1}}&operation=putConfigRule")
        .log("rule ${header.CamelAwsConfigRuleName} is in place");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | | Required: `putConfigRule`, `removeConfigRule`, `describeConfigRules`, `describeRuleCompliance`, `putConformancePack`, `removeConformancePack`, `describeConformancePackCompliance`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsConfigOperation` | in | Overrides `operation`. |
| `CamelAwsConfigRuleName` | in | Rule name (optional filter for the describe operations). |
| `CamelAwsConfigRuleSourceIdentifier` | in | Managed rule identifier or Lambda ARN of `putConfigRule`. |
| `CamelAwsConfigRuleSourceOwner` | in | `AWS` (default) or `CUSTOM_LAMBDA`. |
| `CamelAwsConfigRuleInputParameters` | in | Rule parameters (map or JSON text). |
| `CamelAwsConfigRuleScope` | in | Rule scope (map). |
| `CamelAwsConformancePackName` | in | Conformance pack. |
| `CamelAwsConformancePackTemplateBody` | in | Template of `putConformancePack` (otherwise the body). |

`describeConfigRules` returns the list of rules; `describeRuleCompliance` the list of
`ComplianceByConfigRule` entries; `describeConformancePackCompliance` the rule compliance list.

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/config.camel.yaml
```

## Common questions

### How do I get a daily compliance report?

Use a timer with `period=86400000`, call `operation=describeRuleCompliance`, and `split` the body
to send one message per non-compliant rule.

## Related

- [AWS CloudTrail](aws-cloudtrail.md), [AWS IAM](aws2-iam.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Config](https://camel.apache.org/components/next/aws-config-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
