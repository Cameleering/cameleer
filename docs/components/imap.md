# IMAP component

> The `imap` component reads email from an IMAP mailbox: it polls a folder for new (unseen) messages and turns each one into an exchange.

**Scheme:** `imap:` · **Also:** `imaps:` · **Crate:** `cameleer-protocols` ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Mail](https://camel.apache.org/components/next/mail-component.html)

`imaps:` uses implicit TLS (port 993); `imap:` can upgrade with STARTTLS
(`mail.imap.starttls.enable=true`). Each mail header becomes a message header, the body is the
first `text/plain` part, and other parts are attachments.

## When to use it

- Triage a shared inbox: route invoices, orders and support requests to different systems.
- Feed incoming mail to an AI agent that classifies or answers it.
- Turn mail from devices or legacy systems that can only send email into events.

## URI format

```text
imap://[username@]host[:port][?options]
imaps://[username@]host[:port][?options]
```

Default ports: 143 (`imap`), 993 (`imaps`).

## Example

Poll a support mailbox every 30 seconds. Mails with "invoice" in the subject are saved for the
accounting system and moved to the `Processed` folder; everything else goes to a queue for an
AI triage agent.

<!-- example: imap-mail-triage -->
```yaml
- route:
    id: mail-triage
    from:
      uri: "imaps://{{env:IMAP_HOST:imap.example.com}}?username={{env:IMAP_USER:support@example.com}}&password={{env:IMAP_PASSWORD:changeme}}&moveTo=Processed&delay=30000"
      steps:
        - choice:
            when:
              - simple: "${header.Subject} contains 'invoice'"
                steps:
                  - to: "file:{{env:INVOICE_DIR:invoices}}?fileName=${exchangeId}.txt"
            otherwise:
              steps:
                - to: seda:triage
- route:
    id: triage-agent
    from:
      uri: seda:triage
      steps:
        - log: "to triage: ${header.Subject} from ${header.From}"
```

## Rust DSL

```rust
fn mail_triage_routes(b: &mut RouteBuilder, imap: &str, invoice_dir: &str) {
    b.from(imap)
        .route_id("mail-triage")
        .choice()
        .when(simple("${header.Subject} contains 'invoice'"))
        .to(&format!("file:{}?fileName=${{exchangeId}}.txt", invoice_dir))
        .otherwise()
        .to("seda:triage")
        .end_choice();
    b.from("seda:triage").route_id("triage-agent").log("to triage: ${header.Subject} from ${header.From}");
}
```

## Options

| option | default | description |
|---|---|---|
| `username`, `password` | | Login. |
| `folderName` | `INBOX` | Folder to read. |
| `unseen` | `true` | Only messages without the `\Seen` flag. |
| `delete` | `false` | Delete messages after processing. |
| `copyTo` | | Copy processed messages to this folder. |
| `moveTo` | | Move processed messages to this folder. |
| `maxMessagesPerPoll` / `fetchSize` | no limit | Messages per poll. |
| `mapMailMessage` | `true` | `true`: the body is the text content. `false`: the raw RFC 822 message. |
| `delay` | `60000` | Milliseconds between polls (`initialDelay` 1000 and the other scheduler options). |
| `connectionTimeout` | `30s` | Connect timeout. |
| `mail.imap.starttls.enable`, `mail.imap.starttls.required` | `false` | STARTTLS on `imap:`. |
| `sslContextParameters` | | `#bean` with trust material for TLS. |
| `useGlobalSslContextParameters` | `false` | Use the global `camel.ssl.*` parameters. |

`peek`, `closeFolder`, `disconnect`, `skipFailedMessage`, `searchTerm.*` and other `mail.*`
options are accepted and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `Subject`, `From`, `To`, `Cc`, `Date`, `Message-ID`, ... | out | Every mail header; repeated headers become a list. |

MIME parts other than the body are put in the `CamelAttachments` exchange property as bytes.

## Run it with Docker

```sh
docker run --rm --user "$(id -u):$(id -g)" -e IMAP_HOST=imap.example.com -e IMAP_USER -e IMAP_PASSWORD \
  -e INVOICE_DIR=/data/invoices -v "$PWD:/data" \
  -v "$PWD/mail-triage.camel.yaml:/routes/mail-triage.camel.yaml:ro" \
  pmsudhi/cameleer run /routes/mail-triage.camel.yaml
```

## Common questions

### How do I avoid processing the same mail twice?
Keep `unseen=true` (the default): read messages get the `\Seen` flag. `moveTo` or `delete=true`
also take them out of the folder.

### How do I read another folder?
`folderName=Support/Escalations`.

### How do I get the raw message with all parts?
`mapMailMessage=false` gives the raw RFC 822 text as the body.

### IMAP or POP3?
IMAP keeps mail on the server and supports folders, flags and moving; [pop3](pop3.md) only
downloads and optionally deletes.

## Related

- [pop3](pop3.md), [smtp](smtp.md).
- Apache Camel: [Mail component](https://camel.apache.org/components/next/mail-component.html).
