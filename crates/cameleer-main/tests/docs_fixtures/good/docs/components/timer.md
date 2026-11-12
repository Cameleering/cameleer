# Timer component

> The `timer` component fires a message at a fixed interval, so a route can poll, clean up or send heartbeats.

**Scheme:** `timer:` · **Also:** `cron:` · **Crate:** `cameleer-core` ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Timer](https://camel.apache.org/components/next/timer-component.html)

## When to use it

- Poll an API every minute and push changes to a queue.
- Send a heartbeat from an edge device, with *low* memory and **no JVM**.

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
      uri: "timer:heartbeat?period={{env:PERIOD:5000}}"
      steps:
        - setBody:
            constant: "alive <3 & well"
        - to: "log:heartbeat?showHeaders=true"
```

The same kind of route with the endpoint options as a `parameters` map:

<!-- example: timer-parameters -->
```yaml
- route:
    from:
      uri: timer:tick
      parameters:
        period: 1000
        repeatCount: 3
      steps:
        - toD: "log:${header.CamelTimerName}"
        - to:
            uri: seda:ticks
            parameters:
              size: 10
```

## Options

| option | default | description |
|---|---|---|
| `period` | `1000` | Milliseconds between messages. |
| `repeatCount` | `0` | Stop after this many messages; `0` means forever. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/heartbeat.camel.yaml
```

A Compose file is not a route, so it is marked `not-camel`:

<!-- not-camel -->
```yaml
services:
  heartbeat:
    image: cameleering/cameleer
```

## Common questions

### How do I run a route only once?

Set `repeatCount=1`. The timer fires once and then stays idle.

### Can the timer use a cron expression?

No. Use `cron:` (listed on this page) or the [options](#options) of a scheduled consumer.

## Related

- [Simple language](../languages/simple.md)
- [Getting started](../guides/getting-started.md)
- ![Route diagram](../images/route.svg)
