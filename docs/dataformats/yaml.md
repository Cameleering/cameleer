# YAML data format

> The `yaml` data format parses YAML text into a structured body (unmarshal) and writes any body as YAML (marshal), like Camel's SnakeYAML data format.

**Name:** `yaml` · **Also:** `snakeYaml`, `snakeyaml`, `snake-yaml` · **Crate:** `cameleer-dataformats` ·
**Apache Camel:** [YAML SnakeYAML](https://camel.apache.org/components/next/dataformats/yaml-snakeyaml-dataformat.html)

## When to use it

- Read configuration, Kubernetes manifests or OpenAPI documents in a route.
- Ask an LLM to answer in YAML (often fewer tokens than JSON) and parse its answer.
- Turn JSON into readable YAML for people or GitOps repositories.

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `unmarshal` | YAML text (one document) | an object, list or scalar. Anchors and aliases are resolved. |
| `marshal` | any value | YAML text. In the default `AUTO` style, collections holding only scalars use flow style (`{a: 1}`, `[1, 2]`), the others block style. |

Unmarshal is safe by default: documents with application tags such as `!!com.acme.Pojo` are
rejected, and no type is ever constructed from a tag.

## Example

Parse a YAML answer from a model and route on a field:

<!-- example: yaml-parse-answer -->
```yaml
- route:
    id: yaml-answer
    from:
      uri: direct:yaml-answer
      steps:
        - unmarshal:
            yaml: {}
        - setBody:
            simple: "${body.action} on ${body.targets.size()} hosts, first ${body.targets[0]}"
```

<!-- input: yaml-parse-answer -->
```text
action: restart
reason: memory leak
targets:
  - web-1
  - web-2
```

<!-- output: yaml-parse-answer -->
```text
restart on 2 hosts, first web-1
```

Write JSON as YAML in block style:

<!-- example: yaml-json-to-yaml -->
```yaml
- route:
    id: json-to-yaml
    from:
      uri: direct:json-to-yaml
      steps:
        - unmarshal:
            json: {}
        - marshal:
            yaml:
              defaultFlowStyle: BLOCK
```

<!-- input: yaml-json-to-yaml -->
```json
{"service":"orders","replicas":3,"ports":[8080,9090]}
```

<!-- output: yaml-json-to-yaml -->
```text
service: orders
replicas: 3
ports:
- 8080
- 9090
```

## Rust DSL

```rust
b.from("direct:yaml-rust")
    .unmarshal(DataFormatDefinition::new("yaml"))
    .marshal(DataFormatDefinition::json());
```

## Options

| option | default | description |
|---|---|---|
| `defaultFlowStyle` (`flowStyle`) | `AUTO` | `AUTO`, `BLOCK` or `FLOW`. |
| `prettyFlow` | `false` | Write flow collections over several indented lines. |
| `allowAnyType` | `false` | Accept documents with application tags (the tag is ignored and the node read as plain data). |
| `typeFilter` | (none) | Comma separated tag names (or `prefix.*`) accepted without `allowAnyType`. |
| `maxAliasesForCollections` | `50` | Maximum number of aliases in one document (protects against alias bombs). |

`unmarshalType` is accepted and ignored: YAML always maps to a generic value.

## Common questions

### How do I convert YAML to JSON?

`unmarshal: {yaml: {}}` then `marshal: {json: {}}`.

### Is it safe to parse untrusted YAML?

Yes, by default. Tags that name types are rejected, nothing is instantiated, and aliases are
limited by `maxAliasesForCollections`.

## Related

- [JSON data format](json.md), [Simple language](../languages/simple.md)
- [Apache Camel YAML SnakeYAML data format](https://camel.apache.org/components/next/dataformats/yaml-snakeyaml-dataformat.html)
