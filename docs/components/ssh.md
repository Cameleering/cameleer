# SSH component

> The `ssh` component runs commands on remote hosts over SSH: the producer runs the message body as a command and returns its output, the consumer runs a command on a schedule.

**Scheme:** `ssh:` · **Crate:** `cameleer-ssh` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [SSH](https://camel.apache.org/components/next/ssh-component.html)

Cameleer has its own SSH-2 client (interop-tested against OpenSSH): password, public key,
keyboard-interactive, agent and certificate authentication, port forwarding from Rust, and
zlib compression.

## When to use it

- Check the health of edge devices or servers on a schedule (`uptime`, `df`, a status script).
- Restart a service or run a script on a remote host when an alert arrives.
- Give an AI operations agent a narrow, audited way to run predefined commands.

## URI format

```text
ssh://[username[:password]@]host[:port][?options]
```

The default port is 22.

## Example

Every minute, run `uptime` on an edge device and log the result. The host key must be in
`known_hosts` (`failOnUnknownHost=true`).

<!-- example: ssh-device-uptime -->
```yaml
- route:
    id: device-uptime
    from:
      uri: "ssh://{{env:SSH_USER:monitor}}@{{env:DEVICE_HOST:localhost:22}}?password={{env:SSH_PASSWORD:changeme}}&knownHostsFile={{env:KNOWN_HOSTS:known_hosts}}&failOnUnknownHost=true&pollCommand=uptime&delay=60000"
      steps:
        - filter:
            simple: "${header.CamelSshExitValue} == 0"
            steps:
              - log: "uptime: ${body}"
```

To run a command on demand instead, send it as the body to the same URI without `pollCommand`
(`to: ssh://...`); the reply body is the command's stdout.

## Rust DSL

```rust
fn device_uptime_route(b: &mut RouteBuilder, host: &str, password: &str, known_hosts: &str) {
    b.from(&format!("ssh://monitor@{}?password={}&knownHostsFile={}&failOnUnknownHost=true&pollCommand=uptime&delay=60000", host, password, known_hosts))
        .route_id("device-uptime")
        .filter(simple("${header.CamelSshExitValue} == 0"))
        .log("uptime: ${body}");
}
```

## Options

| option | default | description |
|---|---|---|
| `username`, `password` | | Credentials (also `user:password@` in the URI). |
| `pollCommand` | | Consumer: the command to run on each poll. |
| `delay`, `initialDelay` | 500, 1000 ms | Consumer schedule (plus the other scheduler options). |
| `channelType` | `exec` | `exec`, or `shell` (the command is written to an interactive shell). |
| `timeout` | `30000` | Read timeout in ms. |
| `certResource`, `privateKeyFile` | | Private key path (`file:` prefix allowed). |
| `certResourcePassword`, `privateKeyPassphrase` | | Passphrase of an encrypted key. |
| `knownHostsResource`, `knownHostsFile`, `knownHosts` | | `known_hosts` file or text. |
| `useUserKnownHostsFile` | `false` | Also read `~/.ssh/known_hosts`. |
| `failOnUnknownHost` | `false` | Reject hosts not in `known_hosts`. Alias `strictHostKeyChecking`. Changed keys are always rejected. |
| `useAgent`, `agentSocket` | `false` | Authenticate with an SSH agent. |
| `certificateFile` | | OpenSSH user certificate. |
| `preferredAuthentications` | | e.g. `publickey,password`. |
| `kex`, `ciphers`, `macs`, `signatures`, `compressionAlgorithms` | | Algorithm lists. |
| `connectTimeout` | `10s` | Connect timeout. |
| `charset` | UTF-8 | Decoding of stdout and stderr. |

`shellPrompt`, `sleepForShellPrompt`, `keyPairProvider`, `keyType`, `clientBuilder` and
`compressions` are accepted and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelSshStderr` | out | The command's stderr. |
| `CamelSshExitValue` | out | The exit status. |
| `CamelSshUsername`, `CamelSshPassword` | in | Producer: credentials for this exchange. |

The body is the command's stdout. The producer keeps the connection open between exchanges.

## Run it with Docker

```sh
ssh-keyscan device7.example.com > known_hosts
docker run --rm -e DEVICE_HOST=device7.example.com:22 -e SSH_USER -e SSH_PASSWORD -e KNOWN_HOSTS=/routes/known_hosts \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/device-uptime.camel.yaml
```

## Common questions

### How do I run a command when a message arrives?
Set the body to the command and send it to `ssh://user@host?...`. Read stdout from the body and
the exit status from `CamelSshExitValue`.

### How do I use a private key?
`privateKeyFile=/routes/id_ed25519` (or `certResource=file:/routes/id_ed25519`), with
`privateKeyPassphrase` for an encrypted key.

### Is it safe to let an AI agent run commands this way?
Only if the agent picks from fixed commands. Build the command in the route from an allow-list,
never from free text, and use an account with minimal rights.

## Related

- [sftp](sftp.md): file transfer over SSH.
- Apache Camel: [SSH component](https://camel.apache.org/components/next/ssh-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
