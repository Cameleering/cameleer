# iCal data format

> The `iCal` data format parses iCalendar (RFC 5545, `.ics`) text into a structured value and writes such values back as iCalendar text.

**Name:** `iCal` · **Also:** `ical` · **Crate:** `cameleer-dataformats` · **Cargo feature:** [`dataformats`](../guides/build-features.md) ·
**Apache Camel:** [iCal](https://camel.apache.org/components/next/dataformats/ical-dataformat.html)

## When to use it

- Read meeting invitations that arrive by mail and route them by organizer, date or subject.
- Let an AI scheduling agent turn its decision (a JSON object) into a calendar invite that any
  calendar app accepts.
- Sync events between calendar feeds (`.ics` URLs) and other systems.

## Marshal and unmarshal

| direction | input body | output body |
|---|---|---|
| unmarshal | iCalendar text | a component object; an array when the text holds several `VCALENDAR`s |
| marshal | a component object (or an array of them) | iCalendar text with CRLF line endings, lines folded at 75 octets; sets `Content-Type: text/calendar` |

A component is `{"type": "VEVENT", "properties": {...}, "components": [...]}`. Properties are
keyed by name in document order. A property without parameters is its value string; with
parameters it is `{"value": "...", "params": {"TZID": "Europe/Paris"}}`; a repeated property
is an array. Values stay in their RFC 5545 text form, except that TEXT escapes (`\n`, `\,`,
`\;`) are decoded on unmarshal and encoded on marshal for `SUMMARY`, `DESCRIPTION`,
`LOCATION`, `COMMENT`, `CONTACT`, `CATEGORIES` and `RESOURCES`.

## Example

Read an invitation and pick out the event fields.

<!-- example: ical-read-invite -->
```yaml
- route:
    id: ical-read
    from:
      uri: direct:ical-read
      steps:
        - unmarshal:
            iCal: {}
        - setBody:
            simple: "${body.components[0].properties.SUMMARY} at ${body.components[0].properties.DTSTART.value} (${body.components[0].properties.DTSTART.params.TZID})"
```

<!-- input: ical-read-invite -->
```text
BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Example//Docs//EN
BEGIN:VEVENT
UID:42@example.com
DTSTAMP:20261105T090000Z
DTSTART;TZID=Europe/Berlin:20261112T100000
SUMMARY:Design review\, round 2
END:VEVENT
END:VCALENDAR
```

<!-- output: ical-read-invite -->
```text
Design review, round 2 at 20261112T100000 (Europe/Berlin)
```

Build an invite from a JSON decision (for example an agent's tool call result):

<!-- example: ical-build-invite -->
```yaml
- route:
    id: ical-build
    from:
      uri: direct:ical-build
      steps:
        - unmarshal:
            json: {}
        - marshal:
            iCal:
              validating: true
```

<!-- input: ical-build-invite -->
```json
{"type": "VCALENDAR",
 "properties": {"VERSION": "2.0", "PRODID": "-//Example//Agent//EN"},
 "components": [{"type": "VEVENT",
   "properties": {"UID": "call-7@example.com", "DTSTAMP": "20261105T090000Z",
                  "DTSTART": "20261110T140000Z", "DTEND": "20261110T143000Z",
                  "SUMMARY": "Follow-up call; budget, timeline"}}]}
```

<!-- output: ical-build-invite -->
```text
BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Example//Agent//EN
BEGIN:VEVENT
UID:call-7@example.com
DTSTAMP:20261105T090000Z
DTSTART:20261110T140000Z
DTEND:20261110T143000Z
SUMMARY:Follow-up call\; budget\, timeline
END:VEVENT
END:VCALENDAR
```

## Rust DSL

```rust
b.from("direct:ical-rust")
    .unmarshal(DataFormatDefinition::new("iCal"))
    .set_body(simple("${body.components[0].properties.SUMMARY}"));
```

## Options

| option | default | description |
|---|---|---|
| `validating` | `false` | Check that every `VCALENDAR` has `PRODID` and `VERSION` (on marshal and unmarshal). |

## Differences from Apache Camel

Camel's `iCal` data format maps to ical4j `Calendar` objects. Here the value is a plain
object, so you can read and build it with Simple, JSONPath or a JSON body, and recurrence
rules, time zones and dates stay as text (no ical4j calculations).

## Common questions

### How do I get the start time of an event from an .ics file?

`unmarshal: iCal: {}` and then `${body.components[0].properties.DTSTART}` (or `.value` when
the property has parameters such as `TZID`).

### How do I create a calendar invite to send by email?

Build the object (JSON, Simple `${map(...)}` or a template), `marshal: iCal`, and send it as
an attachment or with `Content-Type: text/calendar; method=REQUEST` through the mail
component.

## Related

- [JSON data format](json.md), [MIME multipart data format](mime-multipart.md)
- [Mail (SMTP) component](../components/smtp.md)
- [Apache Camel iCal data format](https://camel.apache.org/components/next/dataformats/ical-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
