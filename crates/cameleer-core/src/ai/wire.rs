//! Wire formats: the neutral [`ChatRequest`] / [`ChatResponse`] to and from each provider's
//! JSON (OpenAI Chat Completions, Amazon Bedrock Converse, Google Gemini, Anthropic Messages).

use crate::error::{CamelError, Result};
use crate::value::{ObjectMap, Value};

use super::{ChatMessage, ChatRequest, ChatResponse, Part, Role, Usage};

fn obj<const N: usize>(items: [(&str, Value); N]) -> Value {
    Value::object(items)
}

fn n(v: Option<&Value>) -> i64 {
    v.and_then(|x| x.as_i64()).unwrap_or(0)
}

fn parse_args(v: Option<&Value>) -> Value {
    match v {
        Some(Value::String(s)) if !s.trim().is_empty() => crate::json::parse(s).unwrap_or(Value::String(s.clone())),
        Some(Value::Object(o)) => Value::Object(o.clone()),
        _ => Value::Object(ObjectMap::new()),
    }
}

fn args_object(v: &Value) -> Value {
    match v {
        Value::Object(_) => v.clone(),
        _ => Value::Object(ObjectMap::new()),
    }
}

// --- OpenAI Chat Completions (also Azure OpenAI, Ollama, vLLM, LM Studio) ---

/// The Chat Completions request body.
pub fn to_openai(model: Option<&str>, req: &ChatRequest) -> Value {
    let mut msgs: Vec<Value> = Vec::new();
    if let Some(s) = &req.system {
        msgs.push(obj([("role", Value::from("system")), ("content", Value::from(s.as_str()))]));
    }
    for m in &req.messages {
        match m.role {
            Role::User => {
                // tool messages must directly follow the assistant message that called them
                for p in &m.parts {
                    if let Part::ToolResult { id, content, .. } = p {
                        msgs.push(obj([("role", Value::from("tool")), ("tool_call_id", Value::from(id.as_str())), ("content", Value::from(content.as_str()))]));
                    }
                }
                let text = m.text();
                if !text.is_empty() || m.parts.iter().all(|p| matches!(p, Part::Text(_))) {
                    msgs.push(obj([("role", Value::from("user")), ("content", Value::from(text))]));
                }
            }
            Role::Assistant => {
                let calls: Vec<Value> = m
                    .tool_uses()
                    .into_iter()
                    .map(|(id, name, args)| {
                        obj([("id", Value::from(id)), ("type", Value::from("function")), ("function", obj([("name", Value::from(name)), ("arguments", Value::from(args_object(args).to_json()))]))])
                    })
                    .collect();
                let text = m.text();
                let mut a = ObjectMap::new();
                a.insert("role".into(), Value::from("assistant"));
                a.insert("content".into(), if text.is_empty() && !calls.is_empty() { Value::Null } else { Value::from(text) });
                if !calls.is_empty() {
                    a.insert("tool_calls".into(), Value::Array(calls));
                }
                msgs.push(Value::Object(a));
            }
        }
    }
    let mut body = ObjectMap::new();
    if let Some(m) = model {
        body.insert("model".into(), Value::from(m));
    }
    body.insert("messages".into(), Value::Array(msgs));
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| obj([("type", Value::from("function")), ("function", obj([("name", Value::from(t.name.as_str())), ("description", Value::from(t.description.as_str())), ("parameters", t.input_schema.clone())]))]))
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    if let Some(m) = req.max_tokens {
        body.insert("max_tokens".into(), Value::Int(m));
    }
    Value::Object(body)
}

/// A Chat Completions response (missing call ids, as from some Ollama versions, are generated).
pub fn from_openai(resp: &Value) -> Result<ChatResponse> {
    let choice = resp.get("choices").and_then(|c| c.as_array()).and_then(|c| c.first()).ok_or_else(|| CamelError::illegal_state("the chat response has no choices"))?;
    let m = choice.get("message").cloned().unwrap_or(Value::Null);
    let mut parts = Vec::new();
    if let Some(t) = m.get("content").filter(|c| !c.is_null()).map(|c| c.to_string()).filter(|t| !t.is_empty()) {
        parts.push(Part::Text(t));
    }
    if let Some(Value::Array(calls)) = m.get("tool_calls") {
        for (i, c) in calls.iter().enumerate() {
            let f = c.get("function").cloned().unwrap_or(Value::Null);
            let id = c.get("id").map(|v| v.to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("call_{}", i + 1));
            parts.push(Part::ToolUse { id, name: f.get("name").map(|v| v.to_string()).unwrap_or_default(), arguments: parse_args(f.get("arguments")), meta: None });
        }
    }
    let usage = resp.get("usage");
    Ok(ChatResponse {
        message: ChatMessage { role: Role::Assistant, parts },
        stop_reason: choice.get("finish_reason").map(|f| f.to_string()).unwrap_or_default(),
        usage: Usage { input: n(usage.and_then(|u| u.get("prompt_tokens"))), output: n(usage.and_then(|u| u.get("completion_tokens"))) },
    })
}

// --- Amazon Bedrock Converse ---

/// The Converse request body (`modelId` goes in the path).
pub fn to_converse(req: &ChatRequest) -> Value {
    let mut msgs = Vec::new();
    for m in &req.messages {
        let content: Vec<Value> = m
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) if t.is_empty() => None,
                Part::Text(t) => Some(obj([("text", Value::from(t.as_str()))])),
                Part::ToolUse { id, name, arguments, .. } => Some(obj([("toolUse", obj([("toolUseId", Value::from(id.as_str())), ("name", Value::from(name.as_str())), ("input", args_object(arguments))]))])),
                Part::ToolResult { id, content, is_error, .. } => {
                    let mut r = ObjectMap::new();
                    r.insert("toolUseId".into(), Value::from(id.as_str()));
                    r.insert("content".into(), Value::Array(vec![obj([("text", Value::from(content.as_str()))])]));
                    if *is_error {
                        r.insert("status".into(), Value::from("error"));
                    }
                    Some(obj([("toolResult", Value::Object(r))]))
                }
            })
            .collect();
        if !content.is_empty() {
            msgs.push(obj([("role", Value::from(m.role.as_str())), ("content", Value::Array(content))]));
        }
    }
    let mut body = ObjectMap::new();
    body.insert("messages".into(), Value::Array(msgs));
    if let Some(s) = &req.system {
        body.insert("system".into(), Value::Array(vec![obj([("text", Value::from(s.as_str()))])]));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| obj([("toolSpec", obj([("name", Value::from(t.name.as_str())), ("description", Value::from(t.description.as_str())), ("inputSchema", obj([("json", t.input_schema.clone())]))]))]))
            .collect();
        body.insert("toolConfig".into(), obj([("tools", Value::Array(tools))]));
    }
    if let Some(m) = req.max_tokens {
        body.insert("inferenceConfig".into(), obj([("maxTokens", Value::Int(m))]));
    }
    Value::Object(body)
}

/// A Converse response.
pub fn from_converse(resp: &Value) -> Result<ChatResponse> {
    let content = resp
        .get("output")
        .and_then(|o| o.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
        .cloned()
        .ok_or_else(|| CamelError::illegal_state("the Converse response has no output message"))?;
    let mut parts = Vec::new();
    for c in content {
        if let Some(t) = c.get("text") {
            parts.push(Part::Text(t.to_string()));
        } else if let Some(u) = c.get("toolUse") {
            parts.push(Part::ToolUse {
                id: u.get("toolUseId").map(|v| v.to_string()).unwrap_or_default(),
                name: u.get("name").map(|v| v.to_string()).unwrap_or_default(),
                arguments: args_object(u.get("input").unwrap_or(&Value::Null)),
                meta: None,
            });
        }
    }
    let usage = resp.get("usage");
    Ok(ChatResponse {
        message: ChatMessage { role: Role::Assistant, parts },
        stop_reason: resp.get("stopReason").map(|s| s.to_string()).unwrap_or_default(),
        usage: Usage { input: n(usage.and_then(|u| u.get("inputTokens"))), output: n(usage.and_then(|u| u.get("outputTokens"))) },
    })
}

// --- Google Gemini (Vertex AI generateContent) ---

/// The `generateContent` request body. A function call's `thoughtSignature` (kept in the
/// part's `meta`) is sent back with it, as Gemini requires.
pub fn to_gemini(req: &ChatRequest) -> Value {
    let mut contents = Vec::new();
    for m in &req.messages {
        let role = if m.role == Role::Assistant { "model" } else { "user" };
        let parts: Vec<Value> = m
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) if t.is_empty() => None,
                Part::Text(t) => Some(obj([("text", Value::from(t.as_str()))])),
                Part::ToolUse { name, arguments, meta, .. } => {
                    let mut x = ObjectMap::new();
                    x.insert("functionCall".into(), obj([("name", Value::from(name.as_str())), ("args", args_object(arguments))]));
                    if let Some(sig) = meta.as_ref().and_then(|m| m.get("thoughtSignature")) {
                        x.insert("thoughtSignature".into(), sig.clone());
                    }
                    Some(Value::Object(x))
                }
                Part::ToolResult { name, content, is_error, .. } => {
                    let key = if *is_error { "error" } else { "content" };
                    Some(obj([("functionResponse", obj([("name", Value::from(name.as_str())), ("response", obj([(key, Value::from(content.as_str()))]))]))]))
                }
            })
            .collect();
        if !parts.is_empty() {
            contents.push(obj([("role", Value::from(role)), ("parts", Value::Array(parts))]));
        }
    }
    let mut body = ObjectMap::new();
    body.insert("contents".into(), Value::Array(contents));
    if let Some(s) = &req.system {
        body.insert("systemInstruction".into(), obj([("parts", Value::Array(vec![obj([("text", Value::from(s.as_str()))])]))]));
    }
    if !req.tools.is_empty() {
        let decls: Vec<Value> = req
            .tools
            .iter()
            .map(|t| obj([("name", Value::from(t.name.as_str())), ("description", Value::from(t.description.as_str())), ("parameters", t.input_schema.clone())]))
            .collect();
        body.insert("tools".into(), Value::Array(vec![obj([("functionDeclarations", Value::Array(decls))])]));
    }
    if let Some(m) = req.max_tokens {
        body.insert("generationConfig".into(), obj([("maxOutputTokens", Value::Int(m))]));
    }
    Value::Object(body)
}

/// A `generateContent` response. Gemini has no call ids, so they are generated.
pub fn from_gemini(resp: &Value) -> Result<ChatResponse> {
    let cand = resp.get("candidates").and_then(|c| c.as_array()).and_then(|c| c.first()).ok_or_else(|| CamelError::illegal_state("the Gemini response has no candidates"))?;
    let mut parts = Vec::new();
    let mut calls = 0;
    for p in cand.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()).cloned().unwrap_or_default() {
        if let Some(fc) = p.get("functionCall") {
            calls += 1;
            let meta = p.get("thoughtSignature").map(|s| obj([("thoughtSignature", s.clone())]));
            parts.push(Part::ToolUse {
                id: format!("call_{}", calls),
                name: fc.get("name").map(|v| v.to_string()).unwrap_or_default(),
                arguments: args_object(fc.get("args").unwrap_or(&Value::Null)),
                meta,
            });
        } else if let Some(t) = p.get("text") {
            if !p.get("thought").map(|v| v.is_truthy()).unwrap_or(false) {
                parts.push(Part::Text(t.to_string()));
            }
        }
    }
    let usage = resp.get("usageMetadata");
    Ok(ChatResponse {
        message: ChatMessage { role: Role::Assistant, parts },
        stop_reason: cand.get("finishReason").map(|s| s.to_string()).unwrap_or_default(),
        usage: Usage { input: n(usage.and_then(|u| u.get("promptTokenCount"))), output: n(usage.and_then(|u| u.get("candidatesTokenCount"))) },
    })
}

// --- Anthropic Messages ---

/// The Messages API request body (`max_tokens` is required there; 1024 by default).
pub fn to_anthropic(model: &str, req: &ChatRequest) -> Value {
    let mut msgs = Vec::new();
    for m in &req.messages {
        let content: Vec<Value> = m
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) if t.is_empty() => None,
                Part::Text(t) => Some(obj([("type", Value::from("text")), ("text", Value::from(t.as_str()))])),
                Part::ToolUse { id, name, arguments, .. } => {
                    Some(obj([("type", Value::from("tool_use")), ("id", Value::from(id.as_str())), ("name", Value::from(name.as_str())), ("input", args_object(arguments))]))
                }
                Part::ToolResult { id, content, is_error, .. } => Some(obj([
                    ("type", Value::from("tool_result")),
                    ("tool_use_id", Value::from(id.as_str())),
                    ("content", Value::from(content.as_str())),
                    ("is_error", Value::Bool(*is_error)),
                ])),
            })
            .collect();
        if !content.is_empty() {
            msgs.push(obj([("role", Value::from(m.role.as_str())), ("content", Value::Array(content))]));
        }
    }
    let mut body = ObjectMap::new();
    body.insert("model".into(), Value::from(model));
    body.insert("max_tokens".into(), Value::Int(req.max_tokens.unwrap_or(1024)));
    if let Some(s) = &req.system {
        body.insert("system".into(), Value::from(s.as_str()));
    }
    body.insert("messages".into(), Value::Array(msgs));
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| obj([("name", Value::from(t.name.as_str())), ("description", Value::from(t.description.as_str())), ("input_schema", t.input_schema.clone())]))
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    Value::Object(body)
}

/// A Messages API response.
pub fn from_anthropic(resp: &Value) -> Result<ChatResponse> {
    let content = resp.get("content").and_then(|c| c.as_array()).cloned().ok_or_else(|| CamelError::illegal_state("the Messages response has no content"))?;
    let mut parts = Vec::new();
    for c in content {
        match c.get("type").and_then(|t| t.as_str()) {
            Some("text") => parts.push(Part::Text(c.get("text").map(|t| t.to_string()).unwrap_or_default())),
            Some("tool_use") => parts.push(Part::ToolUse {
                id: c.get("id").map(|v| v.to_string()).unwrap_or_default(),
                name: c.get("name").map(|v| v.to_string()).unwrap_or_default(),
                arguments: args_object(c.get("input").unwrap_or(&Value::Null)),
                meta: None,
            }),
            _ => {}
        }
    }
    let usage = resp.get("usage");
    Ok(ChatResponse {
        message: ChatMessage { role: Role::Assistant, parts },
        stop_reason: resp.get("stop_reason").map(|s| s.to_string()).unwrap_or_default(),
        usage: Usage { input: n(usage.and_then(|u| u.get("input_tokens"))), output: n(usage.and_then(|u| u.get("output_tokens"))) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ToolSpec;

    fn conversation() -> ChatRequest {
        ChatRequest {
            system: Some("be brief".into()),
            messages: vec![
                ChatMessage::user("weather in Oslo?"),
                ChatMessage {
                    role: Role::Assistant,
                    parts: vec![Part::ToolUse { id: "t1".into(), name: "weather".into(), arguments: Value::object([("city", Value::from("Oslo"))]), meta: Some(Value::object([("thoughtSignature", Value::from("sig"))])) }],
                },
                ChatMessage { role: Role::User, parts: vec![Part::ToolResult { id: "t1".into(), name: "weather".into(), content: "21C".into(), is_error: false }] },
            ],
            tools: vec![ToolSpec { name: "weather".into(), description: "Weather".into(), input_schema: Value::object([("type", Value::from("object"))]) }],
            max_tokens: Some(100),
        }
    }

    #[test]
    fn openai_shapes() {
        let b = to_openai(Some("m"), &conversation());
        assert_eq!(
            b.to_json(),
            r#"{"model":"m","messages":[{"role":"system","content":"be brief"},{"role":"user","content":"weather in Oslo?"},{"role":"assistant","content":null,"tool_calls":[{"id":"t1","type":"function","function":{"name":"weather","arguments":"{\"city\":\"Oslo\"}"}}]},{"role":"tool","tool_call_id":"t1","content":"21C"}],"tools":[{"type":"function","function":{"name":"weather","description":"Weather","parameters":{"type":"object"}}}],"max_tokens":100}"#
        );
        let r = from_openai(&crate::json::parse(r#"{"choices":[{"message":{"content":null,"tool_calls":[{"function":{"name":"w","arguments":"{\"a\":1}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#).unwrap()).unwrap();
        assert_eq!(r.message.tool_uses()[0].0, "call_1");
        assert_eq!(r.usage, Usage { input: 3, output: 2 });
    }

    #[test]
    fn converse_shapes() {
        let b = to_converse(&conversation());
        assert!(b.to_json().contains(r#"{"toolResult":{"toolUseId":"t1","content":[{"text":"21C"}]}}"#), "{}", b.to_json());
        assert!(b.to_json().contains(r#""toolConfig":{"tools":[{"toolSpec":{"name":"weather","description":"Weather","inputSchema":{"json":{"type":"object"}}}}]}"#));
        let r = from_converse(&crate::json::parse(r#"{"output":{"message":{"role":"assistant","content":[{"text":"x"},{"toolUse":{"toolUseId":"u1","name":"w","input":{"a":1}}}]}},"stopReason":"tool_use","usage":{"inputTokens":5,"outputTokens":6}}"#).unwrap()).unwrap();
        assert_eq!((r.message.text().as_str(), r.message.tool_uses()[0].0, r.usage.output), ("x", "u1", 6));
    }

    #[test]
    fn gemini_keeps_the_thought_signature() {
        let b = to_gemini(&conversation());
        let j = b.to_json();
        assert!(j.contains(r#"{"functionCall":{"name":"weather","args":{"city":"Oslo"}},"thoughtSignature":"sig"}"#), "{}", j);
        assert!(j.contains(r#"{"functionResponse":{"name":"weather","response":{"content":"21C"}}}"#));
        assert!(j.contains(r#""role":"model""#));
        let r = from_gemini(&crate::json::parse(r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"w","args":{}},"thoughtSignature":"abc"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":2}}"#).unwrap()).unwrap();
        match &r.message.parts[0] {
            Part::ToolUse { meta, id, .. } => assert_eq!((id.as_str(), meta.as_ref().unwrap().to_json().as_str()), ("call_1", r#"{"thoughtSignature":"abc"}"#)),
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn anthropic_shapes() {
        let b = to_anthropic("claude", &conversation());
        let j = b.to_json();
        assert!(j.contains(r#"{"type":"tool_result","tool_use_id":"t1","content":"21C","is_error":false}"#), "{}", j);
        assert!(j.contains(r#""system":"be brief""#) && j.contains(r#""max_tokens":100"#));
        let r = from_anthropic(&crate::json::parse(r#"{"content":[{"type":"text","text":"ok"},{"type":"tool_use","id":"toolu_1","name":"w","input":{"q":1}}],"stop_reason":"tool_use","usage":{"input_tokens":7,"output_tokens":8}}"#).unwrap()).unwrap();
        assert_eq!((r.message.tool_uses()[0].0, r.stop_reason.as_str(), r.usage.input), ("toolu_1", "tool_use", 7));
    }
}
