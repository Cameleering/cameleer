//! The `aws2-transcribe` component (Camel `camel-aws2-transcribe`, producer) over the Amazon
//! Transcribe AWS JSON 1.1 protocol (signing name `transcribe`).
//!
//! `aws2-transcribe://label?operation=...`
//!
//! Operations (`operation` / `CamelAwsTranscribeOperation`):
//! * `startTranscriptionJob`: `CamelAwsTranscribeTranscriptionJobName`, media
//!   `CamelAwsTranscribeMediaUri` (or the body), `CamelAwsTranscribeLanguageCode` (or
//!   `languageCode`, or `CamelAwsTranscribeIdentifyLanguage=true`), `...MediaFormat`,
//!   `...MediaSampleRateHertz`, `...OutputBucketName`; body = the job map.
//! * `getTranscriptionJob`, `deleteTranscriptionJob` (`...TranscriptionJobName`): body = job.
//! * `listTranscriptionJobs` (`...Status`, `...JobNameContains`): body = job summaries.
//! * Medical jobs: `startMedicalTranscriptionJob` (`CamelAwsTranscribeMedicalTranscriptionJobName`,
//!   media as above, `...OutputBucketName` (required), `CamelAwsTranscribeSpecialty` (default
//!   PRIMARYCARE), `CamelAwsTranscribeType` (CONVERSATION / DICTATION, default CONVERSATION)),
//!   `getMedicalTranscriptionJob`, `deleteMedicalTranscriptionJob`, `listMedicalTranscriptionJobs`.
//! * Call analytics jobs: `startCallAnalyticsJob` (`CamelAwsTranscribeCallAnalyticsJobName`, media,
//!   `CamelAwsTranscribeDataAccessRoleArn`, `CamelAwsTranscribeOutputLocation`,
//!   `CamelAwsTranscribeChannelDefinitions`), `getCallAnalyticsJob`, `deleteCallAnalyticsJob`,
//!   `listCallAnalyticsJobs`; categories `createCallAnalyticsCategory` /
//!   `updateCallAnalyticsCategory` (`CamelAwsTranscribeCategoryName`, `CamelAwsTranscribeRules` or a
//!   list body), `getCallAnalyticsCategory`, `deleteCallAnalyticsCategory`,
//!   `listCallAnalyticsCategories`.
//! * Custom vocabularies: `createVocabulary` / `updateVocabulary` (`CamelAwsTranscribeVocabularyName`,
//!   language code, `CamelAwsTranscribePhrases` list (or a list body) or
//!   `CamelAwsTranscribeVocabularyFileUri`), `getVocabulary`, `deleteVocabulary`,
//!   `listVocabularies` (`CamelAwsTranscribeNameContains`, `CamelAwsTranscribeStateEquals`); the
//!   medical variants `createMedicalVocabulary` / `updateMedicalVocabulary` (file URI required),
//!   `getMedicalVocabulary`, `deleteMedicalVocabulary`, `listMedicalVocabularies`.
//! * Custom language models: `createLanguageModel` (`CamelAwsTranscribeLanguageModelName`,
//!   `CamelAwsTranscribeBaseModelName` NarrowBand / WideBand, `CamelAwsTranscribeInputDataS3Uri`,
//!   data access role), `describeLanguageModel`, `deleteLanguageModel`, `listLanguageModels`.
//! * Tags: `tagResource` (`CamelAwsTranscribeResourceArn`, `CamelAwsTranscribeTags` map),
//!   `untagResource` (`CamelAwsTranscribeTagKeys` list), `listTagsForResource` (body = tag map).
//!
//! Get / start / create operations return the resource map as body and set
//! `CamelAwsTranscribeTranscriptionJobStatus` (job status, vocabulary state or model status).

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::{hdr, need, put, AwsConfig, JsonApi};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_i64, header_str, kinds, obj, Opts};

/// Header names (Camel `Transcribe2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsTranscribeOperation";
    pub const TRANSCRIPTION_JOB_NAME: &str = "CamelAwsTranscribeTranscriptionJobName";
    pub const LANGUAGE_CODE: &str = "CamelAwsTranscribeLanguageCode";
    pub const IDENTIFY_LANGUAGE: &str = "CamelAwsTranscribeIdentifyLanguage";
    pub const MEDIA_FORMAT: &str = "CamelAwsTranscribeMediaFormat";
    pub const MEDIA_URI: &str = "CamelAwsTranscribeMediaUri";
    pub const MEDIA_SAMPLE_RATE_HERTZ: &str = "CamelAwsTranscribeMediaSampleRateHertz";
    pub const OUTPUT_BUCKET_NAME: &str = "CamelAwsTranscribeOutputBucketName";
    pub const STATUS: &str = "CamelAwsTranscribeStatus";
    pub const JOB_NAME_CONTAINS: &str = "CamelAwsTranscribeJobNameContains";
    pub const TRANSCRIPTION_JOB_STATUS: &str = "CamelAwsTranscribeTranscriptionJobStatus";
    pub const MEDICAL_TRANSCRIPTION_JOB_NAME: &str = "CamelAwsTranscribeMedicalTranscriptionJobName";
    pub const SPECIALTY: &str = "CamelAwsTranscribeSpecialty";
    pub const TYPE: &str = "CamelAwsTranscribeType";
    pub const CALL_ANALYTICS_JOB_NAME: &str = "CamelAwsTranscribeCallAnalyticsJobName";
    pub const DATA_ACCESS_ROLE_ARN: &str = "CamelAwsTranscribeDataAccessRoleArn";
    pub const OUTPUT_LOCATION: &str = "CamelAwsTranscribeOutputLocation";
    pub const CHANNEL_DEFINITIONS: &str = "CamelAwsTranscribeChannelDefinitions";
    pub const CATEGORY_NAME: &str = "CamelAwsTranscribeCategoryName";
    pub const RULES: &str = "CamelAwsTranscribeRules";
    pub const VOCABULARY_NAME: &str = "CamelAwsTranscribeVocabularyName";
    pub const PHRASES: &str = "CamelAwsTranscribePhrases";
    pub const VOCABULARY_FILE_URI: &str = "CamelAwsTranscribeVocabularyFileUri";
    pub const NAME_CONTAINS: &str = "CamelAwsTranscribeNameContains";
    pub const STATE_EQUALS: &str = "CamelAwsTranscribeStateEquals";
    pub const LANGUAGE_MODEL_NAME: &str = "CamelAwsTranscribeLanguageModelName";
    pub const BASE_MODEL_NAME: &str = "CamelAwsTranscribeBaseModelName";
    pub const INPUT_DATA_S3_URI: &str = "CamelAwsTranscribeInputDataS3Uri";
    pub const RESOURCE_ARN: &str = "CamelAwsTranscribeResourceArn";
    pub const TAGS: &str = "CamelAwsTranscribeTags";
    pub const TAG_KEYS: &str = "CamelAwsTranscribeTagKeys";
}

const API: JsonApi = JsonApi { host: "transcribe", service: "transcribe", target: "Transcribe", version: "1.1", kind: kinds::TRANSCRIBE };

/// The Transcribe endpoint configuration and client.
pub struct TranscribeService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub language_code: Option<String>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["transcribeClient", "pojoRequest"]);
    Ok(Arc::new(TranscribeService { operation: o.get("operation"), language_code: o.get("languageCode"), aws }))
}

impl CloudService for TranscribeService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "");
        let call = |action: &str, body: Value| self.aws.api(&API, action, &body);
        let job = |r: &Value| r.get("TranscriptionJob").cloned().unwrap_or_default();
        let r = match op.as_str() {
            "startTranscriptionJob" => {
                let mut m = ObjectMap::new();
                m.insert("TranscriptionJobName".into(), Value::from(need(ex, headers::TRANSCRIPTION_JOB_NAME)?));
                let uri = header_str(ex, headers::MEDIA_URI).or_else(|| Some(ex.body().to_string()).filter(|s| !s.is_empty()));
                let uri = uri.ok_or_else(|| CamelError::illegal_argument("startTranscriptionJob requires CamelAwsTranscribeMediaUri (or the media URI as body)"))?;
                m.insert("Media".into(), obj([("MediaFileUri", Value::from(uri))]));
                if ex.header(headers::IDENTIFY_LANGUAGE).is_some_and(|v| v.is_truthy()) {
                    m.insert("IdentifyLanguage".into(), Value::Bool(true));
                } else {
                    m.insert("LanguageCode".into(), Value::from(hdr(ex, headers::LANGUAGE_CODE, &self.language_code).unwrap_or_else(|| "en-US".into())));
                }
                put(&mut m, "MediaFormat", header_str(ex, headers::MEDIA_FORMAT));
                put(&mut m, "MediaSampleRateHertz", header_i64(ex, headers::MEDIA_SAMPLE_RATE_HERTZ));
                put(&mut m, "OutputBucketName", header_str(ex, headers::OUTPUT_BUCKET_NAME));
                job(&call("StartTranscriptionJob", Value::Object(m))?)
            }
            "getTranscriptionJob" => job(&call("GetTranscriptionJob", obj([("TranscriptionJobName", Value::from(need(ex, headers::TRANSCRIPTION_JOB_NAME)?))]))?),
            "deleteTranscriptionJob" => {
                call("DeleteTranscriptionJob", obj([("TranscriptionJobName", Value::from(need(ex, headers::TRANSCRIPTION_JOB_NAME)?))]))?;
                return Ok(());
            }
            "listTranscriptionJobs" => {
                let mut m = ObjectMap::new();
                put(&mut m, "Status", header_str(ex, headers::STATUS));
                put(&mut m, "JobNameContains", header_str(ex, headers::JOB_NAME_CONTAINS));
                let all = self.aws.api_paged(&API, "ListTranscriptionJobs", m, "TranscriptionJobSummaries", "NextToken")?;
                ex.set_body(Value::Array(all));
                return Ok(());
            }
            other => match self.more(ex, other)? {
                Some(r) => r,
                None => return Ok(()),
            },
        };
        if let Some(s) = ["TranscriptionJobStatus", "VocabularyState", "ModelStatus", "CallAnalyticsJobStatus"].iter().find_map(|k| r.get(k)) {
            ex.set_header(headers::TRANSCRIPTION_JOB_STATUS, s.clone());
        }
        ex.set_body(r);
        Ok(())
    }
}

impl TranscribeService {
    fn media(ex: &Exchange, op: &str) -> Result<Value> {
        let uri = header_str(ex, headers::MEDIA_URI).or_else(|| Some(ex.body().to_string()).filter(|s| !s.is_empty()));
        let uri = uri.ok_or_else(|| CamelError::illegal_argument(format!("{} requires CamelAwsTranscribeMediaUri (or the media URI as body)", op)))?;
        Ok(obj([("MediaFileUri", Value::from(uri))]))
    }

    fn language(&self, ex: &Exchange) -> Value {
        Value::from(hdr(ex, headers::LANGUAGE_CODE, &self.language_code).unwrap_or_else(|| "en-US".into()))
    }

    fn list_value(ex: &Exchange, h: &str) -> Option<Value> {
        match ex.header(h) {
            Some(v) => Some(Value::from(crate::util::str_list(v))),
            None => match super::body_json(ex) {
                Some(Value::Array(a)) => Some(Value::Array(a)),
                _ => None,
            },
        }
    }

    /// The operations beyond plain transcription jobs. `Ok(None)`: done, body untouched.
    fn more(&self, ex: &mut Exchange, op: &str) -> Result<Option<Value>> {
        let call = |action: &str, body: ObjectMap| self.aws.api(&API, action, &Value::Object(body));
        let named = |key: &str, h: &str| -> Result<ObjectMap> {
            let mut m = ObjectMap::new();
            m.insert(key.into(), Value::from(need(ex, h)?));
            Ok(m)
        };
        let field = |r: Value, f: &str| r.get(f).cloned().unwrap_or(r);
        let list = |action: &str, mut m: ObjectMap, items: &str| -> Result<Value> {
            if action.ends_with("Jobs") {
                put(&mut m, "Status", header_str(ex, headers::STATUS));
                put(&mut m, "JobNameContains", header_str(ex, headers::JOB_NAME_CONTAINS));
            } else {
                put(&mut m, "NameContains", header_str(ex, headers::NAME_CONTAINS));
            }
            if action.ends_with("Vocabularies") {
                put(&mut m, "StateEquals", header_str(ex, headers::STATE_EQUALS));
            }
            if action == "ListLanguageModels" {
                put(&mut m, "StatusEquals", header_str(ex, headers::STATUS));
            }
            Ok(Value::Array(self.aws.api_paged(&API, action, m, items, "NextToken")?))
        };
        let mut r = match op {
            // medical transcription
            "startMedicalTranscriptionJob" => {
                let mut m = named("MedicalTranscriptionJobName", headers::MEDICAL_TRANSCRIPTION_JOB_NAME)?;
                m.insert("Media".into(), Self::media(ex, op)?);
                m.insert("LanguageCode".into(), self.language(ex));
                m.insert("OutputBucketName".into(), Value::from(need(ex, headers::OUTPUT_BUCKET_NAME)?));
                m.insert("Specialty".into(), Value::from(header_str(ex, headers::SPECIALTY).unwrap_or_else(|| "PRIMARYCARE".into())));
                m.insert("Type".into(), Value::from(header_str(ex, headers::TYPE).unwrap_or_else(|| "CONVERSATION".into())));
                put(&mut m, "MediaFormat", header_str(ex, headers::MEDIA_FORMAT));
                put(&mut m, "MediaSampleRateHertz", header_i64(ex, headers::MEDIA_SAMPLE_RATE_HERTZ));
                field(call("StartMedicalTranscriptionJob", m)?, "MedicalTranscriptionJob")
            }
            "getMedicalTranscriptionJob" => field(call("GetMedicalTranscriptionJob", named("MedicalTranscriptionJobName", headers::MEDICAL_TRANSCRIPTION_JOB_NAME)?)?, "MedicalTranscriptionJob"),
            "deleteMedicalTranscriptionJob" => {
                call("DeleteMedicalTranscriptionJob", named("MedicalTranscriptionJobName", headers::MEDICAL_TRANSCRIPTION_JOB_NAME)?)?;
                return Ok(None);
            }
            "listMedicalTranscriptionJobs" => list("ListMedicalTranscriptionJobs", ObjectMap::new(), "MedicalTranscriptionJobSummaries")?,
            // call analytics
            "startCallAnalyticsJob" => {
                let mut m = named("CallAnalyticsJobName", headers::CALL_ANALYTICS_JOB_NAME)?;
                m.insert("Media".into(), Self::media(ex, op)?);
                put(&mut m, "DataAccessRoleArn", header_str(ex, headers::DATA_ACCESS_ROLE_ARN));
                put(&mut m, "OutputLocation", header_str(ex, headers::OUTPUT_LOCATION));
                put(&mut m, "ChannelDefinitions", super::header_json(ex, headers::CHANNEL_DEFINITIONS));
                field(call("StartCallAnalyticsJob", m)?, "CallAnalyticsJob")
            }
            "getCallAnalyticsJob" => field(call("GetCallAnalyticsJob", named("CallAnalyticsJobName", headers::CALL_ANALYTICS_JOB_NAME)?)?, "CallAnalyticsJob"),
            "deleteCallAnalyticsJob" => {
                call("DeleteCallAnalyticsJob", named("CallAnalyticsJobName", headers::CALL_ANALYTICS_JOB_NAME)?)?;
                return Ok(None);
            }
            "listCallAnalyticsJobs" => list("ListCallAnalyticsJobs", ObjectMap::new(), "CallAnalyticsJobSummaries")?,
            "createCallAnalyticsCategory" | "updateCallAnalyticsCategory" => {
                let mut m = named("CategoryName", headers::CATEGORY_NAME)?;
                let rules = super::header_json(ex, headers::RULES).or_else(|| super::body_json(ex)).ok_or_else(|| CamelError::illegal_argument(format!("{} requires CamelAwsTranscribeRules (or a list body)", op)))?;
                m.insert("Rules".into(), rules);
                let action = if op.starts_with("create") { "CreateCallAnalyticsCategory" } else { "UpdateCallAnalyticsCategory" };
                field(call(action, m)?, "CategoryProperties")
            }
            "getCallAnalyticsCategory" => field(call("GetCallAnalyticsCategory", named("CategoryName", headers::CATEGORY_NAME)?)?, "CategoryProperties"),
            "deleteCallAnalyticsCategory" => {
                call("DeleteCallAnalyticsCategory", named("CategoryName", headers::CATEGORY_NAME)?)?;
                return Ok(None);
            }
            "listCallAnalyticsCategories" => Value::Array(self.aws.api_paged(&API, "ListCallAnalyticsCategories", ObjectMap::new(), "Categories", "NextToken")?),
            // vocabularies
            "createVocabulary" | "updateVocabulary" | "createMedicalVocabulary" | "updateMedicalVocabulary" => {
                let mut m = named("VocabularyName", headers::VOCABULARY_NAME)?;
                m.insert("LanguageCode".into(), self.language(ex));
                let medical = op.contains("Medical");
                match (header_str(ex, headers::VOCABULARY_FILE_URI), Self::list_value(ex, headers::PHRASES)) {
                    (Some(uri), _) => {
                        m.insert("VocabularyFileUri".into(), Value::from(uri));
                    }
                    (None, Some(p)) if !medical => {
                        m.insert("Phrases".into(), p);
                    }
                    _ => return Err(CamelError::illegal_argument(format!("{} requires CamelAwsTranscribeVocabularyFileUri{}", op, if medical { "" } else { " or CamelAwsTranscribePhrases" }))),
                }
                let mut action = op.to_string();
                action[..1].make_ascii_uppercase();
                call(&action, m)?
            }
            "getVocabulary" | "getMedicalVocabulary" => call(if op == "getVocabulary" { "GetVocabulary" } else { "GetMedicalVocabulary" }, named("VocabularyName", headers::VOCABULARY_NAME)?)?,
            "deleteVocabulary" | "deleteMedicalVocabulary" => {
                call(if op == "deleteVocabulary" { "DeleteVocabulary" } else { "DeleteMedicalVocabulary" }, named("VocabularyName", headers::VOCABULARY_NAME)?)?;
                return Ok(None);
            }
            "listVocabularies" | "listMedicalVocabularies" => list(if op == "listVocabularies" { "ListVocabularies" } else { "ListMedicalVocabularies" }, ObjectMap::new(), "Vocabularies")?,
            // language models
            "createLanguageModel" => {
                let mut m = named("ModelName", headers::LANGUAGE_MODEL_NAME)?;
                m.insert("LanguageCode".into(), self.language(ex));
                m.insert("BaseModelName".into(), Value::from(header_str(ex, headers::BASE_MODEL_NAME).unwrap_or_else(|| "WideBand".into())));
                let mut input = ObjectMap::new();
                input.insert("S3Uri".into(), Value::from(need(ex, headers::INPUT_DATA_S3_URI)?));
                input.insert("DataAccessRoleArn".into(), Value::from(need(ex, headers::DATA_ACCESS_ROLE_ARN)?));
                m.insert("InputDataConfig".into(), Value::Object(input));
                call("CreateLanguageModel", m)?
            }
            "describeLanguageModel" => field(call("DescribeLanguageModel", named("ModelName", headers::LANGUAGE_MODEL_NAME)?)?, "LanguageModel"),
            "deleteLanguageModel" => {
                call("DeleteLanguageModel", named("ModelName", headers::LANGUAGE_MODEL_NAME)?)?;
                return Ok(None);
            }
            "listLanguageModels" => list("ListLanguageModels", ObjectMap::new(), "Models")?,
            // tags
            "tagResource" => {
                let mut m = named("ResourceArn", headers::RESOURCE_ARN)?;
                let tags = match ex.header(headers::TAGS).or(Some(ex.body())) {
                    Some(Value::Object(t)) => t.iter().map(|(k, v)| obj([("Key", Value::from(k.clone())), ("Value", Value::from(v.to_string()))])).collect::<Vec<_>>(),
                    _ => return Err(CamelError::illegal_argument("tagResource requires the CamelAwsTranscribeTags map")),
                };
                m.insert("Tags".into(), Value::Array(tags));
                call("TagResource", m)?;
                return Ok(None);
            }
            "untagResource" => {
                let mut m = named("ResourceArn", headers::RESOURCE_ARN)?;
                m.insert("TagKeys".into(), Value::from(crate::util::header_list(ex, headers::TAG_KEYS)));
                call("UntagResource", m)?;
                return Ok(None);
            }
            "listTagsForResource" => {
                let r = call("ListTagsForResource", named("ResourceArn", headers::RESOURCE_ARN)?)?;
                let tags: ObjectMap = r.get("Tags").and_then(|t| t.as_array()).map(|a| a.iter().map(|t| (t.get("Key").map(|k| k.to_string()).unwrap_or_default(), t.get("Value").cloned().unwrap_or_default())).collect()).unwrap_or_default();
                Value::Object(tags)
            }
            other => return Err(unsupported("Transcribe", other)),
        };
        if let Value::Object(m) = &mut r {
            m.remove("ResponseMetadata");
        }
        Ok(Some(r))
    }
}
