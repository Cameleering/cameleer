# Python language

> The `python` language evaluates Python 3 expressions and scripts against the message, with `body`, `headers`, `request` and `exchange` available as variables.

**Name:** `python` · **Crate:** `cameleer-script` ·
**Apache Camel:** [Python](https://camel.apache.org/components/next/languages/python-language.html)

## When to use it

- Post-process an LLM's JSON answer with familiar Python: `json.loads`, comprehensions,
  f-strings.
- Let data people who know Python write routing conditions and small transformations.
- Run on edge devices where installing CPython is not an option: the interpreter is part of the
  Cameleer binary.

## Syntax

Cameleer runs Python in its own interpreter, written in Rust. It is not CPython and has no
`pip`: C extensions and third-party packages (`requests`, `numpy`...) are not available. It
covers Python 3 as used in route scripts: indentation blocks, functions, lambdas, closures,
comprehensions, f-strings, slicing, `try`/`except`, classes, generators, `match`/`case`,
`async`/`await`, and the bundled modules `json`, `math`, `re`, `collections`, `functools`,
`itertools`, `dataclasses` and a few more. `json` is available without an import. User modules
can be imported relative to the script directory (`scriptDir` option) or the working directory.

Variables available to the script:

| variable | what it is |
|---|---|
| `body` | the message body |
| `headers` | the message headers (`headers['name']`, `headers.get('name', default)`) |
| `request`, `message` | the message: `request.body`, `request.headers` |
| `exchange` | the exchange: `exchange.getIn().setHeader(...)`, `exchange.setProperty(...)` |
| `properties`, `exchangeProperties` | the exchange properties |
| `variables` | the exchange variables |
| `camelContext` | the context |
| `log` | a logger |

The value of the last expression statement is the result. Used as an expression the script
works on a copy of the message; in a `script` step its changes are kept.

## Examples

### Extract fields from an LLM answer

<!-- example: python-llm-answer -->
```yaml
- route:
    id: python-llm
    from:
      uri: direct:python-llm
      steps:
        - setBody:
            python: |
              answer = json.loads(body)
              actions = [a['tool'] for a in answer['actions'] if a['confidence'] > 0.6]
              f"run {', '.join(actions)} for {answer['user']}"
```

<!-- input: python-llm-answer -->
```json
{"user":"ada","actions":[{"tool":"refund","confidence":0.9},{"tool":"escalate","confidence":0.3},{"tool":"email","confidence":0.8}]}
```

<!-- output: python-llm-answer -->
```text
run refund, email for ada
```

### Route with a Python predicate

<!-- example: python-choice -->
```yaml
- route:
    id: python-choice
    from:
      uri: direct:python-choice
      steps:
        - choice:
            when:
              - python: "len(body) > 20"
                steps:
                  - setBody:
                      python: "body[:20] + '...'"
            otherwise:
              steps:
                - setBody:
                    python: "body.upper()"
```

<!-- input: python-choice -->
```text
a sensor reading that is far too long
```

<!-- output: python-choice -->
```text
a sensor reading tha...
```

<!-- input: python-choice#2 -->
```text
short
```

<!-- output: python-choice#2 -->
```text
SHORT
```

### A script step

<!-- example: python-script-step -->
```yaml
- route:
    id: python-script
    from:
      uri: direct:python-script
      steps:
        - script:
            python: |
              readings = [float(x) for x in body.split(',')]
              exchange.getIn().setHeader('max', max(readings))
              exchange.getIn().setBody(round(sum(readings) / len(readings), 2))
        - setBody:
            simple: "avg ${body}, max ${header.max}"
```

<!-- input: python-script-step -->
```text
21.5,22.25,20.0
```

<!-- output: python-script-step -->
```text
avg 21.25, max 22.25
```

### Expressions

<!-- fixture: python-msg -->
```json
{"body": "a,b,c", "headers": {"amount": 250, "name": "Ada"}}
```

<!-- eval: python-expressions fixture=python-msg language=python -->
| expression | result |
|---|---|
| `body.upper()` | `A,B,C` |
| `headers['amount'] > 100` | `true` |
| `f"Hi {headers['name']}"` | `Hi Ada` |
| `len(body)` | `5` |
| `'\|'.join(reversed(body.split(',')))` | `c\|b\|a` |
| `[x * 2 for x in range(3)]` | `[0,2,4]` |
| `request.headers.get('missing', 'none')` | `none` |

## Options

| option | default | description |
|---|---|---|
| `resultType` | (none) | Convert the result to this type (`int`, `String`, `boolean`...). |
| `scriptDir` | (none) | Directory that `import` of user modules resolves against. |

## Rust DSL

```rust
b.from("direct:python-rust")
    .set_body(cameleer_script::python("body[::-1]"));
```

## Common questions

### Can I use pip packages?

No. Only the bundled standard modules (`json`, `math`, `re`, `collections`, `itertools`,
`functools`, `dataclasses`...) and your own `.py` modules are available.

### How do I return a value from a multi-line script?

End the script with an expression: its value is the result. A `return` at top level is not
needed.

### How do I set a header from Python?

In a `script` step call `exchange.getIn().setHeader('name', value)`.

## Related

- [JavaScript](js.md), [Groovy](groovy.md) script languages.
- [JSON data format](../dataformats/json.md), [jq](jq.md), [JSONPath](jsonpath.md).
- [Apache Camel Python language](https://camel.apache.org/components/next/languages/python-language.html)
