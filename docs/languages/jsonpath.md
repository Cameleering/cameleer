# JSONPath language

> The `jsonpath` language selects values from a JSON document with JSONPath expressions such as `$.choices[0].message.content`.

**Name:** `jsonpath` · **Crate:** `cameleer-core` · **Apache Camel:** [JSONPath](https://camel.apache.org/components/next/languages/jsonpath-language.html)

## When to use it

- Pick a field out of a JSON message: an order id, a status, a price.
- Read the answer of an LLM API: `$.choices[0].message.content` of an OpenAI-style chat
  completion, or the arguments of a tool call.
- Filter or route JSON messages with a predicate (`[?(@.price < 10)]`, or "does this path
  exist").
- Split a JSON array into one message per element.

## Syntax

Cameleer implements the Jayway JsonPath syntax that Apache Camel uses, written from scratch.

| syntax | meaning |
|---|---|
| `$` | the root (the body) |
| `.name`, `['name']`, `["name"]` | a child by name |
| `['a','b']` | several children (union) |
| `[n]`, `[-n]` | array element; negative counts from the end |
| `[a,b]` | several elements |
| `[start:end:step]` | slice |
| `[*]`, `.*` | all elements or children |
| `..name`, `..*`, `..[n]` | recursive descent |
| `[?(<filter>)]` | filter: `@` is the current item, `$` the root |
| `.length()` `.size()` `.min()` `.max()` `.avg()` `.sum()` `.stddev()` `.keys()` `.first()` `.last()` | trailing functions |

Filter operators: `==`, `!=`, `<`, `<=`, `>`, `>=`, `=~` (regex, `/.../` or `/.../i`), `in`,
`nin`, `contains`, `size`, `empty`, `anyof`, `noneof`, `subsetof`, combined with `&&`, `||`,
`!` and parentheses. A bare path (`[?(@.isbn)]`) tests that the field exists.

**Result shape.** A definite path (no wildcard, recursion, slice, filter or union) returns a
single value. Any other path returns an array, possibly empty. A definite path that does not
exist raises `PathNotFoundException` unless `suppressExceptions` is true; as a predicate it is
simply false.

**Input.** The body can be a structured value or JSON text (a string or bytes); text is parsed
first.

### Syntax reference, tested

<!-- fixture: jsonpath-store -->
```json
{"body": {"store": {"book": [
  {"category": "reference", "author": "Nigel Rees", "title": "Sayings of the Century", "price": 8.95},
  {"category": "fiction", "author": "Evelyn Waugh", "title": "Sword of Honour", "price": 12.99},
  {"category": "fiction", "author": "Herman Melville", "title": "Moby Dick", "isbn": "0-553-21311-3", "price": 8.99}],
  "bicycle": {"color": "red", "price": 19.95}}}}
```

<!-- eval: jsonpath-syntax fixture=jsonpath-store language=jsonpath -->
| expression | result |
|---|---|
| `$.store.book[0].title` | `Sayings of the Century` |
| `$['store']['bicycle']['color']` | `red` |
| `$.store.book[-1].title` | `Moby Dick` |
| `$.store.book[*].author` | `["Nigel Rees","Evelyn Waugh","Herman Melville"]` |
| `$.store.book[0,2].title` | `["Sayings of the Century","Moby Dick"]` |
| `$.store.book[0:2].title` | `["Sayings of the Century","Sword of Honour"]` |
| `$.store.bicycle['color','price']` | `["red",19.95]` |
| `$.store.*.color` | `["red"]` |
| `$..price` | `[8.95,12.99,8.99,19.95]` |
| `$.store.book[?(@.price < 10)].title` | `["Sayings of the Century","Moby Dick"]` |
| `$.store.book[?(@.isbn)].title` | `["Moby Dick"]` |
| `$.store.book[?(@.category == 'fiction' && @.price > 10)].author` | `["Evelyn Waugh"]` |
| `$.store.book[?(!(@.price < 10))].title` | `["Sword of Honour"]` |
| `$.store.book[?(@.author =~ /.*Melville/)].title` | `["Moby Dick"]` |
| `$.store.book[?(@.category in ['reference'])].title` | `["Sayings of the Century"]` |
| `$.store.book[?(@.category nin ['reference'])].title` | `["Sword of Honour","Moby Dick"]` |
| `$..book[?(@.title size 9)].title` | `["Moby Dick"]` |
| `$.store.book[?(@.price < $.store.bicycle.price)].category` | `["reference","fiction","fiction"]` |
| `$.store.book.length()` | `3` |
| `$.store.book[*].price.sum()` | `30.93` |
| `$.store.book[*].price.avg()` | `10.31` |
| `$..price.max()` | `19.95` |
| `$.store.bicycle.keys()` | `["color","price"]` |
| `$.store.book[*].title.first()` | `Sayings of the Century` |
| `$.store.missing` | `error: PathNotFoundException` |

As predicates (used by `filter`, `choice`, `validate`): a non-empty result is true, a missing
path is false.

<!-- eval: jsonpath-predicates fixture=jsonpath-store language=jsonpath predicate -->
| expression | result |
|---|---|
| `$.store.book[?(@.price > 10)]` | `true` |
| `$.store.book[?(@.price > 100)]` | `false` |
| `$.store.bicycle.color` | `true` |
| `$.store.missing` | `false` |

## Examples

### Read the answer of an LLM chat completion

<!-- example: jsonpath-llm-answer -->
```yaml
- route:
    id: jsonpath-llm-answer
    from:
      uri: direct:llm-answer
      steps:
        - setHeader:
            name: finishReason
            jsonpath: "$.choices[0].finish_reason"
        - setBody:
            jsonpath: "$.choices[0].message.content"
        - setBody:
            simple: "${body} (${header.finishReason})"
```

<!-- input: jsonpath-llm-answer -->
```json
{"id":"chatcmpl-1","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"Refund approved"}}]}
```

<!-- output: jsonpath-llm-answer -->
```text
Refund approved (stop)
```

### Route on a field, with a default when it is missing

<!-- example: jsonpath-route-on-field -->
```yaml
- route:
    id: jsonpath-route
    from:
      uri: direct:jsonpath-route
      steps:
        - choice:
            when:
              - jsonpath: "$.order[?(@.total > 100)]"
                steps:
                  - setBody:
                      constant: review
              - jsonpath:
                  expression: "$.order.express"
                  suppressExceptions: true
                steps:
                  - setBody:
                      constant: express
            otherwise:
              steps:
                - setBody:
                    constant: standard
```

<!-- input: jsonpath-route-on-field -->
```json
{"order":{"id":1,"total":250}}
```

<!-- output: jsonpath-route-on-field -->
```text
review
```

<!-- input: jsonpath-route-on-field#2 -->
```json
{"order":{"id":2,"total":20,"express":true}}
```

<!-- output: jsonpath-route-on-field#2 -->
```text
express
```

<!-- input: jsonpath-route-on-field#3 -->
```json
{"order":{"id":3,"total":20}}
```

<!-- output: jsonpath-route-on-field#3 -->
```text
standard
```

### Split tool calls into one message each

<!-- example: jsonpath-split-tool-calls -->
```yaml
- route:
    id: jsonpath-split
    from:
      uri: direct:tool-calls
      steps:
        - split:
            jsonpath: "$.choices[0].message.tool_calls[*]"
            aggregationStrategy: GroupedBodyAggregationStrategy
            steps:
              - setBody:
                  simple: "${body.function.name}(${body.function.arguments})"
```

<!-- input: jsonpath-split-tool-calls -->
```json
{"choices":[{"message":{"tool_calls":[
  {"id":"c1","function":{"name":"get_weather","arguments":"{\"city\":\"Paris\"}"}},
  {"id":"c2","function":{"name":"get_time","arguments":"{\"tz\":\"CET\"}"}}]}}]}
```

<!-- output: jsonpath-split-tool-calls -->
```text
["get_weather({\"city\":\"Paris\"})","get_time({\"tz\":\"CET\"})"]
```

### Read from a header instead of the body

<!-- example: jsonpath-from-header -->
```yaml
- route:
    id: jsonpath-header-source
    from:
      uri: direct:jsonpath-header
      steps:
        - setHeader:
            name: user
            simple: '{"name":"Ada","roles":["admin","dev"]}'
        - setBody:
            jsonpath:
              expression: "$.roles.length()"
              headerName: user
              resultType: String
```

<!-- input: jsonpath-from-header -->
```text
ignored
```

<!-- output: jsonpath-from-header -->
```text
2
```

## Options

| option | default | description |
|---|---|---|
| `resultType` | (none) | Convert the result, e.g. `String`, `Integer`, `Double`, `Boolean`. |
| `suppressExceptions` | `false` | Return null instead of failing when a definite path does not exist or the input is null. |
| `headerName` | (none) | Evaluate the header with this name instead of the body. |
| `propertyName` | (none) | Evaluate this exchange property. |
| `variableName` | (none) | Evaluate this variable. |
| `source` | (none) | `header:name`, `property:name` (or `exchangeProperty:name`), `variable:name` or `body`. |
| `unpackArray` | `false` | A one-element array result becomes the element itself. |
| `writeAsString` | `false` | Object and array results become JSON text. |
| `allowSimple` | `false` | Accepted; with it a null input gives null instead of an error. |

## Rust DSL

```rust
b.from("direct:jsonpath-rust")
    .set_header("finishReason", jsonpath("$.choices[0].finish_reason"))
    .set_body(jsonpath_as("$.choices[0].message.content", "String"));
```

## Common questions

### How do I get the text of an OpenAI chat completion?

`jsonpath: "$.choices[0].message.content"`. For tool calls use
`$.choices[0].message.tool_calls[*]` and split on it.

### Why do I get an array when I expected one value?

Wildcards, filters, slices, unions and `..` always return an array. Add `unpackArray: true`,
take an element in a later step, or end the path with `.first()`.

### How do I avoid an exception when a field is missing?

Set `suppressExceptions: true` to get null. In a `filter` or `choice` a missing path is already
false.

### Can I use JSONPath inside a Simple template?

Yes: `${jsonpath('$.order.id')}` evaluates against the body inside [Simple](simple.md).

## Related

- [jq language](jq.md): transformations as well as queries
- [Simple language](simple.md) and its `jsonpath(...)` function
- [JSON data format](../dataformats/json.md), [json-validator component](../components/json-validator.md)
- [Apache Camel JSONPath language](https://camel.apache.org/components/next/languages/jsonpath-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
