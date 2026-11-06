# Azure Files component

> The `azure-storage-files` component uploads, downloads, lists and deletes files and directories in Azure Files shares, and polls a directory for new files.

**Scheme:** `azure-storage-files:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** Camel's [Azure Files](https://camel.apache.org/components/next/azure-files-component.html) component uses the scheme `azure-files` and FTP-style options; `azure-storage-files` follows the other Azure storage components and also supports those FTP-style options

## When to use it

- Exchange files with applications that mount an Azure file share (SMB) from a route.
- Pick up files written to a share, then delete or move them.
- Publish exports to a share with an atomic rename (`tempPrefix`).

## URI format

```text
azure-storage-files://accountName/shareName[?options]
```

## Example

Upload an export into a directory of a share:

<!-- example: azure-storage-files-upload-export -->
```yaml
- route:
    id: export-to-share
    from:
      uri: "timer:export?repeatCount=1"
      steps:
        - setBody:
            constant: "sku,stock;A-1,12;B-7,0"
        - setHeader:
            name: CamelAzureStorageFilesFileName
            constant: stock.csv
        - to: "azure-storage-files://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/exports?operation=uploadFile&directoryName=daily&accessKey=RAW({{env:AZURE_STORAGE_KEY:}})"
        - log: "uploaded daily/${header.CamelAzureStorageFilesFileName}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:export?repeatCount=1")
        .set_body(constant("sku,stock;A-1,12;B-7,0"))
        .set_header("CamelAzureStorageFilesFileName", constant("stock.csv"))
        .to("azure-storage-files://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/exports?operation=uploadFile&directoryName=daily&accessKey=RAW({{env:AZURE_STORAGE_KEY:}})")
        .log("uploaded daily/${header.CamelAzureStorageFilesFileName}");
})?;
```

## Options

Common Azure options (credentials, endpoint override, proxy) are described in
[Cloud credentials](../guides/cloud-credentials.md#azure-options). Shared Key and SAS are supported.

| option | default | description |
|---|---|---|
| `operation` | `listDirectoriesAndFiles` | `listShares`, `createShare`, `deleteShare`, `listDirectoriesAndFiles`, `createDirectory`, `deleteDirectory`, `uploadFile`, `downloadFile`, `getFileProperties`, `deleteFile`, `renameFile` (Cameleer). |
| `directoryName` | share root | Directory. |
| `fileName` | | File (the `CamelAzureStorageFilesFileName` header wins). |
| `prefix` | | Listing prefix. |
| `maxMessagesPerPoll` | `10` | Consumer: files per poll. |
| `delete` | `false` | Consumer: delete processed files (otherwise files already delivered, by name and ETag, are skipped). |
| `recursive`, `minDepth`, `maxDepth` | `false` | Consumer: descend into sub-directories. |
| `include`, `exclude` | | Consumer: regular expressions on file names. |
| `noop`, `idempotent`, `idempotentKey`, `idempotentRepository` | `false` | Consumer: leave files in place and remember them (`idempotentKey=name-etag` re-reads changed files). |
| `preMove`, `move`, `moveFailed` | | Consumer: rename files before processing, after success or after failure. |
| `fileExist` | `Override` | `uploadFile` when the file exists: `Override`, `Append`, `Fail`, `Ignore`, `Move` (into `moveExisting`). |
| `tempPrefix` | | Upload to a temporary name, then rename (atomic replace). |
| `autoCreate` | `true` | Create missing directories of `CamelFileName`. |
| `flatten` | `false` | Keep only the last segment of the file name. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAzureStorageFilesOperation` | in | Overrides `operation`. |
| `CamelAzureStorageFilesShareName`, `CamelAzureStorageFilesDirectoryName`, `CamelAzureStorageFilesFileName` | in/out | Location. |
| `CamelAzureStorageFilesPrefix`, `CamelAzureStorageFilesContentType` | in | Listing prefix, content type. |
| `CamelAzureStorageFilesContentLength`, `CamelAzureStorageFilesETag`, `CamelAzureStorageFilesLastModified` | out | File properties. |
| `CamelAzureStorageFilesRenameTo` | in | Target of `renameFile`. |
| `CamelFileName`, `CamelFileNameOnly`, `CamelFileParent`, `CamelFileRelativePath`, `CamelFileLength` | out | Consumer file headers. |

## Run it with Docker

```sh
docker run --rm -e AZURE_STORAGE_ACCOUNT -e AZURE_STORAGE_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/export.camel.yaml
```

## Common questions

### How do I avoid readers seeing half-written files?

Set `tempPrefix=.tmp-`: the file is uploaded under a temporary name and renamed when complete.

### Can I process files and keep them?

Yes: `noop=true` leaves them and remembers which ones were consumed (in memory, or in your
`idempotentRepository`).

## Related

- [Azure Storage Blob](azure-storage-blob.md), [Azure Data Lake Storage](azure-storage-datalake.md)
- [Cloud credentials](../guides/cloud-credentials.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
