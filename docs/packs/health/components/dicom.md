# DICOM component

> The `dicom` component speaks DIMSE, the DICOM network protocol: a consumer is an SCP that receives images and queries from modalities and PACS, and a producer is an SCU that stores, queries and retrieves them, over TCP or TLS.

**Scheme:** `dicom:` · **Crate:** `cameleer-health` · **Pack:** [health](../index.md) (feature `health`) · **Cargo feature:** [`health`](../../../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** no equivalent (Cameleer extension)

## When to use it

- Run an edge imaging gateway: receive studies from CT, MR or ultrasound modalities (C-STORE),
  filter and fix tags, and forward them to a PACS (C-STORE) or a cloud DICOM store
  ([`dicomweb`](dicomweb.md)).
- Route studies to AI models and send the results back, de-identifying them on the way out
  ([`dicomDeidentify`](../dataformats/dicomDeidentify.md)).
- Answer C-FIND, C-MOVE and C-GET from any source a route can reach (a folder, a database, a
  DICOMweb archive), for example as a DIMSE front end to a cloud store.
- Query and retrieve from a PACS on a schedule.

## URI format

```text
dicom:AET@host:port[?options]
```

`AET` is the Application Entity title (1 to 16 characters). A consumer listens on `host:port`
as `AET` (`0.0.0.0` for every interface); a producer calls `AET` at `host:port`. The port
defaults to 104.

## Example

<!-- example: dicom-gateway -->
```yaml
- route:
    id: modality-to-pacs
    from:
      uri: "dicom:GATEWAY@0.0.0.0:{{env:DICOM_PORT:11112}}?acceptedCallingAets=CT01,MR01"
      steps:
        - log: "C-STORE from ${header.CamelDicomCallingAet}: ${dicom.Modality} ${dicom.SOPInstanceUID}"
        - filter:
            simple: "${dicom.Modality} in 'CT,MR'"
            steps:
              - to: "dicom:PACS@{{env:PACS_HOST:pacs.local}}:104?callingAet=GATEWAY"
```

The gateway accepts associations from `CT01` and `MR01`, logs each instance, and forwards CT and
MR to the PACS. The modality gets a success status once the PACS has stored the instance, and a
failure status (with the reason as the Error Comment) when the route fails, so it can retry.

A query and retrieve, answered by the SCU producer:

<!-- example: dicom-find-move -->
```yaml
- route:
    id: nightly-prefetch
    from:
      uri: "cron:prefetch?schedule=0 0 2 * * ?"
      steps:
        - setBody:
            simple: '{"PatientID": "{{env:PATIENT:P1}}", "StudyInstanceUID": "", "StudyDate": ""}'
        - unmarshal:
            json: {}
        - to: "dicom:PACS@{{env:PACS_HOST:pacs.local}}:104?operation=find&queryLevel=STUDY"
        - split:
            simple: "${body}"
            steps:
              - setHeader:
                  name: CamelDicomMoveDestination
                  constant: WORKSTATION
              - to: "dicom:PACS@{{env:PACS_HOST:pacs.local}}:104?operation=move"
```

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_pack(&cameleer_health::HEALTH);
    ctx.add_routes(|b| {
        b.from("dicom:GATEWAY@0.0.0.0:11112")
            .route_id("modality-to-pacs")
            .filter(simple("${dicom.Modality} == 'CT'"))
            .to("dicom:PACS@pacs.local:104?callingAet=GATEWAY");
    })
}
```

## What the consumer does

| request | the exchange | what the route answers |
|---|---|---|
| C-ECHO | none (answered directly), unless `echoToRoute=true` | — |
| C-STORE | body: the instance as Part 10 bytes | nothing: success, or a failure status when the exchange fails |
| C-FIND | body: the identifier as a [Value tree](../dataformats/dicom.md) | the matches: an array of Value trees or Part 10 bytes |
| C-MOVE, C-GET | body: the identifier as a Value tree | the instances: an array of Part 10 bytes or Value trees |

For C-MOVE the SCP opens a new association to the destination named in the request, using
`moveDestinations` to find its address, and reports pending, completed, failed and warning
counts. For C-GET it sends the instances back on the same association (the SCU proposes the SCP
role for the storage classes). A route can choose the status with `CamelDicomStatus` (for example
a warning `B000`) and its Error Comment with `CamelDicomErrorComment`.

## What the producer does

| `operation` | body in | body out |
|---|---|---|
| `store` (default) | Part 10 bytes, a Value tree, or an array of them (sent on one association) | unchanged |
| `echo` | ignored | unchanged |
| `find` | the identifier: a Value tree or a map of keywords (`{"PatientID": "P1", "StudyInstanceUID": ""}`; empty values ask for the attribute) | an array of Value trees |
| `move` | the identifier | unchanged; counts in headers |
| `get` | the identifier | an array of Part 10 bytes |

A failure status raises `DicomStatusException` (the message has the status in hex and the Error
Comment); warnings only set `CamelDicomStatus`. A refused association raises
`DicomAssociationException` with the reason (for example "called AE title not recognized").

The SCU proposes the instance's own transfer syntax plus Explicit and Implicit VR Little Endian.
When the peer accepts only a native syntax for a compressed instance, the producer fails with
`DicomTranscodeException` naming both syntaxes: pixel data codecs are not part of this build.
Native syntaxes (Implicit, Explicit, Deflated, Big Endian) are converted into each other as
needed.

## Options

| option | default | description |
|---|---|---|
| `callingAet` | `CAMELEER` | Producer: this side's AE title. |
| `operation` | `store` | Producer: `echo`, `store`, `find`, `move`, `get`. Header `CamelDicomOperation` overrides it. |
| `queryModel` | `StudyRoot` | `StudyRoot`, `PatientRoot` or `MWL` (Modality Worklist, find only). |
| `queryLevel` | `STUDY` | Query/Retrieve Level added to identifiers that have none: `PATIENT`, `STUDY`, `SERIES`, `IMAGE`. |
| `moveDestination` | | Producer: the C-MOVE destination AE title (or header `CamelDicomMoveDestination`). |
| `priority` | `MEDIUM` | `LOW`, `MEDIUM`, `HIGH`. |
| `transferSyntaxes` | | Producer: extra transfer syntaxes to propose (comma list of UIDs or keywords). Consumer: the accepted syntaxes in order of preference (default: Explicit, Implicit, Deflated, Big Endian, then any proposed one). |
| `storageSopClasses` | `common` | Producer, `get`: the storage SOP classes offered for the returned instances (comma list of UIDs or keywords). |
| `sopClasses` | `verification,storage,query` | Consumer: accepted abstract syntaxes: `all`, or a comma list of `verification`, `storage`, `query`, UIDs and keywords. |
| `moveDestinations` | | Consumer: `AET=host:port` pairs (comma separated) for C-MOVE destinations. |
| `acceptedCallingAets` | | Consumer: accept only these calling AE titles. |
| `checkCalledAet` | `false` | Consumer: reject associations whose Called AE Title is not this endpoint's. |
| `requireUserIdentity` | `false` | Consumer: reject associations without User Identity Negotiation. |
| `username`, `password` | | Producer: User Identity Negotiation (type 1, or type 2 with a password). |
| `maxAssociations` | `50` | Consumer: concurrent associations; more are rejected (transient). |
| `echoToRoute` | `false` | Consumer: route C-ECHO requests too. |
| `maxPduLength` | `16384` | Largest PDU this side receives. |
| `connectTimeout` | `30s` | Producer: TCP connect timeout. |
| `timeout` | `60s` | Time to wait for an association answer or a response. |
| `tls` | `false` | Use TLS with the global SSL context parameters. |
| `sslContextParameters` | | TLS for the consumer (needs key material) and the producer (verifies the peer). |
| `useGlobalSslContextParameters` | `false` | Use `camel.ssl.*`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelDicomCommand` | out (consumer) | `C-ECHO`, `C-STORE`, `C-FIND`, `C-MOVE`, `C-GET`. |
| `CamelDicomCallingAet`, `CamelDicomCalledAet` | out (consumer), in (producer, overrides) | AE titles. |
| `CamelDicomRemoteAddress` | out (consumer) | Peer address. |
| `CamelDicomSopClassUid`, `CamelDicomSopInstanceUid`, `CamelDicomTransferSyntaxUid` | out | The object (or, for queries, the information model). |
| `CamelDicomMessageId`, `CamelDicomPriority` | out (consumer) | From the request. |
| `CamelDicomMoveDestination` | out (consumer), in (producer) | C-MOVE destination. |
| `CamelDicomMoveOriginatorAet` | out (consumer) | For a C-STORE caused by a C-MOVE. |
| `CamelDicomQueryLevel` | out (consumer), in (producer) | Query/Retrieve Level. |
| `CamelDicomUsername` | out (consumer) | Username of a User Identity Negotiation. |
| `CamelDicomOperation` | in (producer) | Overrides `operation`. |
| `CamelDicomStatus`, `CamelDicomErrorComment` | out (producer), in (consumer, to answer with) | DIMSE status and Error Comment. |
| `CamelDicomCompleted`, `CamelDicomFailed`, `CamelDicomWarning` | out (producer) | Sub-operation counts (store batches, move, get). |
| `CamelDicomResultCount` | out (producer) | Number of matches or retrieved instances. |

## Run it with Docker

```sh
docker run --rm -p 11112:11112 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/gateway.camel.yaml
storescu -aec GATEWAY localhost 11112 image.dcm
```

## Common questions

### Which DICOM tools has this been tested with?

DCMTK 3.6.7 (`echoscu`, `storescu`, `findscu`, `getscu`, `movescu` against a Cameleer SCP, and
`storescp` as a peer, both with and without TLS) and Orthanc 1.12.2 (as a PACS for C-STORE, C-FIND,
C-GET and C-MOVE back to a Cameleer SCP). The live tests are `crates/cameleer-health/tests/live_dicom.rs`,
run with `scripts/dicom-interop.sh`; see [Interop](../../../INTEROP.md).

### Does it decompress JPEG or JPEG 2000 images?

No. Routing does not need pixel codecs: instances are forwarded in the transfer syntax they
arrived in, and native syntaxes are converted into each other. A receiver that accepts only
uncompressed data cannot receive a compressed instance; the error names both syntaxes.

### How do I read or change a tag?

Read it with `${dicom.PatientID}` in Simple or the [`dicom` language](../languages/dicom.md).
To change tags, unmarshal with the [`dicom` data format](../dataformats/dicom.md), edit the
Value tree, and marshal it back.

### Is TLS supported?

Yes, TLS 1.2 and 1.3 through `sslContextParameters`, which matches the DICOM BCP 195 TLS profiles
(PS3.15 B.9 to B.13). Port 2762 is the usual port for DICOM over TLS.

### What is not implemented?

Storage Commitment, MPPS (N-CREATE, N-SET), C-CANCEL of a request in progress, asynchronous
operations (one request at a time per association) and association pooling.

## Related

- [dicomweb](dicomweb.md), [dicom data format](../dataformats/dicom.md), [dicomDeidentify](../dataformats/dicomDeidentify.md), [dicom language](../languages/dicom.md), [mllp](mllp.md), [fhir](fhir.md)
- [DICOM guide](../guides/dicom.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
