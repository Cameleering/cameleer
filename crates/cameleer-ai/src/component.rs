//! The `ai-tool`, `ai-resource` and `ai-prompt` components.
//!
//! A consumer publishes its route in the context's [`AiRegistry`] when the route starts. A
//! producer (a Cameleer extension) calls, reads or renders through the registry.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use cameleer_core::context::CamelContext;
use cameleer_core::error::{kinds, register_kind_parent, CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::processor::errors::idempotent::IdempotentRepository;
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use crate::headers;
use crate::registry::{AiRegistry, ApprovalRule, AskOutcome, PromptEntry, ResourceEntry, ToolAuthorizationPolicy, ToolCall, ToolEntry, ToolPolicy, ToolStatus, PROMPT_NOT_FOUND, RESOURCE_NOT_FOUND};
use crate::tool::{check_schema_bounds, parse_schema_option, schema_from_parameters, Hints, ToolDefinition};

/// Error kind raised by the `ai-tool` producer with `throwOnError=true`.
pub const TOOL_CALL_EXCEPTION: &str = "org.apache.camel.component.ai.ToolCallException";

pub(crate) fn register_kinds() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        register_kind_parent(TOOL_CALL_EXCEPTION, kinds::RUNTIME);
        register_kind_parent(RESOURCE_NOT_FOUND, kinds::RUNTIME);
        register_kind_parent(PROMPT_NOT_FOUND, kinds::RUNTIME);
    });
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Tool,
    Resource,
    Prompt,
}

/// One component type for the three schemes.
pub struct AiComponent {
    kind: Kind,
}

impl AiComponent {
    pub fn tool() -> Self {
        AiComponent { kind: Kind::Tool }
    }
    pub fn resource() -> Self {
        AiComponent { kind: Kind::Resource }
    }
    pub fn prompt() -> Self {
        AiComponent { kind: Kind::Prompt }
    }
}

impl Component for AiComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        register_kinds();
        let name = remaining.trim_start_matches('/').to_string();
        if name.is_empty() {
            return Err(CamelError::illegal_argument(format!("{} needs a name, e.g. {}:my-name", scheme(self.kind), scheme(self.kind))));
        }
        let spec = match self.kind {
            Kind::Tool => Spec::Tool(Box::new(tool_spec(ctx, &name, params)?)),
            Kind::Resource => Spec::Resource(resource_spec(&name, params)),
            Kind::Prompt => Spec::Prompt(prompt_spec(&name, params)),
        };
        let throw_on_error = params.take_bool("throwOnError", false)?;
        let operation = params.take_string("operation", "read");
        let approval_op = match operation.as_str() {
            "approve" | "deny" | "progress" | "log" | "elicit" | "sample" if self.kind == Kind::Tool => Some(operation.clone()),
            _ => None,
        };
        let notify_updated = match operation.as_str() {
            "read" | "call" => false,
            "approve" | "deny" | "progress" | "log" | "elicit" | "sample" if self.kind == Kind::Tool => false,
            "updated" if self.kind == Kind::Resource => true,
            other => {
                return Err(CamelError::illegal_argument(format!(
                    "Unknown operation {} for {} (use {})",
                    other,
                    scheme(self.kind),
                    match self.kind {
                        Kind::Resource => "read or updated",
                        Kind::Tool => "call, approve, deny, progress, log, elicit or sample",
                        Kind::Prompt => "read",
                    }
                )))
            }
        };
        Ok(Arc::new(AiEndpoint { uri: uri.to_string(), name, spec, throw_on_error, notify_updated, approval_op, ctx: ctx.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn scheme(k: Kind) -> &'static str {
    match k {
        Kind::Tool => "ai-tool",
        Kind::Resource => "ai-resource",
        Kind::Prompt => "ai-prompt",
    }
}

fn csv(v: Option<String>) -> Vec<String> {
    v.map(|s| s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()).unwrap_or_default()
}

fn opt_bool(params: &mut Parameters, key: &str) -> Result<Option<bool>> {
    match params.get(key) {
        None => Ok(None),
        Some(_) => Ok(Some(params.take_bool(key, false)?)),
    }
}

struct ToolSpec {
    definition: ToolDefinition,
    policy: ToolPolicy,
}

fn tool_spec(ctx: &CamelContext, name: &str, params: &mut Parameters) -> Result<ToolSpec> {
    // Camel: the description defaults to the tool's name
    let description = params.take("description").filter(|d| !d.trim().is_empty()).unwrap_or_else(|| name.to_string());
    let title = params.take("title");
    let tags = csv(params.take("tags"));
    let flat = params.take_prefixed("parameter.");
    let input_schema = match params.take("argSchema").or_else(|| params.take("inputSchema")) {
        Some(s) => {
            if !flat.is_empty() {
                return Err(CamelError::illegal_argument("Use either argSchema or parameter.* options, not both"));
            }
            let v = parse_schema_option("argSchema", &s)?;
            match &v {
                Value::Object(m) if m.get("type").and_then(|t| t.as_str()).map(|t| t == "object").unwrap_or(true) => v,
                _ => return Err(CamelError::illegal_argument("argSchema must be an object schema (\"type\": \"object\")")),
            }
        }
        None => schema_from_parameters(&flat)?,
    };
    let out_flat = params.take_prefixed("outputParameter.");
    let output_schema = match params.take("outputSchema") {
        Some(s) => Some(parse_schema_option("outputSchema", &s)?),
        None if !out_flat.is_empty() => Some(schema_from_parameters(&out_flat)?),
        None => None,
    };
    if let Some(o) = &output_schema {
        check_schema_bounds("outputSchema", o)?;
    }
    let hints = Hints {
        read_only: opt_bool(params, "readOnlyHint")?,
        destructive: opt_bool(params, "destructiveHint")?,
        idempotent: opt_bool(params, "idempotentHint")?,
        open_world: opt_bool(params, "openWorldHint")?,
    };
    let definition = ToolDefinition {
        name: name.to_string(),
        title,
        description,
        tags,
        input_schema,
        output_schema,
        hints: hints.clone(),
        version: params.take_string("version", "1"),
        searchable: params.take_bool("searchable", true)?,
        return_direct: params.take_bool("returnDirect", false)?,
    };

    let mut policy = ToolPolicy { arguments_as_body: params.take_bool("argumentsAsBody", false)?, ..Default::default() };
    policy.idempotency = match params.take_string("idempotency", "auto").trim().to_ascii_lowercase().as_str() {
        "auto" => hints.has_side_effects(),
        "true" | "on" => true,
        "false" | "off" => false,
        other => return Err(CamelError::illegal_argument(format!("idempotency must be auto, true or false, got {}", other))),
    };
    policy.idempotency_window = params.take_duration("idempotencyWindow", Duration::from_secs(3600))?;
    if let Some(r) = params.take("idempotentRepository") {
        let bean = r.trim().trim_start_matches('#');
        let repo = ctx
            .registry()
            .lookup::<Arc<dyn IdempotentRepository>>(bean)
            .ok_or_else(|| CamelError::no_such_bean(bean))?;
        policy.idempotent_repository = Some((*repo).clone());
    }
    let approval = params.take_string("approval", "auto");
    policy.approval = match approval.trim() {
        "auto" => {
            if hints.destructive == Some(true) {
                ApprovalRule::Always
            } else {
                ApprovalRule::Never
            }
        }
        "always" | "true" => ApprovalRule::Always,
        "never" | "false" => ApprovalRule::Never,
        expr => {
            let pred = ctx.resolve_language("simple")?.create_predicate(expr, &BTreeMap::new())?;
            ApprovalRule::When(pred, expr.to_string())
        }
    };
    policy.approval_endpoint = params.take("approvalEndpoint");
    policy.approval_timeout = params.take_duration("approvalTimeout", Duration::from_secs(3600))?;
    // Camel's camel.server.mcp-tool-timeout is the default for tools that set no timeout
    let default_timeout = match ctx.properties().get("camel.server.mcp-tool-timeout").or_else(|| ctx.properties().get("camel.server.mcpToolTimeout")) {
        Some(t) => cameleer_core::util::parse_duration(t.trim())?,
        None => Duration::from_secs(20),
    };
    policy.timeout = params.take_duration("timeout", default_timeout)?;
    let limit = params.take_usize("rateLimit", 0)?;
    let period = params.take_duration("rateLimitPeriod", Duration::from_secs(60))?;
    if limit > 0 {
        policy.rate_limit = Some((limit, period));
    }
    policy.breaker_threshold = params.take_u64("circuitBreakerThreshold", 0)? as u32;
    policy.breaker_delay = params.take_duration("circuitBreakerDelay", Duration::from_secs(30))?;
    policy.breaker_fallback = params.take("circuitBreakerFallback");
    policy.compensation = params.take("compensation");
    policy.audit = params.take_bool("audit", true)?;
    if let Some(expr) = params.take("authorize") {
        let pred = ctx.resolve_language("simple")?.create_predicate(&expr, &BTreeMap::new())?;
        policy.authorize = Some((pred, expr));
    }
    if let Some(r) = params.take("redact") {
        policy.redact = Arc::new(crate::guard::parse(&r)?);
    }
    if let Some(t) = params.take("cacheTtl") {
        policy.cache_ttl = Some(cameleer_core::util::parse_duration(t.trim())?).filter(|d| !d.is_zero());
    }
    policy.pin = params.take_bool("pin", true)?;
    if let Some(a) = params.take("authorizationPolicy") {
        let bean = a.trim().trim_start_matches('#');
        let p = ctx
            .registry()
            .lookup::<Arc<dyn ToolAuthorizationPolicy>>(bean)
            .ok_or_else(|| CamelError::no_such_bean(bean))?;
        policy.authorization = Some((*p).clone());
    }
    Ok(ToolSpec { definition, policy })
}

struct ResourceSpec {
    completions: BTreeMap<String, Vec<String>>,
    uri: Option<String>,
    title: Option<String>,
    description: Option<String>,
    mime_type: String,
    tags: Vec<String>,
}

fn resource_spec(name: &str, params: &mut Parameters) -> ResourceSpec {
    let _ = name;
    ResourceSpec {
        completions: params.take_prefixed("values.").into_iter().map(|(k, v)| (k, csv(Some(v)))).collect(),
        uri: params.take("uri").or_else(|| params.take("resourceUri")).or_else(|| params.take("uriTemplate")),
        title: params.take("title"),
        description: params.take("description"),
        mime_type: params.take_string("mimeType", "text/plain"),
        tags: csv(params.take("tags")),
    }
}

struct PromptSpec {
    completions: BTreeMap<String, Vec<String>>,
    title: Option<String>,
    description: Option<String>,
    arguments: Vec<(String, Option<String>, bool)>,
    tags: Vec<String>,
}

fn prompt_spec(name: &str, params: &mut Parameters) -> PromptSpec {
    let _ = name;
    let raw = params.take_prefixed("argument.");
    let mut args: BTreeMap<String, (Option<String>, bool)> = BTreeMap::new();
    let mut completions = BTreeMap::new();
    for (k, v) in raw {
        let (n, attr) = match k.split_once('.') {
            Some((n, a)) => (n.to_string(), a.to_string()),
            None => (k.clone(), "description".to_string()),
        };
        if attr == "values" {
            completions.insert(n.clone(), csv(Some(v)));
            args.entry(n).or_insert((None, false));
            continue;
        }
        let e = args.entry(n).or_insert((None, false));
        match attr.as_str() {
            "required" => e.1 = v.trim().eq_ignore_ascii_case("true"),
            _ => e.0 = Some(v),
        }
    }
    PromptSpec {
        completions,
        title: params.take("title"),
        description: params.take("description"),
        arguments: args.into_iter().map(|(n, (d, r))| (n, d, r)).collect(),
        tags: csv(params.take("tags")),
    }
}

enum Spec {
    Tool(Box<ToolSpec>),
    Resource(ResourceSpec),
    Prompt(PromptSpec),
}

/// An `ai-tool`, `ai-resource` or `ai-prompt` endpoint.
pub struct AiEndpoint {
    uri: String,
    name: String,
    spec: Spec,
    throw_on_error: bool,
    notify_updated: bool,
    approval_op: Option<String>,
    ctx: CamelContext,
}

impl AiEndpoint {
    /// The tool definition (for `ai-tool` endpoints).
    pub fn tool_definition(&self) -> Option<&ToolDefinition> {
        match &self.spec {
            Spec::Tool(t) => Some(&t.definition),
            _ => None,
        }
    }
}

impl Endpoint for AiEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        let kind = match &self.spec {
            Spec::Tool(_) => Kind::Tool,
            Spec::Resource(_) => Kind::Resource,
            Spec::Prompt(_) => Kind::Prompt,
        };
        let uri = match &self.spec {
            Spec::Resource(r) => r.uri.clone(),
            _ => None,
        };
        Ok(Arc::new(AiProducer {
            kind,
            name: self.name.clone(),
            resource_uri: uri,
            throw_on_error: self.throw_on_error,
            notify_updated: self.notify_updated,
            approval_op: self.approval_op.clone(),
            ctx: self.ctx.clone(),
        }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(AiConsumer { endpoint_name: self.name.clone(), spec: self.spec_clone(), processor, ctx: self.ctx.clone(), registered: Mutex::new(false) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl AiEndpoint {
    fn spec_clone(&self) -> SpecData {
        match &self.spec {
            Spec::Tool(t) => SpecData::Tool(t.definition.clone(), t.policy.clone()),
            Spec::Resource(r) => SpecData::Resource {
                uri: r.uri.clone().unwrap_or_else(|| format!("ai-resource://{}", self.name)),
                title: r.title.clone(),
                description: r.description.clone(),
                mime_type: r.mime_type.clone(),
                tags: r.tags.clone(),
                completions: r.completions.clone(),
            },
            Spec::Prompt(p) => SpecData::Prompt {
                title: p.title.clone(),
                description: p.description.clone(),
                arguments: p.arguments.clone(),
                tags: p.tags.clone(),
                completions: p.completions.clone(),
            },
        }
    }
}

enum SpecData {
    Tool(ToolDefinition, ToolPolicy),
    Resource {
        uri: String,
        title: Option<String>,
        description: Option<String>,
        mime_type: String,
        tags: Vec<String>,
        completions: BTreeMap<String, Vec<String>>,
    },
    Prompt {
        title: Option<String>,
        description: Option<String>,
        arguments: Vec<(String, Option<String>, bool)>,
        tags: Vec<String>,
        completions: BTreeMap<String, Vec<String>>,
    },
}

struct AiConsumer {
    endpoint_name: String,
    spec: SpecData,
    processor: Arc<dyn Processor>,
    ctx: CamelContext,
    registered: Mutex<bool>,
}

impl Service for AiConsumer {
    fn start(&self) -> Result<()> {
        let reg = AiRegistry::of(&self.ctx)?;
        let mut done = self.registered.lock().unwrap_or_else(|p| p.into_inner());
        if *done {
            return Ok(());
        }
        match &self.spec {
            SpecData::Tool(def, policy) => reg.register_tool(ToolEntry::new(def.clone(), policy.clone(), self.processor.clone(), &self.ctx))?,
            SpecData::Resource { uri, title, description, mime_type, tags, completions } => {
                let mut e = ResourceEntry::new(
                    self.endpoint_name.clone(),
                    uri.clone(),
                    title.clone(),
                    description.clone(),
                    mime_type.clone(),
                    tags.clone(),
                    self.processor.clone(),
                    &self.ctx,
                );
                e.completions = completions.clone();
                reg.register_resource(e)?
            }
            SpecData::Prompt { title, description, arguments, tags, completions } => {
                let mut e = PromptEntry::new(
                    self.endpoint_name.clone(),
                    title.clone(),
                    description.clone(),
                    arguments.clone(),
                    tags.clone(),
                    self.processor.clone(),
                    &self.ctx,
                );
                e.completions = completions.clone();
                reg.register_prompt(e)?
            }
        }
        *done = true;
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        let mut done = self.registered.lock().unwrap_or_else(|p| p.into_inner());
        if !*done {
            return Ok(());
        }
        if let Ok(reg) = AiRegistry::of(&self.ctx) {
            match &self.spec {
                SpecData::Tool(def, _) => reg.unregister_tool(&def.name),
                SpecData::Resource { .. } => reg.unregister_resource(&self.endpoint_name),
                SpecData::Prompt { .. } => reg.unregister_prompt(&self.endpoint_name),
            }
        }
        *done = false;
        Ok(())
    }
}

struct AiProducer {
    kind: Kind,
    name: String,
    resource_uri: Option<String>,
    throw_on_error: bool,
    notify_updated: bool,
    approval_op: Option<String>,
    ctx: CamelContext,
}

impl Service for AiProducer {}

impl Processor for AiProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let reg = AiRegistry::of(&self.ctx)?;
        match self.kind {
            Kind::Tool if matches!(self.approval_op.as_deref(), Some("progress" | "log" | "elicit" | "sample")) => {
                interact(&reg, ex, self.approval_op.as_deref().unwrap_or_default())
            }
            Kind::Tool if self.approval_op.is_some() => {
                // resolve a parked call: header CamelAiToolApprovalHandle, approver in CamelAiToolApprovedBy
                let handle = ex
                    .header(headers::APPROVAL_HANDLE)
                    .or_else(|| ex.header("CamelAiApprovalHandle"))
                    .filter(|v| !v.is_null())
                    .map(|v| v.to_string())
                    .ok_or_else(|| CamelError::illegal_argument("ai-tool approve/deny needs the CamelAiToolApprovalHandle header"))?;
                let approver = ex.header(headers::APPROVED_BY).filter(|v| !v.is_null()).map(|v| v.to_string()).unwrap_or_else(|| "route".into());
                let r = if self.approval_op.as_deref() == Some("approve") {
                    reg.approve(&handle, &approver)
                } else {
                    let reason = ex.body().to_string();
                    reg.deny(&handle, &approver, if ex.body().is_null() { "" } else { reason.as_str() })
                };
                ex.set_header(headers::STATUS, r.status.as_str());
                ex.set_header(headers::IS_ERROR, r.is_error);
                ex.set_body(r.text);
                Ok(())
            }
            Kind::Tool => {
                let text_header = |ex: &Exchange, k: &str| ex.header(k).filter(|v| !v.is_null()).map(|v| v.to_string());
                let call = ToolCall {
                    name: self.name.clone(),
                    arguments: match ex.body() {
                        Value::Bytes(b) => Value::from(String::from_utf8_lossy(b).into_owned()),
                        other => other.clone(),
                    },
                    call_id: text_header(ex, headers::CALL_ID),
                    conversation_id: text_header(ex, headers::CONVERSATION_ID),
                    principal: text_header(ex, headers::PRINCIPAL),
                    approved_by: text_header(ex, headers::APPROVED_BY),
                    properties: ex.properties.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
                    headers: Vec::new(),
                    interaction: Default::default(),
                };
                let r = reg.call(call);
                ex.set_header(headers::STATUS, r.status.as_str());
                ex.set_header(headers::IS_ERROR, r.is_error);
                ex.set_header(headers::DUPLICATE, r.duplicate);
                ex.set_header(headers::RETURN_DIRECT, r.return_direct);
                if let ToolStatus::ApprovalRequired { handle } = &r.status {
                    ex.set_header(headers::APPROVAL_HANDLE, handle.as_str());
                }
                if r.is_error && self.throw_on_error {
                    return Err(CamelError::new(TOOL_CALL_EXCEPTION, r.text));
                }
                match r.structured {
                    Some(s) => {
                        ex.set_header(headers::STRUCTURED, s.clone());
                        ex.set_body(s);
                    }
                    None => ex.set_body(r.text),
                }
                Ok(())
            }
            Kind::Resource => {
                let uri = ex
                    .header(headers::RESOURCE_URI)
                    .filter(|v| !v.is_null())
                    .map(|v| v.to_string())
                    .or_else(|| self.resource_uri.clone())
                    .or_else(|| reg.resource_uri(&self.name))
                    .unwrap_or_else(|| format!("ai-resource://{}", self.name));
                if self.notify_updated {
                    reg.resource_updated(&uri);
                    return Ok(());
                }
                let c = reg.read_resource(&uri)?;
                ex.set_header(headers::RESOURCE_MIME_TYPE, c.mime_type.as_str());
                match (c.text, c.blob) {
                    (Some(t), _) => ex.set_body(t),
                    (None, Some(b)) => ex.set_body(b),
                    _ => ex.set_body(Value::Null),
                }
                Ok(())
            }
            Kind::Prompt => {
                let args: BTreeMap<String, String> = match ex.body() {
                    Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v.to_string())).collect(),
                    Value::String(s) if !s.trim().is_empty() => match cameleer_core::json::parse(s) {
                        Ok(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v.to_string())).collect(),
                        _ => return Err(CamelError::illegal_argument("ai-prompt producer: the body must be a JSON object of arguments")),
                    },
                    _ => BTreeMap::new(),
                };
                let p = reg.get_prompt(&self.name, &args)?;
                let msgs: Vec<Value> = p
                    .messages
                    .into_iter()
                    .map(|m| {
                        let mut o = cameleer_core::value::ObjectMap::new();
                        o.insert("role".into(), Value::from(m.role));
                        o.insert("content".into(), Value::from(m.text));
                        Value::Object(o)
                    })
                    .collect();
                ex.set_body(Value::Array(msgs));
                Ok(())
            }
        }
    }
}

/// Error kind when a tool route asks the client for input it can't give.
pub const INTERACTION_UNAVAILABLE: &str = "org.apache.camel.component.ai.InteractionUnavailableException";
/// Error kind that ends a call so it can be retried with the client's answer (2026-07-28).
pub const INPUT_REQUIRED: &str = "org.apache.camel.component.ai.InputRequiredException";

/// `ai-tool:current?operation=progress|log|elicit|sample` inside a tool's route: talk back to
/// the caller of the running call.
fn interact(reg: &AiRegistry, ex: &mut Exchange, op: &str) -> Result<()> {
    let Some(i) = reg.interaction(ex) else {
        // outside an MCP call (a producer or agent call): progress and logs go nowhere
        return match op {
            "progress" | "log" => Ok(()),
            _ => Err(CamelError::new(INTERACTION_UNAVAILABLE, format!("ai-tool {}: this call has no client to ask (only MCP calls do)", op))),
        };
    };
    let num = |ex: &Exchange, k: &str| ex.header(k).and_then(|v| v.as_f64().or_else(|| v.to_string().trim().parse().ok()));
    match op {
        "progress" => {
            let progress = num(ex, headers::PROGRESS).ok_or_else(|| CamelError::illegal_argument("ai-tool progress needs the CamelAiToolProgress header"))?;
            let message = match ex.body() {
                Value::Null => None,
                v => Some(v.to_string()),
            };
            i.progress(progress, num(ex, headers::PROGRESS_TOTAL), message);
            Ok(())
        }
        "log" => {
            let level = ex.header(headers::LOG_LEVEL).map(|v| v.to_string()).unwrap_or_else(|| "info".into());
            let logger = ex.header(headers::LOGGER).map(|v| v.to_string());
            i.log(&level, logger.as_deref(), ex.body().clone());
            Ok(())
        }
        "elicit" | "sample" => {
            let (method, params) = if op == "elicit" {
                let p = match ex.body() {
                    Value::Object(m) => Value::Object(m.clone()),
                    Value::String(s) if s.trim_start().starts_with('{') => cameleer_core::json::parse(s).map_err(|e| CamelError::illegal_argument(format!("ai-tool elicit: {}", e.message)))?,
                    v => {
                        let schema = match ex.header(headers::ELICIT_SCHEMA) {
                            Some(Value::String(s)) => cameleer_core::json::parse(s).map_err(|e| CamelError::illegal_argument(format!("ai-tool elicit schema: {}", e.message)))?,
                            Some(s @ Value::Object(_)) => s.clone(),
                            _ => return Err(CamelError::illegal_argument("ai-tool elicit needs a {message, requestedSchema} body or the CamelAiToolElicitSchema header")),
                        };
                        Value::object([("message", Value::from(v.to_string())), ("requestedSchema", schema)])
                    }
                };
                ("elicitation/create", p)
            } else {
                let p = match ex.body() {
                    Value::Object(m) => Value::Object(m.clone()),
                    v => Value::object([
                        ("messages", Value::Array(vec![Value::object([("role", Value::from("user")), ("content", Value::object([("type", Value::from("text")), ("text", Value::from(v.to_string()))]))])])),
                        ("maxTokens", Value::Int(num(ex, headers::SAMPLE_MAX_TOKENS).map(|n| n as i64).unwrap_or(500))),
                    ]),
                };
                ("sampling/createMessage", p)
            };
            match i.ask(method, params) {
                AskOutcome::Answer(v) => {
                    if op == "elicit" {
                        let action = v.get("action").map(|a| a.to_string()).unwrap_or_else(|| "cancel".into());
                        ex.set_header(headers::ELICIT_ACTION, action.as_str());
                        ex.set_body(v.get("content").cloned().unwrap_or(Value::Null));
                    } else {
                        let text = v.get("content").and_then(|c| c.get("text")).map(|t| t.to_string()).unwrap_or_default();
                        ex.set_header(headers::SAMPLE_MODEL, v.get("model").cloned().unwrap_or(Value::Null));
                        ex.set_body(text);
                    }
                    Ok(())
                }
                AskOutcome::Retry => Err(CamelError::new(INPUT_REQUIRED, format!("{} needs the client's answer", method))),
                AskOutcome::Unavailable(why) => Err(CamelError::new(INTERACTION_UNAVAILABLE, why)),
            }
        }
        _ => Ok(()),
    }
}
