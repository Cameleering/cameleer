# Exec component

> The `exec` component runs a local program or script from a route, writes the message body to its standard input and returns its standard output as the new body.

**Scheme:** `exec:` · **Crate:** `cameleer-core` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Exec](https://camel.apache.org/components/next/exec-component.html)

## When to use it

- Call an existing command-line tool or script from an integration (convert a file, run a diagnostic).
- On an edge gateway, read device state with a local command (`vcgencmd`, `nvidia-smi`, `df`) on a timer.
- Let an AI agent route run a fixed, allow-listed command as a tool. Keep the executable fixed in the
  route; never build it from model output.

## URI format

```text
exec:executable[?options]
```

## Example

<!-- example: exec-tool-version -->
```yaml
- route:
    id: record-tool-version
    from:
      uri: timer:version
      parameters:
        repeatCount: 1
        delay: 0
      steps:
        - to: exec:git?args=--version&timeout=5000
        - log: "exit ${header.CamelExecExitValue}: ${body}"
        - to: log:versions
```

At startup the route runs `git --version` once; the body becomes the output, such as `git version 2.47.0`.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:version?repeatCount=1&delay=0")
            .route_id("record-tool-version")
            .to("exec:git?args=--version&timeout=5000")
            .log("exit ${header.CamelExecExitValue}: ${body}")
            .to("log:versions");
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `args` | | Arguments, separated by spaces. Quote an argument that contains spaces with `"`. |
| `workingDir` | current directory | Directory to run the program in. |
| `timeout` | none | Kill the program and fail with `ExecException` after this many ms. |
| `outFile` | | Use the content of this file as the result instead of standard output. |
| `useStderrOnEmptyStdout` | `false` | Use standard error as the body when standard output is empty. |
| `exitValues` | any | Comma separated list of accepted exit codes; any other code fails with `ExecException`. |
| `binding`, `commandExecutor`, `commandLogLevel`, `lazyStartProducer` | | Accepted and ignored. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelExecCommandExecutable` | in | Overrides the executable. |
| `CamelExecCommandArgs` | in | Overrides `args` (a string or a list). |
| `CamelExecCommandWorkingDir` | in | Overrides `workingDir`. |
| `CamelExecCommandTimeout` | in | Overrides `timeout` (ms). |
| `CamelExecCommandOutFile` | in | Overrides `outFile`. |
| `CamelExecUseStderrOnEmptyStdout` | in | Overrides `useStderrOnEmptyStdout`. |
| `CamelExecExitValue` | out | The exit code. |
| `CamelExecStderr` | out | Standard error output. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/version.camel.yaml
```

The program must exist inside the container. The image is minimal, so build your own image `FROM
pmsudhi/cameleer` with the tools you need.

## Common questions

### Does exec use a shell?

No. The program is started directly with its arguments, so pipes and `*` do not work. To use them, run
`exec:sh?args=-c "your | pipeline"` explicitly.

### How do I fail the route when the command fails?

Set `exitValues=0`. Any other exit code then fails the exchange with `ExecException`.

### Is the message body passed to the program?

Yes. A non-null body is written to standard input.

## Related

- [stream](stream.md), [timer](timer.md)
- [Apache Camel Exec component](https://camel.apache.org/components/next/exec-component.html)
