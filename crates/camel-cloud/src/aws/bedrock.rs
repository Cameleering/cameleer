//! The `aws-bedrock` (also `aws2-bedrock`) and `aws-bedrock-agent-runtime` components (Camel
//! `camel-aws-bedrock`, producers) over the Bedrock Runtime and Bedrock Agent Runtime REST
//! APIs (signing name `bedrock`).
//!
//! `aws-bedrock://label?modelId=anthropic.claude-3-haiku-20240307-v1:0&operation=...`
//!
//! Operations (`operation` / `CamelAwsBedrockOperation`):
//! * `invokeTextModel` (default), `invokeImageModel`, `invokeEmbeddingsModel` (all
//!   `InvokeModel`): the body (map → JSON, or the raw text / bytes) is posted to the model;
//!   content types from `CamelAwsBedrockContentType` / `CamelAwsBedrockAcceptContentType`
//!   (default `application/json`). Body = the parsed JSON response (or raw bytes when not JSON).
//! * `converse`: messages from `CamelAwsBedrockConverseMessages` (list of `{role, content}`
//!   where content is text or a list of content blocks) or the body (a text becomes one user
//!   message, a map is sent as the request), `CamelAwsBedrockConverseSystem` (text or blocks),
//!   `CamelAwsBedrockConverseInferenceConfig` (map); body = the output text,
//!   `CamelAwsBedrockConverseStopReason`, `CamelAwsBedrockConverseUsage`,
//!   `CamelAwsBedrockConverseOutputMessage`.
//!
//! `aws-bedrock-agent-runtime://label?knowledgeBaseId=KB123&operation=...`:
//! * `retrieve` (default): the body text is the query (`CamelAwsBedrockAgentRuntimeNumberOfResults`);
//!   body = list of retrieval results (`{content: {text}, location, score}`).
//! * `retrieveAndGenerate`: with `modelId` (model ARN, or id expanded to
//!   `arn:aws:bedrock:{region}::foundation-model/{id}`) and optional
//!   `CamelAwsBedrockAgentRuntimeSessionId`; body = generated text,
//!   `CamelAwsBedrockAgentRuntimeSessionId` / `...Citations` headers.

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::{header_json, need_or, AwsConfig};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_i64, header_str, kinds, obj, parse_json, uri_encode, Opts};

/// Header names (Camel `BedrockConstants` / `BedrockAgentRuntimeConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsBedrockOperation";
    pub const MODEL_ID: &str = "CamelAwsBedrockModelId";
    pub const CONTENT_TYPE: &str = "CamelAwsBedrockContentType";
    pub const ACCEPT_CONTENT_TYPE: &str = "CamelAwsBedrockAcceptContentType";
    pub const CONVERSE_MESSAGES: &str = "CamelAwsBedrockConverseMessages";
    pub const CONVERSE_SYSTEM: &str = "CamelAwsBedrockConverseSystem";
    pub const CONVERSE_INFERENCE_CONFIG: &str = "CamelAwsBedrockConverseInferenceConfig";
    pub const CONVERSE_STOP_REASON: &str = "CamelAwsBedrockConverseStopReason";
    pub const CONVERSE_USAGE: &str = "CamelAwsBedrockConverseUsage";
    pub const CONVERSE_OUTPUT_MESSAGE: &str = "CamelAwsBedrockConverseOutputMessage";
    pub const AGENT_OPERATION: &str = "CamelAwsBedrockAgentRuntimeOperation";
    pub const KNOWLEDGE_BASE_ID: &str = "CamelAwsBedrockAgentRuntimeKnowledgeBaseId";
    pub const NUMBER_OF_RESULTS: &str = "CamelAwsBedrockAgentRuntimeNumberOfResults";
    pub const SESSION_ID: &str = "CamelAwsBedrockAgentRuntimeSessionId";
    pub const CITATIONS: &str = "CamelAwsBedrockAgentRuntimeCitations";
}

/// The Bedrock Runtime endpoint configuration and client.
pub struct BedrockService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub model_id: Option<String>,
}

/// Factory for `aws-bedrock`.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["bedrockRuntimeClient", "pojoRequest"]);
    Ok(Arc::new(BedrockService { operation: o.get("operation"), model_id: o.get("modelId"), aws }))
}

fn content_blocks(v: &Value) -> Value {
    match v {
        Value::Array(_) => v.clone(),
        Value::Object(_) => Value::Array(vec![v.clone()]),
        other => Value::Array(vec![obj([("text", Value::from(other.to_string()))])]),
    }
}

impl BedrockService {
    fn model(&self, ex: &Exchange) -> Result<String> {
        need_or(ex, headers::MODEL_ID, &self.model_id, "modelId")
    }

    fn converse(&self, ex: &mut Exchange) -> Result<()> {
        let model = self.model(ex)?;
        let mut req = match (header_json(ex, headers::CONVERSE_MESSAGES), ex.body()) {
            (Some(msgs), _) => obj([("messages", msgs)]),
            (None, Value::Object(m)) => Value::Object(m.clone()),
            (None, other) => obj([("messages", Value::Array(vec![obj([("role", Value::from("user")), ("content", content_blocks(other))])]))]),
        };
        if let Value::Object(m) = &mut req {
            if let Some(Value::Array(msgs)) = m.get_mut("messages") {
                for msg in msgs.iter_mut() {
                    if let Value::Object(mm) = msg {
                        if let Some(c) = mm.get("content").cloned() {
                            mm.insert("content".into(), content_blocks(&c));
                        }
                    }
                }
            }
            if let Some(s) = header_json(ex, headers::CONVERSE_SYSTEM) {
                m.insert("system".into(), content_blocks(&s));
            }
            if let Some(c) = header_json(ex, headers::CONVERSE_INFERENCE_CONFIG) {
                m.insert("inferenceConfig".into(), c);
            }
        }
        let r = self.aws.rest_json("POST", "bedrock-runtime", &format!("/model/{}/converse", uri_encode(&model, false)), &[], Some(&req), "bedrock", kinds::BEDROCK)?;
        let message = r.get("output").and_then(|o| o.get("message")).cloned().unwrap_or_default();
        let text: String = message
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| blocks.iter().filter_map(|b| b.get("text").map(|t| t.to_string())).collect::<Vec<_>>().join(""))
            .unwrap_or_default();
        ex.set_header(headers::CONVERSE_STOP_REASON, r.get("stopReason").cloned().unwrap_or_default());
        ex.set_header(headers::CONVERSE_USAGE, r.get("usage").cloned().unwrap_or_default());
        ex.set_header(headers::CONVERSE_OUTPUT_MESSAGE, message);
        ex.set_body(text);
        Ok(())
    }
}

impl CloudService for BedrockService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "invokeTextModel");
        match op.as_str() {
            "invokeTextModel" | "invokeImageModel" | "invokeEmbeddingsModel" | "invokeModel" => {
                let model = self.model(ex)?;
                let ct = header_str(ex, headers::CONTENT_TYPE).unwrap_or_else(|| "application/json".into());
                let accept = header_str(ex, headers::ACCEPT_CONTENT_TYPE).unwrap_or_else(|| "application/json".into());
                let body = match ex.body() {
                    Value::Object(_) | Value::Array(_) => ex.body().to_json().into_bytes(),
                    other => other.to_bytes(),
                };
                let resp = self.aws.rest_call("POST", "bedrock-runtime", &format!("/model/{}/invoke", uri_encode(&model, false)), &[], Some((&ct, body)), &[("Accept", accept)], "bedrock", kinds::BEDROCK)?;
                let json = resp.header("Content-Type").is_some_and(|c| c.contains("json"));
                ex.set_body(if json { parse_json(&resp.body)? } else { Value::Bytes(resp.body) });
            }
            "converse" => self.converse(ex)?,
            other => return Err(unsupported("Bedrock", other)),
        }
        Ok(())
    }
}

/// The Bedrock Agent Runtime endpoint configuration and client.
pub struct BedrockAgentRuntimeService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub knowledge_base_id: Option<String>,
    pub model_id: Option<String>,
}

/// Factory for `aws-bedrock-agent-runtime`.
pub fn create_agent_runtime(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["bedrockAgentRuntimeClient", "pojoRequest"]);
    Ok(Arc::new(BedrockAgentRuntimeService { operation: o.get("operation"), knowledge_base_id: o.get("knowledgeBaseId"), model_id: o.get("modelId"), aws }))
}

impl CloudService for BedrockAgentRuntimeService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::AGENT_OPERATION, &self.operation, "retrieve");
        let kb = need_or(ex, headers::KNOWLEDGE_BASE_ID, &self.knowledge_base_id, "knowledgeBaseId")?;
        let text = ex.body().to_string();
        if text.trim().is_empty() {
            return Err(CamelError::illegal_argument("The body must contain the query text"));
        }
        match op.as_str() {
            "retrieve" => {
                let mut m = ObjectMap::new();
                m.insert("retrievalQuery".into(), obj([("text", Value::from(text))]));
                if let Some(n) = header_i64(ex, headers::NUMBER_OF_RESULTS) {
                    m.insert("retrievalConfiguration".into(), obj([("vectorSearchConfiguration", obj([("numberOfResults", Value::Int(n))]))]));
                }
                let r = self.aws.rest_json("POST", "bedrock-agent-runtime", &format!("/knowledgebases/{}/retrieve", uri_encode(&kb, false)), &[], Some(&Value::Object(m)), "bedrock", kinds::BEDROCK_AGENT)?;
                ex.set_body(r.get("retrievalResults").cloned().unwrap_or(Value::Array(vec![])));
            }
            "retrieveAndGenerate" => {
                let model = need_or(ex, headers::MODEL_ID, &self.model_id, "modelId")?;
                let arn = if model.starts_with("arn:") { model } else { format!("arn:aws:bedrock:{}::foundation-model/{}", self.aws.region, model) };
                let mut m = ObjectMap::new();
                m.insert("input".into(), obj([("text", Value::from(text))]));
                m.insert(
                    "retrieveAndGenerateConfiguration".into(),
                    obj([("type", Value::from("KNOWLEDGE_BASE")), ("knowledgeBaseConfiguration", obj([("knowledgeBaseId", Value::from(kb)), ("modelArn", Value::from(arn))]))]),
                );
                if let Some(s) = header_str(ex, headers::SESSION_ID) {
                    m.insert("sessionId".into(), Value::from(s));
                }
                let r = self.aws.rest_json("POST", "bedrock-agent-runtime", "/retrieveAndGenerate", &[], Some(&Value::Object(m)), "bedrock", kinds::BEDROCK_AGENT)?;
                if let Some(s) = r.get("sessionId") {
                    ex.set_header(headers::SESSION_ID, s.clone());
                }
                ex.set_header(headers::CITATIONS, r.get("citations").cloned().unwrap_or(Value::Array(vec![])));
                ex.set_body(r.get("output").and_then(|o| o.get("text")).cloned().unwrap_or_default());
            }
            other => return Err(unsupported("Bedrock Agent Runtime", other)),
        }
        Ok(())
    }
}
