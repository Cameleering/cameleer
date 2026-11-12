# Cron component

> The `cron` component starts a route on a cron schedule, such as every night at 02:00 in a given time zone.

**Scheme:** `cron:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Cron](https://camel.apache.org/components/next/cron-component.html)

## When to use it

- Nightly reports, exports and clean-up jobs.
- Business-hours polling (`0 0/15 8-18 ? * MON-FRI`).
- Scheduled AI agent tasks, such as a daily summary of yesterday's tickets.

Cameleer has its own cron engine (no Quartz library): it understands Quartz 6 and 7 field syntax,
Unix 5 field syntax, `L`, `W`, `#`, month and day names, and macros such as `@daily`.

## URI format

```text
cron:name?schedule=expression[&timeZone=zone]
```

In a URI, write spaces in the expression as `+`. In the YAML `parameters` block you can use spaces.

## Example

<!-- example: cron-nightly-report -->
```yaml
- route:
    id: nightly-report
    from:
      uri: cron:nightly
      parameters:
        schedule: "0 0 2 * * ?"
        timeZone: Europe/Paris
      steps:
        - setBody:
            simple: "report fired at ${header.fireTime}"
        - to: log:reports
```

The route fires every day at 02:00 Paris time.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("cron:nightly?schedule=0+0+2+*+*+?&timeZone=Europe/Paris")
            .route_id("nightly-report")
            .set_body(simple("report fired at ${header.fireTime}"))
            .to("log:reports");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `schedule` | (required) | The cron expression. |
| `timeZone` (alias `trigger.timeZone`) | process default zone | A region id (`Europe/Paris`) or a fixed offset (`GMT+02:00`). |
| `exchangePattern` | `InOnly` | `InOnly` or `InOut`. |
| `bridgeErrorHandler`, `exceptionHandler` | | Accepted and ignored. |

Fields of a Quartz expression: seconds, minutes, hours, day of month, month, day of week, optional
year. A 5 field expression is read as Unix cron (minutes first).

## Message headers

Each fire sends an exchange with a null body and these Quartz headers. Times are epoch
milliseconds (Camel uses `java.util.Date`).

| header | description |
|---|---|
| `fireTime` | When the trigger actually fired. |
| `scheduledFireTime` | When it was scheduled to fire. |
| `nextFireTime` | The next fire time. |
| `previousFireTime` | The previous fire time. |
| `refireCount` | Always `0`. |
| `jobRunTime` | Always `-1` (as in Quartz before the job finishes). |
| `triggerName`, `triggerGroup` | Trigger identity. |

The exchange properties `CamelTimerName`, `CamelTimerFiredTime` and `CamelTimerCounter` are set as well.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/report.camel.yaml
```

The container has the IANA time-zone database built in, so `timeZone: Europe/Paris` works without
system zoneinfo files.

## Common questions

### What happens if a run takes longer than the interval?

Fires missed while the route is busy are skipped (the Quartz "do nothing" misfire policy).

### Which time zone is used without `timeZone`?

The process default zone: UTC unless the `TZ` environment variable is set.

### How do I run every 30 seconds?

`schedule: "0/30 * * * * ?"`.

## Related

- [quartz](quartz.md), [scheduler](scheduler.md), [timer](timer.md)
- [Apache Camel Cron component](https://camel.apache.org/components/next/cron-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
