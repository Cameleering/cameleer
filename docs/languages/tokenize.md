# Tokenize language

> The `tokenize` language cuts text into a list of pieces (by a token, a regular expression, a start and end token, or an XML element name), mostly to drive the `split` EIP.

**Name:** `tokenize` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) · **Apache Camel:** [Tokenize](https://camel.apache.org/components/next/languages/tokenize-language.html)

## When to use it

- Process a file line by line: `split` with `tokenize: "\n"`.
- Send records in batches, for example 100 lines per request to an embedding API, with `group`.
- Skip a CSV header line with `skipFirst`.
- Cut repeated XML elements out of a large document (`xml: true`) without parsing it as a whole.

## Syntax

The expression text, if any, is the name of a header to tokenize; otherwise the body is used.
Everything else is set with options. In YAML, `tokenize: ","` is short for
`tokenize: {token: ","}`.

| mode | options | result for the input |
|---|---|---|
| token (default) | `token` (default `,`) | text split on the token; a trailing empty piece is dropped |
| regex | `token` + `regex: true` | text split on the regular expression |
| pair | `token` + `endToken` (+ `includeTokens`) | the text between each start and end token |
| xml | `token: <item>` + `xml: true` | each whole `<item>...</item>` element, nesting aware |

`group: N` joins every N pieces with `groupDelimiter` (default: the token in token mode,
nothing in pair and xml mode). `skipFirst: true` drops the first piece. `\n`, `\r` and `\t`
in `token`, `endToken` and `groupDelimiter` are unescaped. A body that is already a list is
passed through (only `skipFirst` applies).

## Examples

### Lines in groups, skipping a header

<!-- example: tokenize-lines-grouped -->
```yaml
- route:
    id: tokenize-lines
    from:
      uri: direct:tokenize-lines
      steps:
        - split:
            tokenize:
              token: "\\n"
              skipFirst: true
              group: 2
            aggregationStrategy: GroupedBodyAggregationStrategy
            steps:
              - setBody:
                  simple: "batch[${body.replace('\n', '|')}]"
```

<!-- input: tokenize-lines-grouped -->
```text
id,name
1,Ada
2,Grace
3,Linus
```

<!-- output: tokenize-lines-grouped -->
```text
["batch[1,Ada|2,Grace]","batch[3,Linus]"]
```

### Regular expression

<!-- example: tokenize-regex -->
```yaml
- route:
    id: tokenize-regex
    from:
      uri: direct:tokenize-regex
      steps:
        - setBody:
            tokenize:
              token: "\\s*[;,]\\s*"
              regex: true
```

<!-- input: tokenize-regex -->
```text
red ; green,blue  ,  black
```

<!-- output: tokenize-regex -->
```text
["red","green","blue","black"]
```

### Text between start and end tokens

Pull the code blocks out of a model's Markdown answer:

<!-- example: tokenize-pair -->
```yaml
- route:
    id: tokenize-pair
    from:
      uri: direct:tokenize-pair
      steps:
        - setBody:
            tokenize:
              token: "<sql>"
              endToken: "</sql>"
```

<!-- input: tokenize-pair -->
```text
First run <sql>SELECT 1</sql>, then <sql>SELECT 2</sql>.
```

<!-- output: tokenize-pair -->
```text
["SELECT 1","SELECT 2"]
```

### XML elements

<!-- example: tokenize-xml -->
```yaml
- route:
    id: tokenize-xml
    from:
      uri: direct:tokenize-xml
      steps:
        - split:
            tokenize:
              token: "<order>"
              xml: true
            aggregationStrategy: GroupedBodyAggregationStrategy
            steps:
              - setBody:
                  xpath:
                    expression: /order/@id
                    resultType: String
```

<!-- input: tokenize-xml -->
```xml
<orders><order id="1"><line/></order><order id="2"/><order id="3">x</order></orders>
```

<!-- output: tokenize-xml -->
```text
["1","2","3"]
```

### Tokenize a header

<!-- example: tokenize-header -->
```yaml
- route:
    id: tokenize-header
    from:
      uri: direct:tokenize-header
      steps:
        - setBody:
            tokenize:
              expression: recipients
              token: ";"
```

<!-- input: tokenize-header {"headers":{"recipients":"ada@example.com;grace@example.com"}} -->
```text
ignored
```

<!-- output: tokenize-header -->
```text
["ada@example.com","grace@example.com"]
```

## Options

| option | default | description |
|---|---|---|
| `token` | `,` | The separator, regex, start token or element name. |
| `endToken` | (none) | End token: pair mode. |
| `includeTokens` | `false` | In pair mode, keep the start and end tokens in each piece. |
| `regex` | `false` | `token` is a regular expression. |
| `xml` | `false` | `token` is an element name (`<order>` or `order`); whole elements are returned. |
| `group` | `0` | Join every N pieces into one. |
| `groupDelimiter` | the token | Separator used when grouping. |
| `skipFirst` | `false` | Drop the first piece (a header line). |
| `headerName` / `source` | (none) | Tokenize this header instead of the body. |
| `inheritNamespaceTagName` | | Accepted and ignored. |

## Rust DSL

```rust
b.from("direct:tokenize-rust")
    .split(tokenize("\n"))
    .to("mock:tokenize-line")
    .end()
    .set_body(tokenize_pair("[", "]", false));
```

Also `tokenize_regex(regex)` and `tokenize_xml(tag)`; add `group`/`skipFirst` with
`LanguageOptions::with_option`.

## Common questions

### How do I split a file into lines?

`split: {tokenize: "\n"}`. Add `group: 100` to process 100 lines per exchange, and
`skipFirst: true` to skip a header line.

### How do I split a big XML file into records?

`tokenize: {token: "<record>", xml: true}` returns each `<record>` element as text, which you
can then query with [XPath](xpath.md).

### Does Cameleer stream the tokens?

No. The tokens of one body are computed as a list, so the whole body is held in memory. This
differs from Apache Camel, where tokenize can scan a stream lazily.

## Related

- [Split EIP](../guides/eips.md), [XPath language](xpath.md), [Simple language](simple.md)
  (`${split(...)}`, `${collate(n)}`)
- [CSV data format](../dataformats/csv.md)
- [Apache Camel Tokenize language](https://camel.apache.org/components/next/languages/tokenize-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
