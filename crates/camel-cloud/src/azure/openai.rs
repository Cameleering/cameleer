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
        match op.as_str() {
            "chatCompletion" => {
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
            other => return Err(unsupported("Azure OpenAI", other)),
        }
        Ok(())
    }
}
