# Syslog data format

> The `syslog` data format parses RFC 5424 and RFC 3164 syslog lines into a structured value with the `CamelSyslog*` headers, and formats such values back into syslog lines.

**Name:** `syslog` · **Crate:** `cameleer-protocols` ·
**Apache Camel:** [Syslog](https://camel.apache.org/components/next/dataformats/syslog-dataformat.html)

## When to use it

- Collect logs from network gear, Linux hosts and appliances, and forward only what matters
  (for example severity `ERR` and worse) to Slack, a ticket system or an LLM that explains the
  error.
- Normalize syslog into JSON for Elasticsearch, OpenSearch or a data lake.
- Emit syslog lines from a route to an existing SIEM.

Pair it with the `syslog` component (`syslog:udp://0.0.0.0:514`, `tcp`, `tls`) or with
`netty`/file sources that deliver raw lines.

## Marshal and unmarshal

| direction | input body | output body |
|---|---|---|
| unmarshal | a syslog line (text or bytes), RFC 5424 or RFC 3164 | an object with the fields below; sets `CamelSyslogFacility`, `CamelSyslogSeverity`, `CamelSyslogHostname`, `CamelSyslogTimestamp` |
| marshal | such an object | a syslog line in the format named by its `format` field; other bodies are passed through as text |

Fields: `format` (`RFC5424` or `RFC3164`), `priority`, `facility` (name such as `AUTH`),
`facilityCode`, `severity` (name such as `CRIT`), `severityCode`, `version` (RFC 5424),
`timestamp` (as written in the message), `timestampMillis` (when it can be parsed),
`hostname`, `appName`, `procId`, `msgId`, `structuredData` (`{"sdId": {"param": "value"}}`)
and `logMessage`. Nil values (`-`) are `null`.

## Example

Parse an RFC 5424 line and keep only errors and worse (severity code 3 or lower).

<!-- example: syslog-filter-errors -->
```yaml
- route:
    id: syslog-errors
    from:
      uri: direct:syslog
      steps:
        - unmarshal:
            syslog: {}
        - choice:
            when:
              - simple: "${body.severityCode} <= 3"
                steps:
                  - setBody:
                      simple: "${header.CamelSyslogSeverity} on ${header.CamelSyslogHostname} (${body.appName}): ${body.logMessage} [region=${body.structuredData[origin][region]}]"
            otherwise:
              steps:
                - setBody:
                    constant: dropped
```

<!-- input: syslog-filter-errors -->
```text
<34>1 2026-11-05T10:15:00.000Z gw-01 sshd 4321 ID47 [origin region="eu-west"] Failed password for root
```

<!-- output: syslog-filter-errors -->
```text
CRIT on gw-01 (sshd): Failed password for root [region=eu-west]
```

<!-- input: syslog-filter-errors#2 -->
```text
<13>Nov  5 10:15:00 web-3 cron[99]: job finished
```

<!-- output: syslog-filter-errors#2 -->
```text
dropped
```

Format a value as an RFC 5424 line:

<!-- example: syslog-marshal -->
```yaml
- route:
    id: syslog-marshal
    from:
      uri: direct:syslog-marshal
      steps:
        - unmarshal:
            json: {}
        - marshal:
            syslog: {}
```

<!-- input: syslog-marshal -->
```json
{"format": "RFC5424", "facility": "LOCAL0", "severity": "WARNING", "timestamp": "2026-11-05T10:15:00Z",
 "hostname": "edge-7", "appName": "cameleer", "procId": null, "msgId": "TEMP", "logMessage": "sensor t1 above 80C"}
```

<!-- output: syslog-marshal -->
```text
<132>1 2026-11-05T10:15:00Z edge-7 cameleer - TEMP - sensor t1 above 80C
```

The priority is `facility * 8 + severity`: LOCAL0 (16) and WARNING (4) give 132. Null
fields (here `procId` and the structured data) are written as `-`.

## Rust DSL

```rust
b.from("direct:syslog-rust")
    .unmarshal(DataFormatDefinition::named("syslog"))
    .set_body(simple("${header.CamelSyslogSeverity}: ${body.logMessage}"));
```

## Message headers

| header | direction | description |
|---|---|---|
| `CamelSyslogFacility` | out (unmarshal) | Facility name, for example `AUTH`, `LOCAL0`. |
| `CamelSyslogSeverity` | out (unmarshal) | Severity name: `EMERG`, `ALERT`, `CRIT`, `ERR`, `WARNING`, `NOTICE`, `INFO`, `DEBUG`. |
| `CamelSyslogHostname` | out (unmarshal) | Host name from the message. |
| `CamelSyslogTimestamp` | out (unmarshal) | Timestamp from the message. |

The data format has no options. It is registered in the registry as `syslog` by
`cameleer-protocols` (included in `cameleer_main::new_main()` and the container image).

## Differences from Apache Camel

Camel unmarshals into `SyslogMessage` / `Rfc5424SyslogMessage` objects. Here the result is a
plain object with the same information (field names follow the Java getters), so Simple,
JSONPath and `marshal: json` work on it directly.

## Common questions

### How do I receive syslog over UDP and parse it?

`from: syslog:udp://0.0.0.0:514` already unmarshals by default (component option
`unmarshal=true`). Use the data format when lines come from elsewhere, such as a file or a
Kafka topic.

### How do I parse old BSD-style syslog lines?

Nothing to configure: lines without the version number after the priority are parsed as RFC
3164 (`format: RFC3164`), as the second input of the first example shows. RFC 3164
timestamps have no year; the original text is kept in `timestamp`.

## Related

- [Syslog component](../components/syslog.md)
- [JSON data format](json.md)
- [Apache Camel Syslog data format](https://camel.apache.org/components/next/dataformats/syslog-dataformat.html)
