//! `openai:` component: OpenAI-compatible REST APIs (OpenAI, Azure OpenAI-compatible
//! gateways, Ollama, vLLM, LM Studio...). A simple counterpart of Camel 4's
//! `langchain4j-chat` / `langchain4j-embeddings`.
//!
//! `openai:chat-completion?model=gpt-4o-mini&apiKey=sk-...&baseUrl=https://api.openai.com/v1`
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
//! Embeddings: a text body (or list of texts) is embedded; the body becomes the vector (a
//! list of numbers) or a list of vectors for a list input.
//!
//! `apiKey` (or the `OPENAI_API_KEY` environment variable) is sent as a Bearer token,
//! `organization` as `OpenAI-Organization`. `baseUrl` defaults to `https://api.openai.com/v1`.
//! API errors fail with `HttpOperationFailedException` carrying the API error message.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::ai::{self as ai, ChatModel, ChatRequest, ChatResponse, Usage};
use cameleer_core::context::CamelContext;
use cameleer_core::error::{kinds, CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use crate::rest::{body_text, header_str, join_url, Req, Rest};

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

/// The `openai` component.
#[derive(Default)]
pub struct OpenAiComponent;

#[derive(Clone, Debug)]
struct Config {
    operation: String,
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

impl Component for OpenAiComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let operation = match remaining.trim() {
            "chat-completion" | "chat" | "chat-completions" => "chat-completion",
            "embeddings" | "embedding" => "embeddings",
            other => return Err(CamelError::illegal_argument(format!("openai: unsupported operation '{}' (chat-completion or embeddings)", other))),
        }
        .to_string();
        let num = |params: &mut Parameters, k: &str| -> Result<Option<f64>> {
            match params.take(k) {
                Some(v) => v.trim().parse().map(Some).map_err(|_| CamelError::illegal_argument(format!("openai: {} must be a number", k))),
                None => Ok(None),
            }
        };
        let cfg = Config {
            operation,
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
        let timeout = params.take_duration("timeout", Duration::from_secs(120))?;
        params.take("lazyStartProducer");
        Ok(Arc::new(OpenAiEndpoint { uri: uri.to_string(), cfg, rest: Rest::new(timeout) }))
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
}

impl Endpoint for OpenAiEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(OpenAiProducer { cfg: self.cfg.clone(), rest: self.rest.clone() }))
    }
    fn chat_model(&self) -> Option<Arc<dyn ChatModel>> {
        if self.cfg.operation != "chat-completion" {
            return None;
        }
        let base: Arc<dyn ChatModel> = Arc::new(OpenAiChatModel { cfg: self.cfg.clone(), rest: self.rest.clone() });
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
    fn call(&self, path: &str, body: &Value) -> Result<Value> {
        let mut req = Req::post(join_url(&self.cfg.base_url, path)).json(body);
        if let Some(k) = &self.cfg.api_key {
            req = req.bearer(k);
        }
        if let Some(o) = &self.cfg.organization {
            req = req.header("OpenAI-Organization", o.clone());
        }
        let r = self.rest.send(req)?;
        if !r.is_success() {
            let m = r.json().ok().and_then(|v| v.get("error").and_then(|e| e.get("message")).map(|m| m.to_string())).unwrap_or_else(|| r.text());
            return Err(CamelError::new(kinds::HTTP_OPERATION_FAILED, format!("HTTP operation failed invoking OpenAI {} with statusCode: {}: {}", path, r.status, m)));
        }
        r.json()
    }
}

impl Processor for OpenAiProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        if self.cfg.operation == "chat-completion" && (!self.cfg.tags.is_empty() || self.cfg.replay.is_some()) && !matches!(ex.body(), Value::Object(_)) {
            return self.tool_chat(ex);
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
        if let Some(t) = self.cfg.temperature {
            request.entry("temperature".into()).or_insert(Value::Float(t));
        }
        if let Some(t) = self.cfg.max_tokens {
            request.entry("max_tokens".into()).or_insert(Value::Int(t));
        }
        if let Some(t) = self.cfg.top_p {
            request.entry("top_p".into()).or_insert(Value::Float(t));
        }
        if let Some(u) = &self.cfg.user {
            request.entry("user".into()).or_insert(Value::String(u.clone()));
        }
        let resp = self.call("/chat/completions", &Value::Object(request))?;
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
        ex.message.body = if self.cfg.full { resp } else { choice.get("message").and_then(|m| m.get("content")).cloned().unwrap_or(Value::Null) };
        Ok(())
    }
}

impl OpenAiProducer {
    /// Chat with the `tags` tools: the automatic tool loop (Camel `autoToolExecution`), or one
    /// turn whose tool calls are returned in `CamelOpenAIToolCalls`.
    fn tool_chat(&self, ex: &mut Exchange) -> Result<()> {
        let mut cfg = self.cfg.clone();
        if let Some(m) = header_str(ex, MODEL) {
            cfg.model = Some(m);
        }
        let base: Arc<dyn ChatModel> = Arc::new(OpenAiChatModel { cfg: cfg.clone(), rest: self.rest.clone() });
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
            return Err(CamelError::illegal_argument("openai: chat-completion requires a body"));
        }
        let request = ChatRequest { system, messages, tools, max_tokens: cfg.max_tokens };
        if !cfg.auto_tool_execution {
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
            conversation_id: ex.header(ai::headers::CONVERSATION_ID).map(|v| v.to_string()),
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

/// The Chat Completions API as a [`ChatModel`] (also Ollama, vLLM and other compatible
/// servers; missing call ids are generated).
pub struct OpenAiChatModel {
    cfg: Config,
    rest: Rest,
}

impl ChatModel for OpenAiChatModel {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let mut body = ai::wire::to_openai(self.cfg.model.as_deref(), req);
        if let (Some(t), Some(o)) = (self.cfg.temperature, body.as_object_mut()) {
            o.insert("temperature".into(), Value::Float(t));
        }
        let producer = OpenAiProducer { cfg: self.cfg.clone(), rest: self.rest.clone() };
        let resp = producer.call("/chat/completions", &body)?;
        ai::wire::from_openai(&resp)
    }
}
