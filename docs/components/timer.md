# Timer component

> The `timer` component starts a route at a fixed interval, for example every 5 seconds, a set number of times or forever.

**Scheme:** `timer:` · **Crate:** `cameleer-core` ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Timer](https://camel.apache.org/components/next/timer-component.html)

## When to use it

- Send a heartbeat or health status from an edge device every few seconds.
- Poll an HTTP API or a database at a fixed rate.
- Trigger an AI agent loop at a regular interval ("check the inbox every minute").
- Fire once at startup (`repeatCount=1`) or once at a given time (`time=...`).

For calendar schedules ("every night at 02:00") use [cron](cron.md) or [quartz](quartz.md).

## URI format

```text
timer:name[?options]
```

## Example

<!-- example: timer-heartbeat -->
```yaml
- route:
    id: heartbeat
    from:
      uri: timer:heartbeat
      parameters:
        period: 5000
        delay: 0
        includeMetadata: true
      steps:
        - setBody:
            simple: "edge-gw-1 alive #${exchangeProperty.CamelTimerCounter}"
        - to: log:heartbeat
```

The first message fires right away (`delay: 0`), then every 5 seconds: `edge-gw-1 alive #1`,
`edge-gw-1 alive #2`, ...

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:heartbeat?period=5000&delay=0&includeMetadata=true")
            .route_id("heartbeat")
            .set_body(simple("edge-gw-1 alive #${exchangeProperty.CamelTimerCounter}"))
            .to("log:heartbeat");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `period` | `1000` | Interval between fires, in ms or with a unit (`5s`, `1m`). |
| `delay` | `1000` | Wait before the first fire, in ms or with a unit. |
| `repeatCount` | `0` | Stop after this many fires. `0` fires forever. |
| `fixedRate` | `false` | Schedule at a fixed rate (catch up after a slow exchange) instead of a fixed delay between fires. |
| `includeMetadata` | `false` | Also set the metadata properties and headers listed below. |
| `time` | | First fire at this local time: `yyyy-MM-dd HH:mm:ss` or `yyyy-MM-ddTHH:mm:ss`. A trailing `Z`, an offset (`+02:00`) or a zone in brackets (`[Europe/Paris]`) selects another zone. |
| `pattern` | | Accepted; the two formats above are always recognised. |
| `exchangePattern` | `InOnly` | `InOnly` or `InOut`. |
| `synchronous`, `daemon`, `runLoggingLevel`, `timer`, `bridgeErrorHandler`, `exceptionHandler` | | Accepted and ignored. |

## Message headers

Each fired exchange has a null body and the exchange property `CamelTimerCounter` (1, 2, 3, ...).

| name | kind | description |
|---|---|---|
| `CamelTimerCounter` | property (always), header (with `includeMetadata`) | Fire count, starting at 1. |
| `CamelTimerName` | property (with `includeMetadata`) | The timer name from the URI. |
| `CamelTimerPeriod` | property (with `includeMetadata`) | The period in ms. |
| `CamelTimerFiredTime` | property and header (with `includeMetadata`) | When the timer fired. |
| `CamelMessageTimestamp` | header (with `includeMetadata`) | The fire time in epoch ms. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/heartbeat.camel.yaml
```

## Common questions

### How do I run a route only once at startup?

Use `timer:once?repeatCount=1&delay=0`.

### How do I run a route every day at a fixed time?

The timer only knows intervals. Use [cron](cron.md) (`schedule=0 0 2 * * ?`) or [quartz](quartz.md).

### What time zone does the `time` option use?

The process default zone, which is UTC unless `TZ` is set. Add `Z`, an offset or `[Zone/Id]` to choose
another one.

## Related

- [scheduler](scheduler.md), [cron](cron.md), [quartz](quartz.md)
- [Apache Camel Timer component](https://camel.apache.org/components/next/timer-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
