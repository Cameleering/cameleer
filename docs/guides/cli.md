# The cameleer command line: every command and flag

> `cameleer` is Cameleer's command line, a native counterpart of Apache Camel's Camel JBang: `cameleer run` runs route files, `init` writes a starter route, `validate` checks routes without running them, `test` runs route tests, `export` turns routes into a Rust project, and `ps`, `get`, `cmd`, `log`, `trace`, `stop` and `tool` manage integrations that are already running. This page lists every command and every flag.

**Applies to:** the `cameleer` binary (`cargo install cameleer-main`, the Docker image `pmsudhi/cameleer`) · **Apache Camel:** [Camel JBang](https://camel.apache.org/manual/camel-jbang.html)

The commands follow Camel JBang's names. Camel JBang's own command is `camel`; Cameleer's is
`cameleer`, so both can be installed side by side. In Docker the image's entry point is the
binary, so `docker run pmsudhi/cameleer <command> ...` runs the same commands.

## Commands at a glance

| command | what it does | Camel JBang |
|---|---|---|
| [`run`](#cameleer-run) | run route files until stopped or a limit is reached | `camel run` |
| [`init`](#cameleer-init) | write a starter route, Kamelet, Pipe, Integration or properties file | `camel init` |
| [`validate`](#cameleer-validate) | check route files without running them | (none) |
| [`schema`](#cameleer-schema) | print the JSON Schema of the YAML DSL | (none) |
| [`test`](#cameleer-test) | run YAML test files against routes | `camel test` (Citrus) |
| [`export`](#cameleer-export) | write a standalone Cargo project that embeds the routes | `camel export` |
| [`bind`](#cameleer-bind) | write a Pipe that binds a source to a sink | `camel bind` |
| [`catalog`](#cameleer-catalog) | list components, languages or Kamelets | `camel catalog` |
| [`ps`](#managing-running-integrations) | list running integrations | `camel ps` |
| [`stop`](#managing-running-integrations) | stop one or all running integrations | `camel stop` |
| [`get`](#managing-running-integrations) | show routes, context, health or endpoints | `camel get` |
| [`cmd`](#managing-running-integrations) | start, stop, suspend or resume routes, reset statistics, reload | `camel cmd` |
| [`log`](#managing-running-integrations) | print or follow an integration's log | `camel log` |
| [`trace`](#managing-running-integrations) | turn message tracing on or off and print traced messages | `camel trace` |
| [`tool`](#cameleer-tool) | AI tools of a running integration: kill switch and approvals | (none) |
| `version` | print the version (a reduced build also lists its component families) | `camel version` |
| `help` | print the usage summary | `camel --help` |

`cameleer --version`, `-v`, `--help` and `-h` work too. A command exits with 0 on success and 1
on failure, with the error on standard error; `validate` also uses 2 for a usage error.

Options take their value as the next argument or after `=`: `--port 9090` and `--port=9090` are
the same. An unknown option is an error that prints the usage.

## cameleer run

```text
cameleer run [files...] [--properties FILE] [--profile P] [--dev] [--name N] [--background]
             [--max-seconds N] [--max-messages N] [--max-idle-seconds N] [--prop key=value]
             [--log-level LEVEL] [--console] [--port N] [--trace] [--mcp | --mcp-stdio]
             [--mcp-expose PATTERNS]
```

Runs route files: Camel YAML (`.yaml`, `.yml`), Camel XML (`.xml`), Kamelets, Pipes and Camel K
`Integration` resources. Without files it runs every `.yaml`, `.yml` and `.xml` file of the
current directory, except `application*` files and `*.test.yaml` test files. It runs until it is
stopped (Ctrl+C, `SIGTERM`, `cameleer stop`) or a limit set with `--max-seconds`,
`--max-messages` or `--max-idle-seconds` is reached.

| flag | what it does | same as |
|---|---|---|
| `--properties FILE` | read this file instead of the `application.*` files of the current directory | |
| `--profile P` | activate a profile: reads `application-P.properties` and `%P.` keys | `camel.main.profile` |
| `--dev` | reload route files when they change, and activate the `dev` profile unless `--profile` is given | `camel.main.dev-reload=true` |
| `--name N` | the integration's name, shown by `ps` and used to select it; defaults to the first file's name without extension | `camel.main.name` |
| `--background` | start in the background, print the process id and return | |
| `--max-seconds N` | stop after N seconds | `camel.main.duration-max-seconds` |
| `--max-messages N` | stop after N messages | `camel.main.duration-max-messages` |
| `--max-idle-seconds N` | stop after N seconds without a message | `camel.main.duration-max-idle-seconds` |
| `--prop key=value` | set any configuration key; repeat it for several | |
| `--log-level LEVEL` | `TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR` or `OFF` | `camel.main.log-level` |
| `--console` | start the management server with the developer console, health checks and metrics | `camel.server.enabled`, `devConsoleEnabled`, `healthCheckEnabled`, `metricsEnabled` = `true` |
| `--port N` | the HTTP port for REST, `platform-http` and the management server (default 8080) | `camel.server.port` |
| `--trace` | trace every message through the routes | `camel.main.tracing=true` |
| `--mcp` | serve `ai-tool` routes as an MCP server over Streamable HTTP at `/mcp` | `camel.server.mcp-enabled=true` |
| `--mcp-stdio` | serve them over standard input and output (for Claude Desktop and other local clients); not with `--background` | `camel.server.mcp-transport=stdio` |
| `--mcp-expose PATTERNS` | serve existing routes as MCP tools without editing them: `rest:*`, `rest:/orders*`, `direct:lookup-*`, `route:billing-*`, comma separated; turns on the MCP server (HTTP, or stdio with `--mcp-stdio`) | `camel.server.mcp-expose-routes` |

`--mcp` and `--mcp-stdio` expose every tool (`camel.server.mcp-tags=*,untagged`) unless you set
`camel.server.mcp-tags` yourself. With `--mcp-expose`, REST operations become tools with typed
arguments, GET and HEAD are read-only and every other exposed tool needs approval
([Expose existing routes](mcp-server.md#expose-existing-routes-without-editing-them)). A `--prop` given on the command line wins over the flags above
and over every file. The full order and every key are in [Configuration](configuration.md).

```sh
cameleer run orders.camel.yaml --profile prod --port 9090
cameleer run --dev                                  # every route file here, reloaded on change
cameleer run job.camel.yaml --max-messages 1        # a run-once job
cameleer run tools.camel.yaml --mcp-stdio           # an MCP server for a desktop client
cameleer run orders.camel.yaml --mcp-expose rest:*  # its REST API as MCP tools, no route edits
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml --console
```

Each running integration writes status and log files to `$CAMEL_HOME` (default `~/.camel`); the
management commands below read them. Set `CAMEL_LOCAL_STATUS=false` to turn this off. The Docker
image does, and so do AWS Lambda, Cloud Run, Knative and Azure Functions, where one process runs
per instance and logs go to standard output. When the binary is named `bootstrap` and started
without arguments, it runs as an [AWS Lambda custom runtime](../DEPLOYMENT.md#aws-lambda).

## cameleer init

```text
cameleer init <file> [--directory DIR]
```

Writes a starter file chosen by the file name, and refuses to overwrite an existing one:

| file name | what it writes |
|---|---|
| `*.yaml`, `*.yml` | a YAML route with a timer and a log |
| `*.xml` | an XML route |
| `*.kamelet.yaml` | a source Kamelet with two parameters |
| `*.pipe.yaml` | a Pipe from `timer-source` to `log-sink` |
| `*.integration.yaml` | a Camel K `Integration` with a property |
| `*.properties` | an `application.properties` skeleton |

`--directory` writes into that folder, creating it if needed.

## cameleer validate

```text
cameleer validate <file-or-dir>... [--format text|json] [--strict] [--properties FILE] [--prop key=value]
```

Checks route files without starting anything: YAML and XML syntax, the DSL structure, endpoint
URIs and their options, expressions, unresolved placeholders, dangling `direct:` endpoints and
duplicate route ids. Exit code 0 means valid, 1 means errors (or warnings with `--strict`), 2 a
usage error. `--format json` prints a machine-readable report for coding agents and CI.
`--properties` (repeatable) and `--prop` supply the values placeholders resolve to; without
`--properties` the `application.*` files of the current directory are read. The codes and the
report format are in [Validate route files](validate.md).

## cameleer schema

```text
cameleer schema
```

Prints the JSON Schema of the YAML DSL to standard output, for editors and coding agents. See
[YAML DSL](yaml-dsl.md).

## cameleer test

```text
cameleer test <test-file>...
```

Runs YAML test files (`*.test.yaml`): each loads routes, sends messages, and checks bodies,
headers, mock endpoints and, for agent routes, recorded model responses replayed without a
model. Fails when a test fails. `cameleer run` and `cameleer validate` skip test files. See
[the agent step](agent-step.md) for an example.

## cameleer export

```text
cameleer export [files...] --directory DIR [--name N] [--camel-path PATH]
```

Writes a standalone Cargo project to `DIR`: the route and `application.*` files are copied to
`resources/` and compiled into the binary, and an `application.properties` next to the binary at
run time overrides the embedded one. Without files it exports every `.yaml`, `.yml`, `.xml` and
`.properties` file of the current directory; with files, the `application.*` files of the current
directory are added. `--name` names the package (default: the first route file's name).

The generated `Cargo.toml` depends on `cameleer-main` by path: `--camel-path` is the folder that
holds the `cameleer-main` crate, and it defaults to the source tree the CLI was built from. With
the Docker image or a `cargo install` build, pass `--camel-path` to the `crates` folder of a clone
of the repository, or replace the line with the published crate (`cameleer-main = "0.5"`).

## cameleer bind

```text
cameleer bind <name> --source S --sink S [--step S]... [--property target.key=value]...
              [--error-handler log|none|sink:URI] [--output FILE]
```

Writes a Camel Pipe resource that binds a source to a sink. `S` is a Kamelet name
(`timer-source`, or `kamelet:timer-source`) or an endpoint URI (`log:out`). `--step` adds
steps in order. `--property` sets a Kamelet property on `source.`, `sink.` or `step-N.` (from
0): `--property source.message=hi`. `--error-handler` adds a `log`, `none` or
`sink:<uri or kamelet>` error handler. The file is `<name>.yaml` unless `--output` says
otherwise; run it with `cameleer run`.

```sh
cameleer bind ticker --source timer-source --sink log-sink --property source.message=hi
cameleer run ticker.yaml
```

## cameleer catalog

```text
cameleer catalog [components|languages|kamelets]
```

Lists the component schemes (the default), expression languages or bundled Kamelets in this
binary, one per line. A build with fewer cargo features lists fewer
([Build features](build-features.md)). Each component's options are on its reference page,
`docs/components/<scheme>.md`.

## Managing running integrations

These commands act on integrations started with `cameleer run` on the same machine, through the
status files in `$CAMEL_HOME`. `[name|pid]` selects one by its name (wildcards such as `ord*`
work) or process id; it can be left out when only one integration is running.

| command | what it does |
|---|---|
| `cameleer ps` | a table of running integrations: PID, name, age, routes, total and failed messages |
| `cameleer stop [name\|pid]` | stop one gracefully; `--all` stops every one |
| `cameleer get routes [name]` | a table of routes with their state and statistics |
| `cameleer get context\|health\|endpoints [name]` | the context, health checks or endpoints as JSON; `get health` exits with 1 when the status is DOWN |
| `cameleer get ... --url URL` | ask a management server over HTTP instead (`/q/dev/*`, `/q/health`), for example in a container or on another host |
| `cameleer cmd ACTION [name] [--id ROUTE]` | `start-route`, `stop-route`, `suspend-route`, `resume-route`, `reset-stats` or `reload`; `--id` names a route, every route by default |
| `cameleer log [name] [--tail N] [--follow]` | print the log, the last N lines, and keep printing new lines until the integration stops |
| `cameleer trace [name]` | turn tracing on, or print the traced messages when it is on; `--disable` turns it off |

```sh
cameleer run orders.camel.yaml --background
cameleer ps
cameleer get routes orders
cameleer cmd suspend-route orders --id ingest
cameleer log orders --tail 20
cameleer stop orders
cameleer get health --url http://localhost:8080      # a container started with --console
```

## cameleer tool

```text
cameleer tool list|approvals [name|pid] [--json]
cameleer tool disable|enable <tool> [name|pid]
cameleer tool approve <handle> [--by NAME] [name|pid]
cameleer tool deny <handle> [--by NAME] [--reason TEXT] [name|pid]
```

Operates the [AI tools](../components/ai-tool.md) of a running integration. `list` shows each
tool and whether it is enabled. `disable` is a kill switch: the tool refuses calls at once,
without a restart, until `enable`. `approvals` lists calls waiting for a person, and `approve` or
`deny` decides one by its handle; `--by` records who decided (default: the user name) and
`--reason` why. `--json` prints machine-readable output. These commands need a build with the
`ai` feature (every default build has it). See [MCP server](mcp-server.md).

## Environment variables the CLI reads

| variable | what it does |
|---|---|
| `CAMEL_*` | configuration keys: `CAMEL_SERVER_PORT=9090` is `camel.server.port=9090` ([Configuration](configuration.md#environment-variables)) |
| `CAMEL_LOG_LEVEL` | the log level, `INFO` by default |
| `CAMEL_HOME` | where status and log files go, `~/.camel` by default |
| `CAMEL_LOCAL_STATUS` | `false` turns status and log files off |
| `SSL_CERT_FILE` | a PEM bundle of trusted CA certificates (needed on Windows, see the [FAQ](faq.md#which-ca-certificates-does-cameleer-trust)) |

## A worked example

This sequence runs in the test suite (`cli_guide_worked_example` in
`crates/cameleer-main/tests/docs_reference.rs`): it writes a starter route, validates it, and runs
it as a job that stops after two messages.

```sh
cameleer init hello.camel.yaml
cameleer validate hello.camel.yaml
cameleer run hello.camel.yaml --max-messages 2
```

`init` prints `Created ./hello.camel.yaml`, `validate` exits with 0, and `run` logs
`Hello Camel from route1` twice (the timer fires once a second) and exits with 0.

## Common questions

### How do I run every route file in a folder?

Run `cameleer run` in that folder without file names. It skips `application*` files and
`*.test.yaml` files.

### How do I run a route once and exit?

Give it a limit: `--max-messages 1` stops after one message, `--max-seconds N` after N seconds
and `--max-idle-seconds N` when nothing has happened for N seconds. A `timer` with
`repeatCount` plus `--max-idle-seconds` ends a batch job.

### How do I see what a running integration is doing?

`cameleer ps` lists it, `cameleer get routes` shows each route's message counts, `cameleer log
--follow` prints its log and `cameleer trace` prints each message as it moves through the routes.
In a container, start it with `--console` and use `cameleer get ... --url`.

### Where is `camel run`?

Cameleer's command is `cameleer`, not `camel`, so it does not clash with Apache Camel's Camel
JBang. The subcommands have the same names.

### How do I list a component's options?

`cameleer catalog components` lists the schemes; the options of each are on its page under
[components](../components/), with the same names as in Apache Camel.

## Related

- [Getting started](getting-started.md), [Configuration](configuration.md),
  [Validate route files](validate.md)
- [Deployment](../DEPLOYMENT.md): Docker, Kubernetes, Cloud Run, AWS Lambda
- [MCP server](mcp-server.md) for `--mcp` and `cameleer tool`

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
