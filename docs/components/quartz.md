# Quartz component

> The `quartz` component starts a route from a cron trigger or a simple repeating trigger, using Apache Camel's Quartz endpoint syntax.

**Scheme:** `quartz:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Quartz](https://camel.apache.org/components/next/quartz-component.html)

## When to use it

- Move existing Camel routes that use `quartz://group/name?cron=...` without changing them.
- Run a job on a cron schedule, such as syncing device configuration every 15 minutes.
- Fire N times at a fixed interval with a simple trigger (`trigger.repeatInterval`, `trigger.repeatCount`).

There is no Quartz scheduler underneath: each endpoint runs its own trigger thread with the same cron
engine as [cron](cron.md). There is no job store and no clustering.

## URI format

```text
quartz:[group/]name?cron=expression[&options]
quartz:[group/]name?trigger.repeatInterval=ms&trigger.repeatCount=n
```

## Example

<!-- example: quartz-sync-devices -->
```yaml
- route:
    id: sync-devices
    from:
      uri: quartz:edge/sync-devices
      parameters:
        cron: "0 0/15 * * * ?"
        trigger.timeZone: UTC
      steps:
        - setBody:
            constant: sync
        - to: log:sync
```

The route fires at minute 0, 15, 30 and 45 of every hour, UTC.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("quartz:edge/sync-devices?cron=0+0/15+*+*+*+?&trigger.timeZone=UTC")
            .route_id("sync-devices")
            .set_body(constant("sync"))
            .to("log:sync");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `cron` | | Cron expression; without it the endpoint uses a simple trigger. |
| `trigger.timeZone` (or `timeZone`) | process default zone | Time zone of the cron expression. |
| `trigger.repeatInterval` | `1000` | Simple trigger: ms between fires. |
| `trigger.repeatCount` | `-1` | Simple trigger: extra fires after the first one. `-1` fires forever. |
| `triggerStartDelay` | `500` | ms before the first simple fire. |
| `fireNow` | `false` | Fire the simple trigger immediately at start. |
| `startDelayedSeconds` | `0` | Delay the start of the trigger by this many seconds. |
| `exchangePattern` | `InOnly` | `InOnly` or `InOut`. |
| `deleteJob`, `pauseJob`, `durableJob`, `stateful`, `recoverableJob`, `usingFixedCamelContextName`, `customCalendar`, `job.*`, other `trigger.*` | | Accepted and ignored (no Quartz job store). |

## Message headers

The same Quartz headers as [cron](cron.md#message-headers): `fireTime`, `scheduledFireTime`,
`nextFireTime`, `previousFireTime`, `refireCount`, `jobRunTime`, `triggerName`, `triggerGroup`.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/sync.camel.yaml
```

## Common questions

### Should I use quartz or cron?

They share one engine. Use `quartz` to keep Camel URIs that already use it or for simple triggers, and
`cron` for new routes.

### Do quartz jobs survive a restart or run once in a cluster?

No. Without a Quartz job store, triggers live in the process. Run one instance for jobs that must run
once, or guard the job with an idempotent consumer.

## Related

- [cron](cron.md), [timer](timer.md), [scheduler](scheduler.md)
- [Apache Camel Quartz component](https://camel.apache.org/components/next/quartz-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
