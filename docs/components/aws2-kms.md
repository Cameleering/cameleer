# AWS KMS component

> The `aws2-kms` component manages AWS KMS keys and, as a Cameleer extension, encrypts and decrypts data with them.

**Scheme:** `aws2-kms:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Key Management Service (KMS)](https://camel.apache.org/components/next/aws2-kms-component.html)

## When to use it

- Encrypt sensitive payloads (personal data, documents) before they are stored or sent on.
- Create, enable, disable and schedule deletion of keys from automation routes.
- List keys for an inventory or audit report.

## URI format

```text
aws2-kms://label?operation=...[&options]
```

## Example

Encrypt every message of a queue with a KMS key before it is archived in S3:

<!-- example: aws2-kms-encrypt-to-s3 -->
```yaml
- route:
    id: encrypt-and-archive
    from:
      uri: "aws2-sqs://customer-records?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - setHeader:
            name: CamelAwsKMSKeyId
            constant: "{{env:KMS_KEY_ID:alias/customer-data}}"
        - setHeader:
            name: CamelAwsS3Key
            simple: "records/${header.CamelAwsSqsMessageId}.bin"
        - to: "aws2-kms://crypto?region={{env:AWS_REGION:us-east-1}}&operation=encrypt"
        - to: "aws2-s3://{{env:ARCHIVE_BUCKET:encrypted-archive}}?region={{env:AWS_REGION:us-east-1}}&autoCreateBucket=true"
```

`encrypt` replaces the body with the ciphertext blob (bytes). `decrypt` turns it back into the
plaintext and sets `CamelAwsKMSKeyId` to the key that was used. KMS encrypts at most 4 KB per call;
for larger data, encrypt a data key or use S3 server-side encryption (`useAwsKMS=true` on
[aws2-s3](aws2-s3.md)).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://customer-records?region={{env:AWS_REGION:us-east-1}}")
        .set_header("CamelAwsKMSKeyId", constant("{{env:KMS_KEY_ID:alias/customer-data}}"))
        .set_header("CamelAwsS3Key", simple("records/${header.CamelAwsSqsMessageId}.bin"))
        .to("aws2-kms://crypto?region={{env:AWS_REGION:us-east-1}}&operation=encrypt")
        .to("aws2-s3://{{env:ARCHIVE_BUCKET:encrypted-archive}}?region={{env:AWS_REGION:us-east-1}}&autoCreateBucket=true");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | | Required: `listKeys`, `createKey`, `describeKey`, `enableKey`, `disableKey`, `scheduleKeyDeletion`, and the Cameleer operations `encrypt`, `decrypt`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsKMSOperation` | in | Overrides `operation`. |
| `CamelAwsKMSKeyId` | in/out | Key id, ARN or alias; set by `createKey` and `decrypt`. |
| `CamelAwsKMSDescription` | in | Description of `createKey`. |
| `CamelAwsKMSLimit` | in | Page size of `listKeys` (all pages are read). |
| `CamelAwsKMSPendingWindowInDays` | in | Waiting period of `scheduleKeyDeletion`. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e KMS_KEY_ID -e ARCHIVE_BUCKET \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/encrypt.camel.yaml
```

## Common questions

### Is `encrypt` available in Apache Camel?

No. Apache Camel's KMS component only manages keys; `encrypt` and `decrypt` are Cameleer
extensions with the same naming style.

## Related

- [AWS Secrets Manager](aws-secrets-manager.md), [AWS S3](aws2-s3.md), [Azure Key Vault](azure-key-vault.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS KMS](https://camel.apache.org/components/next/aws2-kms-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
