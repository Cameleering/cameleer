# AWS Comprehend component

> The `aws2-comprehend` component analyses text with Amazon Comprehend: sentiment, language, entities, key phrases, PII, syntax, toxicity and custom classification.

**Scheme:** `aws2-comprehend:` · **Also:** `aws-comprehend:` (same component) · **Crate:** `cameleer-cloud` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** `camel-aws-comprehend` (see the [component list](https://camel.apache.org/components/next/index.html))

## When to use it

- Route customer messages by sentiment (escalate negative reviews).
- Detect and redact personal data (PII) before text is stored or sent to an LLM.
- Detect the language of a message before translating it.
- Give an AI agent a cheap, deterministic text-analysis tool next to the model.

## URI format

```text
aws2-comprehend://label?operation=...[&options]
```

## Example

Escalate negative product reviews:

<!-- example: aws2-comprehend-sentiment-routing -->
```yaml
- route:
    id: review-sentiment
    from:
      uri: "aws2-sqs://reviews?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - setProperty:
            name: review
            simple: "${body}"
        - to: "aws2-comprehend://reviews?region={{env:AWS_REGION:us-east-1}}&operation=detectSentiment&languageCode=en"
        - choice:
            when:
              - simple: "${body} == 'NEGATIVE'"
                steps:
                  - setBody:
                      simple: "${exchangeProperty.review}"
                  - to: "aws2-sqs://escalations?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true"
            otherwise:
              steps:
                - log: "review sentiment: ${body}"
```

`detectSentiment` replaces the body with the sentiment (`POSITIVE`, `NEGATIVE`, `NEUTRAL` or `MIXED`)
and sets the scores in `CamelAwsComprehendSentimentScore`, so the original text is kept in a property.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://reviews?region={{env:AWS_REGION:us-east-1}}")
        .set_property("review", simple("${body}"))
        .to("aws2-comprehend://reviews?region={{env:AWS_REGION:us-east-1}}&operation=detectSentiment&languageCode=en")
        .choice()
            .when(simple("${body} == 'NEGATIVE'"))
                .set_body(simple("${exchangeProperty.review}"))
                .to("aws2-sqs://escalations?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true")
            .otherwise()
                .log("review sentiment: ${body}")
        .end();
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `detectSentiment` | `detectDominantLanguage`, `detectEntities`, `detectKeyPhrases`, `detectPiiEntities`, `detectSyntax`, `detectSentiment`, `containsPiiEntities`, `detectToxicContent`, `classifyDocument`. |
| `languageCode` | `en` | Language of the text. |
| `endpointArn` | | Custom classifier endpoint of `classifyDocument`. |

| operation | body after the call |
|---|---|
| `detectDominantLanguage` | list of `{LanguageCode, Score}`; the best one in `CamelAwsComprehendDetectedLanguage` |
| `detectEntities`, `detectKeyPhrases`, `detectPiiEntities`, `detectSyntax` | list of entities, key phrases or tokens |
| `detectSentiment` | the sentiment; scores in `CamelAwsComprehendSentimentScore` |
| `containsPiiEntities` | list of PII labels |
| `detectToxicContent` | result list |
| `classifyDocument` | list of classes |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsComprehendOperation` | in | Overrides `operation`. |
| `CamelAwsComprehendLanguageCode` | in | Overrides `languageCode`. |
| `CamelAwsComprehendEndpointArn` | in | Overrides `endpointArn`. |
| `CamelAwsComprehendDetectedLanguage` | out | Best language of `detectDominantLanguage`. |
| `CamelAwsComprehendSentimentScore` | out | Scores of `detectSentiment` (map). |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/reviews.camel.yaml
```

## Common questions

### How do I find personal data in a message?

Use `operation=detectPiiEntities`: the body becomes the list of entities with their type and
offsets, which you can use to mask the text. `containsPiiEntities` only tells which kinds are present.

### How do I detect the language and then translate?

Call `detectDominantLanguage`, keep the text in a property, and pass
`${header.CamelAwsComprehendDetectedLanguage}` as the source language to [aws2-translate](aws2-translate.md).

## Related

- [AWS Translate](aws2-translate.md), [AWS Bedrock](aws-bedrock.md), [AWS Textract](aws2-textract.md)
- [Cloud credentials](../guides/cloud-credentials.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
