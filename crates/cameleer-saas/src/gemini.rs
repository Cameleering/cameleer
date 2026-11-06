//! `gemini:` component: the Google Gemini API (AI Studio, API keys), with tool calling.
//!
//! `gemini:generate?model=gemini-2.5-flash&apiKey=...`
//!
//! A text body is the user message and becomes the answer; a list body is the message list;
//! an object body is sent as the `generateContent` request and the whole response becomes the
//! body. Tool calling uses the same options as the other model components (`tags`,
//! `autoToolExecution`, `maxToolIterations`, ...): each `functionCall` runs through the
//! `ai-tool` envelope and its `thoughtSignature` is sent back, as Gemini requires. The
//! endpoint is also a model for the `agent` step. For Vertex AI (service accounts, regions)
//! use `google-vertexai`.
//!
//! `apiKey` (or `GEMINI_API_KEY`, then `GOOGLE_API_KEY`) is sent as `x-goog-api-key`.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::ai::{self as ai, ChatModel, ChatRequest, ChatResponse, ToolLoopOptions};
use cameleer_core::context::CamelContext;
use cameleer_core::error::{kinds, CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::{ObjectMap, Value};

use crate::rest::{header_str, join_url, Req, Rest};

pub const SYSTEM_MESSAGE: &str = "CamelGeminiSystemMessage";
pub const MODEL: &str = "CamelGeminiModel";
pub const FINISH_REASON: &str = "CamelGeminiFinishReason";
pub const PROMPT_TOKENS: &str = "CamelGeminiPromptTokenCount";
pub const CANDIDATES_TOKENS: &str = "CamelGeminiCandidatesTokenCount";

/// The `gemini` component.
#[derive(Default)]
pub struct GeminiComponent;

#[derive(Clone, Debug)]
struct Config {
    model: Option<String>,
    api_key: Option<String>,
    base_url: String,
    system_message: Option<String>,
    max_tokens: Option<i64>,
    temperature: Option<f64>,
}

impl Component for GeminiComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        match remaining.trim() {
            "generate" | "generateContent" | "chat" | "" => {}
            other => return Err(CamelError::illegal_argument(format!("gemini: unsupported operation '{}' (generate)", other))),
        }
        let num = |params: &mut Parameters, k: &str| -> Result<Option<f64>> {
            match params.take(k) {
                Some(v) => v.trim().parse().map(Some).map_err(|_| CamelError::illegal_argument(format!("gemini: {} must be a number", k))),
                None => Ok(None),
            }
        };
        let cfg = Config {
            model: params.take("model").filter(|s| !s.is_empty()),
            api_key: params
                .take("apiKey")
                .filter(|s| !s.is_empty())
                .or_else(|| std::env::var("GEMINI_API_KEY").ok())
                .or_else(|| std::env::var("GOOGLE_API_KEY").ok()),
            base_url: params.take("baseUrl").unwrap_or_else(|| "https://generativelanguage.googleapis.com".into()),
            system_message: params.take("systemMessage"),
            max_tokens: num(params, "maxTokens")?.map(|v| v as i64),
            temperature: num(params, "temperature")?,
        };
        let tools = ToolLoopOptions::take(params)?.with_context(ctx);
        let timeout = params.take_duration("timeout", Duration::from_secs(120))?;
        params.take("lazyStartProducer");
        Ok(Arc::new(GeminiEndpoint { uri: uri.to_string(), model: Arc::new(GeminiModel { cfg, rest: Rest::new(timeout) }), tools }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The Gemini API as a [`ChatModel`].
pub struct GeminiModel {
    cfg: Config,
    rest: Rest,
}

impl GeminiModel {
    fn post(&self, model: &str, body: &Value) -> Result<Value> {
        let path = format!("/v1beta/models/{}:generateContent", model);
        let mut req = Req::post(join_url(&self.cfg.base_url, &path)).json(body);
        if let Some(k) = &self.cfg.api_key {
            req = req.header("x-goog-api-key", k.clone());
        }
        let r = self.rest.send(req)?;
        if !r.is_success() {
            let m = r.json().ok().and_then(|v| v.get("error").and_then(|e| e.get("message")).map(|m| m.to_string())).unwrap_or_else(|| r.text());
            return Err(CamelError::new(kinds::HTTP_OPERATION_FAILED, format!("HTTP operation failed invoking Gemini {} with statusCode: {}: {}", path, r.status, m)));
        }
        r.json()
    }

    fn model_name(&self) -> Result<String> {
        self.cfg.model.clone().ok_or_else(|| CamelError::illegal_argument("gemini: the model option is required"))
    }
}

impl ChatModel for GeminiModel {
    fn model_id(&self) -> String {
        self.cfg.model.clone().unwrap_or_default()
    }
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        let mut r = request.clone();
        if r.system.is_none() {
            r.system = self.cfg.system_message.clone();
        }
        if r.max_tokens.is_none() {
            r.max_tokens = self.cfg.max_tokens;
        }
        let mut body = ai::wire::to_gemini(&r);
        if let (Some(t), Some(m)) = (self.cfg.temperature, body.as_object_mut()) {
            let mut g = m.get("generationConfig").and_then(|g| g.as_object()).cloned().unwrap_or_else(ObjectMap::new);
            g.insert("temperature".into(), Value::Float(t));
            m.insert("generationConfig".into(), Value::Object(g));
        }
        ai::wire::from_gemini(&self.post(&self.model_name()?, &body)?)
    }
}

/// A `gemini` endpoint.
pub struct GeminiEndpoint {
    uri: String,
    model: Arc<GeminiModel>,
    tools: ToolLoopOptions,
}

impl Endpoint for GeminiEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(GeminiProducer { model: self.model.clone(), tools: self.tools.clone() }))
    }
    fn chat_model(&self) -> Option<Arc<dyn ChatModel>> {
        self.tools.wrap(self.model.clone()).ok()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct GeminiProducer {
    model: Arc<GeminiModel>,
    tools: ToolLoopOptions,
}

impl Service for GeminiProducer {}

impl Processor for GeminiProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let mut model = self.model.clone();
        if let Some(m) = header_str(ex, MODEL) {
            model = Arc::new(GeminiModel { cfg: Config { model: Some(m), ..model.cfg.clone() }, rest: model.rest.clone() });
        }
        if let Value::Object(o) = ex.body() {
            let resp = model.post(&model.model_name()?, &Value::Object(o.clone()))?;
            let cand = resp.get("candidates").and_then(|c| c.as_array()).and_then(|c| c.first().cloned()).unwrap_or(Value::Null);
            ex.set_header(FINISH_REASON, cand.get("finishReason").cloned().unwrap_or(Value::Null));
            ex.set_body(resp);
            return Ok(());
        }
        let system = header_str(ex, SYSTEM_MESSAGE).or_else(|| model.cfg.system_message.clone());
        let run = ai::chat_exchange(ex, model, system, &self.tools)?;
        ex.set_header(PROMPT_TOKENS, run.usage.input);
        ex.set_header(CANDIDATES_TOKENS, run.usage.output);
        Ok(())
    }
}
