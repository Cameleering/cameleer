# Slack component

> The `slack` component posts messages to Slack channels (incoming webhook or the `chat.postMessage` Web API) and reads new channel messages by polling `conversations.history`.

**Scheme:** `slack:` · **Crate:** `cameleer-saas` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Slack](https://camel.apache.org/components/next/slack-component.html)

## When to use it

- Post alerts, deployment notices or daily reports to a channel.
- Let an AI agent publish its summaries or answers to a team channel.
- React to messages people write in a channel (a support bot, a "deploy" keyword, a triage queue).
- Mirror a channel into another system (a ticket tracker, a database, a search index).

## URI format

```text
slack:#channel[?options]
slack://channel[?options]
```

The channel is a name (`#ops`, `general`) or a channel id (`C0123456`). The producer needs
either `webhookUrl` or `token`; the consumer always needs `token`.

## Example

Post a message with an incoming webhook. A text body becomes the message `text`; `username`
and `iconEmoji` are added to the webhook payload.

<!-- example: slack-webhook-post -->
```yaml
- route:
    id: deploy-notice
    from:
      uri: "timer:deploy-notice?repeatCount=1"
      steps:
        - setBody:
            constant: "Deployment of checkout v42 finished"
        - to: "slack:#ops?webhookUrl={{env:SLACK_WEBHOOK_URL}}&username=cameleer&iconEmoji=:rocket:"
```

An AI agent that summarises an incident report with an LLM and posts the summary through the
Web API (`chat.postMessage` with a bot token). The Web API response is kept in the
`CamelSlackResponse` header.

<!-- example: slack-agent-summary -->
```yaml
- route:
    id: incident-summary-to-slack
    from:
      uri: "direct:incident-report"
      steps:
        - to: "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&apiKey={{env:OPENAI_API_KEY}}&temperature=0&systemMessage=Summarise this incident report in two sentences for the on-call channel."
        - setBody:
            simple: ":memo: ${body}"
        - to: "slack:#general?token={{env:SLACK_TOKEN}}&serverUrl={{env:SLACK_API_URL:https://slack.com}}"
        - log: "posted to Slack at ts ${header.CamelSlackResponse[ts]}"
```

Read new messages from `#general`. Each message becomes one exchange whose body is the Slack
message object (`text`, `user`, `ts`, ...). The first poll delivers the latest `maxResults`
messages; later polls deliver only newer ones.

<!-- example: slack-read-channel -->
```yaml
- route:
    id: read-general
    from:
      uri: "slack://general?token={{env:SLACK_TOKEN}}&serverUrl={{env:SLACK_API_URL:https://slack.com}}&maxResults=20&naturalOrder=true&delay=5000"
      steps:
        - filter:
            simple: "${body[text]} contains 'deploy'"
            steps:
              - setBody:
                  simple: "${body[user]} asked: ${body[text]}"
              - to: "log:slack-deploy-requests"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:deploy-notice?repeatCount=1")
        .set_body(constant("Deployment of checkout v42 finished"))
        .to("slack:#ops?webhookUrl={{env:SLACK_WEBHOOK_URL}}&username=cameleer&iconEmoji=:rocket:");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `webhookUrl` | | Incoming webhook URL. When set, the producer posts to the webhook. |
| `token` | | Bot or user token (`xoxb-...`). Producer: `chat.postMessage`. Required by the consumer. |
| `username` | | Display name added to webhook payloads. |
| `iconUrl` | | Icon URL added to webhook payloads (`icon_url`). |
| `iconEmoji` | | Icon emoji added to webhook payloads (`icon_emoji`), e.g. `:rocket:`. |
| `maxResults` | `10` | Consumer: messages fetched per poll (`limit`). |
| `naturalOrder` | `false` | Consumer: deliver the oldest message first (Slack returns newest first). |
| `conversationType` | `PUBLIC_CHANNEL` | Consumer: channel type used to resolve the name: `PUBLIC_CHANNEL`, `PRIVATE_CHANNEL`, `MPIM`, `IM`. |
| `serverUrl` | `https://slack.com` | Web API base URL (Cameleer extension: a proxy, a gateway or a test server). |
| `delay`, `initialDelay`, ... | `delay=10000` | Consumer: the standard scheduled-poll options. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelSlackResponse` | out (producer) | The Web API response of `chat.postMessage` (`ok`, `channel`, `ts`). Not set for webhooks. |

The message body decides the payload: a text body is the message `text`; a map body is sent as
the whole payload (`blocks`, `attachments`, `thread_ts`, ...; `channel` is added when missing).

## Run it with Docker

```sh
export SLACK_WEBHOOK_URL=https://hooks.slack.com/services/T000/B000/XXXX
docker run --rm -e SLACK_WEBHOOK_URL -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/deploy-notice.camel.yaml
```

## Common questions

### Webhook or token: which one should I use?

A webhook is the simplest: one URL, one channel, no scopes. Use a bot token (`chat:write`
scope, plus `channels:history` and `channels:read` for the consumer) when you need to post to
several channels, read messages, or get the message `ts` back for threads.

### How do I send blocks or reply in a thread?

Make the body a map, for example with `unmarshal: json` on a JSON string such as
`{"text":"fallback","thread_ts":"1700000001.000100","blocks":[...]}`. Map bodies are sent as the
payload unchanged.

### What happens when Slack rejects the message?

With a token, a response with `"ok": false` fails the exchange with an `ApiException` whose
message is the Slack error (`Slack: channel_not_found`). With a webhook, a non-2xx response fails
with `HttpOperationFailedException`. Use `onException` or `errorHandler` to retry.

### Is Slack's Socket Mode or Events API supported?

No. The consumer polls `conversations.history`. For push delivery, expose an HTTP endpoint with
[platform-http](platform-http.md) and point a Slack event subscription at it.

## Related

- [openai](openai.md) for LLM summaries before posting, and the [AI components guide](../guides/ai-components.md)
- [jira](jira.md), [github](github.md), [telegram](telegram.md)
- [Filter EIP](../guides/eips.md)
- [Apache Camel Slack component](https://camel.apache.org/components/next/slack-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
