# Google Cloud Storage component

> The `google-storage` component uploads, downloads, lists, copies and deletes objects in Google Cloud Storage buckets, and consumes new objects from a bucket.

**Scheme:** `google-storage:` · **Crate:** `cameleer-cloud` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Google Storage](https://camel.apache.org/components/next/google-storage-component.html)

## When to use it

- Upload files, reports or exports to a bucket ("write files to GCS").
- Pick up objects that land in a bucket and process them, then delete or archive them.
- Ship sensor data or logs from an edge device to the cloud: a small Cameleer binary watches a
  local folder and uploads each file, retrying on failure.
- Hand out time-limited download links (V4 signed URLs) to users or AI agents without making
  the bucket public.

## URI format

```text
google-storage://bucketName[?options]
```

The bucket is created on start when it does not exist (`autoCreateBucket=true`, in the project
of the service account key).

## Example

Upload every file that appears in a local folder, under its own file name:

<!-- example: google-storage-upload-files -->
```yaml
- route:
    id: files-to-gcs
    from:
      uri: "file:{{env:INBOX_DIR:inbox}}?move=.done"
      steps:
        - setHeader:
            name: CamelGoogleCloudStorageObjectName
            simple: "uploads/${file:name}"
        - to: "google-storage://{{env:GCS_BUCKET:my-bucket}}"
        - log: "uploaded ${file:name} to gs://{{env:GCS_BUCKET:my-bucket}}/uploads/"
```

Consume the objects of a bucket: each object becomes one exchange with its content as the body,
and after processing it is copied to an archive bucket and deleted from the source bucket:

<!-- example: google-storage-consume-and-archive -->
```yaml
- route:
    id: gcs-incoming
    from:
      uri: "google-storage://{{env:GCS_BUCKET:incoming-bucket}}?prefix=orders/&moveAfterRead=true&destinationBucket={{env:GCS_ARCHIVE_BUCKET:archive-bucket}}"
      steps:
        - log: "received ${header.CamelGoogleCloudStorageObjectName} (${header.CamelGoogleCloudStorageContentLength} bytes)"
        - setHeader:
            name: CamelFileName
            simple: "${header.CamelGoogleCloudStorageObjectName}"
        - to: "file:{{env:OUT_DIR:out}}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("file:inbox?move=.done")
        .set_header("CamelGoogleCloudStorageObjectName", simple("uploads/${file:name}"))
        .to("google-storage://my-bucket")
        .log("uploaded ${file:name}");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `operation` | `putObject` | Producer operation: `putObject`, `getObject`, `listObjects`, `deleteObject`, `copyObject`, `listBuckets`, `createBucket`, `deleteBucket`, `createDownloadLink`. The `CamelGoogleCloudStorageOperation` header overrides it. |
| `objectName` | | Object name for the producer (or the single object a consumer reads). The header `CamelGoogleCloudStorageObjectName` wins. |
| `prefix` | | Only list or consume objects whose name starts with this prefix. |
| `autoCreateBucket` | `true` | Create the bucket on start if it does not exist. |
| `storageClass` | `STANDARD` | Storage class of auto-created buckets. |
| `storageLocation` | `US-EAST1` | Location of auto-created buckets. |
| `deleteAfterRead` | `true` | Consumer: delete the object after it was processed successfully. |
| `moveAfterRead` | `false` | Consumer: copy the object to `destinationBucket` after processing. Needs `destinationBucket`. |
| `destinationBucket` | | Consumer: target bucket for `moveAfterRead`. |
| `includeBody` | `true` | Consumer: put the object content in the body (otherwise only the headers are set). |
| `includeFolders` | `true` | Consumer: also emit folder placeholder objects. |
| `maxMessagesPerPoll` | `10` | Consumer: objects per poll. |
| `resumableUploadThreshold` | `16777216` | Cameleer: bodies larger than this many bytes use a resumable upload session (`0` = always). |
| `resumableChunkSize` | `8388608` | Cameleer: chunk size of resumable uploads, rounded up to a multiple of 256 KiB. A failed chunk is resumed from the committed size (up to 5 times). |
| `delay`, `initialDelay`, ... | `500`, `1000` | Consumer poll schedule (milliseconds) and the other scheduled-poll options (`useFixedDelay`, `greedy`, `sendEmptyMessageWhenIdle`, `backoffMultiplier`, ...). |

### Credentials

These options are shared by every Google component (`google-pubsub`, `google-bigquery`,
`google-vertexai`, ...).

| option | default | description |
|---|---|---|
| `serviceAccountKey` | `GOOGLE_APPLICATION_CREDENTIALS` | Service account JSON key: a path, `file:path`, `classpath:path` or the inline JSON. An `authorized_user` file (from `gcloud auth application-default login`) or an `external_account` file (workload identity federation) works too. Without the option the `GOOGLE_APPLICATION_CREDENTIALS` environment variable is used, like Google's client libraries. |
| `clientId`, `clientSecret`, `refreshToken` | | OAuth user credentials (refresh token). They take precedence over a service account key. |
| `accessToken` | | A fixed bearer token. |
| `tokenServerUrl` | `https://oauth2.googleapis.com/token` | Cameleer: token endpoint for refresh-token credentials. |
| `useMetadataServer` | `false` | Take tokens from the GCE / GKE metadata server (also used when `GCE_METADATA_HOST` is set and no other credentials are given). |
| `metadataServerUrl` | | Cameleer: base URL of the metadata server. |
| `delegate` (or `subject`) | | User to impersonate with domain-wide delegation (JWT `sub` claim). |
| `authenticate` | `true` | `false` sends no credentials (emulators). |
| `uriEndpointOverride` + `overrideEndpoint=true`, or `endpoint` | | Send requests to another base URL, for example an emulator such as fake-gcs-server. |
| `proxyHost`, `proxyPort` | `3128` | HTTP proxy. |

Tokens are cached per credential until shortly before they expire, and refreshed after a 401.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGoogleCloudStorageOperation` | in | Operation to run (overrides `operation`). |
| `CamelGoogleCloudStorageBucketName` | in / out | Bucket for this exchange (overrides the URI); set by the consumer. |
| `CamelGoogleCloudStorageObjectName` | in / out | Object name; set by the consumer. |
| `CamelGoogleCloudStorageContentType` | in / out | Content type of the upload; set from the object metadata. |
| `CamelGoogleCloudStoragePrefix` | in | Prefix for `listObjects`. |
| `CamelGoogleCloudStorageDestinationBucketName` | in | Target bucket of `copyObject` (required). |
| `CamelGoogleCloudStorageDestinationObjectName` | in | Target name of `copyObject` (default: the same name). |
| `CamelGoogleCloudStorageDownloadLinkExpirationTime` | in | Lifetime of a `createDownloadLink` URL in milliseconds (default 5 minutes, at most 7 days). |
| `CamelGoogleCloudStorageDownloadLink` | out | The signed URL created by `createDownloadLink` (also the body). |
| `CamelGoogleCloudStorageContentLength` | out | Object size in bytes. |
| `CamelGoogleCloudStorageETag` | out | Object ETag. |
| `CamelGoogleCloudStorageContentMd5` | out | Base64 MD5 of the content. |
| `CamelGoogleCloudStorageCreateTime`, `CamelGoogleCloudStorageUpdateTime` | out | Object timestamps. |
| `CamelGoogleCloudStorageGeneration` | out | Object generation. |

Bodies: `putObject` returns the object metadata map, `getObject` the content bytes,
`listObjects` a list of metadata maps, `copyObject` the new object's metadata,
`deleteObject` and `deleteBucket` `true`.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" -v "$PWD/inbox:/inbox" \
  -v "$HOME/keys/sa.json:/keys/sa.json:ro" -e GOOGLE_APPLICATION_CREDENTIALS=/keys/sa.json \
  -e GCS_BUCKET=my-bucket -e INBOX_DIR=/inbox \
  pmsudhi/cameleer run /routes/files-to-gcs.camel.yaml
```

## Common questions

### How do I authenticate to Google Cloud Storage?
Set `GOOGLE_APPLICATION_CREDENTIALS` to a service account key file, or pass `serviceAccountKey`
on the URI. On GCE, GKE or Cloud Run use `useMetadataServer=true` and no key at all.

### How do I download a single object?
Send any message with `operation=getObject` and the `CamelGoogleCloudStorageObjectName` header;
the body becomes the object bytes.

### How do I create a signed download URL?
Use `operation=createDownloadLink` with the object name header. It needs a service account key
(the URL is signed with it, `GOOG4-RSA-SHA256`); the body is the URL.

### How is this different from Apache Camel?
Options and headers follow Camel. Resumable uploads (`resumableUploadThreshold`,
`resumableChunkSize`) are Cameleer additions; the `filter` and `downloadFileName` options are
accepted but ignored.

## Related

- [Google Pub/Sub](google-pubsub.md), [BigQuery](google-bigquery.md) (load jobs read `gs://` files)
- [AWS S3](aws2-s3.md) for the same pattern on AWS
- [Apache Camel Google Storage](https://camel.apache.org/components/next/google-storage-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
