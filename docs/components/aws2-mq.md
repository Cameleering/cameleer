# AWS MQ component

> The `aws2-mq` component lists, creates, describes, reboots and deletes Amazon MQ brokers (ActiveMQ and RabbitMQ).

**Scheme:** `aws2-mq:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS MQ](https://camel.apache.org/components/next/aws2-mq-component.html)

## When to use it

- Report the brokers of an account and their state.
- Provision or reboot brokers from an operations route.

To send and receive messages through an Amazon MQ broker, use the protocol components instead
(AMQP, STOMP, MQTT or JMS-style `amqp:`).

## URI format

```text
aws2-mq://label?operation=...[&options]
```

## Example

List the brokers when the route starts:

<!-- example: aws2-mq-list-brokers -->
```yaml
- route:
    id: mq-inventory
    from:
      uri: "timer:mq-inventory?repeatCount=1"
      steps:
        - to: "aws2-mq://inventory?region={{env:AWS_REGION:us-east-1}}&operation=listBrokers"
        - log: "brokers: ${body}"
        - to: "log:brokers"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:mq-inventory?repeatCount=1")
        .to("aws2-mq://inventory?region={{env:AWS_REGION:us-east-1}}&operation=listBrokers")
        .log("brokers: ${body}")
        .to("log:brokers");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | | Required: `listBrokers`, `createBroker`, `describeBroker`, `deleteBroker`, `rebootBroker`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsMQOperation` | in | Overrides `operation`. |
| `CamelAwsMQMaxResults` | in | Page size of `listBrokers` (all pages are read). |
| `CamelAwsMQBrokerName` | in | Name of `createBroker`. |
| `CamelAwsMQBrokerEngine`, `CamelAwsMQBrokerEngineVersion` | in | Engine (default `ACTIVEMQ`) and version. |
| `CamelAwsMQBrokerInstanceType`, `CamelAwsMQBrokerDeploymentMode`, `CamelAwsMQBrokerPubliclyAccessible` | in | Broker settings (deployment mode default `SINGLE_INSTANCE`). |
| `CamelAwsMQBrokerUsers` | in | Users: list of `{username, password, consoleAccess}`. |
| `CamelAwsMQBrokerId` | in/out | Broker of describe, delete and reboot; set by `createBroker`. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/mq.camel.yaml
```

## Common questions

### What does `listBrokers` return?

The list of broker summaries (id, name, state, engine, deployment mode, ...) of all pages.

## Related

- [AWS MSK](aws2-msk.md), [Azure Service Bus](azure-servicebus.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS MQ](https://camel.apache.org/components/next/aws2-mq-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
