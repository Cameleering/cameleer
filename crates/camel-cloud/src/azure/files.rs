//! The `azure-storage-files` component (rust-camel; Camel's `azure-files` is a file-component
//! flavour, this one mirrors the other storage components) over the Azure Files REST API with
//! Shared Key or SAS authorization.
//!
//! `azure-storage-files://account/share?operation=...&accessKey=RAW(...)`
//!
//! Producer operations (`operation` / `CamelAzureStorageFilesOperation`; directory =
//! `directoryName` / `CamelAzureStorageFilesDirectoryName`, file = `fileName` /
//! `CamelAzureStorageFilesFileName`, the share may be overridden with
//! `CamelAzureStorageFilesShareName`):
//! * `listShares` (`prefix`): body = list of share names.
//! * `createShare`, `deleteShare`.
//! * `listDirectoriesAndFiles` (default; `prefix`): body = list of `{name, type, size}` with
//!   `type` = `file` / `directory`.
//! * `createDirectory`, `deleteDirectory`.
//! * `uploadFile`: creates the file with the body's size and writes it in 4 MiB ranges
//!   (`CamelAzureStorageFilesContentType`); sets `CamelAzureStorageFilesETag`.
//! * `downloadFile`: body = bytes; headers `CamelAzureStorageFilesContentLength`,
//!   `...ContentType`, `...ETag`, `...LastModified`.
//! * `getFileProperties`: headers as for download, body unchanged.
//! * `deleteFile`.
//!
//! Consumer: polls the files of `directoryName` (share root by default), downloading each into
//! an exchange (body = bytes, `CamelAzureStorageFilesFileName`, `CamelFileName`); `delete=true`
//! removes processed files, otherwise files already delivered (same name and ETag) are skipped.
//! `maxMessagesPerPoll` limits the batch.
//!
//! Camel `azure-files` (FTP-like remote file) semantics — recursion, include / exclude,
//! `noop` + idempotent repositories, `move` / `moveFailed` / `preMove` renames, `fileExist`,
//! `tempPrefix`, `autoCreate`, `flatten` and the `renameFile` operation: see
//! [`super::files_ftp`].

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::scheduled::ScheduledPollConfig;
use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::Processor;
use camel_core::value::{ObjectMap, Value};

use super::AzureConfig;
use crate::endpoint::{operation, process, set_batch, unsupported, CloudService};
use crate::http::{Request, Response};
use crate::util::{header_str, kinds, uri_encode, Opts, Xml};

/// Header names.
pub mod headers {
    pub const OPERATION: &str = "CamelAzureStorageFilesOperation";
    pub const SHARE_NAME: &str = "CamelAzureStorageFilesShareName";
    pub const DIRECTORY_NAME: &str = "CamelAzureStorageFilesDirectoryName";
    pub const FILE_NAME: &str = "CamelAzureStorageFilesFileName";
    pub const PREFIX: &str = "CamelAzureStorageFilesPrefix";
    pub const CONTENT_TYPE: &str = "CamelAzureStorageFilesContentType";
    pub const CONTENT_LENGTH: &str = "CamelAzureStorageFilesContentLength";
    pub const ETAG: &str = "CamelAzureStorageFilesETag";
    pub const LAST_MODIFIED: &str = "CamelAzureStorageFilesLastModified";
}

/// Maximum size of one `Put Range` request.
pub const MAX_RANGE: usize = 4 * 1024 * 1024;

/// The Azure Files endpoint configuration and client.
pub struct FilesService {
    pub azure: AzureConfig,
    pub share: String,
    pub operation: Option<String>,
    pub directory: Option<String>,
    pub file_name: Option<String>,
    pub prefix: Option<String>,
    pub delete: bool,
    pub max_messages_per_poll: i64,
    pub scheduler: ScheduledPollConfig,
    seen: Mutex<HashSet<(String, String)>>,
    pub ftp: super::files_ftp::FtpOptions,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let (account, share) = match remaining.split_once('/') {
        Some((a, s)) => (a, s.trim_matches('/')),
        None => (remaining, ""),
    };
    let azure = AzureConfig::from_opts(account, o)?;
    o.ignore(&["shareClient"]);
    Ok(Arc::new(FilesService {
        azure,
        share: share.to_string(),
        operation: o.get("operation"),
        directory: o.get("directoryName"),
        file_name: o.get("fileName"),
        prefix: o.get("prefix"),
        delete: o.bool("delete", false)?,
        max_messages_per_poll: o.i64("maxMessagesPerPoll", 10)?,
        ftp: super::files_ftp::FtpOptions::from_opts(o)?,
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        seen: Mutex::new(HashSet::new()),
    }))
}

pub(crate) fn join(dir: Option<&str>, name: &str) -> String {
    match dir.map(|d| d.trim_matches('/')).filter(|d| !d.is_empty()) {
        Some(d) => format!("{}/{}", d, name.trim_start_matches('/')),
        None => name.trim_start_matches('/').to_string(),
    }
}

impl FilesService {
    pub(crate) fn share_of(&self, ex: Option<&Exchange>) -> Result<String> {
        ex.and_then(|e| header_str(e, headers::SHARE_NAME))
            .or_else(|| Some(self.share.clone()).filter(|s| !s.is_empty()))
            .ok_or_else(|| CamelError::illegal_argument("A share is required: azure-storage-files://account/share or CamelAzureStorageFilesShareName"))
    }

    pub(crate) fn send(&self, method: &str, path: &str, query: &str, headers: &[(&str, String)], body: Vec<u8>) -> Result<Response> {
        let mut req = Request::new(method, self.azure.url("file", &uri_encode(path, true), query)?);
        for (k, v) in headers {
            req.set_header(k, v.clone());
        }
        self.azure.send(req.body(body), kinds::SHARE_STORAGE)
    }

    /// Entries (`{name, type, size}`) of a directory.
    pub fn list(&self, share: &str, dir: Option<&str>, prefix: Option<&str>) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut marker = String::new();
        loop {
            let mut q = "restype=directory&comp=list".to_string();
            if let Some(p) = prefix {
                q.push_str(&format!("&prefix={}", uri_encode(p, false)));
            }
            if !marker.is_empty() {
                q.push_str(&format!("&marker={}", uri_encode(&marker, false)));
            }
            let resp = self.send("GET", &join(Some(share), dir.unwrap_or("")), &q, &[], Vec::new())?;
            let x = Xml::parse(&resp.text())?;
            for kind in ["Directory", "File"] {
                for e in x.descendants(kind) {
                    let size = x.child(e, "Properties").and_then(|p| x.child_text(p, "Content-Length")).and_then(|s| s.parse::<i64>().ok());
                    let mut m = ObjectMap::new();
                    m.insert("name".into(), Value::from(x.child_text(e, "Name").unwrap_or_default()));
                    m.insert("type".into(), Value::from(kind.to_ascii_lowercase()));
                    if let Some(s) = size {
                        m.insert("size".into(), Value::Int(s));
                    }
                    out.push(Value::Object(m));
                }
            }
            marker = x.find_text("NextMarker").unwrap_or_default();
            if marker.is_empty() {
                return Ok(out);
            }
        }
    }

    /// Create a file and write `data` in ranges; returns the final ETag.
    pub fn upload(&self, share: &str, path: &str, data: &[u8], content_type: Option<String>) -> Result<String> {
        let full = join(Some(share), path);
        let mut h = vec![("x-ms-type", "file".to_string()), ("x-ms-content-length", data.len().to_string())];
        if let Some(ct) = content_type {
            h.push(("x-ms-content-type", ct));
        }
        let mut etag = self.send("PUT", &full, "", &h, Vec::new())?.header("ETag").unwrap_or("").to_string();
        for (i, chunk) in data.chunks(MAX_RANGE).enumerate() {
            let start = i * MAX_RANGE;
            let r = self.send("PUT", &full, "comp=range", &[("x-ms-range", format!("bytes={}-{}", start, start + chunk.len() - 1)), ("x-ms-write", "update".into())], chunk.to_vec())?;
            etag = r.header("ETag").unwrap_or(&etag).to_string();
        }
        Ok(etag)
    }

    pub(crate) fn file_headers(ex: &mut Exchange, r: &Response) {
        if let Some(l) = r.header("Content-Length").and_then(|l| l.parse::<i64>().ok()) {
            ex.set_header(headers::CONTENT_LENGTH, l);
        }
        for (h, name) in [("Content-Type", headers::CONTENT_TYPE), ("ETag", headers::ETAG), ("Last-Modified", headers::LAST_MODIFIED)] {
            if let Some(v) = r.header(h) {
                ex.set_header(name, v.to_string());
            }
        }
    }

    pub(crate) fn file_path(&self, ex: &Exchange) -> Result<String> {
        let name = header_str(ex, headers::FILE_NAME)
            .or_else(|| self.file_name.clone())
            .or_else(|| header_str(ex, "CamelFileName"))
            .ok_or_else(|| CamelError::illegal_argument("A file name is required (fileName option or CamelAzureStorageFilesFileName header)"))?;
        Ok(join(header_str(ex, headers::DIRECTORY_NAME).or_else(|| self.directory.clone()).as_deref(), &name))
    }

    fn dir_path(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::DIRECTORY_NAME).or_else(|| self.directory.clone()).ok_or_else(|| CamelError::illegal_argument("A directory is required (directoryName option or CamelAzureStorageFilesDirectoryName header)"))
    }
}

impl CloudService for FilesService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "listDirectoriesAndFiles");
        let prefix = header_str(ex, headers::PREFIX).or_else(|| self.prefix.clone());
        match op.as_str() {
            "listShares" => {
                let mut q = "comp=list".to_string();
                if let Some(p) = &prefix {
                    q.push_str(&format!("&prefix={}", uri_encode(p, false)));
                }
                let resp = self.send("GET", "", &q, &[], Vec::new())?;
                let x = Xml::parse(&resp.text())?;
                let names: Vec<Value> = x.descendants("Share").into_iter().map(|s| Value::from(x.child_text(s, "Name").unwrap_or_default())).collect();
                ex.set_body(Value::Array(names));
            }
            "createShare" => {
                self.send("PUT", &self.share_of(Some(ex))?, "restype=share", &[], Vec::new())?;
            }
            "deleteShare" => {
                self.send("DELETE", &self.share_of(Some(ex))?, "restype=share", &[], Vec::new())?;
            }
            "listDirectoriesAndFiles" => {
                let share = self.share_of(Some(ex))?;
                let dir = header_str(ex, headers::DIRECTORY_NAME).or_else(|| self.directory.clone());
                ex.set_body(Value::Array(self.list(&share, dir.as_deref(), prefix.as_deref())?));
            }
            "createDirectory" => {
                let p = join(Some(&self.share_of(Some(ex))?), &self.dir_path(ex)?);
                self.send("PUT", &p, "restype=directory", &[], Vec::new())?;
            }
            "deleteDirectory" => {
                let p = join(Some(&self.share_of(Some(ex))?), &self.dir_path(ex)?);
                self.send("DELETE", &p, "restype=directory", &[], Vec::new())?;
            }
            "uploadFile" => {
                self.upload_ftp(ex)?;
            }
            "renameFile" => {
                self.rename_op(ex)?;
            }
            "downloadFile" => {
                let p = join(Some(&self.share_of(Some(ex))?), &self.file_path(ex)?);
                let r = self.send("GET", &p, "", &[], Vec::new())?;
                Self::file_headers(ex, &r);
                ex.set_body(Value::Bytes(r.body));
            }
            "getFileProperties" => {
                let p = join(Some(&self.share_of(Some(ex))?), &self.file_path(ex)?);
                let r = self.send("HEAD", &p, "", &[], Vec::new())?;
                Self::file_headers(ex, &r);
            }
            "deleteFile" => {
                let p = join(Some(&self.share_of(Some(ex))?), &self.file_path(ex)?);
                self.send("DELETE", &p, "", &[], Vec::new())?;
            }
            other => return Err(unsupported("Azure Files", other)),
        }
        Ok(())
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        if self.ftp.enabled() {
            return self.poll_ftp(ctx, processor, logger);
        }
        let share = self.share_of(None)?;
        let dir = self.directory.clone();
        let files: Vec<String> = self
            .list(&share, dir.as_deref(), self.prefix.as_deref())?
            .into_iter()
            .filter(|e| e.get("type").map(|t| t.to_string()).as_deref() == Some("file"))
            .map(|e| e.get("name").map(|n| n.to_string()).unwrap_or_default())
            .collect();
        let mut batch = Vec::new();
        for name in files {
            if self.max_messages_per_poll > 0 && batch.len() as i64 >= self.max_messages_per_poll {
                break;
            }
            let path = join(Some(&share), &join(dir.as_deref(), &name));
            let r = self.send("GET", &path, "", &[], Vec::new())?;
            let etag = r.header("ETag").unwrap_or("").to_string();
            if !self.delete && self.seen.lock().unwrap().contains(&(name.clone(), etag.clone())) {
                continue;
            }
            batch.push((name, path, etag, r));
        }
        let total = batch.len();
        for (i, (name, path, etag, r)) in batch.into_iter().enumerate() {
            let mut ex = Exchange::new(ctx.clone());
            Self::file_headers(&mut ex, &r);
            ex.set_header(headers::FILE_NAME, name.clone());
            ex.set_header(headers::SHARE_NAME, share.clone());
            if let Some(d) = &dir {
                ex.set_header(headers::DIRECTORY_NAME, d.clone());
            }
            ex.set_header("CamelFileName", name.clone());
            ex.set_body(Value::Bytes(r.body));
            set_batch(&mut ex, i, total);
            if process(logger, processor, &mut ex) {
                if self.delete {
                    if let Err(e) = self.send("DELETE", &path, "", &[], Vec::new()) {
                        camel_core::camel_warn!(logger, "Error deleting Azure file {}: {}", path, e);
                    }
                } else {
                    self.seen.lock().unwrap().insert((name, etag));
                }
            }
        }
        Ok(total)
    }
}
