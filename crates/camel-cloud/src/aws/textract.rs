//! The `aws2-textract` component (Camel `camel-aws2-textract`, producer) over the Textract
//! AWS JSON 1.1 protocol (signing name `textract`).
//!
//! `aws2-textract://label?operation=...`
//!
//! The document is the S3 object of `s3Bucket` / `s3Object` / `s3ObjectVersion` (options or
//! `CamelAwsTextractS3Bucket` / `...S3Object` / `...S3ObjectVersion` headers) or, for the
//! synchronous operations, the body bytes.
//!
//! Operations (`operation` / `CamelAwsTextractOperation`):
//! * `detectDocumentText` (default), `analyzeDocument` (feature types from
//!   `CamelAwsTextractFeatureTypes`, default `TABLES,FORMS`), `analyzeExpense`: body = the
//!   response map (with `Blocks` / `ExpenseDocuments`).
//! * `startDocumentTextDetection`, `startDocumentAnalysis`, `startExpenseAnalysis`: S3 document
//!   only; sets `CamelAwsTextractJobId` (also the body).
//! * `getDocumentTextDetection`, `getDocumentAnalysis`, `getExpenseAnalysis`:
//!   `CamelAwsTextractJobId`, `CamelAwsTextractMaxResults`, `CamelAwsTextractNextToken`; body =
//!   the response map (`JobStatus`, `Blocks`...).

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::{hdr, need, put, AwsConfig, JsonApi};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{base64_encode, header_i64, header_list, header_str, kinds, obj, Opts};

/// Header names (Camel `Textract2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsTextractOperation";
    pub const S3_BUCKET: &str = "CamelAwsTextractS3Bucket";
    pub const S3_OBJECT: &str = "CamelAwsTextractS3Object";
    pub const S3_OBJECT_VERSION: &str = "CamelAwsTextractS3ObjectVersion";
    pub const FEATURE_TYPES: &str = "CamelAwsTextractFeatureTypes";
    pub const JOB_ID: &str = "CamelAwsTextractJobId";
    pub const MAX_RESULTS: &str = "CamelAwsTextractMaxResults";
    pub const NEXT_TOKEN: &str = "CamelAwsTextractNextToken";
}

const API: JsonApi = JsonApi { host: "textract", service: "textract", target: "Textract", version: "1.1", kind: kinds::TEXTRACT };

/// The Textract endpoint configuration and client.
pub struct TextractService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub s3_bucket: Option<String>,
    pub s3_object: Option<String>,
    pub s3_object_version: Option<String>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["textractClient", "pojoRequest"]);
    Ok(Arc::new(TextractService { operation: o.get("operation"), s3_bucket: o.get("s3Bucket"), s3_object: o.get("s3Object"), s3_object_version: o.get("s3ObjectVersion"), aws }))
}

impl TextractService {
    /// Invoke a Textract API action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        self.aws.api(&API, action, &body)
    }

    fn s3(&self, ex: &Exchange) -> Option<Value> {
        let bucket = hdr(ex, headers::S3_BUCKET, &self.s3_bucket)?;
        let name = hdr(ex, headers::S3_OBJECT, &self.s3_object)?;
        let mut m = ObjectMap::new();
        m.insert("Bucket".into(), Value::from(bucket));
        m.insert("Name".into(), Value::from(name));
        put(&mut m, "Version", hdr(ex, headers::S3_OBJECT_VERSION, &self.s3_object_version));
        Some(Value::Object(m))
    }

    fn document(&self, ex: &Exchange) -> Result<Value> {
        if let Some(s3) = self.s3(ex) {
            return Ok(obj([("S3Object", s3)]));
        }
        let bytes = ex.body().to_bytes();
        if bytes.is_empty() {
            return Err(CamelError::illegal_argument("A document is required: s3Bucket + s3Object or the body bytes"));
        }
        Ok(obj([("Bytes", Value::from(base64_encode(&bytes)))]))
    }

    fn location(&self, ex: &Exchange) -> Result<Value> {
        self.s3(ex).map(|s3| obj([("S3Object", s3)])).ok_or_else(|| CamelError::illegal_argument("Asynchronous Textract operations need s3Bucket and s3Object"))
    }

    fn get_job(&self, ex: &Exchange, action: &str) -> Result<Value> {
        let mut m = ObjectMap::new();
        m.insert("JobId".into(), Value::from(need(ex, headers::JOB_ID)?));
        put(&mut m, "MaxResults", header_i64(ex, headers::MAX_RESULTS));
        put(&mut m, "NextToken", header_str(ex, headers::NEXT_TOKEN));
        self.call(action, Value::Object(m))
    }
}

impl CloudService for TextractService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "detectDocumentText");
        let feature_types = || {
            let f = header_list(ex, headers::FEATURE_TYPES);
            Value::from(if f.is_empty() { vec!["TABLES".to_string(), "FORMS".to_string()] } else { f })
        };
        let r = match op.as_str() {
            "detectDocumentText" => self.call("DetectDocumentText", obj([("Document", self.document(ex)?)]))?,
            "analyzeDocument" => self.call("AnalyzeDocument", obj([("Document", self.document(ex)?), ("FeatureTypes", feature_types())]))?,
            "analyzeExpense" => self.call("AnalyzeExpense", obj([("Document", self.document(ex)?)]))?,
            "startDocumentTextDetection" | "startDocumentAnalysis" | "startExpenseAnalysis" => {
                let mut m = ObjectMap::new();
                m.insert("DocumentLocation".into(), self.location(ex)?);
                if op == "startDocumentAnalysis" {
                    m.insert("FeatureTypes".into(), feature_types());
                }
                let action = format!("{}{}", op[..1].to_ascii_uppercase(), &op[1..]);
                let r = self.call(&action, Value::Object(m))?;
                let id = r.get("JobId").cloned().unwrap_or_default();
                ex.set_header(headers::JOB_ID, id.clone());
                ex.set_body(id);
                return Ok(());
            }
            "getDocumentTextDetection" => self.get_job(ex, "GetDocumentTextDetection")?,
            "getDocumentAnalysis" => self.get_job(ex, "GetDocumentAnalysis")?,
            "getExpenseAnalysis" => self.get_job(ex, "GetExpenseAnalysis")?,
            other => return Err(unsupported("Textract", other)),
        };
        if let Some(t) = r.get("NextToken").filter(|t| !t.to_string().is_empty()) {
            ex.set_header(headers::NEXT_TOKEN, t.clone());
        }
        ex.set_body(r);
        Ok(())
    }
}
