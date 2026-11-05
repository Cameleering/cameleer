# Stream component

> The `stream` component reads lines from standard input or a file (optionally tailing it) and writes messages to standard output, standard error or a file.

**Scheme:** `stream:` · **Crate:** `cameleer-core` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Stream](https://camel.apache.org/components/next/stream-component.html)

## When to use it

- Tail an application log file on an edge device and forward error lines (like `tail -f | grep ERROR`).
- Build command-line pipelines: read lines from `stdin`, transform them, print to `stdout`.
- Print results to the console in a demo or a one-off job.

## URI format

```text
stream:in | stream:out | stream:err | stream:file?fileName=path[&options]
```

## Example

<!-- example: stream-tail-errors -->
```yaml
- route:
    id: tail-errors
    from:
      uri: stream:file
      parameters:
        fileName: "{{env:LOG_FILE:/var/log/app.log}}"
        scanStream: true
        scanStreamDelay: 500
      steps:
        - filter:
            simple: "${body} contains 'ERROR'"
            steps:
              - to: stream:out
```

Every line of the file, and each line appended later, is checked; lines containing `ERROR` are printed.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("stream:file?fileName=/var/log/app.log&scanStream=true&scanStreamDelay=500")
            .route_id("tail-errors")
            .filter(simple("${body} contains 'ERROR'"))
            .to("stream:out")
            .end();
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `fileName` | | Required for `stream:file`. Producers append to it; consumers read it. |
| `encoding` | `UTF-8` | `UTF-8` or `ISO-8859-1`. |
| `delay` | `0` | Producer: wait (ms) before each write. Consumer: wait between messages. |
| `appendNewLine` | `true` | Producer: add a line break after each message. |
| `scanStream` | `false` | Consumer: keep reading the file as it grows. |
| `scanStreamDelay` | `100` | Consumer: ms between checks for new lines with `scanStream`. |
| `groupLines` | `0` | Consumer: send N lines per message, joined with `\n`. |
| `initialPromptDelay` | `2000` | `stream:in`: wait before the first prompt. |
| `promptMessage` | | `stream:in`: prompt printed before reading each line. |
| `promptDelay` | `0` | `stream:in`: wait before each prompt. |
| `closeOnDone`, `autoCloseCount`, `retry`, `fileWatcher`, `readTimeout`, `readLine`, `groupStrategy`, `lazyStartProducer`, `bridgeErrorHandler` | | Accepted and ignored. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelStreamIndex` | out | Line number of the message, starting at 0. |
| `CamelStreamComplete` | out | `true` on the last message when the input ended. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" -v /var/log/app:/logs:ro -e LOG_FILE=/logs/app.log \
  pmsudhi/cameleer run /routes/tail.camel.yaml
```

## Common questions

### How do I read standard input line by line?

`from: stream:in`, then run with `docker run -i ...` or pipe into the `cameleer` CLI.

### Does `scanStream` start at the end of the file?

No. It reads the existing lines first, then follows new ones.

## Related

- [exec](exec.md), [log](log.md), [syslog](syslog.md)
- [Apache Camel Stream component](https://camel.apache.org/components/next/stream-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
