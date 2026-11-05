# Getting started

> Install Cameleer, write a YAML route and run it with one command.

1. Write a route.
2. Run it with `cameleer run route.camel.yaml`.
   The file is read once at startup.

> **Note:** routes are plain Camel YAML.

<!-- example: guide-rest-hello -->
```yaml
- rest:
    path: /hello
    get:
      - to: direct:hello
- route:
    from:
      uri: direct:hello
      steps:
        - setBody:
            constant: "hi"
```

Code is not checked for links: `[x](nowhere.md)`, nor are fenced blocks:

```text
[y](nowhere-either.md)
```

See the [timer](../components/timer.md) and the [Simple language](../languages/simple.md#example).
