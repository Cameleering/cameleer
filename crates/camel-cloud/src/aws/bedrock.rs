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
//!   `CamelAwsBedrockConverseInferenceConfig` (map), `CamelAwsBedrockConverseToolConfig`
//!   (map); body = the output text, `CamelAwsBedrockConverseStopReason`,
//!   `CamelAwsBedrockConverseUsage`, `CamelAwsBedrockConverseOutputMessage`.
//! * Streaming (AWS event stream framing decoded incrementally with CRC checks, see
//!   [`super::eventstream`]): `invokeTextModelStreaming`, `invokeImageModelStreaming`,
//!   `invokeEmbeddingsModelStreaming` (`InvokeModelWithResponseStream`) and `converseStream`
//!   (`ConverseStream`). `streamOutputMode` / `CamelAwsBedrockStreamOutputMode` = `complete`
//!   (default: body = the concatenated text) or `chunks` (body = list of the text chunks);
//!   headers `CamelAwsBedrockStreamingChunkCount`, `CamelAwsBedrockStreamingCompletionReason`,
//!   `CamelAwsBedrockStreamingTokenCount` (converseStream also sets the converse stop reason /
//!   usage / output message headers). Each chunk can also be handed to a processor while the
//!   response is still streaming: `streamingChunkProcessor=#bean` (a `Processor` called with a
//!   copy of the exchange whose body is the chunk text, `CamelAwsBedrockStreamingChunkIndex`).
//! * `applyGuardrail`: `guardrailIdentifier` / `guardrailVersion` (options or
//!   `CamelAwsBedrockGuardrailIdentifier` / `...Version` headers), the body text checked as
//!   `CamelAwsBedrockGuardrailSource` (`INPUT` default / `OUTPUT`); body = the response,
//!   `CamelAwsBedrockGuardrailAction` header. A configured guardrail (+ `guardrailTrace`) is
//!   also applied to the invoke / converse requests.
//!
//! `aws-bedrock-agent-runtime://label?knowledgeBaseId=KB123&operation=...`:
//! * `retrieve` (default): the body text is the query (`CamelAwsBedrockAgentRuntimeNumberOfResults`);
//!   body = list of retrieval results (`{content: {text}, location, score}`).
//! * `retrieveAndGenerate`: with `modelId` (model ARN, or id expanded to
//!   `arn:aws:bedrock:{region}::foundation-model/{id}`) and optional
//!   `CamelAwsBedrockAgentRuntimeSessionId`; body = generated text,
//!   `CamelAwsBedrockAgentRuntimeSessionId` / `...Citations` headers.
//! * `invokeAgent`: `agentId` / `agentAliasId` (options or `CamelAwsBedrockAgentRuntimeAgentId` /
//!   `...AgentAliasId`), session `CamelAwsBedrockAgentRuntimeSessionId` (generated when
//!   missing), `...EnableTrace`, `...EndSession`, `...SessionAttributes` /
//!   `...PromptSessionAttributes` (maps); the event stream response (chunk / trace /
//!   returnControl events) is decoded: body = the agent's answer, `...SessionId`,
//!   `...Citations`, `...Traces` and `...ReturnControl` headers.

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::Processor;
use camel_core::value::{ObjectMap, Value};

use super::eventstream::Message;
use super::{header_json, need_or, AwsConfig};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{base64_decode, header_i64, header_str, kinds, obj, parse_json, uri_encode, uuid, Opts};

/// Header names (Camel `BedrockConstants` / `BedrockAgentRuntimeConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsBedrockOperation";
    pub const MODEL_ID: &str = "CamelAwsBedrockModelId";
    pub const CONTENT_TYPE: &str = "CamelAwsBedrockContentType";
    pub const ACCEPT_CONTENT_TYPE: &str = "CamelAwsBedrockAcceptContentType";
    pub const CONVERSE_MESSAGES: &str = "CamelAwsBedrockConverseMessages";
    pub const CONVERSE_SYSTEM: &str = "CamelAwsBedrockConverseSystem";
    pub const CONVERSE_INFERENCE_CONFIG: &str = "CamelAwsBedrockConverseInferenceConfig";
    pub const CONVERSE_TOOL_CONFIG: &str = "CamelAwsBedrockConverseToolConfig";
    pub const CONVERSE_STOP_REASON: &str = "CamelAwsBedrockConverseStopReason";
    pub const CONVERSE_USAGE: &str = "CamelAwsBedrockConverseUsage";
    pub const CONVERSE_OUTPUT_MESSAGE: &str = "CamelAwsBedrockConverseOutputMessage";
    pub const STREAM_OUTPUT_MODE: &str = "CamelAwsBedrockStreamOutputMode";
    pub const STREAMING_CHUNK_COUNT: &str = "CamelAwsBedrockStreamingChunkCount";
    pub const STREAMING_COMPLETION_REASON: &str = "CamelAwsBedrockStreamingCompletionReason";
    pub const STREAMING_TOKEN_COUNT: &str = "CamelAwsBedrockStreamingTokenCount";
    pub const STREAMING_CHUNK_INDEX: &str = "CamelAwsBedrockStreamingChunkIndex";
    pub const GUARDRAIL_IDENTIFIER: &str = "CamelAwsBedrockGuardrailIdentifier";
    pub const GUARDRAIL_VERSION: &str = "CamelAwsBedrockGuardrailVersion";
    pub const GUARDRAIL_SOURCE: &str = "CamelAwsBedrockGuardrailSource";
    pub const GUARDRAIL_ACTION: &str = "CamelAwsBedrockGuardrailAction";
    pub const AGENT_OPERATION: &str = "CamelAwsBedrockAgentRuntimeOperation";
    pub const KNOWLEDGE_BASE_ID: &str = "CamelAwsBedrockAgentRuntimeKnowledgeBaseId";
    pub const NUMBER_OF_RESULTS: &str = "CamelAwsBedrockAgentRuntimeNumberOfResults";
    pub const SESSION_ID: &str = "CamelAwsBedrockAgentRuntimeSessionId";
    pub const CITATIONS: &str = "CamelAwsBedrockAgentRuntimeCitations";
    pub const AGENT_ID: &str = "CamelAwsBedrockAgentRuntimeAgentId";
    pub const AGENT_ALIAS_ID: &str = "CamelAwsBedrockAgentRuntimeAgentAliasId";
    pub const ENABLE_TRACE: &str = "CamelAwsBedrockAgentRuntimeEnableTrace";
    pub const END_SESSION: &str = "CamelAwsBedrockAgentRuntimeEndSession";
    pub const SESSION_ATTRIBUTES: &str = "CamelAwsBedrockAgentRuntimeSessionAttributes";
    pub const PROMPT_SESSION_ATTRIBUTES: &str = "CamelAwsBedrockAgentRuntimePromptSessionAttributes";
    pub const TRACES: &str = "CamelAwsBedrockAgentRuntimeTraces";
    pub const RETURN_CONTROL: &str = "CamelAwsBedrockAgentRuntimeReturnControl";
}

/// The Bedrock Runtime endpoint configuration and client.
pub struct BedrockService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub model_id: Option<String>,
    pub stream_output_mode: String,
    pub guardrail_identifier: Option<String>,
    pub guardrail_version: Option<String>,
    pub guardrail_trace: bool,
    pub chunk_processor: Option<Arc<dyn Processor>>,
}

/// Factory for `aws-bedrock`.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["bedrockRuntimeClient", "bedrockRuntimeAsyncClient", "pojoRequest", "includeStreamingMetadata"]);
    let mode = o.string("streamOutputMode", "complete");
    if !matches!(mode.as_str(), "complete" | "chunks") {
        return Err(CamelError::illegal_argument(format!("Invalid streamOutputMode: {} (complete or chunks)", mode)));
    }
    let chunk_processor = match o.get("streamingChunkProcessor") {
        Some(r) => Some(o.context().registry().lookup_processor(r.trim_start_matches('#')).ok_or_else(|| CamelError::illegal_argument(format!("No Processor bean {} in the registry", r)))?),
        None => None,
    };
    Ok(Arc::new(BedrockService {
        operation: o.get("operation"),
        model_id: o.get("modelId"),
        stream_output_mode: mode,
        guardrail_identifier: o.get("guardrailIdentifier"),
        guardrail_version: o.get("guardrailVersion"),
        guardrail_trace: o.bool("guardrailTrace", false)?,
        chunk_processor,
        aws,
    }))
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

    fn guardrail(&self, ex: &Exchange) -> Option<(String, String)> {
        let id = header_str(ex, headers::GUARDRAIL_IDENTIFIER).or_else(|| self.guardrail_identifier.clone())?;
        let version = header_str(ex, headers::GUARDRAIL_VERSION).or_else(|| self.guardrail_version.clone()).unwrap_or_else(|| "DRAFT".into());
        Some((id, version))
    }

    fn converse_request(&self, ex: &Exchange) -> Value {
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
            if let Some(c) = header_json(ex, headers::CONVERSE_TOOL_CONFIG) {
                m.insert("toolConfig".into(), c);
            }
            if let Some((id, version)) = self.guardrail(ex) {
                let trace = if self.guardrail_trace { "enabled" } else { "disabled" };
                m.insert("guardrailConfig".into(), obj([("guardrailIdentifier", Value::from(id)), ("guardrailVersion", Value::from(version)), ("trace", Value::from(trace))]));
            }
        }
        req
    }

    fn converse(&self, ex: &mut Exchange) -> Result<()> {
        let model = self.model(ex)?;
        let req = self.converse_request(ex);
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

    fn output_mode(&self, ex: &Exchange) -> String {
        header_str(ex, headers::STREAM_OUTPUT_MODE).unwrap_or_else(|| self.stream_output_mode.clone())
    }

    /// Hand a chunk to the `streamingChunkProcessor`.
    fn on_chunk(&self, ex: &Exchange, index: usize, text: &str) -> Result<()> {
        if let Some(p) = &self.chunk_processor {
            let mut c = ex.clone();
            c.set_body(text.to_string());
            c.set_header(headers::STREAMING_CHUNK_INDEX, index as i64);
            p.process(&mut c)?;
        }
        Ok(())
    }

    fn finish_stream(&self, ex: &mut Exchange, chunks: Vec<String>, reason: Option<String>, tokens: Option<i64>) {
        ex.set_header(headers::STREAMING_CHUNK_COUNT, chunks.len() as i64);
        if let Some(r) = reason {
            ex.set_header(headers::STREAMING_COMPLETION_REASON, r);
        }
        if let Some(t) = tokens {
            ex.set_header(headers::STREAMING_TOKEN_COUNT, t);
        }
        if self.output_mode(ex) == "chunks" {
            ex.set_body(Value::Array(chunks.into_iter().map(Value::from).collect()));
        } else {
            ex.set_body(chunks.concat());
        }
    }

    fn invoke_body(ex: &Exchange) -> Vec<u8> {
        match ex.body() {
            Value::Object(_) | Value::Array(_) => ex.body().to_json().into_bytes(),
            other => other.to_bytes(),
        }
    }

    fn invoke(&self, ex: &mut Exchange) -> Result<()> {
        let model = self.model(ex)?;
        let ct = header_str(ex, headers::CONTENT_TYPE).unwrap_or_else(|| "application/json".into());
        let accept = header_str(ex, headers::ACCEPT_CONTENT_TYPE).unwrap_or_else(|| "application/json".into());
        let body = Self::invoke_body(ex);
        let mut extra = vec![("Accept", accept)];
        if let Some((id, version)) = self.guardrail(ex) {
            extra.push(("X-Amzn-Bedrock-GuardrailIdentifier", id));
            extra.push(("X-Amzn-Bedrock-GuardrailVersion", version));
            extra.push(("X-Amzn-Bedrock-Trace", if self.guardrail_trace { "ENABLED".into() } else { "DISABLED".into() }));
        }
        let resp = self.aws.rest_call("POST", "bedrock-runtime", &format!("/model/{}/invoke", uri_encode(&model, false)), &[], Some((&ct, body)), &extra, "bedrock", kinds::BEDROCK)?;
        let json = resp.header("Content-Type").is_some_and(|c| c.contains("json"));
        ex.set_body(if json { parse_json(&resp.body)? } else { Value::Bytes(resp.body) });
        Ok(())
    }

    fn invoke_streaming(&self, ex: &mut Exchange) -> Result<()> {
        let model = self.model(ex)?;
        let body = parse_json(&Self::invoke_body(ex)).unwrap_or(Value::Null);
        let (mut chunks, mut reason, mut tokens) = (Vec::new(), None, None);
        let snapshot = ex.clone();
        let path = format!("/model/{}/invoke-with-response-stream", uri_encode(&model, false));
        self.aws.rest_event_stream("POST", "bedrock-runtime", &path, Some(&body), "bedrock", kinds::BEDROCK, &mut |m: Message| {
            if m.event_type() != "chunk" {
                return Ok(());
            }
            let b64 = m.json()?.get("bytes").map(|b| b.to_string()).unwrap_or_default();
            let chunk = parse_json(&base64_decode(&b64).unwrap_or_default()).unwrap_or(Value::Null);
            let (text, r, t) = chunk_text(&chunk);
            if r.is_some() {
                reason = r;
            }
            if t.is_some() {
                tokens = t;
            }
            if let Some(text) = text {
                self.on_chunk(&snapshot, chunks.len(), &text)?;
                chunks.push(text);
            }
            Ok(())
        })?;
        self.finish_stream(ex, chunks, reason, tokens);
        Ok(())
    }

    fn converse_stream(&self, ex: &mut Exchange) -> Result<()> {
        let model = self.model(ex)?;
        let req = self.converse_request(ex);
        let (mut chunks, mut reason, mut usage) = (Vec::new(), None, Value::Null);
        let snapshot = ex.clone();
        let path = format!("/model/{}/converse-stream", uri_encode(&model, false));
        self.aws.rest_event_stream("POST", "bedrock-runtime", &path, Some(&req), "bedrock", kinds::BEDROCK, &mut |m: Message| {
            let v = m.json()?;
            match m.event_type().as_str() {
                "contentBlockDelta" => {
                    if let Some(t) = v.get("delta").and_then(|d| d.get("text")) {
                        let t = t.to_string();
                        self.on_chunk(&snapshot, chunks.len(), &t)?;
                        chunks.push(t);
                    }
                }
                "messageStop" => reason = v.get("stopReason").map(|r| r.to_string()),
                "metadata" => usage = v.get("usage").cloned().unwrap_or_default(),
                _ => {}
            }
            Ok(())
        })?;
        let tokens = usage.get("outputTokens").and_then(|t| t.as_i64());
        ex.set_header(headers::CONVERSE_STOP_REASON, reason.clone().map(Value::from).unwrap_or_default());
        ex.set_header(headers::CONVERSE_USAGE, usage);
        let text = chunks.concat();
        ex.set_header(headers::CONVERSE_OUTPUT_MESSAGE, obj([("role", Value::from("assistant")), ("content", Value::Array(vec![obj([("text", Value::from(text))])]))]));
        self.finish_stream(ex, chunks, reason, tokens);
        Ok(())
    }

    fn apply_guardrail(&self, ex: &mut Exchange) -> Result<()> {
        let (id, version) = self.guardrail(ex).ok_or_else(|| CamelError::illegal_argument("applyGuardrail requires guardrailIdentifier"))?;
        let source = header_str(ex, headers::GUARDRAIL_SOURCE).unwrap_or_else(|| "INPUT".into());
        let req = obj([("source", Value::from(source)), ("content", Value::Array(vec![obj([("text", obj([("text", Value::from(ex.body().to_string()))]))])]))]);
        let path = format!("/guardrail/{}/version/{}/apply", uri_encode(&id, false), uri_encode(&version, false));
        let r = self.aws.rest_json("POST", "bedrock-runtime", &path, &[], Some(&req), "bedrock", kinds::BEDROCK)?;
        ex.set_header(headers::GUARDRAIL_ACTION, r.get("action").cloned().unwrap_or_default());
        ex.set_body(r);
        Ok(())
    }
}

impl CloudService for BedrockService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "invokeTextModel");
        match op.as_str() {
            "invokeTextModel" | "invokeImageModel" | "invokeEmbeddingsModel" | "invokeModel" => self.invoke(ex),
            "converse" => self.converse(ex),
            "invokeTextModelStreaming" | "invokeImageModelStreaming" | "invokeEmbeddingsModelStreaming" | "invokeModelWithResponseStream" => self.invoke_streaming(ex),
            "converseStream" => self.converse_stream(ex),
            "applyGuardrail" => self.apply_guardrail(ex),
            other => Err(unsupported("Bedrock", other)),
        }
    }
}

/// Text, completion reason and token count of one `InvokeModelWithResponseStream` chunk
/// (Anthropic messages / completions, Amazon Titan / Nova, Meta Llama, Mistral, Cohere).
pub fn chunk_text(c: &Value) -> (Option<String>, Option<String>, Option<i64>) {
    let s = |v: Option<&Value>| v.filter(|x| !x.is_null()).map(|x| x.to_string());
    let metrics = c.get("amazon-bedrock-invocationMetrics").and_then(|m| m.get("outputTokenCount")).and_then(|t| t.as_i64());
    if let Some(t) = c.get("type").map(|t| t.to_string()) {
        return match t.as_str() {
            "content_block_delta" => (s(c.get("delta").and_then(|d| d.get("text"))), None, metrics),
            "message_delta" => (None, s(c.get("delta").and_then(|d| d.get("stop_reason"))), c.get("usage").and_then(|u| u.get("output_tokens")).and_then(|t| t.as_i64()).or(metrics)),
            _ => (None, None, metrics),
        };
    }
    if let Some(d) = c.get("contentBlockDelta") {
        return (s(d.get("delta").and_then(|x| x.get("text"))), None, metrics);
    }
    if let Some(m) = c.get("messageStop") {
        return (None, s(m.get("stopReason")), metrics);
    }
    let tokens = c.get("totalOutputTextTokenCount").and_then(|t| t.as_i64()).or_else(|| c.get("generation_token_count").and_then(|t| t.as_i64())).or(metrics);
    for (text, reason) in [("outputText", "completionReason"), ("completion", "stop_reason"), ("generation", "stop_reason"), ("text", "finish_reason")] {
        if c.get(text).is_some() {
            return (s(c.get(text)), s(c.get(reason)), tokens);
        }
    }
    if let Some(Value::Array(o)) = c.get("outputs") {
        let first = o.first();
        return (s(first.and_then(|x| x.get("text"))), s(first.and_then(|x| x.get("stop_reason"))), tokens);
    }
    if let Some(e) = c.get("embedding") {
        return (Some(e.to_json()), None, tokens);
    }
    (None, None, tokens)
}

/// The Bedrock Agent Runtime endpoint configuration and client.
pub struct BedrockAgentRuntimeService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub knowledge_base_id: Option<String>,
    pub model_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_alias_id: Option<String>,
}

/// Factory for `aws-bedrock-agent-runtime`.
pub fn create_agent_runtime(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["bedrockAgentRuntimeClient", "pojoRequest"]);
    Ok(Arc::new(BedrockAgentRuntimeService {
        operation: o.get("operation"),
        knowledge_base_id: o.get("knowledgeBaseId"),
        model_id: o.get("modelId"),
        agent_id: o.get("agentId"),
        agent_alias_id: o.get("agentAliasId"),
        aws,
    }))
}

impl BedrockAgentRuntimeService {
    fn invoke_agent(&self, ex: &mut Exchange, text: String) -> Result<()> {
        let agent = need_or(ex, headers::AGENT_ID, &self.agent_id, "agentId")?;
        let alias = need_or(ex, headers::AGENT_ALIAS_ID, &self.agent_alias_id, "agentAliasId")?;
        let session = header_str(ex, headers::SESSION_ID).unwrap_or_else(uuid);
        let mut m = ObjectMap::new();
        m.insert("inputText".into(), Value::from(text));
        let flag = |h: &str| header_str(ex, h).is_some_and(|v| v.eq_ignore_ascii_case("true"));
        m.insert("enableTrace".into(), Value::Bool(flag(headers::ENABLE_TRACE)));
        m.insert("endSession".into(), Value::Bool(flag(headers::END_SESSION)));
        let mut state = ObjectMap::new();
        if let Some(a) = header_json(ex, headers::SESSION_ATTRIBUTES) {
            state.insert("sessionAttributes".into(), a);
        }
        if let Some(a) = header_json(ex, headers::PROMPT_SESSION_ATTRIBUTES) {
            state.insert("promptSessionAttributes".into(), a);
        }
        if !state.is_empty() {
            m.insert("sessionState".into(), Value::Object(state));
        }
        let path = format!("/agents/{}/agentAliases/{}/sessions/{}/text", uri_encode(&agent, false), uri_encode(&alias, false), uri_encode(&session, false));
        let (mut answer, mut citations, mut traces, mut control) = (String::new(), Vec::new(), Vec::new(), None);
        let resp = self.aws.rest_event_stream("POST", "bedrock-agent-runtime", &path, Some(&Value::Object(m)), "bedrock", kinds::BEDROCK_AGENT, &mut |msg: Message| {
            let v = msg.json()?;
            match msg.event_type().as_str() {
                "chunk" => {
                    let bytes = v.get("bytes").and_then(|b| base64_decode(&b.to_string())).unwrap_or_default();
                    answer.push_str(&String::from_utf8_lossy(&bytes));
                    if let Some(Value::Array(c)) = v.get("attribution").and_then(|a| a.get("citations")) {
                        citations.extend(c.iter().cloned());
                    }
                }
                "trace" => traces.push(v.get("trace").cloned().unwrap_or(v)),
                "returnControl" => control = Some(v),
                _ => {}
            }
            Ok(())
        })?;
        let sid = resp.header("x-amz-bedrock-agent-session-id").map(|s| s.to_string()).unwrap_or(session);
        ex.set_header(headers::SESSION_ID, sid);
        ex.set_header(headers::CITATIONS, Value::Array(citations));
        ex.set_header(headers::TRACES, Value::Array(traces));
        if let Some(c) = control {
            ex.set_header(headers::RETURN_CONTROL, c);
        }
        ex.set_body(answer);
        Ok(())
    }
}

impl CloudService for BedrockAgentRuntimeService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::AGENT_OPERATION, &self.operation, "retrieve");
        let text = ex.body().to_string();
        if text.trim().is_empty() {
            return Err(CamelError::illegal_argument("The body must contain the query text"));
        }
        let kb = |ex: &Exchange| need_or(ex, headers::KNOWLEDGE_BASE_ID, &self.knowledge_base_id, "knowledgeBaseId");
        match op.as_str() {
            "retrieve" => {
                let kb = kb(ex)?;
                let mut m = ObjectMap::new();
                m.insert("retrievalQuery".into(), obj([("text", Value::from(text))]));
                if let Some(n) = header_i64(ex, headers::NUMBER_OF_RESULTS) {
                    m.insert("retrievalConfiguration".into(), obj([("vectorSearchConfiguration", obj([("numberOfResults", Value::Int(n))]))]));
                }
                let r = self.aws.rest_json("POST", "bedrock-agent-runtime", &format!("/knowledgebases/{}/retrieve", uri_encode(&kb, false)), &[], Some(&Value::Object(m)), "bedrock", kinds::BEDROCK_AGENT)?;
                ex.set_body(r.get("retrievalResults").cloned().unwrap_or(Value::Array(vec![])));
            }
            "retrieveAndGenerate" => {
                let kb = kb(ex)?;
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
            "invokeAgent" => self.invoke_agent(ex, text)?,
            other => return Err(unsupported("Bedrock Agent Runtime", other)),
        }
        Ok(())
    }
}
