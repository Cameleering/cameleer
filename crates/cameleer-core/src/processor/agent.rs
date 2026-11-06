//! The `agent` step (a Cameleer extension; Camel has no agent EIP): a model and its tools in a
//! loop, as one step of a route.
//!
//! ```yaml
//! - agent:
//!     model: openai:chat-completion?model=gpt-5.4-mini
//!     tools: orders
//!     maxTurns: 10
//!     memory: "#conversations"
//!     onApproval: direct:ask-a-person
//! ```
//!
//! The body is the user's message (text, or a list of `{role, content}` messages). The loop
//! runs the model, calls the tools it asks for through the tool provider (the `ai-tool`
//! envelope: argument checks, deduplication, approvals, timeouts, audit), and stops on the
//! model's answer or a limit. The body becomes the answer; headers report turns, tokens, tool
//! calls and the stop reason. With `memory`, the conversation (`CamelAiConversationId`) is kept
//! between calls, and a call that waits for approval pauses the conversation until an approver
//! resolves it.

use std::sync::Arc;
use std::time::Duration;

use crate::ai::{self, headers, AgentLimits, AgentRun, ChatMessage, ChatModel, ChatRequest, HallucinatedTool, StopReason, ToolInvocation, ToolOutcome};
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::model::{ProcessorDefinition, RouteContext};
use crate::processor::routing::aggregate::{AggregationRepository, MemoryAggregationRepository};
use crate::spi::Processor;
use crate::value::{ObjectMap, Value};

/// The `agent` step.
#[derive(Clone, Debug)]
pub struct AgentDefinition {
    pub id: Option<String>,
    /// The model endpoint, such as `openai:chat-completion?model=...` or `anthropic:messages?...`.
    pub model: String,
    /// Tool tags (`ai-tool` `tags`); `*` offers every tool. Empty offers none.
    pub tools: Vec<String>,
    pub system_message: Option<String>,
    pub max_turns: usize,
    pub max_tokens: Option<i64>,
    pub deadline: Option<Duration>,
    /// `#bean` (an `AggregationRepository`) or `memory` (in memory) to keep conversations.
    pub memory: Option<String>,
    /// Messages of history kept and sent to the model.
    pub max_history_messages: usize,
    /// Endpoint told when a tool call waits for approval.
    pub on_approval: Option<String>,
    pub parallel_tool_calls: usize,
    pub max_result_chars: usize,
    pub fail_on_unknown_tool: bool,
    /// `file:PATH`: append the model's responses to a recording.
    pub record: Option<String>,
    /// `file:PATH`: answer from a recording instead of calling the model.
    pub replay: Option<String>,
}

impl AgentDefinition {
    pub fn new(model: impl Into<String>) -> AgentDefinition {
        AgentDefinition {
            id: None,
            model: model.into(),
            tools: Vec::new(),
            system_message: None,
            max_turns: 10,
            max_tokens: None,
            deadline: None,
            memory: None,
            max_history_messages: 100,
            on_approval: None,
            parallel_tool_calls: 4,
            max_result_chars: 20_000,
            fail_on_unknown_tool: false,
            record: None,
            replay: None,
        }
    }
    pub fn tools(mut self, tags: &str) -> Self {
        self.tools = tags.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        self
    }
    pub fn system_message(mut self, s: impl Into<String>) -> Self {
        self.system_message = Some(s.into());
        self
    }
    pub fn max_turns(mut self, n: usize) -> Self {
        self.max_turns = n;
        self
    }
    pub fn max_tokens(mut self, n: i64) -> Self {
        self.max_tokens = Some(n);
        self
    }
    pub fn deadline(mut self, d: Duration) -> Self {
        self.deadline = Some(d);
        self
    }
    pub fn memory(mut self, m: impl Into<String>) -> Self {
        self.memory = Some(m.into());
        self
    }
    pub fn on_approval(mut self, uri: impl Into<String>) -> Self {
        self.on_approval = Some(uri.into());
        self
    }
    pub fn replay(mut self, file: impl Into<String>) -> Self {
        self.replay = Some(file.into());
        self
    }
    pub fn record(mut self, file: impl Into<String>) -> Self {
        self.record = Some(file.into());
        self
    }
}

impl ProcessorDefinition for AgentDefinition {
    fn name(&self) -> &str {
        "agent"
    }
    fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    fn set_id(&mut self, id: String) {
        self.id = Some(id);
    }
    fn label(&self) -> String {
        format!("agent[{}]", self.model)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let ctx = rc.context.clone();
        let model_uri = ctx.resolve_placeholders(&self.model)?;
        // `record`/`replay` take placeholders; an empty value turns them off
        let resolve = |v: &Option<String>| -> Result<Option<String>> {
            match v {
                Some(x) => Ok(Some(ctx.resolve_placeholders(x)?).filter(|s| !s.trim().is_empty())),
                None => Ok(None),
            }
        };
        let (record, replay) = (resolve(&self.record)?, resolve(&self.replay)?);
        let model: Option<Arc<dyn ChatModel>> = if replay.is_some() {
            None
        } else {
            let ep = ctx.get_endpoint(&model_uri)?;
            Some(ep.chat_model().ok_or_else(|| CamelError::illegal_argument(format!("The endpoint {} has no chat model for the agent step (use openai, llm, azure-openai, aws-bedrock, google-vertexai or anthropic)", model_uri)))?)
        };
        let model = ai::with_recording(model, record.as_deref(), replay.as_deref())?.ok_or_else(|| CamelError::illegal_state("agent: no model"))?;
        let memory: Option<Arc<dyn AggregationRepository>> = match self.memory.as_deref().map(str::trim) {
            None | Some("") | Some("false") => None,
            Some("memory") | Some("true") => Some(Arc::new(MemoryAggregationRepository::new())),
            Some(r) => {
                let name = r.trim_start_matches('#');
                let repo = ctx.registry().lookup::<Arc<dyn AggregationRepository>>(name).ok_or_else(|| CamelError::no_such_bean(name))?;
                Some((*repo).clone())
            }
        };
        let limits = AgentLimits {
            max_turns: self.max_turns.max(1),
            max_tokens: self.max_tokens,
            deadline: self.deadline,
            max_result_chars: self.max_result_chars,
            parallel: self.parallel_tool_calls.max(1),
            hallucinated: if self.fail_on_unknown_tool { HallucinatedTool::Fail } else { HallucinatedTool::Report },
            max_repeated_failures: 3,
        };
        Ok(Arc::new(AgentProcessor {
            model,
            tags: self.tools.clone(),
            system: self.system_message.clone(),
            limits,
            memory,
            max_history: self.max_history_messages.max(2),
            on_approval: self.on_approval.clone(),
        }))
    }
}

struct AgentProcessor {
    model: Arc<dyn ChatModel>,
    tags: Vec<String>,
    system: Option<String>,
    limits: AgentLimits,
    memory: Option<Arc<dyn AggregationRepository>>,
    max_history: usize,
    on_approval: Option<String>,
}

/// What memory keeps per conversation.
struct Saved {
    messages: Vec<ChatMessage>,
    /// (call id, tool name, approval handle) of a call that waits.
    pending: Option<(String, String, String)>,
}

impl AgentProcessor {
    fn load(&self, conv: &str) -> Result<Saved> {
        let Some(repo) = &self.memory else { return Ok(Saved { messages: Vec::new(), pending: None }) };
        let Some(ex) = repo.get(conv) else { return Ok(Saved { messages: Vec::new(), pending: None }) };
        let state = ex.body().as_object().cloned().unwrap_or_default();
        let messages = match state.get("messages") {
            Some(Value::Array(a)) => a.iter().map(ChatMessage::from_value).collect::<Result<Vec<_>>>()?,
            _ => Vec::new(),
        };
        let pending = state.get("pending").and_then(|p| p.as_object()).map(|p| {
            let s = |k: &str| p.get(k).map(|v| v.to_string()).unwrap_or_default();
            (s("id"), s("name"), s("handle"))
        });
        Ok(Saved { messages, pending })
    }

    fn save(&self, ex: &Exchange, conv: &str, messages: &[ChatMessage], pending: Option<(String, String, String)>) {
        let Some(repo) = &self.memory else { return };
        let mut state = ObjectMap::new();
        state.insert("messages".into(), Value::Array(messages.iter().map(|m| m.to_value()).collect()));
        if let Some((id, name, handle)) = pending {
            state.insert("pending".into(), Value::object([("id", Value::from(id)), ("name", Value::from(name)), ("handle", Value::from(handle))]));
        }
        let mut stored = Exchange::new(ex.context.clone());
        stored.set_body(Value::Object(state));
        repo.add(conv, stored);
    }

    /// The most recent history that starts with a plain user message (never between a tool
    /// call and its result).
    fn trim(&self, mut messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
        if messages.len() <= self.max_history {
            return messages;
        }
        let mut start = messages.len() - self.max_history;
        while start < messages.len() {
            let m = &messages[start];
            if m.role == ai::Role::User && m.parts.iter().all(|p| matches!(p, ai::Part::Text(_))) {
                break;
            }
            start += 1;
        }
        messages.drain(..start);
        messages
    }
}

impl Processor for AgentProcessor {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let conv = ex.header(headers::CONVERSATION_ID).filter(|v| !v.is_null()).map(|v| v.to_string());
        let ctx = ex.context()?.clone();
        let provider = ai::tool_provider(&ctx);
        if !self.tags.is_empty() && provider.is_none() {
            return Err(CamelError::illegal_state("agent: tools need the ai feature (cameleer-ai registered on the context)"));
        }
        let specs = provider.as_ref().map(|p| p.tools(&self.tags)).unwrap_or_default();
        let base = ToolInvocation {
            conversation_id: conv.clone(),
            principal: ex.header("CamelAiToolPrincipal").or_else(|| ex.header("CamelMcpSecurityPrincipal")).map(|v| v.to_string()),
            properties: ex.properties.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            ..Default::default()
        };
        let saved = match &conv {
            Some(c) => self.load(c)?,
            None => Saved { messages: Vec::new(), pending: None },
        };
        let request = |messages: Vec<ChatMessage>| ChatRequest { system: self.system.clone(), messages: self.trim(messages), tools: specs.clone(), max_tokens: None };
        let tools = provider.as_deref();
        let run: AgentRun = match saved.pending {
            Some((id, name, handle)) => {
                // a conversation paused for an approval resumes once it is resolved
                match provider.as_ref().and_then(|p| p.approval_outcome(&handle)) {
                    Some(outcome) => ai::resume_agent(self.model.as_ref(), tools, request(saved.messages), (id, name), outcome, &self.limits, &base)?,
                    None => {
                        ex.set_header(headers::STOP_REASON, "approval-pending");
                        ex.set_header(headers::APPROVAL_HANDLE, handle.as_str());
                        ex.set_body(format!("The tool call {} is still waiting for approval (handle {}).", name, handle));
                        return Ok(());
                    }
                }
            }
            None => {
                let mut messages = saved.messages;
                messages.extend(ai::exchange_messages(ex)?);
                if messages.is_empty() {
                    return Err(CamelError::illegal_argument("agent: the body has no message for the model"));
                }
                ai::run_agent(self.model.as_ref(), tools, request(messages), &self.limits, &base)?
            }
        };
        let pending = match (&run.stop, &run.pending) {
            (StopReason::ApprovalRequired(h), Some((id, name))) => Some((id.clone(), name.clone(), h.clone())),
            _ => None,
        };
        if let Some(c) = &conv {
            self.save(ex, c, &run.messages, pending.clone());
        }
        ex.set_header(headers::TURNS, run.turns as i64);
        ex.set_header(headers::INPUT_TOKENS, run.usage.input);
        ex.set_header(headers::OUTPUT_TOKENS, run.usage.output);
        ex.set_header(headers::TOTAL_TOKENS, run.usage.input + run.usage.output);
        ex.set_header(headers::TOOL_CALLS, run.tool_calls.len() as i64);
        ex.set_header(headers::STOP_REASON, run.stop.as_str());
        match pending {
            Some((_, name, handle)) => {
                ex.set_header(headers::APPROVAL_HANDLE, handle.as_str());
                if let Some(uri) = &self.on_approval {
                    let mut note = Exchange::new(Some(ctx.clone()));
                    note.set_header(headers::APPROVAL_HANDLE, handle.as_str());
                    note.set_header("CamelAiToolApprovalHandle", handle.as_str());
                    note.set_header("CamelAiToolName", name.as_str());
                    if let Some(c) = &conv {
                        note.set_header(headers::CONVERSATION_ID, c.as_str());
                    }
                    let args = run.tool_calls.iter().rev().find(|t| t.name == name).map(|t| t.arguments.to_json()).unwrap_or_default();
                    note.set_body(args);
                    let done = ctx.create_producer_template().send(uri, note);
                    if let Some(e) = done.exception {
                        return Err(e);
                    }
                }
                ex.set_body(format!("The tool call {} needs approval before the agent can continue (handle {}).", name, handle));
            }
            None => ex.set_body(run.answer),
        }
        Ok(())
    }
}

/// A finished approval as the loop sees it (used by tool providers).
pub fn outcome(text: impl Into<String>, is_error: bool) -> ToolOutcome {
    ToolOutcome { text: text.into(), is_error, status: if is_error { ai::ToolOutcomeStatus::Error } else { ai::ToolOutcomeStatus::Completed } }
}
