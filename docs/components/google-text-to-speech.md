# Google Text-to-Speech component

> The `google-text-to-speech` component turns text or SSML into speech audio (MP3, OGG Opus, WAV) with the Google Cloud Text-to-Speech v1 API.

**Scheme:** `google-text-to-speech:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** none (Cameleer component)

## When to use it

- Generate audio announcements, alerts or prompts for IVR and kiosks.
- Read out an AI agent's answer: chain an LLM call and this component.
- Produce spoken versions of short articles or notifications in a batch job.

## URI format

```text
google-text-to-speech://label[?options]
```

The label is free text that names the endpoint; it is not sent to the API.

## Example

Turn every `.txt` file in a folder into an MP3 file with a chosen voice:

<!-- example: google-text-to-speech-folder-to-mp3 -->
```yaml
- route:
    id: text-to-mp3
    from:
      uri: "file:{{env:INBOX_DIR:announcements}}?include=.*\\.txt&move=.done"
      steps:
        - to: "google-text-to-speech://announcer?voiceName={{env:VOICE:en-US-Neural2-C}}&audioEncoding=MP3&speakingRate=0.95"
        - setHeader:
            name: CamelFileName
            simple: "${file:onlyname.noext}.mp3"
        - to: "file:{{env:OUT_DIR:audio}}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("file:announcements?include=.*\\.txt")
        .to("google-text-to-speech://announcer?voiceName=en-US-Neural2-C")
        .set_header("CamelFileName", simple("${file:onlyname.noext}.mp3"))
        .to("file:audio");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `operation` | `synthesize` | `synthesize` or `listVoices`. Header `CamelGoogleTextToSpeechOperation`. |
| `languageCode` | `en-US` | Voice language; also the filter of `listVoices`. |
| `voiceName` | | Voice, for example `en-US-Neural2-C`. |
| `ssmlGender` | | `MALE`, `FEMALE` or `NEUTRAL`. |
| `audioEncoding` | `MP3` | `MP3`, `OGG_OPUS`, `LINEAR16`, `MULAW`, `ALAW`. |
| `speakingRate` | | Speed, 0.25 to 4.0 (1.0 is normal). |
| `pitch` | | Pitch in semitones, -20.0 to 20.0. |
| `textType` | | `ssml` to send the body as SSML. A body starting with `<speak` is SSML anyway. |

Credentials are shared by all Google components: see [credentials](google-storage.md#credentials).

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGoogleTextToSpeechOperation` | in | Operation. |
| `CamelGoogleTextToSpeechAudioEncoding` | out | Encoding of the returned audio. |

`synthesize` returns the audio bytes as the body; `listVoices` returns the list of voices.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" -v "$PWD/announcements:/in" -v "$PWD/audio:/out" \
  -v "$HOME/keys/sa.json:/keys/sa.json:ro" -e GOOGLE_APPLICATION_CREDENTIALS=/keys/sa.json \
  -e INBOX_DIR=/in -e OUT_DIR=/out \
  pmsudhi/cameleer run /routes/text-to-mp3.camel.yaml
```

## Common questions

### How do I use SSML?
Send a body that starts with `<speak>`, or set `textType=ssml`.

### Which voices can I use?
Call `operation=listVoices` (optionally with `languageCode`) and pick a `name`.

### Can it stream audio?
No. Each call returns the complete audio file.

## Related

- [Google Speech-to-Text](google-speech-to-text.md), [AWS Polly](aws2-polly.md)
- [AI components guide](../guides/ai-components.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
