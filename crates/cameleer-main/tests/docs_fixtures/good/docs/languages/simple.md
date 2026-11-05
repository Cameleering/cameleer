# Simple language

> Simple is Camel's built-in expression language for reading headers, the body and properties inside routes.

**Name:** `simple` · **Also:** `csimple`

## Example

<!-- example: simple-greeting -->
```yaml
- route:
    from:
      uri: direct:greet
      steps:
        - setBody:
            simple: "Hello ${body}"
        - to: mock:greeting
```

| expression | result |
|---|---|
| `${body}` | the message body |
| `${header.name}` | a header (a `|` in code is fine: `a\|b`) |

## Common questions

### How do I compare a header with a number?

Write `${header.amount} > 100` inside a `filter` or `choice`.

## Related

- [Timer component](../components/timer.md)
