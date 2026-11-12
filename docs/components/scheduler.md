# Scheduler component

> The `scheduler` component starts a route repeatedly with a scheduled poll consumer: a delay between runs, optional back-off when idle, or a cron expression.

**Scheme:** `scheduler:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Scheduler](https://camel.apache.org/components/next/scheduler-component.html)

## When to use it

- Poll an inventory API, a database table or a mailbox with a fixed delay between polls.
- Slow down polling when there is nothing to do (`backoffMultiplier` with `backoffIdleThreshold`).
- Poll on a calendar schedule with `scheduler=quartz&scheduler.cron=...`.

The scheduler waits `delay` after a run finishes (fixed delay). Use [timer](timer.md) for a fixed rate.

## URI format

```text
scheduler:name[?options]
```

## Example

<!-- example: scheduler-poll-inventory -->
```yaml
- route:
    id: poll-inventory
    from:
      uri: scheduler:inventory
      parameters:
        initialDelay: 1000
        delay: 60000
      steps:
        - setBody:
            constant: refresh
        - to: log:inventory
```

The route runs one second after startup and then one minute after each run finishes.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("scheduler:inventory?initialDelay=1000&delay=60000")
            .route_id("poll-inventory")
            .set_body(constant("refresh"))
            .to("log:inventory");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `initialDelay` | `1000` | Wait before the first poll (in `timeUnit`, or with a unit such as `5s`). |
| `delay` | `500` | Wait between polls. |
| `timeUnit` | `MILLISECONDS` | Unit of `initialDelay` and `delay` (`NANOSECONDS` ... `DAYS`). |
| `useFixedDelay` | `true` | Wait `delay` after each poll finishes; `false` schedules at a fixed rate. |
| `repeatCount` | `0` | Stop after this many polls. `0` polls forever. |
| `greedy` | `false` | Poll again at once when the previous poll processed messages. |
| `sendEmptyMessageWhenIdle` | `false` | Accepted for poll consumers that may find nothing; the scheduler always sends one exchange per poll. |
| `backoffMultiplier` | `0` | Skip this many polls after the idle or error threshold is reached. Required with the thresholds. |
| `backoffIdleThreshold` | `0` | Number of idle polls in a row before backing off. |
| `backoffErrorThreshold` | `0` | Number of failed polls in a row before backing off. |
| `runLoggingLevel` | `TRACE` | Log level of the "poll start/end" messages. |
| `startScheduler` | `true` | With `false`, the scheduler thread is not started. |
| `scheduler` | | `quartz` or `spring` (any value is accepted). Combined with `scheduler.cron`, polls happen at the cron fire times and the delays are ignored. |
| `scheduler.cron` | | A cron expression, see [cron](cron.md). |
| `scheduler.timeZone` / `scheduler.trigger.timeZone` | process zone | Time zone of `scheduler.cron`. |
| `poolSize`, `synchronous`, `pollStrategy`, `bridgeErrorHandler`, `exceptionHandler`, `exchangePattern` | | Accepted and ignored. |

A route tells the scheduler that a poll found nothing by setting the exchange property
`CamelSchedulerPolledMessages` to `false`. This drives `greedy` and the idle back-off.

## Message headers

Each poll sends an exchange with a null body and the exchange properties `CamelTimerName` and
`CamelTimerFiredTime`.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/inventory.camel.yaml
```

## Common questions

### What is the difference between scheduler and timer?

Both start a route repeatedly. `scheduler` is a poll consumer: fixed delay after each run, greedy
polling and back-off. `timer` fires at fixed intervals and counts fires in `CamelTimerCounter`.

### How do I poll every weekday at 08:00?

`scheduler:report?scheduler=quartz&scheduler.cron=0+0+8+?+*+MON-FRI` (`+` is a space in a URI).

## Related

- [timer](timer.md), [cron](cron.md), [quartz](quartz.md)
- [Apache Camel Scheduler component](https://camel.apache.org/components/next/scheduler-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
