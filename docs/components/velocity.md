# Velocity component

> The `velocity` component renders an Apache Velocity (VTL) template against the message and sets the rendered text as the new body.

**Scheme:** `velocity:` · **Crate:** `cameleer-core` · **Cargo feature:** [`templates`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [Velocity](https://camel.apache.org/components/next/velocity-component.html)

## When to use it

- Render emails, notifications and reports from a message with loops and conditions.
- Build a few-shot prompt for a model: loop over example pairs and append the new input.
- Reuse Velocity templates from an existing Java Camel application.

## URI format

```text
velocity:templateResource[?options]
```

`templateResource` is a file path relative to the working directory, `file:path`,
`classpath:path` (read as a relative path) or `ref:beanName` (a `String` in the registry).

## Example

A few-shot classification prompt: the examples come from the request, the count from
`$foreach.count`, and the instruction changes with a header.

<!-- file: templates/few-shot.vm -->
```text
#set($n = $body.examples.size())
Classify the sentiment. $n examples follow.
#foreach($e in $body.examples)
$foreach.count. "$e.text" => $e.label
#end
Text: "$body.input"
#if($headers.strict == "true")
Answer with exactly one word.
#end
```

<!-- example: velocity-few-shot-prompt -->
```yaml
- route:
    id: velocity-few-shot
    from:
      uri: direct:velocity-few-shot
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: strict
            constant: "true"
        - to: velocity:templates/few-shot.vm
```

<!-- input: velocity-few-shot-prompt -->
```json
{"examples":[{"text":"Great support, thanks!","label":"positive"},{"text":"Still broken after a week.","label":"negative"}],"input":"The update fixed it."}
```

<!-- output: velocity-few-shot-prompt -->
```text
Classify the sentiment. 2 examples follow.
1. "Great support, thanks!" => positive
2. "Still broken after a week." => negative
Text: "The update fixed it."
Answer with exactly one word.
```

### Supplemental context

The `CamelVelocitySupplementalContext` header (a map) adds entries to the data model; the
`CamelVelocityContext` header replaces it entirely.

<!-- file: templates/hello.vm -->
```text
Hello $name from $team, quiet missing value: [$!missing]
```

<!-- example: velocity-supplemental-context -->
```yaml
- route:
    id: velocity-supplemental
    from:
      uri: direct:velocity-supplemental
      steps:
        - setHeader:
            name: CamelVelocitySupplementalContext
            simple: "${map('name','Ada','team','platform')}"
        - to: velocity:templates/hello.vm
```

<!-- input: velocity-supplemental-context -->
```text
ignored
```

<!-- output: velocity-supplemental-context -->
```text
Hello Ada from platform, quiet missing value: []
```

## Rust DSL

```rust
b.from("direct:velocity-rust")
    .set_header("CamelVelocitySupplementalContext", simple("${map('name','Ada','team','ops')}"))
    .to("velocity:templates/hello.vm");
```

## Options

| option | default | description |
|---|---|---|
| `allowTemplateFromHeader` | `false` | Use the `CamelVelocityTemplate` / `CamelVelocityResourceUri` headers when present. |
| `contentCache` | `false` | Read the template once; otherwise it is re-read for every message. |
| `allowContextMapAll` | `false` | Add `camelContext` (with `name`) to the data model. |
| `encoding` | UTF-8 | Accepted; templates are always read as UTF-8. |
| `loaderCache`, `propertiesFile` | | Accepted and ignored (Java Velocity settings). |

The data model: `$body`, `$headers`, `$variables`, `$exchange` (`$exchange.properties`,
`$exchange.exchangeId`, `$exchange.getIn().getHeader("x")`), `$request` / `$in`, and
`$camelContext` with `allowContextMapAll=true`.

### Supported VTL

References `$x`, `${x}`, quiet `$!x`, properties, method calls (`$s.toUpperCase()`,
`$headers.get("x")`), index access `$list[0]` and `$map["k"]`; `#set`, `#if/#elseif/#else`,
`#foreach` with `$foreach.count/index/hasNext/first/last`, `#break`, `#stop`, `#macro` and
calls, `#include`, `#parse`, `#evaluate`, `#define`, comments `##` and `#* *#`, unparsed
blocks `#[[ ]]#`, escapes, arithmetic, comparisons and logical operators. Conditions follow
Velocity 2: null, `false`, empty strings, empty collections and zero are false.

Known difference: a `#end` line directly followed by another directive line (such as
`#if(...)`) can leave an empty line in the output where Velocity 2 removes both lines. Put a text line between the two directives, as the example does.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelVelocityTemplate` | in | Template text used instead of the resource (needs `allowTemplateFromHeader=true`). |
| `CamelVelocityResourceUri` | in | Template resource used instead of the endpoint's one (needs `allowTemplateFromHeader=true`). |
| `CamelVelocityContext` | in | A map used as the whole data model. |
| `CamelVelocitySupplementalContext` | in | A map merged into the data model. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/few-shot.camel.yaml
```

## Common questions

### How do I print nothing for a missing value?

Use a quiet reference: `$!name`. A plain `$name` that is null renders as the literal text
`$name`, as in Velocity.

### How do I get the loop index?

`$foreach.count` starts at 1, `$foreach.index` at 0; `$foreach.hasNext` helps with
separators.

### Can I pass extra values without changing the body?

Yes: set `CamelVelocitySupplementalContext` to a map (for example with Simple's
`${map(...)}`), and its keys become top-level template variables.

## Related

- [Mustache](mustache.md), [FreeMarker](freemarker.md), [StringTemplate](string-template.md),
  [MVEL](mvel.md).
- [Simple language](../languages/simple.md) for the `map(...)` function.
- [Apache Camel Velocity component](https://camel.apache.org/components/next/velocity-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
