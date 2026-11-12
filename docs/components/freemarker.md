# FreeMarker component

> The `freemarker` component renders a FreeMarker (FTL) template against the message and sets the rendered text as the new body.

**Scheme:** `freemarker:` · **Crate:** `cameleer-core` · **Cargo feature:** [`templates`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [FreeMarker](https://camel.apache.org/components/next/freemarker-component.html)

## When to use it

- Build a JSON request for an LLM API from an incoming message, with `?json_string` escaping
  so user text cannot break the JSON.
- Render documents with logic: conditionals, loops with separators, macros, number formats.
- Reuse existing FreeMarker templates from a Java Camel application unchanged.

## URI format

```text
freemarker:templateResource[?options]
```

`templateResource` is a file path relative to the working directory, `file:path`,
`classpath:path` (read as a relative path) or `ref:beanName` (a `String` in the registry).

## Example

Turn a support ticket into an OpenAI-style chat completion request. The template escapes the
ticket text with `?json_string`, lists the tags with `<#sep>` and picks the model by priority.

<!-- file: templates/chat-request.ftl -->
```text
{
  "model": "<#if body.priority == "high">gpt-4o<#else>gpt-4o-mini</#if>",
  "messages": [
    {"role": "system", "content": "Classify the ticket. Tags: <#list body.tags as t>${t}<#sep>, </#list>."},
    {"role": "user", "content": "${body.text?json_string}"}
  ],
  "max_tokens": ${headers.maxTokens}
}
```

<!-- example: freemarker-llm-request -->
```yaml
- route:
    id: freemarker-request
    from:
      uri: direct:freemarker-request
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: maxTokens
            constant: 200
        - to: freemarker:templates/chat-request.ftl
```

<!-- input: freemarker-llm-request -->
```json
{"priority":"high","tags":["billing","refund"],"text":"I was charged twice, see \"order 42\"."}
```

<!-- output: freemarker-llm-request -->
```json
{
  "model": "gpt-4o",
  "messages": [
    {"role": "system", "content": "Classify the ticket. Tags: billing, refund."},
    {"role": "user", "content": "I was charged twice, see \"order 42\"."}
  ],
  "max_tokens": 200
}
```

### Built-ins and defaults

Missing values are errors in FreeMarker. Use `!` for a default and `??` to test presence:

<!-- file: templates/greeting.ftl -->
```text
Hello ${body.name?cap_first}<#if body.vip??> (VIP)</#if>, you have ${body.points!0} points.
```

<!-- example: freemarker-defaults -->
```yaml
- route:
    id: freemarker-defaults
    from:
      uri: direct:freemarker-defaults
      steps:
        - unmarshal:
            json: {}
        - to: freemarker:templates/greeting.ftl
```

<!-- input: freemarker-defaults -->
```json
{"name":"ada","vip":true,"points":120}
```

<!-- output: freemarker-defaults -->
```text
Hello Ada (VIP), you have 120 points.
```

<!-- input: freemarker-defaults#2 -->
```json
{"name":"bo"}
```

<!-- output: freemarker-defaults#2 -->
```text
Hello Bo, you have 0 points.
```

## Rust DSL

```rust
b.from("direct:freemarker-rust")
    .unmarshal(DataFormatDefinition::json())
    .to("freemarker:templates/greeting.ftl");
```

## Options

| option | default | description |
|---|---|---|
| `allowTemplateFromHeader` | `false` | Use the `CamelFreemarkerTemplate` / `CamelFreemarkerResourceUri` headers when present. |
| `contentCache` | `false` | Read the template once; otherwise it is re-read for every message. |
| `allowContextMapAll` | `false` | Add `camelContext` (with `name`) to the data model. |
| `encoding` | UTF-8 | Accepted; templates are always read as UTF-8. |
| `configuration`, `templateUpdateDelay`, `localizedLookup` | | Accepted and ignored (Java FreeMarker settings). |

The data model is the same as for the other template components: `body`, `headers`,
`variables`, `exchange` (`exchange.properties`, `exchange.exchangeId`...), `request` / `in`,
and `camelContext` with `allowContextMapAll=true`.

### Supported FTL

Interpolations `${...}`, comments `<#-- -->`, `if/elseif/else`, `list` (with `items`, `sep`,
`else`, `break`, `continue`), `assign/global/local`, `macro` and `<@name/>` calls, `function`,
`include`, `import`, `switch/case`, `attempt/recover`, `compress`, `noparse`, HTML/XML
auto-escaping (`outputformat`, `autoesc`), `setting` for `number_format` and `boolean_format`;
the string, number, boolean, sequence and hash built-ins (`upper_case`, `cap_first`,
`json_string`, `replace`, `split`, `string("0.00")`, `size`, `join`, `sort_by`, `filter`,
`map`, `keys`...), defaults `x!d` and missing tests `x??`. **Dates and times are not
supported.**

## Message headers

| header | direction | description |
|---|---|---|
| `CamelFreemarkerTemplate` | in | Template text used instead of the resource (needs `allowTemplateFromHeader=true`). |
| `CamelFreemarkerResourceUri` | in | Template resource used instead of the endpoint's one (needs `allowTemplateFromHeader=true`). |
| `CamelFreemarkerDataModel` | in | A map used as the whole data model instead of the default one. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/chat-request.camel.yaml
```

Relative template paths resolve against `/routes`, the image's working directory.

## Common questions

### How do I put user text inside a JSON template safely?

Use `${value?json_string}`; it escapes quotes, backslashes and control characters. The example
above embeds a ticket that contains quotes.

### Why does my template fail with "has evaluated to null or missing"?

FreeMarker treats a missing value as an error. Give a default (`${body.points!0}`) or test it
first (`<#if body.vip??>`).

### Can I keep a FreeMarker template inside the YAML route?

Use `allowTemplateFromHeader=true` and set `CamelFreemarkerTemplate` with `constant:`. Do not
use `simple:` for it: Simple would evaluate the `${...}` interpolations itself. Files are
easier to read and edit.

## Related

- [Mustache](mustache.md), [Velocity](velocity.md), [StringTemplate](string-template.md),
  [MVEL](mvel.md).
- [JSON data format](../dataformats/json.md).
- [Apache Camel FreeMarker component](https://camel.apache.org/components/next/freemarker-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
