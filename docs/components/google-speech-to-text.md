# Google Speech-to-Text component

> The `google-speech-to-text` component transcribes audio to text with the Google Cloud Speech-to-Text v1 API, from audio bytes or a `gs://` file.

**Scheme:** `google-speech-to-text:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** none (Cameleer component)

## When to use it

- Transcribe voice notes, call recordings or voicemail dropped into a folder or bucket.
- Turn spoken commands from an edge device into text, then route on the text.
- Feed transcripts to an LLM for summarization or classification
  ([OpenAI](openai.md), [Vertex AI](google-vertexai.md)).

## URI format

```text
google-speech-to-text://label[?options]
```

The label is free text that names the endpoint; it is not sent to the API.

## Example

Transcribe every `.wav` file in a folder and write the transcript next to it as a `.txt` file:

<!-- example: google-speech-to-text-transcribe-folder -->
```yaml
- route:
    id: transcribe-voice-notes
    from:
      uri: "file:{{env:INBOX_DIR:voice-notes}}?include=.*\\.wav&move=.done"
      steps:
        - to: "google-speech-to-text://notes?languageCode={{env:LANGUAGE:en-US}}&sampleRateHertz=16000&enableAutomaticPunctuation=true"
        - log: "transcribed ${file:name} (confidence ${header.CamelGoogleSpeechToTextConfidence})"
        - setHeader:
            name: CamelFileName
            simple: "${file:onlyname.noext}.txt"
        - to: "file:{{env:OUT_DIR:transcripts}}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("file:voice-notes?include=.*\\.wav")
        .to("google-speech-to-text://notes?sampleRateHertz=16000")
        .log("${body}");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `operation` | `recognize` | `recognize`, `longRunningRecognize`, `getOperation`. Header `CamelGoogleSpeechToTextOperation`. |
| `encoding` | `LINEAR16` | Audio encoding (`LINEAR16`, `FLAC`, `MULAW`, `OGG_OPUS`, ...). |
| `sampleRateHertz` | | Sample rate of the audio. |
| `languageCode` | `en-US` | Language of the speech. |
| `model` | | Recognition model, for example `latest_long` or `phone_call`. |
| `enableAutomaticPunctuation` | `false` | Add punctuation to the transcript. |

Credentials are shared by all Google components: see [credentials](google-storage.md#credentials).

### Operations

- `recognize`: the body is the audio bytes, or a text `gs://bucket/file` URI. Body = the best
  transcript (results joined with spaces).
- `longRunningRecognize`: starts an asynchronous recognition (for audio longer than a minute);
  body and `CamelGoogleSpeechToTextOperationName` = the operation name.
- `getOperation`: checks the operation in `CamelGoogleSpeechToTextOperationName`; sets
  `CamelGoogleSpeechToTextDone`, and the body is the transcript once done.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGoogleSpeechToTextOperation` | in | Operation. |
| `CamelGoogleSpeechToTextConfidence` | out | Confidence of the first result. |
| `CamelGoogleSpeechToTextResults` | out | All results returned by the API. |
| `CamelGoogleSpeechToTextOperationName` | in / out | Long-running operation name. |
| `CamelGoogleSpeechToTextDone` | out | `getOperation`: whether the operation finished. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" -v "$PWD/voice-notes:/in" -v "$PWD/transcripts:/out" \
  -v "$HOME/keys/sa.json:/keys/sa.json:ro" -e GOOGLE_APPLICATION_CREDENTIALS=/keys/sa.json \
  -e INBOX_DIR=/in -e OUT_DIR=/out \
  pmsudhi/cameleer run /routes/transcribe-voice-notes.camel.yaml
```

## Common questions

### How do I transcribe a file stored in Cloud Storage?
Send its URI as the body text, for example `gs://my-bucket/call.wav`.

### How do I transcribe long recordings?
Use `operation=longRunningRecognize`, then poll with `operation=getOperation` (for example from a
timer route) until `CamelGoogleSpeechToTextDone` is true.

### Is streaming recognition supported?
No. Only the `recognize` and `longrunningrecognize` REST calls are implemented.

## Related

- [Google Text-to-Speech](google-text-to-speech.md), [Google Vertex AI](google-vertexai.md), [AWS Transcribe](aws2-transcribe.md)
- [AI components guide](../guides/ai-components.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
