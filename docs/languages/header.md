# Header language

> The `header` language returns the value of one message header, or null when the header is not set.

**Name:** `header` · **Crate:** `cameleer-core` · **Apache Camel:** [Header](https://camel.apache.org/components/next/languages/header-language.html)

## When to use it

- Copy a header into the body, a property or another header without writing a Simple template.
- Use a header as a predicate in `filter` or `choice`: a header is true when it is set and not
  `false`, `0` or empty.
- Use a header that holds a list as the expression of a `split`.

## Syntax

The expression text is the header name, for example `priority` or `CamelFileName`. Leading
and trailing spaces are trimmed. The value keeps its type: a number stays a number and a list
stays a list. The same value is `${header.priority}` in [Simple](simple.md).

## Examples

Route on a header set by the caller (for example a model's classification of a ticket):

<!-- example: header-route-on-priority -->
```yaml
- route:
    id: header-demo
    from:
      uri: direct:header
      steps:
        - choice:
            when:
              - header: urgent
                steps:
                  - setBody:
                      header: assignee
            otherwise:
              steps:
                - setBody:
                    constant: backlog
```

<!-- input: header-route-on-priority {"headers":{"urgent":true,"assignee":"on-call"}} -->
```text
Checkout is down
```

<!-- output: header-route-on-priority -->
```text
on-call
```

Without the `urgent` header the predicate is false:

<!-- input: header-route-on-priority#2 {"headers":{"assignee":"on-call"}} -->
```text
How do I change my address?
```

<!-- output: header-route-on-priority#2 -->
```text
backlog
```

Split a header that holds a list:

<!-- example: header-split-list -->
```yaml
- route:
    id: header-split
    from:
      uri: direct:header-split
      steps:
        - setHeader:
            name: tags
            simple: "${body.split(',')}"
        - split:
            header: tags
            aggregationStrategy: GroupedBodyAggregationStrategy
            steps:
              - setBody:
                  simple: "tag:${body}"
```

<!-- input: header-split-list -->
```text
red,green,blue
```

<!-- output: header-split-list -->
```text
["tag:red","tag:green","tag:blue"]
```

Values against a fixture message:

<!-- fixture: header-msg -->
```json
{"body": "x", "headers": {"priority": "high", "retries": 3, "tags": ["a", "b"]}}
```

<!-- eval: header-values fixture=header-msg language=header -->
| expression | result |
|---|---|
| `priority` | `high` |
| `retries` | `3` |
| `tags` | `["a","b"]` |
| `missing` | (empty) |

## Options

The header language has no options.

## Rust DSL

```rust
b.from("direct:header-rust")
    .filter(header("urgent"))
    .set_body(header("assignee"));
```

## Common questions

### How do I read a header in a route?

Use `header: name` wherever an expression is expected, or `${header.name}` inside a Simple
template when you need it in a larger text.

### What happens when the header does not exist?

The result is null. As a predicate it is false, and `setBody` with a missing header sets an
empty body.

### Are header names case sensitive?

Yes, names are matched exactly.

## Related

- [Simple language](simple.md) (`${header.name}`, `${headers}`)
- [exchangeProperty language](exchangeProperty.md), [variable language](variable.md),
  [constant language](constant.md)
- [Apache Camel Header language](https://camel.apache.org/components/next/languages/header-language.html)
