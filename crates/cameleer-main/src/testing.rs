//! `cameleer test`: run routes from a YAML test file and check what they do, without a model
//! (agent and model endpoints replay recorded responses, `replay=file:...`).
//!
//! ```yaml
//! - test:
//!     name: a refund waits for approval, then runs once
//!     routes: [refund.camel.yaml]            # relative to the test file
//!     properties:
//!       agent.replay: file:refund.jsonl   # file: paths are relative to the test file
//!     steps:
//!       - send:
//!           to: direct:agent
//!           body: refund order A1
//!           headers: {CamelAiConversationId: c1}
//!         expect:
//!           headers: {CamelAiAgentStopReason: approval-required}
//!           mocks: {"mock:refunds": 0}
//!       - approve: {tool: refund, by: alice}
//!         expect: {mocks: {"mock:refunds": 1}}
//!       - callTool: {name: refund, arguments: {orderId: A1}, callId: c-1, approvedBy: alice}
//!         expect: {duplicate: true}
//!       - compensate: {conversation: c1}
//! ```
//!
//! Steps: `send` (to, body, headers), `callTool` (name, arguments, callId, conversation,
//! principal, approvedBy), `approve` / `deny` (tool, by, reason: the oldest call of that tool
//! waiting for approval) and `compensate` (conversation). `expect` checks `body`,
//! `bodyContains`, `headers`, `error` (the message contains it), `status`, `isError`,
//! `duplicate` and `mocks` (messages received per mock endpoint).

use std::io::Write;
use std::path::{Path, PathBuf};

use cameleer_core::message::Exchange;
use cameleer_core::value::Value;

/// One step's observable result.
#[derive(Default)]
struct Observed {
    body: Option<Value>,
    headers: Vec<(String, Value)>,
    error: Option<String>,
    status: Option<String>,
    is_error: Option<bool>,
    duplicate: Option<bool>,
}

/// Run the tests in `files`; returns (passed, failed).
pub fn run_files(files: &[PathBuf], out: &mut dyn Write) -> Result<(usize, usize), String> {
    let mut passed = 0;
    let mut failed = 0;
    for f in files {
        let text = std::fs::read_to_string(f).map_err(|e| format!("Cannot read {}: {}", f.display(), e))?;
        let doc = cameleer_yaml::parse_yaml(&text).map_err(|e| format!("{}: {}", f.display(), e))?;
        let items = match doc {
            Value::Array(a) => a,
            Value::Null => Vec::new(),
            other => vec![other],
        };
        let dir = f.parent().map(Path::to_path_buf).unwrap_or_default();
        for item in items {
            let Some(t) = item.as_object().and_then(|m| m.get("test")).cloned() else {
                return Err(format!("{}: every entry must be a `test:` mapping", f.display()));
            };
            let name = t.get("name").map(|n| n.to_string()).unwrap_or_else(|| "unnamed".into());
            match run_test(&t, &dir) {
                Ok(()) => {
                    passed += 1;
                    let _ = writeln!(out, "PASS {}", name);
                }
                Err(e) => {
                    failed += 1;
                    let _ = writeln!(out, "FAIL {}\n     {}", name, e);
                }
            }
        }
    }
    let _ = writeln!(out, "{} passed, {} failed", passed, failed);
    Ok((passed, failed))
}

fn run_test(t: &Value, dir: &Path) -> Result<(), String> {
    let mut main = crate::new_main();
    if let Some(Value::Object(props)) = t.get("properties") {
        for (k, v) in props.iter() {
            let mut value = v.to_string();
            // `file:` paths are relative to the test file
            if let Some(path) = value.strip_prefix("file:") {
                if !Path::new(path).is_absolute() {
                    value = format!("file:{}", dir.join(path).to_string_lossy().replace('\\', "/"));
                }
            }
            main.set_property(k.clone(), value);
        }
    }
    main.set_property("camel.main.name", "cameleer-test");
    let routes: Vec<String> = match t.get("routes") {
        Some(Value::Array(a)) => a.iter().map(|r| r.to_string()).collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    };
    if routes.is_empty() {
        return Err("the test lists no routes".into());
    }
    for r in &routes {
        let p = if Path::new(r).is_absolute() { PathBuf::from(r) } else { dir.join(r) };
        main.add_route_file(p.to_string_lossy().replace('\\', "/"));
    }
    main.start().map_err(|e| format!("the routes did not start: {}", e))?;
    let result = run_steps(&main, t);
    let _ = main.stop();
    result
}

fn run_steps(main: &crate::Main, t: &Value) -> Result<(), String> {
    let ctx = main.context().clone();
    let steps = t.get("steps").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    for (i, step) in steps.iter().enumerate() {
        let m = step.as_object().ok_or_else(|| format!("step {} is not a mapping", i + 1))?;
        let fail = |msg: String| format!("step {}: {}", i + 1, msg);
        let observed = if let Some(s) = m.get("send") {
            let to = s.get("to").map(|v| v.to_string()).ok_or_else(|| fail("send needs `to`".into()))?;
            let mut ex = Exchange::new(Some(ctx.clone()));
            ex.set_body(s.get("body").cloned().unwrap_or(Value::Null));
            if let Some(Value::Object(h)) = s.get("headers") {
                for (k, v) in h.iter() {
                    ex.set_header(k.as_str(), v.clone());
                }
            }
            let done = ctx.create_producer_template().send(&to, ex);
            Observed {
                body: Some(done.body().clone()),
                headers: done.message.headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
                error: done.exception.as_ref().map(|e| e.to_string()),
                ..Default::default()
            }
        } else {
            tool_step(&ctx, m).map_err(fail)?
        };
        if let Some(e) = m.get("expect") {
            check(e, &observed, &ctx).map_err(fail)?;
        } else if let Some(err) = &observed.error {
            return Err(fail(format!("failed: {}", err)));
        }
    }
    Ok(())
}

#[cfg(feature = "ai")]
fn tool_step(ctx: &cameleer_core::context::CamelContext, m: &cameleer_core::value::ObjectMap) -> Result<Observed, String> {
    use cameleer_ai::{AiRegistry, ToolCall};
    let reg = AiRegistry::of(ctx).map_err(|e| e.to_string())?;
    let s = |v: &Value, k: &str| v.get(k).filter(|x| !x.is_null()).map(|x| x.to_string());
    let from = |r: cameleer_ai::ToolResult| Observed {
        body: Some(r.structured.clone().unwrap_or_else(|| Value::from(r.text.clone()))),
        status: Some(r.status.as_str().to_string()),
        is_error: Some(r.is_error),
        duplicate: Some(r.duplicate),
        ..Default::default()
    };
    if let Some(c) = m.get("callTool") {
        let call = ToolCall {
            name: s(c, "name").ok_or("callTool needs `name`")?,
            arguments: c.get("arguments").cloned().unwrap_or(Value::Null),
            call_id: s(c, "callId"),
            conversation_id: s(c, "conversation"),
            principal: s(c, "principal"),
            approved_by: s(c, "approvedBy"),
            ..Default::default()
        };
        return Ok(from(reg.call(call)));
    }
    for op in ["approve", "deny"] {
        if let Some(a) = m.get(op) {
            let tool = s(a, "tool").ok_or_else(|| format!("{} needs `tool`", op))?;
            let by = s(a, "by").unwrap_or_else(|| "cameleer-test".into());
            let pending = reg.pending_approvals().into_iter().find(|p| p.tool == tool).ok_or_else(|| format!("no call of {} is waiting for approval", tool))?;
            let r = if op == "approve" { reg.approve(&pending.handle, &by) } else { reg.deny(&pending.handle, &by, &s(a, "reason").unwrap_or_default()) };
            return Ok(from(r));
        }
    }
    if let Some(c) = m.get("compensate") {
        let conv = s(c, "conversation").ok_or("compensate needs `conversation`")?;
        let results = reg.compensate(&conv);
        let failed: Vec<String> = results.iter().filter_map(|(t, r)| r.as_ref().err().map(|e| format!("{}: {}", t, e))).collect();
        return Ok(Observed {
            body: Some(Value::Array(results.iter().map(|(t, _)| Value::from(t.as_str())).collect())),
            error: if failed.is_empty() { None } else { Some(failed.join("; ")) },
            ..Default::default()
        });
    }
    Err(format!("unknown step {:?} (send, callTool, approve, deny, compensate)", m.keys().collect::<Vec<_>>()))
}

#[cfg(not(feature = "ai"))]
fn tool_step(_ctx: &cameleer_core::context::CamelContext, m: &cameleer_core::value::ObjectMap) -> Result<Observed, String> {
    Err(format!("step {:?} needs the ai feature (only send works without it)", m.keys().collect::<Vec<_>>()))
}

fn check(e: &Value, o: &Observed, ctx: &cameleer_core::context::CamelContext) -> Result<(), String> {
    let expected_error = e.get("error").map(|v| v.to_string());
    match (&expected_error, &o.error) {
        (Some(want), Some(got)) if !got.contains(want.as_str()) => return Err(format!("expected an error containing {:?}, got {:?}", want, got)),
        (Some(want), None) => return Err(format!("expected an error containing {:?}, got none", want)),
        (None, Some(got)) => return Err(format!("failed: {}", got)),
        _ => {}
    }
    let body = o.body.clone().unwrap_or(Value::Null);
    let body_text = match &body {
        Value::Object(_) | Value::Array(_) => body.to_json(),
        v => v.to_string(),
    };
    if let Some(want) = e.get("body") {
        let want_text = match want {
            Value::Object(_) | Value::Array(_) => want.to_json(),
            v => v.to_string(),
        };
        if want_text != body_text {
            return Err(format!("expected the body {:?}, got {:?}", want_text, body_text));
        }
    }
    if let Some(want) = e.get("bodyContains").map(|v| v.to_string()) {
        if !body_text.contains(&want) {
            return Err(format!("expected the body to contain {:?}, got {:?}", want, body_text));
        }
    }
    if let Some(Value::Object(h)) = e.get("headers") {
        for (k, want) in h.iter() {
            let got = o.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.to_string());
            if got.as_deref() != Some(want.to_string().as_str()) {
                return Err(format!("expected header {} = {:?}, got {:?}", k, want.to_string(), got));
            }
        }
    }
    for (key, got) in [("status", o.status.clone()), ("isError", o.is_error.map(|b| b.to_string())), ("duplicate", o.duplicate.map(|b| b.to_string()))] {
        if let Some(want) = e.get(key).map(|v| v.to_string()) {
            if got.as_deref() != Some(want.as_str()) {
                return Err(format!("expected {} {:?}, got {:?}", key, want, got));
            }
        }
    }
    if let Some(Value::Object(mocks)) = e.get("mocks") {
        for (uri, want) in mocks.iter() {
            let got = ctx.mock_endpoint(uri).map(|m| m.received_counter() as i64).map_err(|err| format!("{}: {}", uri, err))?;
            if Some(got) != want.as_i64() {
                return Err(format!("expected {} to receive {} messages, got {}", uri, want.to_string(), got));
            }
        }
    }
    Ok(())
}
