# Validate routes with `cameleer validate`

> `cameleer validate` checks YAML and XML route files for syntax, structure, endpoint options,
> expressions, placeholders and route ids without starting any route or connecting to
> anything, and reports each problem with its line, column and a hint.

Use it before every run, in CI, in a pre-commit hook, and in the loop of a coding agent that
generates routes ([YAML for coding agents](yaml-for-coding-agents.md#the-validate-loop)). It
resolves endpoints with the same code the runtime uses, so an option it accepts is an option
Cameleer supports, exactly as Apache Camel would decide.

## Usage

```text
cameleer validate <file-or-dir>... [--format text|json] [--strict]
                  [--properties FILE] [--prop key=value]...
```


| argument | description |
|---|---|
| `<file-or-dir>...` | one or more route files, or folders: `*.yaml`, `*.yml` and `*.xml` files are found recursively, skipping `application*` files, hidden folders and `target` |
| `--format text` | human-readable output (the default) |
| `--format json` | one JSON document on standard output, for tools and agents |
| `--strict` | warnings make the exit code 1 |
| `--properties FILE` | a properties file used to resolve `{{...}}` placeholders (repeatable). Without it, `application.*` in the current folder is loaded, as `run` does |
| `--prop key=value` | one property; repeat for more |

Files are validated together: a `direct:` endpoint used in one file and consumed in another is
not a dangling endpoint, and two files with the same route id are a duplicate.

The command never starts routes, never opens ports and never connects to brokers, databases or
APIs, so it needs no credentials and runs in sandboxes and CI without network access.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer validate /routes
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer validate /routes --format json --strict
```

## What it checks

| code | severity | what it means |
|---|---|---|
| `yaml-syntax` | error | the file is not valid YAML (bad indentation, an unclosed quote, a tab) |
| `xml-syntax` | error | an `.xml` route file is not well-formed XML |
| `dsl-structure` | error | the YAML does not have the shape of the YAML DSL: an unknown top-level element, step or key, a missing required key, a value of the wrong type. The hint suggests the name you probably meant ("did you mean ...") |
| `endpoint` | error | an unknown scheme, or an option the component does not support, decided the same way Apache Camel does |
| `expression` | error | an expression (Simple, JSONPath, jq, XPath, ...) does not compile |
| `unresolved-placeholder` | warning | a `{{...}}` placeholder has no value from `--properties`/`--prop` and no default |
| `dangling-endpoint` | warning | `to: direct:x` (or a similar in-process endpoint) with no route consuming `from: direct:x` |
| `duplicate-route-id` | error | two routes share an `id` |

`unresolved-placeholder` is a warning because the value often comes from the environment where
the route runs. Pass the same properties file you run with, or use `--strict` to make warnings
fail too.

How placeholders, secrets and beans are treated, so results are the same on every machine:

- `{{env:NAME}}` is looked up in `--prop`/`--properties` (key `NAME`), never in the process
  environment. `{{env:NAME:default}}` uses the default.
- A secret with an empty default (`accessKey=RAW({{env:KEY:}})`) is checked again with a stand-in
  value when the empty value is rejected, so misspelled option names are still caught.
- An endpoint that only fails because a bean is missing from the registry (a `dataSource`, a
  `ref:` endpoint) is not reported: beans can come from code, `camel.beans.*` or
  `camel.datasource.*`.
- Endpoints that read a companion file when they are created (a `.proto`, `.thrift` or WSDL file
  next to the route) are reported if the file is not where the URI says. Validate the route from
  the folder it runs in.
- XML routes go through the same structure checks after the loader converts them; findings in
  XML files have no line or column (except `xml-syntax`).

## Output

### Text

One line per finding, in the usual compiler format, then a summary:

```text
path:line:column: severity[code]: message (hint)
```

For example, for a file with a misspelled step and an unknown option:

```text
$ cameleer validate orders.camel.yaml
orders.camel.yaml:8:11: error[dsl-structure]: unknown step 'setbody' (did you mean 'setBody'?)
orders.camel.yaml:12:15: error[endpoint]: timer:tick?perod=1000: unknown option 'perod' (did you mean 'period'?)
orders.camel.yaml:15:15: warning[dangling-endpoint]: no route consumes direct:billing
1 file, 2 errors, 1 warning
```

### JSON

```json
{
  "valid": false,
  "files": [
    {
      "path": "orders.camel.yaml",
      "valid": false,
      "routes": 2,
      "findings": [
        {
          "severity": "error",
          "code": "dsl-structure",
          "message": "unknown step 'setbody'",
          "line": 8,
          "column": 11,
          "hint": "did you mean 'setBody'?"
        }
      ]
    }
  ],
  "summary": {"files": 1, "errors": 1, "warnings": 0}
}
```

| field | description |
|---|---|
| `valid` | `true` when no file has an error (and, with `--strict`, no warning) |
| `files[].path`, `files[].valid`, `files[].routes` | each file, whether it is valid, how many routes it defines |
| `findings[].severity` | `error` or `warning` |
| `findings[].code` | one of the codes above |
| `findings[].message`, `findings[].hint` | what is wrong, and a suggestion (the hint may be empty) |
| `findings[].line`, `findings[].column` | 1-based position in the file |
| `summary` | counts of files, errors and warnings |

### Exit codes

| code | meaning |
|---|---|
| `0` | valid |
| `1` | at least one error, or at least one warning with `--strict` |
| `2` | bad usage: an unknown option, no file given, a file that does not exist |

## A route that passes

This route validates cleanly: every option exists, the expression compiles, the placeholder has a
default and `direct:billing` is consumed in the same file.

<!-- example: validate-clean-route -->
```yaml
- route:
    id: orders
    from:
      uri: timer:tick
      parameters:
        period: "{{orders.period:1000}}"
        repeatCount: 1
      steps:
        - setBody:
            simple: "order of ${date:now:yyyy-MM-dd}"
        - to: direct:billing
- route:
    id: billing
    from:
      uri: direct:billing
      steps:
        - log: "billing ${body}"
        - to: mock:billed
```

## Use it in CI

A GitHub Actions job that fails the build when a route file is invalid:

<!-- not-camel -->
```yaml
name: routes
on: [push, pull_request]
jobs:
  validate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Validate route files
        run: docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer validate /routes --strict
```

As a pre-commit hook (`.git/hooks/pre-commit`):

```sh
#!/bin/sh
cameleer validate $(git diff --cached --name-only -- '*.camel.yaml') --strict
```

## Editor support

`cameleer schema` prints a JSON Schema (draft 2020-12) of the YAML DSL, generated from the
same tables the validator uses: every top-level element, all steps with their keys and types,
required keys, nested elements and every expression form. Use it for completion and inline
errors while you type:

```sh
cameleer schema > cameleer.schema.json
```

- VS Code with the YAML extension: add to `.vscode/settings.json`:
  `"yaml.schemas": {"./cameleer.schema.json": "*.camel.yaml"}`, or put
  `# yaml-language-server: $schema=./cameleer.schema.json` on the first line of a route file.
- JetBrains IDEs: Settings, Languages & Frameworks, Schemas and DTDs, JSON Schema Mappings, add
  the file for `*.camel.yaml`.

The schema does not know component options (they depend on the URI) or data-format and language
options, and covers Kamelet, Pipe and Integration resources only partly. Run `cameleer validate`
for those checks.

## Common questions

### Does validate start my routes or connect to my broker?

No. It loads the files with the real YAML loader, creates endpoint objects to check their
options, compiles expressions, and stops there. Nothing is started and nothing connects.

### Why is a missing property only a warning?

Because properties often come from the environment of the machine that runs the route. Pass
`--properties` with the file you run with, or use `--strict` in CI.

### Can I validate a whole folder?

Yes: `cameleer validate routes/` checks its route files together, which also makes
cross-file `direct:` references and duplicate ids visible.

### Is validate the same as running with `--max-seconds 1`?

No. Running starts routes and connects to real systems; validate does neither, so it is safe on
any machine and fast enough for every save.

## Related

- [YAML for coding agents](yaml-for-coding-agents.md), [YAML DSL reference](yaml-dsl.md)
- [Getting started](getting-started.md), [deployment guide](../DEPLOYMENT.md)
- [Components](../components/): the options each endpoint accepts

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
