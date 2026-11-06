//! The kill switch and approvals of the AI action layer, for operators: `cameleer tool ...`
//! (through the local status files, see [`crate::local`]) and the `/q/dev/tools` console
//! (see [`crate::console`]).
//!
//! | action document | effect | `data` of the result |
//! |---|---|---|
//! | `{"action":"tool-list"}` | | [`tools_value`] |
//! | `{"action":"tool-disable","tool":"T"}` | `AiRegistry::disable` | the tool list |
//! | `{"action":"tool-enable","tool":"T"}` | `AiRegistry::enable` | the tool list |
//! | `{"action":"tool-approvals"}` | | [`approvals_value`] |
//! | `{"action":"tool-approve","handle":"H","by":"ann"}` | `AiRegistry::approve` | the call's result |
//! | `{"action":"tool-deny","handle":"H","by":"ann","reason":"R"}` | `AiRegistry::deny` | the call's result |
//!
//! Disabling withdraws the tool from every list (MCP clients get `notifications/tools/list_changed`
//! through the registry's listeners) and refuses its calls until it is enabled again.

use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::Value;

/// The message for a process or binary built without the `ai` feature.
pub const NO_AI_FEATURE: &str = "this build of cameleer has no AI tools: it was built without the `ai` feature (rebuild with --features ai)";

/// Whether this build has the action layer.
pub fn available() -> bool {
    cfg!(feature = "ai")
}

/// Whether an action document is one of the `tool-*` actions.
pub fn is_tool_action(action: &Value) -> bool {
    action.get("action").map(|a| a.to_string()).map(|a| a.starts_with("tool-")).unwrap_or(false)
}

fn text(action: &Value, key: &str) -> Option<String> {
    action.get(key).filter(|v| !v.is_null()).map(|v| v.to_string()).filter(|s| !s.is_empty())
}

/// Execute a `tool-*` action document: returns the message and the data for the CLI.
pub fn execute(ctx: &CamelContext, action: &Value) -> Result<(String, Value)> {
    let name = text(action, "action").unwrap_or_default();
    match name.as_str() {
        "tool-list" => Ok((String::new(), tools_value(ctx)?)),
        "tool-disable" | "tool-enable" => {
            let tool = text(action, "tool").ok_or_else(|| CamelError::illegal_argument(format!("{} needs a tool name", name)))?;
            let on = name == "tool-enable";
            set_enabled(ctx, &tool, on)?;
            Ok((format!("tool {} {}", tool, if on { "enabled" } else { "disabled" }), tools_value(ctx)?))
        }
        "tool-approvals" => Ok((String::new(), approvals_value(ctx)?)),
        "tool-approve" | "tool-deny" => {
            let handle = text(action, "handle").ok_or_else(|| CamelError::illegal_argument(format!("{} needs an approval handle", name)))?;
            let by = text(action, "by").unwrap_or_else(|| "cli".into());
            let reason = text(action, "reason").unwrap_or_default();
            resolve(ctx, &handle, name == "tool-approve", &by, &reason)
        }
        other => Err(CamelError::illegal_argument(format!("Unknown action {}", other))),
    }
}

#[cfg(not(feature = "ai"))]
fn no_ai() -> CamelError {
    CamelError::illegal_state(NO_AI_FEATURE)
}

/// The tools of a context: name, title, tags, hints, version, disabled flag, plus disabled
/// names that no route publishes and the number of calls waiting for approval.
#[cfg(feature = "ai")]
pub fn tools_value(ctx: &CamelContext) -> Result<Value> {
    let reg = cameleer_ai::AiRegistry::of(ctx)?;
    let disabled = reg.disabled_tools();
    let hint = |h: Option<bool>| h.map(Value::Bool).unwrap_or(Value::Null);
    let all = reg.all_tools();
    let tools = all
        .iter()
        .map(|t| {
            Value::object([
                ("name", Value::from(t.name.as_str())),
                ("title", t.title.as_deref().map(Value::from).unwrap_or(Value::Null)),
                ("description", Value::from(t.description.as_str())),
                ("tags", Value::Array(t.tags.iter().map(|g| Value::from(g.as_str())).collect())),
                ("version", Value::from(t.version.as_str())),
                ("readOnly", hint(t.hints.read_only)),
                ("destructive", hint(t.hints.destructive)),
                ("disabled", Value::Bool(disabled.contains(&t.name))),
            ])
        })
        .collect();
    let unpublished = disabled.iter().filter(|d| !all.iter().any(|t| &t.name == *d)).map(|d| Value::from(d.as_str())).collect();
    Ok(Value::object([
        ("tools", Value::Array(tools)),
        ("disabledUnpublished", Value::Array(unpublished)),
        ("pendingApprovals", Value::from(reg.pending_approvals().len() as i64)),
    ]))
}

#[cfg(not(feature = "ai"))]
pub fn tools_value(_ctx: &CamelContext) -> Result<Value> {
    Err(no_ai())
}

/// The calls waiting for approval: handle, tool, principal, reason, age.
#[cfg(feature = "ai")]
pub fn approvals_value(ctx: &CamelContext) -> Result<Value> {
    let reg = cameleer_ai::AiRegistry::of(ctx)?;
    let list = reg
        .pending_approvals()
        .into_iter()
        .map(|a| {
            let age = a.created.elapsed().unwrap_or_default().as_millis() as i64;
            Value::object([
                ("handle", Value::from(a.handle.as_str())),
                ("tool", Value::from(a.tool.as_str())),
                ("principal", a.principal.as_deref().map(Value::from).unwrap_or(Value::Null)),
                ("conversationId", a.conversation_id.as_deref().map(Value::from).unwrap_or(Value::Null)),
                ("reason", Value::from(a.reason.as_str())),
                ("ageMillis", Value::from(age)),
                ("age", Value::from(cameleer_core::management::format_uptime(age as u64))),
            ])
        })
        .collect();
    Ok(Value::object([("approvals", Value::Array(list))]))
}

#[cfg(not(feature = "ai"))]
pub fn approvals_value(_ctx: &CamelContext) -> Result<Value> {
    Err(no_ai())
}

/// Disable or enable a tool. A name that no route publishes is refused unless it is
/// already disabled (so a typo doesn't silently pre-disable a tool).
#[cfg(feature = "ai")]
pub fn set_enabled(ctx: &CamelContext, tool: &str, enabled: bool) -> Result<()> {
    let reg = cameleer_ai::AiRegistry::of(ctx)?;
    if reg.tool(tool).is_none() && !reg.is_disabled(tool) {
        let names: Vec<String> = reg.all_tools().into_iter().map(|t| t.name).collect();
        return Err(CamelError::illegal_argument(format!(
            "No tool named {} (tools: {})",
            tool,
            if names.is_empty() { "none".to_string() } else { names.join(", ") }
        )));
    }
    if enabled {
        reg.enable(tool);
    } else {
        reg.disable(tool);
    }
    Ok(())
}

#[cfg(not(feature = "ai"))]
pub fn set_enabled(_ctx: &CamelContext, _tool: &str, _enabled: bool) -> Result<()> {
    Err(no_ai())
}

/// Approve (the call runs now) or deny a parked call; an unknown handle is an error.
#[cfg(feature = "ai")]
pub fn resolve(ctx: &CamelContext, handle: &str, approve: bool, by: &str, reason: &str) -> Result<(String, Value)> {
    let reg = cameleer_ai::AiRegistry::of(ctx)?;
    if !reg.pending_approvals().iter().any(|a| a.handle == handle) {
        return Err(CamelError::illegal_argument(format!("No call is waiting for approval with handle {} (it may have expired)", handle)));
    }
    let r = if approve { reg.approve(handle, by) } else { reg.deny(handle, by, reason) };
    let data = Value::object([
        ("handle", Value::from(handle)),
        ("status", Value::from(r.status.as_str())),
        ("isError", Value::Bool(r.is_error)),
        ("text", Value::from(r.text.as_str())),
    ]);
    let msg = if approve { format!("approved {}: {}", handle, r.text) } else { r.text.clone() };
    Ok((msg, data))
}

#[cfg(not(feature = "ai"))]
pub fn resolve(_ctx: &CamelContext, _handle: &str, _approve: bool, _by: &str, _reason: &str) -> Result<(String, Value)> {
    Err(no_ai())
}

fn cell(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::Array(a)) => a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","),
        Some(v) => v.to_string(),
    }
}

fn table(headers: &[&str], rows: Vec<Vec<String>>) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in &rows {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let fmt_row = |cells: Vec<String>| -> String {
        let parts: Vec<String> = cells.iter().enumerate().map(|(i, c)| format!("{:<w$}", c, w = widths[i])).collect();
        parts.join("  ").trim_end().to_string()
    };
    let mut out = fmt_row(headers.iter().map(|s| s.to_string()).collect());
    out.push('\n');
    for row in rows {
        out.push_str(&fmt_row(row));
        out.push('\n');
    }
    out
}

/// Format [`tools_value`] as a table (`cameleer tool list`).
pub fn format_tools(v: &Value) -> String {
    let empty = Vec::new();
    let tools = v.get("tools").and_then(|t| t.as_array()).unwrap_or(&empty);
    let flag = |x: Option<&Value>| match x {
        Some(Value::Bool(true)) => "yes".to_string(),
        Some(Value::Bool(false)) => "no".to_string(),
        _ => String::new(),
    };
    let rows = tools
        .iter()
        .map(|t| {
            vec![
                cell(t.get("name")),
                cell(t.get("tags")),
                flag(t.get("readOnly")),
                flag(t.get("destructive")),
                cell(t.get("version")),
                if t.get("disabled").map(|d| d.is_truthy()).unwrap_or(false) { "disabled".to_string() } else { "enabled".to_string() },
            ]
        })
        .collect();
    let mut out = table(&["NAME", "TAGS", "READ-ONLY", "DESTRUCTIVE", "VERSION", "STATUS"], rows);
    if let Some(names) = v.get("disabledUnpublished").and_then(|d| d.as_array()).filter(|d| !d.is_empty()) {
        out.push_str(&format!("Disabled, not published: {}\n", names.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")));
    }
    out.push_str(&format!("Pending approvals: {}\n", cell(v.get("pendingApprovals"))));
    out
}

/// Format [`approvals_value`] as a table (`cameleer tool approvals`).
pub fn format_approvals(v: &Value) -> String {
    let empty = Vec::new();
    let list = v.get("approvals").and_then(|t| t.as_array()).unwrap_or(&empty);
    if list.is_empty() {
        return "No calls are waiting for approval\n".to_string();
    }
    let rows = list.iter().map(|a| vec![cell(a.get("handle")), cell(a.get("tool")), cell(a.get("principal")), cell(a.get("age")), cell(a.get("reason"))]).collect();
    table(&["HANDLE", "TOOL", "PRINCIPAL", "AGE", "REASON"], rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables() {
        let v = cameleer_core::json::parse(
            r#"{"tools":[{"name":"refund","tags":["orders","public"],"version":"2","readOnly":false,"destructive":true,"disabled":true},
            {"name":"lookup","tags":[],"version":"1","readOnly":true,"destructive":null,"disabled":false}],
            "disabledUnpublished":["legacy"],"pendingApprovals":1}"#,
        )
        .unwrap();
        let t = format_tools(&v);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "NAME    TAGS           READ-ONLY  DESTRUCTIVE  VERSION  STATUS", "{}", t);
        assert_eq!(lines[1], "refund  orders,public  no         yes          2        disabled", "{}", t);
        assert_eq!(lines[2], "lookup                 yes                     1        enabled", "{}", t);
        assert_eq!(lines[3], "Disabled, not published: legacy");
        assert_eq!(lines[4], "Pending approvals: 1");
        let a = cameleer_core::json::parse(r#"{"approvals":[{"handle":"h1","tool":"refund","principal":"bot","age":"3s","reason":"Tool refund is destructive"}]}"#).unwrap();
        let t = format_approvals(&a);
        assert!(t.lines().nth(1).unwrap().starts_with("h1      refund  bot        3s   Tool refund is destructive"), "{}", t);
        assert_eq!(format_approvals(&cameleer_core::json::parse(r#"{"approvals":[]}"#).unwrap()), "No calls are waiting for approval\n");
    }

    #[test]
    fn tool_actions_are_recognised() {
        assert!(is_tool_action(&cameleer_core::json::parse(r#"{"action":"tool-list"}"#).unwrap()));
        assert!(!is_tool_action(&cameleer_core::json::parse(r#"{"action":"stop"}"#).unwrap()));
    }

    #[cfg(not(feature = "ai"))]
    #[test]
    fn without_the_ai_feature_the_actions_say_so() {
        let ctx = CamelContext::new();
        let e = execute(&ctx, &cameleer_core::json::parse(r#"{"action":"tool-list"}"#).unwrap()).unwrap_err();
        assert!(e.to_string().contains("without the `ai` feature"), "{}", e);
    }

    #[cfg(feature = "ai")]
    #[test]
    fn disable_enable_approve_and_deny() {
        let ctx = CamelContext::new();
        cameleer_ai::register(&ctx);
        ctx.add_routes(|b| {
            b.from("ai-tool:lookup?readOnlyHint=true&tags=public").set_body(cameleer_core::builder::constant("found"));
            b.from("ai-tool:wipe?destructiveHint=true&version=3").set_body(cameleer_core::builder::constant("wiped"));
        })
        .unwrap();
        ctx.start().unwrap();
        let act = |s: &str| execute(&ctx, &cameleer_core::json::parse(s).unwrap());
        let (_, list) = act(r#"{"action":"tool-list"}"#).unwrap();
        let j = list.to_json();
        assert!(j.contains(r#""name":"lookup""#) && j.contains(r#""version":"3""#) && j.contains(r#""pendingApprovals":0"#), "{}", j);
        // disable: gone from the agent-facing list, still in the management list
        let (msg, list) = act(r#"{"action":"tool-disable","tool":"lookup"}"#).unwrap();
        assert_eq!(msg, "tool lookup disabled");
        assert!(format_tools(&list).contains("disabled"));
        let reg = cameleer_ai::AiRegistry::of(&ctx).unwrap();
        assert!(reg.tools(&[]).iter().all(|t| t.name != "lookup"));
        assert!(act(r#"{"action":"tool-disable","tool":"lokup"}"#).unwrap_err().to_string().contains("No tool named lokup (tools: lookup, wipe)"));
        act(r#"{"action":"tool-enable","tool":"lookup"}"#).unwrap();
        assert!(!reg.is_disabled("lookup"));
        // approvals
        let parked = |reg: &cameleer_ai::AiRegistry| match reg.call(cameleer_ai::ToolCall::new("wipe", Value::Null).with_principal("bot")).status {
            cameleer_ai::ToolStatus::ApprovalRequired { handle } => handle,
            other => panic!("not parked: {:?}", other),
        };
        let h = parked(&reg);
        let (_, a) = act(r#"{"action":"tool-approvals"}"#).unwrap();
        assert!(format_approvals(&a).contains(&h) && a.to_json().contains(r#""principal":"bot""#), "{}", a.to_json());
        let (msg, data) = act(&format!(r#"{{"action":"tool-approve","handle":"{}","by":"ann"}}"#, h)).unwrap();
        assert!(msg.contains("wiped") && data.to_json().contains(r#""status":"completed""#), "{} {}", msg, data.to_json());
        assert!(act(&format!(r#"{{"action":"tool-approve","handle":"{}"}}"#, h)).unwrap_err().to_string().contains("No call is waiting"));
        let h2 = parked(&reg);
        let (msg, _) = act(&format!(r#"{{"action":"tool-deny","handle":"{}","by":"ann","reason":"not today"}}"#, h2)).unwrap();
        assert!(msg.contains("rejected by ann: not today"), "{}", msg);
        assert!(reg.pending_approvals().is_empty());
        ctx.stop().unwrap();
    }
}
