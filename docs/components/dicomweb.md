# DICOMweb component

> The `dicomweb` component speaks DICOMweb (PS3.18): as a producer it stores (STOW-RS), searches (QIDO-RS) and retrieves (WADO-RS) studies on a DICOMweb server such as a cloud DICOM store; as a consumer it serves those resources from routes on the shared HTTP server.

**Scheme:** `dicomweb:` · **Crate:** `cameleer-dicom` · **Cargo feature:** [`dicom`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** no equivalent (Cameleer extension)

## When to use it

- Upload studies from a hospital gateway to Azure Health Data Services DICOM, the Google Cloud
  Healthcare API, AWS HealthImaging (STOW-RS import) or Orthanc.
- Bridge DIMSE and DICOMweb: a [`dicom`](dicom.md) consumer receives from modalities, a
  `dicomweb` producer stores in the cloud; or a `dicomweb` consumer gives a DIMSE PACS a
  DICOMweb front end.
- Search and pull studies for AI pipelines and research exports.

## URI format

```text
dicomweb:http(s)://host:port/base[?options]    (producer)
dicomweb:/base[?options]                       (consumer)
```

The producer's URL is the DICOMweb base (the part before `/studies`). The consumer serves
`/base/studies...` on the HTTP server that `platform-http` uses (`camel.server.port`, default
8080, or `port=`).

## Example

<!-- example: dicomweb-stow -->
```yaml
- route:
    id: dimse-to-dicomweb
    from:
      uri: "dicom:GATEWAY@0.0.0.0:{{env:DICOM_PORT:11112}}"
      steps:
        - to: "dicomweb:{{env:DICOMWEB_URL:http://localhost:8042/dicom-web}}?bearerToken={{env:DICOMWEB_TOKEN:}}"
        - log: "Stored ${header.CamelDicomCompleted} instance(s), HTTP ${header.CamelDicomHttpStatus}"
```

Search, then retrieve the metadata of each study found:

<!-- example: dicomweb-qido -->
```yaml
- route:
    id: find-studies
    from:
      uri: "timer:find?repeatCount=1"
      steps:
        - setBody:
            simple: '{"PatientID": "{{env:PATIENT:P1}}", "StudyDate": "20260101-", "StudyDescription": ""}'
        - unmarshal:
            json: {}
        - to: "dicomweb:{{env:DICOMWEB_URL:http://localhost:8042/dicom-web}}?operation=qido&limit=20"
        - split:
            simple: "${body}"
            steps:
              - setHeader:
                  name: CamelDicomStudyInstanceUid
                  simple: "${body[0020000D][Value][0]}"
              - to: "dicomweb:{{env:DICOMWEB_URL:http://localhost:8042/dicom-web}}?operation=metadata"
              - log: "Study ${header.CamelDicomStudyInstanceUid}: ${header.CamelDicomResultCount} instances"
```

A DICOMweb server backed by a route (here, forwarding to a DIMSE PACS):

<!-- example: dicomweb-server -->
```yaml
- route:
    id: dicomweb-front-end
    from:
      uri: "dicomweb:/dicom-web"
      steps:
        - choice:
            when:
              - simple: "${header.CamelDicomWebOperation} == 'stow'"
                steps:
                  - to: "dicom:PACS@{{env:PACS_HOST:pacs.local}}:104"
              - simple: "${header.CamelDicomWebOperation} == 'qido'"
                steps:
                  - to: "dicom:PACS@{{env:PACS_HOST:pacs.local}}:104?operation=find"
            otherwise:
              steps:
                - to: "dicom:PACS@{{env:PACS_HOST:pacs.local}}:104?operation=get"
```

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    cameleer_dicom::register(ctx);
    ctx.add_routes(|b| {
        b.from("dicom:GATEWAY@0.0.0.0:11112")
            .to("dicomweb:https://dicom.example.org/v2?bearerToken={{dicomweb.token}}");
    })
}
```

## Producer operations

| `operation` | request | body in | body out |
|---|---|---|---|
| `stow` (default) | `POST /studies` (or `/studies/{CamelDicomStudyInstanceUid}`), `multipart/related; type="application/dicom"` | Part 10 bytes, a Value tree, or an array of them | the response (a Value tree of the DICOM JSON answer) |
| `qido` | `GET /studies`, `/series` or `/instances` (`queryLevel`), within a study or series when the UID headers are set | the query keys: a map of keywords or tags, or a Value tree (empty values become `includefield`) | an array of Value trees |
| `wado` | `GET /studies/{s}[/series/{se}[/instances/{i}]]` | ignored (UIDs from headers or the body) | an array of Part 10 bytes |
| `metadata` | the same path plus `/metadata` | ignored | an array of Value trees |

HTTP errors raise `DicomWebException` with the status and the start of the response body. A
STOW-RS answer that lists failed instances (HTTP 202) raises it too, naming each failed instance
and its Failure Reason, unless `failOnPartialSuccess=false`.

## Consumer resources

| request | the exchange (`CamelDicomWebOperation`) | the route answers |
|---|---|---|
| `POST /studies[/{s}]` | `stow`, once per instance, body: Part 10 bytes | nothing; a failed exchange marks the instance failed (Failure Reason `A700`, or the route's `CamelDicomStatus`) |
| `GET /studies`, `/series`, `/instances`, `/studies/{s}/series`, `/studies/{s}/instances`, `/studies/{s}/series/{se}/instances` | `qido`, body: the identifier as a Value tree, `CamelDicomQueryLevel`, `CamelDicomQuery` (the raw parameters) | an array of Value trees (`limit` and `offset` are applied to it) |
| `GET /studies/{s}[/series/{se}[/instances/{i}]]` | `wado`, body: the identifier with the UIDs | an array of Part 10 bytes, sent as `multipart/related` |
| `.../metadata` | `metadata` | the same; answered as DICOM JSON with bulk data over `bulkDataThreshold` as `BulkDataURI` |
| `.../instances/{i}/bulkdata/{tag}` | `wado` | the same; the attribute's bytes are answered |

STOW-RS answers 200 when every instance was stored, 202 when some failed and 409 when all failed,
with `ReferencedSOPSequence` (and a Retrieve URL per instance) and `FailedSOPSequence`. An
instance posted to `/studies/{s}` that belongs to another study fails with reason `A900`. WADO-RS
honours `transfer-syntax=` in the `Accept` header by converting between native syntaxes.
`frames`, `rendered` and `thumbnail` answer 501: they need pixel codecs.

## Options

| option | default | description |
|---|---|---|
| `operation` | `stow` | Producer: `stow`, `qido`, `wado`, `metadata`. Header `CamelDicomWebOperation` overrides it. |
| `queryLevel` | `studies` | Producer, `qido`: `studies`, `series` or `instances`. |
| `limit`, `offset`, `fuzzymatching`, `includefield` | | Producer, `qido`: QIDO-RS parameters (`includefield` takes a comma list). |
| `transferSyntax` | `*` | Producer, `wado`: the `transfer-syntax` asked for in the `Accept` header (`*` = as stored). |
| `bearerToken` | | Producer: `Authorization: Bearer ...` (use a property placeholder or `{{vault:...}}`). |
| `username`, `password` | | Producer: Basic authentication. |
| `failOnPartialSuccess` | `true` | Producer, `stow`: fail when the server reports failed instances. |
| `timeout` | `60s` | Producer: request timeout. |
| `sslContextParameters` | | Producer: TLS for `https` (default: the system trust store). Consumer: serve HTTPS. |
| `port`, `host` | `camel.server.port` / `8080`, `0.0.0.0` | Consumer: the HTTP server. |
| `ssl` | `false` | Consumer: serve HTTPS with the `camel.server.ssl.*` settings. |
| `bulkDataThreshold` | `1024` | Consumer: bulk values larger than this go out as `BulkDataURI` in metadata and search results. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelDicomWebOperation` | out (consumer), in (producer) | `stow`, `qido`, `wado`, `metadata`. |
| `CamelDicomCommand` | out (consumer) | `STOW-RS`, `QIDO-RS`, `WADO-RS`. |
| `CamelDicomStudyInstanceUid`, `CamelDicomSeriesInstanceUid`, `CamelDicomSopInstanceUid` | in (producer), out (consumer) | UIDs of the request. |
| `CamelDicomSopClassUid` | out (consumer, `stow`) | Of the stored instance. |
| `CamelDicomQueryLevel`, `CamelDicomQuery` | out (consumer) / in (producer, `CamelDicomQuery` adds parameters) | Search level and parameters. |
| `CamelDicomWebAuthorization` | in (producer) | An `Authorization` value for this request. |
| `CamelDicomHttpStatus` | out (producer) | HTTP status of the response. |
| `CamelDicomCompleted`, `CamelDicomFailed` | out (producer, `stow`) | Stored and failed instances. |
| `CamelDicomResultCount` | out (producer) | Matches or retrieved instances. |
| `CamelDicomStatus` | in (consumer, `stow`) | A failure status to report for the instance. |

## Run it with Docker

```sh
docker run --rm -p 11112:11112 -e DICOMWEB_URL -e DICOMWEB_TOKEN -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/gateway.camel.yaml
```

## Common questions

### Which servers has this been tested with?

Orthanc 1.12.2 with its DICOMweb plugin 1.16, in both directions: the `dicomweb` producer
against Orthanc (STOW, QIDO, WADO, metadata), and Orthanc's DICOMweb client against a `dicomweb`
consumer (STOW, QIDO, WADO). Cloud services have not been tested with real accounts yet.

### How do I authenticate to Azure, Google Cloud or AWS?

Azure Health Data Services and the Google Cloud Healthcare API take OAuth 2.0 bearer tokens:
get one with the cloud's CLI or identity service and pass it as `bearerToken` (or per message in
`CamelDicomWebAuthorization`). AWS HealthImaging needs SigV4 request signing, which this
component does not do yet.

### Is JSON metadata with separate bulk data accepted by STOW-RS?

Not by the consumer: it takes `application/dicom` parts (415 otherwise). The producer always
sends Part 10 instances.

## Related

- [dicom](dicom.md), [dicomJson data format](../dataformats/dicomJson.md), [dicomDeidentify](../dataformats/dicomDeidentify.md), [platform-http](platform-http.md)
- [DICOM guide](../guides/dicom.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
