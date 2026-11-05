# Broken page

> Every block on this page is wrong in a different way; the harness test expects each error.

```yaml
- route:
    from:
      uri: direct:unmarked
```

<!-- example: dup-id -->
```yaml
- route:
    from:
      uri: direct:a
```

<!-- example: dup-id -->
```yaml
- route:
    from:
      uri: direct:b
```

<!-- example: bad-yaml -->
```yaml
- route: [unclosed
```

<!-- example: not-a-route -->
```yaml
services:
  app:
    image: x
```

<!-- example: unknown-step -->
```yaml
- route:
    from:
      uri: direct:c
      steps:
        - noSuchStep:
            x: 1
```

<!-- example: unknown-option -->
```yaml
- route:
    from:
      uri: timer:tick?perod=1000
      steps:
        - to: log:x
```

<!-- example: no-component -->
```yaml
- route:
    from:
      uri: direct:d
      steps:
        - to: nosuchscheme:x
```

Links: [gone](missing.md), [also gone](../guides/nope.md#x), [fine](broken.md#broken-page).

```text
[ignored](code.md)
```
