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
//! Append blobs: `createAppendBlob`, `commitAppendBlob` (appends the body as a block; creates
//! the blob first when missing unless `createAppendBlob=false` / `CamelAzureStorageBlobCreateAppendBlob`).
//! Page blobs: `createPageBlob` (`pageBlobSize` / `CamelAzureStorageBlobPageBlobSize`, default
//! 512), `uploadPageBlob` (body written at `CamelAzureStorageBlobPageBlobRange` = `"start-end"`
//! or `{start, end}`, 512-byte aligned; default from offset 0; creates the blob first unless
//! `createPageBlob=false`), `resizePageBlob`, `clearPageBlob`, `getPageBlobRanges` (body = list
//! of `{start, end}`).
//! Leases (rust-camel operations, as Camel only exposes `leaseBlob`): `acquireBlobLease`
//! (`leaseDurationInSeconds` / `CamelAzureStorageBlobLeaseDuration`, -1 = infinite; sets
//! `CamelAzureStorageBlobLeaseId`), `renewBlobLease`, `releaseBlobLease`, `breakBlobLease`.
//! Write and delete operations send `CamelAzureStorageBlobLeaseId` as `x-ms-lease-id`; with
//! `leaseBlob=true` they acquire a lease for the duration of the operation.
//!
//! Consumer: with `blobName`, polls that blob; otherwise lists the container (`prefix`) and
//! emits one exchange per blob (body = content) with the `CamelAzureStorageBlob*` headers. Blobs
//! are deleted after successful processing when `deleteAfterRead=true` (rust-camel extension,
//! default false like Camel, which re-reads the container on every poll).
//!
//! Change feed ([`super::changefeed`]): operation `getChangeFeed` (body = list of change events,
//! `changeFeedStartTime` / `changeFeedEndTime` options or the `CamelAzureStorageBlobChangeFeedStartTime`
//! / `...EndTime` headers, epoch millis or ISO-8601), and with `changeFeed=true` (rust-camel) a
//! change feed consumer: every poll reads the events after the newest one already delivered
//! (from `changeFeedStartTime`, default: the consumer start) and emits one exchange per event
//! (body = the event map, headers `CamelAzureStorageBlobChangeFeedEventType`,
//! `...ChangeFeedEventTime`, `...ChangeFeedEventId`, `CamelAzureStorageBlobContainerName`,
//! `CamelAzureStorageBlobBlobName`); no container is needed in the uri.

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
    pub const PAGE_BLOB_RANGE: &str = "CamelAzureStorageBlobPageBlobRange";
    pub const PAGE_BLOB_SIZE: &str = "CamelAzureStorageBlobPageBlobSize";
    pub const CREATE_APPEND_BLOB: &str = "CamelAzureStorageBlobCreateAppendBlob";
    pub const CREATE_PAGE_BLOB: &str = "CamelAzureStorageBlobCreatePageBlob";
    pub const COMMITTED_BLOCK_COUNT: &str = "CamelAzureStorageBlobCommittedBlockCount";
    /// rust-camel extensions.
    pub const LEASE_ID: &str = "CamelAzureStorageBlobLeaseId";
    pub const LEASE_DURATION: &str = "CamelAzureStorageBlobLeaseDuration";
    pub const CHANGE_FEED_START_TIME: &str = "CamelAzureStorageBlobChangeFeedStartTime";
    pub const CHANGE_FEED_END_TIME: &str = "CamelAzureStorageBlobChangeFeedEndTime";
    pub const CHANGE_FEED_EVENT_TYPE: &str = "CamelAzureStorageBlobChangeFeedEventType";
    pub const CHANGE_FEED_EVENT_TIME: &str = "CamelAzureStorageBlobChangeFeedEventTime";
    pub const CHANGE_FEED_EVENT_ID: &str = "CamelAzureStorageBlobChangeFeedEventId";
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
    pub create_append_blob: bool,
    pub create_page_blob: bool,
    pub page_blob_size: i64,
    pub lease_blob: bool,
    pub lease_duration: i64,
    pub change_feed: bool,
    pub change_feed_start: Option<i64>,
    pub change_feed_end: Option<i64>,
    pub scheduler: ScheduledPollConfig,
    /// Change feed consumer position: (newest event time delivered, ids delivered at that time).
    feed_position: std::sync::Mutex<(Option<i64>, std::collections::HashSet<String>)>,
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
        create_append_blob: o.bool("createAppendBlob", true)?,
        create_page_blob: o.bool("createPageBlob", true)?,
        page_blob_size: o.i64("pageBlobSize", 512)?,
        lease_blob: o.bool("leaseBlob", false)?,
        lease_duration: o.i64("leaseDurationInSeconds", 60)?,
        change_feed: o.bool("changeFeed", false)?,
        change_feed_start: o.get("changeFeedStartTime").map(|v| super::changefeed::time_value(&Value::from(v.clone())).ok_or_else(|| CamelError::illegal_argument(format!("Invalid changeFeedStartTime: {}", v)))).transpose()?,
        change_feed_end: o.get("changeFeedEndTime").map(|v| super::changefeed::time_value(&Value::from(v.clone())).ok_or_else(|| CamelError::illegal_argument(format!("Invalid changeFeedEndTime: {}", v)))).transpose()?,
        feed_position: std::sync::Mutex::new((None, std::collections::HashSet::new())),
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        azure,
    };
    o.ignore(&["blobType", "closeStreamAfterRead", "closeStreamAfterWrite", "dataCount", "fileDir", "blobOffset", "regex", "downloadLinkExpiration", "commitBlockListLater", "blockListType", "maxRetryRequests"]);
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

    fn blob_url(&self, container: &str, blob: &str, query: &str) -> Result<crate::http::Url> {
        self.azure.url("blob", &Self::blob_path(container, blob), query)
    }

    fn with_lease(mut req: Request, lease: Option<&str>) -> Request {
        if let Some(l) = lease {
            req.set_header("x-ms-lease-id", l);
        }
        req
    }

    /// Create an empty append blob.
    pub fn create_append_blob(&self, container: &str, blob: &str, lease: Option<&str>) -> Result<Response> {
        self.send(Self::with_lease(Request::new("PUT", self.blob_url(container, blob, "")?).header("x-ms-blob-type", "AppendBlob"), lease))
    }

    /// Append a block to an append blob.
    pub fn append_block(&self, container: &str, blob: &str, data: Vec<u8>, lease: Option<&str>) -> Result<Response> {
        self.send(Self::with_lease(Request::new("PUT", self.blob_url(container, blob, "comp=appendblock")?).body(data), lease))
    }

    /// Create a page blob of `size` bytes (a multiple of 512).
    pub fn create_page_blob(&self, container: &str, blob: &str, size: i64, lease: Option<&str>) -> Result<Response> {
        self.send(Self::with_lease(Request::new("PUT", self.blob_url(container, blob, "")?).header("x-ms-blob-type", "PageBlob").header("x-ms-blob-content-length", size.to_string()), lease))
    }

    /// Write (`data` = Some) or clear pages `start..=end`.
    pub fn put_pages(&self, container: &str, blob: &str, start: i64, end: i64, data: Option<Vec<u8>>, lease: Option<&str>) -> Result<Response> {
        let mut req = Request::new("PUT", self.blob_url(container, blob, "comp=page")?).header("x-ms-range", format!("bytes={}-{}", start, end));
        req = match data {
            Some(d) => req.header("x-ms-page-write", "update").body(d),
            None => req.header("x-ms-page-write", "clear"),
        };
        self.send(Self::with_lease(req, lease))
    }

    /// Resize a page blob.
    pub fn resize_page_blob(&self, container: &str, blob: &str, size: i64, lease: Option<&str>) -> Result<Response> {
        self.send(Self::with_lease(Request::new("PUT", self.blob_url(container, blob, "comp=properties")?).header("x-ms-blob-content-length", size.to_string()), lease))
    }

    /// Valid page ranges of a page blob.
    pub fn page_ranges(&self, container: &str, blob: &str) -> Result<Vec<(i64, i64)>> {
        let resp = self.send(Request::new("GET", self.blob_url(container, blob, "comp=pagelist")?))?;
        let x = Xml::parse(&resp.text())?;
        Ok(x.descendants("PageRange")
            .into_iter()
            .map(|r| (x.child_text(r, "Start").and_then(|v| v.trim().parse().ok()).unwrap_or(0), x.child_text(r, "End").and_then(|v| v.trim().parse().ok()).unwrap_or(0)))
            .collect())
    }

    /// Lease actions (`acquire`, `renew`, `release`, `break`); returns the response.
    pub fn lease(&self, container: &str, blob: &str, action: &str, lease_id: Option<&str>, duration: Option<i64>) -> Result<Response> {
        let mut req = Request::new("PUT", self.blob_url(container, blob, "comp=lease")?).header("x-ms-lease-action", action);
        if let Some(id) = lease_id {
            req.set_header("x-ms-lease-id", id);
        }
        if let Some(d) = duration {
            req.set_header("x-ms-lease-duration", d.to_string());
        }
        if action == "break" {
            req.set_header("x-ms-lease-break-period", "0");
        }
        self.send(req)
    }

    /// Run a write operation with the exchange's lease id, or (leaseBlob=true) under a lease
    /// acquired for its duration.
    fn leased<T>(&self, ex: &Exchange, container: &str, blob: &str, may_not_exist: bool, f: impl FnOnce(Option<&str>) -> Result<T>) -> Result<T> {
        if let Some(id) = header_str(ex, headers::LEASE_ID) {
            return f(Some(&id));
        }
        if !self.lease_blob {
            return f(None);
        }
        let acquired = match self.lease(container, blob, "acquire", None, Some(self.lease_duration)) {
            Ok(r) => r.header("x-ms-lease-id").map(|s| s.to_string()),
            Err(e) if may_not_exist && error_code(&e).is_some_and(|c| c == "BlobNotFound") => None,
            Err(e) => return Err(e),
        };
        let res = f(acquired.as_deref());
        if let Some(id) = acquired {
            let _ = self.lease(container, blob, "release", Some(&id), None);
        }
        res
    }

    fn page_range(ex: &Exchange, len: usize) -> Result<(i64, i64)> {
        match ex.header(headers::PAGE_BLOB_RANGE) {
            Some(Value::Object(m)) => {
                let g = |k: &str| m.get(k).and_then(|v| v.as_i64());
                let start = g("start").unwrap_or(0);
                Ok((start, g("end").unwrap_or(start + len as i64 - 1)))
            }
            Some(v) if !v.is_null() => {
                let t = v.to_string();
                let t = t.trim().trim_start_matches("bytes=");
                let (a, b) = t.split_once('-').ok_or_else(|| CamelError::illegal_argument(format!("Invalid page range: {}", t)))?;
                let p = |x: &str| x.trim().parse::<i64>().map_err(|_| CamelError::illegal_argument(format!("Invalid page range: {}", t)));
                Ok((p(a)?, p(b)?))
            }
            _ => Ok((0, len as i64 - 1)),
        }
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

impl BlobService {
    fn poll_change_feed(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let (since, seen) = self.feed_position.lock().unwrap().clone();
        let events = super::changefeed::read(&self.azure, since, self.change_feed_end)?;
        let fresh: Vec<Value> = events.into_iter().filter(|e| !seen.contains(&e.get("id").map(|i| i.to_string()).unwrap_or_default())).collect();
        let total = fresh.len();
        for (i, e) in fresh.into_iter().enumerate() {
            let id = e.get("id").map(|i| i.to_string()).unwrap_or_default();
            let t = e.get("eventTime").and_then(|t| super::changefeed::iso_millis(&t.to_string())).unwrap_or(0);
            let (container, blob) = super::changefeed::subject_parts(&e.get("subject").map(|s| s.to_string()).unwrap_or_default());
            let mut ex = Exchange::new(ctx.clone());
            ex.set_header(headers::CHANGE_FEED_EVENT_TYPE, e.get("eventType").cloned().unwrap_or_default());
            ex.set_header(headers::CHANGE_FEED_EVENT_TIME, e.get("eventTime").cloned().unwrap_or_default());
            ex.set_header(headers::CHANGE_FEED_EVENT_ID, id.clone());
            ex.set_header(headers::BLOB_CONTAINER_NAME, container);
            ex.set_header(headers::BLOB_NAME, blob);
            ex.set_body(e);
            set_batch(&mut ex, i, total);
            process(logger, processor, &mut ex);
            let mut p = self.feed_position.lock().unwrap();
            if p.0.is_none_or(|l| t > l) {
                *p = (Some(t), std::collections::HashSet::new());
            }
            p.1.insert(id);
        }
        Ok(total)
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
                let data = ex.body().to_bytes();
                let resp = self.leased(ex, &container, &blob, true, |lease| {
                    let mut req = Request::new("PUT", self.blob_url(&container, &blob, "")?)
                        .header("x-ms-blob-type", "BlockBlob")
                        .header("Content-Type", ct.as_deref().unwrap_or("application/octet-stream"))
                        .body(data);
                    for (k, v) in &meta {
                        req.set_header(&format!("x-ms-meta-{}", k), v.clone());
                    }
                    self.send(Self::with_lease(req, lease))
                })?;
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
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                self.leased(ex, &container, &blob, false, |lease| self.send(Self::with_lease(Request::new("DELETE", self.blob_url(&container, &blob, "")?), lease)))?;
            }
            "createAppendBlob" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let r = self.leased(ex, &container, &blob, true, |lease| self.create_append_blob(&container, &blob, lease))?;
                Self::set_blob_headers(ex, &r);
            }
            "commitAppendBlob" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let create = ex.header(headers::CREATE_APPEND_BLOB).map(|v| v.is_truthy()).unwrap_or(self.create_append_blob);
                if create && self.send(Request::new("HEAD", self.blob_url(&container, &blob, "")?)).is_err() {
                    self.create_append_blob(&container, &blob, None)?;
                }
                let data = ex.body().to_bytes();
                let r = self.leased(ex, &container, &blob, false, |lease| self.append_block(&container, &blob, data, lease))?;
                if let Some(e) = r.header("ETag") {
                    ex.set_header(headers::E_TAG, e.to_string());
                }
                if let Some(c) = r.header("x-ms-blob-committed-block-count").and_then(|c| c.parse::<i64>().ok()) {
                    ex.set_header(headers::COMMITTED_BLOCK_COUNT, c);
                }
            }
            "createPageBlob" | "resizePageBlob" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let size = ex.header(headers::PAGE_BLOB_SIZE).and_then(|v| v.as_i64()).unwrap_or(self.page_blob_size);
                if size % 512 != 0 {
                    return Err(CamelError::illegal_argument(format!("Page blob size must be a multiple of 512 bytes: {}", size)));
                }
                let r = if op == "createPageBlob" {
                    self.leased(ex, &container, &blob, true, |lease| self.create_page_blob(&container, &blob, size, lease))?
                } else {
                    self.leased(ex, &container, &blob, false, |lease| self.resize_page_blob(&container, &blob, size, lease))?
                };
                if let Some(e) = r.header("ETag") {
                    ex.set_header(headers::E_TAG, e.to_string());
                }
            }
            "uploadPageBlob" | "clearPageBlob" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let data = ex.body().to_bytes();
                let (start, end) = Self::page_range(ex, if op == "uploadPageBlob" { data.len() } else { 512 })?;
                if start % 512 != 0 || (end + 1) % 512 != 0 || end < start {
                    return Err(CamelError::illegal_argument(format!("Page range {}-{} must be aligned to 512-byte pages", start, end)));
                }
                if op == "uploadPageBlob" {
                    if data.len() as i64 != end - start + 1 {
                        return Err(CamelError::illegal_argument(format!("The body ({} bytes) does not match the page range {}-{}", data.len(), start, end)));
                    }
                    let create = ex.header(headers::CREATE_PAGE_BLOB).map(|v| v.is_truthy()).unwrap_or(self.create_page_blob);
                    if create && self.send(Request::new("HEAD", self.blob_url(&container, &blob, "")?)).is_err() {
                        let size = ex.header(headers::PAGE_BLOB_SIZE).and_then(|v| v.as_i64()).unwrap_or(self.page_blob_size).max(end + 1);
                        self.create_page_blob(&container, &blob, (size + 511) / 512 * 512, None)?;
                    }
                }
                let payload = if op == "uploadPageBlob" { Some(data) } else { None };
                let r = self.leased(ex, &container, &blob, false, |lease| self.put_pages(&container, &blob, start, end, payload, lease))?;
                if let Some(e) = r.header("ETag") {
                    ex.set_header(headers::E_TAG, e.to_string());
                }
            }
            "getPageBlobRanges" => {
                let ranges = self.page_ranges(&self.container(ex)?, &self.blob(ex)?)?;
                ex.set_body(Value::Array(ranges.into_iter().map(|(a, b)| obj([("start", Value::Int(a)), ("end", Value::Int(b))])).collect()));
            }
            "acquireBlobLease" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let d = ex.header(headers::LEASE_DURATION).and_then(|v| v.as_i64()).unwrap_or(self.lease_duration);
                let r = self.lease(&container, &blob, "acquire", None, Some(d))?;
                ex.set_header(headers::LEASE_ID, r.header("x-ms-lease-id").unwrap_or("").to_string());
            }
            "renewBlobLease" | "releaseBlobLease" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let id = header_str(ex, headers::LEASE_ID).ok_or_else(|| CamelError::illegal_argument(format!("{} requires the CamelAzureStorageBlobLeaseId header", op)))?;
                self.lease(&container, &blob, if op == "renewBlobLease" { "renew" } else { "release" }, Some(&id), None)?;
            }
            "breakBlobLease" => {
                self.lease(&self.container(ex)?, &self.blob(ex)?, "break", None, None)?;
            }
            "getChangeFeed" => {
                let start = ex.header(headers::CHANGE_FEED_START_TIME).and_then(super::changefeed::time_value).or(self.change_feed_start);
                let end = ex.header(headers::CHANGE_FEED_END_TIME).and_then(super::changefeed::time_value).or(self.change_feed_end);
                ex.set_body(Value::Array(super::changefeed::read(&self.azure, start, end)?));
            }
            other => return Err(unsupported("azure-storage-blob", other)),
        }
        Ok(())
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn on_start(&self) -> Result<()> {
        if self.change_feed {
            let mut p = self.feed_position.lock().unwrap();
            if p.0.is_none() {
                p.0 = Some(self.change_feed_start.unwrap_or_else(crate::util::now_millis));
            }
        }
        Ok(())
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        if self.change_feed {
            return self.poll_change_feed(ctx, processor, logger);
        }
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
