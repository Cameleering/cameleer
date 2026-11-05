# Google Mail component

> The `google-mail` component sends, lists, reads, labels and deletes Gmail messages, drafts, labels and threads through the Gmail v1 REST API.

**Scheme:** `google-mail:` · **Crate:** `cameleer-cloud` ·
**Consumer:** no (see [google-mail-stream](google-mail-stream.md)) · **Producer:** yes · **Apache Camel:** [Google Mail](https://camel.apache.org/components/next/google-mail-component.html)

## When to use it

- Send notification or report e-mails from a Google Workspace mailbox without an SMTP relay.
- Let an AI agent search a mailbox (`messages/list` with a Gmail query), read a message
  (`messages/get`) and file it (`messages/modify` with labels).
- Create drafts for a human to review before sending (`drafts/create`).
- Watch a mailbox through Pub/Sub push notifications (`users/watch`).

## URI format

```text
google-mail://apiName/methodName[?options]
```

Core methods: `messages/send`, `messages/list`, `messages/get`, `messages/delete`,
`messages/trash`, `messages/untrash`, `messages/modify`, `labels/list`, `threads/get`.
Also: `drafts/create|list|get|update|send|delete`, `history/list`, `attachments/get`,
`users/getProfile`, `users/watch`, `users/stop`, `labels/create|get|update|delete`,
`threads/list|modify|trash|untrash|delete`. Unknown methods fail when the endpoint is created.

## Example

Once a day, send a short digest from a Workspace mailbox. The service account acts as
`GOOGLE_USER` through domain-wide delegation.

<!-- example: google-mail-daily-digest -->
```yaml
- route:
    id: daily-digest
    from:
      uri: "timer:daily-digest?period=86400000"
      steps:
        - setBody:
            simple: "Good morning. This digest was generated at ${date:now:yyyy-MM-dd HH:mm}."
        - to: "google-mail://messages/send?delegate={{env:GOOGLE_USER}}&to={{env:DIGEST_TO}}&subject=Daily digest"
        - log: "sent Gmail message ${header[CamelGoogleMail.id]}"
```

A text body with the `to` option becomes a plain-text MIME message. A map body
`{to, cc, bcc, from, subject, text, html, replyTo}` gives you full control (with `text` and `html`
it becomes `multipart/alternative`). A text body without `to` is sent as a raw RFC 822 message.
After the call the body is the sent message resource (`id`, `threadId`, `labelIds`).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:daily-digest?period=86400000")
        .set_body(simple("Good morning. This digest was generated at ${date:now:yyyy-MM-dd HH:mm}."))
        .to("google-mail://messages/send?delegate={{env:GOOGLE_USER}}&to={{env:DIGEST_TO}}&subject=Daily digest")
        .log("sent Gmail message ${header[CamelGoogleMail.id]}");
})?;
```

## What the core methods do

| api/method | input | body after the call |
|---|---|---|
| `messages/send` | map body, or text body + `to`/`subject`/`cc`/`bcc`/`from`/`replyTo`, or raw RFC 822 | sent message resource; header `CamelGoogleMail.id` |
| `messages/list` | `q`, `labelIds`, `maxResults`, `includeSpamTrash` | list of `{id, threadId}`; all pages unless `maxResults` is set (then one page) |
| `messages/get` | `id`, `format` (default `full`) | message resource; headers `CamelGoogleMail.text`, `.subject`, `.from`, `.to` |
| `messages/modify` | `id`, `addLabelIds`, `removeLabelIds` | updated message |
| `messages/trash`, `messages/untrash` | `id` | the message |
| `messages/delete` | `id` | unchanged (permanent delete) |
| `labels/list` | | list of labels |
| `threads/get` | `id` | the thread |
| `drafts/create`, `drafts/update` | message map or text body (like `messages/send`), or a `{message}` map | the draft |
| `drafts/send` | `id`, or a content map | the sent message |
| `attachments/get` | `messageId`, `id` | decoded attachment bytes; header `CamelGoogleMail.size` |
| `users/watch` | `topicName` (a Pub/Sub topic), `labelIds` | `{historyId, expiration}`; Gmail then publishes `{emailAddress, historyId}` to the topic |
| `history/list` | `startHistoryId`, `historyTypes`, `labelId`, `maxResults` | history records (all pages) |

Methods that take a request body (`labels/create`, `threads/modify`, ...) read it from the
`CamelGoogleMail.content` header or a map body.

## Options

| option | default | description |
|---|---|---|
| `userId` | `me` | Mailbox to act on (`me` = the authenticated or delegated user). |
| `to`, `cc`, `bcc`, `from`, `replyTo`, `subject` | | Message fields for a text body sent with `messages/send` or `drafts/create`. |
| `id` | | Message, thread, draft or label id. |
| `q` | | Gmail search query for `messages/list`, `threads/list`, `drafts/list` (for example `from:billing@example.com is:unread`). |
| `labelIds` | | Comma separated label ids to filter by. |
| `maxResults` | | Page size; when set, only one page is returned. |
| `includeSpamTrash` | | Include spam and trash in `messages/list`. |
| `format` | `full` | `full`, `metadata`, `minimal` or `raw` for `messages/get` and `drafts/get`. |
| `addLabelIds`, `removeLabelIds` | | Labels to add or remove with `messages/modify`. |
| `messageId`, `startHistoryId`, `historyTypes`, `labelId`, `topicName` | | Parameters of `attachments/get`, `history/list` and `users/watch`. |
| `delegate` (alias `subject`) | | User to impersonate with domain-wide delegation. A service account needs it to reach a mailbox. |
| `scopes` | `https://mail.google.com/` | Comma separated OAuth scopes. |
| `serviceAccountKey` | `GOOGLE_APPLICATION_CREDENTIALS` | Service account JSON key: `file:path`, a plain path or inline JSON (`authorized_user` and `external_account` files also work). |
| `clientId`, `clientSecret`, `refreshToken` | | OAuth user credentials (refresh-token flow) instead of a service account. |
| `accessToken` | | A fixed OAuth access token. |
| `tokenServerUrl` | `https://oauth2.googleapis.com/token` | Token endpoint for the refresh-token flow (Cameleer). |
| `uriEndpointOverride` + `overrideEndpoint=true` | | Base URL of an emulator or proxy. |

`useMetadataServer`, `authenticate`, `proxyHost`, `proxyPort` and `trustAllCertificates` work as
on the [google-sheets](google-sheets.md#options) page. `applicationName`, `inBody` and
`splitResult` are accepted and ignored.

## Message headers

Every option can also be set per message with a `CamelGoogleMail.<option>` header (for example
`CamelGoogleMail.id`, `CamelGoogleMail.q`), which wins over the URI option.

| header | direction | description |
|---|---|---|
| `CamelGoogleMail.content` | in | Request body for methods that take one. |
| `CamelGoogleMail.id` | out | Id of the message sent by `messages/send`. |
| `CamelGoogleMail.text` | out | Decoded plain-text part, after `messages/get`. |
| `CamelGoogleMail.subject`, `CamelGoogleMail.from`, `CamelGoogleMail.to` | out | Decoded headers, after `messages/get`. |
| `CamelGoogleMail.size` | out | Attachment size, after `attachments/get`. |

## Run it with Docker

The service account needs domain-wide delegation for the `https://mail.google.com/` scope
(Google Admin console, Security, API controls).

```sh
docker run --rm \
  -e GOOGLE_USER=reports@example.com -e DIGEST_TO=team@example.com \
  -e GOOGLE_APPLICATION_CREDENTIALS=/secrets/key.json \
  -v "$PWD/key.json:/secrets/key.json:ro" \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/digest.camel.yaml
```

## Common questions

### How do I send an e-mail with Gmail from a route?
Send a text body to `google-mail://messages/send?to=...&subject=...`, or send a map with `to`,
`subject`, `text` and optionally `html`. Authenticate as the sender with a service account plus
`delegate`, or with `clientId`/`clientSecret`/`refreshToken`.

### Why does Gmail return 400 "Precondition check failed" or 403?
A service account has no mailbox of its own. Set `delegate` to a user of your domain and allow
the service account's client id for the Gmail scope in the Admin console.

### How do I read the text of a message?
Call `messages/get` with `CamelGoogleMail.id`; the plain-text part is decoded into the
`CamelGoogleMail.text` header and the body keeps the full resource.

### How do I process new e-mails as they arrive?
Use the [google-mail-stream](google-mail-stream.md) consumer.

## Related

- [google-mail-stream](google-mail-stream.md): consume new messages.
- [google-calendar](google-calendar.md), [google-drive](google-drive.md),
  [google-sheets](google-sheets.md).
- [Apache Camel Google Mail component](https://camel.apache.org/components/next/google-mail-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
