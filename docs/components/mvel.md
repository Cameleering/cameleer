# MVEL component

> The `mvel` component renders an MVEL 2 template (orb tags such as `@{expr}` and `@foreach{}`) against the message and sets the rendered text as the new body.

**Scheme:** `mvel:` · **Crate:** `cameleer-script` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [MVEL](https://camel.apache.org/components/next/mvel-component.html)

## When to use it

- Render text with real expressions inside the template: arithmetic, method calls, ternaries.
- Build an order summary or a prompt where values need computing, not just inserting.
- Reuse MVEL templates from a Java Camel application.

For MVEL as an expression language (in `setBody`, `filter`...), see the
[MVEL language](../languages/mvel.md).

## URI format

```text
mvel:templateResource[?options]
```

`templateResource` is a file path relative to the working directory, `file:path`,
`classpath:path` (read as a relative path) or `ref:beanName`.

## Example

An order summary with a computed total, a loop with a separator and a condition.

<!-- file: templates/order.mvel -->
```text
Order @{body.id} for @{body.customer.toUpperCase()}: @foreach{i : body.items}@{i.qty} x @{i.sku}@end{', '}
@code{total = 0; foreach (i : body.items) { total += i.qty * i.price; }}Total: @{total}
@if{total > 50}Free shipping.@else{}Shipping: 5.@end{}
```

<!-- example: mvel-order-summary -->
```yaml
- route:
    id: mvel-order
    from:
      uri: direct:mvel-order
      steps:
        - unmarshal:
            json: {}
        - to: mvel:templates/order.mvel
```

<!-- input: mvel-order-summary -->
```json
{"id":42,"customer":"ada","items":[{"sku":"A1","qty":2,"price":10},{"sku":"B2","qty":1,"price":40}]}
```

<!-- output: mvel-order-summary -->
```text
Order 42 for ADA: 2 x A1, 1 x B2
Total: 60
Free shipping.
```

## Rust DSL

```rust
b.from("direct:mvel-rust")
    .unmarshal(DataFormatDefinition::json())
    .to("mvel:templates/order.mvel");
```

## Options

| option | default | description |
|---|---|---|
| `allowTemplateFromHeader` | `false` | Use the `CamelMvelTemplate` / `CamelMvelResourceUri` headers when present. |
| `contentCache` | `false` | Read the template once; otherwise it is re-read for every message. |
| `allowContextMapAll` | `false` | Add `camelContext` (with `name`) to the data model. |
| `encoding` | UTF-8 | Accepted; templates are always read as UTF-8. |

The data model: `body`, `headers`, `variables`, `exchange`, `request` / `in`, and
`camelContext` with `allowContextMapAll=true`.

### Supported orb tags

`@{expr}`, `@if{cond}...@elseif{cond}...@else{}...@end{}`, `@foreach{x : list}...@end{}` (an
optional separator in `@end{', '}`, several iterables `@foreach{a : as, b : bs}`),
`@code{statements}` (runs without output), `@comment{...}`, `@include{'file'}`,
`@declare{'name'}...@end{}` with `@includeNamed{'name'}`, and `@@` for a literal `@`.
Expressions are full MVEL 2.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelMvelTemplate` | in | Template text used instead of the resource (needs `allowTemplateFromHeader=true`). |
| `CamelMvelResourceUri` | in | Template resource used instead of the endpoint's one (needs `allowTemplateFromHeader=true`). |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/order.camel.yaml
```

## Common questions

### How do I compute a value once and reuse it?

Assign it in `@code{...}` (no output), then print it with `@{name}`, as `total` in the example.

### How do I put a separator between loop items?

Give `@end{}` an expression: `@foreach{i : items}@{i}@end{', '}`.

### Is the `mvel` component the same as the MVEL language?

They share the MVEL interpreter. The component renders a template file; the language
evaluates one expression inside a route step.

## Related

- [MVEL language](../languages/mvel.md).
- [Mustache](mustache.md), [FreeMarker](freemarker.md), [Velocity](velocity.md),
  [StringTemplate](string-template.md).
- [Apache Camel MVEL component](https://camel.apache.org/components/next/mvel-component.html)
