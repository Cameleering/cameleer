# FTP component

> The `ftp` component downloads files from and uploads files to an FTP server, polling a remote directory for new files.

**Scheme:** `ftp:` · **Crate:** `cameleer-protocols` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [FTP](https://camel.apache.org/components/next/ftp-component.html)

The options and headers follow the [file](file.md) component. For TLS use [ftps](ftps.md); for
SSH use [sftp](sftp.md).

## When to use it

- Collect files that remote sites, partners or machines drop on an FTP server.
- Upload exports and reports to a partner's FTP server.
- Move files from FTP into object storage, a database or a message broker.

## URI format

```text
ftp://[username@]host[:port]/directory[?options]
```

The directory is relative to the login directory; `ftp://host//abs/dir` is absolute. The
default port is 21.

## Example

Collect XML orders that a partner leaves in `outgoing/`, save them locally, and move each file
into `outgoing/.archive` on the server once it has been saved.

<!-- example: ftp-collect-orders -->
```yaml
- route:
    id: collect-orders
    from:
      uri: "ftp://{{env:FTP_USER:camel}}@{{env:FTP_HOST:localhost:21}}/outgoing?password={{env:FTP_PASSWORD:changeme}}&passiveMode=true&include=.*\\.xml&move=.archive&delay=10000"
      steps:
        - log: "received ${file:name} from ${header.CamelFileHost}"
        - to: "file:{{env:LOCAL_DIR:received}}"
```

## Rust DSL

```rust
fn collect_orders_route(b: &mut RouteBuilder, ftp: &str, password: &str, local_dir: &str) {
    b.from(&format!("ftp://{}/outgoing?password={}&passiveMode=true&include=.*\\.xml&move=.archive&delay=10000", ftp, password))
        .route_id("collect-orders")
        .log("received ${file:name} from ${header.CamelFileHost}")
        .to(&format!("file:{}", local_dir));
}
```

## Options

Connection:

| option | default | description |
|---|---|---|
| `username`, `password` | `anonymous` | Login (also `user@` in the URI). |
| `passiveMode` | `false` | Passive mode (needed behind NAT and most firewalls). `false` uses active mode (`PORT`/`EPRT`). |
| `binary` | `false` | `TYPE I`; `false` transfers in ASCII mode with line-ending conversion. |
| `stepwise` | `true` | `CWD` into each directory level. |
| `disconnect` | `false` | Close the connection after each poll or upload. |
| `connectTimeout` | `10s` | Connect timeout. |
| `soTimeout` | `5m` | Control connection read timeout. |
| `timeout` | `30s` | Data connection timeout. |
| `sendNoop` | `true` | Check pooled connections with `NOOP`. |
| `maximumReconnectAttempts`, `reconnectDelay` | 3, 1s | Reconnects. |
| `throwExceptionOnConnectFailed` | `false` | Fail the poll on connect errors. |
| `siteCommand` | | `SITE` commands after login (one per line). |
| `autoCreate` | `true` | Create the remote directory. |
| `charset` | | Text encoding of bodies. |

Consumer: `delay` (500 ms), `initialDelay` (1000 ms) and the scheduler options, `noop`,
`delete`, `move`, `moveFailed`, `preMove`, `include`, `exclude`, `includeExt`, `excludeExt`,
`antInclude`, `antExclude`, `antFilterCaseSensitive`, `fileName`, `recursive`, `minDepth`,
`maxDepth`, `maxMessagesPerPoll`, `sortBy`, `idempotent`, `idempotentKey`,
`idempotentRepository`, `download` (`false` = headers only), `localWorkDirectory` (stream
downloads to a local file instead of memory), `resumeDownload` (continue partial downloads, needs
`localWorkDirectory`), `streamDownload` (body is a stream handle), `ignoreFileNotFoundOrPermissionError`,
`readLock` (`none`, `changed`, `rename`, `idempotent`, `idempotent-changed`, `idempotent-rename`)
with `readLockCheckInterval` (5s), `readLockTimeout` (20s), `readLockMinLength`,
`readLockMinAge`.

Producer: `fileName`, `fileExist` (`Override`, `Append`, `Fail`, `Ignore`, `Move` with
`moveExisting`, `TryRename`), `tempFileName`, `tempPrefix`, `eagerDeleteTargetFile`,
`allowNullBody`, `flatten`, `doneFileName`.

As in Apache Camel, consumed files stay on the server unless `delete`, `move`, `noop` or
`idempotent` is set. Options such as `account`, `chmod`, `bufferSize`, `fastExistsCheck` and the
`ftpClient.*` group are accepted and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelFileName` | out / in | Relative file name; producer: target name. |
| `CamelFileNameOnly`, `CamelFileAbsolutePath`, `CamelFileRelativePath`, `CamelFilePath`, `CamelFileParent` | out | Path details. |
| `CamelFileLength`, `CamelFileLastModified` | out | Size and time. |
| `CamelFileHost` | out | Server host. |
| `CamelFileLocalWorkPath` | out | Local copy with `localWorkDirectory`. |
| `CamelFtpReplyCode`, `CamelFtpReplyString` | out | Last FTP reply. |
| `CamelFtpResumedFrom` | out | Offset a resumed download continued from. |
| `CamelFileNameProduced` | out | Producer: the remote path written. |
| `CamelOverruleFileName` | in | Producer: overrides the name once. |

## Run it with Docker

```sh
docker run --rm --user "$(id -u):$(id -g)" -e FTP_HOST=ftp.partner.example -e FTP_USER -e FTP_PASSWORD \
  -e LOCAL_DIR=/data/received -v "$PWD:/data" \
  -v "$PWD/collect-orders.camel.yaml:/routes/collect-orders.camel.yaml:ro" \
  pmsudhi/cameleer run /routes/collect-orders.camel.yaml
```

## Common questions

### How do I upload a file to FTP?
`to: ftp://user@host/incoming?password=...&passiveMode=true`; the name comes from the
`CamelFileName` header or `fileName`. Add `tempPrefix=.part-` so the partner never reads a
partial file.

### My downloads hang or time out. What should I check?
Most servers behind NAT or a firewall need `passiveMode=true`.

### How do I download large files without holding them in memory?
Set `localWorkDirectory=/tmp/ftp-work` (and `resumeDownload=true` to continue interrupted
transfers), or `streamDownload=true`.

### How do I avoid picking up a file that is still uploading?
`readLock=changed` waits until the size and time stop changing; `readLock=rename` checks that the
file can be renamed.

## Related

- [ftps](ftps.md), [sftp](sftp.md), [file](file.md).
- Apache Camel: [FTP component](https://camel.apache.org/components/next/ftp-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
