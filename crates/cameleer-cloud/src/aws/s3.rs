//! The `aws2-s3` component (Camel `camel-aws2-s3`) over the S3 REST API.
//!
//! `aws2-s3://bucketNameOrArn?operation=...&...`
//!
//! Producer operations (`operation` option or `CamelAwsS3Operation` header; default
//! `putObject`): `putObject`, `getObject`, `getObjectRange`, `headObject`, `listObjects`,
//! `deleteObject`, `copyObject`, `listBuckets`, `createBucket`, `deleteBucket`,
//! `headBucket`, `createDownloadLink` (presigned GET URL), plus `deleteObjects`,
//! `createUploadLink`, `restoreObject`, tagging, ACL, object lock and versioning operations
//! and SSE-KMS / SSE-S3 / SSE-C encryption (see [`super::s3_ops`]). `putObject` honours
//! `CamelAwsS3CannedAcl`, `...ServerSideEncryption`, `...ContentDisposition`,
//! `...ContentEncoding`, `...ContentControl`, `...ObjectTags`, `...Headers` and the object lock
//! headers; `conditionalWritesEnabled=true` sends `If-None-Match: *` (no overwrite).
//!
//! Consumer: polls the bucket (`ListObjectsV2`, `prefix`, `delimiter`, `maxMessagesPerPoll`
//! default 10, or a single object with `fileName`), one exchange per object with the body
//! (`includeBody`, default true) and the `CamelAwsS3*` headers; after successful processing
//! the object is copied to `destinationBucket` (`moveAfterRead`, with
//! `destinationBucketPrefix`/`destinationBucketSuffix`) and deleted (`deleteAfterRead`,
//! default true). Scheduled-poll options apply (`delay` default 500 ms). `ignoreBody=true`
//! does not download the objects at all (HEAD only, empty body); Cameleer `localDirectory`
//! streams each object into `{localDirectory}/{key}` without buffering it (body = the path).
//!
//! Other options: `keyName`, `prefix`, `delimiter`, `autoCreateBucket` (default false),
//! `forcePathStyle` (virtual-host style is used otherwise, except for overridden endpoints),
//! `includeFolders` (default true), `storageClass`, plus the [common AWS options](super).
//!
//! Large objects: `multiPartUpload=true` uploads bodies larger than `partSize` (default 25 MiB)
//! with a multipart upload (aborted on failure). Cameleer streaming extensions: the
//! `CamelAwsS3SourceFile` header (a local path) uploads that file without loading it in
//! memory (a streamed `UNSIGNED-PAYLOAD` PUT, or a multipart upload reading one part at a time),
//! and `getObject` with the `CamelAwsS3DownloadFile` header streams the object into that file
//! (body = the path).
//!
//! `streamingUploadMode=true` aggregates the bodies of many exchanges into multipart objects
//! named after `keyName` (`namingStrategy` `progressive` (default: `key`, `key-1`, ...) or
//! `random`): a part is uploaded every `partSize` bytes and the object is completed after
//! `batchMessageNumber` (default 10) messages, `batchSize` (default 1 000 000) bytes,
//! `streamingUploadTimeout` (ms) or when the producer stops. `restartingPolicy=lastPart`
//! continues after the highest existing progressive index (default `override`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::component::scheduled::ScheduledPollConfig;
use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::Processor;
use cameleer_core::value::Value;

use super::s3_multipart::{S3Client, StreamingConfig, StreamingUploader, DEFAULT_PART_SIZE};
use super::s3_ops::{exchange_upload_headers, Sse};
use super::{error_code, AwsConfig};
use crate::endpoint::{operation, process, set_batch, unsupported, CloudService};
use crate::http::{Request, Response, Url};
use crate::util::{header_str, kinds, now_secs, uri_encode, xml_escape, Opts, Xml};


/// Header names (Camel `AWS2S3Constants`).
pub mod headers {
    pub const KEY: &str = "CamelAwsS3Key";
    pub const BUCKET_NAME: &str = "CamelAwsS3BucketName";
    pub const E_TAG: &str = "CamelAwsS3ETag";
    pub const CONTENT_LENGTH: &str = "CamelAwsS3ContentLength";
    pub const CONTENT_TYPE: &str = "CamelAwsS3ContentType";
    pub const LAST_MODIFIED: &str = "CamelAwsS3LastModified";
    pub const VERSION_ID: &str = "CamelAwsS3VersionId";
    pub const OPERATION: &str = "CamelAwsS3Operation";
    pub const BUCKET_DESTINATION_NAME: &str = "CamelAwsS3BucketDestinationName";
    pub const DESTINATION_KEY: &str = "CamelAwsS3DestinationKey";
    pub const PREFIX: &str = "CamelAwsS3Prefix";
    pub const DELIMITER: &str = "CamelAwsS3Delimiter";
    pub const STORAGE_CLASS: &str = "CamelAwsS3StorageClass";
    pub const RANGE_START: &str = "CamelAwsS3RangeStart";
    pub const RANGE_END: &str = "CamelAwsS3RangeEnd";
    pub const DOWNLOAD_LINK_EXPIRATION_TIME: &str = "CamelAwsS3DownloadLinkExpirationTime";
    /// User metadata (`x-amz-meta-*`) as a map.
    pub const METADATA: &str = "CamelAwsS3Metadata";
    /// Cameleer extension: upload this local file (streamed).
    pub const SOURCE_FILE: &str = "CamelAwsS3SourceFile";
    /// Cameleer extension: `getObject` streams the object into this local file.
    pub const DOWNLOAD_FILE: &str = "CamelAwsS3DownloadFile";
}

/// An object entry of a listing.
#[derive(Clone, Debug, PartialEq)]
pub struct S3Object {
    pub key: String,
    pub size: i64,
    pub etag: String,
    pub last_modified: String,
    pub storage_class: String,
}

impl S3Object {
    pub fn to_value(&self) -> Value {
        crate::util::obj([
            ("key", Value::from(self.key.clone())),
            ("size", Value::Int(self.size)),
            ("eTag", Value::from(self.etag.clone())),
            ("lastModified", Value::from(self.last_modified.clone())),
            ("storageClass", Value::from(self.storage_class.clone())),
        ])
    }
}

/// One page of `ListObjectsV2`.
#[derive(Clone, Debug, Default)]
pub struct ListResult {
    pub objects: Vec<S3Object>,
    pub common_prefixes: Vec<String>,
    pub truncated: bool,
    pub next_token: Option<String>,
}

/// Parse a `ListBucketResult` document (V1 or V2).
pub fn parse_list_result(xml: &str) -> Result<ListResult> {
    let x = Xml::parse(xml)?;
    let root = x.root();
    let objects = x
        .children(root, "Contents")
        .into_iter()
        .map(|c| S3Object {
            key: x.child_text(c, "Key").unwrap_or_default(),
            size: x.child_text(c, "Size").and_then(|s| s.trim().parse().ok()).unwrap_or(0),
            etag: x.child_text(c, "ETag").unwrap_or_default(),
            last_modified: x.child_text(c, "LastModified").unwrap_or_default(),
            storage_class: x.child_text(c, "StorageClass").unwrap_or_default(),
        })
        .collect();
    let common_prefixes = x.children(root, "CommonPrefixes").into_iter().filter_map(|c| x.child_text(c, "Prefix")).collect();
    Ok(ListResult {
        objects,
        common_prefixes,
        truncated: x.child_text(root, "IsTruncated").is_some_and(|t| t.trim() == "true"),
        next_token: x.child_text(root, "NextContinuationToken"),
    })
}

/// The S3 endpoint configuration and client.
pub struct S3Service {
    pub aws: AwsConfig,
    pub bucket: String,
    pub operation: Option<String>,
    pub key_name: Option<String>,
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    pub delete_after_read: bool,
    pub move_after_read: bool,
    pub destination_bucket: Option<String>,
    pub destination_bucket_prefix: String,
    pub destination_bucket_suffix: String,
    pub auto_create_bucket: bool,
    pub include_body: bool,
    pub include_folders: bool,
    pub force_path_style: bool,
    pub max_messages_per_poll: i64,
    pub file_name: Option<String>,
    pub storage_class: Option<String>,
    pub multi_part_upload: bool,
    pub part_size: i64,
    pub sse: Sse,
    pub ignore_body: bool,
    pub conditional_writes: bool,
    pub local_directory: Option<String>,
    pub scheduler: ScheduledPollConfig,
    marker: Mutex<Option<String>>,
    streaming: Option<Arc<StreamingUploader>>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    Ok(Arc::new(S3Service::from_opts(remaining, o)?))
}

impl S3Service {
    pub fn from_opts(remaining: &str, o: &mut Opts) -> Result<S3Service> {
        let bucket = remaining.rsplit(":::").next().unwrap_or(remaining).trim_matches('/').to_string();
        if bucket.is_empty() {
            return Err(CamelError::illegal_argument("Bucket name must be specified: aws2-s3://bucketName"));
        }
        let aws = AwsConfig::from_opts(o)?;
        let s = S3Service {
            bucket,
            operation: o.get("operation"),
            key_name: o.get("keyName"),
            prefix: o.get("prefix"),
            delimiter: o.get("delimiter"),
            delete_after_read: o.bool("deleteAfterRead", true)?,
            move_after_read: o.bool("moveAfterRead", false)?,
            destination_bucket: o.get("destinationBucket"),
            destination_bucket_prefix: o.string("destinationBucketPrefix", ""),
            destination_bucket_suffix: o.string("destinationBucketSuffix", ""),
            auto_create_bucket: o.bool("autoCreateBucket", false)?,
            include_body: o.bool("includeBody", true)?,
            include_folders: o.bool("includeFolders", true)?,
            force_path_style: o.bool("forcePathStyle", false)?,
            max_messages_per_poll: o.i64("maxMessagesPerPoll", 10)?,
            file_name: o.get("fileName"),
            storage_class: o.get("storageClass"),
            multi_part_upload: o.bool("multiPartUpload", false)?,
            part_size: o.i64("partSize", DEFAULT_PART_SIZE)?,
            sse: Sse::from_opts(o)?,
            ignore_body: o.bool("ignoreBody", false)?,
            conditional_writes: o.bool("conditionalWritesEnabled", false)?,
            local_directory: o.get("localDirectory"),
            scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
            marker: Mutex::new(None),
            streaming: None,
            aws,
        };
        let mut s = s;
        if s.part_size <= 0 {
            return Err(CamelError::illegal_argument("partSize must be positive"));
        }
        if o.bool("streamingUploadMode", false)? {
            let key_name = s.key_name.clone().ok_or_else(|| CamelError::illegal_argument("streamingUploadMode requires the keyName option"))?;
            let timeout = o.get("streamingUploadTimeout").map(|t| t.parse::<u64>().map(Duration::from_millis).map_err(|_| CamelError::illegal_argument(format!("Invalid streamingUploadTimeout: {}", t)))).transpose()?;
            let cfg = StreamingConfig {
                bucket: s.bucket.clone(),
                key_name,
                part_size: s.part_size as usize,
                batch_message_number: o.i64("batchMessageNumber", 10)?,
                batch_size: o.i64("batchSize", 1_000_000)?,
                timeout,
                naming_strategy: o.string("namingStrategy", "progressive"),
                restarting_policy: o.string("restartingPolicy", "override"),
            };
            s.streaming = Some(StreamingUploader::new(s.client(), cfg));
        } else {
            o.ignore(&["batchMessageNumber", "batchSize", "streamingUploadTimeout", "namingStrategy", "restartingPolicy"]);
        }
        o.ignore(&["pojoRequest", "autocloseBody", "doneFileName", "removePrefixOnMove"]);
        if s.move_after_read && s.destination_bucket.is_none() {
            return Err(CamelError::illegal_argument("moveAfterRead requires the destinationBucket option"));
        }
        Ok(s)
    }

    /// The addressing / transport part of this endpoint.
    pub fn client(&self) -> S3Client {
        S3Client { aws: self.aws.clone(), force_path_style: self.force_path_style, part_headers: self.sse.customer_headers() }
    }

    /// URL of a bucket or object; `query` is an already encoded query string.
    pub fn url(&self, bucket: &str, key: Option<&str>, query: Option<String>) -> Result<Url> {
        self.client().url(bucket, key, query)
    }

    fn send(&self, req: Request) -> Result<Response> {
        self.aws.send(req, "s3", kinds::S3)
    }

    /// PUT an object; returns the response (ETag, version id headers).
    pub fn put_object(&self, bucket: &str, key: &str, body: Vec<u8>, content_type: Option<&str>, extra: &[(String, String)]) -> Result<Response> {
        let mut req = Request::new("PUT", self.url(bucket, Some(key), None)?).body(body);
        req.set_header("Content-Type", content_type.unwrap_or("application/octet-stream"));
        for (k, v) in extra {
            req.set_header(k, v.clone());
        }
        self.send(req)
    }

    /// GET an object (optionally a byte range `start-end`).
    pub fn get_object(&self, bucket: &str, key: &str, range: Option<(i64, i64)>) -> Result<Response> {
        self.get_object_version(bucket, key, range, None)
    }

    /// GET an object version (SSE-C key headers added when configured).
    pub fn get_object_version(&self, bucket: &str, key: &str, range: Option<(i64, i64)>, version: Option<&str>) -> Result<Response> {
        let mut req = Request::new("GET", self.url(bucket, Some(key), version.map(|v| format!("versionId={}", uri_encode(v, false))))?);
        if let Some((a, b)) = range {
            req.set_header("Range", format!("bytes={}-{}", a, b));
        }
        for (k, v) in self.sse.customer_headers() {
            req.set_header(&k, v);
        }
        self.send(req)
    }

    pub fn head_object(&self, bucket: &str, key: &str) -> Result<Response> {
        self.head_object_version(bucket, key, None)
    }

    fn head_object_version(&self, bucket: &str, key: &str, version: Option<&str>) -> Result<Response> {
        let mut req = Request::new("HEAD", self.url(bucket, Some(key), version.map(|v| format!("versionId={}", uri_encode(v, false))))?);
        for (k, v) in self.sse.customer_headers() {
            req.set_header(&k, v);
        }
        self.send(req)
    }

    pub fn delete_object(&self, bucket: &str, key: &str) -> Result<Response> {
        self.send(Request::new("DELETE", self.url(bucket, Some(key), None)?))
    }

    /// Server-side copy; returns the new ETag.
    pub fn copy_object(&self, src_bucket: &str, src_key: &str, dst_bucket: &str, dst_key: &str) -> Result<String> {
        let req = Request::new("PUT", self.url(dst_bucket, Some(dst_key), None)?)
            .header("x-amz-copy-source", format!("/{}/{}", uri_encode(src_bucket, false), uri_encode(src_key, true)));
        let resp = self.send(req)?;
        let x = Xml::parse(&resp.text())?;
        Ok(x.find_text("ETag").unwrap_or_default())
    }

    /// One page of `ListObjectsV2`.
    pub fn list_objects(&self, bucket: &str, prefix: Option<&str>, delimiter: Option<&str>, max_keys: Option<i64>, start_after: Option<&str>) -> Result<ListResult> {
        let mut q = vec!["list-type=2".to_string()];
        if let Some(d) = delimiter {
            q.push(format!("delimiter={}", uri_encode(d, false)));
        }
        if let Some(m) = max_keys.filter(|m| *m > 0) {
            q.push(format!("max-keys={}", m));
        }
        if let Some(p) = prefix {
            q.push(format!("prefix={}", uri_encode(p, false)));
        }
        if let Some(s) = start_after {
            q.push(format!("start-after={}", uri_encode(s, false)));
        }
        let resp = self.send(Request::new("GET", self.url(bucket, None, Some(q.join("&")))?))?;
        parse_list_result(&resp.text())
    }

    /// `ListBuckets`: (name, creation date) pairs.
    pub fn list_buckets(&self) -> Result<Vec<(String, String)>> {
        let resp = self.send(Request::new("GET", self.url("", None, None)?))?;
        let x = Xml::parse(&resp.text())?;
        Ok(x.descendants("Bucket").into_iter().map(|b| (x.child_text(b, "Name").unwrap_or_default(), x.child_text(b, "CreationDate").unwrap_or_default())).collect())
    }

    pub fn create_bucket(&self, bucket: &str) -> Result<Response> {
        let mut req = Request::new("PUT", self.url(bucket, None, None)?);
        if self.aws.region != "us-east-1" {
            req.body = format!(
                "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><LocationConstraint>{}</LocationConstraint></CreateBucketConfiguration>",
                xml_escape(&self.aws.region)
            )
            .into_bytes();
            req.set_header("Content-Type", "application/xml");
        }
        self.send(req)
    }

    pub fn delete_bucket(&self, bucket: &str) -> Result<Response> {
        self.send(Request::new("DELETE", self.url(bucket, None, None)?))
    }

    /// Whether the bucket exists.
    pub fn head_bucket(&self, bucket: &str) -> Result<bool> {
        match self.send(Request::new("HEAD", self.url(bucket, None, None)?)) {
            Ok(_) => Ok(true),
            Err(e) if matches!(error_code(&e).as_deref(), Some("Http404") | Some("NoSuchBucket") | Some("NotFound")) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Presigned GET URL.
    pub fn download_link(&self, bucket: &str, key: &str, expires_secs: u64) -> Result<String> {
        let url = self.url(bucket, Some(key), None)?;
        Ok(super::sigv4::presign_url("GET", &url, &self.aws.credentials()?, &self.aws.region, "s3", now_secs(), expires_secs).to_string())
    }

    fn key(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::KEY)
            .or_else(|| self.key_name.clone())
            .ok_or_else(|| CamelError::illegal_argument("AWS S3 Key header missing (CamelAwsS3Key) and no keyName option configured"))
    }

    fn set_object_headers(ex: &mut Exchange, resp: &Response) {
        if let Some(v) = resp.header("ETag") {
            ex.set_header(headers::E_TAG, v.to_string());
        }
        if let Some(v) = resp.header("Content-Length").and_then(|v| v.parse::<i64>().ok()) {
            ex.set_header(headers::CONTENT_LENGTH, v);
        }
        if let Some(v) = resp.header("Content-Type") {
            ex.set_header(headers::CONTENT_TYPE, v.to_string());
        }
        if let Some(v) = resp.header("Last-Modified") {
            ex.set_header(headers::LAST_MODIFIED, v.to_string());
        }
        if let Some(v) = resp.header("x-amz-version-id") {
            ex.set_header(headers::VERSION_ID, v.to_string());
        }
        let meta: Vec<(String, String)> = resp
            .headers
            .iter()
            .filter_map(|(k, v)| k.to_ascii_lowercase().strip_prefix("x-amz-meta-").map(|m| (m.to_string(), v.clone())))
            .collect();
        if !meta.is_empty() {
            ex.set_header(headers::METADATA, crate::util::string_map(meta));
        }
    }

    fn produce_op(&self, ex: &mut Exchange, op: &str) -> Result<()> {
        let bucket = header_str(ex, headers::BUCKET_NAME).unwrap_or_else(|| self.bucket.clone());
        match op {
            "putObject" => {
                let key = self.key(ex)?;
                let ct = header_str(ex, headers::CONTENT_TYPE);
                let mut extra = self.sse.upload_headers();
                extra.extend(exchange_upload_headers(ex));
                if self.conditional_writes {
                    extra.push(("If-None-Match".into(), "*".into()));
                }
                if let Some(sc) = header_str(ex, headers::STORAGE_CLASS).or_else(|| self.storage_class.clone()) {
                    extra.push(("x-amz-storage-class".to_string(), sc));
                }
                if let Some(Value::Object(m)) = ex.header(headers::METADATA) {
                    for (k, v) in m {
                        extra.push((format!("x-amz-meta-{}", k.to_ascii_lowercase()), v.to_string()));
                    }
                }
                let part_size = self.part_size as usize;
                if let Some(path) = header_str(ex, headers::SOURCE_FILE) {
                    let (etag, len) = self.client().upload_file(&bucket, &key, &path, ct.as_deref(), &extra, self.multi_part_upload, part_size)?;
                    ex.set_header(headers::E_TAG, etag);
                    ex.set_header(headers::CONTENT_LENGTH, len as i64);
                    return Ok(());
                }
                let body = ex.body().to_bytes();
                if self.multi_part_upload && body.len() > part_size {
                    let mut offset = 0;
                    let etag = self.client().multipart_upload(&bucket, &key, ct.as_deref(), &extra, part_size, &mut |max| {
                        let end = (offset + max).min(body.len());
                        let chunk = body[offset..end].to_vec();
                        offset = end;
                        Ok(if chunk.is_empty() { None } else { Some(chunk) })
                    })?;
                    ex.set_header(headers::E_TAG, etag);
                    return Ok(());
                }
                let resp = self.put_object(&bucket, &key, body, ct.as_deref(), &extra)?;
                if let Some(e) = resp.header("ETag") {
                    ex.set_header(headers::E_TAG, e.to_string());
                }
                if let Some(v) = resp.header("x-amz-version-id") {
                    ex.set_header(headers::VERSION_ID, v.to_string());
                }
            }
            "getObject" | "getObjectRange" | "headObject" => {
                let key = self.key(ex)?;
                if op == "getObject" {
                    if let Some(path) = header_str(ex, headers::DOWNLOAD_FILE) {
                        let (resp, n) = self.client().download_file(&bucket, &key, &path)?;
                        Self::set_object_headers(ex, &resp);
                        ex.set_header(headers::CONTENT_LENGTH, n as i64);
                        ex.set_body(path);
                        return Ok(());
                    }
                }
                let version = header_str(ex, headers::VERSION_ID);
                let resp = match op {
                    "headObject" => self.head_object_version(&bucket, &key, version.as_deref())?,
                    "getObjectRange" => {
                        let start = ex.header(headers::RANGE_START).and_then(|v| v.as_i64()).unwrap_or(0);
                        let end = ex
                            .header(headers::RANGE_END)
                            .and_then(|v| v.as_i64())
                            .ok_or_else(|| CamelError::illegal_argument("getObjectRange requires the CamelAwsS3RangeEnd header"))?;
                        self.get_object_version(&bucket, &key, Some((start, end)), version.as_deref())?
                    }
                    _ => self.get_object_version(&bucket, &key, None, version.as_deref())?,
                };
                Self::set_object_headers(ex, &resp);
                if op != "headObject" {
                    ex.set_body(Value::Bytes(resp.body));
                }
            }
            "listObjects" => {
                let prefix = header_str(ex, headers::PREFIX).or_else(|| self.prefix.clone());
                let delim = header_str(ex, headers::DELIMITER).or_else(|| self.delimiter.clone());
                let r = self.list_objects(&bucket, prefix.as_deref(), delim.as_deref(), None, None)?;
                ex.set_body(Value::Array(r.objects.iter().map(|o| o.to_value()).collect()));
            }
            "deleteObject" => {
                let key = self.key(ex)?;
                let q = header_str(ex, headers::VERSION_ID).map(|v| format!("versionId={}", uri_encode(&v, false)));
                let mut req = Request::new("DELETE", self.url(&bucket, Some(&key), q)?);
                if header_str(ex, super::s3_ops::extra_headers::BYPASS_GOVERNANCE_RETENTION).is_some_and(|b| b == "true") {
                    req.set_header("x-amz-bypass-governance-retention", "true");
                }
                let resp = self.send(req)?;
                if let Some(v) = resp.header("x-amz-version-id") {
                    ex.set_header(headers::VERSION_ID, v.to_string());
                }
                ex.set_body(true);
            }
            "copyObject" => {
                let key = self.key(ex)?;
                let dst_bucket = header_str(ex, headers::BUCKET_DESTINATION_NAME)
                    .or_else(|| self.destination_bucket.clone())
                    .ok_or_else(|| CamelError::illegal_argument("copyObject requires the CamelAwsS3BucketDestinationName header"))?;
                let dst_key = header_str(ex, headers::DESTINATION_KEY)
                    .ok_or_else(|| CamelError::illegal_argument("copyObject requires the CamelAwsS3DestinationKey header"))?;
                let etag = self.copy_object(&bucket, &key, &dst_bucket, &dst_key)?;
                ex.set_header(headers::E_TAG, etag);
            }
            "listBuckets" => {
                let b = self.list_buckets()?;
                ex.set_body(Value::Array(
                    b.into_iter().map(|(n, d)| crate::util::obj([("name", Value::from(n)), ("creationDate", Value::from(d))])).collect(),
                ));
            }
            "createBucket" => {
                self.create_bucket(&bucket)?;
            }
            "deleteBucket" => {
                self.delete_bucket(&bucket)?;
            }
            "headBucket" => {
                let exists = self.head_bucket(&bucket)?;
                ex.set_body(exists);
            }
            "createDownloadLink" => {
                let key = self.key(ex)?;
                let millis = ex.header(headers::DOWNLOAD_LINK_EXPIRATION_TIME).and_then(|v| v.as_i64()).unwrap_or(3_600_000);
                let link = self.download_link(&bucket, &key, (millis / 1000).max(1) as u64)?;
                ex.set_body(link);
            }
            other => {
                return match self.extra_op(ex, other, &bucket) {
                    Some(r) => r,
                    None => Err(unsupported("aws2-s3", other)),
                }
            }
        }
        Ok(())
    }

    fn commit(&self, key: &str) -> Result<()> {
        if self.move_after_read {
            if let Some(dst) = &self.destination_bucket {
                let dst_key = format!("{}{}{}", self.destination_bucket_prefix, key, self.destination_bucket_suffix);
                self.copy_object(&self.bucket, key, dst, &dst_key)?;
            }
        }
        if self.delete_after_read {
            self.delete_object(&self.bucket, key)?;
        }
        Ok(())
    }
}

impl CloudService for S3Service {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        if let Some(up) = &self.streaming {
            if let Some((key, etag)) = up.write(&ex.body().to_bytes())? {
                ex.set_header(headers::KEY, key);
                ex.set_header(headers::E_TAG, etag);
            }
            return Ok(());
        }
        let op = operation(ex, headers::OPERATION, &self.operation, "putObject");
        self.produce_op(ex, &op)
    }

    fn on_stop(&self) -> Result<()> {
        match &self.streaming {
            Some(up) => up.stop(),
            None => Ok(()),
        }
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn on_start(&self) -> Result<()> {
        if self.auto_create_bucket && !self.head_bucket(&self.bucket)? {
            self.create_bucket(&self.bucket)?;
        }
        if let Some(up) = &self.streaming {
            up.start();
        }
        Ok(())
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let keys: Vec<String> = match &self.file_name {
            Some(f) => vec![f.clone()],
            None => {
                let marker = self.marker.lock().unwrap().clone();
                let r = self.list_objects(&self.bucket, self.prefix.as_deref(), self.delimiter.as_deref(), Some(self.max_messages_per_poll), marker.as_deref())?;
                *self.marker.lock().unwrap() = if r.truncated { r.objects.last().map(|o| o.key.clone()) } else { None };
                r.objects.into_iter().map(|o| o.key).filter(|k| self.include_folders || !k.ends_with('/')).collect()
            }
        };
        let mut processed = 0;
        let total = keys.len();
        for (i, key) in keys.iter().enumerate() {
            let download = self.local_directory.as_ref().filter(|_| self.include_body && !self.ignore_body).map(|d| std::path::Path::new(d).join(key));
            if let Some(path) = &download {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
            let resp = match &download {
                Some(path) => self.client().download_file(&self.bucket, key, &path.to_string_lossy()).map(|(r, _)| r),
                None if self.include_body && !self.ignore_body => self.get_object(&self.bucket, key, None),
                None => self.head_object(&self.bucket, key),
            };
            let resp = match resp {
                Ok(r) => r,
                Err(e) if self.file_name.is_some() && error_code(&e).is_some_and(|c| c == "NoSuchKey" || c == "Http404") => return Ok(0),
                Err(e) => {
                    cameleer_core::camel_warn!(logger, "Cannot read S3 object {}: {}", key, e);
                    continue;
                }
            };
            let mut ex = Exchange::new(ctx.clone());
            Self::set_object_headers(&mut ex, &resp);
            ex.set_header(headers::KEY, key.clone());
            ex.set_header(headers::BUCKET_NAME, self.bucket.clone());
            match &download {
                Some(path) => ex.set_body(path.to_string_lossy().into_owned()),
                None if self.include_body && !self.ignore_body => ex.set_body(Value::Bytes(resp.body)),
                None => {}
            }
            set_batch(&mut ex, i, total);
            processed += 1;
            if process(logger, processor, &mut ex) {
                if let Err(e) = self.commit(key) {
                    cameleer_core::camel_warn!(logger, "Error committing S3 object {}: {}", key, e);
                }
            }
        }
        Ok(processed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cameleer_core::util::Parameters;

    fn svc(uri_params: &[(&str, &str)]) -> S3Service {
        let ctx = CamelContext::new();
        let mut p = Parameters::new(uri_params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect());
        let mut o = Opts::new(&mut p, &ctx, "aws2-s3");
        S3Service::from_opts("my-bucket", &mut o).unwrap()
    }

    #[test]
    fn url_styles() {
        let s = svc(&[("region", "eu-west-1"), ("accessKey", "a"), ("secretKey", "b")]);
        assert_eq!(s.url("my-bucket", Some("dir/a b.txt"), None).unwrap().to_string(), "https://my-bucket.s3.eu-west-1.amazonaws.com/dir/a%20b.txt");
        assert_eq!(s.url("my-bucket", None, Some("list-type=2".into())).unwrap().to_string(), "https://my-bucket.s3.eu-west-1.amazonaws.com/?list-type=2");
        assert_eq!(s.url("", None, None).unwrap().to_string(), "https://s3.eu-west-1.amazonaws.com/");
        let p = svc(&[("region", "eu-west-1"), ("forcePathStyle", "true")]);
        assert_eq!(p.url("my-bucket", Some("k"), None).unwrap().to_string(), "https://s3.eu-west-1.amazonaws.com/my-bucket/k");
        let o = svc(&[("uriEndpointOverride", "http://127.0.0.1:4566"), ("overrideEndpoint", "true")]);
        assert_eq!(o.url("my-bucket", Some("k"), None).unwrap().to_string(), "http://127.0.0.1:4566/my-bucket/k");
    }

    #[test]
    fn list_result_parsing() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>b</Name><Prefix></Prefix><KeyCount>2</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>true</IsTruncated><NextContinuationToken>tok</NextContinuationToken>
<Contents><Key>a.txt</Key><LastModified>2009-10-12T17:50:30.000Z</LastModified><ETag>&quot;abc&quot;</ETag><Size>434234</Size><StorageClass>STANDARD</StorageClass></Contents>
<Contents><Key>b.txt</Key><Size>1</Size></Contents><CommonPrefixes><Prefix>photos/</Prefix></CommonPrefixes></ListBucketResult>"#;
        let r = parse_list_result(xml).unwrap();
        assert_eq!(r.objects.len(), 2);
        assert_eq!(r.objects[0].etag, "\"abc\"");
        assert_eq!(r.objects[0].size, 434234);
        assert!(r.truncated);
        assert_eq!(r.next_token.as_deref(), Some("tok"));
        assert_eq!(r.common_prefixes, vec!["photos/"]);
    }

    #[test]
    fn move_requires_destination() {
        let ctx = CamelContext::new();
        let mut p = Parameters::new([("moveAfterRead".to_string(), "true".to_string())].into_iter().collect());
        let mut o = Opts::new(&mut p, &ctx, "aws2-s3");
        assert!(S3Service::from_opts("b", &mut o).is_err());
    }
}
