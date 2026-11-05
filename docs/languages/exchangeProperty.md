# ExchangeProperty language

> The `exchangeProperty` language returns the value of one exchange property, or null when the property is not set.

**Name:** `exchangeProperty` · **Crate:** `cameleer-core` · **Apache Camel:** [ExchangeProperty](https://camel.apache.org/components/next/languages/exchangeProperty-language.html)

## When to use it

- Keep a value for the whole exchange (it is not sent to endpoints like headers are) and read
  it back later in the route: the original request, a tenant id, a correlation key.
- Restore the original body after a call that replaced it, for example after asking a model.
- Read properties that Cameleer sets, such as `CamelSplitIndex` inside a split or
  `CamelTimerCounter` on timer routes.

## Syntax

The expression text is the property name. The value keeps its type. In
[Simple](simple.md) the same value is `${exchangeProperty.name}`.

## Examples

Save the request in a property, replace the body, then put the request back:

<!-- example: exchangeProperty-restore-body -->
```yaml
- route:
    id: property-demo
    from:
      uri: direct:property
      steps:
        - setProperty:
            name: originalRequest
            simple: "${body}"
        - setBody:
            constant: "(answer from a model)"
        - setHeader:
            name: answer
            simple: "${body}"
        - setBody:
            exchangeProperty: originalRequest
        - setBody:
            simple: "${body} -> ${header.answer}"
```

<!-- input: exchangeProperty-restore-body -->
```text
Summarize ticket 42
```

<!-- output: exchangeProperty-restore-body -->
```text
Summarize ticket 42 -> (answer from a model)
```

Read the split index set by the `split` EIP:

<!-- example: exchangeProperty-split-index -->
```yaml
- route:
    id: property-split-index
    from:
      uri: direct:property-split
      steps:
        - split:
            tokenize: ","
            aggregationStrategy: GroupedBodyAggregationStrategy
            steps:
              - setHeader:
                  name: index
                  exchangeProperty: CamelSplitIndex
              - setBody:
                  simple: "${header.index}=${body}"
```

<!-- input: exchangeProperty-split-index -->
```text
a,b,c
```

<!-- output: exchangeProperty-split-index -->
```text
["0=a","1=b","2=c"]
```

<!-- fixture: exchangeProperty-msg -->
```json
{"body": "x", "properties": {"tenant": "acme", "attempt": 2}}
```

<!-- eval: exchangeProperty-values fixture=exchangeProperty-msg language=exchangeProperty -->
| expression | result |
|---|---|
| `tenant` | `acme` |
| `attempt` | `2` |
| `missing` | (empty) |

## Options

The exchangeProperty language has no options.

## Rust DSL

```rust
b.from("direct:property-rust")
    .set_property("original", body())
    .set_body(constant("replaced"))
    .set_body(exchange_property("original"));
```

## Common questions

### What is the difference between a header and an exchange property?

Headers belong to the message and are sent to endpoints (HTTP headers, Kafka headers...).
Exchange properties stay inside Cameleer for the life of the exchange.

### How do I read the split index or the loop counter?

Use `exchangeProperty: CamelSplitIndex` (or `CamelSplitSize`, `CamelSplitComplete`,
`CamelLoopIndex`).

## Related

- [Simple language](simple.md) (`${exchangeProperty.name}`)
- [Header language](header.md), [variable language](variable.md)
- [Apache Camel ExchangeProperty language](https://camel.apache.org/components/next/languages/exchangeProperty-language.html)
