# AWS Translate component

> The `aws2-translate` component translates text between languages with Amazon Translate, with optional automatic source language detection.

**Scheme:** `aws2-translate:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Translate](https://camel.apache.org/components/next/aws2-translate-component.html)

## When to use it

- Translate support replies or product texts into the customer's language.
- Normalize incoming messages to one language before classification or an LLM call.
- Apply custom terminologies (brand names, product terms) to every translation.

## URI format

```text
aws2-translate://label[?options]
```

## Example

Translate replies into German:

<!-- example: aws2-translate-replies -->
```yaml
- route:
    id: translate-replies
    from:
      uri: "aws2-sqs://replies-en?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - to: "aws2-translate://replies?region={{env:AWS_REGION:us-east-1}}&sourceLanguage=en&targetLanguage=de"
        - to: "aws2-sqs://replies-de?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true"
```

The body text is translated; the body after the call is the translation.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://replies-en?region={{env:AWS_REGION:us-east-1}}")
        .to("aws2-translate://replies?region={{env:AWS_REGION:us-east-1}}&sourceLanguage=en&targetLanguage=de")
        .to("aws2-sqs://replies-de?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `translateText` | The only operation. |
| `sourceLanguage` | | Source language code. |
| `targetLanguage` | | Target language code. |
| `autodetectSourceLanguage` | `false` | Send `auto` as the source language; the detected language is set in `CamelAwsTranslateSourceLanguage`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsTranslateOperation` | in | Overrides `operation`. |
| `CamelAwsTranslateSourceLanguage` | in/out | Source language (out: the detected one with auto-detection). |
| `CamelAwsTranslateTargetLanguage` | in | Target language. |
| `CamelAwsTranslateTerminologyNames` | in | Custom terminologies (list). |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/translate.camel.yaml
```

## Common questions

### How do I translate into the language of each customer?

Set `CamelAwsTranslateTargetLanguage` per message (for example from a customer header) instead of
the `targetLanguage` option.

### What if I do not know the source language?

Set `autodetectSourceLanguage=true`.

## Related

- [AWS Comprehend](aws2-comprehend.md), [AWS Polly](aws2-polly.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Translate](https://camel.apache.org/components/next/aws2-translate-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
