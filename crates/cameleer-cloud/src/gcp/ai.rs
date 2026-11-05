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
//! * Streaming: `generateTextStreaming` / `generateChatStreaming` / `generateCodeStreaming`
//!   (`streamGenerateContent?alt=sse`, SSE decoded incrementally): body = the concatenated text
//!   (`streamOutputMode=complete`, default) or the list of chunks (`chunks`), headers
//!   `CamelGoogleVertexAIStreamingChunkCount`, finish reason, token counts; a
//!   `streamingChunkProcessor=#bean` processor gets each chunk.
//! * `generateEmbeddings` (text embedding models, `:predict` with `instances[].content`, optional
//!   `taskType` / `outputDimensionality`): a text body → one vector, a list body → vectors.
//! * `generateImage` (Imagen, `:predict` with the prompt, `sampleCount` (default 1),
//!   `aspectRatio`): body = list of image bytes.
//! * Partner models (`publisher=anthropic`, `meta`, `mistralai`...): `rawPredict` (the body map
//!   sent as is, e.g. an Anthropic Messages request with `anthropic_version`; body = the parsed
//!   response, the text of Anthropic `content` blocks in `CamelGoogleVertexAIResponseText`) and
//!   `streamRawPredict` (SSE: Anthropic `content_block_delta` text deltas or OpenAI-style
//!   `choices[].delta.content` chunks concatenated; `message_delta` stop reason / usage as
//!   headers). A text body is wrapped into an Anthropic Messages request (`anthropic_version`
//!   `vertex-2023-10-16`, `maxOutputTokens` → `max_tokens`, default 1024).
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

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

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
    pub const RESPONSE_TEXT: &str = "CamelGoogleVertexAIResponseText";
    pub const STREAMING_CHUNK_COUNT: &str = "CamelGoogleVertexAIStreamingChunkCount";
    pub const STREAMING_CHUNK_INDEX: &str = "CamelGoogleVertexAIStreamingChunkIndex";
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
    pub stream_output_mode: String,
    pub chunk_processor: Option<Arc<dyn cameleer_core::spi::Processor>>,
    pub predict_options: ObjectMap,
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
        stream_output_mode: o.string("streamOutputMode", "complete"),
        chunk_processor: match o.get("streamingChunkProcessor") {
            Some(r) => {
                let name = r.trim_start_matches('#').trim_start_matches("bean:").to_string();
                let p = o.context().registry().lookup::<Arc<dyn cameleer_core::spi::Processor>>(&name).ok_or_else(|| CamelError::no_such_bean(&name))?;
                Some((*p).clone())
            }
            None => None,
        },
        predict_options: {
            let mut m = ObjectMap::new();
            for k in ["taskType", "outputDimensionality", "sampleCount", "aspectRatio"] {
                if let Some(v) = o.get(k) {
                    m.insert(k.into(), v.parse::<i64>().map(Value::Int).unwrap_or(Value::from(v)));
                }
            }
            m
        },
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

impl VertexAiService {
    /// A partner model request: the body map as is, or a text wrapped into an Anthropic
    /// Messages request.
    fn raw_request(&self, ex: &Exchange) -> Value {
        match ex.body() {
            Value::Object(m) => Value::Object(m.clone()),
            other => {
                let mut m = ObjectMap::new();
                m.insert("anthropic_version".into(), Value::from("vertex-2023-10-16"));
                m.insert("max_tokens".into(), self.generation.get("maxOutputTokens").cloned().unwrap_or(Value::Int(1024)));
                if let Some(si) = header_str(ex, vertex_headers::SYSTEM_INSTRUCTION).or_else(|| self.system_instruction.clone()) {
                    m.insert("system".into(), Value::from(si));
                }
                m.insert("messages".into(), Value::Array(vec![obj([("role", Value::from("user")), ("content", Value::from(other.to_string()))])]));
                Value::Object(m)
            }
        }
    }

    fn chunk(&self, ctx: &Option<cameleer_core::context::CamelContext>, chunks: &mut Vec<String>, text: String, err: &mut Option<CamelError>) {
        if let Some(p) = &self.chunk_processor {
            let mut cex = Exchange::new(ctx.clone());
            cex.set_header(vertex_headers::STREAMING_CHUNK_INDEX, chunks.len() as i64);
            cex.set_body(text.clone());
            if let Err(e) = p.process(&mut cex) {
                err.get_or_insert(e);
            }
        }
        chunks.push(text);
    }

    fn finish_stream(&self, ex: &mut Exchange, chunks: Vec<String>) {
        ex.set_header(vertex_headers::STREAMING_CHUNK_COUNT, chunks.len() as i64);
        ex.set_body(if self.stream_output_mode.eq_ignore_ascii_case("chunks") { Value::from(chunks) } else { Value::from(chunks.concat()) });
    }

    fn stream_generate(&self, ex: &mut Exchange, req: &Value) -> Result<()> {
        let url = format!("{}?alt=sse", self.url("streamGenerateContent"));
        let request = crate::http::Request::new("POST", crate::http::Url::parse(&url)?).header("Content-Type", "application/json").header("Accept", "text/event-stream").body(req.to_json().into_bytes());
        let ctx = ex.context().ok().cloned();
        let mut chunks = Vec::new();
        let (mut finish, mut usage, mut perr) = (Value::Null, Value::Null, None);
        {
            let mut dec = crate::sse::SseDecoder::new(|e: crate::sse::SseEvent| {
                let v = crate::util::parse_json(e.data.as_bytes())?;
                if let Some(u) = v.get("usageMetadata") {
                    usage = u.clone();
                }
                let cand = v.get("candidates").and_then(|c| c.as_array()).and_then(|c| c.first().cloned()).unwrap_or_default();
                if let Some(f) = cand.get("finishReason") {
                    finish = f.clone();
                }
                let text: String = cand.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()).map(|ps| ps.iter().filter_map(|p| p.get("text").map(|t| t.to_string())).collect::<Vec<_>>().join("")).unwrap_or_default();
                if !text.is_empty() {
                    self.chunk(&ctx, &mut chunks, text, &mut perr);
                }
                Ok(())
            });
            let r = self.gcp.send_to(request, kinds::GOOGLE_API, &mut dec);
            if let Some(e) = dec.take_error() {
                return Err(e);
            }
            r?;
            dec.finish()?;
        }
        if let Some(e) = perr {
            return Err(e);
        }
        ex.set_header(vertex_headers::FINISH_REASON, finish);
        for (k, h) in [("promptTokenCount", vertex_headers::PROMPT_TOKEN_COUNT), ("candidatesTokenCount", vertex_headers::CANDIDATES_TOKEN_COUNT), ("totalTokenCount", vertex_headers::TOTAL_TOKEN_COUNT)] {
            if let Some(v) = usage.get(k) {
                ex.set_header(h, v.clone());
            }
        }
        self.finish_stream(ex, chunks);
        Ok(())
    }

    fn stream_raw(&self, ex: &mut Exchange, req: &Value) -> Result<()> {
        let mut body = req.as_object().cloned().unwrap_or_default();
        body.insert("stream".into(), Value::Bool(true));
        let request = crate::http::Request::new("POST", crate::http::Url::parse(&self.url("streamRawPredict"))?).header("Content-Type", "application/json").header("Accept", "text/event-stream").body(Value::Object(body).to_json().into_bytes());
        let ctx = ex.context().ok().cloned();
        let mut chunks = Vec::new();
        let (mut stop, mut usage, mut perr) = (Value::Null, Value::Null, None);
        {
            let mut dec = crate::sse::SseDecoder::new(|e: crate::sse::SseEvent| {
                if e.data.trim() == "[DONE]" {
                    return Ok(());
                }
                let v = crate::util::parse_json(e.data.as_bytes())?;
                let kind = v.get("type").map(|t| t.to_string()).or(e.event.clone()).unwrap_or_default();
                match kind.as_str() {
                    "content_block_delta" => {
                        if let Some(t) = v.get("delta").and_then(|d| d.get("text")) {
                            self.chunk(&ctx, &mut chunks, t.to_string(), &mut perr);
                        }
                    }
                    "message_delta" => {
                        if let Some(sr) = v.get("delta").and_then(|d| d.get("stop_reason")) {
                            stop = sr.clone();
                        }
                        if let Some(u) = v.get("usage") {
                            usage = u.clone();
                        }
                    }
                    "error" => return Err(crate::util::cloud_error(kinds::GOOGLE_API, format!("Streaming error: {}", v.get("error").map(|x| x.to_json()).unwrap_or_default()))),
                    _ => {
                        for c in v.get("choices").and_then(|c| c.as_array()).cloned().unwrap_or_default() {
                            if let Some(t) = c.get("delta").and_then(|d| d.get("content")).filter(|t| !t.is_null()) {
                                self.chunk(&ctx, &mut chunks, t.to_string(), &mut perr);
                            }
                            if let Some(f) = c.get("finish_reason").filter(|f| !f.is_null()) {
                                stop = f.clone();
                            }
                        }
                    }
                }
                Ok(())
            });
            let r = self.gcp.send_to(request, kinds::GOOGLE_API, &mut dec);
            if let Some(e) = dec.take_error() {
                return Err(e);
            }
            r?;
            dec.finish()?;
        }
        if let Some(e) = perr {
            return Err(e);
        }
        ex.set_header(vertex_headers::FINISH_REASON, stop);
        ex.set_header(vertex_headers::RESPONSE, usage);
        self.finish_stream(ex, chunks);
        Ok(())
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
            "generateTextStreaming" | "generateChatStreaming" | "generateCodeStreaming" => {
                let req = self.request(ex, op.trim_end_matches("Streaming"))?;
                self.stream_generate(ex, &req)?;
            }
            "generateEmbeddings" => {
                let (items, single) = match ex.body() {
                    Value::Array(a) => (a.iter().map(|x| x.to_string()).collect::<Vec<_>>(), false),
                    other => (vec![other.to_string()], true),
                };
                let instances: Vec<Value> = items
                    .iter()
                    .map(|t| {
                        let mut m = ObjectMap::new();
                        m.insert("content".into(), Value::from(t.clone()));
                        if let Some(tt) = self.predict_options.get("taskType") {
                            m.insert("task_type".into(), tt.clone());
                        }
                        Value::Object(m)
                    })
                    .collect();
                let mut params = ObjectMap::new();
                if let Some(d) = self.predict_options.get("outputDimensionality") {
                    params.insert("outputDimensionality".into(), d.clone());
                }
                let r = self.gcp.json("POST", &self.url("predict"), Some(&obj([("instances", Value::Array(instances)), ("parameters", Value::Object(params))])), kinds::GOOGLE_API)?;
                let vectors: Vec<Value> = r.get("predictions").and_then(|p| p.as_array()).cloned().unwrap_or_default().iter().map(|p| p.get("embeddings").and_then(|e| e.get("values")).cloned().unwrap_or_default()).collect();
                ex.set_body(if single { vectors.into_iter().next().unwrap_or_default() } else { Value::Array(vectors) });
            }
            "generateImage" => {
                let mut params = ObjectMap::new();
                params.insert("sampleCount".into(), self.predict_options.get("sampleCount").cloned().unwrap_or(Value::Int(1)));
                if let Some(a) = self.predict_options.get("aspectRatio") {
                    params.insert("aspectRatio".into(), a.clone());
                }
                let r = self.gcp.json("POST", &self.url("predict"), Some(&obj([("instances", Value::Array(vec![obj([("prompt", Value::from(ex.body().to_string()))])])), ("parameters", Value::Object(params))])), kinds::GOOGLE_API)?;
                let images: Vec<Value> = r.get("predictions").and_then(|p| p.as_array()).cloned().unwrap_or_default().iter().map(|p| Value::Bytes(p.get("bytesBase64Encoded").and_then(|b| base64_decode(&b.to_string())).unwrap_or_default())).collect();
                ex.set_body(Value::Array(images));
            }
            "rawPredict" => {
                let req = self.raw_request(ex);
                let r = self.gcp.json("POST", &self.url("rawPredict"), Some(&req), kinds::GOOGLE_API)?;
                let text: String = r.get("content").and_then(|c| c.as_array()).map(|blocks| blocks.iter().filter_map(|b| b.get("text").map(|t| t.to_string())).collect::<Vec<_>>().join("")).unwrap_or_default();
                ex.set_header(vertex_headers::RESPONSE_TEXT, text);
                if let Some(sr) = r.get("stop_reason") {
                    ex.set_header(vertex_headers::FINISH_REASON, sr.clone());
                }
                ex.set_body(r);
            }
            "streamRawPredict" => {
                let req = self.raw_request(ex);
                self.stream_raw(ex, &req)?;
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
