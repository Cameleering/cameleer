# StringTemplate component

> The `string-template` component renders a StringTemplate 4 template against the message and sets the rendered text as the new body.

**Scheme:** `string-template:` · **Crate:** `cameleer-core` · **Cargo feature:** [`templates`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [String Template](https://camel.apache.org/components/next/string-template-component.html)

## When to use it

- Render text where the template must stay logic-free: only attribute references, conditions
  on presence and list application.
- Produce lists with separators (`; separator=", "`) without loop code.
- Reuse StringTemplate files from a Java Camel application.

## URI format

```text
string-template:templateResource[?options]
```

`templateResource` is a file path relative to the working directory, `file:path`,
`classpath:path` (read as a relative path) or `ref:beanName`.

## Example

Summarize the tools an agent may call. Attributes use `<...>`, an anonymous sub-template is
applied to each list item, and `if` tests presence.

<!-- file: templates/tools.st -->
```text
Agent <body.name> may call: <body.tools; separator=", ">.
<body.tools:{t | <i>. <t>}; separator="\n">
<if(headers.readonly)>Read-only mode: do not call write tools.<endif>
```

<!-- example: string-template-tool-list -->
```yaml
- route:
    id: string-template-tools
    from:
      uri: direct:string-template-tools
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: readonly
            constant: "yes"
        - to: string-template:templates/tools.st
```

<!-- input: string-template-tool-list -->
```json
{"name":"triage","tools":["search_docs","read_ticket","close_ticket"]}
```

<!-- output: string-template-tool-list -->
```text
Agent triage may call: search_docs, read_ticket, close_ticket.
1. search_docs
2. read_ticket
3. close_ticket
Read-only mode: do not call write tools.
```

### Other delimiters

`delimiterStart` and `delimiterStop` change `<` and `>`, which helps when the output is HTML
or XML:

<!-- file: templates/item.st -->
```text
<li>$body$</li>
```

<!-- example: string-template-delimiters -->
```yaml
- route:
    id: string-template-dollar
    from:
      uri: direct:string-template-dollar
      steps:
        - to: string-template:templates/item.st?delimiterStart=$&delimiterStop=$
```

<!-- input: string-template-delimiters -->
```text
Cameleer
```

<!-- output: string-template-delimiters -->
```text
<li>Cameleer</li>
```

## Rust DSL

```rust
b.from("direct:string-template-rust")
    .to("string-template:templates/item.st?delimiterStart=$&delimiterStop=$");
```

## Options

| option | default | description |
|---|---|---|
| `delimiterStart` | `<` | Character that opens an expression. |
| `delimiterStop` | `>` | Character that closes an expression. |
| `allowTemplateFromHeader` | `false` | Use the `CamelStringTemplateTemplate` / `CamelStringTemplateResourceUri` headers when present. |
| `contentCache` | `false` | Read the template once; otherwise it is re-read for every message. |
| `allowContextMapAll` | `false` | Add `camelContext` (with `name`) to the data model. |
| `encoding` | UTF-8 | Accepted; templates are always read as UTF-8. |

The data model: `body`, `headers`, `variables`, `exchange` (`exchange.properties`...),
`request` / `in`, and `camelContext` with `allowContextMapAll=true`.

### Supported StringTemplate

Attribute references and properties (`<a.b>`, `<a.(expr)>`), string and list literals,
multi-valued attributes with `separator` and `null` options, anonymous sub-templates
(`<names:{n | <n>}>`, implicit `it`, `<i>` / `<i0>`, chained and zipped application),
`if/elseif/else/endif` with `!`, `&&`, `||`, the functions `first last rest trunc strip length
reverse trim strlen`, special characters (`<\n>`, `<\t>`, `<\uXXXX>`), comments `<! !>` and
escapes. Named template references (`<x:t()>`) need template groups and are not supported.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelStringTemplateTemplate` | in | Template text used instead of the resource (needs `allowTemplateFromHeader=true`). |
| `CamelStringTemplateResourceUri` | in | Template resource used instead of the endpoint's one (needs `allowTemplateFromHeader=true`). |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/tools.camel.yaml
```

## Common questions

### How do I join a list with commas?

`<body.tools; separator=", ">`. Missing items can be replaced with `null="n/a"`.

### How do I write HTML or XML with StringTemplate?

Switch the delimiters with `delimiterStart=$&delimiterStop=$`, so `<` and `>` are plain text.

### Why does `<if(x)>` not compare values?

StringTemplate is logic-free: `if` only tests that an attribute is present, not `false` and not
an empty list. Compute the condition before rendering, for example in a header.

## Related

- [Mustache](mustache.md), [FreeMarker](freemarker.md), [Velocity](velocity.md), [MVEL](mvel.md).
- [Apache Camel String Template component](https://camel.apache.org/components/next/string-template-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
