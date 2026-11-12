# AWS Polly component

> The `aws2-polly` component turns text into speech with Amazon Polly and manages lexicons and long speech synthesis tasks.

**Scheme:** `aws2-polly:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Polly](https://camel.apache.org/components/next/aws2-polly-component.html)

## When to use it

- Generate audio announcements (station displays, IVR prompts, kiosk messages) from text.
- Give a voice to an AI assistant's answers on a device.
- Convert long articles to audio files in S3 (`startSpeechSynthesisTask`).

## URI format

```text
aws2-polly://label[?options]
```

## Example

Turn announcements from a queue into MP3 files in S3:

<!-- example: aws2-polly-announcements -->
```yaml
- route:
    id: announcements-to-audio
    from:
      uri: "aws2-sqs://announcements?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - setHeader:
            name: CamelAwsS3Key
            simple: "audio/${header.CamelAwsSqsMessageId}.mp3"
        - to: "aws2-polly://speech?region={{env:AWS_REGION:us-east-1}}&operation=synthesizeSpeech&voiceId=Joanna&outputFormat=mp3"
        - to: "aws2-s3://{{env:AUDIO_BUCKET:announcement-audio}}?region={{env:AWS_REGION:us-east-1}}&autoCreateBucket=true"
```

The body text is spoken; the body after the call is the audio bytes and `CamelAwsPollyContentType`
is set (for example `audio/mpeg`).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://announcements?region={{env:AWS_REGION:us-east-1}}")
        .set_header("CamelAwsS3Key", simple("audio/${header.CamelAwsSqsMessageId}.mp3"))
        .to("aws2-polly://speech?region={{env:AWS_REGION:us-east-1}}&operation=synthesizeSpeech&voiceId=Joanna&outputFormat=mp3")
        .to("aws2-s3://{{env:AUDIO_BUCKET:announcement-audio}}?region={{env:AWS_REGION:us-east-1}}&autoCreateBucket=true");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `synthesizeSpeech` | `synthesizeSpeech`, `describeVoices`, `listLexicons`, `getLexicon`, `putLexicon`, `deleteLexicon`, `startSpeechSynthesisTask`, `getSpeechSynthesisTask`. |
| `voiceId` | | Voice, for example `Joanna`. |
| `outputFormat` | `mp3` | `mp3`, `ogg_vorbis`, `pcm` or `json` (speech marks). |
| `textType` | `text` | `text` or `ssml`. |
| `engine` | | `standard`, `neural`, `long-form` or `generative`. |
| `sampleRate` | | Sample rate in Hz. |
| `languageCode` | | Language of bilingual voices. |
| `lexiconNames` | | Lexicons to apply. |

Each option can also be given per message with the matching `CamelAwsPolly*` header.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsPollyOperation` | in | Overrides `operation`. |
| `CamelAwsPollyVoiceId`, `CamelAwsPollyOutputFormat`, `CamelAwsPollyTextType`, `CamelAwsPollyEngine`, `CamelAwsPollySampleRate`, `CamelAwsPollyLanguageCode`, `CamelAwsPollyLexiconNames` | in | Override the options. |
| `CamelAwsPollyLexiconName` | in | Lexicon of the lexicon operations (`putLexicon`: the body is the PLS content). |
| `CamelAwsPollyS3Bucket`, `CamelAwsPollyS3KeyPrefix` | in | Output of `startSpeechSynthesisTask`. |
| `CamelAwsPollyTaskId` | in/out | Task of the synthesis task operations. |
| `CamelAwsPollyContentType` | out | Content type of the audio. |
| `CamelAwsPollyRequestCharacters` | out | Characters billed. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e AUDIO_BUCKET \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/announcements.camel.yaml
```

## Common questions

### How do I control pronunciation and pauses?

Send SSML (`<speak>...<break time="1s"/>...</speak>`) with `textType=ssml`, or upload a lexicon with
`putLexicon` and reference it in `lexiconNames`.

### How do I convert a very long text?

Use `startSpeechSynthesisTask` with `CamelAwsPollyS3Bucket`: Polly writes the audio to S3 and you
poll the task with `getSpeechSynthesisTask`.

## Related

- [AWS Transcribe](aws2-transcribe.md), [AWS Translate](aws2-translate.md), [AWS S3](aws2-s3.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Polly](https://camel.apache.org/components/next/aws2-polly-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
