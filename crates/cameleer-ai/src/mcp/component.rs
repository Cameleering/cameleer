//! `mcp:` component: call a remote MCP server from a route.
//!
//! ```text
//! mcp:SERVER/TOOL                     call a tool (the body is the arguments)
//! mcp:SERVER?operation=listTools      the server's tools
//! mcp:SERVER?operation=readResource   read the resource in CamelMcpResourceUri (or `uri=`)
//! mcp:SERVER?operation=getPrompt&prompt=NAME
//! ```
//!
//! `SERVER` is a client configured with `camel.ai.mcp.SERVER.*`. A tool call goes through the
//! local envelope when the tool is imported (argument checks, approvals, deduplication, audit);
//! `direct=true` skips it.

use std::any::Any;
use std::sync::Arc;

use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use super::client::McpClients;
use crate::registry::{AiRegistry, ToolCall};

/// Header: the resource URI for `operation=readResource`.
pub const RESOURCE_URI: &str = "CamelMcpResourceUri";
/// Header, out: whether the tool call failed.
pub const IS_ERROR: &str = "CamelMcpIsError";

/// The `mcp` component.
pub struct McpComponent;

impl Component for McpComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let rest = remaining.trim_start_matches('/');
        let (server, tool) = match rest.split_once('/') {
            Some((s, t)) => (s.to_string(), Some(t.to_string())),
            None => (rest.to_string(), None),
        };
        if server.is_empty() {
            return Err(CamelError::illegal_argument("mcp needs a server name: mcp:SERVER/TOOL"));
        }
        let tool = tool.or_else(|| params.take("toolName"));
        let operation = params.take("operation").unwrap_or_else(|| if tool.is_some() { "callTool".into() } else { "listTools".into() });
        if !matches!(operation.as_str(), "callTool" | "listTools" | "readResource" | "getPrompt") {
            return Err(CamelError::illegal_argument(format!("mcp: unknown operation {} (callTool, listTools, readResource, getPrompt)", operation)));
        }
        if operation == "callTool" && tool.is_none() {
            return Err(CamelError::illegal_argument("mcp callTool needs a tool: mcp:SERVER/TOOL"));
        }
        Ok(Arc::new(McpEndpoint {
            uri: uri.to_string(),
            server,
            tool,
            operation,
            resource: params.take("uri"),
            prompt: params.take("prompt"),
            direct: params.take_bool("direct", false)?,
            ctx: ctx.clone(),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct McpEndpoint {
    uri: String,
    server: String,
    tool: Option<String>,
    operation: String,
    resource: Option<String>,
    prompt: Option<String>,
    direct: bool,
    ctx: CamelContext,
}

impl Endpoint for McpEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(McpProducer {
            server: self.server.clone(),
            tool: self.tool.clone(),
            operation: self.operation.clone(),
            resource: self.resource.clone(),
            prompt: self.prompt.clone(),
            direct: self.direct,
            ctx: self.ctx.clone(),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct McpProducer {
    server: String,
    tool: Option<String>,
    operation: String,
    resource: Option<String>,
    prompt: Option<String>,
    direct: bool,
    ctx: CamelContext,
}

impl Service for McpProducer {}

fn arguments(ex: &Exchange) -> Value {
    match ex.body() {
        Value::String(s) if s.trim_start().starts_with('{') => cameleer_core::json::parse(s).unwrap_or(Value::Null),
        v @ Value::Object(_) => v.clone(),
        _ => Value::Null,
    }
}

impl Processor for McpProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let clients = McpClients::of(&self.ctx);
        let client = clients.get(&self.server).ok_or_else(|| CamelError::illegal_state(format!("No MCP client named {} (configure camel.ai.mcp.{}.url or .command)", self.server, self.server)))?;
        match self.operation.as_str() {
            "callTool" => {
                let tool = self.tool.clone().unwrap_or_default();
                let local = format!("{}{}", client.config.tool_prefix, tool);
                let reg = AiRegistry::of(&self.ctx)?;
                if !self.direct && reg.tool(&local).is_some() {
                    let mut call = ToolCall::new(local, arguments(ex));
                    call.call_id = ex.header(crate::headers::CALL_ID).map(|v| v.to_string());
                    call.conversation_id = ex.header(crate::headers::CONVERSATION_ID).map(|v| v.to_string());
                    call.principal = ex.header(crate::headers::PRINCIPAL).map(|v| v.to_string());
                    let r = reg.call(call);
                    ex.set_header(IS_ERROR, r.is_error);
                    ex.set_header(crate::headers::STATUS, r.status.as_str());
                    match r.structured {
                        Some(s) => ex.set_body(s),
                        None => ex.set_body(r.text),
                    }
                } else {
                    let r = client.call_tool(&tool, &arguments(ex))?;
                    ex.set_header(IS_ERROR, r.is_error);
                    if r.content.iter().any(|c| c.get("type").map(|t| t.to_string()).as_deref() != Some("text")) {
                        ex.set_header(crate::headers::CONTENT, Value::Array(r.content.clone()));
                    }
                    let text = r.text();
                    match r.structured {
                        Some(s) => ex.set_body(s),
                        None => ex.set_body(text),
                    }
                }
            }
            "listTools" => {
                let tools: Vec<Value> = client
                    .list_tools()?
                    .into_iter()
                    .map(|t| Value::object([("name", Value::from(t.name)), ("description", Value::from(t.description)), ("inputSchema", t.input_schema)]))
                    .collect();
                ex.set_body(Value::Array(tools));
            }
            "readResource" => {
                let uri = ex.header(RESOURCE_URI).map(|v| v.to_string()).or_else(|| self.resource.clone()).ok_or_else(|| CamelError::illegal_argument("mcp readResource needs the CamelMcpResourceUri header or uri="))?;
                let contents = client.read_resource(&uri)?;
                let text: Vec<String> = contents.iter().filter_map(|c| c.get("text").map(|t| t.to_string())).collect();
                if text.len() == contents.len() {
                    ex.set_body(text.join("\n"));
                } else {
                    ex.set_body(Value::Array(contents));
                }
            }
            _ => {
                let name = self.prompt.clone().ok_or_else(|| CamelError::illegal_argument("mcp getPrompt needs prompt=NAME"))?;
                ex.set_body(Value::Array(client.get_prompt(&name, &arguments(ex))?));
            }
        }
        Ok(())
    }
}
