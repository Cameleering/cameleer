//! The `azure-storage-blob` component (Camel `camel-azure-storage-blob`) over the Blob REST API.
//!
//! `azure-storage-blob://accountName[/containerName]?operation=...&blobName=...&accessKey=...`
//!
//! Producer operations (`operation` / `CamelAzureStorageBlobOperation`; default
//! `listBlobContainers` as in Camel): `listBlobContainers`, `createBlobContainer`,
//! `deleteBlobContainer`, `listBlobs` (`prefix` / `CamelAzureStorageBlobPrefix`,
//! `maxResultsPerPage`), `uploadBlockBlob` (body, `CamelAzureStorageBlobContentType`,
//! `CamelAzureStorageBlobMetadata` map → `x-ms-meta-*`), `getBlob`, `deleteBlob`.
//! The blob name comes from `CamelAzureStorageBlobBlobName` or `blobName`, the container from
//! `CamelAzureStorageBlobContainerName` or the path.
//!
//! Consumer: with `blobName`, polls that blob; otherwise lists the container (`prefix`) and
//! emits one exchange per blob (body = content) with the `CamelAzureStorageBlob*` headers. Blobs
//! are deleted after successful processing when `deleteAfterRead=true` (rust-camel extension,
//! default false like Camel, which re-reads the container on every poll).

use std::sync::Arc;
use std::time::Duration;

use camel_core::component::scheduled::ScheduledPollConfig;
use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::Processor;
use camel_core::value::Value;

use super::{error_code, AzureConfig};
use crate::endpoint::{operation, process, set_batch, unsupported, CloudService};
use crate::http::{Request, Response};
use crate::util::{header_str, kinds, obj, uri_encode, Opts, Xml};

/// Header names (Camel `BlobConstants`).
pub mod headers {
    pub const BLOB_NAME: &str = "CamelAzureStorageBlobBlobName";
    pub const BLOB_CONTAINER_NAME: &str = "CamelAzureStorageBlobContainerName";
    pub const OPERATION: &str = "CamelAzureStorageBlobOperation";
    pub const E_TAG: &str = "CamelAzureStorageBlobETag";
    pub const LAST_MODIFIED: &str = "CamelAzureStorageBlobLastModified";
    pub const CONTENT_TYPE: &str = "CamelAzureStorageBlobContentType";
    pub const BLOB_SIZE: &str = "CamelAzureStorageBlobBlobSize";
    pub const CONTENT_MD5: &str = "CamelAzureStorageBlobContentMD5";
    pub const BLOB_TYPE: &str = "CamelAzureStorageBlobBlobType";
    pub const METADATA: &str = "CamelAzureStorageBlobMetadata";
    pub const PREFIX: &str = "CamelAzureStorageBlobPrefix";
}

/// A blob of a listing.
#[derive(Clone, Debug, PartialEq)]
pub struct BlobItem {
    pub name: String,
    pub size: i64,
    pub etag: String,
    pub last_modified: String,
    pub content_type: String,
}

impl BlobItem {
    pub fn to_value(&self) -> Value {
        obj([
            ("name", Value::from(self.name.clone())),
            ("size", Value::Int(self.size)),
            ("eTag", Value::from(self.etag.clone())),
            ("lastModified", Value::from(self.last_modified.clone())),
            ("contentType", Value::from(self.content_type.clone())),
        ])
    }
}

/// The blob endpoint configuration and client.
pub struct BlobService {
    pub azure: AzureConfig,
    pub container: Option<String>,
    pub operation: Option<String>,
    pub blob_name: Option<String>,
    pub prefix: Option<String>,
    pub max_results_per_page: Option<i64>,
    pub delete_after_read: bool,
    pub scheduler: ScheduledPollConfig,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let (account, container) = match remaining.split_once('/') {
        Some((a, c)) => (a, Some(c.trim_matches('/').to_string()).filter(|c| !c.is_empty())),
        None => (remaining, None),
    };
    let azure = AzureConfig::from_opts(account, o)?;
    let s = BlobService {
        container,
        operation: o.get("operation"),
        blob_name: o.get("blobName"),
        prefix: o.get("prefix"),
        max_results_per_page: o.get("maxResultsPerPage").and_then(|v| v.parse().ok()),
        delete_after_read: o.bool("deleteAfterRead", false)?,
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        azure,
    };
    o.ignore(&["blobType", "closeStreamAfterRead", "closeStreamAfterWrite", "dataCount", "fileDir", "blobOffset", "regex", "downloadLinkExpiration", "changeFeedStartTime", "changeFeedEndTime", "leaseBlob", "leaseDurationInSeconds", "commitBlockListLater", "createAppendBlob", "createPageBlob", "blockListType", "pageBlobSize", "maxRetryRequests"]);
    Ok(Arc::new(s))
}

impl BlobService {
    fn send(&self, req: Request) -> Result<Response> {
        self.azure.send(req, kinds::BLOB_STORAGE)
    }

    fn blob_path(container: &str, blob: &str) -> String {
        format!("{}/{}", uri_encode(container, false), uri_encode(blob, true))
    }

    pub fn upload_block_blob(&self, container: &str, blob: &str, data: Vec<u8>, content_type: Option<&str>, metadata: &[(String, String)]) -> Result<Response> {
        let mut req = Request::new("PUT", self.azure.url("blob", &Self::blob_path(container, blob), "")?)
            .header("x-ms-blob-type", "BlockBlob")
            .header("Content-Type", content_type.unwrap_or("application/octet-stream"))
            .body(data);
        for (k, v) in metadata {
            req.set_header(&format!("x-ms-meta-{}", k), v.clone());
        }
        self.send(req)
    }

    pub fn get_blob(&self, container: &str, blob: &str) -> Result<Response> {
        self.send(Request::new("GET", self.azure.url("blob", &Self::blob_path(container, blob), "")?))
    }

    pub fn delete_blob(&self, container: &str, blob: &str) -> Result<Response> {
        self.send(Request::new("DELETE", self.azure.url("blob", &Self::blob_path(container, blob), "")?))
    }

    pub fn create_container(&self, container: &str) -> Result<Response> {
        self.send(Request::new("PUT", self.azure.url("blob", &uri_encode(container, false), "restype=container")?))
    }

    pub fn delete_container(&self, container: &str) -> Result<Response> {
        self.send(Request::new("DELETE", self.azure.url("blob", &uri_encode(container, false), "restype=container")?))
    }

    /// `List Blobs` (all pages).
    pub fn list_blobs(&self, container: &str, prefix: Option<&str>, max_results: Option<i64>) -> Result<Vec<BlobItem>> {
        let mut out = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let mut q = vec!["restype=container".to_string(), "comp=list".to_string()];
            if let Some(p) = prefix {
                q.push(format!("prefix={}", uri_encode(p, false)));
            }
            if let Some(m) = max_results {
                q.push(format!("maxresults={}", m));
            }
            if let Some(m) = &marker {
                q.push(format!("marker={}", uri_encode(m, false)));
            }
            let resp = self.send(Request::new("GET", self.azure.url("blob", &uri_encode(container, false), &q.join("&"))?))?;
            let x = Xml::parse(&resp.text())?;
            for b in x.descendants("Blob") {
                let props = x.child(b, "Properties");
                let p = |n: &str| props.and_then(|pi| x.child_text(pi, n)).unwrap_or_default();
                out.push(BlobItem {
                    name: x.child_text(b, "Name").unwrap_or_default(),
                    size: p("Content-Length").parse().unwrap_or(0),
                    etag: p("Etag"),
                    last_modified: p("Last-Modified"),
                    content_type: p("Content-Type"),
                });
            }
            marker = x.find_text("NextMarker").filter(|m| !m.is_empty());
            if marker.is_none() || max_results.is_some() {
                return Ok(out);
            }
        }
    }

    /// `List Containers`: (name, last modified).
    pub fn list_containers(&self) -> Result<Vec<(String, String)>> {
        let resp = self.send(Request::new("GET", self.azure.url("blob", "", "comp=list")?))?;
        let x = Xml::parse(&resp.text())?;
        Ok(x
            .descendants("Container")
            .into_iter()
            .map(|c| (x.child_text(c, "Name").unwrap_or_default(), x.child(c, "Properties").and_then(|p| x.child_text(p, "Last-Modified")).unwrap_or_default()))
            .collect())
    }

    fn container(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::BLOB_CONTAINER_NAME)
            .or_else(|| self.container.clone())
            .ok_or_else(|| CamelError::illegal_argument("No container name: set it in the uri (azure-storage-blob://account/container) or the CamelAzureStorageBlobContainerName header"))
    }

    fn blob(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::BLOB_NAME)
            .or_else(|| self.blob_name.clone())
            .ok_or_else(|| CamelError::illegal_argument("No blob name: set the blobName option or the CamelAzureStorageBlobBlobName header"))
    }

    fn set_blob_headers(ex: &mut Exchange, resp: &Response) {
        if let Some(v) = resp.header("ETag") {
            ex.set_header(headers::E_TAG, v.to_string());
        }
        if let Some(v) = resp.header("Last-Modified") {
            ex.set_header(headers::LAST_MODIFIED, v.to_string());
        }
        if let Some(v) = resp.header("Content-Type") {
            ex.set_header(headers::CONTENT_TYPE, v.to_string());
        }
        if let Some(v) = resp.header("Content-Length").and_then(|v| v.parse::<i64>().ok()) {
            ex.set_header(headers::BLOB_SIZE, v);
        }
        if let Some(v) = resp.header("Content-MD5") {
            ex.set_header(headers::CONTENT_MD5, v.to_string());
        }
        if let Some(v) = resp.header("x-ms-blob-type") {
            ex.set_header(headers::BLOB_TYPE, v.to_string());
        }
        let meta: Vec<(String, String)> =
            resp.headers.iter().filter_map(|(k, v)| k.to_ascii_lowercase().strip_prefix("x-ms-meta-").map(|m| (m.to_string(), v.clone()))).collect();
        if !meta.is_empty() {
            ex.set_header(headers::METADATA, crate::util::string_map(meta));
        }
    }
}

impl CloudService for BlobService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "listBlobContainers");
        match op.as_str() {
            "listBlobContainers" => {
                let c = self.list_containers()?;
                ex.set_body(Value::Array(c.into_iter().map(|(n, m)| obj([("name", Value::from(n)), ("lastModified", Value::from(m))])).collect()));
            }
            "createBlobContainer" => {
                self.create_container(&self.container(ex)?)?;
            }
            "deleteBlobContainer" => {
                self.delete_container(&self.container(ex)?)?;
            }
            "listBlobs" => {
                let prefix = header_str(ex, headers::PREFIX).or_else(|| self.prefix.clone());
                let blobs = self.list_blobs(&self.container(ex)?, prefix.as_deref(), self.max_results_per_page)?;
                ex.set_body(Value::Array(blobs.iter().map(|b| b.to_value()).collect()));
            }
            "uploadBlockBlob" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let ct = header_str(ex, headers::CONTENT_TYPE);
                let meta: Vec<(String, String)> = match ex.header(headers::METADATA) {
                    Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v.to_string())).collect(),
                    _ => Vec::new(),
                };
                let resp = self.upload_block_blob(&container, &blob, ex.body().to_bytes(), ct.as_deref(), &meta)?;
                if let Some(e) = resp.header("ETag") {
                    ex.set_header(headers::E_TAG, e.to_string());
                }
                if let Some(v) = resp.header("Last-Modified") {
                    ex.set_header(headers::LAST_MODIFIED, v.to_string());
                }
            }
            "getBlob" => {
                let resp = self.get_blob(&self.container(ex)?, &self.blob(ex)?)?;
                Self::set_blob_headers(ex, &resp);
                ex.set_body(Value::Bytes(resp.body));
            }
            "deleteBlob" => {
                self.delete_blob(&self.container(ex)?, &self.blob(ex)?)?;
            }
            other => return Err(unsupported("azure-storage-blob", other)),
        }
        Ok(())
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let container = self.container.clone().ok_or_else(|| CamelError::illegal_argument("The blob consumer requires a container name in the uri"))?;
        let names: Vec<String> = match &self.blob_name {
            Some(b) => vec![b.clone()],
            None => self.list_blobs(&container, self.prefix.as_deref(), self.max_results_per_page)?.into_iter().map(|b| b.name).collect(),
        };
        let total = names.len();
        let mut processed = 0;
        for (i, name) in names.iter().enumerate() {
            let resp = match self.get_blob(&container, name) {
                Ok(r) => r,
                Err(e) if error_code(&e).is_some_and(|c| c == "BlobNotFound" || c == "Http404") => continue,
                Err(e) => return Err(e),
            };
            let mut ex = Exchange::new(ctx.clone());
            Self::set_blob_headers(&mut ex, &resp);
            ex.set_header(headers::BLOB_NAME, name.clone());
            ex.set_header(headers::BLOB_CONTAINER_NAME, container.clone());
            ex.set_body(Value::Bytes(resp.body));
            set_batch(&mut ex, i, total);
            processed += 1;
            if process(logger, processor, &mut ex) && self.delete_after_read {
                if let Err(e) = self.delete_blob(&container, name) {
                    camel_core::camel_warn!(logger, "Error deleting blob {}: {}", name, e);
                }
            }
        }
        Ok(processed)
    }
}
