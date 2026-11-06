//! `anthropic:` component: the Anthropic Messages API (Claude models), with tool calling.
//!
//! `anthropic:messages?model=claude-sonnet-4-5&apiKey=...`
//!
//! A text body is the user message (with `systemMessage` or the `CamelAnthropicSystemMessage`
//! header as the system prompt); a list body is the message list (`{"role","content"}`); an
//! object body is sent as the request (`model` and `max_tokens` are added when missing) and the
//! body becomes the whole response. Otherwise the body becomes the answer's text.
//!
//! Tool calling uses the same options as `openai`: `tags` offers the `ai-tool` routes with
//! those tags, and the tool loop runs them through the `ai-tool` envelope
//! (`autoToolExecution`, `maxToolIterations`, `maxAgenticTokens`, `parallelToolExecution`,
//! `hallucinatedToolNameStrategy`), with `record` / `replay` for tests. The endpoint is also a
//! model for the `agent` step.
//!
//! `apiKey` (or `ANTHROPIC_API_KEY`) is sent as `x-api-key`, with `anthropic-version`
//! (`anthropicVersion`, default `2023-06-01`). `baseUrl` defaults to
//! `https://api.anthropic.com`.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::ai::{self as ai, ChatModel, ChatRequest, ChatResponse, ToolLoopOptions};
use cameleer_core::context::CamelContext;
use cameleer_core::error::{kinds, CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use crate::rest::{header_str, join_url, Req, Rest};

pub const SYSTEM_MESSAGE: &str = "CamelAnthropicSystemMessage";
pub const MODEL: &str = "CamelAnthropicModel";
pub const STOP_REASON: &str = "CamelAnthropicStopReason";
pub const INPUT_TOKENS: &str = "CamelAnthropicInputTokens";
pub const OUTPUT_TOKENS: &str = "CamelAnthropicOutputTokens";

/// The `anthropic` component.
#[derive(Default)]
pub struct AnthropicComponent;

#[derive(Clone, Debug)]
struct Config {
    model: Option<String>,
    api_key: Option<String>,
    base_url: String,
    version: String,
    system_message: Option<String>,
    max_tokens: i64,
    temperature: Option<f64>,
    /// `promptCaching=auto` (default): cache breakpoints on tools, system and history.
    prompt_caching: bool,
}

impl Component for AnthropicComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        match remaining.trim() {
            "messages" | "chat" | "" => {}
            other => return Err(CamelError::illegal_argument(format!("anthropic: unsupported operation '{}' (messages)", other))),
        }
        let cfg = Config {
            model: params.take("model").filter(|s| !s.is_empty()),
            api_key: params.take("apiKey").filter(|s| !s.is_empty()).or_else(|| std::env::var("ANTHROPIC_API_KEY").ok()),
            base_url: params.take("baseUrl").unwrap_or_else(|| "https://api.anthropic.com".into()),
            version: params.take_string("anthropicVersion", "2023-06-01"),
            system_message: params.take("systemMessage"),
            max_tokens: params.take_i64("maxTokens", 1024)?,
            temperature: match params.take("temperature") {
                Some(v) => Some(v.trim().parse().map_err(|_| CamelError::illegal_argument("anthropic: temperature must be a number"))?),
                None => None,
            },
            prompt_caching: match params.take_string("promptCaching", "auto").to_ascii_lowercase().as_str() {
                "auto" | "true" | "on" => true,
                "off" | "false" | "none" => false,
                other => return Err(CamelError::illegal_argument(format!("anthropic: promptCaching must be auto or off, got {}", other))),
            },
        };
        let tools = ToolLoopOptions::take(params)?.with_context(ctx);
        let timeout = params.take_duration("timeout", Duration::from_secs(120))?;
        params.take("lazyStartProducer");
        Ok(Arc::new(AnthropicEndpoint { uri: uri.to_string(), model: Arc::new(AnthropicModel { cfg, rest: Rest::new(timeout) }), tools }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The Messages API as a [`ChatModel`].
pub struct AnthropicModel {
    cfg: Config,
    rest: Rest,
}

impl AnthropicModel {
    fn post(&self, body: &Value) -> Result<Value> {
        let mut req = Req::post(join_url(&self.cfg.base_url, "/v1/messages")).json(body).header("anthropic-version", self.cfg.version.clone());
        if let Some(k) = &self.cfg.api_key {
            req = req.header("x-api-key", k.clone());
        }
        let r = self.rest.send(req)?;
        if !r.is_success() {
            let m = r.json().ok().and_then(|v| v.get("error").and_then(|e| e.get("message")).map(|m| m.to_string())).unwrap_or_else(|| r.text());
            return Err(CamelError::new(kinds::HTTP_OPERATION_FAILED, format!("HTTP operation failed invoking Anthropic /v1/messages with statusCode: {}: {}", r.status, m)));
        }
        r.json()
    }
}

impl ChatModel for AnthropicModel {
    fn model_id(&self) -> String {
        self.cfg.model.clone().unwrap_or_default()
    }
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        let model = self.cfg.model.clone().ok_or_else(|| CamelError::illegal_argument("anthropic: the model option is required"))?;
        let mut r = request.clone();
        if r.system.is_none() {
            r.system = self.cfg.system_message.clone();
        }
        if r.max_tokens.is_none() {
            r.max_tokens = Some(self.cfg.max_tokens);
        }
        let mut body = ai::wire::to_anthropic(&model, &r);
        if let (Some(t), Some(m)) = (self.cfg.temperature, body.as_object_mut()) {
            m.insert("temperature".into(), Value::Float(t));
        }
        if self.cfg.prompt_caching {
            ai::wire::anthropic_cache_breakpoints(&mut body);
        }
        ai::wire::from_anthropic(&self.post(&body)?)
    }
}

/// An `anthropic` endpoint (producer, and a model for the `agent` step).
pub struct AnthropicEndpoint {
    uri: String,
    model: Arc<AnthropicModel>,
    tools: ToolLoopOptions,
}

impl Endpoint for AnthropicEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(AnthropicProducer { model: self.model.clone(), tools: self.tools.clone() }))
    }
    fn chat_model(&self) -> Option<Arc<dyn ChatModel>> {
        self.tools.wrap(self.model.clone()).ok()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct AnthropicProducer {
    model: Arc<AnthropicModel>,
    tools: ToolLoopOptions,
}

impl Service for AnthropicProducer {}

impl Processor for AnthropicProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let mut model = self.model.clone();
        if let Some(m) = header_str(ex, MODEL) {
            model = Arc::new(AnthropicModel { cfg: Config { model: Some(m), ..model.cfg.clone() }, rest: model.rest.clone() });
        }
        if let Value::Object(o) = ex.body() {
            // the raw API: the body is the request, the response becomes the body
            let mut req = o.clone();
            if let Some(m) = &model.cfg.model {
                req.entry("model".into()).or_insert(Value::from(m.as_str()));
            }
            req.entry("max_tokens".into()).or_insert(Value::Int(model.cfg.max_tokens));
            let resp = model.post(&Value::Object(req))?;
            ex.set_header(STOP_REASON, resp.get("stop_reason").cloned().unwrap_or(Value::Null));
            ex.set_body(resp);
            return Ok(());
        }
        let system = header_str(ex, SYSTEM_MESSAGE).or_else(|| model.cfg.system_message.clone());
        let run = ai::chat_exchange(ex, model, system, &self.tools)?;
        ex.set_header(INPUT_TOKENS, run.usage.input);
        ex.set_header(OUTPUT_TOKENS, run.usage.output);
        ex.set_header(STOP_REASON, if run.stop == ai::StopReason::Answer { "end_turn" } else { run.stop.as_str() });
        Ok(())
    }
}
