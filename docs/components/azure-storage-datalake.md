# Azure Data Lake Storage component

> The `azure-storage-datalake` component reads, writes, appends, lists and deletes files and directories in Azure Data Lake Storage Gen2, sets ACLs and runs query acceleration.

**Scheme:** `azure-storage-datalake:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [Azure Storage Datalake Service](https://camel.apache.org/components/next/azure-storage-datalake-component.html)

## When to use it

- Land raw data (events, sensor readings, exports) in a data lake for analytics.
- Append records to a file in place, then flush it (`appendToFile`, `flushToFile`).
- Pick up files that analytics jobs write and hand them to other systems.
- Filter large CSV or JSON files on the server with SQL (`openQueryInputStream`).

## URI format

```text
azure-storage-datalake://accountName[/fileSystemName][?options]
```

## Example

Upload a file into a directory of a file system:

<!-- example: azure-storage-datalake-upload -->
```yaml
- route:
    id: land-readings
    from:
      uri: "timer:readings?repeatCount=1"
      steps:
        - setBody:
            constant: '{"device":"edge-7","temperature":21.5}'
        - setHeader:
            name: CamelAzureStorageDataLakeFileName
            constant: readings.json
        - to: "azure-storage-datalake://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/raw?operation=upload&directoryName=sensors&accountKey=RAW({{env:AZURE_STORAGE_KEY:}})"
        - log: "stored sensors/${header.CamelAzureStorageDataLakeFileName}"
```

`upload` creates the file, appends the body and flushes it. The directory name is prefixed to the
file name.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:readings?repeatCount=1")
        .set_body(constant(r#"{"device":"edge-7","temperature":21.5}"#))
        .set_header("CamelAzureStorageDataLakeFileName", constant("readings.json"))
        .to("azure-storage-datalake://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/raw?operation=upload&directoryName=sensors&accountKey=RAW({{env:AZURE_STORAGE_KEY:}})")
        .log("stored sensors/${header.CamelAzureStorageDataLakeFileName}");
})?;
```

## Options

Common Azure options (credentials, endpoint override, proxy) are described in
[Cloud credentials](../guides/cloud-credentials.md#azure-options). The account key option is
`accountKey` (as in Camel; `accessKey` works too).

| option | default | description |
|---|---|---|
| `operation` | `listFileSystem` | `listFileSystem`, `createFileSystem`, `deleteFileSystem`, `listPaths`, `createFile`, `upload`, `uploadFromFile`, `appendToFile`, `flushToFile`, `getFile`, `downloadToFile`, `downloadLink`, `deleteFile`, `createDirectory` (Cameleer), `deleteDirectory`, `openQueryInputStream`, and the Cameleer ACL operations `getAccessControl`, `setAccessControl`. |
| `fileName` | | File (the `CamelAzureStorageDataLakeFileName` header wins). |
| `directoryName` | | Directory, prefixed to the file name. |
| `path` | | Directory of `listPaths` and of the consumer. |
| `recursive` | `false` | Recursive listing and consumer. |
| `regex` | | Consumer: only file names matching this regular expression. |
| `deleteAfterRead` | `false` | Cameleer: consumer deletes processed files (otherwise it reads them again on every poll, as in Camel). |
| `permission`, `umask` | | Permissions of `createFile` (symbolic `rwxr-x---` or octal). |
| `userPrincipalNameReturned` | `false` | `getAccessControl` returns user principal names. |
| `fileDir` | | Local directory of `downloadToFile` and `uploadFromFile`. |
| `downloadLinkExpiration` | 1 hour | Validity (ms) of `downloadLink`. |
| `expression` | | SQL of `openQueryInputStream`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAzureStorageDataLakeOperation` | in | Overrides `operation`. |
| `CamelAzureStorageDataLakeFileSystemName`, `CamelAzureStorageDataLakeDirectoryName`, `CamelAzureStorageDataLakeFileName`, `CamelAzureStorageDataLakePath`, `CamelAzureStorageDataLakeRecursive` | in/out | Location. |
| `CamelAzureStorageDataLakeFileOffset`, `CamelAzureStorageDataLakePosition` | in/out | `appendToFile` offset and the position to flush. |
| `CamelAzureStorageDataLakeETag`, `CamelAzureStorageDataLakeLastModified`, `CamelAzureStorageDataLakeContentLength` | out | File properties. |
| `CamelAzureStorageDataLakeFileDir`, `CamelAzureStorageDataLakeDownloadLink`, `CamelAzureStorageDataLakeDownloadLinkExpiration` | in/out | Downloads and links. |
| `CamelAzureStorageDataLakeExpression`, `CamelAzureStorageDataLakeQueryInputFormat`, `CamelAzureStorageDataLakeQueryHasHeaders` | in | Query acceleration (`csv` default or `json`). |
| `CamelAzureStorageDataLakeAcl`, `CamelAzureStorageDataLakePermission`, `CamelAzureStorageDataLakeOwner`, `CamelAzureStorageDataLakeGroup` | in | `setAccessControl`. |

## Run it with Docker

```sh
docker run --rm -e AZURE_STORAGE_ACCOUNT -e AZURE_STORAGE_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/lake.camel.yaml
```

## Common questions

### How do I append to an existing file?

Use `appendToFile` (the data goes to the current end, or `CamelAzureStorageDataLakeFileOffset`), then
`flushToFile`, which commits up to `CamelAzureStorageDataLakePosition`.

### How do I use a service principal?

Set `clientId`, `clientSecret` and `tenantId` (with `credentialType=CLIENT_SECRET`), or use
`credentialType=AZURE_IDENTITY` for workload and managed identities.

## Related

- [Azure Storage Blob](azure-storage-blob.md), [Azure Files](azure-storage-files.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel Azure Storage Data Lake](https://camel.apache.org/components/next/azure-storage-datalake-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
