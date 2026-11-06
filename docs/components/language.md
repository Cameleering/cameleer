# Language component

> The `language` component evaluates a script in any supported language (Simple, JSONPath, XPath, jq, ...) and uses the result as the new message body.

**Scheme:** `language:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Language](https://camel.apache.org/components/next/language-component.html)

## When to use it

- Build a reply or a prompt from a template script, for example a prompt for an LLM from message fields.
- Run a script stored in a file (`script=resource:file:/scripts/reply.txt`), so it can change without editing the route.
- Evaluate a script that arrives with the message (header `CamelLanguageScript` or the body).

## URI format

```text
language:languageName[:script][?options]
```

## Example

<!-- example: language-prompt -->
```yaml
- route:
    id: build-prompt
    from:
      uri: direct:prompt
      steps:
        - to: "language:simple:Summarize this support ticket in one sentence: ${body}"
```

Sending `Printer on floor 3 is jammed` returns
`Summarize this support ticket in one sentence: Printer on floor 3 is jammed`.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:prompt")
            .route_id("build-prompt")
            .to("language:simple:Summarize this support ticket in one sentence: ${body}");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `script` | | The script, instead of the URI path. Prefix `resource:file:` (or `file:`) to load it from a file. `classpath:` is not supported. |
| `transform` | `true` | Replace the body with the result. With `false` the message is left unchanged. |
| `resultType` | | Convert the result to this type. |
| `contentCache`, `cacheScript`, `binary`, `allowContextMapAll`, `allowTemplateFromHeader`, `lazyStartProducer` | | Accepted and ignored. |

Without a script in the URI or `script`, the script comes from the `CamelLanguageScript` header, or else
from the message body.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelLanguageScript` | in | The script to evaluate when the endpoint has none. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/prompt.camel.yaml
```

## Common questions

### Which languages can I use?

Any language registered in the context, for example `simple`, `constant`, `header`, `jsonpath`, `jq`,
`xpath` and the script languages of `cameleer-script`.

### How do I keep the body and only run the script for its side effects?

Add `transform=false`.

## Related

- [controlbus](controlbus.md) (`controlbus:language:...`), [dataformat](dataformat.md)
- [Apache Camel Language component](https://camel.apache.org/components/next/language-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
