# jq language

> The `jq` language queries and transforms JSON with jq 1.7 programs such as `.items | map(.price) | add`.

**Name:** `jq` · **Crate:** `cameleer-core` · **Cargo feature:** [`jq`](../guides/build-features.md) · **Apache Camel:** [JQ](https://camel.apache.org/components/next/languages/jq-language.html)

## When to use it

- Reshape JSON in one expression: pick fields, rename them, build a new object for the next API.
- Clean up an LLM's JSON answer: parse the tool-call `arguments` string with `fromjson`, apply
  defaults with `//`, keep only the fields the next step accepts.
- Compute values: sums, counts, grouping, sorting.
- Filter and route with a predicate such as `.priority == "P1"`.

## Syntax

Cameleer contains its own implementation of the jq 1.7 language (Apache Camel uses
jackson-jq). Supported:

- paths `.`, `..`, `.foo`, `."foo"`, `.[e]`, `.[a:b]`, `.[]`, optional `?`
- pipes `|`, comma `,`, alternative `//`, arithmetic, comparison and boolean operators
- assignment `=`, `|=`, `+=`, `-=`, `*=`, `/=`, `%=`, `//=`
- array and object construction, string interpolation `"\(.x)"`
- formats `@text @json @html @uri @csv @tsv @sh @base64 @base64d @base32 @base32d`
- `if/elif/else/end`, `try/catch`, `reduce`, `foreach`, `label`/`break`, variables and
  destructuring, `?//`, function definitions with recursion, `$ENV`, `$__loc__`, comments
- the jq 1.7 builtins except module and I/O functions: `length keys has in map map_values
  path paths getpath setpath del to_entries from_entries with_entries select empty error add
  any all flatten range floor sqrt pow log tostring tonumber type sort sort_by group_by unique
  unique_by min max min_by max_by reverse contains indices index startswith endswith ltrimstr
  rtrimstr trim explode implode split join ascii_downcase ascii_upcase recurse transpose first
  last nth until while repeat limit walk tojson fromjson test match capture scan splits sub
  gsub todate fromdate now mktime gmtime strftime strptime abs toarray IN INDEX`, math
  functions and more

Camel functions (as in camel-jq): `header(name)`, `header(name; default)`,
`property(name)`, `property(name; default)`, `variable(name)`.

**Result.** One output gives that value, no output gives null, several outputs give an
array of all outputs. `input` and `inputs` fail: there is one input per exchange.

**Input.** A structured body is used as is. A text or bytes body is parsed as JSON, so a
plain text body that is not JSON fails with a parse error.

### Syntax reference, tested

<!-- fixture: jq-store -->
```json
{"body": {"store": {"book": [
  {"category": "reference", "author": "Nigel Rees", "title": "Sayings of the Century", "price": 8.95},
  {"category": "fiction", "author": "Evelyn Waugh", "title": "Sword of Honour", "price": 12.99},
  {"category": "fiction", "author": "Herman Melville", "title": "Moby Dick", "isbn": "0-553-21311-3", "price": 8.99}],
  "bicycle": {"color": "red", "price": 19.95}}},
 "headers": {"tenant": "acme"}, "properties": {"region": "eu"}, "variables": {"user": "ada"}}
```

<!-- eval: jq-syntax fixture=jq-store language=jq -->
| expression | result |
|---|---|
| `.store.book[0].title` | `Sayings of the Century` |
| `.store.book[].author` | `["Nigel Rees","Evelyn Waugh","Herman Melville"]` |
| `.store.book \| length` | `3` |
| `[.store.book[] \| select(.price < 10) \| .title]` | `["Sayings of the Century","Moby Dick"]` |
| `.store.book \| map(.price) \| add` | `30.93` |
| `{color: .store.bicycle.color, books: (.store.book \| length)}` | `{"color":"red","books":3}` |
| `.store.book \| group_by(.category) \| map({(.[0].category): length}) \| add` | `{"fiction":2,"reference":1}` |
| `.store.book \| sort_by(.price) \| .[0].title` | `Sayings of the Century` |
| `[.store.book[].category] \| unique` | `["fiction","reference"]` |
| `.store.bicycle \| keys` | `["color","price"]` |
| `.store.bicycle \| to_entries \| map("\(.key)=\(.value)") \| join("&")` | `color=red&price=19.95` |
| `.store.book[0].isbn // "n/a"` | `n/a` |
| `if .store.bicycle.price > 15 then "pricey" else "cheap" end` | `pricey` |
| `[.store.book[].title \| ascii_upcase \| .[0:4]]` | `["SAYI","SWOR","MOBY"]` |
| `reduce .store.book[] as $b (0; . + $b.price) \| floor` | `30` |
| `.store.book[1].title \| test("honou?r"; "i")` | `true` |
| `.store.book[2].author \| sub("Herman "; "H. ")` | `H. Melville` |
| `.store.bicycle \| del(.price)` | `{"color":"red"}` |
| `.store.bicycle.price \|= . * 2 \| .store.bicycle` | `{"color":"red","price":39.9}` |
| `.store.bicycle \| tojson` | `{"color":"red","price":19.95}` |
| `.store.book[0] \| [.author, .price] \| @csv` | `"Nigel Rees",8.95` |
| `[paths(type == "number")] \| length` | `4` |
| `.store.bicycle.color, .store.bicycle.price` | `["red",19.95]` |
| `.store.missing` | (empty) |
| `header("tenant")` | `acme` |
| `header("missing"; "none")` | `none` |
| `property("region")` | `eu` |
| `variable("user")` | `ada` |

As predicates, the expression is true when any output is truthy (not `false` or `null`):

<!-- eval: jq-predicates fixture=jq-store language=jq predicate -->
| expression | result |
|---|---|
| `.store.bicycle.price > 15` | `true` |
| `any(.store.book[]; .price > 20)` | `false` |
| `.store.book[2] \| has("isbn")` | `true` |
| `.store.missing` | `false` |

## Examples

### Parse tool-call arguments from an LLM answer

The `arguments` field of an OpenAI-style tool call is a JSON string; `fromjson` parses it.

<!-- example: jq-tool-call-arguments -->
```yaml
- route:
    id: jq-tool-call
    from:
      uri: direct:jq-tool-call
      steps:
        - setHeader:
            name: tool
            jq: ".choices[0].message.tool_calls[0].function.name"
        - setBody:
            jq: '.choices[0].message.tool_calls[0].function.arguments | fromjson | {city, units: (.units // "metric")}'
```

<!-- input: jq-tool-call-arguments -->
```json
{"choices":[{"message":{"tool_calls":[{"id":"c1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Paris\"}"}}]}}]}
```

<!-- output: jq-tool-call-arguments -->
```text
{"city":"Paris","units":"metric"}
```

### Reshape an order and route on the result

<!-- example: jq-reshape-and-route -->
```yaml
- route:
    id: jq-reshape
    from:
      uri: direct:jq-reshape
      steps:
        - setBody:
            jq: '{id: .orderId, total: (.lines | map(.qty * .price) | add), customer: .customer.name}'
        - choice:
            when:
              - jq: ".total > 100"
                steps:
                  - setHeader:
                      name: lane
                      constant: review
            otherwise:
              steps:
                - setHeader:
                    name: lane
                    constant: auto
        - setBody:
            simple: "${header.lane}: ${body}"
```

<!-- input: jq-reshape-and-route -->
```json
{"orderId":"A-7","customer":{"name":"Ada"},"lines":[{"sku":"x","qty":2,"price":30},{"sku":"y","qty":1,"price":50}]}
```

<!-- output: jq-reshape-and-route -->
```text
review: {"id":"A-7","total":110,"customer":"Ada"}
```

## Options

| option | default | description |
|---|---|---|
| `resultType` | (none) | Convert the result, e.g. `String`, `Integer`, `Boolean`. |
| `headerName` | (none) | Evaluate the header with this name instead of the body. |
| `propertyName` | (none) | Evaluate this exchange property. |
| `variableName` | (none) | Evaluate this variable. |
| `source` | (none) | `header:name`, `property:name`, `variable:name` or `body`. |

## Rust DSL

```rust
b.from("direct:jq-rust")
    .filter(jq(".total > 100"))
    .set_body(jq("{id, total}"));
```

## Common questions

### How do I parse a JSON string that sits inside JSON?

Use `fromjson`, for example `.function.arguments | fromjson`. The reverse is `tojson`.

### How do I give a default when a field is missing?

Use the alternative operator: `.units // "metric"`.

### Can jq read headers?

Yes: `header("name")`, or `header("name"; "default")`. Use `property(...)` and `variable(...)`
for exchange properties and variables.

### Is it real jq?

It is an independent implementation of the jq 1.7 language in Rust, with no `jq` binary
involved. Module (`import`, `include`) and I/O functions are not available.

## Related

- [JSONPath language](jsonpath.md): simpler path queries
- [Simple language](simple.md) and its `jq(...)` function
- [JSON data format](../dataformats/json.md), [json-validator component](../components/json-validator.md)
- [Apache Camel JQ language](https://camel.apache.org/components/next/languages/jq-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
