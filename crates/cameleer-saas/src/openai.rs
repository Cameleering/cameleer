//! `openai:` component: OpenAI-compatible REST APIs (OpenAI, Azure OpenAI-compatible
//! gateways, Ollama, vLLM, LM Studio...). A simple counterpart of Camel 4's
//! `langchain4j-chat` / `langchain4j-embeddings`.
//!
//! `openai:chat-completion?model=gpt-4o-mini&apiKey=sk-...&baseUrl=https://api.openai.com/v1`
//! `openai:responses?model=gpt-4o-mini` (the Responses API)
//! `openai:chat-completion?model=llama3.2&apiFormat=ollama&baseUrl=http://localhost:11434`
//! `openai:embeddings?model=text-embedding-3-small`
//!
//! Chat completion: a text body is the user message (with the `systemMessage` option or the
//! `CamelOpenAISystemMessage` header as system prompt); a list body is the message list
//! (`{"role","content"}` objects, plain strings are user messages); an object body is sent
//! as the request (the model is added when missing). Options `temperature`, `maxTokens`,
//! `topP`, `user`; header `CamelOpenAIModel` overrides the model. The body becomes the
//! assistant message content (`outputType=full` keeps the whole response), headers
//! `CamelOpenAIResponseId`, `CamelOpenAIResponseModel`, `CamelOpenAIFinishReason`,
//! `CamelOpenAIPromptTokens`, `CamelOpenAICompletionTokens`, `CamelOpenAITotalTokens`.
//!
//! Streaming (`streaming=true` or the `CamelOpenAIStreaming` header): requests go with
//! `"stream": true` and the event stream is decoded as it arrives; each text delta goes to the
//! `streamingChunkProcessor=#bean` processor (body = the delta), also between the turns of a
//! tool loop. The body is the whole answer.
//!
//! The Responses API (`openai:responses`) and Ollama's native API (`apiFormat=ollama`,
//! `POST {baseUrl}/api/chat`) take the same bodies, options and tool options.
//!
//! Embeddings: a text body (or list of texts) is embedded; the body becomes the vector (a
//! list of numbers) or a list of vectors for a list input.
//!
//! `apiKey` (or the `OPENAI_API_KEY` environment variable) is sent as a Bearer token,
//! `organization` as `OpenAI-Organization`. `baseUrl` defaults to `https://api.openai.com/v1`.
//! API errors fail with `HttpOperationFailedException` carrying the API error message.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::ai::{self as ai, ChatModel, ChatRequest, ChatResponse, Usage};
use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{kinds, CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::{ObjectMap, Value};

use crate::rest::{body_text, header_str, join_url, Req, Resp, Rest};

pub const SYSTEM_MESSAGE: &str = "CamelOpenAISystemMessage";
pub const MODEL: &str = "CamelOpenAIModel";
pub const RESPONSE_ID: &str = "CamelOpenAIResponseId";
pub const RESPONSE_MODEL: &str = "CamelOpenAIResponseModel";
pub const FINISH_REASON: &str = "CamelOpenAIFinishReason";
pub const PROMPT_TOKENS: &str = "CamelOpenAIPromptTokens";
pub const COMPLETION_TOKENS: &str = "CamelOpenAICompletionTokens";
pub const TOTAL_TOKENS: &str = "CamelOpenAITotalTokens";
/// Model turns of an automatic tool loop.
pub const TOOL_ITERATIONS: &str = "CamelOpenAIToolIterations";
/// The tool calls: with `autoToolExecution=false`, the calls the model asked for
/// (`[{id, name, arguments}]`); with the loop, the calls it made.
pub const TOOL_CALLS: &str = "CamelOpenAIToolCalls";
/// Why an automatic tool loop stopped (`answer`, `max-turns`, `max-tokens`, ...).
pub const TOOL_STOP_REASON: &str = "CamelOpenAIToolStopReason";
/// In: stream this call (overrides the `streaming` option).
pub const STREAMING: &str = "CamelOpenAIStreaming";
/// On each exchange given to the `streamingChunkProcessor`: the delta's index (0, 1, ...),
/// counted across the turns of a tool loop.
pub const STREAMING_CHUNK_INDEX: &str = "CamelOpenAIStreamingChunkIndex";
/// Out: the number of text deltas received.
pub const STREAMING_CHUNK_COUNT: &str = "CamelOpenAIStreamingChunkCount";

/// The `openai` component.
#[derive(Default)]
pub struct OpenAiComponent;

/// The wire format of the chat operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Api {
    /// `POST /chat/completions`.
    ChatCompletions,
    /// `POST /responses`.
    Responses,
    /// Ollama's `POST /api/chat`.
    Ollama,
}

#[derive(Clone, Debug)]
struct Config {
    operation: String,
    api: Api,
    model: Option<String>,
    api_key: Option<String>,
    organization: Option<String>,
    base_url: String,
    system_message: Option<String>,
    temperature: Option<f64>,
    max_tokens: Option<i64>,
    top_p: Option<f64>,
    user: Option<String>,
    full: bool,
    streaming: bool,
    /// Bean name of the processor that receives each streamed text delta.
    chunk_processor: Option<String>,
    /// `ai-tool` tags offered to the model (Camel `tags`).
    tags: Vec<String>,
    auto_tool_execution: bool,
    max_tool_iterations: usize,
    max_agentic_tokens: Option<i64>,
    parallel_tool_execution: bool,
    fail_on_unknown_tool: bool,
    record: Option<String>,
    replay: Option<String>,
}

impl Config {
    fn is_chat(&self) -> bool {
        self.operation != "embeddings"
    }
}

impl Component for OpenAiComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let operation = match remaining.trim() {
            "chat-completion" | "chat" | "chat-completions" => "chat-completion",
            "responses" | "response" => "responses",
            "embeddings" | "embedding" => "embeddings",
            other => return Err(CamelError::illegal_argument(format!("openai: unsupported operation '{}' (chat-completion, responses or embeddings)", other))),
        }
        .to_string();
        let num = |params: &mut Parameters, k: &str| -> Result<Option<f64>> {
            match params.take(k) {
                Some(v) => v.trim().parse().map(Some).map_err(|_| CamelError::illegal_argument(format!("openai: {} must be a number", k))),
                None => Ok(None),
            }
        };
        let api = match params.take("apiFormat").map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("openai") => {
                if operation == "responses" {
                    Api::Responses
                } else {
                    Api::ChatCompletions
                }
            }
            Some("ollama") => match operation.as_str() {
                "chat-completion" => Api::Ollama,
                op => return Err(CamelError::illegal_argument(format!("openai: apiFormat=ollama works with chat-completion, not {} (Ollama also serves the OpenAI API under /v1)", op))),
            },
            Some(other) => return Err(CamelError::illegal_argument(format!("openai: apiFormat must be openai or ollama, got {}", other))),
        };
        let cfg = Config {
            operation,
            api,
            model: params.take("model").filter(|s| !s.is_empty()),
            api_key: params.take("apiKey").filter(|s| !s.is_empty()).or_else(|| std::env::var("OPENAI_API_KEY").ok()),
            organization: params.take("organization"),
            base_url: params.take("baseUrl").unwrap_or_else(|| "https://api.openai.com/v1".into()),
            system_message: params.take("systemMessage"),
            temperature: num(params, "temperature")?,
            max_tokens: num(params, "maxTokens")?.map(|v| v as i64),
            top_p: num(params, "topP")?,
            user: params.take("user"),
            full: params.take("outputType").map(|o| o.eq_ignore_ascii_case("full")).unwrap_or(false),
            streaming: params.take_bool("streaming", false)?,
            chunk_processor: params.take("streamingChunkProcessor").map(|r| r.trim().trim_start_matches('#').trim_start_matches("bean:").to_string()).filter(|s| !s.is_empty()),
            tags: params.take("tags").map(|t| t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default(),
            auto_tool_execution: params.take_bool("autoToolExecution", true)?,
            max_tool_iterations: params.take_usize("maxToolIterations", 50)?,
            max_agentic_tokens: match params.take("maxAgenticTokens") {
                Some(v) => Some(v.trim().parse().map_err(|_| CamelError::illegal_argument("openai: maxAgenticTokens must be a number"))?),
                None => None,
            },
            parallel_tool_execution: params.take_bool("parallelToolExecution", true)?,
            fail_on_unknown_tool: match params.take_string("hallucinatedToolNameStrategy", "report").to_ascii_lowercase().as_str() {
                "report" | "inform" | "continue" => false,
                "fail" | "throw" | "exception" => true,
                other => return Err(CamelError::illegal_argument(format!("openai: hallucinatedToolNameStrategy must be report or fail, got {}", other))),
            },
            record: params.take("record"),
            replay: params.take("replay"),
        };
        if cfg.api == Api::Ollama && cfg.streaming {
            return Err(CamelError::illegal_argument("openai: streaming is not available with apiFormat=ollama; use the default format with baseUrl http://localhost:11434/v1"));
        }
        let timeout = params.take_duration("timeout", Duration::from_secs(120))?;
        params.take("lazyStartProducer");
        Ok(Arc::new(OpenAiEndpoint { uri: uri.to_string(), cfg, rest: Rest::new(timeout), ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An openai endpoint (producer only).
pub struct OpenAiEndpoint {
    uri: String,
    cfg: Config,
    rest: Rest,
    ctx: WeakContext,
}

impl Endpoint for OpenAiEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(OpenAiProducer { cfg: self.cfg.clone(), rest: self.rest.clone() }))
    }
    fn chat_model(&self) -> Option<Arc<dyn ChatModel>> {
        if !self.cfg.is_chat() {
            return None;
        }
        let deltas = if self.cfg.streaming { Some(Deltas::new(Some(self.ctx.clone()), self.cfg.chunk_processor.clone(), None)) } else { None };
        let base: Arc<dyn ChatModel> = Arc::new(OpenAiChatModel { cfg: self.cfg.clone(), rest: self.rest.clone(), deltas });
        cameleer_core::ai::with_recording(Some(base), self.cfg.record.as_deref(), self.cfg.replay.as_deref()).ok().flatten()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct OpenAiProducer {
    cfg: Config,
    rest: Rest,
}

impl Service for OpenAiProducer {}

fn msg(role: &str, content: impl Into<Value>) -> Value {
    Value::object([("role", Value::from(role)), ("content", content.into())])
}

impl OpenAiProducer {
    /// The URL of an API path (Ollama's paths are under the server root, so a `/v1` base URL
    /// left over from the OpenAI-compatible API is dropped).
    fn url(&self, path: &str) -> String {
        if self.cfg.api == Api::Ollama {
            let base = self.cfg.base_url.trim_end_matches('/');
            return join_url(base.strip_suffix("/v1").unwrap_or(base), path);
        }
        join_url(&self.cfg.base_url, path)
    }

    fn request(&self, path: &str, body: &Value) -> Req {
        let mut req = Req::post(self.url(path)).json(body);
        if let Some(k) = &self.cfg.api_key {
            req = req.bearer(k);
        }
        if let Some(o) = &self.cfg.organization {
            req = req.header("OpenAI-Organization", o.clone());
        }
        req
    }

    fn failed(path: &str, r: &Resp) -> CamelError {
        let m = r
            .json()
            .ok()
            .and_then(|v| v.get("error").map(|e| e.get("message").map(|m| m.to_string()).unwrap_or_else(|| e.to_string())))
            .unwrap_or_else(|| r.text());
        CamelError::new(kinds::HTTP_OPERATION_FAILED, format!("HTTP operation failed invoking OpenAI {} with statusCode: {}: {}", path, r.status, m))
    }

    fn call(&self, path: &str, body: &Value) -> Result<Value> {
        let r = self.rest.send(self.request(path, body))?;
        if !r.is_success() {
            return Err(Self::failed(path, &r));
        }
        r.json()
    }

    /// A streamed Chat Completions call: text deltas go to `on_delta` as they arrive, tool call
    /// deltas are put together by `index`; the result has the shape of a `chat.completion`.
    fn stream_chat_completions(&self, body: &Value, on_delta: &mut dyn FnMut(&str) -> Result<()>) -> Result<Value> {
        let mut body = body.clone();
        if let Some(o) = body.as_object_mut() {
            o.insert("stream".into(), Value::Bool(true));
            o.insert("stream_options".into(), Value::object([("include_usage", Value::Bool(true))]));
        }
        let path = "/chat/completions";
        let mut acc = ChunkAccumulator::default();
        let r = self.rest.send_events(self.request(path, &body).header("Accept", "text/event-stream"), &mut |data| {
            if data.trim() == "[DONE]" {
                return Ok(());
            }
            acc.push(&cameleer_core::json::parse(data)?, on_delta)
        })?;
        if !r.is_success() {
            return Err(Self::failed(path, &r));
        }
        if !r.body.is_empty() {
            // the server answered without streaming
            let v = r.json()?;
            if let Some(t) = v.get("choices").and_then(|c| c.as_array()).and_then(|c| c.first()).and_then(|c| c.get("message")).and_then(|m| m.get("content")).filter(|t| !t.is_null()) {
                on_delta(&t.to_string())?;
            }
            return Ok(v);
        }
        Ok(acc.into_completion())
    }

    /// A streamed Responses API call: `response.output_text.delta` events go to `on_delta`, the
    /// result is the response of the final `response.completed` (or `incomplete`) event.
    fn stream_responses(&self, body: &Value, on_delta: &mut dyn FnMut(&str) -> Result<()>) -> Result<Value> {
        let mut body = body.clone();
        if let Some(o) = body.as_object_mut() {
            o.insert("stream".into(), Value::Bool(true));
        }
        let path = "/responses";
        let mut last: Option<Value> = None;
        let r = self.rest.send_events(self.request(path, &body).header("Accept", "text/event-stream"), &mut |data| {
            if data.trim() == "[DONE]" {
                return Ok(());
            }
            let v = cameleer_core::json::parse(data)?;
            match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                "response.output_text.delta" => on_delta(&v.get("delta").map(|d| d.to_string()).unwrap_or_default()),
                "response.completed" | "response.incomplete" | "response.failed" => {
                    last = v.get("response").cloned();
                    Ok(())
                }
                "error" => Err(CamelError::new(kinds::HTTP_OPERATION_FAILED, format!("OpenAI /responses stream failed: {}", v.get("message").map(|m| m.to_string()).unwrap_or_else(|| v.to_json())))),
                _ => Ok(()),
            }
        })?;
        if !r.is_success() {
            return Err(Self::failed(path, &r));
        }
        if !r.body.is_empty() {
            let v = r.json()?;
            let text = ai::wire::from_responses(&v)?.message.text();
            if !text.is_empty() {
                on_delta(&text)?;
            }
            return Ok(v);
        }
        last.ok_or_else(|| CamelError::illegal_state("openai: the /responses stream ended without a final response"))
    }

    fn path(&self) -> &'static str {
        match self.cfg.api {
            Api::ChatCompletions => "/chat/completions",
            Api::Responses => "/responses",
            Api::Ollama => "/api/chat",
        }
    }

    /// Add the endpoint's sampling options to a request body (keys already there win).
    fn apply_options(&self, body: &mut ObjectMap) {
        let cfg = &self.cfg;
        match cfg.api {
            Api::Ollama => {
                let mut options = body.get("options").and_then(|o| o.as_object()).cloned().unwrap_or_default();
                for (k, v) in [("temperature", cfg.temperature.map(Value::Float)), ("num_predict", cfg.max_tokens.map(Value::Int)), ("top_p", cfg.top_p.map(Value::Float))] {
                    if let Some(v) = v {
                        options.entry(k.into()).or_insert(v);
                    }
                }
                if !options.is_empty() {
                    body.insert("options".into(), Value::Object(options));
                }
                body.entry("stream".into()).or_insert(Value::Bool(false));
            }
            api => {
                let max = if api == Api::Responses { "max_output_tokens" } else { "max_tokens" };
                for (k, v) in [("temperature", cfg.temperature.map(Value::Float)), (max, cfg.max_tokens.map(Value::Int)), ("top_p", cfg.top_p.map(Value::Float)), ("user", cfg.user.clone().map(Value::String))] {
                    if let Some(v) = v {
                        body.entry(k.into()).or_insert(v);
                    }
                }
            }
        }
    }

    fn streaming(&self, ex: &Exchange) -> bool {
        ex.header(STREAMING).map(|v| v.is_truthy() && v.to_string() != "false").unwrap_or(self.cfg.streaming)
    }
}

impl Processor for OpenAiProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let streaming = self.streaming(ex);
        if self.cfg.is_chat() && !matches!(ex.body(), Value::Object(_)) && (!self.cfg.tags.is_empty() || self.cfg.replay.is_some() || self.cfg.record.is_some() || self.cfg.api != Api::ChatCompletions) {
            return self.tool_chat(ex, streaming);
        }
        let model = header_str(ex, MODEL).or_else(|| self.cfg.model.clone());
        if self.cfg.operation == "embeddings" {
            let input = match &ex.message.body {
                Value::Array(a) => Value::Array(a.iter().map(|v| Value::String(v.to_string())).collect()),
                _ => Value::String(body_text(ex).ok_or_else(|| CamelError::illegal_argument("openai: embeddings require a body"))?),
            };
            let multi = matches!(input, Value::Array(_));
            let mut req = vec![("input", input)];
            if let Some(m) = model {
                req.push(("model", Value::String(m)));
            }
            let resp = self.call("/embeddings", &Value::object(req))?;
            let vectors: Vec<Value> = resp.get("data").and_then(|d| d.as_array()).map(|d| d.iter().map(|e| e.get("embedding").cloned().unwrap_or(Value::Null)).collect()).unwrap_or_default();
            ex.message.body = if self.cfg.full {
                resp
            } else if multi {
                Value::Array(vectors)
            } else {
                vectors.into_iter().next().unwrap_or(Value::Null)
            };
            return Ok(());
        }
        let system = header_str(ex, SYSTEM_MESSAGE).or_else(|| self.cfg.system_message.clone());
        let mut request = match &ex.message.body {
            Value::Object(o) => o.clone(),
            Value::Array(a) => {
                let mut msgs: Vec<Value> = Vec::new();
                if let Some(s) = &system {
                    msgs.push(msg("system", s.as_str()));
                }
                msgs.extend(a.iter().map(|m| match m {
                    Value::Object(_) => m.clone(),
                    other => msg("user", other.to_string()),
                }));
                [("messages".to_string(), Value::Array(msgs))].into_iter().collect()
            }
            _ => {
                let text = body_text(ex).ok_or_else(|| CamelError::illegal_argument("openai: chat-completion requires a body"))?;
                let mut msgs = Vec::new();
                if let Some(s) = &system {
                    msgs.push(msg("system", s.as_str()));
                }
                msgs.push(msg("user", text));
                [("messages".to_string(), Value::Array(msgs))].into_iter().collect()
            }
        };
        if let Some(m) = model {
            request.entry("model".into()).or_insert(Value::String(m));
        }
        self.apply_options(&mut request);
        let deltas = if streaming { Some(Deltas::new(ex.context().ok().map(|c| c.downgrade()), self.cfg.chunk_processor.clone(), conversation(ex))) } else { None };
        if self.cfg.api != Api::ChatCompletions {
            // a request map for the Responses API or Ollama: sent as is, the answer parsed
            let request = Value::Object(request);
            let resp = match (&deltas, self.cfg.api) {
                (Some(d), Api::Responses) => self.stream_responses(&request, &mut |t| d.send(t))?,
                _ => self.call(self.path(), &request)?,
            };
            let parsed = if self.cfg.api == Api::Responses { ai::wire::from_responses(&resp)? } else { ai::wire::from_ollama(&resp)? };
            set_usage(ex, parsed.usage);
            for (k, v) in [(RESPONSE_ID, resp.get("id")), (RESPONSE_MODEL, resp.get("model"))] {
                if let Some(v) = v.filter(|v| !v.is_null()) {
                    ex.message.headers.set(k, v.clone());
                }
            }
            ex.message.headers.set(FINISH_REASON, Value::from(parsed.stop_reason.as_str()));
            if let Some(d) = &deltas {
                ex.message.headers.set(STREAMING_CHUNK_COUNT, Value::Int(d.count() as i64));
            }
            ex.message.body = if self.cfg.full { resp } else { Value::from(parsed.message.text()) };
            return Ok(());
        }
        let request = Value::Object(request);
        let resp = match &deltas {
            Some(d) => self.stream_chat_completions(&request, &mut |t| d.send(t))?,
            None => self.call("/chat/completions", &request)?,
        };
        let choice = resp.get("choices").and_then(|c| c.as_array()).and_then(|c| c.first()).cloned().unwrap_or(Value::Null);
        let h = &mut ex.message.headers;
        for (k, v) in [
            (RESPONSE_ID, resp.get("id")),
            (RESPONSE_MODEL, resp.get("model")),
            (FINISH_REASON, choice.get("finish_reason")),
            (PROMPT_TOKENS, resp.get("usage").and_then(|u| u.get("prompt_tokens"))),
            (COMPLETION_TOKENS, resp.get("usage").and_then(|u| u.get("completion_tokens"))),
            (TOTAL_TOKENS, resp.get("usage").and_then(|u| u.get("total_tokens"))),
        ] {
            if let Some(v) = v {
                h.set(k, v.clone());
            }
        }
        if let Some(d) = &deltas {
            h.set(STREAMING_CHUNK_COUNT, Value::Int(d.count() as i64));
        }
        ex.message.body = if self.cfg.full { resp } else { choice.get("message").and_then(|m| m.get("content")).cloned().unwrap_or(Value::Null) };
        Ok(())
    }
}

fn conversation(ex: &Exchange) -> Option<String> {
    ex.header(ai::headers::CONVERSATION_ID).filter(|v| !v.is_null()).map(|v| v.to_string())
}

impl OpenAiProducer {
    /// Chat through the neutral model: with the `tags` tools, the automatic tool loop (Camel
    /// `autoToolExecution`), or one turn whose tool calls are returned in `CamelOpenAIToolCalls`.
    fn tool_chat(&self, ex: &mut Exchange, streaming: bool) -> Result<()> {
        let mut cfg = self.cfg.clone();
        if let Some(m) = header_str(ex, MODEL) {
            cfg.model = Some(m);
        }
        let deltas = if streaming { Some(Deltas::new(ex.context().ok().map(|c| c.downgrade()), cfg.chunk_processor.clone(), conversation(ex))) } else { None };
        let base: Arc<dyn ChatModel> = Arc::new(OpenAiChatModel { cfg: cfg.clone(), rest: self.rest.clone(), deltas: deltas.clone() });
        let model = ai::with_recording(Some(base), cfg.record.as_deref(), cfg.replay.as_deref())?.expect("model");
        let ctx = ex.context()?.clone();
        let provider = ai::tool_provider(&ctx);
        if !cfg.tags.is_empty() && provider.is_none() {
            return Err(CamelError::illegal_state("openai: tags need the ai feature (cameleer-ai registered on the context)"));
        }
        let tools = provider.as_ref().map(|p| p.tools(&cfg.tags)).unwrap_or_default();
        let system = header_str(ex, SYSTEM_MESSAGE).or_else(|| cfg.system_message.clone());
        let messages = ai::exchange_messages(ex)?;
        if messages.is_empty() {
            return Err(CamelError::illegal_argument(format!("openai: {} requires a body", cfg.operation)));
        }
        let request = ChatRequest { system, messages, tools, max_tokens: cfg.max_tokens };
        if !cfg.auto_tool_execution || (cfg.tags.is_empty() && cfg.replay.is_none() && cfg.record.is_none()) {
            let resp = model.chat(&request)?;
            let calls: Vec<Value> = resp
                .message
                .tool_uses()
                .into_iter()
                .map(|(id, name, args)| Value::object([("id", Value::from(id)), ("name", Value::from(name)), ("arguments", args.clone())]))
                .collect();
            set_usage(ex, resp.usage);
            ex.message.headers.set(FINISH_REASON, Value::from(resp.stop_reason.as_str()));
            if !calls.is_empty() {
                ex.message.headers.set(TOOL_CALLS, Value::Array(calls));
            }
            if let Some(d) = &deltas {
                ex.message.headers.set(STREAMING_CHUNK_COUNT, Value::Int(d.count() as i64));
            }
            ex.message.body = Value::from(resp.message.text());
            return Ok(());
        }
        let limits = ai::AgentLimits {
            max_turns: cfg.max_tool_iterations.max(1),
            max_tokens: cfg.max_agentic_tokens,
            parallel: if cfg.parallel_tool_execution { 4 } else { 1 },
            hallucinated: if cfg.fail_on_unknown_tool { ai::HallucinatedTool::Fail } else { ai::HallucinatedTool::Report },
            ..Default::default()
        };
        let base_call = ai::ToolInvocation {
            conversation_id: conversation(ex),
            properties: ex.properties.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            ..Default::default()
        };
        let run = ai::run_agent(model.as_ref(), provider.as_deref(), request, &limits, &base_call)?;
        set_usage(ex, run.usage);
        let calls: Vec<Value> = run
            .tool_calls
            .iter()
            .map(|c| Value::object([("id", Value::from(c.id.as_str())), ("name", Value::from(c.name.as_str())), ("arguments", c.arguments.clone()), ("isError", Value::Bool(c.is_error))]))
            .collect();
        let h = &mut ex.message.headers;
        h.set(TOOL_ITERATIONS, Value::Int(run.turns as i64));
        h.set(TOOL_STOP_REASON, Value::from(run.stop.as_str()));
        h.set(TOOL_CALLS, Value::Array(calls));
        if let Some(d) = &deltas {
            h.set(STREAMING_CHUNK_COUNT, Value::Int(d.count() as i64));
        }
        ex.message.body = Value::from(run.answer);
        Ok(())
    }
}

fn set_usage(ex: &mut Exchange, u: Usage) {
    let h = &mut ex.message.headers;
    h.set(PROMPT_TOKENS, Value::Int(u.input));
    h.set(COMPLETION_TOKENS, Value::Int(u.output));
    h.set(TOTAL_TOKENS, Value::Int(u.input + u.output));
}

/// Where streamed text deltas go: the `streamingChunkProcessor` bean (looked up when the first
/// delta arrives), with a count across the turns of a call.
#[derive(Clone)]
struct Deltas {
    ctx: Option<WeakContext>,
    processor: Option<String>,
    conversation: Option<String>,
    count: Arc<AtomicUsize>,
}

impl Deltas {
    fn new(ctx: Option<WeakContext>, processor: Option<String>, conversation: Option<String>) -> Deltas {
        Deltas { ctx, processor, conversation, count: Arc::new(AtomicUsize::new(0)) }
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    fn send(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let index = self.count.fetch_add(1, Ordering::SeqCst);
        let Some(name) = &self.processor else { return Ok(()) };
        let ctx = self.ctx.as_ref().and_then(|c| c.upgrade());
        let p = ctx
            .as_ref()
            .and_then(|c| c.registry().lookup::<Arc<dyn Processor>>(name))
            .ok_or_else(|| CamelError::no_such_bean(name))?;
        let mut cex = Exchange::new(ctx);
        cex.set_header(STREAMING_CHUNK_INDEX, index as i64);
        if let Some(c) = &self.conversation {
            cex.set_header(ai::headers::CONVERSATION_ID, c.as_str());
        }
        cex.set_body(text.to_string());
        p.process(&mut cex)?;
        match cex.exception.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Puts the `chat.completion.chunk` events of a streamed Chat Completions call back together:
/// text deltas are appended (and passed on), tool call deltas are accumulated by `index` (the
/// id and function name come first, the arguments in string pieces), usage comes in the last
/// chunk.
#[derive(Default)]
struct ChunkAccumulator {
    id: Option<Value>,
    model: Option<Value>,
    content: String,
    finish: Option<Value>,
    usage: Option<Value>,
    /// index → (id, name, arguments)
    calls: BTreeMap<i64, (String, String, String)>,
}

impl ChunkAccumulator {
    fn push(&mut self, v: &Value, on_delta: &mut dyn FnMut(&str) -> Result<()>) -> Result<()> {
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            return Err(CamelError::new(kinds::HTTP_OPERATION_FAILED, format!("OpenAI stream failed: {}", e.get("message").map(|m| m.to_string()).unwrap_or_else(|| e.to_json()))));
        }
        if let Some(x) = v.get("id").filter(|x| !x.is_null()) {
            self.id = Some(x.clone());
        }
        if let Some(x) = v.get("model").filter(|x| !x.is_null()) {
            self.model = Some(x.clone());
        }
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(u.clone());
        }
        for c in v.get("choices").and_then(|c| c.as_array()).cloned().unwrap_or_default() {
            if let Some(f) = c.get("finish_reason").filter(|f| !f.is_null()) {
                self.finish = Some(f.clone());
            }
            let Some(delta) = c.get("delta") else { continue };
            if let Some(t) = delta.get("content").filter(|t| !t.is_null()) {
                let t = t.to_string();
                if !t.is_empty() {
                    self.content.push_str(&t);
                    on_delta(&t)?;
                }
            }
            if let Some(Value::Array(tcs)) = delta.get("tool_calls") {
                for (pos, tc) in tcs.iter().enumerate() {
                    let index = tc.get("index").and_then(|i| i.as_i64()).unwrap_or(pos as i64);
                    let entry = self.calls.entry(index).or_default();
                    if let Some(id) = tc.get("id").filter(|x| !x.is_null()).map(|x| x.to_string()).filter(|x| !x.is_empty()) {
                        entry.0 = id;
                    }
                    if let Some(f) = tc.get("function") {
                        if let Some(n) = f.get("name").filter(|x| !x.is_null()).map(|x| x.to_string()).filter(|x| !x.is_empty()) {
                            entry.1 = n;
                        }
                        if let Some(a) = f.get("arguments").filter(|x| !x.is_null()) {
                            entry.2.push_str(&a.to_string());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// The whole answer in the shape of a (non-streamed) `chat.completion`.
    fn into_completion(self) -> Value {
        let mut message = ObjectMap::new();
        message.insert("role".into(), Value::from("assistant"));
        let calls: Vec<Value> = self
            .calls
            .into_values()
            .map(|(id, name, args)| {
                Value::object([("id", Value::from(id)), ("type", Value::from("function")), ("function", Value::object([("name", Value::from(name)), ("arguments", Value::from(args))]))])
            })
            .collect();
        message.insert("content".into(), if self.content.is_empty() && !calls.is_empty() { Value::Null } else { Value::from(self.content) });
        if !calls.is_empty() {
            message.insert("tool_calls".into(), Value::Array(calls));
        }
        let mut out = ObjectMap::new();
        out.insert("id".into(), self.id.unwrap_or(Value::Null));
        out.insert("object".into(), Value::from("chat.completion"));
        out.insert("model".into(), self.model.unwrap_or(Value::Null));
        out.insert("choices".into(), Value::Array(vec![Value::object([("index", Value::Int(0)), ("message", Value::Object(message)), ("finish_reason", self.finish.unwrap_or(Value::Null))])]));
        if let Some(u) = self.usage {
            out.insert("usage".into(), u);
        }
        Value::Object(out)
    }
}

/// The chat APIs as a [`ChatModel`]: Chat Completions (also Ollama's and vLLM's OpenAI API;
/// missing call ids are generated), the Responses API, or Ollama's native `/api/chat`; with
/// streaming, text deltas go to the chunk processor as they arrive.
pub struct OpenAiChatModel {
    cfg: Config,
    rest: Rest,
    deltas: Option<Deltas>,
}

impl ChatModel for OpenAiChatModel {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let producer = OpenAiProducer { cfg: self.cfg.clone(), rest: self.rest.clone() };
        let model = self.cfg.model.as_deref();
        let mut body = match self.cfg.api {
            Api::ChatCompletions => ai::wire::to_openai(model, req),
            Api::Responses => ai::wire::to_responses(model, req),
            Api::Ollama => ai::wire::to_ollama(model, req),
        };
        if let Some(o) = body.as_object_mut() {
            producer.apply_options(o);
        }
        match (self.cfg.api, &self.deltas) {
            (Api::ChatCompletions, Some(d)) => ai::wire::from_openai(&producer.stream_chat_completions(&body, &mut |t| d.send(t))?),
            (Api::ChatCompletions, None) => ai::wire::from_openai(&producer.call("/chat/completions", &body)?),
            (Api::Responses, Some(d)) => ai::wire::from_responses(&producer.stream_responses(&body, &mut |t| d.send(t))?),
            (Api::Responses, None) => ai::wire::from_responses(&producer.call("/responses", &body)?),
            (Api::Ollama, _) => ai::wire::from_ollama(&producer.call("/api/chat", &body)?),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_are_put_back_together() {
        let chunks = [
            r#"{"id":"c1","model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#,
            r#"{"id":"c1","choices":[{"index":0,"delta":{"content":"Hello"}}]}"#,
            r#"{"id":"c1","choices":[{"index":0,"delta":{"content":" there"}}]}"#,
            r#"{"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":""}}]}}]}"#,
            r#"{"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_2","type":"function","function":{"name":"other","arguments":"{}"}}]}}]}"#,
            r#"{"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\":"}}]}}]}"#,
            r#"{"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]}"#,
            r#"{"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"id":"c1","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}}"#,
        ];
        let mut acc = ChunkAccumulator::default();
        let mut deltas = Vec::new();
        for c in chunks {
            acc.push(&cameleer_core::json::parse(c).unwrap(), &mut |t| {
                deltas.push(t.to_string());
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(deltas, vec!["Hello", " there"]);
        let v = acc.into_completion();
        let r = ai::wire::from_openai(&v).unwrap();
        assert_eq!(r.message.text(), "Hello there");
        let calls: Vec<(String, String, String)> = r.message.tool_uses().into_iter().map(|(i, n, a)| (i.to_string(), n.to_string(), a.to_json())).collect();
        assert_eq!(calls, vec![("call_1".into(), "lookup".into(), r#"{"q":1}"#.into()), ("call_2".into(), "other".into(), "{}".into())]);
        assert_eq!((r.stop_reason.as_str(), r.usage), ("tool_calls", Usage { input: 5, output: 3 }));
        let mut acc = ChunkAccumulator::default();
        assert!(acc.push(&cameleer_core::json::parse(r#"{"error":{"message":"overloaded"}}"#).unwrap(), &mut |_| Ok(())).is_err());
    }
}
