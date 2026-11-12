# File component

> The `file` component reads files from a directory (polling for new ones) and writes message bodies to files.

**Scheme:** `file:` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [File](https://camel.apache.org/components/next/file-component.html)

## When to use it

- Watch a drop folder on an edge gateway: machines, scanners or other programs write files, a
  route picks each one up, processes it and moves it aside.
- Write exports, reports or received messages to disk.
- Feed documents from a folder into an AI pipeline (summarize, classify, extract) one file at a
  time.

## URI format

```text
file:directoryName[?options]
```

The directory is relative to the working directory unless it is absolute. On Windows, use
forward slashes (`file:C:/data/inbox`).

## Example

A drop folder for CSV files. A file is picked up only after its size has stopped changing
(`readLock=changed`), copied to the outbox under a new name, and moved into a dated archive
folder; on failure it goes to `.failed`.

<!-- example: file-drop-folder -->
```yaml
- route:
    id: drop-folder
    from:
      uri: "file:{{env:INBOX:inbox}}?include=.*\\.csv&readLock=changed&move=.done/${date:now:yyyyMMdd}/${file:name}&moveFailed=.failed"
      steps:
        - log: "picked up ${file:name} (${file:size} bytes)"
        - to: "file:{{env:OUTBOX:outbox}}?fileName=${file:name.noext}-processed.csv&tempPrefix=.part-"
```

`tempPrefix` writes to a temporary name first and renames when complete, so a program watching
the outbox never sees a half-written file.

## Rust DSL

```rust
fn drop_folder_route(b: &mut RouteBuilder, inbox: &str, outbox: &str) {
    b.from(&format!("file:{}?include=.*\\.csv&readLock=changed&move=.done/${{date:now:yyyyMMdd}}/${{file:name}}&moveFailed=.failed", inbox))
        .route_id("drop-folder")
        .log("picked up ${file:name} (${file:size} bytes)")
        .to(&format!("file:{}?fileName=${{file:name.noext}}-processed.csv&tempPrefix=.part-", outbox));
}
```

## Options

Consumer:

| option | default | description |
|---|---|---|
| `delay` | `500` | Milliseconds between polls (with `initialDelay` 1000, `useFixedDelay`, `greedy`, `timeUnit`, `startScheduler`, `sendEmptyMessageWhenIdle`, `scheduler`). |
| `noop` | `false` | Leave files in place (implies `idempotent=true`). |
| `delete` | `false` | Delete files after processing. |
| `move` | `.camel` | Where to move processed files (file language expression). |
| `moveFailed` | | Where to move files whose processing failed. |
| `preMove` | | Where to move files before processing. |
| `include`, `exclude` | | Regular expressions on the file name. |
| `includeExt`, `excludeExt` | | Comma-separated extensions, e.g. `csv,txt`. |
| `antInclude`, `antExclude` | | Ant patterns on the relative path, e.g. `**/*.xml`. `antFilterCaseSensitive` (default `true`). |
| `fileName` | | Consume only this file (expression). |
| `filter` | | `#bean` with a custom file filter. |
| `recursive` | `false` | Also read subdirectories (`minDepth`, `maxDepth`). |
| `maxMessagesPerPoll` | `0` | Limit per poll (`0` = no limit). |
| `sortBy` | | `file:name`, `file:modified` or `file:size`, with `reverse:` / `ignoreCase:` prefixes; groups separated by `;`. |
| `shuffle` | `false` | Random order. |
| `readLock` | `none` | `markerFile`, `changed`, `fileLock`, `rename`, or `none`. |
| `readLockCheckInterval`, `readLockTimeout`, `readLockMinLength`, `readLockMinAge` | 1000 ms, 10000 ms, 1, 0 | Read lock tuning. |
| `readLockMarkerFile`, `readLockDeleteOrphanLockFiles` | `true` | Marker file handling. |
| `idempotent`, `idempotentKey`, `idempotentRepository` | | Skip files already processed. |
| `doneFileName` | | Only consume a file when this done file exists. |
| `charset` | | Read the body as text in this charset. |
| `includeHiddenFiles`, `includeHiddenDirs` | `false` | Also read hidden entries. |
| `startingDirectoryMustExist`, `directoryMustExist` | `false` | Fail when the directory is missing. |
| `autoCreate` | `true` | Create the directory. |

Producer:

| option | default | description |
|---|---|---|
| `fileName` | message id | File name expression; header `CamelFileName` also sets it. |
| `fileExist` | `Override` | `Override`, `Append`, `Fail`, `Ignore`, `Move` (with `moveExisting`), `TryRename`. |
| `tempFileName`, `tempPrefix` | | Write to a temporary name, then rename. |
| `doneFileName` | | Write a done file after the file. |
| `appendChars` | | Characters appended after each write (e.g. `\n`). |
| `charset` | | Encoding of string bodies. |
| `flatten` | `false` | Drop directories from the file name. |
| `keepLastModified` | `false` | Keep the `CamelFileLastModified` time. |
| `allowNullBody` | `false` | Write an empty file for a null body. |
| `eagerDeleteTargetFile` | `true` | Delete an existing target before renaming a temp file. |
| `jailStartingDirectory` | `true` | Refuse to write outside the directory. |
| `autoCreate` | `true` | Create missing directories. |

Expressions support `${file:name}`, `${file:name.noext}`, `${file:ext}`, `${file:onlyname}`,
`${file:parent}`, `${file:path}`, `${file:absolute.path}`, `${file:size}`, `${file:modified}`,
`${date:now:FORMAT}`, `${date:file:FORMAT}`, `${exchangeId}` and any other Simple expression.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelFileName` | out / in | Relative file name; producer: target name. |
| `CamelFileNameOnly` | out | Name without path. |
| `CamelFileAbsolutePath`, `CamelFileAbsolute` | out | Absolute path / whether the path is absolute. |
| `CamelFileRelativePath`, `CamelFilePath`, `CamelFileParent` | out | Path details. |
| `CamelFileLength` | out | Size in bytes. |
| `CamelFileLastModified` | out | Modification time. |
| `CamelFileNameConsumed` | out | Name of the consumed file. |
| `CamelFileNameProduced` | out | Producer: the absolute path written. |
| `CamelOverruleFileName` | in | Producer: overrides the name once. |

The body of a consumed file is its bytes. File errors raise `GenericFileOperationFailedException`.

## Run it with Docker

```sh
mkdir -p inbox outbox
docker run --rm --user "$(id -u):$(id -g)" -e INBOX=/data/inbox -e OUTBOX=/data/outbox \
  -v "$PWD:/data" -v "$PWD/drop-folder.camel.yaml:/routes/drop-folder.camel.yaml:ro" \
  cameleering/cameleer run /routes/drop-folder.camel.yaml
```

The data folder is mounted writable; the route file stays read-only.

## Common questions

### How do I avoid reading a file that is still being written?
Use `readLock=changed` (waits until the size and time stop changing), or have the writer use a
done file (`doneFileName`) or a temporary name that the `include` pattern does not match.

### How do I keep the original file where it is?
`noop=true`. Cameleer remembers the files it has read (in memory, or in an
`idempotentRepository`).

### How do I process files in order?
`sortBy=file:modified` (oldest first) or `sortBy=reverse:file:name`.

### Where do processed files go by default?
Into a `.camel` subfolder, as in Apache Camel. Set `move`, `delete=true` or `noop=true` to change
that.

## Related

- [ftp](ftp.md), [ftps](ftps.md), [sftp](sftp.md): the same options for remote servers.
- Apache Camel: [File component](https://camel.apache.org/components/next/file-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
