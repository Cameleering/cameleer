# Google Mail Stream component

> The `google-mail-stream` component polls a Gmail mailbox and emits each new message that matches a Gmail search query, then marks it as read.

**Scheme:** `google-mail-stream:` · **Crate:** `cameleer-cloud` ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Google Mail Stream](https://camel.apache.org/components/next/google-mail-stream-component.html)

## When to use it

- Triage an inbox with an LLM: summarize or classify each new e-mail and route it to the right
  team, ticket system or chat channel.
- Turn e-mails sent to a shared address (orders@, invoices@) into messages on a queue.
- Extract data from automated e-mails (alerts, receipts) without running an IMAP client.

## URI format

```text
google-mail-stream://index[?options]
```

The path is a free name (Camel uses `index`); it is not used.

## Example

Every minute, take the unread messages of the authenticated user, ask an LLM for a one-line
summary and a label, and log the result. The route authenticates as the user with an OAuth
refresh token. The OpenAI endpoint works with any OpenAI-compatible server (set
`OPENAI_BASE_URL`, for example to an Ollama or vLLM server).

<!-- example: google-mail-stream-llm-triage -->
```yaml
- route:
    id: inbox-triage
    from:
      uri: "google-mail-stream://index?maxResults=20&clientId={{env:GOOGLE_CLIENT_ID}}&clientSecret=RAW({{env:GOOGLE_CLIENT_SECRET}})&refreshToken=RAW({{env:GOOGLE_REFRESH_TOKEN}})&delay=60000"
      steps:
        - setHeader:
            name: subject
            simple: "${header.CamelGoogleMailStreamSubject}"
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&temperature=0&systemMessage=Summarize this email in one sentence, then label it URGENT, ACTION or FYI."
        - log: "${header.subject}: ${body}"
        - to: "log:inbox-triage"
```

The body of each exchange is the plain-text part of the message (the raw RFC 822 bytes with
`raw=true`). After the route processes a message successfully, the consumer removes its
`UNREAD` label, so the default query `is:unread` does not return it again. If processing fails,
the message stays unread and is not emitted again by this consumer while it keeps running.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("google-mail-stream://index?maxResults=20&clientId={{env:GOOGLE_CLIENT_ID}}&clientSecret=RAW({{env:GOOGLE_CLIENT_SECRET}})&refreshToken=RAW({{env:GOOGLE_REFRESH_TOKEN}})&delay=60000")
        .set_header("subject", simple("${header.CamelGoogleMailStreamSubject}"))
        .to("openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&temperature=0&systemMessage=Summarize this email in one sentence, then label it URGENT, ACTION or FYI.")
        .log("${header.subject}: ${body}")
        .to("log:inbox-triage");
})?;
```

## How it polls

Each poll lists up to `maxResults` messages matching `query` and `labels`, fetches each one and
emits them oldest first. Messages already emitted are remembered (in memory) while they still
match the query, so a message is not emitted twice even with `markAsRead=false`. After a
restart, messages that are still unread are emitted again.

## Options

| option | default | description |
|---|---|---|
| `query` | `is:unread` | Gmail search query, for example `is:unread label:invoices`. |
| `labels` | | Comma separated label ids that messages must have (for example `INBOX`). |
| `maxResults` | `10` | Maximum messages per poll. |
| `markAsRead` | `true` | Remove the `UNREAD` label after a message is processed successfully. |
| `raw` | `false` | Body = the raw RFC 822 message bytes instead of the decoded text part. |
| `userId` | `me` | Mailbox to read. |
| `delay` | `500` | Milliseconds between polls. Scheduled poll options such as `initialDelay` (default `1000`), `repeatCount` and `backoffMultiplier` also apply. |
| `clientId`, `clientSecret`, `refreshToken` | | OAuth user credentials (refresh-token flow). Wrap secrets in `RAW(...)`. |
| `serviceAccountKey` | `GOOGLE_APPLICATION_CREDENTIALS` | Service account JSON key; needs `delegate`. |
| `delegate` (alias `subject`) | | User to impersonate with domain-wide delegation. |
| `accessToken` | | A fixed OAuth access token. |
| `tokenServerUrl` | `https://oauth2.googleapis.com/token` | Token endpoint for the refresh-token flow (Cameleer). |
| `scopes` | `https://mail.google.com/` | Comma separated OAuth scopes. |
| `uriEndpointOverride` + `overrideEndpoint=true` | | Base URL of an emulator or proxy. |

`useMetadataServer`, `authenticate`, `proxyHost`, `proxyPort` and `trustAllCertificates` work as
on the [google-sheets](google-sheets.md#options) page.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGoogleMailStreamId` | out | Message id. |
| `CamelGoogleMailStreamThreadId` | out | Thread id. |
| `CamelGoogleMailStreamSubject` | out | `Subject` header. |
| `CamelGoogleMailStreamFrom` | out | `From` header. |
| `CamelGoogleMailStreamTo`, `CamelGoogleMailStreamCc`, `CamelGoogleMailStreamBcc` | out | Recipient headers. |
| `CamelGoogleMailStreamReceivedDate` | out | Gmail `internalDate` (milliseconds since the epoch). |
| `CamelGoogleMailStreamLabels` | out | List of label ids. |

## Run it with Docker

```sh
docker run --rm \
  -e GOOGLE_CLIENT_ID -e GOOGLE_CLIENT_SECRET -e GOOGLE_REFRESH_TOKEN \
  -e OPENAI_API_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/inbox-triage.camel.yaml
```

Get a refresh token once with the OAuth consent flow for the `https://mail.google.com/` scope
(for example with the OAuth 2.0 Playground and your own client id).

## Common questions

### How do I process new Gmail messages in a route?
Use `from: google-mail-stream://index` with credentials. Each new unread message arrives with
its text as the body and its subject and sender in `CamelGoogleMailStream*` headers.

### How do I read only one label?
Set `query=label:my-label is:unread`, or `labels=<labelId>` with a label id from
`google-mail://labels/list`.

### How do I keep messages unread?
Set `markAsRead=false`. The consumer still does not emit the same message twice while it runs.

### Does it use Gmail push notifications?
No, it polls. For push, call `google-mail://users/watch` with a Pub/Sub topic and consume that
topic with [google-pubsub](google-pubsub.md).

## Related

- [google-mail](google-mail.md): send and manage messages.
- [openai](openai.md) and the [AI components guide](../guides/ai-components.md).
- [Apache Camel Google Mail Stream component](https://camel.apache.org/components/next/google-mail-stream-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
