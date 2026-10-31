//! The `aws2-rekognition` component (rust-camel, producer) over the Amazon Rekognition AWS
//! JSON 1.1 protocol (signing name `rekognition`).
//!
//! `aws2-rekognition://label?operation=detectLabels`
//!
//! The image is the S3 object of `s3Bucket` / `s3Object` (options or
//! `CamelAwsRekognitionS3Bucket` / `...S3Object` headers) or the body bytes.
//!
//! Operations (`operation` / `CamelAwsRekognitionOperation`): `detectLabels` (default;
//! `maxLabels` / `CamelAwsRekognitionMaxLabels`, `minConfidence` /
//! `CamelAwsRekognitionMinConfidence`; body = list of labels), `detectText` (body = text
//! detections), `detectFaces` (body = face details), `detectModerationLabels` (body =
//! moderation labels), `recognizeCelebrities` (body = celebrity faces).

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::{hdr, put, AwsConfig, JsonApi};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{base64_encode, header_i64, header_str, kinds, obj, Opts};

/// Header names.
pub mod headers {
    pub const OPERATION: &str = "CamelAwsRekognitionOperation";
    pub const S3_BUCKET: &str = "CamelAwsRekognitionS3Bucket";
    pub const S3_OBJECT: &str = "CamelAwsRekognitionS3Object";
    pub const MAX_LABELS: &str = "CamelAwsRekognitionMaxLabels";
    pub const MIN_CONFIDENCE: &str = "CamelAwsRekognitionMinConfidence";
}

const API: JsonApi = JsonApi { host: "rekognition", service: "rekognition", target: "RekognitionService", version: "1.1", kind: kinds::REKOGNITION };

/// The Rekognition endpoint configuration and client.
pub struct RekognitionService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub s3_bucket: Option<String>,
    pub s3_object: Option<String>,
    pub max_labels: Option<i64>,
    pub min_confidence: Option<f64>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["rekognitionClient", "pojoRequest"]);
    let min_confidence = match o.get("minConfidence") {
        Some(v) => Some(v.parse::<f64>().map_err(|_| CamelError::illegal_argument(format!("Invalid minConfidence: {}", v)))?),
        None => None,
    };
    Ok(Arc::new(RekognitionService {
        operation: o.get("operation"),
        s3_bucket: o.get("s3Bucket"),
        s3_object: o.get("s3Object"),
        max_labels: crate::util::opt_i64(o, "maxLabels")?,
        min_confidence,
        aws,
    }))
}

impl RekognitionService {
    fn image(&self, ex: &Exchange) -> Result<Value> {
        if let (Some(b), Some(n)) = (hdr(ex, headers::S3_BUCKET, &self.s3_bucket), hdr(ex, headers::S3_OBJECT, &self.s3_object)) {
            return Ok(obj([("S3Object", obj([("Bucket", Value::from(b)), ("Name", Value::from(n))]))]));
        }
        let bytes = ex.body().to_bytes();
        if bytes.is_empty() {
            return Err(CamelError::illegal_argument("An image is required: s3Bucket + s3Object or the body bytes"));
        }
        Ok(obj([("Bytes", Value::from(base64_encode(&bytes)))]))
    }
}

impl CloudService for RekognitionService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "detectLabels");
        let mut m = ObjectMap::new();
        m.insert("Image".into(), self.image(ex)?);
        let min_conf = header_str(ex, headers::MIN_CONFIDENCE).and_then(|v| v.parse::<f64>().ok()).or(self.min_confidence);
        let (action, field) = match op.as_str() {
            "detectLabels" => {
                put(&mut m, "MaxLabels", header_i64(ex, headers::MAX_LABELS).or(self.max_labels));
                put(&mut m, "MinConfidence", min_conf);
                ("DetectLabels", "Labels")
            }
            "detectText" => ("DetectText", "TextDetections"),
            "detectFaces" => {
                m.insert("Attributes".into(), Value::from(vec!["DEFAULT"]));
                ("DetectFaces", "FaceDetails")
            }
            "detectModerationLabels" => {
                put(&mut m, "MinConfidence", min_conf);
                ("DetectModerationLabels", "ModerationLabels")
            }
            "recognizeCelebrities" => ("RecognizeCelebrities", "CelebrityFaces"),
            other => return Err(unsupported("Rekognition", other)),
        };
        let r = self.aws.api(&API, action, &Value::Object(m))?;
        ex.set_body(r.get(field).cloned().unwrap_or(Value::Array(vec![])));
        Ok(())
    }
}
