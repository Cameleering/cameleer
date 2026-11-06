//! The action layer's guarantees, each proven against real routes: a retried call runs once,
//! a call that needs approval runs nothing until approved, a timeout cancels the route, a
//! failed conversation compensates, a changed tool is refused, the audit chain detects edits.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_ai::registry::ToolAuthorizationPolicy;
use cameleer_ai::{headers, AiRegistry, ToolCall, ToolDefinition, ToolStatus};
use cameleer_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_ai::register(&ctx);
    ctx
}

fn json(s: &str) -> Value {
    cameleer_core::json::parse(s).unwrap()
}

fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

#[test]
fn a_route_becomes_a_tool_with_camel_semantics() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("ai-tool:weather?description=Current weather for a city&tags=weather,public&parameter.city=string&parameter.city.description=The city&parameter.city.required=true&parameter.unit=string&parameter.unit.enum=celsius,fahrenheit&readOnlyHint=true")
            .set_body(simple("${header.city}: 21 degrees ${header.unit} (call ${header.CamelAiToolCallId})"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();

    let tools = reg.tools(&[]);
    assert_eq!(tools.len(), 1);
    let t = &tools[0];
    assert_eq!(t.name, "weather");
    assert_eq!(t.description, "Current weather for a city");
    assert_eq!(
        t.to_value().to_json(),
        r#"{"name":"weather","description":"Current weather for a city","inputSchema":{"type":"object","properties":{"city":{"type":"string","description":"The city"},"unit":{"type":"string","enum":["celsius","fahrenheit"]}},"required":["city"]},"annotations":{"readOnlyHint":true}}"#
    );
    assert_eq!(reg.tools(&["weather"]).len(), 1);
    assert!(reg.tools(&["internal"]).is_empty());

    // camel* and undeclared arguments are dropped before the route sees them
    let r = reg.call(
        ToolCall::new("weather", json(r#"{"city":"Oslo","unit":"celsius","CamelHttpUri":"http://evil","extra":1}"#)).with_call_id("c-1"),
    );
    assert_eq!(r.status, ToolStatus::Completed);
    assert_eq!(r.text, "Oslo: 21 degrees celsius (call c-1)");

    // full schema validation, with an error the model can act on
    let bad = reg.call(ToolCall::new("weather", json(r#"{"unit":"kelvin"}"#)));
    assert!(bad.is_error);
    assert!(bad.text.contains("$.city: is missing but it is required"), "{}", bad.text);
    assert!(bad.text.contains("$.unit: does not have a value in the enumeration [celsius, fahrenheit]"), "{}", bad.text);
    assert!(bad.text.contains(r#"Example of valid arguments: {"city":"text"}"#), "{}", bad.text);

    assert!(reg.call(ToolCall::new("nope", Value::Null)).text.contains("Unknown tool: nope"));

    // stopping the route withdraws the tool
    ctx.stop().unwrap();
    assert!(reg.tools(&[]).is_empty());
}

#[test]
fn results_are_structured_and_empty_is_no_result() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("ai-tool:order?parameter.id=integer&readOnlyHint=true&outputParameter.id=integer&outputParameter.status=string")
            .process(|ex: &mut Exchange| {
                let id = ex.header("id").and_then(|v| v.as_i64()).unwrap_or(0);
                if id == 0 {
                    ex.set_body(json(r#"{"id":"zero"}"#));
                } else {
                    ex.set_body(json(&format!(r#"{{"id":{},"status":"shipped"}}"#, id)));
                }
                Ok(())
            });
        b.from("ai-tool:nothing?readOnlyHint=true").set_body(constant(Value::Null));
        b.from("ai-tool:echo?argumentsAsBody=true&parameter.a=string&readOnlyHint=true").to("mock:echo");
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    let r = reg.call(ToolCall::new("order", json(r#"{"id":7}"#)));
    assert_eq!(r.structured.as_ref().unwrap().to_json(), r#"{"id":7,"status":"shipped"}"#);
    assert_eq!(r.text, r#"{"id":7,"status":"shipped"}"#);
    // the output schema is enforced
    let wrong = reg.call(ToolCall::new("order", json(r#"{"id":0}"#)));
    assert!(wrong.is_error && wrong.text.contains("does not match its outputSchema"), "{}", wrong.text);
    assert_eq!(reg.call(ToolCall::new("nothing", Value::Null)).text, "No result");
    // argumentsAsBody: the arguments arrive as a JSON string body
    assert_eq!(reg.call(ToolCall::new("echo", json(r#"{"a":"x"}"#))).text, r#"{"a":"x"}"#);
    ctx.stop().unwrap();
}

#[test]
fn a_call_runs_on_a_copy_that_keeps_only_properties_and_variables() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("direct:agent")
            .set_header("secret", constant("caller-header"))
            .set_variable("tenant", constant("acme"))
            .set_body(constant(r#"{"q":"x"}"#))
            .to("ai-tool:lookup");
        b.from("ai-tool:lookup?parameter.q=string&readOnlyHint=true")
            .set_body(simple("${variable.tenant}|${header.secret}|${header.q}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let out = ctx.create_producer_template().request_body("direct:agent", Value::Null).unwrap();
    assert_eq!(out.to_string(), "acme||x");
    ctx.stop().unwrap();
}

#[test]
fn a_retried_call_runs_once() {
    let ctx = context();
    let runs = counter();
    let r2 = runs.clone();
    ctx.add_routes(move |b| {
        let r2 = r2.clone();
        b.from("ai-tool:pay?parameter.amount=integer&parameter.to=string").process(move |ex: &mut Exchange| {
            let n = r2.fetch_add(1, Ordering::SeqCst) + 1;
            std::thread::sleep(Duration::from_millis(50));
            ex.set_body(format!("payment {} sent", n));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    let call = ToolCall::new("pay", json(r#"{"amount":10,"to":"bob"}"#)).with_call_id("call-9");
    let first = reg.call(call.clone());
    assert_eq!(first.text, "payment 1 sent");
    assert!(!first.duplicate);
    let again = reg.call(call.clone());
    assert_eq!(again.text, "payment 1 sent");
    assert!(again.duplicate);
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    // concurrent retries of a new call: still one run
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let reg = reg.clone();
            std::thread::spawn(move || reg.call(ToolCall::new("pay", json(r#"{"amount":5,"to":"ann"}"#)).with_call_id("call-10")))
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    assert_eq!(results.iter().filter(|r| r.duplicate).count(), 3);
    assert!(results.iter().all(|r| r.text == "payment 2 sent"));

    // without a call id the arguments are the key; a read-only tool is not deduplicated
    reg.call(ToolCall::new("pay", json(r#"{"amount":1,"to":"cy"}"#)));
    reg.call(ToolCall::new("pay", json(r#"{"amount":1,"to":"cy"}"#)));
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    ctx.stop().unwrap();
}

#[test]
fn a_declared_idempotency_key_wins_and_failures_can_be_retried() {
    let ctx = context();
    let runs = counter();
    let r2 = runs.clone();
    ctx.add_routes(move |b| {
        let r2 = r2.clone();
        b.from("ai-tool:ship?parameter.order=string&parameter.idempotency_key=string").process(move |ex: &mut Exchange| {
            let n = r2.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 1 {
                return Err(CamelError::illegal_state("warehouse offline"));
            }
            ex.set_body("shipped");
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    let args = json(r#"{"order":"o-1","idempotency_key":"k-1"}"#);
    let failed = reg.call(ToolCall::new("ship", args.clone()).with_call_id("a"));
    assert!(failed.is_error && failed.text.contains("warehouse offline"));
    // a failure did not happen: the retry runs, even with another call id
    assert_eq!(reg.call(ToolCall::new("ship", args.clone()).with_call_id("b")).text, "shipped");
    let dup = reg.call(ToolCall::new("ship", args).with_call_id("c"));
    assert!(dup.duplicate);
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    ctx.stop().unwrap();
}

#[test]
fn destructive_tools_wait_for_approval_and_a_rejection_runs_nothing() {
    let ctx = context();
    let runs = counter();
    let r2 = runs.clone();
    ctx.add_routes(move |b| {
        let r2 = r2.clone();
        b.from("ai-tool:delete_account?parameter.user=string&destructiveHint=true&approvalEndpoint=mock:approvals").process(move |ex: &mut Exchange| {
            r2.fetch_add(1, Ordering::SeqCst);
            ex.set_body(simple_text(ex, "deleted"));
            Ok(())
        });
        b.from("ai-tool:refund?parameter.amount=integer&approval=${header.amount} > 500").set_body(simple("refunded ${header.amount}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    let parked = reg.call(ToolCall::new("delete_account", json(r#"{"user":"bob"}"#)).with_principal("agent-1"));
    let ToolStatus::ApprovalRequired { handle } = parked.status.clone() else { panic!("{:?}", parked) };
    assert!(parked.text.contains("needs approval"));
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    // the approver was notified with the arguments and the handle
    let mock = ctx.mock_endpoint("mock:approvals").unwrap();
    assert_eq!(mock.received_counter(), 1);
    let note = &mock.received_exchanges()[0];
    assert_eq!(note.header(headers::APPROVAL_HANDLE).unwrap().to_string(), handle);
    assert_eq!(note.header("user").unwrap().to_string(), "bob");
    assert_eq!(reg.pending_approvals().len(), 1);

    let rejected = reg.deny(&handle, "alice", "not today");
    assert_eq!(rejected.status, ToolStatus::Denied);
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert!(reg.approve(&handle, "alice").text.contains("No call is waiting"));

    let parked = reg.call(ToolCall::new("delete_account", json(r#"{"user":"cy"}"#)));
    let ToolStatus::ApprovalRequired { handle } = parked.status else { panic!() };
    let done = reg.approve(&handle, "alice");
    assert_eq!(done.text, "deleted cy");
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    // a predicate decides per call
    assert_eq!(reg.call(ToolCall::new("refund", json(r#"{"amount":20}"#))).text, "refunded 20");
    let big = reg.call(ToolCall::new("refund", json(r#"{"amount":900}"#)));
    assert!(matches!(big.status, ToolStatus::ApprovalRequired { .. }));
    assert!(big.text.contains("approval rule ${header.amount} > 500"), "{}", big.text);

    let audit = reg.audit().records();
    let decisions: Vec<String> = audit.iter().map(|r| r.as_object().unwrap().get("decision").unwrap().to_string()).collect();
    assert!(decisions.contains(&"approval-required".to_string()));
    assert!(decisions.contains(&"rejected".to_string()));
    assert!(decisions.contains(&"approved".to_string()));
    ctx.stop().unwrap();
}

fn simple_text(ex: &Exchange, prefix: &str) -> String {
    format!("{} {}", prefix, ex.header("user").map(|v| v.to_string()).unwrap_or_default())
}

#[test]
fn a_timeout_cancels_the_route() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("ai-tool:slow?timeout=200ms&readOnlyHint=true")
            .to("mock:started")
            .delay(constant(600))
            .to("mock:after-delay")
            .set_body(constant("too late"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    let t0 = Instant::now();
    let r = reg.call(ToolCall::new("slow", Value::Null));
    assert!(t0.elapsed() < Duration::from_millis(550), "the caller waited {:?}", t0.elapsed());
    assert!(r.is_error && r.text.contains("did not answer within 200 ms and was cancelled"), "{}", r.text);
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(ctx.mock_endpoint("mock:started").unwrap().received_counter(), 1);
    // the step after the delay never ran: the route stopped at its next step
    assert_eq!(ctx.mock_endpoint("mock:after-delay").unwrap().received_counter(), 0);
    ctx.stop().unwrap();
}

#[test]
fn a_failed_conversation_compensates_newest_first() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("ai-tool:book_flight?parameter.flight=string&compensation=direct:cancel-flight").set_body(simple("booked ${header.flight}"));
        b.from("ai-tool:book_hotel?parameter.hotel=string&compensation=direct:cancel-hotel").set_body(simple("booked ${header.hotel}"));
        b.from("direct:cancel-flight").to("mock:undo");
        b.from("direct:cancel-hotel").to("mock:undo");
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    reg.call(ToolCall::new("book_flight", json(r#"{"flight":"LH1"}"#)).with_conversation("trip-1"));
    reg.call(ToolCall::new("book_hotel", json(r#"{"hotel":"Ritz"}"#)).with_conversation("trip-1"));
    assert_eq!(reg.compensations("trip-1"), 2);
    let undone = reg.compensate("trip-1");
    assert_eq!(undone.iter().map(|(t, r)| (t.as_str(), r.is_ok())).collect::<Vec<_>>(), [("book_hotel", true), ("book_flight", true)]);
    let mock = ctx.mock_endpoint("mock:undo").unwrap();
    let got = mock.received_exchanges();
    assert_eq!(got[0].header("hotel").unwrap().to_string(), "Ritz");
    assert_eq!(got[0].body().to_string(), "booked Ritz");
    assert_eq!(got[0].header(headers::COMPENSATING).unwrap(), &Value::Bool(true));
    assert_eq!(got[1].header("flight").unwrap().to_string(), "LH1");
    assert_eq!(reg.compensations("trip-1"), 0);

    // a completed conversation drops its compensations
    reg.call(ToolCall::new("book_flight", json(r#"{"flight":"LH2"}"#)).with_conversation("trip-2"));
    reg.complete("trip-2");
    assert!(reg.compensate("trip-2").is_empty());
    ctx.stop().unwrap();
}

#[test]
fn rate_limits_are_per_principal() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("ai-tool:search?parameter.q=string&readOnlyHint=true&rateLimit=2&rateLimitPeriod=1m").set_body(constant("ok"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    let call = |p: &str| reg.call(ToolCall::new("search", json(r#"{"q":"x"}"#)).with_principal(p));
    assert_eq!(call("ann").text, "ok");
    assert_eq!(call("ann").text, "ok");
    let limited = call("ann");
    assert!(limited.is_error && limited.text.contains("Rate limit for tool search reached (2 calls per 1 min)"), "{}", limited.text);
    assert_eq!(call("bob").text, "ok");
    ctx.stop().unwrap();
}

#[test]
fn a_circuit_breaker_opens_after_failures_and_recovers() {
    let ctx = context();
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let f2 = fail.clone();
    let runs = counter();
    let r2 = runs.clone();
    ctx.add_routes(move |b| {
        let f2 = f2.clone();
        let r2 = r2.clone();
        b.from("ai-tool:crm?readOnlyHint=true&circuitBreakerThreshold=2&circuitBreakerDelay=150ms&circuitBreakerFallback=The CRM is down, tell the user to try later")
            .process(move |ex: &mut Exchange| {
                r2.fetch_add(1, Ordering::SeqCst);
                if f2.load(Ordering::SeqCst) {
                    return Err(CamelError::illegal_state("503"));
                }
                ex.set_body("up");
                Ok(())
            });
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    reg.call(ToolCall::new("crm", Value::Null));
    reg.call(ToolCall::new("crm", Value::Null));
    let open = reg.call(ToolCall::new("crm", Value::Null));
    assert_eq!(open.text, "The CRM is down, tell the user to try later");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    fail.store(false, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(reg.call(ToolCall::new("crm", Value::Null)).text, "up");
    assert_eq!(reg.call(ToolCall::new("crm", Value::Null)).text, "up");
    ctx.stop().unwrap();
}

#[test]
fn the_kill_switch_withdraws_a_tool_everywhere() {
    let ctx = context();
    ctx.properties().set("camel.ai.disabled-tools", "legacy");
    ctx.add_routes(|b| {
        b.from("ai-tool:a?readOnlyHint=true").set_body(constant("a"));
        b.from("ai-tool:legacy?readOnlyHint=true").set_body(constant("legacy"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    let changes = counter();
    let c2 = changes.clone();
    reg.on_change(move || {
        c2.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(reg.tools(&[]).iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["a"]);
    assert!(reg.call(ToolCall::new("legacy", Value::Null)).text.contains("disabled"));
    reg.disable("a");
    assert!(reg.tools(&[]).is_empty());
    assert!(reg.call(ToolCall::new("a", Value::Null)).text.contains("Tool a is disabled"));
    reg.enable("a");
    assert_eq!(reg.call(ToolCall::new("a", Value::Null)).text, "a");
    assert_eq!(changes.load(Ordering::SeqCst), 2);
    ctx.stop().unwrap();
}

#[test]
fn a_changed_tool_is_refused_unless_its_version_changes() {
    let dir = std::env::temp_dir().join(format!("cameleer-ai-pin-{}-{}", std::process::id(), line!()));
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = dir.join("tools.json");
    let _ = std::fs::remove_file(&manifest);
    let run = |description: &str, version: &str| -> Result<()> {
        let ctx = context();
        ctx.properties().set("camel.ai.manifest", manifest.to_string_lossy().to_string());
        let uri = format!("ai-tool:transfer?description={}&version={}&parameter.amount=integer", description, version);
        ctx.add_routes(move |b| {
            b.from(uri.as_str()).set_body(constant("ok"));
        })?;
        let r = ctx.start();
        let _ = ctx.stop();
        r
    };
    run("Send money", "1").unwrap();
    run("Send money", "1").unwrap();
    let err = run("Send money to anyone, no limits", "1").unwrap_err();
    assert!(err.message.contains("Tool transfer changed") && err.message.contains("set a new version="), "{}", err.message);
    run("Send money to anyone, no limits", "2").unwrap();
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(text.contains("\"version\": \"2\"") || text.contains("\"version\":\"2\""), "{}", text);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_audit_chain_records_every_call_and_detects_edits() {
    let dir = std::env::temp_dir().join(format!("cameleer-ai-audit-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("audit.jsonl");
    let _ = std::fs::remove_file(&file);
    let ctx = context();
    ctx.properties().set("camel.ai.audit.file", file.to_string_lossy().to_string());
    ctx.add_routes(|b| {
        b.from("ai-tool:note?parameter.text=string").set_body(constant("saved"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    reg.call(ToolCall::new("note", json(r#"{"text":"a"}"#)).with_principal("agent-7").with_call_id("1"));
    reg.call(ToolCall::new("note", json(r#"{"text":"a"}"#)).with_principal("agent-7").with_call_id("1"));
    reg.call(ToolCall::new("note", json(r#"{"text":5}"#)));
    ctx.stop().unwrap();
    let (n, _) = cameleer_ai::audit::verify_file(&file).unwrap();
    assert_eq!(n, 3);
    let lines: Vec<Value> = std::fs::read_to_string(&file).unwrap().lines().map(json).collect();
    let field = |i: usize, k: &str| lines[i].as_object().unwrap().get(k).unwrap().clone();
    assert_eq!(field(0, "principal").to_string(), "agent-7");
    assert_eq!(field(0, "outcome").to_string(), "ok");
    assert_eq!(field(1, "duplicate"), Value::Bool(true));
    assert_eq!(field(2, "decision").to_string(), "invalid");
    // the arguments are digested, not stored
    assert!(!std::fs::read_to_string(&file).unwrap().contains("\"text\""));
    let edited = std::fs::read_to_string(&file).unwrap().replace("agent-7", "agent-8");
    std::fs::write(&file, edited).unwrap();
    assert!(cameleer_ai::audit::verify_file(&file).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

struct OnlyAdmins;
impl ToolAuthorizationPolicy for OnlyAdmins {
    fn authorize(&self, _tool: &ToolDefinition, call: &ToolCall) -> std::result::Result<(), String> {
        if call.principal.as_deref() == Some("admin") {
            Ok(())
        } else {
            Err("admin only".into())
        }
    }
}

#[test]
fn an_authorization_policy_bean_decides() {
    let ctx = context();
    let policy: Arc<dyn ToolAuthorizationPolicy> = Arc::new(OnlyAdmins);
    ctx.registry().bind("admins", policy);
    ctx.add_routes(|b| {
        b.from("ai-tool:reset?readOnlyHint=true&authorizationPolicy=#admins").set_body(constant("reset"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    assert_eq!(reg.call(ToolCall::new("reset", Value::Null).with_principal("admin")).text, "reset");
    let no = reg.call(ToolCall::new("reset", Value::Null).with_principal("guest"));
    assert!(no.is_error && no.text.contains("admin only"));
    ctx.stop().unwrap();
}

#[test]
fn the_producer_calls_tools_and_reports_status_headers() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("ai-tool:greet?parameter.name=string&parameter.name.required=true&readOnlyHint=true&returnDirect=true").set_body(simple("Hello ${header.name}"));
        b.from("direct:call").to("ai-tool:greet").to("mock:out");
        b.from("direct:strict").to("ai-tool:greet?throwOnError=true");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let _ = t.send_body("direct:call", Value::from(r#"{"name":"Ann"}"#));
    let mock = ctx.mock_endpoint("mock:out").unwrap();
    let ex = &mock.received_exchanges()[0];
    assert_eq!(ex.body().to_string(), "Hello Ann");
    assert_eq!(ex.header(headers::STATUS).unwrap().to_string(), "completed");
    assert_eq!(ex.header(headers::RETURN_DIRECT).unwrap(), &Value::Bool(true));
    let failed = t.send("direct:strict", Exchange::with_body(Some(ctx.clone()), json("{}")));
    let err = failed.exception.unwrap();
    assert_eq!(err.kind, cameleer_ai::component::TOOL_CALL_EXCEPTION);
    assert!(err.message.contains("is missing but it is required"));
    ctx.stop().unwrap();
}

#[test]
fn resources_and_prompts() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("ai-resource:handbook?uri=docs://handbook&description=The staff handbook&mimeType=text/markdown").set_body(constant("# Handbook"));
        b.from("ai-resource:order?uri=orders://{id}&mimeType=application/json").set_body(simple(r#"{"id":"${header.id}"}"#));
        b.from("ai-prompt:review?description=Review code&argument.code.description=The code&argument.code.required=true&argument.style=Style guide")
            .set_body(simple("Review this code in ${header.style} style: ${header.code}"));
        b.from("ai-prompt:chat").process(|ex: &mut Exchange| {
            ex.set_body(json(r#"[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"}]"#));
            Ok(())
        });
        b.from("direct:read").to("ai-resource:handbook");
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    let listed: Vec<String> = reg.resources(&[]).iter().map(|r| r.to_json()).collect();
    assert!(listed.contains(&r#"{"uri":"docs://handbook","name":"handbook","description":"The staff handbook","mimeType":"text/markdown"}"#.to_string()), "{:?}", listed);
    assert!(listed.iter().any(|r| r.contains(r#""uriTemplate":"orders://{id}""#)));
    let c = reg.read_resource("docs://handbook").unwrap();
    assert_eq!((c.text.as_deref(), c.mime_type.as_str()), (Some("# Handbook"), "text/markdown"));
    assert_eq!(reg.read_resource("orders://42").unwrap().text.unwrap(), r#"{"id":"42"}"#);
    assert_eq!(reg.read_resource("nope://x").unwrap_err().kind, cameleer_ai::registry::RESOURCE_NOT_FOUND);
    assert_eq!(ctx.create_producer_template().request_body("direct:read", Value::Null).unwrap().to_string(), "# Handbook");

    let prompts: Vec<String> = reg.prompts(&[]).iter().map(|p| p.to_json()).collect();
    assert!(prompts.contains(
        &r#"{"name":"review","description":"Review code","arguments":[{"name":"code","description":"The code","required":true},{"name":"style","description":"Style guide","required":false}]}"#.to_string()
    ), "{:?}", prompts);
    let mut args = BTreeMap::new();
    assert!(reg.get_prompt("review", &args).is_err());
    args.insert("code".to_string(), "fn main(){}".to_string());
    args.insert("style".to_string(), "Rust".to_string());
    let p = reg.get_prompt("review", &args).unwrap();
    assert_eq!(p.messages[0].text, "Review this code in Rust style: fn main(){}");
    assert_eq!(p.messages[0].role, "user");
    let chat = reg.get_prompt("chat", &BTreeMap::new()).unwrap();
    assert_eq!(chat.messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(), ["user", "assistant"]);
    ctx.stop().unwrap();
}

#[test]
fn bad_options_fail_when_the_route_is_added() {
    let ctx = context();
    for uri in [
        "ai-tool:x?parameter.a=uuid",
        "ai-tool:x?argSchema=https://example.com/schema.json",
        "ai-tool:x?argSchema={\"type\":\"string\"}",
        "ai-tool:x?idempotency=sometimes",
        "ai-tool:x?authorizationPolicy=#missing",
        "ai-tool:x?bogus=1",
        "ai-tool:",
    ] {
        let u = uri.to_string();
        let r = ctx.add_routes(move |b| {
            b.from(u.as_str()).to("mock:x");
        });
        assert!(r.is_err() || ctx.start().is_err(), "{} should fail", uri);
        let _ = ctx.stop();
    }
}

#[test]
fn a_full_argschema_is_validated_with_2020_12_keywords() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from(r##"ai-tool:create_user?readOnlyHint=true&argSchema={"type":"object","$defs":{"email":{"type":"string","format":"email"}},"properties":{"email":{"$ref":"#/$defs/email"},"roles":{"type":"array","prefixItems":[{"const":"user"}]}},"required":["email"],"unevaluatedProperties":false}"##)
            .set_body(simple("created ${header.email}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    assert_eq!(reg.call(ToolCall::new("create_user", json(r#"{"email":"a@b.co","roles":["user"]}"#))).text, "created a@b.co");
    let bad = reg.call(ToolCall::new("create_user", json(r#"{"email":"nope","roles":["admin"]}"#)));
    assert!(bad.text.contains("$.email: does not match the email pattern"), "{}", bad.text);
    assert!(bad.text.contains("$.roles[0]: must be a constant value user"), "{}", bad.text);
    ctx.stop().unwrap();
}

#[test]
fn guardrails_redact_authorize_and_cache() {
    let ctx = context();
    let runs = counter();
    let r2 = runs.clone();
    ctx.add_routes(move |b| {
        let r2 = r2.clone();
        b.from("ai-tool:customer?parameter.id=string&readOnlyHint=true&redact=email,secret&cacheTtl=1m").process(move |ex: &mut Exchange| {
            r2.fetch_add(1, Ordering::SeqCst);
            ex.set_body(json(r#"{"name":"Ann","email":"ann@example.com","note":"key sk-abcdefghijklmnopqrstuv"}"#));
            Ok(())
        });
        b.from("ai-tool:refund_small?parameter.amount=integer&readOnlyHint=true&authorize=${header.amount} <= 100 %26%26 ${header.CamelAiToolPrincipal} == 'support'")
            .set_body(simple("refunded ${header.amount}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = AiRegistry::of(&ctx).unwrap();
    let r = reg.call(ToolCall::new("customer", json(r#"{"id":"c1"}"#)));
    assert_eq!(r.structured.unwrap().to_json(), r#"{"name":"Ann","email":"[REDACTED:email]","note":"key [REDACTED:secret]"}"#);
    assert!(!r.text.contains("ann@example.com"));
    // the second call with the same arguments comes from the cache
    reg.call(ToolCall::new("customer", json(r#"{"id":"c1"}"#)));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    reg.call(ToolCall::new("customer", json(r#"{"id":"c2"}"#)));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    // authorize looks at the arguments and the caller
    assert_eq!(reg.call(ToolCall::new("refund_small", json(r#"{"amount":50}"#)).with_principal("support")).text, "refunded 50");
    assert!(reg.call(ToolCall::new("refund_small", json(r#"{"amount":500}"#)).with_principal("support")).text.contains("Not authorized"));
    assert!(reg.call(ToolCall::new("refund_small", json(r#"{"amount":50}"#)).with_principal("guest")).text.contains("Not authorized"));
    // a tool's description defaults to its name, as in Camel
    assert_eq!(reg.tool("customer").unwrap().description, "customer");
    ctx.stop().unwrap();
}
