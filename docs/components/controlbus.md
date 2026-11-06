# Control Bus component

> The `controlbus` component starts, stops, suspends and inspects routes from a message, and evaluates scripts sent as messages.

**Scheme:** `controlbus:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Control Bus](https://camel.apache.org/components/next/controlbus-component.html)

## When to use it

- Pause a route during a maintenance window and resume it afterwards.
- Expose route status and statistics to an operator, a dashboard or an AI agent ("is the ingest route running?").
- Stop a consumer when a downstream system is down, from an error handler.

## URI format

```text
controlbus:route?routeId=id&action=start|stop|suspend|resume|restart|status|stats|fail
controlbus:language:languageName
```

## Example

<!-- example: controlbus-maintenance -->
```yaml
- route:
    id: ingest
    from:
      uri: timer:ingest
      parameters:
        period: 60000
      steps:
        - to: log:ingest
- route:
    id: ingest-status
    from:
      uri: direct:status
      steps:
        - to: controlbus:route?routeId=ingest&action=status
- route:
    id: ingest-pause
    from:
      uri: direct:pause
      steps:
        - to: controlbus:route?routeId=ingest&action=stop
```

Calling `direct:status` returns `Started`. After `direct:pause`, it returns `Stopped`.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:ingest?period=60000").route_id("ingest").to("log:ingest");
        b.from("direct:status")
            .route_id("ingest-status")
            .to("controlbus:route?routeId=ingest&action=status");
        b.from("direct:pause")
            .route_id("ingest-pause")
            .to("controlbus:route?routeId=ingest&action=stop");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `routeId` | | The route to act on. `current` means the route of the exchange. Without it, `status` returns the context status and `stats` the statistics of all routes. |
| `action` | `status` | `start`, `stop`, `suspend`, `resume`, `restart`, `status`, `stats` or `fail`. |
| `async` | `false` | Run the action in the background and return at once. |
| `restartDelay` | `1000` | ms between stop and start for `restart`. |
| `loggingLevel` | `INFO` | Level of the log line written for each action. |

`status` and `stats` put their result in the message body. Stopping, suspending or restarting the route
that is processing the exchange always happens in the background, so the exchange can complete.

With `controlbus:language:simple` (or another language), the message body is evaluated as a script and
the result becomes the body.

## Message headers

None read or added.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/maintenance.camel.yaml
```

## Common questions

### How do I stop a route from inside itself?

Use `controlbus:route?routeId=current&action=stop`. The stop runs in the background after the exchange
completes.

### How do I get statistics for all routes?

Send any message to `controlbus:route?action=stats` (no `routeId`).

## Related

- [direct](direct.md), [timer](timer.md)
- [Apache Camel Control Bus component](https://camel.apache.org/components/next/controlbus-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
