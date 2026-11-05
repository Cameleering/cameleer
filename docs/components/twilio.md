# Twilio component

> The `twilio` component calls the Twilio REST API (2010-04-01): send and list SMS and MMS messages, place calls, manage phone numbers, queues, recordings and the other resources Camel's `twilio` component exposes.

**Scheme:** `twilio:` · **Crate:** `cameleer-saas` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Twilio](https://camel.apache.org/components/next/twilio-component.html)

## When to use it

- Send an SMS when an alert fires, an order ships or an IoT sensor crosses a threshold.
- Let an AI agent text a short summary to the on-call phone.
- Read the message log, or fetch a message's delivery status.
- Automate calls, queues, phone numbers or usage reports.

## URI format

```text
twilio:api/method[?options]
```

`api` is a Twilio resource in Camel's naming (`message`, `call`, `queue`, `incoming-phone-number`,
`recording`, `usage-record-today`, ...). `method` is `create`, `read`, `fetch`, `update` or
`delete` (or Camel's `creator`, `reader`, `fetcher`, `updater`, `deleter`). Unsupported
combinations fail when the endpoint is created.

## Example

Send an SMS. For `message/create` the body is the message text unless a `body` or `mediaUrl`
parameter is set. After the call the body is the created message (`sid`, `status`, ...).

<!-- example: twilio-send-sms -->
```yaml
- route:
    id: sms-alert
    from:
      uri: "direct:sms-alert"
      steps:
        - setBody:
            simple: "ALERT ${header.device}: temperature ${header.celsius} C"
        - to: "twilio:message/create?from=RAW({{env:TWILIO_FROM:+15005550006}})&to=RAW({{env:ONCALL_PHONE:+15005550001}})&username={{env:TWILIO_ACCOUNT_SID}}&password={{env:TWILIO_AUTH_TOKEN}}&serverUrl={{env:TWILIO_API_URL:https://api.twilio.com}}"
        - log: "sent SMS ${body[sid]}"
```

List the messages sent to a number. `read` returns the resource list as the body; every
parameter that is not a `path*` parameter becomes a query filter (`to` → `To`).

<!-- example: twilio-read-messages -->
```yaml
- route:
    id: sms-log
    from:
      uri: "timer:sms-log?repeatCount=1"
      steps:
        - to: "twilio:message/read?to=RAW({{env:ONCALL_PHONE:+15005550001}})&username={{env:TWILIO_ACCOUNT_SID}}&password={{env:TWILIO_AUTH_TOKEN}}&serverUrl={{env:TWILIO_API_URL:https://api.twilio.com}}"
        - split:
            simple: "${body}"
            steps:
              - to: "log:sms-log"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:sms-alert")
        .set_body(simple("ALERT ${header.device}: temperature ${header.celsius} C"))
        .to("twilio:message/create?from=RAW(+15005550006)&to=RAW(+15005550001)&username={{env:TWILIO_ACCOUNT_SID}}&password={{env:TWILIO_AUTH_TOKEN}}");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `username` | | Account SID (or API key SID), required. |
| `password` | | Auth token (or API key secret), required. |
| `accountSid` | `username` | Account the resources belong to (`Accounts/{accountSid}/...`). |
| `inBody` | | Name of the parameter taken from the message body (default `body` for `message/create`). |
| `serverUrl` | `https://api.twilio.com` | API base URL (Cameleer extension: a proxy or a test server). |
| `path*` | | Resource path parameters: `pathSid` (the instance for `fetch`, `update`, `delete`), `pathAccountSid`, `pathCallSid`, `pathConferenceSid`, `pathQueueSid`, `pathMessageSid`, `pathCountryCode`, ... |
| any other | | Sent to Twilio as a form field (`create`, `update`) or query parameter (`read`), with the Twilio capitalisation: `to` → `To`, `statusCallback` → `StatusCallback`. |

Because every unknown option is an API parameter, a misspelled option is sent to Twilio rather
than rejected when the endpoint is created.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelTwilio.<parameter>` | in | Any API parameter, for example `CamelTwilio.to` or `CamelTwilio.pathSid`. Headers win over endpoint options. List values repeat the field. |

The body after the call: the created, fetched or updated resource as a map; the resource list
for `read`; `true` for `delete`.

## Run it with Docker

```sh
export TWILIO_ACCOUNT_SID=AC... TWILIO_AUTH_TOKEN=... TWILIO_FROM=+1... ONCALL_PHONE=+1...
docker run --rm -e TWILIO_ACCOUNT_SID -e TWILIO_AUTH_TOKEN -e TWILIO_FROM -e ONCALL_PHONE \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/sms-log.camel.yaml
```

## Common questions

### How do I send to a different number per message?

Set the `CamelTwilio.to` header before the endpoint; it overrides the `to` option.

### Why do phone numbers need `RAW(...)`?

In an endpoint URI a `+` is decoded as a space, as in Apache Camel, so `to=+15005550001` would
reach Twilio as ` 15005550001`. Write `to=RAW(+15005550001)` or `to=%2B15005550001`, or set the
number in the `CamelTwilio.to` header, which is not URI-decoded.

### How do I send an MMS?

Add `mediaUrl=https://example.com/chart.png` (or the `CamelTwilio.mediaUrl` header, a list for
several images). Set `inBody=body` if the body should still be the text.

### What happens when Twilio rejects a request?

The exchange fails with `HttpOperationFailedException`; the message contains the HTTP status,
the Twilio error code and message (for example `code: 21211, message: Invalid 'To' Phone Number`).

### Is there a consumer for incoming SMS?

No. Twilio pushes incoming messages with webhooks; receive them with
[platform-http](platform-http.md) and point the number's messaging webhook at that URL.

## Related

- [telegram](telegram.md), [slack](slack.md)
- [Split EIP](../guides/eips.md)
- [Apache Camel Twilio component](https://camel.apache.org/components/next/twilio-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
