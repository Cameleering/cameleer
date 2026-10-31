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

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

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
            other => return Err(unsupported("Transcribe", other)),
        };
        if let Some(s) = r.get("TranscriptionJobStatus") {
            ex.set_header(headers::TRANSCRIPTION_JOB_STATUS, s.clone());
        }
        ex.set_body(r);
        Ok(())
    }
}
