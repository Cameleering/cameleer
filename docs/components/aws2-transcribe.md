# AWS Transcribe component

> The `aws2-transcribe` component starts and manages Amazon Transcribe speech-to-text jobs, including medical and call analytics jobs, custom vocabularies and language models.

**Scheme:** `aws2-transcribe:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Transcribe](https://camel.apache.org/components/next/aws2-transcribe-component.html)

## When to use it

- Transcribe call recordings or voice messages uploaded to S3.
- Feed transcripts to an LLM for summaries, or to Comprehend for sentiment.
- Analyse contact center calls with call analytics categories.

## URI format

```text
aws2-transcribe://label?operation=...[&options]
```

## Example

Start a transcription job for every recording URI that arrives on a queue:

<!-- example: aws2-transcribe-start-jobs -->
```yaml
- route:
    id: transcribe-recordings
    from:
      uri: "aws2-sqs://recordings?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - setHeader:
            name: CamelAwsTranscribeTranscriptionJobName
            simple: "call-${header.CamelAwsSqsMessageId}"
        - setHeader:
            name: CamelAwsTranscribeOutputBucketName
            constant: "{{env:TRANSCRIPT_BUCKET:call-transcripts}}"
        - to: "aws2-transcribe://calls?region={{env:AWS_REGION:us-east-1}}&operation=startTranscriptionJob&languageCode=en-US"
        - log: "job ${header.CamelAwsTranscribeTranscriptionJobName}: ${header.CamelAwsTranscribeTranscriptionJobStatus}"
        - to: "log:jobs"
```

The body is the media URI (for example `s3://recordings/call-17.wav`); the body after the call is the
job map and `CamelAwsTranscribeTranscriptionJobStatus` is set.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://recordings?region={{env:AWS_REGION:us-east-1}}")
        .set_header("CamelAwsTranscribeTranscriptionJobName", simple("call-${header.CamelAwsSqsMessageId}"))
        .set_header("CamelAwsTranscribeOutputBucketName", constant("{{env:TRANSCRIPT_BUCKET:call-transcripts}}"))
        .to("aws2-transcribe://calls?region={{env:AWS_REGION:us-east-1}}&operation=startTranscriptionJob&languageCode=en-US")
        .log("job ${header.CamelAwsTranscribeTranscriptionJobName}: ${header.CamelAwsTranscribeTranscriptionJobStatus}")
        .to("log:jobs");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | | Required. Jobs: `startTranscriptionJob`, `getTranscriptionJob`, `deleteTranscriptionJob`, `listTranscriptionJobs`. Medical: `startMedicalTranscriptionJob`, `getMedicalTranscriptionJob`, `deleteMedicalTranscriptionJob`, `listMedicalTranscriptionJobs`. Call analytics: `startCallAnalyticsJob`, `getCallAnalyticsJob`, `deleteCallAnalyticsJob`, `listCallAnalyticsJobs`, `createCallAnalyticsCategory`, `updateCallAnalyticsCategory`, `getCallAnalyticsCategory`, `deleteCallAnalyticsCategory`, `listCallAnalyticsCategories`. Vocabularies: `createVocabulary`, `updateVocabulary`, `getVocabulary`, `deleteVocabulary`, `listVocabularies` and the `...MedicalVocabulary` variants. Language models: `createLanguageModel`, `describeLanguageModel`, `deleteLanguageModel`, `listLanguageModels`. Tags: `tagResource`, `untagResource`, `listTagsForResource`. |
| `languageCode` | | Language of the media (`CamelAwsTranscribeLanguageCode` wins). |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsTranscribeOperation` | in | Overrides `operation`. |
| `CamelAwsTranscribeTranscriptionJobName` | in | Job name. |
| `CamelAwsTranscribeMediaUri` | in | Media URI (otherwise the body). |
| `CamelAwsTranscribeLanguageCode`, `CamelAwsTranscribeIdentifyLanguage` | in | Language, or automatic identification. |
| `CamelAwsTranscribeMediaFormat`, `CamelAwsTranscribeMediaSampleRateHertz` | in | Media settings. |
| `CamelAwsTranscribeOutputBucketName` | in | Bucket for the transcript. |
| `CamelAwsTranscribeStatus`, `CamelAwsTranscribeJobNameContains` | in | Filters of `listTranscriptionJobs`. |
| `CamelAwsTranscribeTranscriptionJobStatus` | out | Job status (or vocabulary state / model status). |
| `CamelAwsTranscribeMedicalTranscriptionJobName`, `CamelAwsTranscribeSpecialty`, `CamelAwsTranscribeType` | in | Medical jobs. |
| `CamelAwsTranscribeCallAnalyticsJobName`, `CamelAwsTranscribeDataAccessRoleArn`, `CamelAwsTranscribeOutputLocation`, `CamelAwsTranscribeChannelDefinitions`, `CamelAwsTranscribeCategoryName`, `CamelAwsTranscribeRules` | in | Call analytics. |
| `CamelAwsTranscribeVocabularyName`, `CamelAwsTranscribePhrases`, `CamelAwsTranscribeVocabularyFileUri`, `CamelAwsTranscribeNameContains`, `CamelAwsTranscribeStateEquals` | in | Vocabularies. |
| `CamelAwsTranscribeLanguageModelName`, `CamelAwsTranscribeBaseModelName`, `CamelAwsTranscribeInputDataS3Uri` | in | Language models. |
| `CamelAwsTranscribeResourceArn`, `CamelAwsTranscribeTags`, `CamelAwsTranscribeTagKeys` | in | Tags. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e TRANSCRIPT_BUCKET \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/transcribe.camel.yaml
```

## Common questions

### How do I know when the transcript is ready?

Poll `getTranscriptionJob` with the job name until the status is `COMPLETED` (for example from a
timer route), or let S3 notify you when the transcript object appears in the output bucket.

### Can it transcribe a live audio stream?

No. Streaming transcription is a separate WebSocket / HTTP/2 API that this component does not cover;
it works with recorded media in S3.

## Related

- [AWS Polly](aws2-polly.md), [AWS Comprehend](aws2-comprehend.md), [AWS S3](aws2-s3.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Transcribe](https://camel.apache.org/components/next/aws2-transcribe-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
