//! Provider-neutral tool calling: one model of messages, tool calls and results that every
//! model component converts on the wire, the [`ToolProvider`] that runs tools (the
//! `cameleer-ai` registry registers one), and the agent loop ([`run_agent`]) shared by the
//! `agent` step and the model components' `autoToolExecution`.
//!
//! Model components expose their chat API through [`crate::spi::Endpoint::chat_model`].

pub mod wire;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{CamelError, Result};
use crate::value::{ObjectMap, Value};

/// The bean name under which the tool provider is bound (`cameleer-ai` binds the registry).
pub const TOOL_PROVIDER_BEAN: &str = "cameleer.ai.tools";

/// Header and property names of the agent step.
pub mod headers {
    /// The conversation an agent call belongs to (memory, compensation, approvals).
    pub const CONVERSATION_ID: &str = "CamelAiConversationId";
    pub const TURNS: &str = "CamelAiAgentTurns";
    pub const INPUT_TOKENS: &str = "CamelAiAgentInputTokens";
    pub const OUTPUT_TOKENS: &str = "CamelAiAgentOutputTokens";
    pub const TOTAL_TOKENS: &str = "CamelAiAgentTotalTokens";
    pub const TOOL_CALLS: &str = "CamelAiAgentToolCalls";
    /// `answer`, `max-turns`, `max-tokens`, `deadline`, `repeated-failure`, `approval-required`,
    /// `approval-denied`, `approval-pending`.
    pub const STOP_REASON: &str = "CamelAiAgentStopReason";
    /// The approval handle when the agent stopped for an approval.
    pub const APPROVAL_HANDLE: &str = "CamelAiApprovalHandle";
}

// ---------------------------------------------------------------------------------------------
// messages

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// One part of a message.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    /// The model asks for a tool. `meta` keeps provider data that must be sent back with the
    /// call (Gemini's `thoughtSignature`).
    ToolUse { id: String, name: String, arguments: Value, meta: Option<Value> },
    /// A tool's answer to a call.
    ToolResult { id: String, name: String, content: String, is_error: bool },
}

/// A message of the conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatMessage {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl ChatMessage {
    pub fn user(text: impl Into<String>) -> ChatMessage {
        ChatMessage { role: Role::User, parts: vec![Part::Text(text.into())] }
    }
    pub fn assistant(text: impl Into<String>) -> ChatMessage {
        ChatMessage { role: Role::Assistant, parts: vec![Part::Text(text.into())] }
    }
    /// The text parts joined.
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
    /// The tool calls of this message.
    pub fn tool_uses(&self) -> Vec<(&str, &str, &Value)> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::ToolUse { id, name, arguments, .. } => Some((id.as_str(), name.as_str(), arguments)),
                _ => None,
            })
            .collect()
    }

    pub fn to_value(&self) -> Value {
        let parts: Vec<Value> = self
            .parts
            .iter()
            .map(|p| match p {
                Part::Text(t) => Value::object([("type", Value::from("text")), ("text", Value::from(t.as_str()))]),
                Part::ToolUse { id, name, arguments, meta } => {
                    let mut m = ObjectMap::new();
                    m.insert("type".into(), Value::from("tool_use"));
                    m.insert("id".into(), Value::from(id.as_str()));
                    m.insert("name".into(), Value::from(name.as_str()));
                    m.insert("arguments".into(), arguments.clone());
                    if let Some(x) = meta {
                        m.insert("meta".into(), x.clone());
                    }
                    Value::Object(m)
                }
                Part::ToolResult { id, name, content, is_error } => Value::object([
                    ("type", Value::from("tool_result")),
                    ("id", Value::from(id.as_str())),
                    ("name", Value::from(name.as_str())),
                    ("content", Value::from(content.as_str())),
                    ("isError", Value::Bool(*is_error)),
                ]),
            })
            .collect();
        Value::object([("role", Value::from(self.role.as_str())), ("parts", Value::Array(parts))])
    }

    pub fn from_value(v: &Value) -> Result<ChatMessage> {
        let m = v.as_object().ok_or_else(|| CamelError::illegal_argument("a chat message must be an object"))?;
        let role = match m.get("role").and_then(|r| r.as_str()) {
            Some("assistant") => Role::Assistant,
            _ => Role::User,
        };
        // the neutral form, or the common {"role", "content": "text"} form
        if let Some(Value::Array(parts)) = m.get("parts") {
            let mut out = Vec::new();
            for p in parts {
                let p = p.as_object().ok_or_else(|| CamelError::illegal_argument("a message part must be an object"))?;
                let s = |k: &str| p.get(k).map(|v| v.to_string()).unwrap_or_default();
                out.push(match p.get("type").and_then(|t| t.as_str()) {
                    Some("tool_use") => Part::ToolUse { id: s("id"), name: s("name"), arguments: p.get("arguments").cloned().unwrap_or(Value::Null), meta: p.get("meta").cloned() },
                    Some("tool_result") => Part::ToolResult { id: s("id"), name: s("name"), content: s("content"), is_error: p.get("isError").map(|v| v.is_truthy()).unwrap_or(false) },
                    _ => Part::Text(s("text")),
                });
            }
            return Ok(ChatMessage { role, parts: out });
        }
        let text = m.get("content").map(|c| c.to_string()).unwrap_or_default();
        Ok(ChatMessage { role, parts: vec![Part::Text(text)] })
    }
}

/// A tool as models see it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments (an object schema).
    pub input_schema: Value,
}

/// A request to a chat model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChatRequest {
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: Option<i64>,
}

/// Token usage of one model call.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
}

/// A model's answer.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatResponse {
    pub message: ChatMessage,
    /// The provider's stop reason as given (`stop`, `tool_calls`, `end_turn`, `tool_use`, ...).
    pub stop_reason: String,
    pub usage: Usage,
}

impl ChatResponse {
    pub fn to_value(&self) -> Value {
        Value::object([
            ("message", self.message.to_value()),
            ("stopReason", Value::from(self.stop_reason.as_str())),
            ("usage", Value::object([("input", Value::Int(self.usage.input)), ("output", Value::Int(self.usage.output))])),
        ])
    }
    pub fn from_value(v: &Value) -> Result<ChatResponse> {
        let m = v.as_object().ok_or_else(|| CamelError::illegal_argument("a recorded response must be an object"))?;
        let usage = m.get("usage").and_then(|u| u.as_object());
        let n = |k: &str| usage.and_then(|u| u.get(k)).and_then(|x| x.as_i64()).unwrap_or(0);
        Ok(ChatResponse {
            message: ChatMessage::from_value(m.get("message").unwrap_or(&Value::Null))?,
            stop_reason: m.get("stopReason").map(|s| s.to_string()).unwrap_or_default(),
            usage: Usage { input: n("input"), output: n("output") },
        })
    }
}

/// A chat model with tool calling (implemented by the model components).
pub trait ChatModel: Send + Sync {
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse>;
}

// ---------------------------------------------------------------------------------------------
// tools

/// A tool call to run.
#[derive(Clone, Debug, Default)]
pub struct ToolInvocation {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    pub conversation_id: Option<String>,
    pub principal: Option<String>,
    /// Properties and variables of the calling exchange.
    pub properties: Vec<(String, Value)>,
}

/// How a tool call ended.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolOutcomeStatus {
    Completed,
    Error,
    /// Waiting for approval, with the handle to resolve it.
    ApprovalRequired(String),
    Denied,
}

/// A tool's answer.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutcome {
    pub text: String,
    pub is_error: bool,
    pub status: ToolOutcomeStatus,
}

/// Lists and runs tools (the `cameleer-ai` registry, through its envelope).
pub trait ToolProvider: Send + Sync {
    /// The tools with one of these tags (all tools for `*`).
    fn tools(&self, tags: &[String]) -> Vec<ToolSpec>;
    fn call(&self, invocation: ToolInvocation) -> ToolOutcome;
    /// The outcome of a call that waited for approval, once an approver resolved it.
    fn approval_outcome(&self, _handle: &str) -> Option<ToolOutcome> {
        None
    }
}

/// The context's tool provider, if `cameleer-ai` is registered.
pub fn tool_provider(ctx: &crate::context::CamelContext) -> Option<Arc<dyn ToolProvider>> {
    ctx.registry().lookup::<Arc<dyn ToolProvider>>(TOOL_PROVIDER_BEAN).map(|a| (*a).clone())
}

// ---------------------------------------------------------------------------------------------
// the agent loop

/// What to do when the model names a tool that doesn't exist (Camel's
/// `hallucinatedToolNameStrategy`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HallucinatedTool {
    /// Tell the model the tool doesn't exist (default).
    Report,
    /// Fail the exchange.
    Fail,
}

/// Limits and options of a loop.
#[derive(Clone, Debug)]
pub struct AgentLimits {
    pub max_turns: usize,
    /// Total tokens (input plus output) across turns.
    pub max_tokens: Option<i64>,
    pub deadline: Option<Duration>,
    /// Longer tool results are cut, with a note, before they go back to the model.
    pub max_result_chars: usize,
    /// Tools of one turn run in parallel on at most this many threads.
    pub parallel: usize,
    pub hallucinated: HallucinatedTool,
    /// Stop after the same call (tool and arguments) failed this many times.
    pub max_repeated_failures: usize,
}

impl Default for AgentLimits {
    fn default() -> Self {
        AgentLimits {
            max_turns: 10,
            max_tokens: None,
            deadline: None,
            max_result_chars: 20_000,
            parallel: 4,
            hallucinated: HallucinatedTool::Report,
            max_repeated_failures: 3,
        }
    }
}

/// Why a loop ended.
#[derive(Clone, Debug, PartialEq)]
pub enum StopReason {
    Answer,
    MaxTurns,
    MaxTokens,
    Deadline,
    RepeatedFailure,
    /// A tool call waits for approval; the handle resolves it and the loop can resume.
    ApprovalRequired(String),
    ApprovalDenied,
}

impl StopReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            StopReason::Answer => "answer",
            StopReason::MaxTurns => "max-turns",
            StopReason::MaxTokens => "max-tokens",
            StopReason::Deadline => "deadline",
            StopReason::RepeatedFailure => "repeated-failure",
            StopReason::ApprovalRequired(_) => "approval-required",
            StopReason::ApprovalDenied => "approval-denied",
        }
    }
}

/// One tool call the loop made.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    pub result: String,
    pub is_error: bool,
}

/// The outcome of a loop.
#[derive(Clone, Debug)]
pub struct AgentRun {
    /// The whole conversation after the loop (without the system message).
    pub messages: Vec<ChatMessage>,
    pub answer: String,
    pub stop: StopReason,
    pub turns: usize,
    pub usage: Usage,
    pub tool_calls: Vec<ToolCallRecord>,
    /// For a stop on approval: the id and name of the call that waits.
    pub pending: Option<(String, String)>,
}

/// Run the model and its tools until the model answers or a limit is reached. Every tool call
/// gets a result in the conversation (providers require one per call id), including calls
/// that were refused or not run because the loop stopped.
pub fn run_agent(
    model: &dyn ChatModel,
    tools: Option<&dyn ToolProvider>,
    mut request: ChatRequest,
    limits: &AgentLimits,
    base: &ToolInvocation,
) -> Result<AgentRun> {
    let started = Instant::now();
    let mut usage = Usage::default();
    let mut turns = 0;
    let mut records = Vec::new();
    let mut failures: HashMap<String, usize> = HashMap::new();
    let known: Vec<String> = request.tools.iter().map(|t| t.name.clone()).collect();
    loop {
        if turns >= limits.max_turns {
            return Ok(finish(request, StopReason::MaxTurns, turns, usage, records, None));
        }
        if let Some(d) = limits.deadline {
            if started.elapsed() >= d {
                return Ok(finish(request, StopReason::Deadline, turns, usage, records, None));
            }
        }
        if let Some(max) = limits.max_tokens {
            if usage.input + usage.output >= max {
                return Ok(finish(request, StopReason::MaxTokens, turns, usage, records, None));
            }
        }
        let resp = model.chat(&request)?;
        turns += 1;
        usage.input += resp.usage.input;
        usage.output += resp.usage.output;
        let calls: Vec<(String, String, Value)> = resp.message.tool_uses().into_iter().map(|(i, n, a)| (i.to_string(), n.to_string(), a.clone())).collect();
        request.messages.push(resp.message.clone());
        if calls.is_empty() {
            return Ok(finish(request, StopReason::Answer, turns, usage, records, None));
        }
        // run this turn's calls (in parallel, bounded)
        let mut results: Vec<(String, String, ToolOutcome)> = Vec::new();
        let mut to_run: Vec<(String, String, Value)> = Vec::new();
        for (id, name, args) in calls {
            if !known.contains(&name) || tools.is_none() {
                if limits.hallucinated == HallucinatedTool::Fail {
                    return Err(CamelError::illegal_state(format!("The model called the unknown tool {}", name)));
                }
                let text = format!("There is no tool named {}. Available tools: {}", name, known.join(", "));
                results.push((id, name, ToolOutcome { text, is_error: true, status: ToolOutcomeStatus::Error }));
            } else {
                to_run.push((id, name, args));
            }
        }
        if let Some(tp) = tools {
            results.extend(run_parallel(tp, &to_run, base, limits.parallel));
        }
        // keep the model's order
        let order: Vec<String> = request.messages.last().map(|m| m.tool_uses().iter().map(|(i, _, _)| i.to_string()).collect()).unwrap_or_default();
        results.sort_by_key(|(id, _, _)| order.iter().position(|o| o == id).unwrap_or(usize::MAX));
        let mut parts = Vec::new();
        let mut stop = None;
        let mut pending = None;
        for (id, name, outcome) in &results {
            let args = request.messages.last().and_then(|m| m.tool_uses().into_iter().find(|(i, _, _)| i == id).map(|(_, _, a)| a.clone())).unwrap_or(Value::Null);
            let content = truncate(&outcome.text, limits.max_result_chars);
            records.push(ToolCallRecord { id: id.clone(), name: name.clone(), arguments: args.clone(), result: content.clone(), is_error: outcome.is_error });
            match &outcome.status {
                ToolOutcomeStatus::ApprovalRequired(h) if stop.is_none() => {
                    stop = Some(StopReason::ApprovalRequired(h.clone()));
                    pending = Some((id.clone(), name.clone()));
                    continue; // its result is added when the loop resumes
                }
                ToolOutcomeStatus::Denied if stop.is_none() => stop = Some(StopReason::ApprovalDenied),
                _ => {}
            }
            if outcome.is_error {
                let key = format!("{}:{}", name, args.to_json());
                let n = failures.entry(key).or_insert(0);
                *n += 1;
                if *n >= limits.max_repeated_failures && stop.is_none() {
                    stop = Some(StopReason::RepeatedFailure);
                }
            }
            parts.push(Part::ToolResult { id: id.clone(), name: name.clone(), content, is_error: outcome.is_error });
        }
        if !parts.is_empty() {
            request.messages.push(ChatMessage { role: Role::User, parts });
        }
        if let Some(s) = stop {
            return Ok(finish(request, s, turns, usage, records, pending));
        }
    }
}

/// Continue a loop that stopped for an approval, once the call's outcome is known.
pub fn resume_agent(
    model: &dyn ChatModel,
    tools: Option<&dyn ToolProvider>,
    mut request: ChatRequest,
    pending: (String, String),
    outcome: ToolOutcome,
    limits: &AgentLimits,
    base: &ToolInvocation,
) -> Result<AgentRun> {
    let (id, name) = pending;
    let part = Part::ToolResult { id, name, content: truncate(&outcome.text, limits.max_result_chars), is_error: outcome.is_error };
    // the other results of that turn may already be in the last user message
    match request.messages.last_mut() {
        Some(m) if m.role == Role::User && m.parts.iter().all(|p| matches!(p, Part::ToolResult { .. })) => m.parts.push(part),
        _ => request.messages.push(ChatMessage { role: Role::User, parts: vec![part] }),
    }
    if outcome.status == ToolOutcomeStatus::Denied {
        return Ok(finish(request, StopReason::ApprovalDenied, 0, Usage::default(), Vec::new(), None));
    }
    run_agent(model, tools, request, limits, base)
}

fn finish(request: ChatRequest, stop: StopReason, turns: usize, usage: Usage, tool_calls: Vec<ToolCallRecord>, pending: Option<(String, String)>) -> AgentRun {
    let answer = request.messages.iter().rev().find(|m| m.role == Role::Assistant).map(|m| m.text()).unwrap_or_default();
    AgentRun { messages: request.messages, answer, stop, turns, usage, tool_calls, pending }
}

fn truncate(text: &str, max: usize) -> String {
    if max == 0 || text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{}\n[truncated: the result has {} characters; ask for a narrower query]", cut, text.chars().count())
}

fn run_parallel(tp: &dyn ToolProvider, calls: &[(String, String, Value)], base: &ToolInvocation, parallel: usize) -> Vec<(String, String, ToolOutcome)> {
    let invoke = |(id, name, args): &(String, String, Value)| {
        let inv = ToolInvocation { id: id.clone(), name: name.clone(), arguments: args.clone(), ..base.clone() };
        (id.clone(), name.clone(), tp.call(inv))
    };
    if calls.len() <= 1 || parallel <= 1 {
        return calls.iter().map(invoke).collect();
    }
    let out = Mutex::new(Vec::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..parallel.min(calls.len()) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(c) = calls.get(i) else { break };
                let r = invoke(c);
                out.lock().unwrap_or_else(|p| p.into_inner()).push(r);
            });
        }
    });
    out.into_inner().unwrap_or_else(|p| p.into_inner())
}

/// The user's messages in an exchange body: a text body is one message, a list holds
/// `{role, content}` (or neutral `{role, parts}`) messages.
pub fn exchange_messages(ex: &crate::message::Exchange) -> Result<Vec<ChatMessage>> {
    match ex.body() {
        Value::Array(a) => a
            .iter()
            .map(|m| match m {
                Value::Object(_) => ChatMessage::from_value(m),
                other => Ok(ChatMessage::user(other.to_string())),
            })
            .collect(),
        Value::Null => Ok(Vec::new()),
        Value::Bytes(b) => Ok(vec![ChatMessage::user(String::from_utf8_lossy(b).into_owned())]),
        v => Ok(vec![ChatMessage::user(v.to_string())]),
    }
}

// ---------------------------------------------------------------------------------------------
// tool options of a model endpoint

/// The tool-calling options a model endpoint takes (Camel's names on `openai`/`llm`): `tags`,
/// `autoToolExecution`, `maxToolIterations`, `maxAgenticTokens`, `parallelToolExecution`,
/// `hallucinatedToolNameStrategy`, plus Cameleer's `record` and `replay`.
#[derive(Clone, Debug, Default)]
pub struct ToolLoopOptions {
    pub tags: Vec<String>,
    pub auto: bool,
    pub limits: AgentLimits,
    pub record: Option<String>,
    pub replay: Option<String>,
}

impl ToolLoopOptions {
    /// Take the options from endpoint parameters.
    pub fn take(params: &mut crate::util::Parameters) -> Result<ToolLoopOptions> {
        let tags = params.take("tags").map(|t| t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
        let auto = params.take_bool("autoToolExecution", true)?;
        let max_turns = params.take_usize("maxToolIterations", 50)?;
        let max_tokens = match params.take("maxAgenticTokens") {
            Some(v) => Some(v.trim().parse().map_err(|_| CamelError::illegal_argument("maxAgenticTokens must be a number"))?),
            None => None,
        };
        let parallel = params.take_bool("parallelToolExecution", true)?;
        let hallucinated = match params.take_string("hallucinatedToolNameStrategy", "report").to_ascii_lowercase().as_str() {
            "report" | "inform" | "continue" => HallucinatedTool::Report,
            "fail" | "throw" | "exception" => HallucinatedTool::Fail,
            other => return Err(CamelError::illegal_argument(format!("hallucinatedToolNameStrategy must be report or fail, got {}", other))),
        };
        Ok(ToolLoopOptions {
            tags,
            auto,
            limits: AgentLimits { max_turns: max_turns.max(1), max_tokens, parallel: if parallel { 4 } else { 1 }, hallucinated, ..Default::default() },
            record: params.take("record"),
            replay: params.take("replay"),
        })
    }

    /// Whether the endpoint should chat through the neutral model (tools or a recording).
    pub fn is_active(&self) -> bool {
        !self.tags.is_empty() || self.replay.is_some() || self.record.is_some()
    }

    /// The model wrapped for `record` / `replay`.
    pub fn wrap(&self, model: Arc<dyn ChatModel>) -> Result<Arc<dyn ChatModel>> {
        with_recording(Some(model), self.record.as_deref(), self.replay.as_deref()).map(|m| m.expect("model"))
    }
}

/// Header with the tool calls the model asked for, when `autoToolExecution=false`
/// (`[{id, name, arguments}]`).
pub const REQUESTED_TOOL_CALLS: &str = "CamelAiRequestedToolCalls";

/// Chat on an exchange with the endpoint's tools: the body is the user's message (or a list
/// of messages) and becomes the answer; `CamelAiAgent*` headers report turns, tokens, tool
/// calls and the stop reason. With `autoToolExecution=false` one turn runs and the requested
/// calls are in `CamelAiRequestedToolCalls`.
pub fn chat_exchange(ex: &mut crate::message::Exchange, model: Arc<dyn ChatModel>, system: Option<String>, opts: &ToolLoopOptions) -> Result<AgentRun> {
    let model = opts.wrap(model)?;
    let ctx = ex.context()?.clone();
    let provider = tool_provider(&ctx);
    if !opts.tags.is_empty() && provider.is_none() {
        return Err(CamelError::illegal_state("tags need the ai feature (cameleer-ai registered on the context)"));
    }
    let tools = provider.as_ref().map(|p| p.tools(&opts.tags)).unwrap_or_default();
    let messages = exchange_messages(ex)?;
    if messages.is_empty() {
        return Err(CamelError::illegal_argument("the body has no message for the model"));
    }
    let request = ChatRequest { system, messages, tools, max_tokens: None };
    let run = if opts.auto {
        let base = ToolInvocation {
            conversation_id: ex.header(headers::CONVERSATION_ID).map(|v| v.to_string()),
            properties: ex.properties.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            ..Default::default()
        };
        run_agent(model.as_ref(), provider.as_deref(), request, &opts.limits, &base)?
    } else {
        let resp = model.chat(&request)?;
        let calls: Vec<Value> = resp.message.tool_uses().into_iter().map(|(id, name, args)| Value::object([("id", Value::from(id)), ("name", Value::from(name)), ("arguments", args.clone())])).collect();
        if !calls.is_empty() {
            ex.set_header(REQUESTED_TOOL_CALLS, Value::Array(calls));
        }
        let mut messages = request.messages;
        let answer = resp.message.text();
        messages.push(resp.message);
        AgentRun { messages, answer, stop: StopReason::Answer, turns: 1, usage: resp.usage, tool_calls: Vec::new(), pending: None }
    };
    ex.set_header(headers::TURNS, run.turns as i64);
    ex.set_header(headers::INPUT_TOKENS, run.usage.input);
    ex.set_header(headers::OUTPUT_TOKENS, run.usage.output);
    ex.set_header(headers::TOTAL_TOKENS, run.usage.input + run.usage.output);
    ex.set_header(headers::TOOL_CALLS, run.tool_calls.len() as i64);
    ex.set_header(headers::STOP_REASON, run.stop.as_str());
    ex.set_body(run.answer.clone());
    Ok(run)
}

// ---------------------------------------------------------------------------------------------
// record and replay

/// A model that answers from a recording, in order: an agent route runs in tests without a
/// model. The file has one recorded response per line (JSON, [`ChatResponse::to_value`]).
pub struct ReplayModel {
    responses: Mutex<std::collections::VecDeque<ChatResponse>>,
    source: String,
}

impl ReplayModel {
    pub fn from_file(path: &str) -> Result<ReplayModel> {
        let text = std::fs::read_to_string(path).map_err(|e| CamelError::io(format!("Cannot read the recording {}: {}", path, e)))?;
        let mut responses = std::collections::VecDeque::new();
        for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let v = crate::json::parse(line).map_err(|e| CamelError::illegal_argument(format!("{} line {}: {}", path, i + 1, e.message)))?;
            responses.push_back(ChatResponse::from_value(v.as_object().and_then(|m| m.get("response")).unwrap_or(&v))?);
        }
        Ok(ReplayModel { responses: Mutex::new(responses), source: path.to_string() })
    }
    pub fn from_responses(responses: Vec<ChatResponse>) -> ReplayModel {
        ReplayModel { responses: Mutex::new(responses.into()), source: "memory".into() }
    }
}

impl ChatModel for ReplayModel {
    fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse> {
        self.responses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop_front()
            .ok_or_else(|| CamelError::illegal_state(format!("The recording {} has no more model responses", self.source)))
    }
}

/// A model that appends every response to a recording file (for [`ReplayModel`]).
pub struct RecordingModel {
    inner: Arc<dyn ChatModel>,
    path: PathBuf,
    lock: Mutex<()>,
}

impl RecordingModel {
    pub fn new(inner: Arc<dyn ChatModel>, path: impl Into<PathBuf>) -> RecordingModel {
        RecordingModel { inner, path: path.into(), lock: Mutex::new(()) }
    }
}

impl ChatModel for RecordingModel {
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        let resp = self.inner.chat(request)?;
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let line = format!("{}\n", Value::object([("response", resp.to_value())]).to_json());
        use std::io::Write;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| f.write_all(line.as_bytes()))
            .map_err(|e| CamelError::io(format!("Cannot write the recording {}: {}", self.path.display(), e)))?;
        Ok(resp)
    }
}

/// Wrap a model for `record=file:...` / `replay=file:...` options.
pub fn with_recording(model: Option<Arc<dyn ChatModel>>, record: Option<&str>, replay: Option<&str>) -> Result<Option<Arc<dyn ChatModel>>> {
    if let Some(r) = replay {
        let path = r.strip_prefix("file:").unwrap_or(r);
        return Ok(Some(Arc::new(ReplayModel::from_file(path)?)));
    }
    match (model, record) {
        (Some(m), Some(r)) => Ok(Some(Arc::new(RecordingModel::new(m, r.strip_prefix("file:").unwrap_or(r))))),
        (m, _) => Ok(m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Calls `lookup` with the user's text once, then answers with the tool result.
    struct Scripted;
    impl ChatModel for Scripted {
        fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
            let last = req.messages.last().unwrap();
            let results: Vec<String> = last.parts.iter().filter_map(|p| if let Part::ToolResult { content, .. } = p { Some(content.clone()) } else { None }).collect();
            let message = if results.is_empty() {
                ChatMessage {
                    role: Role::Assistant,
                    parts: vec![
                        Part::ToolUse { id: "c1".into(), name: "lookup".into(), arguments: Value::object([("q", Value::from(last.text()))]), meta: None },
                        Part::ToolUse { id: "c2".into(), name: "nope".into(), arguments: Value::Null, meta: None },
                    ],
                }
            } else {
                ChatMessage::assistant(format!("answer: {}", results.join(" | ")))
            };
            Ok(ChatResponse { message, stop_reason: "x".into(), usage: Usage { input: 10, output: 5 } })
        }
    }

    struct Tools;
    impl ToolProvider for Tools {
        fn tools(&self, _tags: &[String]) -> Vec<ToolSpec> {
            vec![ToolSpec { name: "lookup".into(), description: "d".into(), input_schema: Value::Null }]
        }
        fn call(&self, inv: ToolInvocation) -> ToolOutcome {
            ToolOutcome { text: format!("found {}", inv.arguments.as_object().unwrap().get("q").unwrap()), is_error: false, status: ToolOutcomeStatus::Completed }
        }
    }

    #[test]
    fn the_loop_runs_tools_and_answers() {
        let req = ChatRequest { messages: vec![ChatMessage::user("x1")], tools: Tools.tools(&[]), ..Default::default() };
        let run = run_agent(&Scripted, Some(&Tools), req, &AgentLimits::default(), &ToolInvocation::default()).unwrap();
        assert_eq!(run.stop, StopReason::Answer);
        assert_eq!(run.turns, 2);
        assert_eq!(run.usage, Usage { input: 20, output: 10 });
        assert_eq!(run.answer, "answer: found x1 | There is no tool named nope. Available tools: lookup");
        // every call id has a result
        let results = run.messages[2].parts.len();
        assert_eq!(results, 2);
    }

    #[test]
    fn limits_stop_the_loop() {
        let req = ChatRequest { messages: vec![ChatMessage::user("x")], tools: Tools.tools(&[]), ..Default::default() };
        let run = run_agent(&Scripted, Some(&Tools), req.clone(), &AgentLimits { max_turns: 1, ..Default::default() }, &ToolInvocation::default()).unwrap();
        assert_eq!(run.stop, StopReason::MaxTurns);
        let run = run_agent(&Scripted, Some(&Tools), req, &AgentLimits { max_tokens: Some(10), ..Default::default() }, &ToolInvocation::default()).unwrap();
        assert_eq!(run.stop, StopReason::MaxTokens);
    }

    #[test]
    fn messages_and_responses_round_trip() {
        let m = ChatMessage {
            role: Role::Assistant,
            parts: vec![Part::Text("a".into()), Part::ToolUse { id: "1".into(), name: "t".into(), arguments: Value::object([("x", Value::Int(1))]), meta: Some(Value::from("sig")) }],
        };
        let r = ChatResponse { message: m, stop_reason: "tool_use".into(), usage: Usage { input: 3, output: 4 } };
        assert_eq!(ChatResponse::from_value(&r.to_value()).unwrap(), r);
        assert_eq!(truncate("abcdef", 3).lines().next(), Some("abc"));
    }

    #[test]
    fn replay_answers_in_order() {
        let m = ReplayModel::from_responses(vec![ChatResponse { message: ChatMessage::assistant("one"), stop_reason: "stop".into(), usage: Usage::default() }]);
        assert_eq!(m.chat(&ChatRequest::default()).unwrap().message.text(), "one");
        assert!(m.chat(&ChatRequest::default()).is_err());
    }
}
