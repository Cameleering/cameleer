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
                let reasoning = m.parts.iter().find_map(|p| match p {
                    Part::ToolUse { meta: Some(meta), .. } => meta.get("reasoning_content").cloned(),
                    _ => None,
                });
                if let Some(r) = reasoning {
                    a.insert("reasoning_content".into(), r);
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
            // DeepSeek, Qwen and Kimi want their reasoning back with the calls during a tool loop
            let meta = if i == 0 { m.get("reasoning_content").filter(|r| !r.is_null()).map(|r| obj([("reasoning_content", r.clone())])) } else { None };
            parts.push(Part::ToolUse { id, name: f.get("name").map(|v| v.to_string()).unwrap_or_default(), arguments: parse_args(f.get("arguments")), meta });
        }
    }
    let usage = resp.get("usage");
    Ok(ChatResponse {
        message: ChatMessage { role: Role::Assistant, parts },
        stop_reason: choice.get("finish_reason").map(|f| f.to_string()).unwrap_or_default(),
        usage: Usage {
            input: n(usage.and_then(|u| u.get("prompt_tokens"))),
            output: n(usage.and_then(|u| u.get("completion_tokens"))),
            cache_read: n(usage.and_then(|u| u.get("prompt_tokens_details")).and_then(|d| d.get("cached_tokens")).or_else(|| usage.and_then(|u| u.get("prompt_cache_hit_tokens")))),
            cache_write: 0,
        },
    })
}

/// Prompt caching for the Messages API: `cache_control` breakpoints on the last tool, the
/// system prompt and the last message, so the stable prefix (tools, system, history) is read
/// from Anthropic's cache on the next turn. Prompts below the model's minimum are simply not
/// cached.
pub fn anthropic_cache_breakpoints(body: &mut Value) {
    let Some(m) = body.as_object_mut() else { return };
    let mark = || obj([("type", Value::from("ephemeral"))]);
    if let Some(Value::Array(tools)) = m.get_mut("tools") {
        if let Some(Value::Object(t)) = tools.last_mut() {
            t.insert("cache_control".into(), mark());
        }
    }
    if let Some(Value::String(s)) = m.get("system").cloned() {
        m.insert("system".into(), Value::Array(vec![obj([("type", Value::from("text")), ("text", Value::from(s)), ("cache_control", mark())])]));
    }
    if let Some(Value::Array(msgs)) = m.get_mut("messages") {
        if let Some(Value::Object(last)) = msgs.last_mut() {
            if let Some(Value::String(s)) = last.get("content").cloned() {
                last.insert("content".into(), Value::Array(vec![obj([("type", Value::from("text")), ("text", Value::from(s))])]));
            }
            if let Some(Value::Array(blocks)) = last.get_mut("content") {
                if let Some(Value::Object(b)) = blocks.last_mut() {
                    b.insert("cache_control".into(), mark());
                }
            }
        }
    }
}

/// Prompt caching for Bedrock Converse: `cachePoint` blocks after the tools, the system prompt
/// and the last message (for models that support it; others reject the request).
pub fn converse_cache_points(body: &mut Value) {
    let Some(m) = body.as_object_mut() else { return };
    let point = || obj([("cachePoint", obj([("type", Value::from("default"))]))]);
    if let Some(Value::Object(tc)) = m.get_mut("toolConfig") {
        if let Some(Value::Array(tools)) = tc.get_mut("tools") {
            if !tools.is_empty() {
                tools.push(point());
            }
        }
    }
    if let Some(Value::Array(sys)) = m.get_mut("system") {
        if !sys.is_empty() {
            sys.push(point());
        }
    }
    if let Some(Value::Array(msgs)) = m.get_mut("messages") {
        if let Some(Value::Object(last)) = msgs.last_mut() {
            if let Some(Value::Array(c)) = last.get_mut("content") {
                c.push(point());
            }
        }
    }
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
        usage: Usage {
            input: n(usage.and_then(|u| u.get("inputTokens"))),
            output: n(usage.and_then(|u| u.get("outputTokens"))),
            cache_read: n(usage.and_then(|u| u.get("cacheReadInputTokens"))),
            cache_write: n(usage.and_then(|u| u.get("cacheWriteInputTokens"))),
        },
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
        usage: Usage {
            input: n(usage.and_then(|u| u.get("promptTokenCount"))),
            output: n(usage.and_then(|u| u.get("candidatesTokenCount"))),
            cache_read: n(usage.and_then(|u| u.get("cachedContentTokenCount"))),
            cache_write: 0,
        },
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
        usage: Usage {
            input: n(usage.and_then(|u| u.get("input_tokens"))),
            output: n(usage.and_then(|u| u.get("output_tokens"))),
            cache_read: n(usage.and_then(|u| u.get("cache_read_input_tokens"))),
            cache_write: n(usage.and_then(|u| u.get("cache_creation_input_tokens"))),
        },
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
        assert_eq!(r.usage, Usage::new(3, 2));
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

// --- OpenAI Responses API ---

/// The Responses API request body (`POST /responses`): the system message is `instructions`,
/// the conversation is `input` items (messages, `function_call` and `function_call_output`).
pub fn to_responses(model: Option<&str>, req: &ChatRequest) -> Value {
    let mut input: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::User => {
                for p in &m.parts {
                    if let Part::ToolResult { id, content, .. } = p {
                        input.push(obj([("type", Value::from("function_call_output")), ("call_id", Value::from(id.as_str())), ("output", Value::from(content.as_str()))]));
                    }
                }
                let text = m.text();
                if !text.is_empty() || m.parts.iter().all(|p| matches!(p, Part::Text(_))) {
                    input.push(obj([("type", Value::from("message")), ("role", Value::from("user")), ("content", Value::from(text))]));
                }
            }
            Role::Assistant => {
                let text = m.text();
                if !text.is_empty() {
                    input.push(obj([("type", Value::from("message")), ("role", Value::from("assistant")), ("content", Value::from(text))]));
                }
                for (id, name, args) in m.tool_uses() {
                    input.push(obj([
                        ("type", Value::from("function_call")),
                        ("call_id", Value::from(id)),
                        ("name", Value::from(name)),
                        ("arguments", Value::from(args_object(args).to_json())),
                    ]));
                }
            }
        }
    }
    let mut body = ObjectMap::new();
    if let Some(m) = model {
        body.insert("model".into(), Value::from(m));
    }
    if let Some(s) = &req.system {
        body.insert("instructions".into(), Value::from(s.as_str()));
    }
    body.insert("input".into(), Value::Array(input));
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| obj([("type", Value::from("function")), ("name", Value::from(t.name.as_str())), ("description", Value::from(t.description.as_str())), ("parameters", t.input_schema.clone())]))
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    if let Some(m) = req.max_tokens {
        body.insert("max_output_tokens".into(), Value::Int(m));
    }
    Value::Object(body)
}

/// A Responses API response: `output_text` content of `message` items and `function_call`
/// items (their `call_id` is the call id); the stop reason is `status` (`completed`, or
/// `incomplete: <reason>`).
pub fn from_responses(resp: &Value) -> Result<ChatResponse> {
    if let Some(e) = resp.get("error").filter(|e| !e.is_null()) {
        return Err(CamelError::illegal_state(format!("the response failed: {}", e.get("message").map(|m| m.to_string()).unwrap_or_else(|| e.to_json()))));
    }
    let output = resp.get("output").and_then(|o| o.as_array()).cloned().ok_or_else(|| CamelError::illegal_state("the Responses API response has no output"))?;
    let mut parts = Vec::new();
    let mut calls = 0;
    for item in output {
        match item.get("type").and_then(|t| t.as_str()) {
            Some("message") => {
                let text: String = match item.get("content") {
                    Some(Value::Array(content)) => content
                        .iter()
                        .filter(|c| matches!(c.get("type").and_then(|t| t.as_str()), Some("output_text") | Some("text")))
                        .filter_map(|c| c.get("text").map(|t| t.to_string()))
                        .collect(),
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                };
                if !text.is_empty() {
                    parts.push(Part::Text(text));
                }
            }
            Some("function_call") => {
                calls += 1;
                let id = item.get("call_id").or_else(|| item.get("id")).map(|v| v.to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("call_{}", calls));
                parts.push(Part::ToolUse { id, name: item.get("name").map(|v| v.to_string()).unwrap_or_default(), arguments: parse_args(item.get("arguments")), meta: None });
            }
            _ => {}
        }
    }
    let status = resp.get("status").map(|s| s.to_string()).unwrap_or_default();
    let stop_reason = match resp.get("incomplete_details").and_then(|d| d.get("reason")).filter(|r| !r.is_null()) {
        Some(r) if status == "incomplete" => format!("incomplete: {}", r),
        _ => status,
    };
    let usage = resp.get("usage");
    Ok(ChatResponse {
        message: ChatMessage { role: Role::Assistant, parts },
        stop_reason,
        usage: Usage {
            input: n(usage.and_then(|u| u.get("input_tokens"))),
            output: n(usage.and_then(|u| u.get("output_tokens"))),
            cache_read: n(usage.and_then(|u| u.get("input_tokens_details")).and_then(|d| d.get("cached_tokens"))),
            cache_write: 0,
        },
    })
}

// --- Ollama native chat API ---

/// Ollama's `POST /api/chat` body (not streamed): tool call arguments are objects, tool
/// results are `tool` messages with `tool_name`, `max_tokens` is `options.num_predict`.
pub fn to_ollama(model: Option<&str>, req: &ChatRequest) -> Value {
    let mut msgs: Vec<Value> = Vec::new();
    if let Some(s) = &req.system {
        msgs.push(obj([("role", Value::from("system")), ("content", Value::from(s.as_str()))]));
    }
    for m in &req.messages {
        match m.role {
            Role::User => {
                for p in &m.parts {
                    if let Part::ToolResult { name, content, .. } = p {
                        msgs.push(obj([("role", Value::from("tool")), ("content", Value::from(content.as_str())), ("tool_name", Value::from(name.as_str()))]));
                    }
                }
                let text = m.text();
                if !text.is_empty() || m.parts.iter().all(|p| matches!(p, Part::Text(_))) {
                    msgs.push(obj([("role", Value::from("user")), ("content", Value::from(text))]));
                }
            }
            Role::Assistant => {
                let calls: Vec<Value> = m.tool_uses().into_iter().map(|(_, name, args)| obj([("function", obj([("name", Value::from(name)), ("arguments", args_object(args))]))])).collect();
                let mut a = ObjectMap::new();
                a.insert("role".into(), Value::from("assistant"));
                a.insert("content".into(), Value::from(m.text()));
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
    body.insert("stream".into(), Value::Bool(false));
    if let Some(m) = req.max_tokens {
        body.insert("options".into(), obj([("num_predict", Value::Int(m))]));
    }
    Value::Object(body)
}

/// An Ollama `/api/chat` response. Ollama has no call ids, so they are generated (`call_N`);
/// usage is `prompt_eval_count` / `eval_count`, the stop reason `done_reason`.
pub fn from_ollama(resp: &Value) -> Result<ChatResponse> {
    if let Some(e) = resp.get("error").filter(|e| !e.is_null()) {
        return Err(CamelError::illegal_state(format!("Ollama failed: {}", e)));
    }
    let m = resp.get("message").filter(|m| m.as_object().is_some()).ok_or_else(|| CamelError::illegal_state("the Ollama response has no message"))?;
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
    Ok(ChatResponse {
        message: ChatMessage { role: Role::Assistant, parts },
        stop_reason: resp.get("done_reason").map(|f| f.to_string()).unwrap_or_default(),
        usage: Usage::new(n(resp.get("prompt_eval_count")), n(resp.get("eval_count"))),
    })
}

#[cfg(test)]
mod responses_and_ollama_tests {
    use super::*;
    use crate::ai::ToolSpec;

    fn conversation() -> ChatRequest {
        ChatRequest {
            system: Some("be brief".into()),
            messages: vec![
                ChatMessage::user("weather in Oslo?"),
                ChatMessage { role: Role::Assistant, parts: vec![Part::ToolUse { id: "t1".into(), name: "weather".into(), arguments: Value::object([("city", Value::from("Oslo"))]), meta: None }] },
                ChatMessage { role: Role::User, parts: vec![Part::ToolResult { id: "t1".into(), name: "weather".into(), content: "21C".into(), is_error: false }] },
            ],
            tools: vec![ToolSpec { name: "weather".into(), description: "Weather".into(), input_schema: Value::object([("type", Value::from("object"))]) }],
            max_tokens: Some(100),
        }
    }

    #[test]
    fn responses_shapes() {
        let b = to_responses(Some("m"), &conversation());
        assert_eq!(
            b.to_json(),
            r#"{"model":"m","instructions":"be brief","input":[{"type":"message","role":"user","content":"weather in Oslo?"},{"type":"function_call","call_id":"t1","name":"weather","arguments":"{\"city\":\"Oslo\"}"},{"type":"function_call_output","call_id":"t1","output":"21C"}],"tools":[{"type":"function","name":"weather","description":"Weather","parameters":{"type":"object"}}],"max_output_tokens":100}"#
        );
        let r = from_responses(
            &crate::json::parse(r#"{"id":"resp_1","status":"completed","output":[{"type":"reasoning","summary":[]},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Let me "},{"type":"output_text","text":"check."}]},{"type":"function_call","id":"fc_1","call_id":"call_a","name":"weather","arguments":"{\"city\":\"Oslo\"}"}],"usage":{"input_tokens":9,"output_tokens":3}}"#).unwrap(),
        )
        .unwrap();
        assert_eq!(r.message.text(), "Let me check.");
        let city = Value::object([("city", Value::from("Oslo"))]);
        assert_eq!(r.message.tool_uses(), vec![("call_a", "weather", &city)]);
        assert_eq!((r.stop_reason.as_str(), r.usage), ("completed", Usage::new(9, 3)));
        let r = from_responses(&crate::json::parse(r#"{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[]}"#).unwrap()).unwrap();
        assert_eq!(r.stop_reason, "incomplete: max_output_tokens");
        assert!(from_responses(&crate::json::parse(r#"{"status":"failed","error":{"message":"boom"},"output":[]}"#).unwrap()).is_err());
        assert!(from_responses(&crate::json::parse(r#"{"status":"completed"}"#).unwrap()).is_err());
    }

    #[test]
    fn ollama_shapes() {
        let b = to_ollama(Some("llama3.2"), &conversation());
        assert_eq!(
            b.to_json(),
            r#"{"model":"llama3.2","messages":[{"role":"system","content":"be brief"},{"role":"user","content":"weather in Oslo?"},{"role":"assistant","content":"","tool_calls":[{"function":{"name":"weather","arguments":{"city":"Oslo"}}}]},{"role":"tool","content":"21C","tool_name":"weather"}],"tools":[{"type":"function","function":{"name":"weather","description":"Weather","parameters":{"type":"object"}}}],"stream":false,"options":{"num_predict":100}}"#
        );
        let r = from_ollama(&crate::json::parse(r#"{"model":"llama3.2","message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"a","arguments":{"x":1}}},{"function":{"name":"b","arguments":{}}}]},"done":true,"done_reason":"stop","prompt_eval_count":20,"eval_count":7}"#).unwrap()).unwrap();
        let ids: Vec<&str> = r.message.tool_uses().iter().map(|(i, _, _)| *i).collect();
        assert_eq!(ids, vec!["call_1", "call_2"]);
        assert_eq!((r.stop_reason.as_str(), r.usage), ("stop", Usage::new(20, 7)));
        assert!(r.message.text().is_empty());
        let r = from_ollama(&crate::json::parse(r#"{"message":{"role":"assistant","content":"hi"},"done_reason":"stop"}"#).unwrap()).unwrap();
        assert_eq!(r.message.text(), "hi");
        assert!(from_ollama(&crate::json::parse(r#"{"error":"model not found"}"#).unwrap()).is_err());
    }

    #[test]
    fn reasoning_content_and_cache_markers() {
        // DeepSeek-style reasoning comes back with the calls on the next turn
        let r = from_openai(&crate::json::parse(r#"{"choices":[{"message":{"role":"assistant","content":null,"reasoning_content":"think","tool_calls":[{"id":"c1","type":"function","function":{"name":"f","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":6}}}"#).unwrap()).unwrap();
        assert_eq!(r.usage, Usage { input: 10, output: 2, cache_read: 6, cache_write: 0 });
        let req = ChatRequest { messages: vec![ChatMessage::user("q"), r.message], ..ChatRequest::default() };
        let body = to_openai(Some("m"), &req).to_json();
        assert!(body.contains(r#""reasoning_content":"think""#), "{}", body);
        // Converse cache points after tools, system and the last message
        let mut v = crate::json::parse(r#"{"system":[{"text":"s"}],"messages":[{"role":"user","content":[{"text":"q"}]}],"toolConfig":{"tools":[{"toolSpec":{"name":"f"}}]}}"#).unwrap();
        converse_cache_points(&mut v);
        let j = v.to_json();
        assert_eq!(j.matches(r#"{"cachePoint":{"type":"default"}}"#).count(), 3, "{}", j);
        // Anthropic usage with cache reads and writes
        let r = from_anthropic(&crate::json::parse(r#"{"content":[{"type":"text","text":"a"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":1,"cache_read_input_tokens":100,"cache_creation_input_tokens":20}}"#).unwrap()).unwrap();
        assert_eq!(r.usage, Usage { input: 3, output: 1, cache_read: 100, cache_write: 20 });
    }
}
