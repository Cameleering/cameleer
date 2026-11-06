//! The tool registry and the reliability envelope.
//!
//! Every surface (the `ai-tool` producer, and later the MCP server and the agent step) calls a
//! tool through [`AiRegistry::call`], which applies, in order:
//!
//! 1. the kill switch (`camel.ai.disabled-tools`, [`AiRegistry::disable`]);
//! 2. the tool's `authorizationPolicy`;
//! 3. argument filtering (`camel*` and undeclared arguments are dropped, as in Camel) and
//!    JSON Schema 2020-12 validation, with an error the model can act on;
//! 4. the rate limit, per principal;
//! 5. the circuit breaker;
//! 6. approval (`approval=auto|always|never|<Simple predicate>`): a call that needs it is
//!    parked and returns a handle;
//! 7. idempotency: a repeated call returns the recorded result, and a concurrent repeat waits
//!    for the first one, so the route runs once;
//! 8. compensation, registered before the route runs;
//! 9. the call itself, on a copy that keeps only the caller's properties and variables, with a
//!    timeout that cancels the route;
//! 10. output schema validation, and an audit record.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use cameleer_core::component::json_validator::validate as validate_schema;
use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::{Exchange, ExchangePattern};
use cameleer_core::processor::errors::idempotent::IdempotentRepository;
use cameleer_core::spi::{Predicate, Processor};
use cameleer_core::value::{ObjectMap, Value};

use crate::audit::{AuditEntry, AuditLog};
use crate::headers;
use crate::tool::{canonical_json, example_for, sha256_hex, ToolDefinition};

/// The name the registry is bound under in the context's bean registry.
pub const REGISTRY_BEAN: &str = "cameleer.ai.registry";

// ---------------------------------------------------------------------------------------------
// public call and result types

/// A tool call, from any surface.
#[derive(Clone, Debug, Default)]
pub struct ToolCall {
    pub name: String,
    /// The arguments object (`Null` means none).
    pub arguments: Value,
    /// The model's or protocol's id for this call (`CamelAiToolCallId`).
    pub call_id: Option<String>,
    /// The conversation or saga the call belongs to (`CamelAiToolConversationId`).
    pub conversation_id: Option<String>,
    /// Who the call is made for; rate limits and the audit log use it.
    pub principal: Option<String>,
    /// Set when a person or policy approved the call.
    pub approved_by: Option<String>,
    /// Properties (and variables, `CamelVariable.*`) copied to the tool's exchange.
    pub properties: Vec<(String, Value)>,
    /// Headers set by the surface itself (never from the model's arguments), such as the
    /// authenticated principal (`CamelMcpSecurityPrincipal`) or `traceparent`.
    pub headers: Vec<(String, Value)>,
    /// How the tool's route can talk back to the caller during the call (progress, logs,
    /// questions for the user or the client's model). MCP sets it.
    pub interaction: Interaction,
}

/// A tool route's line back to whoever called the tool (an MCP client).
pub trait CallInteraction: Send + Sync {
    /// Report progress (`notifications/progress`).
    fn progress(&self, progress: f64, total: Option<f64>, message: Option<String>);
    /// Send a log message (`notifications/message`) at `level` (`debug`, `info`, `warning`, `error`, ...).
    fn log(&self, level: &str, logger: Option<&str>, data: Value);
    /// Ask the client: `elicitation/create` or `sampling/createMessage`.
    fn ask(&self, method: &str, params: Value) -> AskOutcome;
}

/// The answer to [`CallInteraction::ask`].
#[derive(Clone, Debug)]
pub enum AskOutcome {
    /// The client's result.
    Answer(Value),
    /// The question was recorded; the call must end and be retried with the answer
    /// (2026-07-28 multi round-trip requests).
    Retry,
    /// The client can't answer (no capability, no channel, an error).
    Unavailable(String),
}

/// The optional interaction of a call (a wrapper so `ToolCall` stays `Clone` and `Debug`).
#[derive(Clone, Default)]
pub struct Interaction(pub Option<Arc<dyn CallInteraction>>);

impl std::fmt::Debug for Interaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "Interaction(Some)" } else { "Interaction(None)" })
    }
}

/// The property that identifies a running call's interaction (see [`AiRegistry::interaction`]).
pub const CALL_CONTEXT: &str = "CamelAiToolCallContext";

impl ToolCall {
    pub fn new(name: impl Into<String>, arguments: Value) -> ToolCall {
        ToolCall { name: name.into(), arguments, ..Default::default() }
    }
    pub fn with_call_id(mut self, id: impl Into<String>) -> Self {
        self.call_id = Some(id.into());
        self
    }
    pub fn with_conversation(mut self, id: impl Into<String>) -> Self {
        self.conversation_id = Some(id.into());
        self
    }
    pub fn with_principal(mut self, p: impl Into<String>) -> Self {
        self.principal = Some(p.into());
        self
    }
    pub fn approved_by(mut self, approver: impl Into<String>) -> Self {
        self.approved_by = Some(approver.into());
        self
    }
}

/// How a call ended.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolStatus {
    Completed,
    /// The call failed or was refused; the text tells the model why.
    Error,
    /// The call waits for approval; resolve it with [`AiRegistry::approve`] or
    /// [`AiRegistry::deny`].
    ApprovalRequired { handle: String },
    /// An approver rejected the call.
    Denied,
}

impl ToolStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolStatus::Completed => "completed",
            ToolStatus::Error => "error",
            ToolStatus::ApprovalRequired { .. } => "approval-required",
            ToolStatus::Denied => "denied",
        }
    }
}

/// The result of a call: text for the model, structured content when the route returned an
/// object or array.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub text: String,
    pub structured: Option<Value>,
    pub is_error: bool,
    pub status: ToolStatus,
    /// The call repeated an earlier one; this is the recorded result and the route did not
    /// run again.
    pub duplicate: bool,
    /// The tool's `returnDirect` flag.
    pub return_direct: bool,
    /// MCP content blocks the route set in `CamelAiToolContent` (images, audio, embedded
    /// resources); `None` means one text block with `text`.
    pub content: Option<Vec<Value>>,
    /// The tool's route failed (an exception), as opposed to a refusal by the envelope; MCP
    /// sends clients a generic message for these unless `camel.server.mcp-expose-errors`.
    pub route_error: bool,
}

impl ToolResult {
    pub fn ok(text: impl Into<String>, structured: Option<Value>) -> ToolResult {
        ToolResult { text: text.into(), structured, is_error: false, status: ToolStatus::Completed, duplicate: false, return_direct: false, content: None, route_error: false }
    }
    pub fn error(text: impl Into<String>) -> ToolResult {
        ToolResult { text: text.into(), structured: None, is_error: true, status: ToolStatus::Error, duplicate: false, return_direct: false, content: None, route_error: false }
    }
}

/// A call waiting for approval.
#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub handle: String,
    pub tool: String,
    pub arguments: Value,
    pub principal: Option<String>,
    pub conversation_id: Option<String>,
    pub call_id: Option<String>,
    /// Why approval is needed (`destructiveHint`, `always`, or the predicate).
    pub reason: String,
    pub created: std::time::SystemTime,
}

/// A resource read.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceContent {
    pub uri: String,
    pub mime_type: String,
    /// Text content, or `None` for binary content in `blob`.
    pub text: Option<String>,
    pub blob: Option<Vec<u8>>,
}

/// One message of a prompt.
#[derive(Clone, Debug, PartialEq)]
pub struct PromptMessage {
    pub role: String,
    pub text: String,
    /// The MCP content block when the route gave one (an image, an embedded resource);
    /// `None` means a text block with `text`.
    pub content: Option<Value>,
}

/// A rendered prompt.
#[derive(Clone, Debug, PartialEq)]
pub struct PromptResult {
    pub description: Option<String>,
    pub messages: Vec<PromptMessage>,
}

/// What a completion request completes.
#[derive(Clone, Debug, PartialEq)]
pub enum CompletionRef {
    /// A prompt argument (`ref/prompt`).
    Prompt(String),
    /// A resource template variable (`ref/resource`, the template URI).
    Resource(String),
}

/// Decides whether a principal may call a tool (`authorizationPolicy=#bean`, bound as
/// `Arc<dyn ToolAuthorizationPolicy>`).
pub trait ToolAuthorizationPolicy: Send + Sync {
    /// `Err(reason)` refuses the call; the reason is returned to the model.
    fn authorize(&self, tool: &ToolDefinition, call: &ToolCall) -> std::result::Result<(), String>;
}

// ---------------------------------------------------------------------------------------------
// tool policies (set from endpoint options)

/// When a call needs approval.
#[derive(Clone)]
pub enum ApprovalRule {
    Never,
    Always,
    /// A Simple predicate over the arguments (as headers); the text is shown as the reason.
    When(Arc<dyn Predicate>, String),
}

/// The envelope settings of one tool.
#[derive(Clone)]
pub struct ToolPolicy {
    pub idempotency: bool,
    pub idempotency_window: Duration,
    pub idempotent_repository: Option<Arc<dyn IdempotentRepository>>,
    pub approval: ApprovalRule,
    pub approval_endpoint: Option<String>,
    pub approval_timeout: Duration,
    /// Zero runs the route on the caller's thread with no timeout.
    pub timeout: Duration,
    pub rate_limit: Option<(usize, Duration)>,
    /// Consecutive failures that open the breaker (0 = no breaker).
    pub breaker_threshold: u32,
    pub breaker_delay: Duration,
    pub breaker_fallback: Option<String>,
    pub compensation: Option<String>,
    pub audit: bool,
    pub pin: bool,
    pub arguments_as_body: bool,
    pub authorization: Option<Arc<dyn ToolAuthorizationPolicy>>,
    /// `authorize`: a Simple predicate over the arguments (as headers) and the caller
    /// (`CamelAiToolPrincipal`, `CamelMcpSecurityPrincipal*`); false refuses the call.
    pub authorize: Option<(Arc<dyn Predicate>, String)>,
    /// `redact`: personal data and secrets replaced in results.
    pub redact: Arc<Vec<crate::guard::Redaction>>,
    /// `cacheTtl`: successful results are reused for the same arguments and caller.
    pub cache_ttl: Option<Duration>,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        ToolPolicy {
            idempotency: false,
            idempotency_window: Duration::from_secs(3600),
            idempotent_repository: None,
            approval: ApprovalRule::Never,
            approval_endpoint: None,
            approval_timeout: Duration::from_secs(3600),
            timeout: Duration::from_secs(20),
            rate_limit: None,
            breaker_threshold: 0,
            breaker_delay: Duration::from_secs(30),
            breaker_fallback: None,
            compensation: None,
            audit: true,
            pin: true,
            arguments_as_body: false,
            authorization: None,
            authorize: None,
            redact: Arc::new(Vec::new()),
            cache_ttl: None,
        }
    }
}

#[derive(Default)]
struct Breaker {
    failures: u32,
    open_until: Option<Instant>,
    trial_running: bool,
}

/// A published tool: its definition, the route that runs it and its envelope state.
pub struct ToolEntry {
    pub definition: ToolDefinition,
    pub policy: ToolPolicy,
    processor: Arc<dyn Processor>,
    ctx: WeakContext,
    breaker: Mutex<Breaker>,
    calls: Mutex<HashMap<String, VecDeque<Instant>>>,
    cache: Mutex<HashMap<String, (Instant, ToolResult)>>,
}

impl ToolEntry {
    pub fn new(definition: ToolDefinition, policy: ToolPolicy, processor: Arc<dyn Processor>, ctx: &CamelContext) -> ToolEntry {
        ToolEntry { definition, policy, processor, ctx: ctx.downgrade(), breaker: Mutex::default(), calls: Mutex::default(), cache: Mutex::default() }
    }
}

/// A published resource.
pub struct ResourceEntry {
    pub name: String,
    pub uri: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub mime_type: String,
    pub tags: Vec<String>,
    /// Completion values per template variable.
    pub completions: BTreeMap<String, Vec<String>>,
    processor: Arc<dyn Processor>,
    ctx: WeakContext,
}

impl ResourceEntry {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: String,
        uri: String,
        title: Option<String>,
        description: Option<String>,
        mime_type: String,
        tags: Vec<String>,
        processor: Arc<dyn Processor>,
        ctx: &CamelContext,
    ) -> ResourceEntry {
        ResourceEntry { name, uri, title, description, mime_type, tags, completions: BTreeMap::new(), processor, ctx: ctx.downgrade() }
    }
    /// The resource as MCP lists it (`uri`, `name`, `title`, `description`, `mimeType`; a
    /// URI with `{...}` is listed as a template with `uriTemplate`).
    pub fn to_value(&self) -> Value {
        let mut m = ObjectMap::new();
        let key = if self.is_template() { "uriTemplate" } else { "uri" };
        m.insert(key.into(), Value::from(self.uri.as_str()));
        m.insert("name".into(), Value::from(self.name.as_str()));
        if let Some(t) = &self.title {
            m.insert("title".into(), Value::from(t.as_str()));
        }
        if let Some(d) = &self.description {
            m.insert("description".into(), Value::from(d.as_str()));
        }
        m.insert("mimeType".into(), Value::from(self.mime_type.as_str()));
        Value::Object(m)
    }
    pub fn is_template(&self) -> bool {
        self.uri.contains('{')
    }
}

/// A published prompt.
pub struct PromptEntry {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    /// (name, description, required)
    pub arguments: Vec<(String, Option<String>, bool)>,
    pub tags: Vec<String>,
    /// Completion values per argument.
    pub completions: BTreeMap<String, Vec<String>>,
    processor: Arc<dyn Processor>,
    ctx: WeakContext,
}

impl PromptEntry {
    pub fn new(
        name: String,
        title: Option<String>,
        description: Option<String>,
        arguments: Vec<(String, Option<String>, bool)>,
        tags: Vec<String>,
        processor: Arc<dyn Processor>,
        ctx: &CamelContext,
    ) -> PromptEntry {
        PromptEntry { name, title, description, arguments, tags, completions: BTreeMap::new(), processor, ctx: ctx.downgrade() }
    }
    /// The prompt as MCP lists it.
    pub fn to_value(&self) -> Value {
        let mut m = ObjectMap::new();
        m.insert("name".into(), Value::from(self.name.as_str()));
        if let Some(t) = &self.title {
            m.insert("title".into(), Value::from(t.as_str()));
        }
        if let Some(d) = &self.description {
            m.insert("description".into(), Value::from(d.as_str()));
        }
        let args: Vec<Value> = self
            .arguments
            .iter()
            .map(|(n, d, r)| {
                let mut a = ObjectMap::new();
                a.insert("name".into(), Value::from(n.as_str()));
                if let Some(d) = d {
                    a.insert("description".into(), Value::from(d.as_str()));
                }
                a.insert("required".into(), Value::Bool(*r));
                Value::Object(a)
            })
            .collect();
        m.insert("arguments".into(), Value::Array(args));
        Value::Object(m)
    }
}

// ---------------------------------------------------------------------------------------------
// registry

enum Idem {
    InFlight,
    Done(ToolResult, Instant),
}

struct Pending {
    request: ApprovalRequest,
    call: ToolCall,
    at: Instant,
}

struct Compensation {
    tool: String,
    uri: String,
    arguments: ObjectMap,
    call_id: Option<String>,
    result: Option<String>,
    ctx: WeakContext,
}

type Listener = Arc<dyn Fn(&Change) + Send + Sync>;

/// What changed in the registry (MCP `list_changed` and `resources/updated`).
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Tools,
    Resources,
    Prompts,
    /// A resource's content changed (`AiRegistry::resource_updated`).
    ResourceUpdated(String),
}

/// The context's tools, resources and prompts, and the envelope state they share.
pub struct AiRegistry {
    tools: RwLock<BTreeMap<String, Arc<ToolEntry>>>,
    resources: RwLock<BTreeMap<String, Arc<ResourceEntry>>>,
    prompts: RwLock<BTreeMap<String, Arc<PromptEntry>>>,
    disabled: RwLock<HashSet<String>>,
    pins: Mutex<BTreeMap<String, (String, String)>>,
    manifest: Option<PathBuf>,
    audit: AuditLog,
    idem: Mutex<HashMap<String, Idem>>,
    idem_cv: Condvar,
    approvals: Mutex<HashMap<String, Pending>>,
    /// Interactions of running calls, by id (the `CamelAiToolCallContext` property).
    interactions: Mutex<HashMap<u64, Arc<dyn CallInteraction>>>,
    /// Results of resolved approvals, by handle (a paused agent resumes from them).
    resolved: Mutex<HashMap<String, (Instant, ToolResult)>>,
    sagas: Mutex<HashMap<String, Vec<Compensation>>>,
    listeners: Mutex<Vec<(u64, Listener)>>,
    seq: AtomicU64,
    ctx: WeakContext,
}

static CREATE: Mutex<()> = Mutex::new(());

impl AiRegistry {
    /// The context's registry, created on first use from the `camel.ai.*` properties.
    pub fn of(ctx: &CamelContext) -> Result<Arc<AiRegistry>> {
        if let Some(r) = ctx.registry().lookup::<AiRegistry>(REGISTRY_BEAN) {
            return Ok(r);
        }
        let _guard = CREATE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(r) = ctx.registry().lookup::<AiRegistry>(REGISTRY_BEAN) {
            return Ok(r);
        }
        let props = ctx.properties();
        let enabled = props.get("camel.ai.audit.enabled").map(|v| !v.trim().eq_ignore_ascii_case("false")).unwrap_or(true);
        let file = props.get("camel.ai.audit.file").filter(|f| !f.trim().is_empty()).map(|f| PathBuf::from(f.trim()));
        let max = props.get("camel.ai.audit.maxRecords").and_then(|m| m.trim().parse().ok()).unwrap_or(1000);
        let audit = AuditLog::new(enabled, file, max)?;
        let manifest = props.get("camel.ai.manifest").filter(|f| !f.trim().is_empty()).map(|f| PathBuf::from(f.trim()));
        let pins = match &manifest {
            Some(path) if path.exists() => load_manifest(path)?,
            _ => BTreeMap::new(),
        };
        let disabled: HashSet<String> = props
            .get("camel.ai.disabled-tools")
            .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        let reg = Arc::new(AiRegistry {
            tools: RwLock::default(),
            resources: RwLock::default(),
            prompts: RwLock::default(),
            disabled: RwLock::new(disabled),
            pins: Mutex::new(pins),
            manifest,
            audit,
            idem: Mutex::default(),
            idem_cv: Condvar::new(),
            approvals: Mutex::default(),
            resolved: Mutex::default(),
            interactions: Mutex::default(),
            sagas: Mutex::default(),
            listeners: Mutex::default(),
            seq: AtomicU64::new(0),
            ctx: ctx.downgrade(),
        });
        ctx.registry().bind_arc(REGISTRY_BEAN, reg.clone());
        Ok(reg)
    }

    // --- publishing ---

    /// Publish a tool (called when its route starts). Fails when another route already
    /// publishes the name, or when pinning finds a changed definition with the same version.
    pub fn register_tool(&self, entry: ToolEntry) -> Result<()> {
        let name = entry.definition.name.clone();
        if entry.policy.pin {
            self.check_pin(&entry.definition)?;
        }
        {
            let mut tools = self.tools.write().unwrap_or_else(|p| p.into_inner());
            if tools.contains_key(&name) {
                return Err(CamelError::illegal_state(format!("Tool {} is already published by another route", name)));
            }
            tools.insert(name, Arc::new(entry));
        }
        self.notify(Change::Tools);
        Ok(())
    }

    /// Withdraw a tool (called when its route stops).
    pub fn unregister_tool(&self, name: &str) {
        let removed = self.tools.write().unwrap_or_else(|p| p.into_inner()).remove(name).is_some();
        if removed {
            self.notify(Change::Tools);
        }
    }

    pub fn register_resource(&self, entry: ResourceEntry) -> Result<()> {
        let mut res = self.resources.write().unwrap_or_else(|p| p.into_inner());
        if res.contains_key(&entry.name) {
            return Err(CamelError::illegal_state(format!("Resource {} is already published by another route", entry.name)));
        }
        res.insert(entry.name.clone(), Arc::new(entry));
        drop(res);
        self.notify(Change::Resources);
        Ok(())
    }

    pub fn unregister_resource(&self, name: &str) {
        if self.resources.write().unwrap_or_else(|p| p.into_inner()).remove(name).is_some() {
            self.notify(Change::Resources);
        }
    }

    pub fn register_prompt(&self, entry: PromptEntry) -> Result<()> {
        let mut pr = self.prompts.write().unwrap_or_else(|p| p.into_inner());
        if pr.contains_key(&entry.name) {
            return Err(CamelError::illegal_state(format!("Prompt {} is already published by another route", entry.name)));
        }
        pr.insert(entry.name.clone(), Arc::new(entry));
        drop(pr);
        self.notify(Change::Prompts);
        Ok(())
    }

    pub fn unregister_prompt(&self, name: &str) {
        if self.prompts.write().unwrap_or_else(|p| p.into_inner()).remove(name).is_some() {
            self.notify(Change::Prompts);
        }
    }

    /// Run `f` whenever the tool, resource or prompt lists change (MCP `list_changed`).
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        self.subscribe(move |_| f());
    }

    /// Run `f` on every change, with what changed; returns an id for [`AiRegistry::unsubscribe`].
    pub fn subscribe(&self, f: impl Fn(&Change) + Send + Sync + 'static) -> u64 {
        let id = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        self.listeners.lock().unwrap_or_else(|p| p.into_inner()).push((id, Arc::new(f)));
        id
    }

    /// Remove a listener added with [`AiRegistry::subscribe`].
    pub fn unsubscribe(&self, id: u64) {
        self.listeners.lock().unwrap_or_else(|p| p.into_inner()).retain(|(i, _)| *i != id);
    }

    /// Tell subscribers that a resource's content changed (MCP `notifications/resources/updated`).
    pub fn resource_updated(&self, uri: &str) {
        self.notify(Change::ResourceUpdated(uri.to_string()));
    }

    fn notify(&self, change: Change) {
        let ls: Vec<Listener> = self.listeners.lock().unwrap_or_else(|p| p.into_inner()).iter().map(|(_, l)| l.clone()).collect();
        for l in ls {
            l(&change);
        }
    }

    /// The approval endpoint of a tool, if it has one (approvals then happen out of band).
    pub fn approval_endpoint(&self, name: &str) -> Option<String> {
        self.tools.read().unwrap_or_else(|p| p.into_inner()).get(name).and_then(|t| t.policy.approval_endpoint.clone())
    }

    /// Completion values for a prompt argument or a resource template variable that start
    /// with `prefix` (`argument.NAME.values` / `values.NAME` options).
    pub fn completions(&self, reference: &CompletionRef, argument: &str, prefix: &str) -> Vec<String> {
        let values: Vec<String> = match reference {
            CompletionRef::Prompt(name) => {
                self.prompts.read().unwrap_or_else(|p| p.into_inner()).get(name).and_then(|p| p.completions.get(argument).cloned()).unwrap_or_default()
            }
            CompletionRef::Resource(uri) => self
                .resources
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .values()
                .find(|r| &r.uri == uri)
                .and_then(|r| r.completions.get(argument).cloned())
                .unwrap_or_default(),
        };
        values.into_iter().filter(|v| v.starts_with(prefix)).collect()
    }

    // --- pinning ---

    fn check_pin(&self, def: &ToolDefinition) -> Result<()> {
        let hash = def.definition_hash();
        let mut pins = self.pins.lock().unwrap_or_else(|p| p.into_inner());
        match pins.get(&def.name) {
            Some((version, pinned)) if *version == def.version && *pinned != hash => {
                return Err(CamelError::illegal_state(format!(
                    "Tool {} changed (definition hash {} is now {}) but its version is still {}: set a new version= to publish the change",
                    def.name,
                    &pinned[..12.min(pinned.len())],
                    &hash[..12],
                    version
                )));
            }
            Some((version, pinned)) if *version == def.version && *pinned == hash => return Ok(()),
            _ => {}
        }
        pins.insert(def.name.clone(), (def.version.clone(), hash));
        if let Some(path) = &self.manifest {
            save_manifest(path, &pins)?;
        }
        Ok(())
    }

    /// The pinned tools: name to (version, definition hash).
    pub fn manifest(&self) -> BTreeMap<String, (String, String)> {
        self.pins.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    // --- kill switch ---

    /// Withdraw a tool everywhere: it is no longer listed, and calls are refused.
    pub fn disable(&self, name: &str) {
        let added = self.disabled.write().unwrap_or_else(|p| p.into_inner()).insert(name.to_string());
        if added {
            self.notify(Change::Tools);
        }
    }

    pub fn enable(&self, name: &str) {
        let removed = self.disabled.write().unwrap_or_else(|p| p.into_inner()).remove(name);
        if removed {
            self.notify(Change::Tools);
        }
    }

    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled.read().unwrap_or_else(|p| p.into_inner()).contains(name)
    }

    /// The names withdrawn by the kill switch (also names no route publishes yet), sorted.
    pub fn disabled_tools(&self) -> Vec<String> {
        let mut v: Vec<String> = self.disabled.read().unwrap_or_else(|p| p.into_inner()).iter().cloned().collect();
        v.sort();
        v
    }

    // --- listing ---

    /// The enabled tools, optionally only those with one of `tags`.
    pub fn tools(&self, tags: &[&str]) -> Vec<ToolDefinition> {
        let disabled = self.disabled.read().unwrap_or_else(|p| p.into_inner()).clone();
        self.tools
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter(|t| !disabled.contains(&t.definition.name))
            .filter(|t| tags.is_empty() || t.definition.tags.iter().any(|g| tags.contains(&g.as_str())))
            .map(|t| t.definition.clone())
            .collect()
    }

    /// Every published tool, disabled ones included (management views), by name.
    pub fn all_tools(&self) -> Vec<ToolDefinition> {
        self.tools.read().unwrap_or_else(|p| p.into_inner()).values().map(|t| t.definition.clone()).collect()
    }

    pub fn tool(&self, name: &str) -> Option<ToolDefinition> {
        self.tools.read().unwrap_or_else(|p| p.into_inner()).get(name).map(|t| t.definition.clone())
    }

    pub fn resources(&self, tags: &[&str]) -> Vec<Value> {
        self.resources
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter(|r| tags.is_empty() || r.tags.iter().any(|g| tags.contains(&g.as_str())))
            .map(|r| r.to_value())
            .collect()
    }

    pub fn prompts(&self, tags: &[&str]) -> Vec<Value> {
        self.prompts
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter(|r| tags.is_empty() || r.tags.iter().any(|g| tags.contains(&g.as_str())))
            .map(|r| r.to_value())
            .collect()
    }

    /// The published resources.
    pub fn resource_entries(&self) -> Vec<Arc<ResourceEntry>> {
        self.resources.read().unwrap_or_else(|p| p.into_inner()).values().cloned().collect()
    }

    /// The published prompts.
    pub fn prompt_entries(&self) -> Vec<Arc<PromptEntry>> {
        self.prompts.read().unwrap_or_else(|p| p.into_inner()).values().cloned().collect()
    }

    /// The audit log.
    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    // --- calling ---

    /// Call a tool through the envelope. Refusals and failures come back as error results
    /// (text for the model), never as panics; only a missing context is an `Err`.
    pub fn call(&self, call: ToolCall) -> ToolResult {
        let started = Instant::now();
        let entry = self.tools.read().unwrap_or_else(|p| p.into_inner()).get(&call.name).cloned();
        let mut audit = AuditEntry {
            tool: call.name.clone(),
            principal: call.principal.clone(),
            call_id: call.call_id.clone(),
            conversation_id: call.conversation_id.clone(),
            arguments_digest: sha256_hex(canonical_json(&call.arguments).as_bytes()),
            approver: call.approved_by.clone(),
            outcome: "none".into(),
            ..Default::default()
        };
        let Some(entry) = entry else {
            audit.decision = "unknown-tool".into();
            self.audit.append(&audit);
            return ToolResult::error(format!("Unknown tool: {}", call.name));
        };
        let mut result = self.call_entry(&entry, &call, &mut audit);
        if !entry.policy.redact.is_empty() && result.status == ToolStatus::Completed {
            let (text, n) = crate::guard::redact_text(&entry.policy.redact, &result.text);
            if n > 0 {
                result.text = text;
                result.structured = result.structured.as_ref().map(|s| crate::guard::redact_value(&entry.policy.redact, s).0);
                result.content = result.content.as_ref().map(|c| c.iter().map(|b| crate::guard::redact_value(&entry.policy.redact, b).0).collect());
                audit.detail = Some(format!("redacted {} value(s)", n));
            }
        }
        audit.duration_ms = started.elapsed().as_millis() as u64;
        if audit.result_digest.is_none() && !result.text.is_empty() && audit.outcome != "none" {
            audit.result_digest = Some(sha256_hex(result.text.as_bytes()));
        }
        audit.duplicate = result.duplicate;
        if entry.policy.audit {
            self.audit.append(&audit);
        }
        let mut result = result;
        result.return_direct = entry.definition.return_direct && result.status == ToolStatus::Completed;
        result
    }

    fn call_entry(&self, entry: &Arc<ToolEntry>, call: &ToolCall, audit: &mut AuditEntry) -> ToolResult {
        let def = &entry.definition;
        let policy = &entry.policy;
        // 1. kill switch
        if self.is_disabled(&def.name) || property_lists(entry, "camel.ai.disabled-tools", &def.name) {
            audit.decision = "disabled".into();
            return ToolResult::error(format!("Tool {} is disabled", def.name));
        }
        // 2. authorization
        if let Some(policy) = &policy.authorization {
            if let Err(reason) = policy.authorize(def, call) {
                audit.decision = "denied".into();
                audit.detail = Some(reason.clone());
                return ToolResult::error(format!("Not authorized to call {}: {}", def.name, reason));
            }
        }
        // 3. arguments
        let args = match filter_arguments(def, &call.arguments) {
            Ok(a) => a,
            Err(msg) => {
                audit.decision = "invalid".into();
                return ToolResult::error(msg);
            }
        };
        let errors = validate_schema(&def.input_schema, &Value::Object(args.clone()));
        if !errors.is_empty() {
            audit.decision = "invalid".into();
            audit.detail = Some(errors.join("; "));
            return ToolResult::error(format!(
                "Invalid arguments for tool {}:\n- {}\nExample of valid arguments: {}",
                def.name,
                errors.join("\n- "),
                example_for(&def.input_schema).to_json()
            ));
        }
        // 3b. the authorize predicate, over the arguments and the caller
        if let Some((pred, text)) = &policy.authorize {
            let mut ex = Exchange::new(entry.ctx.upgrade());
            for (k, v) in args.iter() {
                ex.set_header(k.as_str(), v.clone());
            }
            for (k, v) in &call.headers {
                ex.set_header(k.as_str(), v.clone());
            }
            if let Some(p) = &call.principal {
                ex.set_header(headers::PRINCIPAL, p.as_str());
            }
            ex.set_header(headers::TOOL_NAME, def.name.as_str());
            match pred.matches(&ex) {
                Ok(true) => {}
                Ok(false) => {
                    audit.decision = "denied".into();
                    audit.detail = Some(format!("authorize {}", text));
                    return ToolResult::error(format!("Not authorized to call {} with these arguments", def.name));
                }
                Err(e) => {
                    audit.decision = "denied".into();
                    return ToolResult::error(format!("Cannot evaluate the authorize rule of {}: {}", def.name, e.message));
                }
            }
        }
        // 3c. the result cache
        let cache_key = policy.cache_ttl.map(|_| sha256_hex(format!("{}\u{0}{}", call.principal.as_deref().unwrap_or(""), canonical_json(&Value::Object(args.clone()))).as_bytes()));
        if let (Some(ttl), Some(key)) = (policy.cache_ttl, &cache_key) {
            if let Some((at, r)) = entry.cache.lock().unwrap_or_else(|p| p.into_inner()).get(key) {
                if at.elapsed() < ttl {
                    audit.decision = "cached".into();
                    audit.outcome = "ok".into();
                    return r.clone();
                }
            }
        }
        // 4. rate limit
        if let Some((limit, period)) = policy.rate_limit {
            let key = call.principal.clone().unwrap_or_default();
            let now = Instant::now();
            let mut calls = entry.calls.lock().unwrap_or_else(|p| p.into_inner());
            let log = calls.entry(key).or_default();
            while log.front().map(|t| now.duration_since(*t) >= period).unwrap_or(false) {
                log.pop_front();
            }
            if log.len() >= limit {
                let wait = log.front().map(|t| period.saturating_sub(now.duration_since(*t))).unwrap_or(period);
                audit.decision = "rate-limited".into();
                return ToolResult::error(format!(
                    "Rate limit for tool {} reached ({} calls per {}); retry in {} s",
                    def.name,
                    limit,
                    fmt_duration(period),
                    wait.as_secs().max(1)
                ));
            }
            log.push_back(now);
        }
        // 5. circuit breaker
        if policy.breaker_threshold > 0 {
            let mut b = entry.breaker.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(until) = b.open_until {
                if Instant::now() < until || b.trial_running {
                    audit.decision = "circuit-open".into();
                    let text = policy
                        .breaker_fallback
                        .clone()
                        .unwrap_or_else(|| format!("Tool {} is temporarily unavailable after repeated failures; try again later", def.name));
                    return ToolResult::error(text);
                }
                b.trial_running = true; // half-open: let this one call through
            }
        }
        // 6. approval
        if call.approved_by.is_none() {
            match needs_approval(entry, &args, call) {
                Ok(Some(reason)) => {
                    self.release_trial(entry);
                    return self.park(entry, call, args, reason, audit);
                }
                Ok(None) => {}
                Err(e) => {
                    self.release_trial(entry);
                    audit.decision = "invalid".into();
                    return ToolResult::error(format!("Cannot evaluate the approval rule of {}: {}", def.name, e.message));
                }
            }
        } else {
            audit.decision = "approved".into();
        }
        if audit.decision.is_empty() {
            audit.decision = "allowed".into();
        }
        // 7. idempotency
        let idem_key = if policy.idempotency { Some(format!("{}\u{0}{}", def.name, idempotency_key(def, &args, call))) } else { None };
        if let Some(key) = &idem_key {
            match self.idem_begin(key, policy) {
                IdemBegin::Run => {}
                IdemBegin::Recorded(mut r) => {
                    self.release_trial(entry);
                    r.duplicate = true;
                    audit.outcome = if r.is_error { "error" } else { "ok" }.into();
                    return r;
                }
                IdemBegin::Busy => {
                    self.release_trial(entry);
                    audit.outcome = "error".into();
                    return ToolResult::error(format!(
                        "The same call to {} is still running from an earlier attempt; try again later",
                        def.name
                    ));
                }
            }
        }
        // 8. compensation, registered before the side effect
        let saga_slot = match (&policy.compensation, &call.conversation_id) {
            (Some(uri), Some(conv)) => {
                let mut sagas = self.sagas.lock().unwrap_or_else(|p| p.into_inner());
                let list = sagas.entry(conv.clone()).or_default();
                list.push(Compensation {
                    tool: def.name.clone(),
                    uri: uri.clone(),
                    arguments: args.clone(),
                    call_id: call.call_id.clone(),
                    result: None,
                    ctx: entry.ctx.clone(),
                });
                Some((conv.clone(), list.len() - 1))
            }
            _ => None,
        };
        // 9. the call
        let outcome = self.execute(entry, &args, call, idem_key.clone());
        let result = match outcome {
            Exec::Done(r) => {
                audit.outcome = if r.is_error { "error" } else { "ok" }.into();
                self.record_breaker(entry, !r.is_error);
                if let (Some(key), false) = (&cache_key, r.is_error) {
                    let mut c = entry.cache.lock().unwrap_or_else(|p| p.into_inner());
                    if c.len() >= 10_000 {
                        c.clear();
                    }
                    c.insert(key.clone(), (Instant::now(), r.clone()));
                }
                if let Some(key) = &idem_key {
                    self.idem_finish(key, &r, policy);
                }
                r
            }
            Exec::TimedOut => {
                audit.outcome = "timeout".into();
                self.record_breaker(entry, false);
                ToolResult::error(format!("Tool {} did not answer within {} and was cancelled", def.name, fmt_duration(policy.timeout)))
            }
        };
        if let Some((conv, i)) = saga_slot {
            if let Some(c) = self.sagas.lock().unwrap_or_else(|p| p.into_inner()).get_mut(&conv).and_then(|l| l.get_mut(i)) {
                c.result = Some(result.text.clone());
            }
        }
        result
    }

    fn release_trial(&self, entry: &ToolEntry) {
        if entry.policy.breaker_threshold > 0 {
            entry.breaker.lock().unwrap_or_else(|p| p.into_inner()).trial_running = false;
        }
    }

    fn record_breaker(&self, entry: &ToolEntry, success: bool) {
        if entry.policy.breaker_threshold == 0 {
            return;
        }
        let mut b = entry.breaker.lock().unwrap_or_else(|p| p.into_inner());
        b.trial_running = false;
        if success {
            b.failures = 0;
            b.open_until = None;
        } else {
            b.failures += 1;
            if b.failures >= entry.policy.breaker_threshold || b.open_until.is_some() {
                b.open_until = Some(Instant::now() + entry.policy.breaker_delay);
            }
        }
    }

    // --- idempotency ---

    fn idem_begin(&self, key: &str, policy: &ToolPolicy) -> IdemBegin {
        let deadline = Instant::now() + policy.timeout.max(Duration::from_secs(1));
        let mut map = self.idem.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            let now = Instant::now();
            match map.get(key) {
                Some(Idem::Done(r, at)) if now.duration_since(*at) < policy.idempotency_window => return IdemBegin::Recorded(r.clone()),
                Some(Idem::InFlight) => {
                    if now >= deadline {
                        return IdemBegin::Busy;
                    }
                    map = self.idem_cv.wait_timeout(map, deadline - now).unwrap_or_else(|p| p.into_inner()).0;
                    continue;
                }
                _ => {}
            }
            // the external repository dedupes across processes; it holds keys only
            if let Some(repo) = &policy.idempotent_repository {
                if !repo.add(key) {
                    return IdemBegin::Recorded(ToolResult::ok(
                        "This call was already made (it was recorded by the idempotent repository); it was not run again",
                        None,
                    ));
                }
            }
            map.insert(key.to_string(), Idem::InFlight);
            if map.len() > 10_000 {
                let window = policy.idempotency_window;
                map.retain(|_, v| match v {
                    Idem::Done(_, at) => now.duration_since(*at) < window,
                    Idem::InFlight => true,
                });
            }
            return IdemBegin::Run;
        }
    }

    fn idem_finish(&self, key: &str, r: &ToolResult, policy: &ToolPolicy) {
        let mut map = self.idem.lock().unwrap_or_else(|p| p.into_inner());
        if r.is_error {
            // a failed call did not happen: a retry may run it
            map.remove(key);
            if let Some(repo) = &policy.idempotent_repository {
                repo.remove(key);
            }
        } else {
            map.insert(key.to_string(), Idem::Done(r.clone(), Instant::now()));
        }
        drop(map);
        self.idem_cv.notify_all();
    }

    // --- execution ---

    fn execute(&self, entry: &Arc<ToolEntry>, args: &ObjectMap, call: &ToolCall, idem_key: Option<String>) -> Exec {
        let Some(ctx) = entry.ctx.upgrade() else {
            return Exec::Done(ToolResult::error("The tool's context has stopped"));
        };
        let mut ex = Exchange::new(Some(ctx));
        ex.pattern = ExchangePattern::InOut;
        for (k, v) in &call.properties {
            ex.set_property(k.as_str(), v.clone());
        }
        if entry.policy.arguments_as_body {
            ex.set_body(Value::Object(args.clone()).to_json());
        }
        for (k, v) in args.iter() {
            ex.set_header(k.as_str(), v.clone());
        }
        ex.set_header(headers::TOOL_NAME, entry.definition.name.as_str());
        if let Some(id) = &call.call_id {
            ex.set_header(headers::CALL_ID, id.as_str());
        }
        if let Some(id) = &call.conversation_id {
            ex.set_header(headers::CONVERSATION_ID, id.as_str());
        }
        if let Some(p) = &call.principal {
            ex.set_header(headers::PRINCIPAL, p.as_str());
        }
        for (k, v) in &call.headers {
            ex.set_header(k.as_str(), v.clone());
        }
        let interaction_id = call.interaction.0.as_ref().map(|i| {
            let id = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
            self.interactions.lock().unwrap_or_else(|p| p.into_inner()).insert(id, i.clone());
            id
        });
        if let Some(id) = interaction_id {
            ex.set_property(CALL_CONTEXT, Value::Int(id as i64));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        ex.set_cancel_token(cancel.clone());
        let timeout = entry.policy.timeout;
        if timeout.is_zero() {
            let r = run_route(entry, &mut ex);
            if let Some(id) = interaction_id {
                self.interactions.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
            }
            return Exec::Done(r);
        }
        // run on a worker; the caller waits up to the timeout, then cancels
        let slot = Arc::new(Handoff { state: Mutex::new((None, false)), cv: Condvar::new() });
        let worker_slot = slot.clone();
        let worker_entry = entry.clone();
        let registry_ctx = self.ctx.clone();
        let worker_policy = entry.policy.clone();
        let spawned = std::thread::Builder::new().name(format!("ai-tool-{}", entry.definition.name)).spawn(move || {
            let r = run_route(&worker_entry, &mut ex);
            drop(ex);
            if let (Some(id), Some(reg)) = (interaction_id, registry_ctx.upgrade().and_then(|c| AiRegistry::of(&c).ok())) {
                reg.interactions.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
            }
            let mut st = worker_slot.state.lock().unwrap_or_else(|p| p.into_inner());
            if st.1 {
                // the caller gave up: settle idempotency and the breaker here
                drop(st);
                if let (Some(key), Some(reg)) = (idem_key, registry_ctx.upgrade().and_then(|c| AiRegistry::of(&c).ok())) {
                    reg.idem_finish(&key, &r, &worker_policy);
                }
            } else {
                st.0 = Some(r);
                drop(st);
                worker_slot.cv.notify_all();
            }
        });
        if let Err(e) = spawned {
            return Exec::Done(ToolResult::error(format!("Cannot start the call: {}", e)));
        }
        let deadline = Instant::now() + timeout;
        let mut st = slot.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(r) = st.0.take() {
                return Exec::Done(r);
            }
            let now = Instant::now();
            if now >= deadline {
                st.1 = true;
                cancel.store(true, Ordering::Relaxed);
                return Exec::TimedOut;
            }
            st = slot.cv.wait_timeout(st, deadline - now).unwrap_or_else(|p| p.into_inner()).0;
        }
    }

    // --- approvals ---

    fn park(&self, entry: &Arc<ToolEntry>, call: &ToolCall, args: ObjectMap, reason: String, audit: &mut AuditEntry) -> ToolResult {
        let n = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let handle = format!("approval-{}-{}", n, &sha256_hex(format!("{}{:?}{}", n, std::time::SystemTime::now(), entry.definition.name).as_bytes())[..16]);
        let request = ApprovalRequest {
            handle: handle.clone(),
            tool: entry.definition.name.clone(),
            arguments: Value::Object(args.clone()),
            principal: call.principal.clone(),
            conversation_id: call.conversation_id.clone(),
            call_id: call.call_id.clone(),
            reason: reason.clone(),
            created: std::time::SystemTime::now(),
        };
        let mut parked = call.clone();
        parked.arguments = Value::Object(args.clone());
        self.approvals
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(handle.clone(), Pending { request: request.clone(), call: parked, at: Instant::now() });
        audit.decision = "approval-required".into();
        audit.outcome = "pending".into();
        audit.detail = Some(format!("{} ({})", handle, reason));
        if let (Some(uri), Some(ctx)) = (&entry.policy.approval_endpoint, entry.ctx.upgrade()) {
            let mut ex = Exchange::new(Some(ctx.clone()));
            for (k, v) in args.iter() {
                ex.set_header(k.as_str(), v.clone());
            }
            ex.set_header(headers::TOOL_NAME, entry.definition.name.as_str());
            ex.set_header(headers::APPROVAL_HANDLE, handle.as_str());
            if let Some(p) = &call.principal {
                ex.set_header(headers::PRINCIPAL, p.as_str());
            }
            ex.set_body(Value::Object(args).to_json());
            let out = ctx.create_producer_template().send(uri, ex);
            if let Some(e) = out.exception {
                cameleer_core::camel_warn!("ai-tool", "Approval endpoint {} failed for {}: {}", uri, handle, e);
            }
        }
        ToolResult {
            text: format!(
                "This call to {} needs approval before it runs ({}). It is waiting for an approver; its handle is {}.",
                entry.definition.name, reason, handle
            ),
            structured: None,
            is_error: false,
            status: ToolStatus::ApprovalRequired { handle },
            duplicate: false,
            return_direct: false,
            content: None,
            route_error: false,
        }
    }

    /// The calls waiting for approval (expired ones are dropped).
    pub fn pending_approvals(&self) -> Vec<ApprovalRequest> {
        self.expire_approvals();
        let mut v: Vec<ApprovalRequest> = self.approvals.lock().unwrap_or_else(|p| p.into_inner()).values().map(|p| p.request.clone()).collect();
        v.sort_by(|a, b| a.created.cmp(&b.created));
        v
    }

    fn expire_approvals(&self) {
        let tools = self.tools.read().unwrap_or_else(|p| p.into_inner()).clone();
        self.approvals.lock().unwrap_or_else(|p| p.into_inner()).retain(|_, p| {
            let limit = tools.get(&p.request.tool).map(|t| t.policy.approval_timeout).unwrap_or(Duration::from_secs(3600));
            p.at.elapsed() < limit
        });
    }

    /// Approve a parked call: it runs now (through the rest of the envelope) and its result
    /// is returned.
    pub fn approve(&self, handle: &str, approver: &str) -> ToolResult {
        self.expire_approvals();
        let pending = self.approvals.lock().unwrap_or_else(|p| p.into_inner()).remove(handle);
        match pending {
            Some(p) => {
                let r = self.call(p.call.approved_by(approver));
                self.remember(handle, &r);
                r
            }
            None => ToolResult::error(format!("No call is waiting for approval with handle {} (it may have expired)", handle)),
        }
    }

    fn remember(&self, handle: &str, r: &ToolResult) {
        let mut m = self.resolved.lock().unwrap_or_else(|p| p.into_inner());
        m.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(24 * 3600));
        m.insert(handle.to_string(), (Instant::now(), r.clone()));
    }

    /// The interaction of a running call, from the `CamelAiToolCallContext` property of its
    /// exchange.
    pub fn interaction(&self, ex: &Exchange) -> Option<Arc<dyn CallInteraction>> {
        let id = ex.property(CALL_CONTEXT).and_then(|v| v.as_i64())? as u64;
        self.interactions.lock().unwrap_or_else(|p| p.into_inner()).get(&id).cloned()
    }

    /// The result of a resolved approval (approved and run, or rejected), kept for a day.
    pub fn approval_result(&self, handle: &str) -> Option<ToolResult> {
        self.resolved.lock().unwrap_or_else(|p| p.into_inner()).get(handle).map(|(_, r)| r.clone())
    }

    /// Reject a parked call: it never runs.
    pub fn deny(&self, handle: &str, approver: &str, reason: &str) -> ToolResult {
        self.expire_approvals();
        let pending = self.approvals.lock().unwrap_or_else(|p| p.into_inner()).remove(handle);
        let Some(p) = pending else {
            return ToolResult::error(format!("No call is waiting for approval with handle {} (it may have expired)", handle));
        };
        let audit = AuditEntry {
            tool: p.request.tool.clone(),
            principal: p.request.principal.clone(),
            call_id: p.request.call_id.clone(),
            conversation_id: p.request.conversation_id.clone(),
            arguments_digest: sha256_hex(canonical_json(&p.request.arguments).as_bytes()),
            decision: "rejected".into(),
            approver: Some(approver.to_string()),
            outcome: "none".into(),
            detail: if reason.is_empty() { None } else { Some(reason.to_string()) },
            ..Default::default()
        };
        self.audit.append(&audit);
        let text = if reason.is_empty() {
            format!("The call to {} was rejected by {}", p.request.tool, approver)
        } else {
            format!("The call to {} was rejected by {}: {}", p.request.tool, approver, reason)
        };
        let r = ToolResult { text, structured: None, is_error: true, status: ToolStatus::Denied, duplicate: false, return_direct: false, content: None, route_error: false };
        self.remember(handle, &r);
        r
    }

    // --- compensation ---

    /// Undo a conversation: run the compensation route of every compensable call made in it,
    /// newest first. Returns each tool's name and whether its compensation succeeded.
    pub fn compensate(&self, conversation_id: &str) -> Vec<(String, Result<()>)> {
        let list = self.sagas.lock().unwrap_or_else(|p| p.into_inner()).remove(conversation_id).unwrap_or_default();
        let mut out = Vec::new();
        for c in list.into_iter().rev() {
            let Some(ctx) = c.ctx.upgrade() else {
                out.push((c.tool, Err(CamelError::illegal_state("context stopped"))));
                continue;
            };
            let mut ex = Exchange::new(Some(ctx.clone()));
            for (k, v) in c.arguments.iter() {
                ex.set_header(k.as_str(), v.clone());
            }
            ex.set_header(headers::TOOL_NAME, c.tool.as_str());
            ex.set_header(headers::CONVERSATION_ID, conversation_id);
            ex.set_header(headers::COMPENSATING, true);
            if let Some(id) = &c.call_id {
                ex.set_header(headers::CALL_ID, id.as_str());
            }
            ex.set_body(c.result.clone().map(Value::from).unwrap_or(Value::Null));
            let done = ctx.create_producer_template().send(&c.uri, ex);
            let r = match done.exception {
                Some(e) => Err(e),
                None => Ok(()),
            };
            let audit = AuditEntry {
                tool: c.tool.clone(),
                conversation_id: Some(conversation_id.to_string()),
                call_id: c.call_id.clone(),
                arguments_digest: sha256_hex(canonical_json(&Value::Object(c.arguments.clone())).as_bytes()),
                decision: "compensate".into(),
                outcome: if r.is_ok() { "ok" } else { "error" }.into(),
                detail: r.as_ref().err().map(|e| e.message.clone()),
                ..Default::default()
            };
            self.audit.append(&audit);
            out.push((c.tool, r));
        }
        out
    }

    /// End a conversation without undoing it: its compensations are dropped.
    pub fn complete(&self, conversation_id: &str) {
        self.sagas.lock().unwrap_or_else(|p| p.into_inner()).remove(conversation_id);
    }

    /// How many compensations a conversation has registered.
    pub fn compensations(&self, conversation_id: &str) -> usize {
        self.sagas.lock().unwrap_or_else(|p| p.into_inner()).get(conversation_id).map(|l| l.len()).unwrap_or(0)
    }

    // --- resources and prompts ---

    /// The URI of a resource published under `name`.
    pub fn resource_uri(&self, name: &str) -> Option<String> {
        self.resources.read().unwrap_or_else(|p| p.into_inner()).get(name).map(|r| r.uri.clone())
    }

    /// Read a resource by URI (a template's variables become headers).
    pub fn read_resource(&self, uri: &str) -> Result<ResourceContent> {
        let entries: Vec<Arc<ResourceEntry>> = self.resources.read().unwrap_or_else(|p| p.into_inner()).values().cloned().collect();
        let mut found = None;
        for e in &entries {
            if e.uri == uri {
                found = Some((e.clone(), Vec::new()));
                break;
            }
        }
        if found.is_none() {
            for e in entries.iter().filter(|e| e.is_template()) {
                if let Some(vars) = match_template(&e.uri, uri) {
                    found = Some((e.clone(), vars));
                    break;
                }
            }
        }
        let Some((entry, vars)) = found else {
            return Err(CamelError::new(RESOURCE_NOT_FOUND, format!("Resource not found: {}", uri)));
        };
        let ctx = entry.ctx.upgrade().ok_or_else(|| CamelError::illegal_state("The resource's context has stopped"))?;
        let mut ex = Exchange::new(Some(ctx));
        ex.pattern = ExchangePattern::InOut;
        ex.set_header(headers::RESOURCE_URI, uri);
        ex.set_header(headers::RESOURCE_NAME, entry.name.as_str());
        for (k, v) in vars {
            ex.set_header(k, v);
        }
        entry.processor.process(&mut ex)?;
        if let Some(e) = ex.exception.take() {
            return Err(e);
        }
        let mime = ex.header(headers::RESOURCE_MIME_TYPE).map(|v| v.to_string()).unwrap_or_else(|| entry.mime_type.clone());
        let (text, blob) = match std::mem::take(&mut ex.message.body) {
            Value::Bytes(b) if !is_textual(&mime) => (None, Some(b)),
            Value::Bytes(b) => (Some(String::from_utf8_lossy(&b).into_owned()), None),
            Value::Null => (Some(String::new()), None),
            v @ (Value::Object(_) | Value::Array(_)) => (Some(v.to_json()), None),
            v => (Some(v.to_string()), None),
        };
        Ok(ResourceContent { uri: uri.to_string(), mime_type: mime, text, blob })
    }

    /// Render a prompt with its arguments (required ones are checked).
    pub fn get_prompt(&self, name: &str, arguments: &BTreeMap<String, String>) -> Result<PromptResult> {
        let entry = self
            .prompts
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(name)
            .cloned()
            .ok_or_else(|| CamelError::new(PROMPT_NOT_FOUND, format!("Prompt not found: {}", name)))?;
        for (arg, _, required) in &entry.arguments {
            if *required && !arguments.contains_key(arg) {
                return Err(CamelError::illegal_argument(format!("Prompt {} needs the argument {}", name, arg)));
            }
        }
        let ctx = entry.ctx.upgrade().ok_or_else(|| CamelError::illegal_state("The prompt's context has stopped"))?;
        let mut ex = Exchange::new(Some(ctx));
        ex.pattern = ExchangePattern::InOut;
        ex.set_header(headers::PROMPT_NAME, name);
        for (k, v) in arguments {
            if entry.arguments.iter().any(|(a, _, _)| a == k) {
                ex.set_header(k.as_str(), v.as_str());
            }
        }
        entry.processor.process(&mut ex)?;
        if let Some(e) = ex.exception.take() {
            return Err(e);
        }
        let messages = match std::mem::take(&mut ex.message.body) {
            Value::Array(items) => items.iter().map(prompt_message).collect::<Result<Vec<_>>>()?,
            Value::Object(m) => vec![prompt_message(&Value::Object(m))?],
            Value::Null => Vec::new(),
            Value::Bytes(b) => vec![PromptMessage { role: "user".into(), text: String::from_utf8_lossy(&b).into_owned(), content: None }],
            v => vec![PromptMessage { role: "user".into(), text: v.to_string(), content: None }],
        };
        Ok(PromptResult { description: entry.description.clone(), messages })
    }
}

/// Error kind for an unknown resource URI.
pub const RESOURCE_NOT_FOUND: &str = "org.apache.camel.component.ai.ResourceNotFoundException";
/// Error kind for an unknown prompt.
pub const PROMPT_NOT_FOUND: &str = "org.apache.camel.component.ai.PromptNotFoundException";

enum IdemBegin {
    Run,
    Recorded(ToolResult),
    Busy,
}

enum Exec {
    Done(ToolResult),
    TimedOut,
}

struct Handoff {
    /// (result, caller gave up)
    state: Mutex<(Option<ToolResult>, bool)>,
    cv: Condvar,
}

fn run_route(entry: &ToolEntry, ex: &mut Exchange) -> ToolResult {
    let r = entry.processor.process(ex);
    if ex.is_cancelled() {
        return ToolResult::error(format!("Tool {} was cancelled", entry.definition.name));
    }
    // a client that can't answer a question is a refusal the caller should see, not an
    // internal failure to hide
    let failed = |e: &CamelError| ToolResult {
        route_error: e.kind != crate::component::INTERACTION_UNAVAILABLE && e.kind != crate::component::INPUT_REQUIRED,
        ..ToolResult::error(format!("Tool {} failed: {}", entry.definition.name, e.message))
    };
    if let Err(e) = r {
        return failed(&e);
    }
    if let Some(e) = &ex.exception {
        return failed(e);
    }
    let blocks = match ex.message.headers.remove(headers::CONTENT) {
        Some(Value::Array(a)) => Some(a),
        Some(v @ Value::Object(_)) => Some(vec![v]),
        Some(Value::String(s)) => match cameleer_core::json::parse(&s) {
            Ok(Value::Array(a)) => Some(a),
            Ok(v @ Value::Object(_)) => Some(vec![v]),
            _ => None,
        },
        _ => None,
    };
    let body = std::mem::take(&mut ex.message.body);
    match blocks {
        Some(blocks) => {
            // the text mirror: the text blocks, else the body
            let texts: Vec<String> = blocks.iter().filter_map(|b| b.as_object()).filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text")).filter_map(|b| b.get("text").map(|t| t.to_string())).collect();
            let mut r = if texts.is_empty() { to_result(&entry.definition, body) } else { ToolResult::ok(texts.join("\n"), None) };
            r.content = Some(blocks);
            r
        }
        None => to_result(&entry.definition, body),
    }
}

/// The body of a finished call as a result: an object or array is structured content (with
/// its JSON as the text), an empty body is "No result" (as in Camel).
fn to_result(def: &ToolDefinition, body: Value) -> ToolResult {
    let (text, mut structured) = match body {
        Value::Null => ("No result".to_string(), None),
        v @ (Value::Object(_) | Value::Array(_)) => (v.to_json(), Some(v)),
        Value::Bytes(b) => (String::from_utf8_lossy(&b).into_owned(), None),
        v => (v.to_string(), None),
    };
    let text = if text.is_empty() { "No result".to_string() } else { text };
    if let Some(schema) = &def.output_schema {
        if structured.is_none() {
            if let Ok(v @ (Value::Object(_) | Value::Array(_))) = cameleer_core::json::parse(&text) {
                structured = Some(v);
            }
        }
        let Some(s) = &structured else {
            return ToolResult::error(format!("Tool {} declares an outputSchema but returned no structured result", def.name));
        };
        let errors = validate_schema(schema, s);
        if !errors.is_empty() {
            return ToolResult::error(format!("Tool {} returned a result that does not match its outputSchema:\n- {}", def.name, errors.join("\n- ")));
        }
    }
    ToolResult::ok(text, structured)
}

/// Drop `camel*` arguments (any case) and, unless the schema accepts them, undeclared ones.
fn filter_arguments(def: &ToolDefinition, args: &Value) -> std::result::Result<ObjectMap, String> {
    let obj = match args {
        Value::Null => ObjectMap::new(),
        Value::Object(m) => m.clone(),
        Value::String(s) if s.trim().is_empty() => ObjectMap::new(),
        Value::String(s) => match cameleer_core::json::parse(s) {
            Ok(Value::Object(m)) => m,
            _ => return Err(format!("The arguments for tool {} must be a JSON object", def.name)),
        },
        _ => return Err(format!("The arguments for tool {} must be a JSON object", def.name)),
    };
    let declared = def.declared_arguments();
    let open = def.accepts_undeclared_arguments();
    let mut out = ObjectMap::new();
    for (k, v) in obj.iter() {
        if k.len() >= 5 && k[..5].eq_ignore_ascii_case("camel") {
            continue;
        }
        if open || declared.iter().any(|d| d == k) {
            out.insert(k.clone(), v.clone());
        }
    }
    Ok(out)
}

fn needs_approval(entry: &ToolEntry, args: &ObjectMap, call: &ToolCall) -> Result<Option<String>> {
    match &entry.policy.approval {
        ApprovalRule::Never => Ok(None),
        ApprovalRule::Always => Ok(Some(if entry.definition.hints.destructive == Some(true) {
            "the tool is destructive".to_string()
        } else {
            "the tool always needs approval".to_string()
        })),
        ApprovalRule::When(pred, text) => {
            let ctx = entry.ctx.upgrade();
            let mut ex = Exchange::new(ctx);
            for (k, v) in args.iter() {
                ex.set_header(k.as_str(), v.clone());
            }
            ex.set_body(Value::Object(args.clone()).to_json());
            ex.set_header(headers::TOOL_NAME, entry.definition.name.as_str());
            if let Some(p) = &call.principal {
                ex.set_header(headers::PRINCIPAL, p.as_str());
            }
            for (k, v) in &call.properties {
                ex.set_property(k.as_str(), v.clone());
            }
            Ok(if pred.matches(&ex)? { Some(format!("approval rule {}", text)) } else { None })
        }
    }
}

/// The key a call is deduplicated on: the declared `idempotency_key` argument, else the call
/// id, else a digest of the principal and arguments.
fn idempotency_key(def: &ToolDefinition, args: &ObjectMap, call: &ToolCall) -> String {
    if def.declared_arguments().iter().any(|a| a == "idempotency_key") {
        if let Some(v) = args.get("idempotency_key").filter(|v| !v.is_null()) {
            return format!("key:{}", v);
        }
    }
    if let Some(id) = &call.call_id {
        return format!("call:{}", id);
    }
    let digest = sha256_hex(format!("{}\u{0}{}", call.principal.as_deref().unwrap_or(""), canonical_json(&Value::Object(args.clone()))).as_bytes());
    format!("args:{}", digest)
}

fn property_lists(entry: &ToolEntry, property: &str, name: &str) -> bool {
    entry
        .ctx
        .upgrade()
        .and_then(|c| c.properties().get(property))
        .map(|v| v.split(',').any(|s| s.trim() == name))
        .unwrap_or(false)
}

fn prompt_message(v: &Value) -> Result<PromptMessage> {
    match v {
        Value::Object(m) => {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user").to_string();
            if role != "user" && role != "assistant" {
                return Err(CamelError::illegal_argument(format!("Prompt message role must be user or assistant, got {}", role)));
            }
            let (text, content) = match m.get("content").or_else(|| m.get("text")) {
                Some(Value::Object(c)) => {
                    let text = c.get("text").map(|t| t.to_string()).unwrap_or_default();
                    // a typed block other than text (image, audio, resource) is kept as it is
                    let raw = match c.get("type").and_then(|t| t.as_str()) {
                        Some("text") | None => None,
                        Some(_) => Some(Value::Object(c.clone())),
                    };
                    (text, raw)
                }
                Some(t) => (t.to_string(), None),
                None => (String::new(), None),
            };
            Ok(PromptMessage { role, text, content })
        }
        other => Ok(PromptMessage { role: "user".into(), text: other.to_string(), content: None }),
    }
}

/// Match a URI against an RFC 6570 level-1 template (`file:///{path}`, `db://{table}/{id}`).
/// `{name}` matches up to the next literal (or the end); returns the variables.
pub fn match_template(template: &str, uri: &str) -> Option<Vec<(String, String)>> {
    let mut vars = Vec::new();
    let mut t = template;
    let mut u = uri;
    while !t.is_empty() {
        if let Some(rest) = t.strip_prefix('{') {
            let end = rest.find('}')?;
            let name = &rest[..end];
            t = &rest[end + 1..];
            let next_literal: &str = match t.find('{') {
                Some(i) => &t[..i],
                None => t,
            };
            let value_len = if next_literal.is_empty() { u.len() } else { u.find(next_literal)? };
            if value_len == 0 {
                return None;
            }
            vars.push((name.to_string(), cameleer_core::util::url_decode(&u[..value_len])));
            u = &u[value_len..];
        } else {
            let lit_end = t.find('{').unwrap_or(t.len());
            let lit = &t[..lit_end];
            u = u.strip_prefix(lit)?;
            t = &t[lit_end..];
        }
    }
    if u.is_empty() {
        Some(vars)
    } else {
        None
    }
}

fn is_textual(mime: &str) -> bool {
    let m = mime.to_ascii_lowercase();
    m.starts_with("text/") || m.contains("json") || m.contains("xml") || m.contains("yaml") || m.contains("javascript")
}

fn fmt_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms % 60_000 == 0 && ms >= 60_000 {
        format!("{} min", ms / 60_000)
    } else if ms % 1000 == 0 {
        format!("{} s", ms / 1000)
    } else {
        format!("{} ms", ms)
    }
}

fn load_manifest(path: &std::path::Path) -> Result<BTreeMap<String, (String, String)>> {
    let text = std::fs::read_to_string(path).map_err(|e| CamelError::illegal_state(format!("Cannot read the tool manifest {}: {}", path.display(), e)))?;
    let v = cameleer_core::json::parse(&text).map_err(|e| CamelError::illegal_state(format!("Invalid tool manifest {}: {}", path.display(), e.message)))?;
    let mut out = BTreeMap::new();
    if let Some(tools) = v.as_object().and_then(|m| m.get("tools")).and_then(|t| t.as_object()) {
        for (name, t) in tools.iter() {
            let version = t.as_object().and_then(|m| m.get("version")).map(|x| x.to_string()).unwrap_or_default();
            let hash = t.as_object().and_then(|m| m.get("hash")).map(|x| x.to_string()).unwrap_or_default();
            out.insert(name.clone(), (version, hash));
        }
    }
    Ok(out)
}

fn save_manifest(path: &std::path::Path, pins: &BTreeMap<String, (String, String)>) -> Result<()> {
    let mut tools = ObjectMap::new();
    for (name, (version, hash)) in pins {
        let mut t = ObjectMap::new();
        t.insert("version".into(), Value::from(version.as_str()));
        t.insert("hash".into(), Value::from(hash.as_str()));
        tools.insert(name.clone(), Value::Object(t));
    }
    let mut root = ObjectMap::new();
    root.insert("tools".into(), Value::Object(tools));
    std::fs::write(path, Value::Object(root).to_json_pretty() + "\n")
        .map_err(|e| CamelError::illegal_state(format!("Cannot write the tool manifest {}: {}", path.display(), e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_templates() {
        assert_eq!(match_template("file:///{path}", "file:///a/b.txt"), Some(vec![("path".into(), "a/b.txt".into())]));
        assert_eq!(
            match_template("db://{table}/{id}", "db://orders/42"),
            Some(vec![("table".into(), "orders".into()), ("id".into(), "42".into())])
        );
        assert_eq!(match_template("db://{table}/{id}", "db://orders"), None);
        assert_eq!(match_template("db://x", "db://y"), None);
        assert_eq!(match_template("db://{table}/", "db:///"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(Duration::from_secs(120)), "2 min");
        assert_eq!(fmt_duration(Duration::from_secs(20)), "20 s");
        assert_eq!(fmt_duration(Duration::from_millis(150)), "150 ms");
    }
}

/// The registry as the context's [`cameleer_core::ai::ToolProvider`]: the `agent` step and the
/// model components' tool loops call tools through the envelope.
pub struct RegistryTools {
    pub(crate) ctx: WeakContext,
}

impl RegistryTools {
    fn registry(&self) -> Option<Arc<AiRegistry>> {
        self.ctx.upgrade().and_then(|c| AiRegistry::of(&c).ok())
    }
}

fn outcome(r: ToolResult) -> cameleer_core::ai::ToolOutcome {
    use cameleer_core::ai::{ToolOutcome, ToolOutcomeStatus};
    let status = match &r.status {
        ToolStatus::Completed => ToolOutcomeStatus::Completed,
        ToolStatus::Error => ToolOutcomeStatus::Error,
        ToolStatus::ApprovalRequired { handle } => ToolOutcomeStatus::ApprovalRequired(handle.clone()),
        ToolStatus::Denied => ToolOutcomeStatus::Denied,
    };
    ToolOutcome { text: r.text, is_error: r.is_error, status }
}

impl cameleer_core::ai::ToolProvider for RegistryTools {
    fn tools(&self, tags: &[String]) -> Vec<cameleer_core::ai::ToolSpec> {
        let Some(reg) = self.registry() else { return Vec::new() };
        let all = tags.iter().any(|t| t == "*");
        let wanted: Vec<&str> = tags.iter().map(|t| t.as_str()).collect();
        if tags.is_empty() {
            return Vec::new();
        }
        reg.tools(if all { &[] } else { &wanted })
            .into_iter()
            .map(|d| cameleer_core::ai::ToolSpec { name: d.name, description: d.description, input_schema: d.input_schema })
            .collect()
    }
    fn call(&self, inv: cameleer_core::ai::ToolInvocation) -> cameleer_core::ai::ToolOutcome {
        let Some(reg) = self.registry() else { return outcome(ToolResult::error("The tool registry is gone")) };
        let call = ToolCall {
            name: inv.name,
            arguments: inv.arguments,
            call_id: if inv.id.is_empty() { None } else { Some(inv.id) },
            conversation_id: inv.conversation_id,
            principal: inv.principal,
            approved_by: None,
            properties: inv.properties,
            headers: Vec::new(),
            interaction: Interaction::default(),
        };
        outcome(reg.call(call))
    }
    fn approval_outcome(&self, handle: &str) -> Option<cameleer_core::ai::ToolOutcome> {
        self.registry().and_then(|r| r.approval_result(handle)).map(outcome)
    }
}
