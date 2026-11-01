//! The remaining `azure-storage-datalake` operations, ACLs, query acceleration and the consumer:
//!
//! * `downloadToFile`: the file written to `fileDir` / `CamelAzureStorageDataLakeFileDir` (name =
//!   the file name's last segment); body = the local path.
//! * `downloadLink`: a read-only service SAS URL of the file (Shared Key: signed with the
//!   account key; SAS credentials: the URL with the token), valid `downloadLinkExpiration` /
//!   `CamelAzureStorageDataLakeDownloadLinkExpiration` millis (default one hour); body and
//!   `CamelAzureStorageDataLakeDownloadLink`.
//! * `uploadFromFile`: uploads the local file given by the body (a path) or
//!   `CamelAzureStorageDataLakeFileDir` + file name (create + append + flush, 4 MiB appends).
//! * `openQueryInputStream`: query acceleration (Query Blob Contents on the blob endpoint) with
//!   the SQL `expression` / `CamelAzureStorageDataLakeExpression` over delimited text
//!   (`CamelAzureStorageDataLakeQueryInputFormat` = `csv` default / `json`,
//!   `CamelAzureStorageDataLakeQueryHasHeaders`); the Avro response stream (resultData /
//!   progress / error / end records) is decoded: body = the result text, query errors raise.
//! * ACLs (rust-camel operation names): `createFile` honours `permission` (`x-ms-permissions`,
//!   symbolic `rwxr-x---` or octal) and `umask`; `getAccessControl` (body = `{owner, group,
//!   permissions, acl}`, `userPrincipalNameReturned`), `setAccessControl` (headers
//!   `CamelAzureStorageDataLakeAcl` (e.g. `user::rwx,group::r-x,other::---`),
//!   `CamelAzureStorageDataLakePermission`, `CamelAzureStorageDataLakeOwner`,
//!   `CamelAzureStorageDataLakeGroup`) on the file or, without file name, the directory.
//! * Consumer: polls the file system (the `fileName` file, or every file under `path` /
//!   `directoryName`, `recursive`, file names matching `regex`) and emits one exchange per file
//!   (body = the bytes, `CamelAzureStorageDataLake*` headers); with the rust-camel
//!   `deleteAfterRead=true` processed files are deleted (Camel re-reads them on every poll).

use std::io::Read;

use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::Processor;
use camel_core::value::{ObjectMap, Value};

use super::datalake::{headers as dh, DataLakeService};
use super::{blob_service_sas, Credential};
use crate::endpoint::{process, set_batch};
use crate::http::Request;
use crate::util::{header_str, iso8601, now_secs, uri_encode, xml_escape};

/// Extra header names.
pub mod headers {
    pub const FILE_DIR: &str = "CamelAzureStorageDataLakeFileDir";
    pub const DOWNLOAD_LINK: &str = "CamelAzureStorageDataLakeDownloadLink";
    pub const DOWNLOAD_LINK_EXPIRATION: &str = "CamelAzureStorageDataLakeDownloadLinkExpiration";
    pub const EXPRESSION: &str = "CamelAzureStorageDataLakeExpression";
    pub const QUERY_INPUT_FORMAT: &str = "CamelAzureStorageDataLakeQueryInputFormat";
    pub const QUERY_HAS_HEADERS: &str = "CamelAzureStorageDataLakeQueryHasHeaders";
    pub const ACL: &str = "CamelAzureStorageDataLakeAcl";
    pub const PERMISSION: &str = "CamelAzureStorageDataLakePermission";
    pub const OWNER: &str = "CamelAzureStorageDataLakeOwner";
    pub const GROUP: &str = "CamelAzureStorageDataLakeGroup";
}

impl DataLakeService {
    fn target(&self, ex: &Exchange) -> Result<String> {
        if let Ok(f) = self.file(ex) {
            return Ok(f);
        }
        header_str(ex, dh::DIRECTORY_NAME).or_else(|| self.directory_name.clone()).ok_or_else(|| CamelError::illegal_argument("A file name or directory name is required"))
    }

    /// A read-only SAS URL of a file.
    pub fn download_link(&self, fs: &str, path: &str, millis: i64) -> Result<String> {
        let mut url = self.path_url(fs, path, "")?;
        let expiry = iso8601(now_secs() + (millis / 1000).max(1));
        url.query = Some(match &self.azure.credential {
            Credential::SharedKey(key) => blob_service_sas(&self.azure.account, key, fs, path.trim_start_matches('/'), "r", &expiry)?,
            Credential::Sas(t) => t.clone(),
            Credential::Bearer(_) => return Err(CamelError::illegal_argument("downloadLink needs Shared Key or SAS credentials")),
        });
        Ok(url.to_string())
    }

    fn query(&self, fs: &str, path: &str, expression: &str, format: &str, has_headers: bool) -> Result<String> {
        let input = if format.eq_ignore_ascii_case("json") {
            "<Format><Type>json</Type><JsonTextConfiguration><RecordSeparator>\n</RecordSeparator></JsonTextConfiguration></Format>".to_string()
        } else {
            format!("<Format><Type>delimited</Type><DelimitedTextConfiguration><ColumnSeparator>,</ColumnSeparator><FieldQuote>\"</FieldQuote><RecordSeparator>\n</RecordSeparator><EscapeChar></EscapeChar><HasHeaders>{}</HasHeaders></DelimitedTextConfiguration></Format>", has_headers)
        };
        let xml = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><QueryRequest><QueryType>SQL</QueryType><Expression>{}</Expression><InputSerialization>{}</InputSerialization></QueryRequest>", xml_escape(expression), input);
        let url = self.azure.url("blob", &format!("{}/{}", uri_encode(fs, false), uri_encode(path.trim_start_matches('/'), true)), "comp=query")?;
        let resp = self.azure.send(Request::new("POST", url).header("Content-Type", "application/xml").body(xml.into_bytes()), crate::util::kinds::DATALAKE_STORAGE)?;
        let c = camel_dataformats::avro::read_container(&resp.body, camel_dataformats::avro::Conv::default())?;
        let mut out = Vec::new();
        for r in c.records {
            if let Some(d) = r.get("data") {
                out.extend_from_slice(&d.to_bytes());
            } else if r.get("fatal").is_some() {
                let fatal = r.get("fatal").is_some_and(|f| f.is_truthy());
                let msg = format!("{}: {} (position {})", r.get("name").map(|v| v.to_string()).unwrap_or_default(), r.get("description").map(|v| v.to_string()).unwrap_or_default(), r.get("position").map(|v| v.to_string()).unwrap_or_default());
                if fatal {
                    return Err(crate::util::cloud_error(crate::util::kinds::DATALAKE_STORAGE, format!("Query failed: {}", msg)));
                }
                camel_core::camel_warn!("azure-storage-datalake", "Query error: {}", msg);
            }
        }
        Ok(String::from_utf8_lossy(&out).to_string())
    }

    /// The operations of this module; false when `op` is not one of them.
    pub fn more_ops(&self, ex: &mut Exchange, op: &str) -> Result<bool> {
        match op {
            "downloadToFile" => {
                let (fs, path) = (self.fs(ex)?, self.file(ex)?);
                let dir = header_str(ex, headers::FILE_DIR).or_else(|| self.file_dir.clone()).ok_or_else(|| CamelError::illegal_argument("downloadToFile requires the fileDir option or the CamelAzureStorageDataLakeFileDir header"))?;
                let r = self.send(Request::new("GET", self.path_url(&fs, &path, "")?))?;
                std::fs::create_dir_all(&dir).map_err(|e| CamelError::io(format!("Cannot create {}: {}", dir, e)))?;
                let local = std::path::Path::new(&dir).join(path.rsplit('/').next().unwrap_or(&path));
                std::fs::write(&local, &r.body).map_err(|e| CamelError::io(format!("Cannot write {}: {}", local.display(), e)))?;
                if let Some(e) = r.header("ETag") {
                    ex.set_header(dh::E_TAG, e.to_string());
                }
                ex.set_header(dh::CONTENT_LENGTH, r.body.len() as i64);
                ex.set_body(local.display().to_string());
            }
            "downloadLink" => {
                let (fs, path) = (self.fs(ex)?, self.file(ex)?);
                let millis = ex.header(headers::DOWNLOAD_LINK_EXPIRATION).and_then(|v| v.as_i64()).or(self.download_link_expiration).unwrap_or(3_600_000);
                let link = self.download_link(&fs, &path, millis)?;
                ex.set_header(headers::DOWNLOAD_LINK, link.clone());
                ex.set_body(link);
            }
            "uploadFromFile" => {
                let (fs, path) = (self.fs(ex)?, self.file(ex)?);
                let local = match header_str(ex, headers::FILE_DIR).or_else(|| self.file_dir.clone()) {
                    Some(d) => std::path::Path::new(&d).join(path.rsplit('/').next().unwrap_or(&path)),
                    None => std::path::PathBuf::from(ex.body().to_string()),
                };
                let mut f = std::fs::File::open(&local).map_err(|e| CamelError::io(format!("Cannot open {}: {}", local.display(), e)))?;
                self.create_file(&fs, &path)?;
                let mut pos = 0i64;
                let mut buf = vec![0u8; 4 * 1024 * 1024];
                loop {
                    let n = f.read(&mut buf).map_err(|e| CamelError::io(format!("Cannot read {}: {}", local.display(), e)))?;
                    if n == 0 {
                        break;
                    }
                    self.append(&fs, &path, pos, buf[..n].to_vec())?;
                    pos += n as i64;
                }
                let r = self.flush(&fs, &path, pos)?;
                if let Some(e) = r.header("ETag") {
                    ex.set_header(dh::E_TAG, e.to_string());
                }
                ex.set_header(dh::CONTENT_LENGTH, pos);
            }
            "openQueryInputStream" | "query" => {
                let (fs, path) = (self.fs(ex)?, self.file(ex)?);
                let expr = header_str(ex, headers::EXPRESSION).or_else(|| self.expression.clone()).ok_or_else(|| CamelError::illegal_argument("Query requires the expression option or the CamelAzureStorageDataLakeExpression header"))?;
                let format = header_str(ex, headers::QUERY_INPUT_FORMAT).unwrap_or_else(|| "csv".into());
                let has_headers = ex.header(headers::QUERY_HAS_HEADERS).map(|v| v.is_truthy()).unwrap_or(false);
                let text = self.query(&fs, &path, &expr, &format, has_headers)?;
                ex.set_body(text);
            }
            "getAccessControl" => {
                let (fs, path) = (self.fs(ex)?, self.target(ex)?);
                let q = format!("action=getAccessControl&upn={}", self.user_principal_name_returned);
                let r = self.send(Request::new("HEAD", self.path_url(&fs, &path, &q)?))?;
                let mut m = ObjectMap::new();
                for (k, h) in [("owner", "x-ms-owner"), ("group", "x-ms-group"), ("permissions", "x-ms-permissions"), ("acl", "x-ms-acl")] {
                    m.insert(k.into(), Value::from(r.header(h).unwrap_or("").to_string()));
                }
                ex.set_body(Value::Object(m));
            }
            "setAccessControl" => {
                let (fs, path) = (self.fs(ex)?, self.target(ex)?);
                let mut req = Request::new("PATCH", self.path_url(&fs, &path, "action=setAccessControl")?);
                let mut any = false;
                for (h, wire) in [(headers::ACL, "x-ms-acl"), (headers::PERMISSION, "x-ms-permissions"), (headers::OWNER, "x-ms-owner"), (headers::GROUP, "x-ms-group")] {
                    if let Some(v) = header_str(ex, h) {
                        req.set_header(wire, v);
                        any = true;
                    }
                }
                if !any {
                    return Err(CamelError::illegal_argument("setAccessControl requires CamelAzureStorageDataLakeAcl, ...Permission, ...Owner or ...Group"));
                }
                let r = self.send(req)?;
                if let Some(e) = r.header("ETag") {
                    ex.set_header(dh::E_TAG, e.to_string());
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The consumer poll.
    pub fn poll_files(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let fs = self.file_system.clone().ok_or_else(|| CamelError::illegal_argument("The Data Lake consumer requires a file system name in the uri"))?;
        let names: Vec<String> = match &self.file_name {
            Some(f) => vec![match &self.directory_name {
                Some(d) => format!("{}/{}", d.trim_end_matches('/'), f),
                None => f.clone(),
            }],
            None => {
                let dir = self.path.clone().or_else(|| self.directory_name.clone());
                let re = self.regex.as_deref().map(camel_core::regex::Regex::new).transpose()?;
                self.list_paths(&fs, dir.as_deref(), self.recursive)?
                    .into_iter()
                    .filter(|p| p.get("isDirectory") != Some(&Value::Bool(true)))
                    .map(|p| p.get("name").map(|n| n.to_string()).unwrap_or_default())
                    .filter(|n| {
                        let base = n.rsplit('/').next().unwrap_or(n);
                        re.as_ref().is_none_or(|r| r.find(base) == Some((0, base.len())))
                    })
                    .collect()
            }
        };
        let total = names.len();
        let mut count = 0;
        for (i, name) in names.iter().enumerate() {
            let r = match self.send(Request::new("GET", self.path_url(&fs, name, "")?)) {
                Ok(r) => r,
                Err(e) if super::error_code(&e).is_some_and(|c| c == "PathNotFound" || c == "Http404") => continue,
                Err(e) => return Err(e),
            };
            let mut ex = Exchange::new(ctx.clone());
            ex.set_header(dh::FILESYSTEM_NAME, fs.clone());
            ex.set_header(dh::FILE_NAME, name.clone());
            if let Some(e) = r.header("ETag") {
                ex.set_header(dh::E_TAG, e.to_string());
            }
            if let Some(v) = r.header("Last-Modified") {
                ex.set_header(dh::LAST_MODIFIED, v.to_string());
            }
            ex.set_header(dh::CONTENT_LENGTH, r.body.len() as i64);
            ex.set_body(Value::Bytes(r.body));
            set_batch(&mut ex, i, total);
            count += 1;
            if process(logger, processor, &mut ex) && self.delete_after_read {
                if let Err(e) = self.send(Request::new("DELETE", self.path_url(&fs, name, "")?)) {
                    camel_core::camel_warn!(logger, "Error deleting {}: {}", name, e);
                }
            }
        }
        Ok(count)
    }
}
