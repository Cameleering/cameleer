# Mustache component

> The `mustache` component renders a Mustache template against the message (body, headers, properties) and sets the rendered text as the new body.

**Scheme:** `mustache:` · **Crate:** `cameleer-core` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [Mustache](https://camel.apache.org/components/next/mustache-component.html)

## When to use it

- Build an LLM prompt from a JSON request: fill a prompt template with the user's question
  and context documents before calling a model.
- Render an email, a Slack message or a report from structured data.
- Produce a text payload (CSV line, SQL, XML snippet) without code.
- Keep wording in a file that non-developers can edit, separate from the route.

## URI format

```text
mustache:templateResource[?options]
```

`templateResource` is a file path relative to the working directory, `file:path`,
`classpath:path` (read as a relative path, there is no classpath) or `ref:beanName` (a
`String` bound in the registry).

## Example

A support agent receives `{"customer": ..., "question": ..., "docs": [...]}` and turns it into
a prompt for a model. The body is unmarshalled from JSON first so the template can read its
fields.

<!-- file: templates/support-prompt.mustache -->
```text
You are a support assistant for {{headers.product}}.
Customer: {{body.customer}}
Question: {{body.question}}
{{#body.docs}}
- {{title}}: {{text}}
{{/body.docs}}
{{^body.docs}}
No documents matched.
{{/body.docs}}
Answer in at most {{headers.maxWords}} words.
```

<!-- example: mustache-llm-prompt -->
```yaml
- route:
    id: mustache-prompt
    from:
      uri: direct:mustache-prompt
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: product
            constant: Cameleer
        - setHeader:
            name: maxWords
            constant: "80"
        - to: mustache:templates/support-prompt.mustache
```

<!-- input: mustache-llm-prompt -->
```json
{"customer":"Ada","question":"How do I retry a failed HTTP call?","docs":[{"title":"Error handling","text":"Use onException with maximumRedeliveries."},{"title":"HTTP","text":"Set throwExceptionOnFailure=true."}]}
```

<!-- output: mustache-llm-prompt -->
```text
You are a support assistant for Cameleer.
Customer: Ada
Question: How do I retry a failed HTTP call?
- Error handling: Use onException with maximumRedeliveries.
- HTTP: Set throwExceptionOnFailure=true.
Answer in at most 80 words.
```

With no documents, the inverted section renders instead:

<!-- input: mustache-llm-prompt#2 -->
```json
{"customer":"Bo","question":"Is there a Kafka component?","docs":[]}
```

<!-- output: mustache-llm-prompt#2 -->
```text
You are a support assistant for Cameleer.
Customer: Bo
Question: Is there a Kafka component?
No documents matched.
Answer in at most 80 words.
```

### Template from a header

With `allowTemplateFromHeader=true` the `CamelMustacheTemplate` header replaces the template
file, so a route can render templates stored elsewhere (a database, a config service).

`{{...}}` inside a route file is a property placeholder, so a template written in the route
itself uses other delimiters (`startDelimiter` / `endDelimiter`). Template files are not
affected.

<!-- example: mustache-template-from-header -->
```yaml
- route:
    id: mustache-header
    from:
      uri: direct:mustache-header
      steps:
        - setHeader:
            name: CamelMustacheTemplate
            constant: "Hello [[body]]!"
        - to:
            uri: mustache:templates/support-prompt.mustache
            parameters:
              allowTemplateFromHeader: true
              startDelimiter: "[["
              endDelimiter: "]]"
```

<!-- input: mustache-template-from-header -->
```text
World
```

<!-- output: mustache-template-from-header -->
```text
Hello World!
```

## Rust DSL

```rust
b.from("direct:mustache-rust")
    .to("mustache:templates/support-prompt.mustache?contentCache=true");
```

## Options

| option | default | description |
|---|---|---|
| `allowTemplateFromHeader` | `false` | Use the `CamelMustacheTemplate` / `CamelMustacheResourceUri` headers when present. |
| `contentCache` | `false` | Read the template once and keep it; otherwise it is re-read for every message. |
| `allowContextMapAll` | `false` | Add `camelContext` (with `name`) to the data model. |
| `startDelimiter` | `{{` | Opening tag delimiter. |
| `endDelimiter` | `}}` | Closing tag delimiter. |
| `encoding` | UTF-8 | Accepted; templates are always read as UTF-8. |

### Data model

| name | value |
|---|---|
| `body` | the message body (an object after `unmarshal: json`, so `body.field` works) |
| `headers` | the message headers (`headers.name`) |
| `variables` | the exchange variables |
| `exchange` | `exchange.properties`, `exchange.exchangeId`, `exchange.pattern`, `exchange.message` |
| `request`, `in` | `body`, `headers`, `messageId` of the message |
| `camelContext` | `camelContext.name`, only with `allowContextMapAll=true` |

### Supported Mustache features

Escaped `{{name}}` and unescaped `{{{name}}}` / `{{& name}}` variables, dotted names, the
implicit iterator `{{.}}`, sections over lists, objects and booleans, inverted sections
`{{^x}}`, comments `{{! ... }}`, partials `{{> name}}` (loaded from the template's directory as
`name` or `name.mustache`), set-delimiter tags `{{=<% %>=}}` and standalone-line removal.
Lambdas are not supported.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelMustacheTemplate` | in | Template text used instead of the endpoint's resource (needs `allowTemplateFromHeader=true`). Removed after use. |
| `CamelMustacheResourceUri` | in | Template resource used instead of the endpoint's one (needs `allowTemplateFromHeader=true`). |

## Run it with Docker

Put the route in `prompt.camel.yaml` and the template in `templates/` next to it:

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/prompt.camel.yaml
```

The image's working directory is `/routes`, so `templates/support-prompt.mustache` resolves to
the mounted file.

## Common questions

### How do I read JSON fields in a Mustache template?

Unmarshal the body first (`unmarshal: json: {}`); then `{{body.field}}` and sections over
`body.list` work. A plain JSON string body has no fields.

### Why does `{{body.text}}` turn `<` into `&lt;`?

Double braces HTML-escape the value, as in every Mustache implementation. Use `{{{body.text}}}`
or `{{& body.text}}` for raw text, which is what you want for prompts and plain text.

### Why do I get "Property with key [x] not found"?

The template text is inside the route file, where `{{x}}` is a property placeholder resolved
when the route starts. Keep templates in files, or switch delimiters with `startDelimiter` and
`endDelimiter` as in the header example above.

### Does the template reload when I edit the file?

Yes, by default the file is read for every message. Set `contentCache=true` to read it once.

## Related

- [FreeMarker](freemarker.md), [Velocity](velocity.md), [StringTemplate](string-template.md),
  [MVEL](mvel.md): other template engines.
- [JSON data format](../dataformats/json.md): unmarshal JSON before rendering.
- [Apache Camel Mustache component](https://camel.apache.org/components/next/mustache-component.html)
