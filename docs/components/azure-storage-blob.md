# Azure Storage Blob component

> The `azure-storage-blob` component uploads, downloads, lists, copies and deletes blobs in Azure Blob Storage (block, append and page blobs), and polls a container for new blobs.

**Scheme:** `azure-storage-blob:` · **Crate:** `cameleer-cloud` · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [Azure Storage Blob Service](https://camel.apache.org/components/next/azure-storage-blob-component.html)

## When to use it

- Archive files, reports and message payloads in Azure.
- Read files that partners drop into a container, then delete them.
- Append log lines to an append blob from many routes or devices.
- React to every blob change in an account with the change feed consumer.

## URI format

```text
azure-storage-blob://accountName[/containerName][?options]
```

## Example

Upload a daily report. The blob name comes from the `CamelAzureStorageBlobBlobName` header (or the
`blobName` option):

<!-- example: azure-storage-blob-upload-report -->
```yaml
- route:
    id: upload-report
    from:
      uri: "timer:daily-report?repeatCount=1"
      steps:
        - setBody:
            constant: "region,total;eu,42;us,17"
        - setHeader:
            name: CamelAzureStorageBlobBlobName
            simple: "reports/${date:now:yyyy-MM-dd}.csv"
        - setHeader:
            name: CamelAzureStorageBlobContentType
            constant: text/csv
        - to: "azure-storage-blob://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/reports?operation=uploadBlockBlob&accessKey=RAW({{env:AZURE_STORAGE_KEY:}})"
        - log: "uploaded ${header.CamelAzureStorageBlobBlobName} (etag ${header.CamelAzureStorageBlobETag})"
```

Poll a container and delete each blob after it was processed:

<!-- example: azure-storage-blob-consume-incoming -->
```yaml
- route:
    id: incoming-blobs
    from:
      uri: "azure-storage-blob://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/incoming?accessKey=RAW({{env:AZURE_STORAGE_KEY:}})&deleteAfterRead=true"
      steps:
        - log: "received ${header.CamelAzureStorageBlobBlobName} (${header.CamelAzureStorageBlobBlobSize} bytes)"
        - to: "log:incoming"
```

Instead of the account key you can use a SAS token (`sasToken=RAW(...)`) or Microsoft Entra ID
(`credentialType=AZURE_IDENTITY`, for managed identities); see
[Cloud credentials](../guides/cloud-credentials.md#azure).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("azure-storage-blob://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/incoming?accessKey=RAW({{env:AZURE_STORAGE_KEY:}})&deleteAfterRead=true")
        .log("received ${header.CamelAzureStorageBlobBlobName} (${header.CamelAzureStorageBlobBlobSize} bytes)")
        .to("log:incoming");
})?;
```

## Options

Common Azure options (`accessKey`, `sasToken`, `credentialType`, Entra ID options,
`uriEndpointOverride`, `overrideEndpoint`, `proxyHost`, `proxyPort`, `trustAllCertificates`) are
described in [Cloud credentials](../guides/cloud-credentials.md#azure-options).

| option | default | description |
|---|---|---|
| `operation` | `listBlobContainers` | Containers: `listBlobContainers`, `createBlobContainer`, `deleteBlobContainer`. Blobs: `listBlobs`, `uploadBlockBlob`, `getBlob`, `deleteBlob`, `downloadBlobToFile`, `downloadLink`, `copyBlob`, `stageBlockBlobList`, `commitBlobBlockList`, `getBlobBlockList`. Append blobs: `createAppendBlob`, `commitAppendBlob`. Page blobs: `createPageBlob`, `uploadPageBlob`, `resizePageBlob`, `clearPageBlob`, `getPageBlobRanges`. Leases (Cameleer): `acquireBlobLease`, `renewBlobLease`, `releaseBlobLease`, `breakBlobLease`. Change feed: `getChangeFeed`. |
| `blobName` | | Blob (the `CamelAzureStorageBlobBlobName` header wins). |
| `prefix` | | `listBlobs` and consumer: only blobs with this prefix. |
| `maxResultsPerPage` | | Page size of listings (all pages are read). |
| `deleteAfterRead` | `false` | Cameleer: consumer deletes blobs after successful processing (otherwise it reads the container again on every poll, as in Camel). |
| `createAppendBlob` | `true` | `commitAppendBlob` creates the blob when missing. |
| `createPageBlob` | `true` | `uploadPageBlob` creates the blob when missing. |
| `pageBlobSize` | `512` | Size of `createPageBlob`. |
| `leaseBlob` | `false` | Hold a lease while writing or deleting. |
| `leaseDurationInSeconds` | `60` | Lease duration (-1 = infinite). |
| `commitBlockListLater` | `true` | `stageBlockBlobList` stages without committing. |
| `blockListType` | `COMMITTED` | `getBlobBlockList`: `COMMITTED`, `UNCOMMITTED` or `ALL`. |
| `fileDir` | | Target directory of `downloadBlobToFile`. |
| `downloadLinkExpiration` | 1 hour | Validity (ms) of `downloadLink`. |
| `sourceBlobAccessKey` | | Key of the source account of a cross-account `copyBlob`. |
| `changeFeed` | `false` | Cameleer: consume the account's change feed instead of a container. |
| `changeFeedStartTime`, `changeFeedEndTime` | | Change feed window (epoch millis or ISO-8601). |

The consumer also accepts the scheduled poll options (`delay`, `initialDelay`, `scheduler.cron`, ...).

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAzureStorageBlobOperation` | in | Overrides `operation`. |
| `CamelAzureStorageBlobBlobName`, `CamelAzureStorageBlobContainerName` | in/out | Blob and container. |
| `CamelAzureStorageBlobContentType`, `CamelAzureStorageBlobMetadata` | in/out | Content type and metadata (map, `x-ms-meta-*`). |
| `CamelAzureStorageBlobETag`, `CamelAzureStorageBlobLastModified`, `CamelAzureStorageBlobBlobSize`, `CamelAzureStorageBlobContentMD5`, `CamelAzureStorageBlobBlobType` | out | Blob properties. |
| `CamelAzureStorageBlobPrefix` | in | Listing prefix. |
| `CamelAzureStorageBlobPageBlobRange`, `CamelAzureStorageBlobPageBlobSize`, `CamelAzureStorageBlobCreateAppendBlob`, `CamelAzureStorageBlobCreatePageBlob` | in | Append and page blobs. |
| `CamelAzureStorageBlobCommittedBlockCount` | out | Blocks of an append blob. |
| `CamelAzureStorageBlobLeaseId`, `CamelAzureStorageBlobLeaseDuration` | in/out | Leases. |
| `CamelAzureStorageBlobBlockIds`, `CamelAzureStorageBlobCommitBlobBlockListLater`, `CamelAzureStorageBlobBlockListType` | in/out | Block lists. |
| `CamelAzureStorageBlobDownloadLink`, `CamelAzureStorageBlobDownloadLinkExpiration` | in/out | `downloadLink`. |
| `CamelAzureStorageBlobFileDir`, `CamelAzureStorageBlobFileName` | in | `downloadBlobToFile`. |
| `CamelAzureStorageBlobSourceBlobAccountName`, `CamelAzureStorageBlobSourceBlobContainerName`, `CamelAzureStorageBlobSourceBlobName`, `CamelAzureStorageBlobCopyId`, `CamelAzureStorageBlobCopyStatus` | in/out | `copyBlob`. |
| `CamelAzureStorageBlobChangeFeedStartTime`, `CamelAzureStorageBlobChangeFeedEndTime`, `CamelAzureStorageBlobChangeFeedEventType`, `CamelAzureStorageBlobChangeFeedEventTime`, `CamelAzureStorageBlobChangeFeedEventId` | in/out | Change feed. |

## Run it with Docker

```sh
docker run --rm -e AZURE_STORAGE_ACCOUNT -e AZURE_STORAGE_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/blobs.camel.yaml
```

## Common questions

### Why do I get `ContainerNotFound`?

Uploads do not create the container. Create it once with `operation=createBlobContainer` (or in the
portal or your infrastructure code).

### How do I test against Azurite?

Use the account `devstoreaccount1` with Azurite's well-known key and add
`uriEndpointOverride=http://127.0.0.1:10000&overrideEndpoint=true`; requests then use Azurite's path
style (`http://127.0.0.1:10000/devstoreaccount1/...`).

### How do I share a blob with someone for an hour?

Use `operation=downloadLink`: the body becomes a read-only SAS URL valid for
`downloadLinkExpiration` milliseconds (default one hour).

### How do I use a managed identity?

Set `credentialType=AZURE_IDENTITY` and drop the key: tokens come from the environment, workload
identity or the managed identity endpoint.

## Related

- [Azure Storage Queue](azure-storage-queue.md), [Azure Data Lake Storage](azure-storage-datalake.md), [Azure Files](azure-storage-files.md)
- [AWS S3](aws2-s3.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel Azure Storage Blob](https://camel.apache.org/components/next/azure-storage-blob-component.html)
