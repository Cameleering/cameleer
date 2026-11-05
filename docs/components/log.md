# Log component

> The `log` component writes each message that reaches it to the log, with control over the level and which parts (body, headers, properties) are shown.

**Scheme:** `log:` · **Crate:** `cameleer-core` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Log](https://camel.apache.org/components/next/log-component.html)

## When to use it

- See what flows through a route while developing or debugging.
- Keep an audit trail of the messages an AI agent route sends and receives (`showHeaders=true`).
- Log throughput instead of every message on busy routes (`groupSize` or `groupInterval`).

For a free-text log line inside a route, the `log` EIP step (`- log: "..."`) is shorter.

## URI format

```text
log:loggerName[?options]
```

## Example

<!-- example: log-debug-orders -->
```yaml
- route:
    id: debug-orders
    from:
      uri: direct:orders
      steps:
        - to:
            uri: log:orders
            parameters:
              level: DEBUG
              showHeaders: true
              showBodyType: false
              maxChars: 500
        - setBody:
            simple: "logged ${body}"
```

Each message produces a line such as
`Exchange[ExchangePattern: InOut, Headers: {region=EU}, Body: order-1]` on the `orders` logger at DEBUG.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:orders")
            .route_id("debug-orders")
            .to("log:orders?level=DEBUG&showHeaders=true&showBodyType=false&maxChars=500")
            .set_body(simple("logged ${body}"));
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `level` | `INFO` | `TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR` or `OFF`. |
| `showAll` | `false` | Show everything below. |
| `showExchangeId` | `false` | Show the exchange id. |
| `showExchangePattern` | `true` | Show `InOnly` / `InOut`. |
| `showProperties` | `false` | Show exchange properties. |
| `showHeaders` | `false` | Show message headers. |
| `showBody` | `true` | Show the body. |
| `showBodyType` | `true` | Show the body type, as the Java class name Camel would show (`java.lang.String`, ...). |
| `showException` | `true` | Show the exception of a failed exchange. |
| `showRouteId` | `false` | Show the route id. |
| `multiline` | `false` | One line per part instead of one line in total. |
| `maxChars` | `10000` | Cut the body after this many characters. |
| `skipBodyLineSeparator` | `true` | Replace line breaks in the body with spaces. |
| `groupSize` | | Log one throughput line every N messages instead of each message. |
| `groupInterval` | | Log one throughput line per interval (ms or a duration). |
| `logMask` | `false` | Mask values of sensitive keys such as `password` or `secret`. |
| `groupDelay`, `groupActiveOnly`, `showCaughtException`, `showStackTrace`, `showFiles`, `showFuture`, `showStreams`, `showCachedStreams`, `showAllProperties`, `showVariables`, `plain`, `style`, `marker`, `sourceLocationLoggerName`, `showRouteGroup` | | Accepted and ignored. |

## Message headers

The log component reads the message but does not change it.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml
```

Log lines go to standard output, so `docker logs` shows them.

## Common questions

### How do I log only throughput on a busy route?

Use `log:throughput?groupSize=1000` to get one line per 1000 messages with the rate, or
`groupInterval=10000` for one line every 10 seconds.

### How do I hide passwords in the log?

Add `logMask=true`. Values of keys that look like secrets are replaced with `xxxxxx`.

### What is the difference between `to: log:x` and `- log: "..."`?

`to: log:x` prints a formatted view of the whole exchange. The `log` step prints the message you write,
with Simple expressions such as `${body}`.

## Related

- [mock](mock.md), [browse](browse.md)
- [Apache Camel Log component](https://camel.apache.org/components/next/log-component.html)
