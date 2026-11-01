//! The `azure-openai` component (rust-camel, producer; Camel offers Azure OpenAI through
//! LangChain4j) over the Azure OpenAI REST API.
//!
//! `azure-openai://resource/deployment?apiKey=RAW(...)&operation=chatCompletion`
//!
//! Base URL `https://{resource}.openai.azure.com` (or `uriEndpointOverride`), path
//! `/openai/deployments/{deployment}/...?api-version={apiVersion}` (default `2024-06-01`).
//! Authorization: `apiKey` (`api-key` header) or Entra ID client credentials (`tenantId`,
//! `clientId`, `clientSecret`; scope `https://cognitiveservices.azure.com/.default`).
//!
//! Operations (`operation` / `CamelAzureOpenAIOperation`):
//! * `chatCompletion` (default): a text body becomes a user message (preceded by
//!   `systemMessage` / `CamelAzureOpenAISystemMessage`), a list body is the message list
//!   (`{role, content}` maps), a map body is sent as the request. `temperature`, `maxTokens`,
//!   `topP` options. Body = the assistant message content, headers
//!   `CamelAzureOpenAIFinishReason`, `CamelAzureOpenAIUsage` (map), `CamelAzureOpenAIModel`,
//!   `CamelAzureOpenAIToolCalls` (when the model called tools).
//! * `embeddings`: a text body → one vector (list of floats); a list body → list of vectors.
//! * Streaming chat (`streaming=true` / `CamelAzureOpenAIStreaming`, or operation
//!   `chatCompletionStream`): `stream: true` with `stream_options.include_usage`; the
//!   `text/event-stream` response is decoded incrementally (`data:` chunks up to `[DONE]`):
//!   body = the concatenated deltas (`streamOutputMode=complete`, default) or the list of deltas
//!   (`chunks`), headers `CamelAzureOpenAIStreamingChunkCount`, finish reason, usage, model; a
//!   `streamingChunkProcessor=#bean` processor is called per delta (body = the delta text,
//!   `CamelAzureOpenAIStreamingChunkIndex`).
//! * Images: `imageGeneration` (DALL-E: body = the prompt, `imageSize` / `CamelAzureOpenAIImageSize`
//!   default `1024x1024`, `imageCount` (`n`), `imageQuality`, `imageResponseFormat` `url` /
//!   `b64_json`; body = list of URLs or of decoded image bytes, `CamelAzureOpenAIRevisedPrompts`).
//! * Assistants (`/openai/assistants`, `/openai/threads`, `assistantsApiVersion` default
//!   `2024-05-01-preview`): `createAssistant` (body = instructions or a request map,
//!   `CamelAzureOpenAIAssistantName`; the deployment is the model; sets
//!   `CamelAzureOpenAIAssistantId`), `getAssistant`, `listAssistants`, `deleteAssistant`,
//!   `createThread` (sets `CamelAzureOpenAIThreadId`), `createMessage` (body = the user text),
//!   `listMessages` (body = messages, newest first), `createRun` (sets `CamelAzureOpenAIRunId`),
//!   `getRun` (body = the run, `CamelAzureOpenAIRunStatus`), and `runAssistant` (thread +
//!   message + run, polled every `runPollInterval` ms until a terminal status within
//!   `runTimeout`; body = the assistant's reply text).

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::rest::{aad_credential, AzureAuth, AzureRest};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_str, kinds, obj, uri_encode, Opts};

/// Header names.
pub mod headers {
    pub const OPERATION: &str = "CamelAzureOpenAIOperation";
    pub const SYSTEM_MESSAGE: &str = "CamelAzureOpenAISystemMessage";
    pub const FINISH_REASON: &str = "CamelAzureOpenAIFinishReason";
    pub const USAGE: &str = "CamelAzureOpenAIUsage";
    pub const MODEL: &str = "CamelAzureOpenAIModel";
    pub const TOOL_CALLS: &str = "CamelAzureOpenAIToolCalls";
    pub const STREAMING: &str = "CamelAzureOpenAIStreaming";
    pub const STREAMING_CHUNK_COUNT: &str = "CamelAzureOpenAIStreamingChunkCount";
    pub const STREAMING_CHUNK_INDEX: &str = "CamelAzureOpenAIStreamingChunkIndex";
    pub const IMAGE_SIZE: &str = "CamelAzureOpenAIImageSize";
    pub const REVISED_PROMPTS: &str = "CamelAzureOpenAIRevisedPrompts";
    pub const ASSISTANT_ID: &str = "CamelAzureOpenAIAssistantId";
    pub const ASSISTANT_NAME: &str = "CamelAzureOpenAIAssistantName";
    pub const THREAD_ID: &str = "CamelAzureOpenAIThreadId";
    pub const RUN_ID: &str = "CamelAzureOpenAIRunId";
    pub const RUN_STATUS: &str = "CamelAzureOpenAIRunStatus";
}

/// Entra ID scope.
pub const SCOPE: &str = "https://cognitiveservices.azure.com/.default";

/// The Azure OpenAI endpoint configuration and client.
pub struct OpenAiService {
    pub rest: AzureRest,
    pub deployment: String,
    pub api_version: String,
    pub operation: Option<String>,
    pub system_message: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    pub top_p: Option<f64>,
    pub streaming: bool,
    pub stream_output_mode: String,
    pub chunk_processor: Option<Arc<dyn camel_core::spi::Processor>>,
    pub image_size: String,
    pub image_count: Option<i64>,
    pub image_quality: Option<String>,
    pub image_response_format: String,
    pub assistants_api_version: String,
    pub run_poll_interval: u64,
    pub run_timeout: u64,
}

fn opt_f64(o: &mut Opts, k: &str) -> Result<Option<f64>> {
    o.get(k).map(|v| v.parse::<f64>().map_err(|_| CamelError::illegal_argument(format!("Invalid number for option {}: {}", k, v)))).transpose()
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let (resource, deployment) = remaining
        .split_once('/')
        .filter(|(r, d)| !r.is_empty() && !d.is_empty())
        .ok_or_else(|| CamelError::illegal_argument("Expected azure-openai://resource/deployment"))?;
    let identity = o.string("credentialType", "").eq_ignore_ascii_case("AZURE_IDENTITY");
    let auth = match o.get("apiKey") {
        Some(k) => AzureAuth::Header("api-key".into(), k),
        None => AzureAuth::Bearer(
            aad_credential(o, identity)?.ok_or_else(|| CamelError::illegal_argument("azure-openai requires apiKey or Entra ID client credentials (tenantId / clientId / clientSecret)"))?,
            SCOPE.into(),
        ),
    };
    let rest = AzureRest::from_opts(o, &format!("https://{}.openai.azure.com", resource), auth, kinds::AZURE_HTTP)?;
    Ok(Arc::new(OpenAiService {
        rest,
        deployment: deployment.to_string(),
        api_version: o.string("apiVersion", "2024-06-01"),
        operation: o.get("operation"),
        system_message: o.get("systemMessage"),
        temperature: opt_f64(o, "temperature")?,
        max_tokens: crate::util::opt_i64(o, "maxTokens")?,
        top_p: opt_f64(o, "topP")?,
        streaming: o.bool("streaming", false)?,
        stream_output_mode: o.string("streamOutputMode", "complete"),
        chunk_processor: match o.get("streamingChunkProcessor") {
            Some(r) => {
                let name = r.trim_start_matches('#').trim_start_matches("bean:").to_string();
                let p = o.context().registry().lookup::<Arc<dyn camel_core::spi::Processor>>(&name).ok_or_else(|| CamelError::no_such_bean(&name))?;
                Some((*p).clone())
            }
            None => None,
        },
        image_size: o.string("imageSize", "1024x1024"),
        image_count: crate::util::opt_i64(o, "imageCount")?,
        image_quality: o.get("imageQuality"),
        image_response_format: o.string("imageResponseFormat", "url"),
        assistants_api_version: o.string("assistantsApiVersion", "2024-05-01-preview"),
        run_poll_interval: o.i64("runPollInterval", 200)?.max(10) as u64,
        run_timeout: o.i64("runTimeout", 60_000)?.max(1) as u64,
    }))
}

impl OpenAiService {
    fn post(&self, what: &str, body: &Value) -> Result<Value> {
        self.rest.json("POST", &format!("/openai/deployments/{}/{}", uri_encode(&self.deployment, false), what), &format!("api-version={}", uri_encode(&self.api_version, false)), Some(body))
    }
}

impl CloudService for OpenAiService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "chatCompletion");
        let streaming = op == "chatCompletionStream" || ex.header(headers::STREAMING).map(|v| v.is_truthy()).unwrap_or(self.streaming && op == "chatCompletion");
        match op.as_str() {
            "chatCompletion" | "chatCompletionStream" => {
                let mut req = match ex.body() {
                    Value::Object(m) => m.clone(),
                    Value::Array(msgs) => {
                        let mut m = ObjectMap::new();
                        m.insert("messages".into(), Value::Array(msgs.clone()));
                        m
                    }
                    other => {
                        let mut msgs = Vec::new();
                        if let Some(s) = header_str(ex, headers::SYSTEM_MESSAGE).or_else(|| self.system_message.clone()) {
                            msgs.push(obj([("role", Value::from("system")), ("content", Value::from(s))]));
                        }
                        msgs.push(obj([("role", Value::from("user")), ("content", Value::from(other.to_string()))]));
                        let mut m = ObjectMap::new();
                        m.insert("messages".into(), Value::Array(msgs));
                        m
                    }
                };
                for (k, v) in [("temperature", self.temperature.map(Value::Float)), ("max_tokens", self.max_tokens.map(Value::Int)), ("top_p", self.top_p.map(Value::Float))] {
                    if let Some(v) = v {
                        req.entry(k.to_string()).or_insert(v);
                    }
                }
                if streaming {
                    return self.stream_chat(ex, req);
                }
                let r = self.post("chat/completions", &Value::Object(req))?;
                let choice = r.get("choices").and_then(|c| c.as_array()).and_then(|c| c.first().cloned()).unwrap_or_default();
                let msg = choice.get("message").cloned().unwrap_or_default();
                ex.set_header(headers::FINISH_REASON, choice.get("finish_reason").cloned().unwrap_or_default());
                ex.set_header(headers::USAGE, r.get("usage").cloned().unwrap_or_default());
                ex.set_header(headers::MODEL, r.get("model").cloned().unwrap_or_default());
                if let Some(tc) = msg.get("tool_calls").filter(|t| !t.is_null()) {
                    ex.set_header(headers::TOOL_CALLS, tc.clone());
                }
                ex.set_body(msg.get("content").cloned().unwrap_or_default());
            }
            "embeddings" => {
                let (input, single) = match ex.body() {
                    Value::Array(a) => (Value::Array(a.clone()), false),
                    other => (Value::from(other.to_string()), true),
                };
                let r = self.post("embeddings", &obj([("input", input)]))?;
                let mut data = r.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
                data.sort_by_key(|d| d.get("index").and_then(|i| i.as_i64()).unwrap_or(0));
                let vectors: Vec<Value> = data.iter().map(|d| d.get("embedding").cloned().unwrap_or_default()).collect();
                ex.set_header(headers::USAGE, r.get("usage").cloned().unwrap_or_default());
                ex.set_body(if single { vectors.into_iter().next().unwrap_or_default() } else { Value::Array(vectors) });
            }
            other => {
                if !self.more_ops(ex, other)? {
                    return Err(unsupported("Azure OpenAI", other));
                }
            }
        }
        Ok(())
    }
}

impl OpenAiService {
    fn stream_chat(&self, ex: &mut Exchange, mut req: ObjectMap) -> Result<()> {
        req.insert("stream".into(), Value::Bool(true));
        req.insert("stream_options".into(), obj([("include_usage", Value::Bool(true))]));
        let url = self.rest.url(&format!("/openai/deployments/{}/chat/completions", uri_encode(&self.deployment, false)), &format!("api-version={}", uri_encode(&self.api_version, false)))?;
        let request = crate::http::Request::new("POST", url).header("Content-Type", "application/json").header("Accept", "text/event-stream").body(Value::Object(req).to_json().into_bytes());
        let mut chunks: Vec<String> = Vec::new();
        let (mut finish, mut usage, mut model, mut done) = (Value::Null, Value::Null, Value::Null, false);
        let ctx = ex.context().ok().cloned();
        let processor = self.chunk_processor.clone();
        let mut proc_err: Option<CamelError> = None;
        {
            let mut decoder = crate::sse::SseDecoder::new(|e: crate::sse::SseEvent| {
                if e.data.trim() == "[DONE]" {
                    done = true;
                    return Ok(());
                }
                let v = crate::util::parse_json(e.data.as_bytes())?;
                if let Some(err) = v.get("error") {
                    return Err(crate::util::cloud_error(kinds::AZURE_HTTP, format!("Streaming error: {}", err.get("message").map(|m| m.to_string()).unwrap_or_default())));
                }
                if let Some(m) = v.get("model").filter(|m| !m.is_null() && !m.to_string().is_empty()) {
                    model = m.clone();
                }
                if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                    usage = u.clone();
                }
                for c in v.get("choices").and_then(|c| c.as_array()).cloned().unwrap_or_default() {
                    if let Some(f) = c.get("finish_reason").filter(|f| !f.is_null()) {
                        finish = f.clone();
                    }
                    if let Some(t) = c.get("delta").and_then(|d| d.get("content")).filter(|t| !t.is_null()) {
                        let text = t.to_string();
                        if let Some(p) = &processor {
                            let mut cex = Exchange::new(ctx.clone());
                            cex.set_header(headers::STREAMING_CHUNK_INDEX, chunks.len() as i64);
                            cex.set_body(text.clone());
                            if let Err(e) = p.process(&mut cex) {
                                proc_err.get_or_insert(e);
                            }
                        }
                        chunks.push(text);
                    }
                }
                Ok(())
            });
            let res = self.rest.send_to(request, &mut decoder);
            if let Some(e) = decoder.take_error() {
                return Err(e);
            }
            res?;
            decoder.finish()?;
        }
        if let Some(e) = proc_err {
            return Err(e);
        }
        if !done {
            camel_core::camel_debug!("azure-openai", "Stream ended without [DONE]");
        }
        ex.set_header(headers::STREAMING_CHUNK_COUNT, chunks.len() as i64);
        ex.set_header(headers::FINISH_REASON, finish);
        ex.set_header(headers::USAGE, usage);
        ex.set_header(headers::MODEL, model);
        ex.set_body(if self.stream_output_mode.eq_ignore_ascii_case("chunks") { Value::from(chunks) } else { Value::from(chunks.concat()) });
        Ok(())
    }

    fn assistants(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut req = crate::http::Request::new(method, self.rest.url(&format!("/openai/{}", path), &format!("api-version={}", uri_encode(&self.assistants_api_version, false)))?);
        if let Some(b) = body {
            req = req.header("Content-Type", "application/json").body(b.to_json().into_bytes());
        }
        let r = self.rest.send(req)?;
        if r.body.is_empty() {
            return Ok(Value::Null);
        }
        crate::util::parse_json(&r.body)
    }

    fn need(ex: &Exchange, h: &str, what: &str) -> Result<String> {
        header_str(ex, h).ok_or_else(|| CamelError::illegal_argument(format!("{} requires the {} header", what, h)))
    }

    fn run_and_wait(&self, ex: &mut Exchange, thread: &str, assistant: &str) -> Result<Value> {
        let run = self.assistants("POST", &format!("threads/{}/runs", thread), Some(&obj([("assistant_id", Value::from(assistant))])))?;
        let run_id = run.get("id").map(|i| i.to_string()).unwrap_or_default();
        ex.set_header(headers::RUN_ID, run_id.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(self.run_timeout);
        loop {
            let r = self.assistants("GET", &format!("threads/{}/runs/{}", thread, run_id), None)?;
            let status = r.get("status").map(|s| s.to_string()).unwrap_or_default();
            ex.set_header(headers::RUN_STATUS, status.clone());
            if matches!(status.as_str(), "completed" | "failed" | "cancelled" | "expired" | "requires_action" | "incomplete") {
                return Ok(r);
            }
            if std::time::Instant::now() >= deadline {
                return Err(CamelError::new(camel_core::error::kinds::TIMEOUT, format!("Assistant run {} did not finish within {} ms", run_id, self.run_timeout)));
            }
            std::thread::sleep(std::time::Duration::from_millis(self.run_poll_interval));
        }
    }

    fn message_text(m: &Value) -> String {
        m.get("content").and_then(|c| c.as_array()).map(|parts| parts.iter().filter_map(|p| p.get("text").and_then(|t| t.get("value")).map(|v| v.to_string())).collect::<Vec<_>>().join("")).unwrap_or_default()
    }

    /// Images and assistants; false when `op` is not one of them.
    fn more_ops(&self, ex: &mut Exchange, op: &str) -> Result<bool> {
        match op {
            "imageGeneration" => {
                let mut m = ObjectMap::new();
                m.insert("prompt".into(), Value::from(ex.body().to_string()));
                m.insert("size".into(), Value::from(header_str(ex, headers::IMAGE_SIZE).unwrap_or_else(|| self.image_size.clone())));
                m.insert("response_format".into(), Value::from(self.image_response_format.clone()));
                if let Some(n) = self.image_count {
                    m.insert("n".into(), Value::Int(n));
                }
                if let Some(q) = &self.image_quality {
                    m.insert("quality".into(), Value::from(q.clone()));
                }
                let r = self.post("images/generations", &Value::Object(m))?;
                let data = r.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
                let revised: Vec<Value> = data.iter().filter_map(|d| d.get("revised_prompt").cloned()).collect();
                ex.set_header(headers::REVISED_PROMPTS, Value::Array(revised));
                let out: Vec<Value> = data
                    .iter()
                    .map(|d| match d.get("b64_json") {
                        Some(b) => Value::Bytes(crate::util::base64_decode(&b.to_string()).unwrap_or_default()),
                        None => d.get("url").cloned().unwrap_or_default(),
                    })
                    .collect();
                ex.set_body(Value::Array(out));
            }
            "createAssistant" => {
                let body = match ex.body() {
                    Value::Object(m) => {
                        let mut m = m.clone();
                        m.entry("model".to_string()).or_insert(Value::from(self.deployment.clone()));
                        Value::Object(m)
                    }
                    other => {
                        let mut m = ObjectMap::new();
                        m.insert("model".into(), Value::from(self.deployment.clone()));
                        m.insert("instructions".into(), Value::from(other.to_string()));
                        if let Some(n) = header_str(ex, headers::ASSISTANT_NAME) {
                            m.insert("name".into(), Value::from(n));
                        }
                        Value::Object(m)
                    }
                };
                let r = self.assistants("POST", "assistants", Some(&body))?;
                ex.set_header(headers::ASSISTANT_ID, r.get("id").cloned().unwrap_or_default());
                ex.set_body(r);
            }
            "getAssistant" | "deleteAssistant" => {
                let id = Self::need(ex, headers::ASSISTANT_ID, op)?;
                let r = self.assistants(if op == "getAssistant" { "GET" } else { "DELETE" }, &format!("assistants/{}", uri_encode(&id, false)), None)?;
                ex.set_body(r);
            }
            "listAssistants" => {
                let r = self.assistants("GET", "assistants", None)?;
                ex.set_body(r.get("data").cloned().unwrap_or(Value::Array(vec![])));
            }
            "createThread" => {
                let r = self.assistants("POST", "threads", Some(&obj([])))?;
                ex.set_header(headers::THREAD_ID, r.get("id").cloned().unwrap_or_default());
                ex.set_body(r);
            }
            "createMessage" => {
                let t = Self::need(ex, headers::THREAD_ID, op)?;
                let r = self.assistants("POST", &format!("threads/{}/messages", t), Some(&obj([("role", Value::from("user")), ("content", Value::from(ex.body().to_string()))])))?;
                ex.set_body(r);
            }
            "listMessages" => {
                let t = Self::need(ex, headers::THREAD_ID, op)?;
                let r = self.assistants("GET", &format!("threads/{}/messages", t), None)?;
                ex.set_body(r.get("data").cloned().unwrap_or(Value::Array(vec![])));
            }
            "createRun" => {
                let (t, a) = (Self::need(ex, headers::THREAD_ID, op)?, Self::need(ex, headers::ASSISTANT_ID, op)?);
                let r = self.assistants("POST", &format!("threads/{}/runs", t), Some(&obj([("assistant_id", Value::from(a))])))?;
                ex.set_header(headers::RUN_ID, r.get("id").cloned().unwrap_or_default());
                ex.set_header(headers::RUN_STATUS, r.get("status").cloned().unwrap_or_default());
                ex.set_body(r);
            }
            "getRun" => {
                let (t, run) = (Self::need(ex, headers::THREAD_ID, op)?, Self::need(ex, headers::RUN_ID, op)?);
                let r = self.assistants("GET", &format!("threads/{}/runs/{}", t, run), None)?;
                ex.set_header(headers::RUN_STATUS, r.get("status").cloned().unwrap_or_default());
                ex.set_body(r);
            }
            "runAssistant" => {
                let a = Self::need(ex, headers::ASSISTANT_ID, op)?;
                let t = match header_str(ex, headers::THREAD_ID) {
                    Some(t) => t,
                    None => {
                        let r = self.assistants("POST", "threads", Some(&obj([])))?;
                        let t = r.get("id").map(|i| i.to_string()).unwrap_or_default();
                        ex.set_header(headers::THREAD_ID, t.clone());
                        t
                    }
                };
                self.assistants("POST", &format!("threads/{}/messages", t), Some(&obj([("role", Value::from("user")), ("content", Value::from(ex.body().to_string()))])))?;
                let run = self.run_and_wait(ex, &t, &a)?;
                let status = run.get("status").map(|s| s.to_string()).unwrap_or_default();
                if status != "completed" {
                    let err = run.get("last_error").and_then(|e| e.get("message")).map(|m| m.to_string()).unwrap_or_default();
                    return Err(crate::util::cloud_error(kinds::AZURE_HTTP, format!("Assistant run ended with status {}: {}", status, err)));
                }
                let msgs = self.assistants("GET", &format!("threads/{}/messages", t), None)?;
                let reply = msgs.get("data").and_then(|d| d.as_array()).and_then(|d| d.iter().find(|m| m.get("role").map(|r| r.to_string()).as_deref() == Some("assistant")).cloned()).unwrap_or_default();
                ex.set_body(Self::message_text(&reply));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
