# AWS S3 component

> The `aws2-s3` component uploads, downloads, lists, copies and deletes objects in Amazon S3 buckets, and polls a bucket for new objects.

**Scheme:** `aws2-s3:` · **Crate:** `cameleer-cloud` · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [AWS S3 Storage Service](https://camel.apache.org/components/next/aws2-s3-component.html)

## When to use it

- Archive files, reports or message payloads to S3.
- Read files that partners or other systems drop into a bucket, then move or delete them.
- Hand large documents to other services through presigned download or upload links.
- Collect sensor data on an edge device and upload it in multipart batches (`streamingUploadMode`).

## URI format

```text
aws2-s3://bucketNameOrArn[?options]
```

## Example

Upload a report. The object key comes from the `CamelAwsS3Key` header (or the `keyName` option):

<!-- example: aws2-s3-upload-report -->
```yaml
- route:
    id: upload-report
    from:
      uri: "timer:daily-report?repeatCount=1"
      steps:
        - setBody:
            constant: "region,total;eu,42;us,17"
        - setHeader:
            name: CamelAwsS3Key
            constant: reports/daily.csv
        - setHeader:
            name: CamelAwsS3ContentType
            constant: text/csv
        - to: "aws2-s3://{{env:S3_BUCKET:my-reports}}?region={{env:AWS_REGION:us-east-1}}&autoCreateBucket=true"
        - log: "uploaded s3://{{env:S3_BUCKET:my-reports}}/${header.CamelAwsS3Key}"
```

Poll a bucket for invoices. Each object becomes one exchange; after successful processing it is
copied to `processed-invoices` and deleted from the source bucket:

<!-- example: aws2-s3-consume-invoices -->
```yaml
- route:
    id: invoices-from-s3
    from:
      uri: "aws2-s3://incoming-invoices?region={{env:AWS_REGION:us-east-1}}&prefix=invoices/&moveAfterRead=true&destinationBucket=processed-invoices"
      steps:
        - log: "received ${header.CamelAwsS3Key} (${header.CamelAwsS3ContentLength} bytes)"
        - to: "log:invoices"
```

Credentials come from the default AWS chain; see [Cloud credentials](../guides/cloud-credentials.md).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-s3://incoming-invoices?region={{env:AWS_REGION:us-east-1}}&prefix=invoices/&moveAfterRead=true&destinationBucket=processed-invoices")
        .log("received ${header.CamelAwsS3Key} (${header.CamelAwsS3ContentLength} bytes)")
        .to("log:invoices");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `putObject` | Producer operation: `putObject`, `getObject`, `getObjectRange`, `headObject`, `listObjects`, `deleteObject`, `deleteObjects`, `copyObject`, `listBuckets`, `createBucket`, `deleteBucket`, `headBucket`, `createDownloadLink`, `createUploadLink`, `restoreObject`, `getObjectTagging`, `putObjectTagging`, `deleteObjectTagging`, `getObjectAcl`, `putObjectAcl`, and the Cameleer operations `putObjectRetention`, `getObjectRetention`, `putObjectLegalHold`, `getObjectLegalHold`, `putBucketVersioning`, `getBucketVersioning`, `listObjectVersions`. |
| `keyName` | | Object key (the `CamelAwsS3Key` header wins). |
| `prefix` | | Consumer and `listObjects`: only keys with this prefix. |
| `delimiter` | | Delimiter of the listing. |
| `fileName` | | Consumer: poll this single object. |
| `maxMessagesPerPoll` | `10` | Consumer: objects per poll. |
| `includeBody` | `true` | Consumer: download the object content. |
| `ignoreBody` | `false` | Consumer: do not download at all (HEAD only, empty body). |
| `includeFolders` | `true` | Consumer: also emit folder placeholder objects. |
| `deleteAfterRead` | `true` | Consumer: delete objects after successful processing. |
| `moveAfterRead` | `false` | Consumer: copy objects to `destinationBucket` first. |
| `destinationBucket`, `destinationBucketPrefix`, `destinationBucketSuffix` | | Target of `moveAfterRead`. |
| `localDirectory` | | Cameleer: consumer streams each object to `{localDirectory}/{key}` without buffering (body = path). |
| `autoCreateBucket` | `false` | Create the bucket when it does not exist. |
| `forcePathStyle` | `false` | Path-style addressing (virtual-host style otherwise; path style is used with an endpoint override). |
| `storageClass` | | Storage class of uploads. |
| `conditionalWritesEnabled` | `false` | Send `If-None-Match: *` so an existing object is never overwritten. |
| `multiPartUpload` | `false` | Upload bodies larger than `partSize` with a multipart upload. |
| `partSize` | 25 MiB | Part size of multipart uploads. |
| `streamingUploadMode` | `false` | Aggregate the bodies of many exchanges into multipart objects. |
| `batchMessageNumber`, `batchSize`, `streamingUploadTimeout` | `10`, `1000000` | When a streaming upload object is completed. |
| `namingStrategy` | `progressive` | Streaming upload object names: `progressive` or `random`. |
| `restartingPolicy` | `override` | Streaming upload: `override` or `lastPart`. |
| `useAwsKMS`, `awsKMSKeyId` | `false` | SSE-KMS encryption of uploads. |
| `useSSES3` | `false` | SSE-S3 encryption. |
| `useCustomerKey`, `customerKeyId`, `customerKeyMD5`, `customerAlgorithm` | `false` | SSE-C encryption with your own key. |

The consumer also accepts the scheduled poll options (`delay` default 500 ms, `initialDelay`,
`greedy`, `scheduler.cron`, ...).

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsS3Key` | in/out | Object key. |
| `CamelAwsS3BucketName` | in/out | Bucket name. |
| `CamelAwsS3Operation` | in | Overrides `operation`. |
| `CamelAwsS3ContentType`, `CamelAwsS3ContentLength` | in/out | Content type and length. |
| `CamelAwsS3ETag`, `CamelAwsS3LastModified`, `CamelAwsS3VersionId` | out | Object metadata (`CamelAwsS3VersionId` is also an input of get, head and delete). |
| `CamelAwsS3Metadata` | in/out | User metadata (map). |
| `CamelAwsS3BucketDestinationName`, `CamelAwsS3DestinationKey` | in | Target of `copyObject`. |
| `CamelAwsS3Prefix`, `CamelAwsS3Delimiter` | in | Listing filters. |
| `CamelAwsS3StorageClass` | in | Storage class. |
| `CamelAwsS3RangeStart`, `CamelAwsS3RangeEnd` | in | Byte range of `getObjectRange`. |
| `CamelAwsS3DownloadLinkExpirationTime`, `CamelAwsS3UploadLinkExpirationTime` | in | Expiry (millis) of presigned links. |
| `CamelAwsS3KeysToDelete`, `CamelAwsS3DeleteErrors` | in/out | Keys of `deleteObjects` and its failures. |
| `CamelAwsS3CannedAcl`, `CamelAwsS3ServerSideEncryption`, `CamelAwsS3ContentDisposition`, `CamelAwsS3ContentEncoding`, `CamelAwsS3ContentControl`, `CamelAwsS3ObjectTags`, `CamelAwsS3Headers` | in | Upload settings. |
| `CamelAwsS3ObjectLockMode`, `CamelAwsS3ObjectLockRetainUntilDate`, `CamelAwsS3ObjectLockLegalHoldStatus`, `CamelAwsS3BypassGovernanceRetention`, `CamelAwsS3VersioningStatus` | in | Object lock and versioning. |
| `CamelAwsS3RestoreDays`, `CamelAwsS3RestoreTier` | in | `restoreObject`. |
| `CamelAwsS3SourceFile` | in | Cameleer: upload this local file without loading it in memory. |
| `CamelAwsS3DownloadFile` | in | Cameleer: `getObject` streams the object into this local file. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e S3_BUCKET \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/s3.camel.yaml
```

## Common questions

### How do I upload a large file without loading it in memory?

Set the `CamelAwsS3SourceFile` header to the file path. With `multiPartUpload=true` the file is
uploaded one part at a time; otherwise it is streamed in a single PUT.

### How do I create a presigned download link?

Use `operation=createDownloadLink` with `CamelAwsS3Key`; the body becomes the URL, valid for
`CamelAwsS3DownloadLinkExpirationTime` milliseconds.

### How do I keep the objects in the bucket after reading them?

Set `deleteAfterRead=false`. The consumer then sees the same objects again on every poll, so
usually you combine it with an idempotent consumer or move the objects with `moveAfterRead`.

### Does it work with MinIO or LocalStack?

Yes: `uriEndpointOverride=http://localhost:9000&overrideEndpoint=true` (path-style addressing is used
automatically with an override).

## Related

- [AWS SQS](aws2-sqs.md), [AWS Textract](aws2-textract.md), [AWS Rekognition](aws2-rekognition.md)
- [Azure Storage Blob](azure-storage-blob.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS S3](https://camel.apache.org/components/next/aws2-s3-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
