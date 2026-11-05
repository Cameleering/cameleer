# SFTP component

> The `sftp` component downloads files from and uploads files to an SSH server over SFTP, polling a remote directory for new files.

**Scheme:** `sftp:` · **Crate:** `cameleer-ssh` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [SFTP](https://camel.apache.org/components/next/sftp-component.html)

Cameleer has its own SSH-2 and SFTP v3 implementation (interop-tested against OpenSSH), with
password, public key, agent and certificate authentication. The consumer and producer options
and headers are those of [ftp](ftp.md).

## When to use it

- Pick up files at remote or edge sites (stores, plants, field devices) that expose SFTP.
- Exchange files with partners and banks over SFTP.
- Push configuration or firmware files to devices.

## URI format

```text
sftp://[username@]host[:port]/directory[?options]
```

The directory is relative to the login (home) directory; `sftp://host//abs/dir` is absolute.
The default port is 22.

## Example

A central server collects JSON files that remote sites leave in `outbox/`, saves them locally
and deletes them on the site. The host key must match `known_hosts`
(`strictHostKeyChecking=yes`).

<!-- example: sftp-site-pickup -->
```yaml
- route:
    id: site-pickup
    from:
      uri: "sftp://{{env:SFTP_USER:edge}}@{{env:SFTP_HOST:localhost:22}}/outbox?password={{env:SFTP_PASSWORD:changeme}}&knownHostsFile={{env:KNOWN_HOSTS:known_hosts}}&strictHostKeyChecking=yes&include=.*\\.json&delete=true&delay=30000"
      steps:
        - log: "picked up ${file:name} from ${header.CamelFileHost}"
        - to: "file:{{env:LOCAL_DIR:incoming}}"
```

For key authentication, replace `password=...` with `privateKeyFile=/routes/id_ed25519` (and
`privateKeyPassphrase` for an encrypted key).

## Rust DSL

```rust
fn site_pickup_route(b: &mut RouteBuilder, sftp: &str, password: &str, known_hosts: &str, local_dir: &str) {
    b.from(&format!(
        "sftp://{}/outbox?password={}&knownHostsFile={}&strictHostKeyChecking=yes&include=.*\\.json&delete=true&delay=30000",
        sftp, password, known_hosts
    ))
    .route_id("site-pickup")
    .log("picked up ${file:name} from ${header.CamelFileHost}")
    .to(&format!("file:{}", local_dir));
}
```

## Options

Connection and authentication:

| option | default | description |
|---|---|---|
| `username`, `password` | `$USER` | Login (also `user[:password]@` in the URI). |
| `privateKeyFile`, `privateKeyUri`, `privateKey` | | Private key: path, `file:` URI, or key text / `#bean`. OpenSSH or PEM. |
| `privateKeyPassphrase` | | Passphrase of an encrypted key. |
| `useAgent`, `agentSocket` | `false` | Authenticate with an SSH agent (`SSH_AUTH_SOCK` or the given socket). |
| `certificateFile` | | OpenSSH user certificate for the key. |
| `knownHostsFile`, `knownHostsUri`, `knownHosts` | | `known_hosts` path, URI or text. |
| `useUserKnownHostsFile` | `true` | Also read `~/.ssh/known_hosts`. |
| `strictHostKeyChecking` | `no` | `yes`: the host key must be known. `no` (Apache Camel's default) accepts unknown hosts; changed or revoked keys are always rejected. |
| `preferredAuthentications` | | e.g. `publickey,keyboard-interactive,password`. |
| `ciphers`, `keyExchangeProtocols`, `serverHostKeys`, `macs`, `compressionAlgorithms` | | Algorithm lists. |
| `compression` | `0` | A level above 0 enables zlib compression. |
| `connectTimeout` | `10s` | Connect timeout. |
| `soTimeout` | `5m` | Read timeout. |
| `disconnect` | `false` | Close the connection after each poll or upload. |
| `sendNoop` | `true` | Check pooled connections before use. |
| `maximumReconnectAttempts`, `reconnectDelay` | 3, 1s | Reconnects. |
| `throwExceptionOnConnectFailed` | `false` | Fail the poll on connect errors. |
| `autoCreate` | `true` | Create the remote directory. |
| `bulkRequests` | `16` | Pipelined SFTP read / write requests. |
| `chmod`, `chmodDirectory` | | Octal permissions of uploaded files / created directories. |
| `charset` | | Text encoding of bodies. |

Consumer: the scheduler options (`delay` 500 ms, `initialDelay` 1000 ms, ...), `noop`, `delete`,
`move`, `moveFailed`, `preMove`, `include`, `exclude`, `includeExt`, `excludeExt`, `antInclude`,
`antExclude`, `antFilterCaseSensitive`, `fileName`, `recursive`, `minDepth`, `maxDepth`,
`maxMessagesPerPoll`, `sortBy`, `idempotent`, `idempotentKey`, `idempotentRepository`,
`download`, `ignoreFileNotFoundOrPermissionError`.

Producer: `fileName`, `fileExist`, `moveExisting`, `tempFileName`, `tempPrefix`,
`eagerDeleteTargetFile`, `allowNullBody`, `flatten`, `doneFileName`.

Not supported: `readLock` (accepted with a warning and ignored) and proxies.

## Message headers

The same as [ftp](ftp.md#message-headers): `CamelFileName`, `CamelFileNameOnly`,
`CamelFileAbsolutePath`, `CamelFileRelativePath`, `CamelFilePath`, `CamelFileParent`,
`CamelFileLength`, `CamelFileLastModified`, `CamelFileNameConsumed`, `CamelFileHost`, and
`CamelFileNameProduced` on upload.

## Run it with Docker

```sh
ssh-keyscan -p 22 site7.example.com > known_hosts
docker run --rm --user "$(id -u):$(id -g)" -e SFTP_HOST=site7.example.com:22 -e SFTP_USER -e SFTP_PASSWORD \
  -e KNOWN_HOSTS=/routes/known_hosts -e LOCAL_DIR=/data/incoming -v "$PWD:/data" \
  -v "$PWD/site-pickup.camel.yaml:/routes/site-pickup.camel.yaml:ro" -v "$PWD/known_hosts:/routes/known_hosts:ro" \
  pmsudhi/cameleer run /routes/site-pickup.camel.yaml
```

## Common questions

### How do I log in with a private key?
`privateKeyFile=/path/id_ed25519`; add `privateKeyPassphrase=...` if it is encrypted. Ed25519,
ECDSA and RSA keys in OpenSSH or PEM format work.

### Why should I set strictHostKeyChecking=yes?
With the default `no`, the first host key seen is accepted, so a man in the middle on the first
connection is not detected. Provide `knownHostsFile` and `strictHostKeyChecking=yes` in
production.

### Can I upload with SFTP?
Yes: `to: sftp://user@host/inbox?password=...&tempPrefix=.part-`.

### How do I wait for a file to be fully written on the remote side?
`readLock` is not supported for SFTP. Have the sender write under a temporary name and rename the
file when it is complete, and match only the final names with `include`.

## Related

- [ssh](ssh.md): run commands over SSH.
- [ftp](ftp.md), [ftps](ftps.md), [file](file.md).
- Apache Camel: [SFTP component](https://camel.apache.org/components/next/sftp-component.html).
