# Telegram component

> The `telegram` component sends messages, photos and documents through a Telegram bot and receives the messages people send to the bot (long polling with `getUpdates`, or a webhook).

**Scheme:** `telegram:` · **Crate:** `cameleer-saas` · **Cargo feature:** [`saas`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Telegram](https://camel.apache.org/components/next/telegram-component.html)

## When to use it

- Build a chat bot: receive a message, answer it with an LLM or a lookup, reply in the same chat.
- Push alerts from monitoring, IoT sensors or edge devices to a phone, without running a server.
- Send reports, photos or documents to a group.
- Turn Telegram into a command channel for an AI agent ("status", "restart worker 3").

## URI format

```text
telegram:bots[?options]
```

`bots` is the only endpoint type. `authorizationToken` (from BotFather) is required.

## Example

An AI chat bot. The consumer delivers each incoming message with the `CamelTelegramChatId`
header set, the LLM writes the answer, and the producer replies to the same chat because the
header is still there.

<!-- example: telegram-ai-bot -->
```yaml
- route:
    id: telegram-ai-bot
    from:
      uri: "telegram:bots?authorizationToken={{env:TELEGRAM_TOKEN}}&baseUri={{env:TELEGRAM_API_URL:https://api.telegram.org}}&timeout=30"
      steps:
        - setBody:
            simple: "${body[text]}"
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&apiKey={{env:OPENAI_API_KEY}}&systemMessage=You are a helpful assistant. Answer in at most three sentences."
        - to: "telegram:bots?authorizationToken={{env:TELEGRAM_TOKEN}}&baseUri={{env:TELEGRAM_API_URL:https://api.telegram.org}}"
```

Send an alert to a fixed chat with HTML formatting:

<!-- example: telegram-send-alert -->
```yaml
- route:
    id: telegram-alert
    from:
      uri: "timer:disk-check?repeatCount=1"
      steps:
        - setBody:
            constant: "<b>edge-gw-07</b>: disk 91% full"
        - to: "telegram:bots?authorizationToken={{env:TELEGRAM_TOKEN}}&chatId={{env:TELEGRAM_CHAT_ID}}&parseMode=HTML&baseUri={{env:TELEGRAM_API_URL:https://api.telegram.org}}"
        - log: "sent Telegram message ${header.CamelTelegramMessageId}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:disk-check?repeatCount=1")
        .set_body(constant("<b>edge-gw-07</b>: disk 91% full"))
        .to("telegram:bots?authorizationToken={{env:TELEGRAM_TOKEN}}&chatId={{env:TELEGRAM_CHAT_ID}}&parseMode=HTML");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `authorizationToken` | | Bot token (required). |
| `chatId` | | Default chat id for the producer (the `CamelTelegramChatId` header wins). |
| `parseMode` | | `HTML`, `MARKDOWN` or `MarkdownV2` for text messages. |
| `timeout` | `30` | Consumer: long-poll timeout in seconds for `getUpdates`. |
| `limit` | `100` | Consumer: maximum updates per poll. |
| `baseUri` | `https://api.telegram.org` | Bot API base URL (a local Bot API server, a proxy or a test server). |
| `delay`, `initialDelay`, ... | `delay=500` | Consumer: the standard scheduled-poll options. |

`proxyHost`, `proxyPort`, `proxyType`, `client` and `bufferSize` are accepted for Camel
compatibility and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelTelegramChatId` | in / out | Producer: target chat. Consumer: chat of the incoming message. |
| `CamelTelegramParseMode` | in | Overrides `parseMode`. |
| `CamelTelegramMediaType` | in | With a byte body: `PHOTO_JPG`, `PHOTO_PNG`, `AUDIO`, `VIDEO` or `DOCUMENT` (multipart upload with `sendPhoto`, `sendAudio`, `sendVideo`, `sendDocument`). |
| `CamelTelegramMediaTitleCaption` | in | Caption of an uploaded file. |
| `CamelTelegramMessageId` | out | Id of the sent message. |

Bodies: a text body is sent with `sendMessage`; a byte body with `CamelTelegramMediaType` is
uploaded; a map body is sent as the API parameters (`sendPhoto` when it has `photo`,
`sendDocument` when it has `document`, otherwise `sendMessage`). For InOut exchanges the body
becomes the API `result`. Consumer bodies are the message object (`text`, `chat`, `from`, ...),
or the whole update for other update kinds.

## Run it with Docker

```sh
export TELEGRAM_TOKEN=123456:ABC... OPENAI_API_KEY=sk-...
docker run --rm -e TELEGRAM_TOKEN -e OPENAI_API_KEY -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/telegram-ai-bot.camel.yaml
```

The bot only needs outbound HTTPS, so it runs behind NAT on a Raspberry Pi or an edge box.

## Common questions

### How do I find my chat id?

Send any message to the bot and run the consumer route with a `log` step: the
`CamelTelegramChatId` header of the incoming message is the chat id.

### Can I use a webhook instead of polling?

Yes: consume from `webhook:telegram:bots?authorizationToken=...&webhookExternalUrl=https://bot.example.com`
(the `webhook` component). It listens with `platform-http`, registers the public URL with
`setWebhook` on start and removes it with `deleteWebhook` on stop. Telegram does not allow
`getUpdates` while a webhook is set.

### How do I send a photo?

Set the body to the image bytes (for example from a `file:` consumer) and the
`CamelTelegramMediaType` header to `PHOTO_JPG` or `PHOTO_PNG`; add
`CamelTelegramMediaTitleCaption` for a caption.

### What happens on errors?

A Bot API answer with `"ok": false` fails the exchange with an `ApiException` carrying the
Telegram description (`Telegram: Bad Request: chat not found`). The bot token is removed from
error messages.

## Related

- [openai](openai.md) and the [AI components guide](../guides/ai-components.md)
- [slack](slack.md), [twilio](twilio.md)
- [Apache Camel Telegram component](https://camel.apache.org/components/next/telegram-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
