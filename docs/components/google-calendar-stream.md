# Google Calendar Stream component

> The `google-calendar-stream` component polls a Google Calendar and emits new and changed events, optionally using incremental sync tokens so deleted events are reported too.

**Scheme:** `google-calendar-stream:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Google Calendar Stream](https://camel.apache.org/components/next/google-calendar-stream-component.html)

## When to use it

- Prepare for meetings automatically: when an event is created, have an agent collect the
  attendees' recent tickets or e-mails and post a briefing.
- Mirror a calendar into another system (a room display, a CRM, a database).
- React to cancellations with `syncFlow=true`, which reports deleted events.

## URI format

```text
google-calendar-stream://index[?options]
```

The path is a free name (Camel uses `index`); it is not used.

## Example

Every minute, emit the events of the user's primary calendar that start in the future and were
created or changed since the previous poll.

<!-- example: google-calendar-stream-new-events -->
```yaml
- route:
    id: new-meetings
    from:
      uri: "google-calendar-stream://index?calendarId={{env:CALENDAR_ID:primary}}&considerLastUpdate=true&delay=60000&delegate={{env:GOOGLE_USER}}"
      steps:
        - log: "event ${header.CamelGoogleCalendarStreamEventId}: ${body[summary]}"
        - marshal:
            json: {}
        - to: "log:meetings"
```

The body of each exchange is the Calendar event resource (`id`, `summary`, `start`, `end`,
`attendees`, `updated`, ...).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("google-calendar-stream://index?calendarId={{env:CALENDAR_ID:primary}}&considerLastUpdate=true&delay=60000&delegate={{env:GOOGLE_USER}}")
        .log("event ${header.CamelGoogleCalendarStreamEventId}: ${body[summary]}")
        .marshal("json")
        .to("log:meetings");
})?;
```

## How it polls

**Default mode.** Each poll calls `events.list` with `singleEvents=true`, `orderBy=startTime`
and `maxResults`. With `consumeFromNow=true` (default) only events starting after the consumer
started are listed (`timeMin`). With `considerLastUpdate=true` only events updated since the
previous poll are asked for (`updatedMin`). An event is emitted when it is new or its `updated`
time changed, so an unchanged event is never emitted twice. Deleted events are not reported in
this mode.

**Sync mode (`syncFlow=true`).** The first poll does a full sync and keeps the
`nextSyncToken`; its events are emitted only with `consumeFromNow=false`. Every later poll asks
for the changes since the token and emits them, cancelled (deleted) events included, with the
`CamelGoogleCalendarStreamEventStatus` header. When Google answers `410 Gone` (expired token), the
consumer runs a full sync again and emits only events it has not delivered before.

State (seen events, sync token) is kept in memory; after a restart the consumer starts over.

## Options

| option | default | description |
|---|---|---|
| `calendarId` | `primary` | Calendar to watch. |
| `query` | | Free-text search (`q`) in default mode. |
| `maxResults` | `10` | Maximum events per poll in default mode. |
| `consumeFromNow` | `true` | Only events starting after the consumer started (default mode); in sync mode, skip the events of the initial full sync. |
| `considerLastUpdate` | `false` | Ask only for events updated since the previous poll. |
| `syncFlow` | `false` | Use incremental sync tokens; reports changes and deletions. |
| `delay` | `500` | Milliseconds between polls. Scheduled poll options such as `initialDelay` (default `1000`) also apply. |
| `delegate` (alias `subject`) | | User to impersonate with domain-wide delegation. |
| `serviceAccountKey` | `GOOGLE_APPLICATION_CREDENTIALS` | Service account JSON key. |
| `clientId`, `clientSecret`, `refreshToken` | | OAuth user credentials (refresh-token flow). |
| `accessToken` | | A fixed OAuth access token. |
| `scopes` | `https://www.googleapis.com/auth/calendar` | Comma separated OAuth scopes. |
| `uriEndpointOverride` + `overrideEndpoint=true` | | Base URL of an emulator or proxy. |

`tokenServerUrl`, `useMetadataServer`, `authenticate`, `proxyHost`, `proxyPort` and
`trustAllCertificates` work as on the [google-sheets](google-sheets.md#options) page.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGoogleCalendarStreamEventId` | out | Event id. |
| `CamelGoogleCalendarStreamEventStatus` | out | With `syncFlow=true`: `confirmed`, `tentative` or `cancelled`. |

## Run it with Docker

```sh
docker run --rm \
  -e GOOGLE_USER=ada@example.com \
  -e GOOGLE_APPLICATION_CREDENTIALS=/secrets/key.json \
  -v "$PWD/key.json:/secrets/key.json:ro" \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/new-meetings.camel.yaml
```

## Common questions

### How do I get notified when a Google Calendar event is created or changed?
Consume `google-calendar-stream://index?calendarId=...&considerLastUpdate=true`. Each new or
changed event arrives as one exchange.

### How do I detect cancelled events?
Set `syncFlow=true` and check `CamelGoogleCalendarStreamEventStatus` for `cancelled`.

### Why are past events not emitted?
`consumeFromNow=true` (default) lists only events that start after the consumer started. Set it
to `false` to include them.

## Related

- [google-calendar](google-calendar.md): create and manage events.
- [Apache Camel Google Calendar Stream component](https://camel.apache.org/components/next/google-calendar-stream-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
