# Alibaba Cloud Simple Log Service component

> The `alibaba-sls` component writes logs to, queries logs in, and lists the Logstores of Alibaba Cloud Simple Log Service (SLS) over its REST API.

**Scheme:** `alibaba-sls:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [Alibaba SLS](https://camel.apache.org/components/next/alibaba-sls-component.html) (Preview, since Camel 4.23)

## When to use it

- Ship application or audit events from a route into an SLS Logstore.
- Search a Logstore from a route, for example behind an internal HTTP endpoint or an AI tool.
- List the Logstores of a project, for example to check a deployment.

## URI format

```text
alibaba-sls:operation[?options]
```

`operation` is `putLogs`, `getLogs` or `listLogStores`. The header `CamelAlibabaSlsOperation`
overrides it, then an exchange property of the same name.

## Example

Receive events over HTTP and write each one to a Logstore as one log:

<!-- example: alibaba-sls-ship-http-events -->
```yaml
- route:
    id: events-to-sls
    from:
      uri: "platform-http:/events?httpMethodRestrict=POST"
      steps:
        - unmarshal:
            json: {}
        - to: "alibaba-sls:putLogs?project={{env:SLS_PROJECT:my-project}}&logStoreName=app-events&region={{env:ALIBABA_CLOUD_REGION:cn-hangzhou}}"
        - setBody:
            constant: "stored"
```

The JSON object becomes one log: each field is a key of the log, and the time is now. The
AccessKey comes from `ALIBABA_CLOUD_ACCESS_KEY_ID` and `ALIBABA_CLOUD_ACCESS_KEY_SECRET`.

Search the errors of a time window, for example `GET /errors?from=1700000000&to=1700003600`:

<!-- example: alibaba-sls-search-errors -->
```yaml
- route:
    id: search-errors
    from:
      uri: "platform-http:/errors?httpMethodRestrict=GET"
      steps:
        - setHeader:
            name: CamelAlibabaSlsFrom
            simple: "${header.from}"
        - setHeader:
            name: CamelAlibabaSlsTo
            simple: "${header.to}"
        - setHeader:
            name: CamelAlibabaSlsQuery
            constant: "level: ERROR"
        - to: "alibaba-sls:getLogs?project={{env:SLS_PROJECT:my-project}}&logStoreName=app-events&region={{env:ALIBABA_CLOUD_REGION:cn-hangzhou}}&line=20&reverse=true"
        - setBody:
            simple: "${body[body][data]}"
        - marshal:
            json: {}
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:audit")
        .to("alibaba-sls:putLogs?project=my-project&logStoreName=audit&region=cn-hangzhou");
})?;
```

## Message body

**putLogs** writes one log group. The body can be:

| body | logs written |
|---|---|
| `{"topic", "source", "logTags", "logItems": [log, ...]}` (the SDK's `LogGroup`; `logs` is accepted too) | the logs, with the group's topic, source and tags |
| a list | one log per element |
| any other map | one log, the map's entries are its contents |
| text | the JSON document it holds, else one log with the text as `message` |

A log is `{"time": seconds, "timeNs": n, "contents": {...}}` (contents as a map or as a list of
`{"key", "value"}`), or a flat map whose entries are the contents (`time` or `__time__` taken as the
time). A log without a time gets the current time. Values that are not text are written as JSON.

After each call the body is a map with `statusCode` and the response `headers`; `getLogs` adds
`body`, the service's answer `{"meta": {...}, "data": [log, ...]}`, and `listLogStores` adds
`logstores` (the names), `count` and `total`.

## Options

| option | default | description |
|---|---|---|
| `project` | | The project. Header `CamelAlibabaSlsProject`. |
| `logStoreName` | | The Logstore of `putLogs` and `getLogs`. Header `CamelAlibabaSlsLogStoreName`. |
| `region` | | Region id, for example `cn-hangzhou`; the endpoint is then `{region}.log.aliyuncs.com`. |
| `endpoint` | | The SLS host, for example `cn-hangzhou-intranet.log.aliyuncs.com` (wins over `region`). `https://` is used unless the value starts with `http://`. |
| `accessKey` | `ALIBABA_CLOUD_ACCESS_KEY_ID` | AccessKey ID. |
| `secretKey` | `ALIBABA_CLOUD_ACCESS_KEY_SECRET` | AccessKey secret. |
| `securityToken` | `ALIBABA_CLOUD_SECURITY_TOKEN` | Cameleer: STS token of a temporary AccessKey, sent as `x-acs-security-token`. |
| `query` | | `getLogs`: search or analytic statement. |
| `from`, `to` | | `getLogs`: the time window in Unix seconds (`from` inclusive, `to` exclusive); both required. |
| `line` | | `getLogs`: maximum logs, at most 100 (the service's default is 100). |
| `offset` | | `getLogs`: offset of the first log. |
| `topic` | | `getLogs`: log topic. |
| `reverse` | | `getLogs`: newest first. |
| `signatureVersion` | `v1` | Cameleer: `v1` or `v4` (`SLS4-HMAC-SHA256`, needs the region; taken from the endpoint when it is `{region}.log.aliyuncs.com`). |
| `signatureMethod` | `hmac-sha256` | Cameleer: the HMAC of `v1`, `hmac-sha256` (as Camel's SDK) or `hmac-sha1` (as the classic SDKs). |
| `compressType` | `lz4` | Cameleer: `putLogs` body compression, `lz4`, `deflate` or `none`. |
| `sslContextParameters` | | TLS settings (trust store); see the [TLS guide](../guides/tls-and-security.md). The server certificate is always verified. |
| `proxyHost`, `proxyPort` | | HTTP proxy (else `HTTPS_PROXY`). |
| `uriEndpointOverride`, `overrideEndpoint` | | Connect to an emulator instead; the `Host` header still names `{project}.{endpoint}`. |

Camel's `slsClient` (an SDK client object) is accepted and ignored; `serviceKeys` (a Java object) is
refused: set `accessKey` and `secretKey`.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAlibabaSlsOperation` | in | Overrides the operation. |
| `CamelAlibabaSlsProject` | in | Overrides `project`. |
| `CamelAlibabaSlsLogStoreName` | in | Overrides `logStoreName`. |
| `CamelAlibabaSlsQuery`, `CamelAlibabaSlsFrom`, `CamelAlibabaSlsTo`, `CamelAlibabaSlsLine`, `CamelAlibabaSlsOffset`, `CamelAlibabaSlsTopic`, `CamelAlibabaSlsReverse` | in | Override the `getLogs` options. |
| `CamelAlibabaSlsListLogStoreName` | in | `listLogStores`: only Logstores whose name contains it. |
| `CamelAlibabaSlsMode` | in | `listLogStores`: `standard` or `query`. |
| `CamelAlibabaSlsSize` | in | `listLogStores`: page size, at most 500. |
| `CamelAlibabaSlsListOffset` | in | `listLogStores`: offset. |
| `CamelAlibabaSlsTelemetryType` | in | `listLogStores`: `None` (logs) or `Metrics`. |
| `CamelAlibabaSlsRequestId` | out | The request id of the service (`x-log-requestid`). |
| `CamelAlibabaSlsStatusCode` | out | The HTTP status. |

Every in header can also be an exchange property of the same name. A failed call raises a
`TeaException` (as Camel's SDK) whose message holds the service's error code, message, status and
request id.

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -e ALIBABA_CLOUD_ACCESS_KEY_ID -e ALIBABA_CLOUD_ACCESS_KEY_SECRET \
  -e SLS_PROJECT=my-project -v "$PWD:/routes:ro" cameleering/cameleer run /routes/events.camel.yaml
```

## Common questions

### How is the request signed?

With the SLS signature V1 by default, as Camel's SDK: `Authorization: LOG <AccessKeyId>:<signature>`
over the method, `Content-MD5`, `Content-Type`, `Date`, the `x-log-*` and `x-acs-*` headers and the
resource, with HMAC-SHA256 (`signatureMethod=hmac-sha1` for the classic HMAC-SHA1). Set
`signatureVersion=v4` for the `SLS4-HMAC-SHA256` scheme. Both are checked against the official SDKs'
test vectors.

### Why is the body lz4 and not zstd?

Camel's SDK compresses `putLogs` with zstd. Cameleer is written without third-party crates and has
no zstd compressor, so it uses lz4 (the classic SDKs' default), which the service accepts as well.

### Is there a consumer?

No, as in Camel. Poll with a `timer` route and `getLogs`, moving the time window forward.

## Related

- [Cloud credentials](../guides/cloud-credentials.md#alibaba-cloud)
- [Observability](../guides/observability.md)
- [Apache Camel Alibaba SLS](https://camel.apache.org/components/next/alibaba-sls-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
