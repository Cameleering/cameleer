# Google Calendar component

> The `google-calendar` component creates, lists, updates and deletes Google Calendar events and manages calendars, ACLs and watch channels through the Calendar v3 REST API.

**Scheme:** `google-calendar:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) ·
**Consumer:** no (see [google-calendar-stream](google-calendar-stream.md)) · **Producer:** yes · **Apache Camel:** [Google Calendar](https://camel.apache.org/components/next/google-calendar-component.html)

## When to use it

- Give an AI scheduling assistant a "book meeting" tool that creates events in a user's calendar.
- Create calendar entries from bookings, maintenance windows or on-call rotations.
- Check availability with `freebusy/query` before proposing a time.
- Sync events from another system with `events/list`, `events/update` and `events/delete`.

## URI format

```text
google-calendar://apiName/methodName[?options]
```

Core methods: `events/list`, `events/insert`, `events/get`, `events/update`, `events/patch`,
`events/delete`, `events/quickAdd`, `calendarList/list`, `calendars/get`.
Also: `acl/list|get|insert|update|delete|watch`, `colors/get`, `freebusy/query`,
`settings/list|get|watch`, `calendars/insert|update|delete|clear`,
`calendarList/get|insert|delete|watch`, `events/instances`, `events/move`, `events/import`,
`events/watch`, `channels/stop`. Unknown methods fail when the endpoint is created.

## Example

An HTTP tool that books a meeting: `POST /tools/book-meeting` with a Calendar event as JSON
creates it in the user's calendar, invites the attendees and returns the created event.

<!-- example: google-calendar-book-meeting -->
```yaml
- route:
    id: book-meeting
    from:
      uri: "platform-http:/tools/book-meeting?httpMethodRestrict=POST"
      steps:
        - to: "google-calendar://events/insert?calendarId={{env:CALENDAR_ID:primary}}&sendUpdates=all&delegate={{env:GOOGLE_USER}}"
        - log: "created event ${body[id]}"
        - marshal:
            json: {}
```

Try it:

```sh
curl -XPOST localhost:8080/tools/book-meeting -H 'Content-Type: application/json' -d '{
  "summary": "Design review",
  "start": {"dateTime": "2026-11-12T10:00:00Z"},
  "end": {"dateTime": "2026-11-12T10:30:00Z"},
  "attendees": [{"email": "ada@example.com"}]}'
```

The event body is a map, or JSON text that is parsed into one. It can also come from the
`CamelGoogleCalendar.content` header.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("platform-http:/tools/book-meeting?httpMethodRestrict=POST")
        .to("google-calendar://events/insert?calendarId={{env:CALENDAR_ID:primary}}&sendUpdates=all&delegate={{env:GOOGLE_USER}}")
        .log("created event ${body[id]}")
        .marshal("json");
})?;
```

## What the core methods do

| api/method | input | body after the call |
|---|---|---|
| `events/list` | `calendarId`, `timeMin`, `timeMax`, `q`, `maxResults`, `singleEvents`, `orderBy` | list of events; all pages unless `maxResults` is set (then one page) |
| `events/insert` | event map (body or `content` header), `sendUpdates` | created event |
| `events/get` | `eventId` | the event |
| `events/update` | `eventId`, full event map, `sendUpdates` | updated event (PUT) |
| `events/patch` | `eventId`, partial event map, `sendUpdates` | updated event (PATCH) |
| `events/delete` | `eventId` | unchanged |
| `events/quickAdd` | `text` option, or the body text (for example `Lunch with Ada tomorrow 12:30`) | created event |
| `calendarList/list` | | list of the user's calendars |
| `calendars/get` | `calendarId` | the calendar |
| `freebusy/query` | content `{timeMin, timeMax, items: [{id}]}` | the free/busy response |
| `events/watch`, `acl/watch`, `calendarList/watch`, `settings/watch` | content `{id, type: web_hook, address, token}` or the `channelId`, `address`, `token` options | the channel; header `CamelGoogleCalendar.resourceId` |
| `channels/stop` | `channelId`, `resourceId` | |

Push notifications from a watch channel arrive as HTTP POSTs to your `address`, with the
`X-Goog-Channel-ID`, `X-Goog-Resource-ID`, `X-Goog-Resource-State` (`sync`, `exists`, ...),
`X-Goog-Message-Number` and `X-Goog-Channel-Token` headers. Receive them with
[platform-http](platform-http.md).

## Options

| option | default | description |
|---|---|---|
| `calendarId` | `primary` | Calendar to use (`primary` or a calendar id such as `team@group.calendar.google.com`). |
| `eventId` | | Event id for `events/get`, `update`, `patch`, `delete`, `instances`, `move`. |
| `timeMin`, `timeMax` | | RFC 3339 bounds for `events/list`. |
| `q` | | Free-text search for `events/list`. |
| `maxResults` | | Page size; when set, only one page is returned. |
| `singleEvents` | | `true` expands recurring events into instances. |
| `orderBy` | | `startTime` (needs `singleEvents=true` on the real API) or `updated`. |
| `text` | | Text for `events/quickAdd`. |
| `sendUpdates` | | `all`, `externalOnly` or `none`: whether attendees get e-mails for `insert`, `update`, `patch`. |
| `ruleId`, `setting`, `destination`, `channelId`, `address`, `token`, `resourceId`, `syncToken`, `sendNotifications` | | Parameters of the ACL, settings, move and watch methods. |
| `delegate` (alias `subject`) | | User to impersonate with domain-wide delegation. |
| `scopes` | `https://www.googleapis.com/auth/calendar` | Comma separated OAuth scopes. |
| `serviceAccountKey` | `GOOGLE_APPLICATION_CREDENTIALS` | Service account JSON key: `file:path`, a plain path or inline JSON. |
| `clientId`, `clientSecret`, `refreshToken` | | OAuth user credentials (refresh-token flow). |
| `accessToken` | | A fixed OAuth access token. |
| `tokenServerUrl` | `https://oauth2.googleapis.com/token` | Token endpoint for the refresh-token flow (Cameleer). |
| `uriEndpointOverride` + `overrideEndpoint=true` | | Base URL of an emulator or proxy. |

`useMetadataServer`, `authenticate`, `proxyHost`, `proxyPort` and `trustAllCertificates` work as
on the [google-sheets](google-sheets.md#options) page. `applicationName`, `inBody` and
`splitResult` are accepted and ignored.

## Message headers

Every option can also be set per message with a `CamelGoogleCalendar.<option>` header (for
example `CamelGoogleCalendar.eventId`), which wins over the URI option.

| header | direction | description |
|---|---|---|
| `CamelGoogleCalendar.content` | in | Event or request map (instead of the body). |
| `CamelGoogleCalendar.eventId` | in | Event id. |
| `CamelGoogleCalendar.resourceId` | out | Resource id of a created watch channel. |

## Run it with Docker

```sh
docker run --rm -p 8080:8080 \
  -e GOOGLE_USER=ada@example.com \
  -e GOOGLE_APPLICATION_CREDENTIALS=/secrets/key.json \
  -v "$PWD/key.json:/secrets/key.json:ro" \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/book-meeting.camel.yaml
```

## Common questions

### How do I create a Google Calendar event from a route?
Send an event map with `summary`, `start` and `end` to
`google-calendar://events/insert?calendarId=primary`. Add `sendUpdates=all` to e-mail attendees.

### How do I update only one field of an event?
Use `events/patch` with `CamelGoogleCalendar.eventId` and a map holding just that field, for
example `{"location": "Room 2"}`.

### Can a service account use its own calendar?
Yes, without `delegate` it uses the service account's calendar. To act on a person's calendar,
share the calendar with the service account or use `delegate` with domain-wide delegation.

### How do I react to new or changed events?
Use the [google-calendar-stream](google-calendar-stream.md) consumer, or a watch channel with
`events/watch`.

## Related

- [google-calendar-stream](google-calendar-stream.md): consume new and changed events.
- [google-mail](google-mail.md), [google-drive](google-drive.md), [google-sheets](google-sheets.md).
- [Apache Camel Google Calendar component](https://camel.apache.org/components/next/google-calendar-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
