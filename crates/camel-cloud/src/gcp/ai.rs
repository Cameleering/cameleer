//! Google AI components over REST with a service account (scope `cloud-platform`):
//!
//! **`google-vertexai:projectId:location:modelId`** (Camel `camel-google-vertexai`, producer;
//! Gemini `generateContent` on
//! `https://{location}-aiplatform.googleapis.com/v1/projects/{p}/locations/{l}/publishers/{publisher}/models/{model}`).
//! Operations (`operation` / `CamelGoogleVertexAIOperation`):
//! * `generateText` (default) / `generateChat` / `generateCode`: a text body is one user turn,
//!   a list body is the conversation (`{role, content}` or `{role, parts}` maps; role
//!   `assistant` maps to `model`), a map body is sent as the request. Generation config from
//!   `temperature`, `maxOutputTokens`, `topP`, `topK`, `candidateCount` options; system
//!   instruction from `systemInstruction` / `CamelGoogleVertexAISystemInstruction`.
//! * `generateMultimodal`: the body bytes (MIME type `CamelGoogleVertexAIMediaMimeType`,
//!   default `image/png`) or a `gs://` URI in `CamelGoogleVertexAIMediaGcsUri`, with the text
//!   prompt `CamelGoogleVertexAIPrompt`.
//! * `countTokens`: body = total token count.
//!
//! Results: body = the text of the first candidate; headers `CamelGoogleVertexAIFinishReason`,
//! `...PromptTokenCount`, `...CandidatesTokenCount`, `...TotalTokenCount`,
//! `...SafetyRatings`, `CamelGoogleVertexAIResponse` (raw response).
//!
//! **`google-speech-to-text://label`** (Speech-to-Text v1): `recognize` (default; body = audio
//! bytes, or a `gs://` URI text; `encoding` (default `LINEAR16`), `sampleRateHertz`,
//! `languageCode` (default `en-US`), `model`, `enableAutomaticPunctuation`; body = best
//! transcript (alternatives joined), `CamelGoogleSpeechToTextConfidence`,
//! `CamelGoogleSpeechToTextResults`), `longRunningRecognize` (body / header
//! `CamelGoogleSpeechToTextOperationName`), `getOperation` (`...OperationName`;
//! `CamelGoogleSpeechToTextDone`, body = transcript once done).
//!
//! **`google-text-to-speech://label`** (Text-to-Speech v1): `synthesize` (default; body text,
//! SSML when `textType=ssml` or the text starts with `<speak`; `languageCode` (default
//! `en-US`), `voiceName`, `ssmlGender`, `audioEncoding` (default `MP3`), `speakingRate`,
//! `pitch`; body = audio bytes, `CamelGoogleTextToSpeechAudioEncoding`), `listVoices` (body =
//! voices, `languageCode` filter).

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::GcpConfig;
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{base64_decode, base64_encode, header_str, kinds, obj, uri_encode, Opts};

/// Vertex AI header names.
pub mod vertex_headers {
    pub const OPERATION: &str = "CamelGoogleVertexAIOperation";
    pub const PROMPT: &str = "CamelGoogleVertexAIPrompt";
    pub const SYSTEM_INSTRUCTION: &str = "CamelGoogleVertexAISystemInstruction";
    pub const MEDIA_MIME_TYPE: &str = "CamelGoogleVertexAIMediaMimeType";
    pub const MEDIA_GCS_URI: &str = "CamelGoogleVertexAIMediaGcsUri";
    pub const FINISH_REASON: &str = "CamelGoogleVertexAIFinishReason";
    pub const PROMPT_TOKEN_COUNT: &str = "CamelGoogleVertexAIPromptTokenCount";
    pub const CANDIDATES_TOKEN_COUNT: &str = "CamelGoogleVertexAICandidatesTokenCount";
    pub const TOTAL_TOKEN_COUNT: &str = "CamelGoogleVertexAITotalTokenCount";
    pub const SAFETY_RATINGS: &str = "CamelGoogleVertexAISafetyRatings";
    pub const RESPONSE: &str = "CamelGoogleVertexAIResponse";
}

/// Speech header names.
pub mod speech_headers {
    pub const STT_OPERATION: &str = "CamelGoogleSpeechToTextOperation";
    pub const STT_CONFIDENCE: &str = "CamelGoogleSpeechToTextConfidence";
    pub const STT_RESULTS: &str = "CamelGoogleSpeechToTextResults";
    pub const STT_OPERATION_NAME: &str = "CamelGoogleSpeechToTextOperationName";
    pub const STT_DONE: &str = "CamelGoogleSpeechToTextDone";
    pub const TTS_OPERATION: &str = "CamelGoogleTextToSpeechOperation";
    pub const TTS_AUDIO_ENCODING: &str = "CamelGoogleTextToSpeechAudioEncoding";
}

fn opt_num(o: &mut Opts, k: &str) -> Result<Option<Value>> {
    match o.get(k) {
        None => Ok(None),
        Some(v) => {
            if let Ok(i) = v.parse::<i64>() {
                Ok(Some(Value::Int(i)))
            } else {
                v.parse::<f64>().map(|f| Some(Value::Float(f))).map_err(|_| CamelError::illegal_argument(format!("Invalid number for option {}: {}", k, v)))
            }
        }
    }
}

/// The Vertex AI endpoint configuration and client.
pub struct VertexAiService {
    pub gcp: GcpConfig,
    pub project: String,
    pub location: String,
    pub model: String,
    pub publisher: String,
    pub operation: Option<String>,
    pub system_instruction: Option<String>,
    generation: ObjectMap,
}

/// Factory for `google-vertexai`.
pub fn create_vertexai(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let parts: Vec<&str> = remaining.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return Err(CamelError::illegal_argument("Expected google-vertexai:projectId:location:modelId"));
    }
    let gcp = GcpConfig::from_opts(o)?;
    let mut generation = ObjectMap::new();
    for k in ["temperature", "maxOutputTokens", "topP", "topK", "candidateCount"] {
        if let Some(v) = opt_num(o, k)? {
            generation.insert(k.into(), v);
        }
    }
    Ok(Arc::new(VertexAiService {
        gcp,
        project: parts[0].into(),
        location: parts[1].into(),
        model: parts[2].into(),
        publisher: o.string("publisher", "google"),
        operation: o.get("operation"),
        system_instruction: o.get("systemInstruction"),
        generation,
    }))
}

fn turn(role: &str, parts: Value) -> Value {
    obj([("role", Value::from(if role == "assistant" { "model" } else { role })), ("parts", parts)])
}

fn text_parts(v: &Value) -> Value {
    match v {
        Value::Array(_) => v.clone(),
        other => Value::Array(vec![obj([("text", Value::from(other.to_string()))])]),
    }
}

impl VertexAiService {
    fn url(&self, verb: &str) -> String {
        format!(
            "{}/v1/projects/{}/locations/{}/publishers/{}/models/{}:{}",
            self.gcp.base(&format!("https://{}-aiplatform.googleapis.com", self.location)),
            uri_encode(&self.project, false),
            uri_encode(&self.location, false),
            uri_encode(&self.publisher, false),
            uri_encode(&self.model, false),
            verb
        )
    }

    fn request(&self, ex: &Exchange, op: &str) -> Result<Value> {
        let mut req = match (op, ex.body()) {
            (_, Value::Object(m)) if op != "generateMultimodal" => m.clone(),
            ("generateMultimodal", body) => {
                let mut parts = Vec::new();
                if let Some(p) = header_str(ex, vertex_headers::PROMPT) {
                    parts.push(obj([("text", Value::from(p))]));
                }
                let mime = header_str(ex, vertex_headers::MEDIA_MIME_TYPE).unwrap_or_else(|| "image/png".into());
                match header_str(ex, vertex_headers::MEDIA_GCS_URI) {
                    Some(uri) => parts.push(obj([("fileData", obj([("mimeType", Value::from(mime)), ("fileUri", Value::from(uri))]))])),
                    None => parts.push(obj([("inlineData", obj([("mimeType", Value::from(mime)), ("data", Value::from(base64_encode(&body.to_bytes())))]))])),
                }
                let mut m = ObjectMap::new();
                m.insert("contents".into(), Value::Array(vec![turn("user", Value::Array(parts))]));
                m
            }
            (_, Value::Array(msgs)) => {
                let contents: Vec<Value> = msgs
                    .iter()
                    .map(|msg| {
                        let role = msg.get("role").map(|r| r.to_string()).unwrap_or_else(|| "user".into());
                        let parts = msg.get("parts").cloned().unwrap_or_else(|| text_parts(msg.get("content").unwrap_or(msg)));
                        turn(&role, parts)
                    })
                    .collect();
                let mut m = ObjectMap::new();
                m.insert("contents".into(), Value::Array(contents));
                m
            }
            (_, other) => {
                let text = other.to_string();
                if text.trim().is_empty() {
                    return Err(CamelError::illegal_argument("The body must contain the prompt"));
                }
                let mut m = ObjectMap::new();
                m.insert("contents".into(), Value::Array(vec![turn("user", text_parts(&Value::from(text)))]));
                m
            }
        };
        if let Some(si) = header_str(ex, vertex_headers::SYSTEM_INSTRUCTION).or_else(|| self.system_instruction.clone()) {
            req.entry("systemInstruction".to_string()).or_insert_with(|| obj([("parts", text_parts(&Value::from(si)))]));
        }
        if !self.generation.is_empty() && op != "countTokens" {
            req.entry("generationConfig".to_string()).or_insert_with(|| Value::Object(self.generation.clone()));
        }
        if op == "countTokens" {
            req.remove("generationConfig");
        }
        Ok(Value::Object(req))
    }
}

impl CloudService for VertexAiService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, vertex_headers::OPERATION, &self.operation, "generateText");
        match op.as_str() {
            "generateText" | "generateChat" | "generateCode" | "generateContent" | "generateMultimodal" => {
                let req = self.request(ex, &op)?;
                let r = self.gcp.json("POST", &self.url("generateContent"), Some(&req), kinds::GOOGLE_API)?;
                let cand = r.get("candidates").and_then(|c| c.as_array()).and_then(|c| c.first().cloned()).unwrap_or_default();
                let text: String = cand
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                    .map(|ps| ps.iter().filter_map(|p| p.get("text").map(|t| t.to_string())).collect::<Vec<_>>().join(""))
                    .unwrap_or_default();
                ex.set_header(vertex_headers::FINISH_REASON, cand.get("finishReason").cloned().unwrap_or_default());
                ex.set_header(vertex_headers::SAFETY_RATINGS, cand.get("safetyRatings").cloned().unwrap_or(Value::Array(vec![])));
                let usage = r.get("usageMetadata").cloned().unwrap_or_default();
                for (k, h) in [("promptTokenCount", vertex_headers::PROMPT_TOKEN_COUNT), ("candidatesTokenCount", vertex_headers::CANDIDATES_TOKEN_COUNT), ("totalTokenCount", vertex_headers::TOTAL_TOKEN_COUNT)] {
                    if let Some(v) = usage.get(k) {
                        ex.set_header(h, v.clone());
                    }
                }
                ex.set_header(vertex_headers::RESPONSE, r.clone());
                ex.set_body(text);
            }
            "countTokens" => {
                let req = self.request(ex, &op)?;
                let r = self.gcp.json("POST", &self.url("countTokens"), Some(&req), kinds::GOOGLE_API)?;
                ex.set_body(r.get("totalTokens").cloned().unwrap_or_default());
            }
            other => return Err(unsupported("google-vertexai", other)),
        }
        Ok(())
    }
}

/// The Speech-to-Text endpoint.
pub struct SpeechToTextService {
    pub gcp: GcpConfig,
    pub operation: Option<String>,
    config: ObjectMap,
}

/// Factory for `google-speech-to-text`.
pub fn create_speech_to_text(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let gcp = GcpConfig::from_opts(o)?;
    let mut config = ObjectMap::new();
    config.insert("encoding".into(), Value::from(o.string("encoding", "LINEAR16")));
    config.insert("languageCode".into(), Value::from(o.string("languageCode", "en-US")));
    if let Some(v) = opt_num(o, "sampleRateHertz")? {
        config.insert("sampleRateHertz".into(), v);
    }
    if let Some(m) = o.get("model") {
        config.insert("model".into(), Value::from(m));
    }
    if o.bool("enableAutomaticPunctuation", false)? {
        config.insert("enableAutomaticPunctuation".into(), Value::Bool(true));
    }
    Ok(Arc::new(SpeechToTextService { gcp, operation: o.get("operation"), config }))
}

fn transcript(results: &Value) -> (String, Option<f64>) {
    let mut texts = Vec::new();
    let mut conf = None;
    for r in results.as_array().cloned().unwrap_or_default() {
        if let Some(alt) = r.get("alternatives").and_then(|a| a.as_array()).and_then(|a| a.first().cloned()) {
            texts.push(alt.get("transcript").map(|t| t.to_string()).unwrap_or_default());
            if conf.is_none() {
                conf = alt.get("confidence").and_then(|c| c.as_f64());
            }
        }
    }
    (texts.join(" ").trim().to_string(), conf)
}

impl SpeechToTextService {
    fn base(&self) -> String {
        self.gcp.base("https://speech.googleapis.com")
    }

    fn set_results(ex: &mut Exchange, results: Value) {
        let (text, conf) = transcript(&results);
        if let Some(c) = conf {
            ex.set_header(speech_headers::STT_CONFIDENCE, c);
        }
        ex.set_header(speech_headers::STT_RESULTS, results);
        ex.set_body(text);
    }
}

impl CloudService for SpeechToTextService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, speech_headers::STT_OPERATION, &self.operation, "recognize");
        match op.as_str() {
            "recognize" | "longRunningRecognize" => {
                let audio = match ex.body() {
                    Value::String(s) if s.starts_with("gs://") => obj([("uri", Value::from(s.clone()))]),
                    other => {
                        let b = other.to_bytes();
                        if b.is_empty() {
                            return Err(CamelError::illegal_argument("The body must contain the audio (bytes) or a gs:// URI"));
                        }
                        obj([("content", Value::from(base64_encode(&b)))])
                    }
                };
                let req = obj([("config", Value::Object(self.config.clone())), ("audio", audio)]);
                let verb = if op == "recognize" { "recognize" } else { "longrunningrecognize" };
                let r = self.gcp.json("POST", &format!("{}/v1/speech:{}", self.base(), verb), Some(&req), kinds::GOOGLE_API)?;
                if op == "recognize" {
                    Self::set_results(ex, r.get("results").cloned().unwrap_or(Value::Array(vec![])));
                } else {
                    let name = r.get("name").cloned().unwrap_or_default();
                    ex.set_header(speech_headers::STT_OPERATION_NAME, name.clone());
                    ex.set_body(name);
                }
            }
            "getOperation" => {
                let name = header_str(ex, speech_headers::STT_OPERATION_NAME).ok_or_else(|| CamelError::illegal_argument("getOperation requires CamelGoogleSpeechToTextOperationName"))?;
                let r = self.gcp.json("GET", &format!("{}/v1/operations/{}", self.base(), uri_encode(&name, true)), None, kinds::GOOGLE_API)?;
                let done = r.get("done").is_some_and(|d| d.is_truthy());
                ex.set_header(speech_headers::STT_DONE, done);
                if done {
                    if let Some(e) = r.get("error") {
                        return Err(crate::util::cloud_error(kinds::GOOGLE_API, format!("Recognition failed: {}", e.get("message").map(|m| m.to_string()).unwrap_or_default())));
                    }
                    Self::set_results(ex, r.get("response").and_then(|x| x.get("results")).cloned().unwrap_or(Value::Array(vec![])));
                } else {
                    ex.set_body(Value::Null);
                }
            }
            other => return Err(unsupported("google-speech-to-text", other)),
        }
        Ok(())
    }
}

/// The Text-to-Speech endpoint.
pub struct TextToSpeechService {
    pub gcp: GcpConfig,
    pub operation: Option<String>,
    pub text_type: Option<String>,
    voice: ObjectMap,
    audio: ObjectMap,
}

/// Factory for `google-text-to-speech`.
pub fn create_text_to_speech(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let gcp = GcpConfig::from_opts(o)?;
    let mut voice = ObjectMap::new();
    voice.insert("languageCode".into(), Value::from(o.string("languageCode", "en-US")));
    if let Some(n) = o.get("voiceName") {
        voice.insert("name".into(), Value::from(n));
    }
    if let Some(g) = o.get("ssmlGender") {
        voice.insert("ssmlGender".into(), Value::from(g));
    }
    let mut audio = ObjectMap::new();
    audio.insert("audioEncoding".into(), Value::from(o.string("audioEncoding", "MP3")));
    for k in ["speakingRate", "pitch", "sampleRateHertz"] {
        if let Some(v) = opt_num(o, k)? {
            audio.insert(k.into(), v);
        }
    }
    Ok(Arc::new(TextToSpeechService { gcp, operation: o.get("operation"), text_type: o.get("textType"), voice, audio }))
}

impl CloudService for TextToSpeechService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, speech_headers::TTS_OPERATION, &self.operation, "synthesize");
        let base = self.gcp.base("https://texttospeech.googleapis.com");
        match op.as_str() {
            "synthesize" => {
                let text = ex.body().to_string();
                if text.trim().is_empty() {
                    return Err(CamelError::illegal_argument("The body must contain the text to synthesize"));
                }
                let ssml = self.text_type.as_deref().map(|t| t.eq_ignore_ascii_case("ssml")).unwrap_or_else(|| text.trim_start().starts_with("<speak"));
                let input = obj([(if ssml { "ssml" } else { "text" }, Value::from(text))]);
                let req = obj([("input", input), ("voice", Value::Object(self.voice.clone())), ("audioConfig", Value::Object(self.audio.clone()))]);
                let r = self.gcp.json("POST", &format!("{}/v1/text:synthesize", base), Some(&req), kinds::GOOGLE_API)?;
                let audio = r.get("audioContent").and_then(|a| base64_decode(&a.to_string())).unwrap_or_default();
                ex.set_header(speech_headers::TTS_AUDIO_ENCODING, self.audio.get("audioEncoding").cloned().unwrap_or_default());
                ex.set_body(Value::Bytes(audio));
            }
            "listVoices" => {
                let lang = self.voice.get("languageCode").map(|l| l.to_string()).unwrap_or_default();
                let r = self.gcp.json("GET", &format!("{}/v1/voices?languageCode={}", base, uri_encode(&lang, false)), None, kinds::GOOGLE_API)?;
                ex.set_body(r.get("voices").cloned().unwrap_or(Value::Array(vec![])));
            }
            other => return Err(unsupported("google-text-to-speech", other)),
        }
        Ok(())
    }
}
