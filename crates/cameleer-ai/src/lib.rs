//! # cameleer-ai
//!
//! The action layer for AI agents, std only. Routes become tools, resources and prompts that
//! an agent can call, and every call goes through the same reliability envelope:
//!
//! | scheme | Camel | what the route is |
//! |---|---|---|
//! | `ai-tool:name` | camel-ai-tool | a tool: arguments arrive as headers, the body is the result |
//! | `ai-resource:name` | camel-ai-resource | a resource: the body is the resource content |
//! | `ai-prompt:name` | (Cameleer extension) | a prompt template: the body is the prompt text or messages |
//!
//! A consumer (`from("ai-tool:...")`) publishes the route in the context's [`AiRegistry`]
//! when the route starts and withdraws it when the route stops. A producer
//! (`to("ai-tool:name")`, a Cameleer extension) calls the tool through the envelope, which is
//! how routes, tests and the later MCP server and agent step all call tools.
//!
//! The envelope ([`registry`]): JSON Schema 2020-12 argument checks, idempotency,
//! approvals, timeouts that cancel the tool's route, rate limits, circuit breakers,
//! compensation, a hash-chained audit log ([`audit`]), definition pinning and a kill switch.

pub mod component;
pub mod expose;
pub mod guard;
pub mod mcp;
pub mod registry;
pub mod tool;

use std::sync::Arc;

use cameleer_core::context::CamelContext;

pub use registry::{AiRegistry, ApprovalRequest, AskOutcome, CallInteraction, Interaction, PromptMessage, PromptResult, ResourceContent, ToolCall, ToolResult, ToolStatus};
pub use tool::{Hints, ToolDefinition};

/// Header and property names (Camel's `CamelAiTool*` names, plus Cameleer's extensions).
pub mod headers {
    /// The id of the tool call (Camel `CamelAiToolCallId`).
    pub const CALL_ID: &str = "CamelAiToolCallId";
    /// The conversation the call belongs to (Camel `CamelAiToolConversationId`).
    pub const CONVERSATION_ID: &str = "CamelAiToolConversationId";
    /// The tool's name.
    pub const TOOL_NAME: &str = "CamelAiToolName";
    /// Who the call is made for (an API key name, a user, an agent id).
    pub const PRINCIPAL: &str = "CamelAiToolPrincipal";
    /// Set on the result when the call repeated an earlier one and its recorded result was
    /// returned without running the route again.
    pub const DUPLICATE: &str = "CamelAiToolDuplicate";
    /// Set on the result when the call failed (the body is the message for the model).
    pub const IS_ERROR: &str = "CamelAiToolIsError";
    /// The call's status: `completed`, `error`, `approval-required` or `denied`.
    pub const STATUS: &str = "CamelAiToolStatus";
    /// The handle of a call that waits for approval.
    pub const APPROVAL_HANDLE: &str = "CamelAiToolApprovalHandle";
    /// Producer: the call is approved by this approver.
    pub const APPROVED_BY: &str = "CamelAiToolApprovedBy";
    /// Set on the exchange a compensation route receives.
    pub const COMPENSATING: &str = "CamelAiToolCompensating";
    /// The tool's `returnDirect` flag, on the result.
    pub const RETURN_DIRECT: &str = "CamelAiToolReturnDirect";
    /// The structured result (an object or array), on the result.
    pub const STRUCTURED: &str = "CamelAiToolStructuredContent";
    /// Set by a tool route: MCP content blocks for the result (a list of
    /// `{"type": "image", "data": ..., "mimeType": ...}`, audio or embedded resources).
    pub const CONTENT: &str = "CamelAiToolContent";
    /// The resource's URI (resource routes).
    pub const RESOURCE_URI: &str = "CamelAiResourceUri";
    /// The resource's name (resource routes).
    pub const RESOURCE_NAME: &str = "CamelAiResourceName";
    /// The resource's MIME type, on the result of a resource producer.
    pub const RESOURCE_MIME_TYPE: &str = "CamelAiResourceMimeType";
    /// The prompt's name (prompt routes).
    pub const PROMPT_NAME: &str = "CamelAiPromptName";
    /// `operation=progress`: the progress so far (a number).
    pub const PROGRESS: &str = "CamelAiToolProgress";
    /// `operation=progress`: the total, if known.
    pub const PROGRESS_TOTAL: &str = "CamelAiToolProgressTotal";
    /// `operation=log`: the level (`debug`, `info`, `notice`, `warning`, `error`, ...).
    pub const LOG_LEVEL: &str = "CamelAiToolLogLevel";
    /// `operation=log`: the logger name.
    pub const LOGGER: &str = "CamelAiToolLogger";
    /// `operation=elicit`: the JSON Schema of the answer when the body is the message text.
    pub const ELICIT_SCHEMA: &str = "CamelAiToolElicitSchema";
    /// `operation=elicit`, out: `accept`, `decline` or `cancel`.
    pub const ELICIT_ACTION: &str = "CamelAiToolElicitAction";
    /// `operation=sample`: the maximum tokens of the client's model answer.
    pub const SAMPLE_MAX_TOKENS: &str = "CamelAiToolSampleMaxTokens";
    /// `operation=sample`, out: the model that answered.
    pub const SAMPLE_MODEL: &str = "CamelAiToolSampleModel";
}

/// Register `ai-tool`, `ai-resource` and `ai-prompt` on a context (lazily created).
pub fn register(ctx: &CamelContext) {
    component::register_kinds();
    ctx.add_component_factory("ai-tool", || Arc::new(component::AiComponent::tool()));
    ctx.add_component_factory("ai-resource", || Arc::new(component::AiComponent::resource()));
    ctx.add_component_factory("ai-prompt", || Arc::new(component::AiComponent::prompt()));
    ctx.add_component_factory("mcp", || Arc::new(mcp::component::McpComponent));
    // the agent step and the model components' tool loops call tools through the registry
    let tools: Arc<dyn cameleer_core::ai::ToolProvider> = Arc::new(registry::RegistryTools { ctx: ctx.downgrade() });
    ctx.registry().bind(cameleer_core::ai::TOOL_PROVIDER_BEAN, tools);
}
