# YAML routes for coding agents

> How an AI coding assistant should write Cameleer YAML routes: start from a tested skeleton,
> follow a short list of rules, look up component options instead of guessing, and run
> `cameleer validate` until the file is clean before running it.

This page is written for coding agents (Claude Code, Codex, Copilot, Cursor and similar) and for
the people who review their work. It is deliberately strict: it lists one way to do each thing,
the way that is known to work. The complete syntax is in the [YAML DSL reference](yaml-dsl.md).
Every skeleton below runs in the test suite (`crates/cameleer-main/tests/docs_yaml.rs`).

## The checklist

Do these in order for every route file you write or change:

1. Name the file `<purpose>.camel.yaml`.
2. Start from the closest [skeleton](#skeletons) on this page. Do not start from memory of the
   Java DSL or Spring XML.
3. For every endpoint, open `docs/components/<scheme>.md` (or run
   `cameleer catalog components`) and use only options listed there. Never invent an option.
4. Put hosts, ports, credentials and tokens in placeholders: `{{env:NAME:default}}` for
   environment variables, `{{name:default}}` for properties. No secret ever goes in the file.
5. Give every route an `id` (kebab-case, unique across files).
6. Validate: `cameleer validate <file> --format json`. Fix every finding with severity `error`,
   read every `warning`, and repeat until `"valid": true`.
7. Run it: `cameleer run <file>` (add `--max-seconds 10` for a smoke test), or in Docker:
   `docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/<file>`.
8. If you changed Cameleer itself rather than a route, run `cargo test --workspace`.

## Rules that prevent the usual mistakes

**Structure**

- The file is a YAML list: every top-level item starts with `- ` and has exactly one key
  (`route`, `rest`, `restConfiguration`, `routeConfiguration`, `routeTemplate`,
  `templatedRoute`, `errorHandler`, `beans`).
- A route is `- route:` with `id` and `from`; `from` has `uri` and `steps`; `steps` is
  indented under `from`, aligned with `uri`.
- Each step is one list item with exactly one key, the step name. All options of a step go
  under that key. A step that contains steps (`filter`, `split`, `choice`/`when`, `doTry`,
  `aggregate`, `loop`, ...) has its own `steps` list under its own keys.
- `choice` takes `when:` (a list, each item with one expression key and `steps`) and
  `otherwise:` (a mapping with `steps`).
- Use Camel 4 names exactly: `setBody`, `setHeader`, `toD`, `wireTap`, `doTry`/`doCatch`/
  `doFinally`, `onException`, `aggregationStrategy`, `completionSize`. They are camelCase.

**Expressions**

- Write expressions in the short form: `simple: "..."`, `constant: ...`, `header: name`,
  `jsonpath: "$.x"`. One language key per step.
- Always quote Simple expressions with double quotes. Use single quotes for string literals
  inside Simple: `simple: "${header.type} == 'gold'"`.
- A JSON body built with Simple goes in single quotes: `simple: '{"id": "${header.id}"}'`.
- `${...}` is evaluated only in expressions, `log` messages and `toD` URIs. In a `to` URI it is
  literal text: use `toD` when the URI depends on the message.
- Header values from HTTP requests, files and queues are text: compare numbers with
  `${header.amount} > 100` (Simple converts) rather than string equality.

**Endpoints**

- Prefer `uri:` plus `parameters:` over long inline query strings: values are URL-encoded for
  you and option names are easy to check.
- Quote any value that starts with `{{`, `{`, `*`, `&`, `!`, `%` or `@`, or that contains `: `
  or ` #`.
- Use `direct:` to connect your own routes; `seda:` for an in-process queue. Sending from a
  request/reply route (REST, HTTP) to `seda:` with the default pattern waits for the consumer;
  add `pattern: InOnly` to hand the message off and reply at once.
- Use booleans `true`/`false` only. `yes`, `no`, `on`, `off` are strings in YAML 1.2 and are
  rejected or ignored.

**Behaviour**

- Error handling goes in a `routeConfiguration` (shared) or an `onException` at the start of a
  route's steps. Use `handled: {constant: true}` when the error should not reach the caller.
- After `split`, the message is the original one unless you set `aggregationStrategy`
  (`groupedBody` collects the parts' bodies in a list).
- Durations: plain numbers are milliseconds; `5s`, `250ms`, `PT1M` also work.
- Do not use `beans:` for Java classes: the `cameleer` binary cannot create them. Use built-in
  aggregation strategies by name (`groupedBody`, `useLatest`, `string`) and Simple, JSONPath or
  jq instead of custom code.

## Look up a component's options

- `docs/components/<scheme>.md` has the URI format, every supported option with its default,
  the message headers, and a tested example. For example `docs/components/kafka.md`,
  `docs/components/http.md`, `docs/components/aws2-s3.md`.
- `cameleer catalog components` lists every scheme in your build; `cameleer catalog languages`
  lists the expression languages; `cameleer catalog kamelets` lists the Kamelets.
- Options and headers have the same names as in Apache Camel. When a page and the Camel docs
  disagree, the page (and the validator) describe what Cameleer does.
- An option that is not supported fails validation with code `endpoint`. Do not "fix" it by
  removing random options: look the option up.

## Secrets and configuration

```text
{{env:KAFKA_BROKERS:localhost:9092}}   environment variable with a default
{{env:SLACK_TOKEN}}                     environment variable, required
{{orders.topic:orders}}                 property from application.properties, --prop or CAMEL_* env vars
{{aws:db-password}}                     secret from AWS Secrets Manager (also azure:, gcp:)
```

A required placeholder (no default) that is not set fails at startup; `cameleer validate`
reports it as `unresolved-placeholder` (a warning, because it may be set where the route runs).
Pass test values with `--prop key=value` or `--properties application.properties`.

## Skeletons

Copy the closest skeleton, rename ids and endpoints, then validate.

### Consume, transform, deliver

Read JSON files from a folder, add a field, write them to another folder. The same shape works
for Kafka, AMQP, SQS or MQTT: change the two URIs and their options.

<!-- example: agent-consume-transform -->
```yaml
- route:
    id: enrich-orders
    from:
      uri: "file:{{env:INBOX_DIR:inbox}}"
      parameters:
        include: ".*\\.json"
        move: .done
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: orderId
            simple: "${body[id]}"
        - setBody:
            simple: '{"id": "${header.orderId}", "status": "received"}'
        - log: "order ${header.orderId} received"
        - to:
            uri: "file:{{env:OUTBOX_DIR:outbox}}"
            parameters:
              fileName: "${header.orderId}.json"
```

### REST API

Two operations backed by `direct:` routes. Path parameters become headers; the response code
is the `CamelHttpResponseCode` header.

<!-- example: agent-rest-api -->
```yaml
- restConfiguration:
    contextPath: /api
- rest:
    path: /devices
    get:
      - path: /{id}
        id: get-device
        to: direct:get-device
    post:
      - id: register-device
        to: direct:register-device
- route:
    id: get-device-route
    from:
      uri: direct:get-device
      steps:
        - setHeader:
            name: Content-Type
            constant: application/json
        - setBody:
            simple: '{"id": "${header.id}", "online": true}'
- route:
    id: register-device-route
    from:
      uri: direct:register-device
      steps:
        - setHeader:
            name: CamelHttpResponseCode
            constant: 201
        - setBody:
            simple: "registered ${body}"
```

### Scheduled job

Call an HTTP API on a schedule and deliver the answer. `timer` takes a period in milliseconds;
use `cron` or `quartz` for calendar schedules.

<!-- example: agent-scheduled-job -->
```yaml
- route:
    id: poll-status
    from:
      uri: timer:poll-status
      parameters:
        period: "{{status.period:60000}}"
      steps:
        - setHeader:
            name: CamelHttpMethod
            constant: GET
        - to: "{{env:STATUS_URL:http://localhost:8080/health}}"
        - convertBodyTo: String
        - log: "status: ${body}"
        - to: "{{env:STATUS_SINK:log:status}}"
```

### Error handling

Retry, then send what still fails to a dead letter endpoint; turn bad input into a clear reply
instead.

<!-- example: agent-error-handling -->
```yaml
- routeConfiguration:
    errorHandler:
      deadLetterChannel:
        deadLetterUri: "{{env:DEAD_LETTER_URI:log:dead-letters}}"
        useOriginalMessage: true
        redeliveryPolicy:
          maximumRedeliveries: 3
          redeliveryDelay: 1s
          useExponentialBackOff: true
    onException:
      - onException:
          exception:
            - java.lang.IllegalArgumentException
          handled:
            constant: true
          steps:
            - setBody:
                simple: "invalid input: ${exception.message}"
- route:
    id: process-payment
    from:
      uri: direct:payment
      steps:
        - filter:
            simple: "${body} == ''"
            steps:
              - throwException:
                  exceptionType: java.lang.IllegalArgumentException
                  message: empty payment
        - to: direct:gateway
- route:
    id: gateway
    from:
      uri: direct:gateway
      steps:
        - setBody:
            simple: "charged ${body}"
```

## The validate loop

`cameleer validate` checks route files without starting routes or
connecting to anything. It is safe to run on any file, any number of times.

```sh
cameleer validate orders.camel.yaml --format json
cameleer validate routes/ --format json --strict --properties application.properties
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer validate /routes
```

The loop:

1. Write the file.
2. Run `cameleer validate <file> --format json`.
3. If `valid` is `false`, fix each finding, using `line`, `column`, `message` and `hint`.
   Fix the first `yaml-syntax` error first: later findings may come from it.
4. Repeat from step 2 until `valid` is `true` (exit code 0). With `--strict`, warnings count
   as errors, which is a good default for generated files.
5. Run the route.

The finding codes:

| code | severity | meaning | typical fix |
|---|---|---|---|
| `yaml-syntax` | error | the file is not valid YAML | fix indentation, quote the value |
| `dsl-structure` | error | the YAML is not a valid route: unknown element, step or key, a missing required key, a wrong type; the hint suggests the name you probably meant | rename or move the key as the hint says |
| `endpoint` | error | an unknown scheme or an option the component does not support (checked the same way Apache Camel does) | look up `docs/components/<scheme>.md` |
| `expression` | error | an expression does not compile | fix the Simple/JSONPath/jq syntax |
| `unresolved-placeholder` | warning | a `{{...}}` placeholder has no value and no default | add a default, or pass `--prop`/`--properties` |
| `dangling-endpoint` | warning | `to: direct:x` but no route consumes `from: direct:x` in the validated files | add the route, or validate all files together |
| `duplicate-route-id` | error | two routes have the same `id` | rename one |

Exit codes: `0` valid, `1` errors (or warnings with `--strict`), `2` bad usage. The full
reference, with the text and JSON output formats, is on the [validate page](validate.md).

## Worked example

**The request:** "Add an endpoint `POST /orders` that takes a JSON order. Reject an order with
no `id` with HTTP 400 and a JSON error. Put valid orders on a queue and answer 202 at once. A
worker with four consumers writes each order to the folder in `ORDERS_DIR` as
`order-<id>.json`."

**First draft.** The agent starts from the REST skeleton and writes the route. One line of the
draft:

```text
        - to: "seda:orders?concurrentConsumer=4"
```

**Validate.**

```text
$ cameleer validate orders.camel.yaml
orders.camel.yaml:33:15: error[endpoint]: seda:orders?concurrentConsumer=4: unknown option 'concurrentConsumer' (did you mean 'concurrentConsumers'?)
1 file, 1 error, 0 warnings
```

Two things are wrong here, and the validator found one of them: the option is
`concurrentConsumers`, and it belongs on the consumer (`from`), not on the producer. Looking up
[docs/components/seda.md](../components/seda.md) shows both. The agent also notices that a
REST route sending to `seda:` waits for the worker unless the send is `InOnly`.

**The fixed route**, which validates cleanly and runs:

<!-- example: agent-worked-example -->
```yaml
- rest:
    path: /orders
    post:
      - id: create-order
        consumes: application/json
        to: direct:create-order
- route:
    id: create-order-route
    from:
      uri: direct:create-order
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: Content-Type
            constant: application/json
        - choice:
            when:
              - simple: "${body[id]} == null"
                steps:
                  - setHeader:
                      name: CamelHttpResponseCode
                      constant: 400
                  - setBody:
                      constant: '{"error": "order id is required"}'
            otherwise:
              steps:
                - to:
                    uri: seda:orders
                    pattern: InOnly
                - setHeader:
                    name: CamelHttpResponseCode
                    constant: 202
                - setBody:
                    simple: '{"accepted": "${body[id]}"}'
- route:
    id: order-worker
    from:
      uri: seda:orders
      parameters:
        concurrentConsumers: 4
      steps:
        - setHeader:
            name: CamelFileName
            simple: "order-${body[id]}.json"
        - marshal:
            json: {}
        - to: "file:{{env:ORDERS_DIR:orders}}"
```

```text
$ cameleer validate orders.camel.yaml --format json
{"valid": true, "files": [{"path": "orders.camel.yaml", "valid": true, "routes": 3, "findings": []}], "summary": {"files": 1, "errors": 0, "warnings": 0}}
$ ORDERS_DIR=/tmp/orders cameleer run orders.camel.yaml
```

`curl -X POST localhost:8080/orders -d '{"id": "A-1", "total": 30}'` answers
`{"accepted": "A-1"}` with status 202 and writes `/tmp/orders/order-A-1.json`; a body without
`id` gets status 400 and `{"error": "order id is required"}`.

## Common questions

### Which file should an agent read first?

`AGENTS.md` at the repository root, then this page, then the [YAML DSL reference](yaml-dsl.md)
for anything a skeleton does not cover, and `docs/components/<scheme>.md` for each endpoint.

### How does an agent know an option exists?

It looks it up in `docs/components/<scheme>.md` and lets `cameleer validate` confirm it. The
validator resolves endpoints the same way the runtime does, so an option it accepts works.

### Can the validator run without the route's servers and credentials?

Yes. It never starts routes or connects to brokers, databases or APIs, so it runs anywhere,
including CI and sandboxes without network access. Placeholders without a value are reported as
warnings, not errors.

### What if the validator accepts a route that then fails at runtime?

The validator checks structure, endpoints and expressions, not reachability or data. Run the
route with `--max-seconds` against test inputs, and if the failure looks like a Cameleer bug,
report it (see below).

## Related

- [YAML DSL reference](yaml-dsl.md), [`cameleer validate`](validate.md)
- [Getting started](getting-started.md), [EIP guide](eips.md), [error handling](error-handling.md)
- [Cameleer for AI agents](agents.md): routes that call LLMs and tools
- [Components](../components/)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
