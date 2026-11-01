//! The `azure-storage-datalake` component (Camel `camel-azure-storage-datalake`, producer) over
//! the ADLS Gen2 DFS REST API (`{account}.dfs.core.windows.net`), Shared Key or SAS.
//!
//! `azure-storage-datalake://accountName[/fileSystemName]?accountKey=RAW(...)&operation=...`
//!
//! Operations (`operation` / `CamelAzureStorageDataLakeOperation`, default `listFileSystem`):
//! `listFileSystem`, `createFileSystem`, `deleteFileSystem`, `listPaths` (`path` /
//! `CamelAzureStorageDataLakePath` directory, `recursive`; body = list of
//! `{name, isDirectory, contentLength, lastModified, eTag}`), `createFile`, `upload` (body
//! written with create + append + flush), `appendToFile` (`fileOffset` /
//! `CamelAzureStorageDataLakeFileOffset`, default = current length; sets
//! `CamelAzureStorageDataLakePosition` to the end of the appended data), `flushToFile`
//! (commits up to `CamelAzureStorageDataLakePosition`), `getFile`
//! (body = bytes), `deleteFile`, `createDirectory` (rust-camel), `deleteDirectory`
//! (recursive). The file comes from `CamelAzureStorageDataLakeFileName` / `fileName`, the
//! directory from `CamelAzureStorageDataLakeDirectoryName` / `directoryName` (prefixed to the
//! file name), the file system from `CamelAzureStorageDataLakeFileSystemName` or the path.
//! Credentials: `accountKey` (or `accessKey`) Shared Key, `sasToken`, Entra ID client secret
//! (`clientId` / `clientSecret` / `tenantId`, `credentialType=CLIENT_SECRET`) or
//! `credentialType=AZURE_IDENTITY` (workload / managed identity); Azurite-style
//! `uriEndpointOverride` + `overrideEndpoint`.
//!
//! More operations, ACLs, query acceleration and the consumer: see [`super::datalake_ops`].

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

use super::AzureConfig;
use crate::endpoint::{operation, unsupported, CloudService};
use crate::http::{Request, Response};
use crate::util::{header_i64, header_str, kinds, obj, parse_json, uri_encode, Opts};

/// Header names (Camel `DataLakeConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAzureStorageDataLakeOperation";
    pub const FILESYSTEM_NAME: &str = "CamelAzureStorageDataLakeFileSystemName";
    pub const DIRECTORY_NAME: &str = "CamelAzureStorageDataLakeDirectoryName";
    pub const FILE_NAME: &str = "CamelAzureStorageDataLakeFileName";
    pub const PATH: &str = "CamelAzureStorageDataLakePath";
    pub const RECURSIVE: &str = "CamelAzureStorageDataLakeRecursive";
    pub const FILE_OFFSET: &str = "CamelAzureStorageDataLakeFileOffset";
    pub const POSITION: &str = "CamelAzureStorageDataLakePosition";
    pub const E_TAG: &str = "CamelAzureStorageDataLakeETag";
    pub const LAST_MODIFIED: &str = "CamelAzureStorageDataLakeLastModified";
    pub const CONTENT_LENGTH: &str = "CamelAzureStorageDataLakeContentLength";
}

/// The Data Lake endpoint configuration and client.
pub struct DataLakeService {
    pub azure: AzureConfig,
    pub file_system: Option<String>,
    pub operation: Option<String>,
    pub file_name: Option<String>,
    pub directory_name: Option<String>,
    pub path: Option<String>,
    pub recursive: bool,
    pub permission: Option<String>,
    pub umask: Option<String>,
    pub file_dir: Option<String>,
    pub download_link_expiration: Option<i64>,
    pub expression: Option<String>,
    pub regex: Option<String>,
    pub delete_after_read: bool,
    pub user_principal_name_returned: bool,
    pub scheduler: camel_core::component::scheduled::ScheduledPollConfig,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let (account, fs) = match remaining.split_once('/') {
        Some((a, f)) => (a, Some(f.trim_matches('/').to_string()).filter(|f| !f.is_empty())),
        None => (remaining, None),
    };
    let azure = AzureConfig::from_opts(account, o)?;
    let s = DataLakeService {
        file_system: fs,
        operation: o.get("operation"),
        file_name: o.get("fileName"),
        directory_name: o.get("directoryName"),
        path: o.get("path"),
        recursive: o.bool("recursive", false)?,
        permission: o.get("permission"),
        umask: o.get("umask"),
        file_dir: o.get("fileDir"),
        download_link_expiration: o.get("downloadLinkExpiration").map(|v| v.parse::<i64>().map_err(|_| CamelError::illegal_argument(format!("Invalid downloadLinkExpiration: {}", v)))).transpose()?,
        expression: o.get("expression"),
        regex: o.get("regex"),
        delete_after_read: o.bool("deleteAfterRead", false)?,
        user_principal_name_returned: o.bool("userPrincipalNameReturned", false)?,
        scheduler: camel_core::component::scheduled::ScheduledPollConfig::from_params(o.params, std::time::Duration::from_millis(1000), std::time::Duration::from_millis(500))?,
        azure,
    };
    o.ignore(&["dataLakeServiceClient", "close", "closeStreamAfterRead", "retainUncommitedData", "maxResults", "dataCount", "maxRetryRequests", "openOptions", "timeout"]);
    Ok(Arc::new(s))
}

impl DataLakeService {
    pub(crate) fn send(&self, req: Request) -> Result<Response> {
        self.azure.send(req, kinds::DATALAKE_STORAGE)
    }

    pub(crate) fn fs(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::FILESYSTEM_NAME)
            .or_else(|| self.file_system.clone())
            .ok_or_else(|| CamelError::illegal_argument("File system name must be specified (path or CamelAzureStorageDataLakeFileSystemName)"))
    }

    pub(crate) fn file(&self, ex: &Exchange) -> Result<String> {
        let name = header_str(ex, headers::FILE_NAME).or_else(|| self.file_name.clone()).ok_or_else(|| CamelError::illegal_argument("File name must be specified (fileName / CamelAzureStorageDataLakeFileName)"))?;
        Ok(match header_str(ex, headers::DIRECTORY_NAME).or_else(|| self.directory_name.clone()) {
            Some(d) => format!("{}/{}", d.trim_end_matches('/'), name.trim_start_matches('/')),
            None => name,
        })
    }

    pub(crate) fn path_url(&self, fs: &str, path: &str, query: &str) -> Result<crate::http::Url> {
        self.azure.url("dfs", &format!("{}/{}", uri_encode(fs, false), uri_encode(path.trim_start_matches('/'), true)), query)
    }

    /// Create (or overwrite) an empty file.
    pub fn create_file(&self, fs: &str, path: &str) -> Result<Response> {
        let mut req = Request::new("PUT", self.path_url(fs, path, "resource=file")?);
        if let Some(p) = &self.permission {
            req.set_header("x-ms-permissions", p.clone());
        }
        if let Some(u) = &self.umask {
            req.set_header("x-ms-umask", u.clone());
        }
        self.send(req)
    }

    /// Append data at `position` (uncommitted until flushed).
    pub fn append(&self, fs: &str, path: &str, position: i64, data: Vec<u8>) -> Result<Response> {
        self.send(Request::new("PATCH", self.path_url(fs, path, &format!("action=append&position={}", position))?).body(data))
    }

    /// Commit the data appended so far (`position` = total file length).
    pub fn flush(&self, fs: &str, path: &str, position: i64) -> Result<Response> {
        self.send(Request::new("PATCH", self.path_url(fs, path, &format!("action=flush&position={}", position))?))
    }

    fn length(&self, fs: &str, path: &str) -> Result<i64> {
        let r = self.send(Request::new("HEAD", self.path_url(fs, path, "")?))?;
        Ok(r.header("Content-Length").and_then(|l| l.parse().ok()).unwrap_or(0))
    }

    /// List paths (all pages).
    pub fn list_paths(&self, fs: &str, directory: Option<&str>, recursive: bool) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut q = vec!["resource=filesystem".to_string(), format!("recursive={}", recursive)];
            if let Some(d) = directory {
                q.push(format!("directory={}", uri_encode(d, false)));
            }
            if let Some(c) = &continuation {
                q.push(format!("continuation={}", uri_encode(c, false)));
            }
            let r = self.send(Request::new("GET", self.azure.url("dfs", &uri_encode(fs, false), &q.join("&"))?))?;
            let v = parse_json(&r.body)?;
            for p in v.get("paths").and_then(|p| p.as_array()).cloned().unwrap_or_default() {
                let g = |k: &str| p.get(k).map(|x| x.to_string()).unwrap_or_default();
                out.push(obj([
                    ("name", Value::from(g("name"))),
                    ("isDirectory", Value::Bool(g("isDirectory") == "true")),
                    ("contentLength", Value::Int(g("contentLength").parse().unwrap_or(0))),
                    ("lastModified", Value::from(g("lastModified"))),
                    ("eTag", Value::from(g("etag"))),
                ]));
            }
            continuation = r.header("x-ms-continuation").map(|c| c.to_string()).filter(|c| !c.is_empty());
            if continuation.is_none() {
                return Ok(out);
            }
        }
    }
}

impl CloudService for DataLakeService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "listFileSystem");
        match op.as_str() {
            "listFileSystem" => {
                let r = self.send(Request::new("GET", self.azure.url("dfs", "", "resource=account")?))?;
                let v = parse_json(&r.body)?;
                let list: Vec<Value> = v
                    .get("filesystems")
                    .and_then(|f| f.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|f| obj([("name", f.get("name").cloned().unwrap_or_default()), ("lastModified", f.get("lastModified").cloned().unwrap_or_default())]))
                    .collect();
                ex.set_body(Value::Array(list));
            }
            "createFileSystem" | "deleteFileSystem" => {
                let method = if op == "createFileSystem" { "PUT" } else { "DELETE" };
                self.send(Request::new(method, self.azure.url("dfs", &uri_encode(&self.fs(ex)?, false), "resource=filesystem")?))?;
            }
            "listPaths" => {
                let dir = header_str(ex, headers::PATH).or_else(|| self.path.clone()).or_else(|| header_str(ex, headers::DIRECTORY_NAME)).or_else(|| self.directory_name.clone());
                let recursive = ex.header(headers::RECURSIVE).map(|v| v.is_truthy()).unwrap_or(self.recursive);
                let v = self.list_paths(&self.fs(ex)?, dir.as_deref(), recursive)?;
                ex.set_body(Value::Array(v));
            }
            "createFile" => {
                let r = self.create_file(&self.fs(ex)?, &self.file(ex)?)?;
                if let Some(e) = r.header("ETag") {
                    ex.set_header(headers::E_TAG, e.to_string());
                }
            }
            "createDirectory" => {
                let dir = header_str(ex, headers::DIRECTORY_NAME).or_else(|| self.directory_name.clone()).ok_or_else(|| CamelError::illegal_argument("createDirectory requires a directory name"))?;
                self.send(Request::new("PUT", self.path_url(&self.fs(ex)?, &dir, "resource=directory")?))?;
            }
            "upload" => {
                let (fs, path) = (self.fs(ex)?, self.file(ex)?);
                let data = ex.body().to_bytes();
                let len = data.len() as i64;
                self.create_file(&fs, &path)?;
                if len > 0 {
                    self.append(&fs, &path, 0, data)?;
                }
                let r = self.flush(&fs, &path, len)?;
                if let Some(e) = r.header("ETag") {
                    ex.set_header(headers::E_TAG, e.to_string());
                }
                ex.set_header(headers::CONTENT_LENGTH, len);
            }
            "appendToFile" => {
                let (fs, path) = (self.fs(ex)?, self.file(ex)?);
                let offset = match header_i64(ex, headers::FILE_OFFSET) {
                    Some(o) => o,
                    None => self.length(&fs, &path)?,
                };
                let data = ex.body().to_bytes();
                let n = data.len() as i64;
                self.append(&fs, &path, offset, data)?;
                ex.set_header(headers::POSITION, offset + n);
            }
            "flushToFile" => {
                let (fs, path) = (self.fs(ex)?, self.file(ex)?);
                let pos = header_i64(ex, headers::POSITION).ok_or_else(|| CamelError::illegal_argument("flushToFile requires the CamelAzureStorageDataLakePosition header (the new file length)"))?;
                let r = self.flush(&fs, &path, pos)?;
                if let Some(e) = r.header("ETag") {
                    ex.set_header(headers::E_TAG, e.to_string());
                }
            }
            "getFile" => {
                let r = self.send(Request::new("GET", self.path_url(&self.fs(ex)?, &self.file(ex)?, "")?))?;
                if let Some(e) = r.header("ETag") {
                    ex.set_header(headers::E_TAG, e.to_string());
                }
                if let Some(v) = r.header("Last-Modified") {
                    ex.set_header(headers::LAST_MODIFIED, v.to_string());
                }
                ex.set_header(headers::CONTENT_LENGTH, r.body.len() as i64);
                ex.set_body(Value::Bytes(r.body));
            }
            "deleteFile" => {
                self.send(Request::new("DELETE", self.path_url(&self.fs(ex)?, &self.file(ex)?, "")?))?;
            }
            "deleteDirectory" => {
                let dir = header_str(ex, headers::DIRECTORY_NAME).or_else(|| self.directory_name.clone()).ok_or_else(|| CamelError::illegal_argument("deleteDirectory requires a directory name"))?;
                self.send(Request::new("DELETE", self.path_url(&self.fs(ex)?, &dir, "recursive=true")?))?;
            }
            other => {
                if !self.more_ops(ex, other)? {
                    return Err(unsupported("azure-storage-datalake", other));
                }
            }
        }
        Ok(())
    }

    fn scheduler(&self) -> Option<&camel_core::component::scheduled::ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn poll(&self, ctx: Option<camel_core::context::CamelContext>, processor: &dyn camel_core::spi::Processor, logger: &str) -> Result<usize> {
        self.poll_files(ctx, processor, logger)
    }
}
