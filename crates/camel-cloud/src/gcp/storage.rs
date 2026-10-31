//! The `google-storage` component (Camel `camel-google-storage`) over the Cloud Storage JSON API.
//!
//! `google-storage://bucketName?serviceAccountKey=file:/path/key.json&operation=...&objectName=...`
//!
//! Producer: without an operation the body is uploaded as `objectName` /
//! `CamelGoogleCloudStorageObjectName` (content type `CamelGoogleCloudStorageContentType`); the
//! body becomes the object metadata. Operations (`operation` /
//! `CamelGoogleCloudStorageOperation`): `putObject` (upload), `getObject` (body = bytes),
//! `listObjects` (`prefix`), `deleteObject`, `copyObject`
//! (`CamelGoogleCloudStorageDestinationBucketName`, `CamelGoogleCloudStorageDestinationObjectName`),
//! `listBuckets`, `deleteBucket`, `createBucket`, `createDownloadLink` (V4 signed URL
//! `GOOG4-RSA-SHA256` with the service account key, valid for
//! `CamelGoogleCloudStorageDownloadLinkExpirationTime` milliseconds (default 5 minutes, at most
//! 7 days); body = the URL, also in `CamelGoogleCloudStorageDownloadLink`).
//!
//! Consumer: lists the bucket (`prefix`, `maxMessagesPerPoll` default 10, or the single
//! `objectName`), one exchange per object (body = content when `includeBody`, default true) with
//! the `CamelGoogleCloudStorage*` headers; after success, `moveAfterRead` copies to
//! `destinationBucket` and `deleteAfterRead` (default true) deletes the object.
//! `autoCreateBucket` (default true) creates the bucket on start in the key's project
//! (`storageClass` default `STANDARD`, `storageLocation` default `US-EAST1`).

use std::sync::Arc;
use std::time::Duration;

use camel_core::component::scheduled::ScheduledPollConfig;
use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::Processor;
use camel_core::value::Value;

use super::{error_status, GcpConfig};
use crate::endpoint::{operation, process, set_batch, unsupported, CloudService};
use crate::http::{Request, Response, Url};
use crate::util::{header_str, kinds, obj, parse_json, uri_encode, Opts};

/// Header names (Camel `GoogleCloudStorageConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelGoogleCloudStorageOperation";
    pub const BUCKET_NAME: &str = "CamelGoogleCloudStorageBucketName";
    pub const OBJECT_NAME: &str = "CamelGoogleCloudStorageObjectName";
    pub const DESTINATION_BUCKET_NAME: &str = "CamelGoogleCloudStorageDestinationBucketName";
    pub const DESTINATION_OBJECT_NAME: &str = "CamelGoogleCloudStorageDestinationObjectName";
    pub const CONTENT_TYPE: &str = "CamelGoogleCloudStorageContentType";
    pub const CONTENT_LENGTH: &str = "CamelGoogleCloudStorageContentLength";
    pub const ETAG: &str = "CamelGoogleCloudStorageETag";
    pub const CONTENT_MD5: &str = "CamelGoogleCloudStorageContentMd5";
    pub const CREATE_TIME: &str = "CamelGoogleCloudStorageCreateTime";
    pub const UPDATE_TIME: &str = "CamelGoogleCloudStorageUpdateTime";
    pub const GENERATION: &str = "CamelGoogleCloudStorageGeneration";
    pub const PREFIX: &str = "CamelGoogleCloudStoragePrefix";
    pub const DOWNLOAD_LINK_EXPIRATION_TIME: &str = "CamelGoogleCloudStorageDownloadLinkExpirationTime";
    pub const DOWNLOAD_LINK: &str = "CamelGoogleCloudStorageDownloadLink";
}

/// The V4 signing algorithm of Cloud Storage signed URLs.
pub const V4_ALGORITHM: &str = "GOOG4-RSA-SHA256";

/// (canonical query without signature, string to sign) of a V4 signed URL.
pub fn v4_string_to_sign(method: &str, host: &str, path: &str, client_email: &str, secs: i64, expires: i64) -> (String, String) {
    let dt = crate::util::amz_date(secs);
    let scope = format!("{}/auto/storage/goog4_request", &dt[..8]);
    let query = format!(
        "X-Goog-Algorithm={}&X-Goog-Credential={}&X-Goog-Date={}&X-Goog-Expires={}&X-Goog-SignedHeaders=host",
        V4_ALGORITHM,
        uri_encode(&format!("{}/{}", client_email, scope), false),
        dt,
        expires
    );
    let cr = crate::aws::sigv4::canonical_request(method, path, &query, &[("host".to_string(), host.to_string())], &["host".to_string()], "UNSIGNED-PAYLOAD", "s3");
    let sts = format!("{}\n{}\n{}\n{}", V4_ALGORITHM, dt, scope, crate::util::sha256_hex(cr.as_bytes()));
    (query, sts)
}

/// A V4 signed URL for `GET {base}/{bucket}/{object}` valid for `expires` seconds.
pub fn v4_signed_url(key: &super::ServiceAccountKey, base: &str, bucket: &str, object: &str, secs: i64, expires: i64) -> Result<String> {
    let url = Url::parse(base)?;
    let path = format!("{}/{}/{}", url.path.trim_end_matches('/'), uri_encode(bucket, false), uri_encode(object, true));
    let (query, sts) = v4_string_to_sign("GET", &url.host_header(), &path, &key.client_email, secs, expires);
    let sig = crate::util::hex(&key.sign(sts.as_bytes())?);
    Ok(format!("{}://{}{}?{}&X-Goog-Signature={}", url.scheme, url.host_header(), path, query, sig))
}

/// The storage endpoint configuration and client.
pub struct StorageService {
    pub gcp: GcpConfig,
    pub bucket: String,
    pub operation: Option<String>,
    pub object_name: Option<String>,
    pub prefix: Option<String>,
    pub delete_after_read: bool,
    pub move_after_read: bool,
    pub destination_bucket: Option<String>,
    pub auto_create_bucket: bool,
    pub include_body: bool,
    pub include_folders: bool,
    pub storage_class: String,
    pub storage_location: String,
    pub max_messages_per_poll: i64,
    pub scheduler: ScheduledPollConfig,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    if remaining.is_empty() {
        return Err(CamelError::illegal_argument("Bucket name must be specified: google-storage://bucketName"));
    }
    let gcp = GcpConfig::from_opts(o)?;
    let s = StorageService {
        bucket: remaining.trim_matches('/').to_string(),
        operation: o.get("operation"),
        object_name: o.get("objectName"),
        prefix: o.get("prefix"),
        delete_after_read: o.bool("deleteAfterRead", true)?,
        move_after_read: o.bool("moveAfterRead", false)?,
        destination_bucket: o.get("destinationBucket"),
        auto_create_bucket: o.bool("autoCreateBucket", true)?,
        include_body: o.bool("includeBody", true)?,
        include_folders: o.bool("includeFolders", true)?,
        storage_class: o.string("storageClass", "STANDARD"),
        storage_location: o.string("storageLocation", "US-EAST1"),
        max_messages_per_poll: o.i64("maxMessagesPerPoll", 10)?,
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        gcp,
    };
    o.ignore(&["filter", "downloadFileName"]);
    if s.move_after_read && s.destination_bucket.is_none() {
        return Err(CamelError::illegal_argument("moveAfterRead requires the destinationBucket option"));
    }
    Ok(Arc::new(s))
}

fn meta_value(v: &Value, key: &str) -> Value {
    v.get(key).cloned().unwrap_or(Value::Null)
}

impl StorageService {
    fn base(&self) -> String {
        self.gcp.base("https://storage.googleapis.com")
    }

    fn url(&self, path: &str, query: &str) -> Result<Url> {
        let mut u = Url::parse(&format!("{}{}", self.base(), path))?;
        u.query = if query.is_empty() { None } else { Some(query.to_string()) };
        Ok(u)
    }

    fn send(&self, req: Request) -> Result<Response> {
        self.gcp.send(req, kinds::GOOGLE_STORAGE)
    }

    fn object_path(bucket: &str, object: &str) -> String {
        format!("/storage/v1/b/{}/o/{}", uri_encode(bucket, false), uri_encode(object, false))
    }

    /// Upload (simple media upload); returns the object metadata.
    pub fn upload(&self, bucket: &str, object: &str, data: Vec<u8>, content_type: Option<&str>) -> Result<Value> {
        let url = self.url(&format!("/upload/storage/v1/b/{}/o", uri_encode(bucket, false)), &format!("uploadType=media&name={}", uri_encode(object, false)))?;
        let req = Request::new("POST", url).header("Content-Type", content_type.unwrap_or("application/octet-stream")).body(data);
        parse_json(&self.send(req)?.body)
    }

    /// Object metadata.
    pub fn get_metadata(&self, bucket: &str, object: &str) -> Result<Value> {
        parse_json(&self.send(Request::new("GET", self.url(&Self::object_path(bucket, object), "")?))?.body)
    }

    /// Object content.
    pub fn download(&self, bucket: &str, object: &str) -> Result<Vec<u8>> {
        Ok(self.send(Request::new("GET", self.url(&Self::object_path(bucket, object), "alt=media")?))?.body)
    }

    pub fn delete_object(&self, bucket: &str, object: &str) -> Result<()> {
        self.send(Request::new("DELETE", self.url(&Self::object_path(bucket, object), "")?))?;
        Ok(())
    }

    pub fn copy_object(&self, src_bucket: &str, src: &str, dst_bucket: &str, dst: &str) -> Result<Value> {
        let path = format!("{}/copyTo/b/{}/o/{}", Self::object_path(src_bucket, src), uri_encode(dst_bucket, false), uri_encode(dst, false));
        parse_json(&self.send(Request::new("POST", self.url(&path, "")?).header("Content-Type", "application/json").body(&b"{}"[..]))?.body)
    }

    /// List objects (all pages, or at most `max`).
    pub fn list_objects(&self, bucket: &str, prefix: Option<&str>, max: Option<i64>) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut q = Vec::new();
            if let Some(p) = prefix {
                q.push(format!("prefix={}", uri_encode(p, false)));
            }
            if let Some(m) = max {
                q.push(format!("maxResults={}", m));
            }
            if let Some(t) = &token {
                q.push(format!("pageToken={}", uri_encode(t, false)));
            }
            let v = parse_json(&self.send(Request::new("GET", self.url(&format!("/storage/v1/b/{}/o", uri_encode(bucket, false)), &q.join("&"))?))?.body)?;
            if let Some(items) = v.get("items").and_then(|i| i.as_array()) {
                out.extend(items.iter().cloned());
            }
            token = v.get("nextPageToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
            if token.is_none() || max.is_some_and(|m| out.len() as i64 >= m) {
                return Ok(out);
            }
        }
    }

    fn project(&self) -> Result<String> {
        self.gcp.project_id().ok_or_else(|| CamelError::illegal_argument("A project id (from the service account key) is required for bucket operations"))
    }

    pub fn bucket_exists(&self, bucket: &str) -> Result<bool> {
        match self.send(Request::new("GET", self.url(&format!("/storage/v1/b/{}", uri_encode(bucket, false)), "")?)) {
            Ok(_) => Ok(true),
            Err(e) if error_status(&e) == Some(404) => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn create_bucket(&self, bucket: &str) -> Result<Value> {
        let body = obj([("name", Value::from(bucket)), ("storageClass", Value::from(self.storage_class.clone())), ("location", Value::from(self.storage_location.clone()))]);
        let req = Request::new("POST", self.url("/storage/v1/b", &format!("project={}", uri_encode(&self.project()?, false)))?)
            .header("Content-Type", "application/json")
            .body(body.to_json().into_bytes());
        parse_json(&self.send(req)?.body)
    }

    fn set_meta_headers(ex: &mut Exchange, m: &Value) {
        ex.set_header(headers::CONTENT_TYPE, meta_value(m, "contentType"));
        ex.set_header(headers::CONTENT_LENGTH, m.get("size").and_then(|s| s.to_string().parse::<i64>().ok()).unwrap_or(0));
        ex.set_header(headers::ETAG, meta_value(m, "etag"));
        ex.set_header(headers::CONTENT_MD5, meta_value(m, "md5Hash"));
        ex.set_header(headers::CREATE_TIME, meta_value(m, "timeCreated"));
        ex.set_header(headers::UPDATE_TIME, meta_value(m, "updated"));
        ex.set_header(headers::GENERATION, meta_value(m, "generation"));
    }

    fn object(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::OBJECT_NAME)
            .or_else(|| self.object_name.clone())
            .ok_or_else(|| CamelError::illegal_argument("No object name: set the objectName option or the CamelGoogleCloudStorageObjectName header"))
    }

    fn commit(&self, name: &str) -> Result<()> {
        if self.move_after_read {
            if let Some(dst) = &self.destination_bucket {
                self.copy_object(&self.bucket, name, dst, name)?;
            }
        }
        if self.delete_after_read {
            self.delete_object(&self.bucket, name)?;
        }
        Ok(())
    }
}

impl CloudService for StorageService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let bucket = header_str(ex, headers::BUCKET_NAME).unwrap_or_else(|| self.bucket.clone());
        let op = operation(ex, headers::OPERATION, &self.operation, "putObject");
        match op.as_str() {
            "putObject" | "uploadObject" => {
                let name = self.object(ex)?;
                let ct = header_str(ex, headers::CONTENT_TYPE);
                let meta = self.upload(&bucket, &name, ex.body().to_bytes(), ct.as_deref())?;
                Self::set_meta_headers(ex, &meta);
                ex.set_body(meta);
            }
            "getObject" => {
                let name = self.object(ex)?;
                let meta = self.get_metadata(&bucket, &name)?;
                let data = self.download(&bucket, &name)?;
                Self::set_meta_headers(ex, &meta);
                ex.set_body(Value::Bytes(data));
            }
            "listObjects" => {
                let prefix = header_str(ex, headers::PREFIX).or_else(|| self.prefix.clone());
                ex.set_body(Value::Array(self.list_objects(&bucket, prefix.as_deref(), None)?));
            }
            "deleteObject" => {
                self.delete_object(&bucket, &self.object(ex)?)?;
                ex.set_body(true);
            }
            "copyObject" => {
                let name = self.object(ex)?;
                let dst_bucket = header_str(ex, headers::DESTINATION_BUCKET_NAME)
                    .ok_or_else(|| CamelError::illegal_argument("copyObject requires the CamelGoogleCloudStorageDestinationBucketName header"))?;
                let dst = header_str(ex, headers::DESTINATION_OBJECT_NAME).unwrap_or_else(|| name.clone());
                ex.set_body(self.copy_object(&bucket, &name, &dst_bucket, &dst)?);
            }
            "listBuckets" => {
                let v = parse_json(&self.send(Request::new("GET", self.url("/storage/v1/b", &format!("project={}", uri_encode(&self.project()?, false)))?))?.body)?;
                ex.set_body(v.get("items").cloned().unwrap_or(Value::Array(vec![])));
            }
            "createBucket" => {
                ex.set_body(self.create_bucket(&bucket)?);
            }
            "deleteBucket" => {
                self.send(Request::new("DELETE", self.url(&format!("/storage/v1/b/{}", uri_encode(&bucket, false)), "")?))?;
                ex.set_body(true);
            }
            "createDownloadLink" => {
                let name = self.object(ex)?;
                let key = self.gcp.key.as_ref().ok_or_else(|| CamelError::illegal_argument("createDownloadLink requires a service account key (serviceAccountKey)"))?;
                let millis = crate::util::header_i64(ex, headers::DOWNLOAD_LINK_EXPIRATION_TIME).unwrap_or(300_000);
                let secs = (millis / 1000).clamp(1, 7 * 86400);
                let link = v4_signed_url(key, &self.gcp.base("https://storage.googleapis.com"), &bucket, &name, crate::util::now_secs(), secs)?;
                ex.set_header(headers::DOWNLOAD_LINK, link.clone());
                ex.set_body(link);
            }
            other => return Err(unsupported("google-storage", other)),
        }
        Ok(())
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn on_start(&self) -> Result<()> {
        if self.auto_create_bucket && !self.bucket_exists(&self.bucket)? {
            self.create_bucket(&self.bucket)?;
        }
        Ok(())
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let metas: Vec<Value> = match &self.object_name {
            Some(n) => match self.get_metadata(&self.bucket, n) {
                Ok(m) => vec![m],
                Err(e) if error_status(&e) == Some(404) => return Ok(0),
                Err(e) => return Err(e),
            },
            None => self.list_objects(&self.bucket, self.prefix.as_deref(), Some(self.max_messages_per_poll))?,
        };
        let metas: Vec<Value> = metas.into_iter().filter(|m| self.include_folders || !m.get("name").map(|n| n.to_string()).unwrap_or_default().ends_with('/')).collect();
        let total = metas.len();
        let mut processed = 0;
        for (i, m) in metas.iter().enumerate() {
            let name = m.get("name").map(|n| n.to_string()).unwrap_or_default();
            let mut ex = Exchange::new(ctx.clone());
            if self.include_body {
                match self.download(&self.bucket, &name) {
                    Ok(d) => ex.set_body(Value::Bytes(d)),
                    Err(e) if error_status(&e) == Some(404) => continue,
                    Err(e) => return Err(e),
                }
            }
            Self::set_meta_headers(&mut ex, m);
            ex.set_header(headers::OBJECT_NAME, name.clone());
            ex.set_header(headers::BUCKET_NAME, self.bucket.clone());
            set_batch(&mut ex, i, total);
            processed += 1;
            if process(logger, processor, &mut ex) {
                if let Err(e) = self.commit(&name) {
                    camel_core::camel_warn!(logger, "Error committing object {}: {}", name, e);
                }
            }
        }
        Ok(processed)
    }
}
