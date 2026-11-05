//! The `aws2-polly` component (Camel `camel-aws2-polly`, producer) over the Amazon Polly REST
//! API (signing name `polly`).
//!
//! `aws2-polly://label?operation=synthesizeSpeech&voiceId=Joanna&outputFormat=mp3`
//!
//! Operations (`operation` / `CamelAwsPollyOperation`):
//! * `synthesizeSpeech` (default): the body text (`textType` `text` / `ssml`) is spoken with
//!   `voiceId`, `outputFormat` (`mp3` default, `ogg_vorbis`, `pcm`, `json`), `engine`,
//!   `sampleRate`, `languageCode`, `lexiconNames` (options or `CamelAwsPolly*` headers); body =
//!   audio bytes, `CamelAwsPollyContentType`, `CamelAwsPollyRequestCharacters`.
//! * `describeVoices` (`engine`, `languageCode`): body = list of voices.
//! * `listLexicons`, `getLexicon` / `deleteLexicon` (`CamelAwsPollyLexiconName`),
//!   `putLexicon` (`CamelAwsPollyLexiconName`, body = PLS content).
//! * `startSpeechSynthesisTask` (`CamelAwsPollyS3Bucket`, optional `...S3KeyPrefix`): body =
//!   task map, `CamelAwsPollyTaskId`; `getSpeechSynthesisTask` (`CamelAwsPollyTaskId`).

use std::sync::Arc;

use cameleer_core::error::Result;
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::{hdr, need, put, AwsConfig};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_list, header_str, kinds, obj, uri_encode, Opts};

/// Header names (Camel `Polly2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsPollyOperation";
    pub const VOICE_ID: &str = "CamelAwsPollyVoiceId";
    pub const OUTPUT_FORMAT: &str = "CamelAwsPollyOutputFormat";
    pub const TEXT_TYPE: &str = "CamelAwsPollyTextType";
    pub const ENGINE: &str = "CamelAwsPollyEngine";
    pub const SAMPLE_RATE: &str = "CamelAwsPollySampleRate";
    pub const LANGUAGE_CODE: &str = "CamelAwsPollyLanguageCode";
    pub const LEXICON_NAMES: &str = "CamelAwsPollyLexiconNames";
    pub const LEXICON_NAME: &str = "CamelAwsPollyLexiconName";
    pub const CONTENT_TYPE: &str = "CamelAwsPollyContentType";
    pub const REQUEST_CHARACTERS: &str = "CamelAwsPollyRequestCharacters";
    pub const S3_BUCKET: &str = "CamelAwsPollyS3Bucket";
    pub const S3_KEY_PREFIX: &str = "CamelAwsPollyS3KeyPrefix";
    pub const TASK_ID: &str = "CamelAwsPollyTaskId";
}

/// The Polly endpoint configuration and client.
pub struct PollyService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub voice_id: Option<String>,
    pub output_format: Option<String>,
    pub text_type: Option<String>,
    pub engine: Option<String>,
    pub sample_rate: Option<String>,
    pub language_code: Option<String>,
    pub lexicon_names: Option<String>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["pollyClient", "pojoRequest"]);
    Ok(Arc::new(PollyService {
        operation: o.get("operation"),
        voice_id: o.get("voiceId"),
        output_format: o.get("outputFormat"),
        text_type: o.get("textType"),
        engine: o.get("engine"),
        sample_rate: o.get("sampleRate"),
        language_code: o.get("languageCode"),
        lexicon_names: o.get("lexiconNames"),
        aws,
    }))
}

impl PollyService {
    fn rest(&self, method: &str, path: &str, query: &[(String, String)], body: Option<&Value>) -> Result<Value> {
        self.aws.rest_json(method, "polly", path, query, body, "polly", kinds::POLLY)
    }

    fn speech_request(&self, ex: &Exchange) -> ObjectMap {
        let mut m = ObjectMap::new();
        m.insert("Text".into(), Value::from(ex.body().to_string()));
        m.insert("VoiceId".into(), Value::from(hdr(ex, headers::VOICE_ID, &self.voice_id).unwrap_or_else(|| "Joanna".into())));
        m.insert("OutputFormat".into(), Value::from(hdr(ex, headers::OUTPUT_FORMAT, &self.output_format).unwrap_or_else(|| "mp3".into())));
        put(&mut m, "TextType", hdr(ex, headers::TEXT_TYPE, &self.text_type));
        put(&mut m, "Engine", hdr(ex, headers::ENGINE, &self.engine));
        put(&mut m, "SampleRate", hdr(ex, headers::SAMPLE_RATE, &self.sample_rate));
        put(&mut m, "LanguageCode", hdr(ex, headers::LANGUAGE_CODE, &self.language_code));
        let lex = match header_list(ex, headers::LEXICON_NAMES) {
            l if !l.is_empty() => l,
            _ => self.lexicon_names.as_deref().map(|s| crate::util::str_list(&Value::from(s))).unwrap_or_default(),
        };
        if !lex.is_empty() {
            m.insert("LexiconNames".into(), Value::from(lex));
        }
        m
    }
}

impl CloudService for PollyService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "synthesizeSpeech");
        match op.as_str() {
            "synthesizeSpeech" => {
                let body = Value::Object(self.speech_request(ex));
                let resp = self.aws.rest_call("POST", "polly", "/v1/speech", &[], Some(("application/json", body.to_json().into_bytes())), &[], "polly", kinds::POLLY)?;
                ex.set_header(headers::CONTENT_TYPE, resp.header("Content-Type").unwrap_or("audio/mpeg").to_string());
                if let Some(n) = resp.header("x-amzn-RequestCharacters").and_then(|n| n.parse::<i64>().ok()) {
                    ex.set_header(headers::REQUEST_CHARACTERS, n);
                }
                ex.set_body(Value::Bytes(resp.body));
            }
            "describeVoices" => {
                let mut q = Vec::new();
                if let Some(e) = hdr(ex, headers::ENGINE, &self.engine) {
                    q.push(("Engine".to_string(), e));
                }
                if let Some(l) = hdr(ex, headers::LANGUAGE_CODE, &self.language_code) {
                    q.push(("LanguageCode".to_string(), l));
                }
                let r = self.rest("GET", "/v1/voices", &q, None)?;
                ex.set_body(r.get("Voices").cloned().unwrap_or(Value::Array(vec![])));
            }
            "listLexicons" => {
                let r = self.rest("GET", "/v1/lexicons", &[], None)?;
                ex.set_body(r.get("Lexicons").cloned().unwrap_or(Value::Array(vec![])));
            }
            "getLexicon" => {
                let name = need(ex, headers::LEXICON_NAME)?;
                let r = self.rest("GET", &format!("/v1/lexicons/{}", uri_encode(&name, false)), &[], None)?;
                ex.set_body(r);
            }
            "putLexicon" => {
                let name = need(ex, headers::LEXICON_NAME)?;
                self.rest("PUT", &format!("/v1/lexicons/{}", uri_encode(&name, false)), &[], Some(&obj([("Content", Value::from(ex.body().to_string()))])))?;
            }
            "deleteLexicon" => {
                let name = need(ex, headers::LEXICON_NAME)?;
                self.rest("DELETE", &format!("/v1/lexicons/{}", uri_encode(&name, false)), &[], None)?;
            }
            "startSpeechSynthesisTask" => {
                let mut m = self.speech_request(ex);
                m.insert("OutputS3BucketName".into(), Value::from(need(ex, headers::S3_BUCKET)?));
                put(&mut m, "OutputS3KeyPrefix", header_str(ex, headers::S3_KEY_PREFIX));
                let r = self.rest("POST", "/v1/synthesisTasks", &[], Some(&Value::Object(m)))?;
                let task = r.get("SynthesisTask").cloned().unwrap_or_default();
                if let Some(id) = task.get("TaskId") {
                    ex.set_header(headers::TASK_ID, id.clone());
                }
                ex.set_body(task);
            }
            "getSpeechSynthesisTask" => {
                let id = need(ex, headers::TASK_ID)?;
                let r = self.rest("GET", &format!("/v1/synthesisTasks/{}", uri_encode(&id, false)), &[], None)?;
                ex.set_body(r.get("SynthesisTask").cloned().unwrap_or_default());
            }
            other => return Err(unsupported("Polly", other)),
        }
        Ok(())
    }
}
