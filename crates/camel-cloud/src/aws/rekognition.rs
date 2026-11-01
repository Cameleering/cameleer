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
//!
//! Face collections (`CamelAwsRekognitionCollectionId` / `collectionId`): `createCollection`,
//! `deleteCollection`, `describeCollection` (body = the description), `listCollections` (body =
//! collection ids), `indexFaces` (image, `CamelAwsRekognitionExternalImageId`,
//! `CamelAwsRekognitionMaxFaces`; body = face records), `listFaces` (body = faces),
//! `deleteFaces` (`CamelAwsRekognitionFaceIds` list or a list body; body = deleted ids),
//! `searchFaces` (`CamelAwsRekognitionFaceId`), `searchFacesByImage` (image) — both with
//! `CamelAwsRekognitionFaceMatchThreshold` / `CamelAwsRekognitionMaxFaces`, body = face
//! matches — and `compareFaces` (source = the image, target =
//! `CamelAwsRekognitionTargetS3Bucket` / `...TargetS3Object` or `CamelAwsRekognitionTargetImage`
//! bytes; body = face matches, `CamelAwsRekognitionUnmatchedFaces` header).
//!
//! Stored video analysis (video = `s3Bucket` / `s3Object`): `startLabelDetection`,
//! `startFaceDetection`, `startTextDetection`, `startFaceSearch` (with the collection) — body =
//! job id, also `CamelAwsRekognitionJobId` — and `getLabelDetection`, `getFaceDetection`,
//! `getTextDetection`, `getFaceSearch` (`CamelAwsRekognitionJobId`; body = the results,
//! `CamelAwsRekognitionJobStatus` header; all pages read).

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
    pub const COLLECTION_ID: &str = "CamelAwsRekognitionCollectionId";
    pub const EXTERNAL_IMAGE_ID: &str = "CamelAwsRekognitionExternalImageId";
    pub const MAX_FACES: &str = "CamelAwsRekognitionMaxFaces";
    pub const FACE_ID: &str = "CamelAwsRekognitionFaceId";
    pub const FACE_IDS: &str = "CamelAwsRekognitionFaceIds";
    pub const FACE_MATCH_THRESHOLD: &str = "CamelAwsRekognitionFaceMatchThreshold";
    pub const TARGET_S3_BUCKET: &str = "CamelAwsRekognitionTargetS3Bucket";
    pub const TARGET_S3_OBJECT: &str = "CamelAwsRekognitionTargetS3Object";
    pub const TARGET_IMAGE: &str = "CamelAwsRekognitionTargetImage";
    pub const UNMATCHED_FACES: &str = "CamelAwsRekognitionUnmatchedFaces";
    pub const JOB_ID: &str = "CamelAwsRekognitionJobId";
    pub const JOB_STATUS: &str = "CamelAwsRekognitionJobStatus";
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
    pub collection_id: Option<String>,
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
        collection_id: o.get("collectionId"),
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

impl RekognitionService {
    fn collection(&self, ex: &Exchange) -> Result<String> {
        hdr(ex, headers::COLLECTION_ID, &self.collection_id).ok_or_else(|| CamelError::illegal_argument("The CamelAwsRekognitionCollectionId header (or the collectionId option) is required"))
    }

    fn video(&self, ex: &Exchange) -> Result<Value> {
        match (hdr(ex, headers::S3_BUCKET, &self.s3_bucket), hdr(ex, headers::S3_OBJECT, &self.s3_object)) {
            (Some(b), Some(n)) => Ok(obj([("S3Object", obj([("Bucket", Value::from(b)), ("Name", Value::from(n))]))])),
            _ => Err(CamelError::illegal_argument("Video operations require s3Bucket and s3Object")),
        }
    }

    fn face_ops(&self, ex: &mut Exchange, op: &str) -> Result<bool> {
        let mut m = ObjectMap::new();
        let threshold = header_str(ex, headers::FACE_MATCH_THRESHOLD).and_then(|v| v.parse::<f64>().ok());
        let (action, field): (&str, Option<&str>) = match op {
            "createCollection" | "deleteCollection" | "describeCollection" => {
                m.insert("CollectionId".into(), Value::from(self.collection(ex)?));
                let action = match op {
                    "createCollection" => "CreateCollection",
                    "deleteCollection" => "DeleteCollection",
                    _ => "DescribeCollection",
                };
                (action, None)
            }
            "listCollections" => {
                let all = self.aws.api_paged(&API, "ListCollections", ObjectMap::new(), "CollectionIds", "NextToken")?;
                ex.set_body(Value::Array(all));
                return Ok(true);
            }
            "indexFaces" => {
                m.insert("CollectionId".into(), Value::from(self.collection(ex)?));
                m.insert("Image".into(), self.image(ex)?);
                put(&mut m, "ExternalImageId", header_str(ex, headers::EXTERNAL_IMAGE_ID));
                put(&mut m, "MaxFaces", header_i64(ex, headers::MAX_FACES));
                ("IndexFaces", Some("FaceRecords"))
            }
            "listFaces" => {
                let mut req = ObjectMap::new();
                req.insert("CollectionId".into(), Value::from(self.collection(ex)?));
                let all = self.aws.api_paged(&API, "ListFaces", req, "Faces", "NextToken")?;
                ex.set_body(Value::Array(all));
                return Ok(true);
            }
            "deleteFaces" => {
                m.insert("CollectionId".into(), Value::from(self.collection(ex)?));
                let ids = match ex.header(headers::FACE_IDS) {
                    Some(v) => crate::util::str_list(v),
                    None => crate::util::str_list(ex.body()),
                };
                m.insert("FaceIds".into(), Value::from(ids));
                ("DeleteFaces", Some("DeletedFaces"))
            }
            "searchFaces" => {
                m.insert("CollectionId".into(), Value::from(self.collection(ex)?));
                m.insert("FaceId".into(), Value::from(header_str(ex, headers::FACE_ID).ok_or_else(|| CamelError::illegal_argument("searchFaces requires the CamelAwsRekognitionFaceId header"))?));
                put(&mut m, "FaceMatchThreshold", threshold);
                put(&mut m, "MaxFaces", header_i64(ex, headers::MAX_FACES));
                ("SearchFaces", Some("FaceMatches"))
            }
            "searchFacesByImage" => {
                m.insert("CollectionId".into(), Value::from(self.collection(ex)?));
                m.insert("Image".into(), self.image(ex)?);
                put(&mut m, "FaceMatchThreshold", threshold);
                put(&mut m, "MaxFaces", header_i64(ex, headers::MAX_FACES));
                ("SearchFacesByImage", Some("FaceMatches"))
            }
            "compareFaces" => {
                m.insert("SourceImage".into(), self.image(ex)?);
                let target = match (header_str(ex, headers::TARGET_S3_BUCKET), header_str(ex, headers::TARGET_S3_OBJECT), ex.header(headers::TARGET_IMAGE)) {
                    (Some(b), Some(n), _) => obj([("S3Object", obj([("Bucket", Value::from(b)), ("Name", Value::from(n))]))]),
                    (_, _, Some(v)) => obj([("Bytes", Value::from(base64_encode(&v.to_bytes())))]),
                    _ => return Err(CamelError::illegal_argument("compareFaces requires a target image (CamelAwsRekognitionTargetS3Bucket/Object or CamelAwsRekognitionTargetImage)")),
                };
                m.insert("TargetImage".into(), target);
                put(&mut m, "SimilarityThreshold", threshold);
                let r = self.aws.api(&API, "CompareFaces", &Value::Object(m))?;
                ex.set_header(headers::UNMATCHED_FACES, r.get("UnmatchedFaces").cloned().unwrap_or(Value::Array(vec![])));
                ex.set_body(r.get("FaceMatches").cloned().unwrap_or(Value::Array(vec![])));
                return Ok(true);
            }
            "startLabelDetection" | "startFaceDetection" | "startTextDetection" | "startFaceSearch" => {
                m.insert("Video".into(), self.video(ex)?);
                if op == "startFaceSearch" {
                    m.insert("CollectionId".into(), Value::from(self.collection(ex)?));
                }
                let mut action = op.to_string();
                action[..1].make_ascii_uppercase();
                let r = self.aws.api(&API, &action, &Value::Object(m))?;
                let job = r.get("JobId").cloned().unwrap_or_default();
                ex.set_header(headers::JOB_ID, job.clone());
                ex.set_body(job);
                return Ok(true);
            }
            "getLabelDetection" | "getFaceDetection" | "getTextDetection" | "getFaceSearch" => {
                let job = header_str(ex, headers::JOB_ID).ok_or_else(|| CamelError::illegal_argument(format!("{} requires the CamelAwsRekognitionJobId header", op)))?;
                let mut action = op.to_string();
                action[..1].make_ascii_uppercase();
                let field = match op {
                    "getLabelDetection" => "Labels",
                    "getFaceDetection" => "Faces",
                    "getTextDetection" => "TextDetections",
                    _ => "Persons",
                };
                let mut all = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let mut req = ObjectMap::new();
                    req.insert("JobId".into(), Value::from(job.clone()));
                    put(&mut req, "NextToken", token.clone());
                    let r = self.aws.api(&API, &action, &Value::Object(req))?;
                    ex.set_header(headers::JOB_STATUS, r.get("JobStatus").cloned().unwrap_or_default());
                    all.extend(r.get(field).and_then(|a| a.as_array()).cloned().unwrap_or_default());
                    token = r.get("NextToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
                    if token.is_none() {
                        break;
                    }
                }
                ex.set_body(Value::Array(all));
                return Ok(true);
            }
            _ => return Ok(false),
        };
        let r = self.aws.api(&API, action, &Value::Object(m))?;
        ex.set_body(match field {
            Some(f) => r.get(f).cloned().unwrap_or(Value::Array(vec![])),
            None => r,
        });
        Ok(true)
    }
}

impl CloudService for RekognitionService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "detectLabels");
        if self.face_ops(ex, &op)? {
            return Ok(());
        }
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
