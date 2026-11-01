//! The remaining `azure-storage-blob` producer operations:
//!
//! * Block lists: `stageBlockBlobList` (the body — a list of byte / text blocks, or one value —
//!   is staged with Put Block under generated base64 block ids; body = the block ids, also
//!   `CamelAzureStorageBlobBlockIds`; with `commitBlockListLater=false` /
//!   `CamelAzureStorageBlobCommitBlobBlockListLater=false` they are committed right away),
//!   `commitBlobBlockList` (body = list of block ids → Put Block List with `<Latest>` entries,
//!   content type / metadata headers as for uploadBlockBlob), `getBlobBlockList`
//!   (`blockListType` / `CamelAzureStorageBlobBlockListType` COMMITTED / UNCOMMITTED / ALL; body =
//!   list of `{name, size, committed}`).
//! * `downloadLink`: a read-only service SAS URL for the blob (Shared Key credentials:
//!   signed with the account key; SAS credentials: the URL with the token), valid
//!   `downloadLinkExpiration` / `CamelAzureStorageBlobDownloadLinkExpiration` millis (default
//!   one hour); set as body and `CamelAzureStorageBlobDownloadLink`.
//! * `downloadBlobToFile`: writes the blob to `fileDir` / `CamelAzureStorageBlobFileDir` (file
//!   name = the blob name's last segment, or `CamelAzureStorageBlobFileName`), streamed; body =
//!   the file path, blob headers set.
//! * `copyBlob`: server-side Copy Blob from `CamelAzureStorageBlobSourceBlobContainerName` /
//!   `CamelAzureStorageBlobSourceBlobName` (default: the target blob name) of the same account,
//!   or of `CamelAzureStorageBlobSourceBlobAccountName` with `sourceBlobAccessKey` (the source
//!   URL is authorized with a read SAS); body = the copy id, `CamelAzureStorageBlobCopyId` and
//!   `...CopyStatus` headers.

use std::io::Write;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::blob::{headers as bh, BlobService};
use super::{blob_service_sas, Credential};
use crate::http::Request;
use crate::util::{base64_encode, header_str, iso8601, now_secs, uuid, xml_escape, Xml};

/// Extra header names.
pub mod headers {
    pub const BLOCK_IDS: &str = "CamelAzureStorageBlobBlockIds";
    pub const COMMIT_BLOCK_LIST_LATER: &str = "CamelAzureStorageBlobCommitBlobBlockListLater";
    pub const BLOCK_LIST_TYPE: &str = "CamelAzureStorageBlobBlockListType";
    pub const DOWNLOAD_LINK: &str = "CamelAzureStorageBlobDownloadLink";
    pub const DOWNLOAD_LINK_EXPIRATION: &str = "CamelAzureStorageBlobDownloadLinkExpiration";
    pub const FILE_DIR: &str = "CamelAzureStorageBlobFileDir";
    pub const FILE_NAME: &str = "CamelAzureStorageBlobFileName";
    pub const SOURCE_BLOB_ACCOUNT_NAME: &str = "CamelAzureStorageBlobSourceBlobAccountName";
    pub const SOURCE_BLOB_CONTAINER_NAME: &str = "CamelAzureStorageBlobSourceBlobContainerName";
    pub const SOURCE_BLOB_NAME: &str = "CamelAzureStorageBlobSourceBlobName";
    pub const COPY_ID: &str = "CamelAzureStorageBlobCopyId";
    pub const COPY_STATUS: &str = "CamelAzureStorageBlobCopyStatus";
}

impl BlobService {
    fn put_block_list(&self, container: &str, blob: &str, ids: &[String], ex: &Exchange) -> Result<crate::http::Response> {
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>");
        for id in ids {
            xml.push_str(&format!("<Latest>{}</Latest>", xml_escape(id)));
        }
        xml.push_str("</BlockList>");
        let mut req = Request::new("PUT", self.blob_url(container, blob, "comp=blocklist")?).header("Content-Type", "application/xml").body(xml.into_bytes());
        if let Some(ct) = header_str(ex, bh::CONTENT_TYPE) {
            req.set_header("x-ms-blob-content-type", ct);
        }
        if let Some(Value::Object(m)) = ex.header(bh::METADATA) {
            for (k, v) in m {
                req.set_header(&format!("x-ms-meta-{}", k), v.to_string());
            }
        }
        self.send(req)
    }

    /// A read SAS URL for a blob.
    pub fn download_link(&self, container: &str, blob: &str, millis: i64) -> Result<String> {
        let mut url = self.blob_url(container, blob, "")?;
        let expiry = iso8601(now_secs() + (millis / 1000).max(1));
        url.query = Some(match &self.azure.credential {
            Credential::SharedKey(key) => blob_service_sas(&self.azure.account, key, container, blob, "r", &expiry)?,
            Credential::Sas(token) => token.clone(),
            Credential::Bearer(_) => return Err(CamelError::illegal_argument("downloadLink needs Shared Key or SAS credentials (user delegation SAS is not supported)")),
        });
        Ok(url.to_string())
    }

    /// The operations of this module; false when `op` is not one of them.
    pub fn more_ops(&self, ex: &mut Exchange, op: &str) -> Result<bool> {
        match op {
            "stageBlockBlobList" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let blocks: Vec<Vec<u8>> = match ex.body() {
                    Value::Array(a) => a.iter().map(|b| b.to_bytes()).collect(),
                    Value::Null => Vec::new(),
                    other => vec![other.to_bytes()],
                };
                if blocks.is_empty() {
                    return Err(CamelError::illegal_argument("stageBlockBlobList needs a body (a list of blocks)"));
                }
                let mut ids = Vec::new();
                for data in blocks {
                    let id = base64_encode(uuid().as_bytes());
                    let q = format!("comp=block&blockid={}", crate::util::uri_encode(&id, false));
                    self.send(Request::new("PUT", self.blob_url(&container, &blob, &q)?).body(data))?;
                    ids.push(id);
                }
                let later = ex.header(headers::COMMIT_BLOCK_LIST_LATER).map(|v| v.is_truthy()).unwrap_or(self.commit_block_list_later);
                if !later {
                    let r = self.put_block_list(&container, &blob, &ids, ex)?;
                    if let Some(e) = r.header("ETag") {
                        ex.set_header(bh::E_TAG, e.to_string());
                    }
                }
                ex.set_header(headers::BLOCK_IDS, Value::from(ids.clone()));
                ex.set_body(Value::from(ids));
            }
            "commitBlobBlockList" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let ids = match ex.header(headers::BLOCK_IDS) {
                    Some(v) => crate::util::str_list(v),
                    None => crate::util::str_list(ex.body()),
                };
                if ids.is_empty() {
                    return Err(CamelError::illegal_argument("commitBlobBlockList needs the block ids (list body or CamelAzureStorageBlobBlockIds)"));
                }
                let r = self.put_block_list(&container, &blob, &ids, ex)?;
                if let Some(e) = r.header("ETag") {
                    ex.set_header(bh::E_TAG, e.to_string());
                }
                if let Some(v) = r.header("Last-Modified") {
                    ex.set_header(bh::LAST_MODIFIED, v.to_string());
                }
                ex.set_body(Value::Bool(true));
            }
            "getBlobBlockList" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let kind = header_str(ex, headers::BLOCK_LIST_TYPE).map(|k| k.to_ascii_uppercase()).unwrap_or_else(|| self.block_list_type.clone());
                if !matches!(kind.as_str(), "COMMITTED" | "UNCOMMITTED" | "ALL") {
                    return Err(CamelError::illegal_argument(format!("Invalid block list type: {}", kind)));
                }
                let r = self.send(Request::new("GET", self.blob_url(&container, &blob, &format!("comp=blocklist&blocklisttype={}", kind.to_ascii_lowercase()))?))?;
                let x = Xml::parse(&r.text())?;
                let mut out = Vec::new();
                for (section, committed) in [("CommittedBlocks", true), ("UncommittedBlocks", false)] {
                    for sec in x.descendants(section) {
                        for b in x.children(sec, "Block") {
                            let mut m = ObjectMap::new();
                            m.insert("name".into(), Value::from(x.child_text(b, "Name").unwrap_or_default()));
                            m.insert("size".into(), Value::Int(x.child_text(b, "Size").and_then(|s| s.parse().ok()).unwrap_or(0)));
                            m.insert("committed".into(), Value::Bool(committed));
                            out.push(Value::Object(m));
                        }
                    }
                }
                ex.set_body(Value::Array(out));
            }
            "downloadLink" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let millis = ex.header(headers::DOWNLOAD_LINK_EXPIRATION).and_then(|v| v.as_i64()).or(self.download_link_expiration).unwrap_or(3_600_000);
                let link = self.download_link(&container, &blob, millis)?;
                ex.set_header(headers::DOWNLOAD_LINK, link.clone());
                ex.set_body(link);
            }
            "downloadBlobToFile" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let dir = header_str(ex, headers::FILE_DIR).or_else(|| self.file_dir.clone()).ok_or_else(|| CamelError::illegal_argument("downloadBlobToFile requires the fileDir option or the CamelAzureStorageBlobFileDir header"))?;
                let name = header_str(ex, headers::FILE_NAME).unwrap_or_else(|| blob.rsplit('/').next().unwrap_or(&blob).to_string());
                std::fs::create_dir_all(&dir).map_err(|e| CamelError::io(format!("Cannot create {}: {}", dir, e)))?;
                let path = std::path::Path::new(&dir).join(&name);
                let resp = self.get_blob(&container, &blob)?;
                let mut f = std::fs::File::create(&path).map_err(|e| CamelError::io(format!("Cannot create {}: {}", path.display(), e)))?;
                f.write_all(&resp.body).map_err(|e| CamelError::io(format!("Cannot write {}: {}", path.display(), e)))?;
                Self::set_blob_headers(ex, &resp);
                ex.set_body(path.display().to_string());
            }
            "copyBlob" => {
                let (container, blob) = (self.container(ex)?, self.blob(ex)?);
                let src_container = header_str(ex, headers::SOURCE_BLOB_CONTAINER_NAME).unwrap_or_else(|| container.clone());
                let src_blob = header_str(ex, headers::SOURCE_BLOB_NAME).unwrap_or_else(|| blob.clone());
                let src_url = match header_str(ex, headers::SOURCE_BLOB_ACCOUNT_NAME).filter(|a| *a != self.azure.account) {
                    Some(account) => {
                        let key = self.source_blob_access_key.clone().ok_or_else(|| CamelError::illegal_argument("copyBlob from another account requires the sourceBlobAccessKey option"))?;
                        let src = super::AzureConfig { account: account.clone(), credential: Credential::SharedKey(key.clone()), ..self.azure.clone() };
                        let mut u = src.url("blob", &format!("{}/{}", crate::util::uri_encode(&src_container, false), crate::util::uri_encode(&src_blob, true)), "")?;
                        u.query = Some(blob_service_sas(&account, &key, &src_container, &src_blob, "r", &iso8601(now_secs() + 3600))?);
                        u.to_string()
                    }
                    None => self.download_link(&src_container, &src_blob, 3_600_000).or_else(|_| self.blob_url(&src_container, &src_blob, "").map(|u| u.to_string()))?,
                };
                let r = self.send(Request::new("PUT", self.blob_url(&container, &blob, "")?).header("x-ms-copy-source", src_url))?;
                let id = r.header("x-ms-copy-id").unwrap_or("").to_string();
                ex.set_header(headers::COPY_ID, id.clone());
                ex.set_header(headers::COPY_STATUS, r.header("x-ms-copy-status").unwrap_or("success").to_string());
                if let Some(e) = r.header("ETag") {
                    ex.set_header(bh::E_TAG, e.to_string());
                }
                ex.set_body(id);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
