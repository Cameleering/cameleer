//! The remaining `aws2-s3` producer operations and server-side encryption support:
//!
//! * `deleteObjects`: keys from `CamelAwsS3KeysToDelete` (list) or a list body
//!   (`DeleteObjects` with `Content-MD5`); body = deleted keys, `CamelAwsS3DeleteErrors`
//!   header = `{key, code, message}` failures.
//! * `createUploadLink`: presigned PUT URL (`CamelAwsS3UploadLinkExpirationTime` millis,
//!   default 1 hour).
//! * `restoreObject`: restores an archived object for `CamelAwsS3RestoreDays` (default 1) days
//!   (`CamelAwsS3RestoreTier`).
//! * `getObjectTagging` (body = map), `putObjectTagging` (`CamelAwsS3ObjectTags` map or a map
//!   body), `deleteObjectTagging`.
//! * `getObjectAcl` (body = `{owner, grants: [{grantee, permission}]}`), `putObjectAcl`
//!   (`CamelAwsS3CannedAcl`, or an `AccessControlPolicy` XML body).
//! * Object lock (Cameleer operation names): `putObjectRetention` /
//!   `getObjectRetention` (`CamelAwsS3ObjectLockMode` GOVERNANCE / COMPLIANCE,
//!   `CamelAwsS3ObjectLockRetainUntilDate` ISO-8601, `CamelAwsS3BypassGovernanceRetention`),
//!   `putObjectLegalHold` / `getObjectLegalHold` (`CamelAwsS3ObjectLockLegalHoldStatus` ON / OFF);
//!   the same headers on `putObject` set lock values at upload.
//! * Versioning (Cameleer): `putBucketVersioning` (`CamelAwsS3VersioningStatus` Enabled /
//!   Suspended), `getBucketVersioning`, `listObjectVersions`; `getObject` / `deleteObject` /
//!   `headObject` honour `CamelAwsS3VersionId`.
//!
//! Encryption options: `useAwsKMS=true` + `awsKMSKeyId` (SSE-KMS), `useSSES3=true` (SSE-S3),
//! `useCustomerKey=true` + `customerKeyId` (base64 256-bit key) / `customerKeyMD5` (computed
//! when omitted) / `customerAlgorithm` (default AES256) for SSE-C, sent on uploads (single and
//! multipart parts) and, for SSE-C, on reads.

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::s3::{headers, S3Service};
use crate::http::Request;
use crate::util::{base64_decode, base64_encode, header_i64, header_str, md5, obj, str_list, uri_encode, xml_escape, Xml};

/// Extra header names used by these operations.
pub mod extra_headers {
    pub const KEYS_TO_DELETE: &str = "CamelAwsS3KeysToDelete";
    pub const DELETE_ERRORS: &str = "CamelAwsS3DeleteErrors";
    pub const UPLOAD_LINK_EXPIRATION_TIME: &str = "CamelAwsS3UploadLinkExpirationTime";
    pub const RESTORE_DAYS: &str = "CamelAwsS3RestoreDays";
    pub const RESTORE_TIER: &str = "CamelAwsS3RestoreTier";
    pub const OBJECT_TAGS: &str = "CamelAwsS3ObjectTags";
    pub const CANNED_ACL: &str = "CamelAwsS3CannedAcl";
    pub const SERVER_SIDE_ENCRYPTION: &str = "CamelAwsS3ServerSideEncryption";
    pub const CONTENT_DISPOSITION: &str = "CamelAwsS3ContentDisposition";
    pub const CONTENT_ENCODING: &str = "CamelAwsS3ContentEncoding";
    pub const CONTENT_CONTROL: &str = "CamelAwsS3ContentControl";
    pub const S3_HEADERS: &str = "CamelAwsS3Headers";
    pub const OBJECT_LOCK_MODE: &str = "CamelAwsS3ObjectLockMode";
    pub const OBJECT_LOCK_RETAIN_UNTIL_DATE: &str = "CamelAwsS3ObjectLockRetainUntilDate";
    pub const OBJECT_LOCK_LEGAL_HOLD_STATUS: &str = "CamelAwsS3ObjectLockLegalHoldStatus";
    pub const BYPASS_GOVERNANCE_RETENTION: &str = "CamelAwsS3BypassGovernanceRetention";
    pub const VERSIONING_STATUS: &str = "CamelAwsS3VersioningStatus";
}
use extra_headers as xh;

/// Server-side encryption configuration of an endpoint.
#[derive(Clone, Debug, Default)]
pub struct Sse {
    pub kms: bool,
    pub kms_key_id: Option<String>,
    pub sse_s3: bool,
    /// (algorithm, base64 key, base64 MD5 of the key)
    pub customer: Option<(String, String, String)>,
}

impl Sse {
    /// Read `useAwsKMS`, `awsKMSKeyId`, `useSSES3`, `useCustomerKey`, `customerKeyId`,
    /// `customerKeyMD5`, `customerAlgorithm`.
    pub fn from_opts(o: &mut crate::util::Opts) -> Result<Sse> {
        let kms = o.bool("useAwsKMS", false)?;
        let kms_key_id = o.get("awsKMSKeyId");
        let sse_s3 = o.bool("useSSES3", false)?;
        let customer = if o.bool("useCustomerKey", false)? {
            let key = o.get("customerKeyId").ok_or_else(|| CamelError::illegal_argument("useCustomerKey requires the customerKeyId option (base64 key)"))?;
            let raw = base64_decode(&key).ok_or_else(|| CamelError::illegal_argument("customerKeyId must be a base64 encoded key"))?;
            let md5_b64 = o.get("customerKeyMD5").unwrap_or_else(|| base64_encode(&md5(&raw)));
            Some((o.string("customerAlgorithm", "AES256"), key, md5_b64))
        } else {
            o.ignore(&["customerKeyId", "customerKeyMD5", "customerAlgorithm"]);
            None
        };
        if kms && kms_key_id.is_none() {
            return Err(CamelError::illegal_argument("useAwsKMS requires the awsKMSKeyId option"));
        }
        Ok(Sse { kms, kms_key_id, sse_s3, customer })
    }

    /// SSE-C headers (uploads, parts and reads).
    pub fn customer_headers(&self) -> Vec<(String, String)> {
        match &self.customer {
            Some((alg, key, md5)) => vec![
                ("x-amz-server-side-encryption-customer-algorithm".into(), alg.clone()),
                ("x-amz-server-side-encryption-customer-key".into(), key.clone()),
                ("x-amz-server-side-encryption-customer-key-MD5".into(), md5.clone()),
            ],
            None => Vec::new(),
        }
    }

    /// Headers of an object upload (`PutObject` / `CreateMultipartUpload` / copy target).
    pub fn upload_headers(&self) -> Vec<(String, String)> {
        let mut h = Vec::new();
        if self.kms {
            h.push(("x-amz-server-side-encryption".into(), "aws:kms".into()));
            if let Some(k) = &self.kms_key_id {
                h.push(("x-amz-server-side-encryption-aws-kms-key-id".into(), k.clone()));
            }
        } else if self.sse_s3 {
            h.push(("x-amz-server-side-encryption".into(), "AES256".into()));
        }
        h.extend(self.customer_headers());
        h
    }
}

/// Per-exchange upload headers (ACL, content disposition / encoding / cache control, SSE
/// header, custom headers, tagging, object lock).
pub fn exchange_upload_headers(ex: &Exchange) -> Vec<(String, String)> {
    let mut h = Vec::new();
    let pairs = [
        (xh::CANNED_ACL, "x-amz-acl"),
        (xh::SERVER_SIDE_ENCRYPTION, "x-amz-server-side-encryption"),
        (xh::CONTENT_DISPOSITION, "Content-Disposition"),
        (xh::CONTENT_ENCODING, "Content-Encoding"),
        (xh::CONTENT_CONTROL, "Cache-Control"),
        (xh::OBJECT_LOCK_MODE, "x-amz-object-lock-mode"),
        (xh::OBJECT_LOCK_RETAIN_UNTIL_DATE, "x-amz-object-lock-retain-until-date"),
        (xh::OBJECT_LOCK_LEGAL_HOLD_STATUS, "x-amz-object-lock-legal-hold"),
    ];
    for (camel, http) in pairs {
        if let Some(v) = header_str(ex, camel) {
            h.push((http.to_string(), v));
        }
    }
    if let Some(Value::Object(tags)) = ex.header(xh::OBJECT_TAGS) {
        let q: Vec<String> = tags.iter().map(|(k, v)| format!("{}={}", uri_encode(k, false), uri_encode(&v.to_string(), false))).collect();
        h.push(("x-amz-tagging".into(), q.join("&")));
    }
    if let Some(Value::Object(m)) = ex.header(xh::S3_HEADERS) {
        for (k, v) in m {
            h.push((k.clone(), v.to_string()));
        }
    }
    h
}

fn tagging_xml(tags: &ObjectMap) -> String {
    let mut x = String::from("<Tagging xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><TagSet>");
    for (k, v) in tags {
        x.push_str(&format!("<Tag><Key>{}</Key><Value>{}</Value></Tag>", xml_escape(k), xml_escape(&v.to_string())));
    }
    x.push_str("</TagSet></Tagging>");
    x
}

impl S3Service {
    fn sub_url(&self, bucket: &str, key: Option<&str>, sub: &str, ex: &Exchange) -> Result<crate::http::Url> {
        let mut q = sub.to_string();
        if let Some(v) = header_str(ex, headers::VERSION_ID) {
            q.push_str(&format!("&versionId={}", uri_encode(&v, false)));
        }
        self.url(bucket, key, Some(q))
    }

    fn send_s3(&self, req: Request) -> Result<crate::http::Response> {
        self.aws.send(req, "s3", crate::util::kinds::S3)
    }

    /// The operations of this module; `None` for an unknown operation.
    pub(super) fn extra_op(&self, ex: &mut Exchange, op: &str, bucket: &str) -> Option<Result<()>> {
        let r = match op {
            "deleteObjects" => self.delete_objects(ex, bucket),
            "createUploadLink" => self.key_of(ex).and_then(|key| {
                let millis = header_i64(ex, xh::UPLOAD_LINK_EXPIRATION_TIME).unwrap_or(3_600_000);
                let url = self.url(bucket, Some(&key), None)?;
                let link = super::sigv4::presign_url("PUT", &url, &self.aws.credentials()?, &self.aws.region, "s3", crate::util::now_secs(), (millis / 1000).max(1) as u64);
                ex.set_body(link.to_string());
                Ok(())
            }),
            "restoreObject" => self.key_of(ex).and_then(|key| {
                let days = header_i64(ex, xh::RESTORE_DAYS).unwrap_or(1);
                let tier = header_str(ex, xh::RESTORE_TIER).unwrap_or_else(|| "Standard".into());
                let xml = format!("<RestoreRequest xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Days>{}</Days><GlacierJobParameters><Tier>{}</Tier></GlacierJobParameters></RestoreRequest>", days, xml_escape(&tier));
                let resp = self.send_s3(Request::new("POST", self.sub_url(bucket, Some(&key), "restore", ex)?).header("Content-Type", "application/xml").body(xml.into_bytes()))?;
                ex.set_body(resp.status == 202 || resp.status == 200);
                Ok(())
            }),
            "getObjectTagging" => self.key_of(ex).and_then(|key| {
                let resp = self.send_s3(Request::new("GET", self.sub_url(bucket, Some(&key), "tagging", ex)?))?;
                let x = Xml::parse(&resp.text())?;
                let mut m = ObjectMap::new();
                for t in x.descendants("Tag") {
                    m.insert(x.child_text(t, "Key").unwrap_or_default(), Value::from(x.child_text(t, "Value").unwrap_or_default()));
                }
                ex.set_body(Value::Object(m));
                Ok(())
            }),
            "putObjectTagging" => self.key_of(ex).and_then(|key| {
                let tags = match (ex.header(xh::OBJECT_TAGS), ex.body()) {
                    (Some(Value::Object(m)), _) | (None, Value::Object(m)) => m.clone(),
                    _ => return Err(CamelError::illegal_argument("putObjectTagging requires the CamelAwsS3ObjectTags header (map) or a map body")),
                };
                let body = tagging_xml(&tags).into_bytes();
                let mut req = Request::new("PUT", self.sub_url(bucket, Some(&key), "tagging", ex)?).header("Content-Type", "application/xml");
                req.set_header("Content-MD5", base64_encode(&md5(&body)));
                self.send_s3(req.body(body))?;
                Ok(())
            }),
            "deleteObjectTagging" => self.key_of(ex).and_then(|key| {
                self.send_s3(Request::new("DELETE", self.sub_url(bucket, Some(&key), "tagging", ex)?))?;
                Ok(())
            }),
            "getObjectAcl" => self.key_of(ex).and_then(|key| {
                let resp = self.send_s3(Request::new("GET", self.sub_url(bucket, Some(&key), "acl", ex)?))?;
                let x = Xml::parse(&resp.text())?;
                let owner = x.descendants("Owner").first().and_then(|o| x.child_text(*o, "ID")).unwrap_or_default();
                let grants: Vec<Value> = x
                    .descendants("Grant")
                    .into_iter()
                    .map(|g| {
                        let grantee = x.child(g, "Grantee").map(|ge| x.child_text(ge, "ID").or_else(|| x.child_text(ge, "URI")).unwrap_or_default()).unwrap_or_default();
                        obj([("grantee", Value::from(grantee)), ("permission", Value::from(x.child_text(g, "Permission").unwrap_or_default()))])
                    })
                    .collect();
                ex.set_body(obj([("owner", Value::from(owner)), ("grants", Value::Array(grants))]));
                Ok(())
            }),
            "putObjectAcl" => self.key_of(ex).and_then(|key| {
                let mut req = Request::new("PUT", self.sub_url(bucket, Some(&key), "acl", ex)?);
                match header_str(ex, xh::CANNED_ACL) {
                    Some(acl) => req.set_header("x-amz-acl", acl),
                    None => {
                        let body = ex.body().to_bytes();
                        if body.is_empty() {
                            return Err(CamelError::illegal_argument("putObjectAcl requires the CamelAwsS3CannedAcl header or an AccessControlPolicy XML body"));
                        }
                        req.set_header("Content-Type", "application/xml");
                        req.set_header("Content-MD5", base64_encode(&md5(&body)));
                        req = req.body(body);
                    }
                }
                self.send_s3(req)?;
                Ok(())
            }),
            "putObjectRetention" => self.key_of(ex).and_then(|key| {
                let mode = header_str(ex, xh::OBJECT_LOCK_MODE).ok_or_else(|| CamelError::illegal_argument("putObjectRetention requires CamelAwsS3ObjectLockMode"))?;
                let until = header_str(ex, xh::OBJECT_LOCK_RETAIN_UNTIL_DATE).ok_or_else(|| CamelError::illegal_argument("putObjectRetention requires CamelAwsS3ObjectLockRetainUntilDate"))?;
                let body = format!("<Retention xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Mode>{}</Mode><RetainUntilDate>{}</RetainUntilDate></Retention>", xml_escape(&mode), xml_escape(&until)).into_bytes();
                let mut req = Request::new("PUT", self.sub_url(bucket, Some(&key), "retention", ex)?).header("Content-Type", "application/xml");
                req.set_header("Content-MD5", base64_encode(&md5(&body)));
                if header_str(ex, xh::BYPASS_GOVERNANCE_RETENTION).is_some_and(|b| b == "true") {
                    req.set_header("x-amz-bypass-governance-retention", "true");
                }
                self.send_s3(req.body(body))?;
                Ok(())
            }),
            "getObjectRetention" => self.key_of(ex).and_then(|key| {
                let resp = self.send_s3(Request::new("GET", self.sub_url(bucket, Some(&key), "retention", ex)?))?;
                let x = Xml::parse(&resp.text())?;
                let (mode, until) = (x.find_text("Mode").unwrap_or_default(), x.find_text("RetainUntilDate").unwrap_or_default());
                ex.set_header(xh::OBJECT_LOCK_MODE, mode.clone());
                ex.set_header(xh::OBJECT_LOCK_RETAIN_UNTIL_DATE, until.clone());
                ex.set_body(obj([("mode", Value::from(mode)), ("retainUntilDate", Value::from(until))]));
                Ok(())
            }),
            "putObjectLegalHold" => self.key_of(ex).and_then(|key| {
                let status = header_str(ex, xh::OBJECT_LOCK_LEGAL_HOLD_STATUS).unwrap_or_else(|| "ON".into());
                let body = format!("<LegalHold xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Status>{}</Status></LegalHold>", xml_escape(&status)).into_bytes();
                let mut req = Request::new("PUT", self.sub_url(bucket, Some(&key), "legal-hold", ex)?).header("Content-Type", "application/xml");
                req.set_header("Content-MD5", base64_encode(&md5(&body)));
                self.send_s3(req.body(body))?;
                Ok(())
            }),
            "getObjectLegalHold" => self.key_of(ex).and_then(|key| {
                let resp = self.send_s3(Request::new("GET", self.sub_url(bucket, Some(&key), "legal-hold", ex)?))?;
                let status = Xml::parse(&resp.text())?.find_text("Status").unwrap_or_default();
                ex.set_header(xh::OBJECT_LOCK_LEGAL_HOLD_STATUS, status.clone());
                ex.set_body(status);
                Ok(())
            }),
            "putBucketVersioning" => (|| {
                let status = header_str(ex, xh::VERSIONING_STATUS).unwrap_or_else(|| "Enabled".into());
                let body = format!("<VersioningConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Status>{}</Status></VersioningConfiguration>", xml_escape(&status)).into_bytes();
                self.send_s3(Request::new("PUT", self.url(bucket, None, Some("versioning".into()))?).header("Content-Type", "application/xml").body(body))?;
                Ok(())
            })(),
            "getBucketVersioning" => (|| {
                let resp = self.send_s3(Request::new("GET", self.url(bucket, None, Some("versioning".into()))?))?;
                let status = Xml::parse(&resp.text())?.find_text("Status").unwrap_or_default();
                ex.set_header(xh::VERSIONING_STATUS, status.clone());
                ex.set_body(status);
                Ok(())
            })(),
            "listObjectVersions" => (|| {
                let mut q = "versions".to_string();
                if let Some(p) = header_str(ex, headers::PREFIX).or_else(|| self.prefix.clone()) {
                    q.push_str(&format!("&prefix={}", uri_encode(&p, false)));
                }
                let resp = self.send_s3(Request::new("GET", self.url(bucket, None, Some(q))?))?;
                let x = Xml::parse(&resp.text())?;
                let mut out = Vec::new();
                for (tag, marker) in [("Version", false), ("DeleteMarker", true)] {
                    for v in x.descendants(tag) {
                        out.push(obj([
                            ("key", Value::from(x.child_text(v, "Key").unwrap_or_default())),
                            ("versionId", Value::from(x.child_text(v, "VersionId").unwrap_or_default())),
                            ("isLatest", Value::Bool(x.child_text(v, "IsLatest").as_deref() == Some("true"))),
                            ("deleteMarker", Value::Bool(marker)),
                            ("size", Value::Int(x.child_text(v, "Size").and_then(|s| s.parse().ok()).unwrap_or(0))),
                        ]));
                    }
                }
                ex.set_body(Value::Array(out));
                Ok(())
            })(),
            _ => return None,
        };
        Some(r)
    }

    fn key_of(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::KEY).or_else(|| self.key_name.clone()).ok_or_else(|| CamelError::illegal_argument("AWS S3 Key header missing (CamelAwsS3Key) and no keyName option configured"))
    }

    fn delete_objects(&self, ex: &mut Exchange, bucket: &str) -> Result<()> {
        let keys = match ex.header(xh::KEYS_TO_DELETE) {
            Some(v) => str_list(v),
            None => match ex.body() {
                Value::Array(_) => str_list(ex.body()),
                _ => return Err(CamelError::illegal_argument("deleteObjects requires the CamelAwsS3KeysToDelete header or a list body")),
            },
        };
        let mut xml = String::from("<Delete xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Quiet>false</Quiet>");
        for k in &keys {
            xml.push_str(&format!("<Object><Key>{}</Key></Object>", xml_escape(k)));
        }
        xml.push_str("</Delete>");
        let body = xml.into_bytes();
        let mut req = Request::new("POST", self.url(bucket, None, Some("delete".into()))?).header("Content-Type", "application/xml");
        req.set_header("Content-MD5", base64_encode(&md5(&body)));
        let resp = self.send_s3(req.body(body))?;
        let x = Xml::parse(&resp.text())?;
        let deleted: Vec<Value> = x.descendants("Deleted").into_iter().map(|d| Value::from(x.child_text(d, "Key").unwrap_or_default())).collect();
        let errors: Vec<Value> = x
            .descendants("Error")
            .into_iter()
            .map(|e| obj([("key", Value::from(x.child_text(e, "Key").unwrap_or_default())), ("code", Value::from(x.child_text(e, "Code").unwrap_or_default())), ("message", Value::from(x.child_text(e, "Message").unwrap_or_default()))]))
            .collect();
        ex.set_header(xh::DELETE_ERRORS, Value::Array(errors));
        ex.set_body(Value::Array(deleted));
        Ok(())
    }
}
