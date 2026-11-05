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
