# VM component

> The `vm` component is a [seda](seda.md) queue shared by every `CamelContext` in the same process, so separately deployed route sets can pass messages in memory.

**Scheme:** `vm:` · **Crate:** `cameleer-core` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [VM](https://camel.apache.org/components/next/vm-component.html)

## When to use it

- Connect two `CamelContext`s embedded in one Rust program (for example a plugin's routes and the host's routes).
- Feed a shared audit or metrics route from several independent route sets.
- Kamelet Pipes map Knative `Channel` and `Broker` references to `vm:<name>`, an in-process channel.

## URI format

```text
vm:name[?options]
```

## Example

<!-- example: vm-shared-audit -->
```yaml
- route:
    id: audit-publisher
    from:
      uri: direct:audit
      steps:
        - to: vm:audit
- route:
    id: audit-writer
    from:
      uri: vm:audit
      parameters:
        concurrentConsumers: 1
      steps:
        - log: "audit: ${body}"
        - to: log:audit
```

Any context in the process that sends to `vm:audit` reaches the `audit-writer` route.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:audit").route_id("audit-publisher").to("vm:audit");
        b.from("vm:audit?concurrentConsumers=1")
            .route_id("audit-writer")
            .log("audit: ${body}")
            .to("log:audit");
    })
}
```

## Options

`vm` takes exactly the same options as [seda](seda.md#options): `size`, `concurrentConsumers`,
`waitForTaskToComplete`, `timeout`, `multipleConsumers`, `blockWhenFull`, `discardWhenFull`,
`offerTimeout`, `pollTimeout`, `purgeWhenStopping`, `failIfNoConsumers`, `discardIfNoConsumers`,
`limitConcurrentConsumers`.

## Message headers

None added. The consumer gets a copy of the exchange with the property `CamelCorrelationId`.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/audit.camel.yaml
```

The container runs one context, so `vm` behaves like `seda` there.

## Common questions

### What is the difference between vm and seda?

A `seda` queue is visible only inside its own `CamelContext`. A `vm` queue with the same name is the
same queue for every context in the process.

### Can vm connect two separate processes or containers?

No. It is in memory. Use a broker ([kafka](kafka.md), [amqp](amqp.md), [paho](paho.md)) between processes.

## Related

- [seda](seda.md), [direct](direct.md), [kamelet](kamelet.md)
- [Apache Camel VM component](https://camel.apache.org/components/next/vm-component.html)
