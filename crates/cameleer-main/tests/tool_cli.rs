//! The AI tools' kill switch and approvals from the command line (`cameleer tool ...` against a
//! running `cameleer run`) and from the dev console (`/q/dev/tools`).
#![cfg(feature = "ai")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_http::http::HttpRequest;
use cameleer_main::console::{handle_request, ConsoleConfig};

const ROUTES: &str = r#"
- route:
    id: lookup-tool
    from:
      uri: ai-tool:lookup
      parameters:
        description: Look up an order
        tags: orders
        readOnlyHint: true
      steps:
        - setBody:
            constant: found
- route:
    id: wipe-tool
    from:
      uri: ai-tool:wipe
      parameters:
        description: Wipe an order
        parameter.orderId: string
        destructiveHint: true
        version: "2"
      steps:
        - setBody:
            simple: "wiped ${header.orderId}"
- route:
    id: park
    from:
      uri: timer:park?repeatCount=1&delay=100
      steps:
        - setBody:
            constant: '{"orderId":"A1"}'
        - setHeader:
            name: CamelAiToolPrincipal
            constant: bot
        - to: ai-tool:wipe
"#;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cameleer-tool-cli-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Kills the integration if the test fails before stopping it.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn cameleer(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cameleer")).args(args).env("CAMEL_HOME", home).env_remove("CAMEL_LOCAL_STATUS").stdin(Stdio::null()).output().unwrap()
}

/// stdout of a command that must succeed.
fn ok(home: &Path, args: &[&str]) -> String {
    let o = cameleer(home, args);
    let (out, err) = (String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned());
    assert!(o.status.success(), "cameleer {:?} failed: {}{}", args, out, err);
    out
}

/// stderr of a command that must fail.
fn fails(home: &Path, args: &[&str]) -> String {
    let o = cameleer(home, args);
    assert!(!o.status.success(), "cameleer {:?} should fail: {}", args, String::from_utf8_lossy(&o.stdout));
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn status_line<'a>(list: &'a str, tool: &str) -> &'a str {
    list.lines().find(|l| l.split_whitespace().next() == Some(tool)).unwrap_or_else(|| panic!("no {} in\n{}", tool, list))
}

#[test]
fn kill_switch_and_approvals_from_the_command_line() {
    let home = temp_dir("home");
    let routes = home.join("tools.camel.yaml");
    std::fs::write(&routes, ROUTES).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_cameleer"))
        .args(["run", routes.to_string_lossy().as_ref(), "--name", "toolapp", "--max-seconds", "120"])
        .env("CAMEL_HOME", &home)
        .env_remove("CAMEL_LOCAL_STATUS")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut running = Running(child);

    // the integration is up when it answers
    let list = wait_for("the tool list", || {
        let o = cameleer(&home, &["tool", "list", "toolapp"]);
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    });
    assert!(list.starts_with("NAME"), "{}", list);
    assert!(status_line(&list, "lookup").contains("orders") && status_line(&list, "lookup").ends_with("enabled"), "{}", list);
    let wipe = status_line(&list, "wipe");
    assert!(wipe.contains("yes") && wipe.contains(" 2 "), "{}", wipe);

    // kill switch
    assert!(ok(&home, &["tool", "disable", "lookup", "toolapp"]).contains("tool lookup disabled"));
    let list = ok(&home, &["tool", "list"]);
    assert!(status_line(&list, "lookup").ends_with("disabled"), "{}", list);
    let json = cameleer_core::json::parse(&ok(&home, &["tool", "list", "--json"])).unwrap();
    let lookup = json.get("tools").unwrap().as_array().unwrap().iter().find(|t| t.get("name").map(|n| n.to_string()).as_deref() == Some("lookup")).cloned().unwrap();
    assert_eq!(lookup.get("disabled"), Some(&Value::Bool(true)));
    ok(&home, &["tool", "enable", "lookup"]);
    assert!(status_line(&ok(&home, &["tool", "list"]), "lookup").ends_with("enabled"));

    // clear errors
    assert!(fails(&home, &["tool", "disable", "lokup"]).contains("No tool named lokup (tools: lookup, wipe)"));
    assert!(fails(&home, &["tool", "list", "no-such-app"]).contains("No running integration matching no-such-app"));
    assert!(fails(&home, &["tool", "disable"]).contains("requires a tool name"));
    assert!(fails(&home, &["tool", "frobnicate"]).contains("Unknown tool command frobnicate"));
    assert!(fails(&home, &["tool", "approve", "no-such-handle"]).contains("No call is waiting for approval with handle no-such-handle"));

    // approvals: the timer route's call to the destructive tool is parked
    let approvals = wait_for("the parked call", || {
        let out = ok(&home, &["tool", "approvals", "--json"]);
        let v = cameleer_core::json::parse(&out).unwrap();
        (!v.get("approvals").unwrap().as_array().unwrap().is_empty()).then_some(v)
    });
    let first = &approvals.get("approvals").unwrap().as_array().unwrap()[0];
    let handle = first.get("handle").unwrap().to_string();
    assert_eq!(first.get("tool").map(|t| t.to_string()).as_deref(), Some("wipe"));
    assert_eq!(first.get("principal").map(|t| t.to_string()).as_deref(), Some("bot"));
    let table = ok(&home, &["tool", "approvals"]);
    assert!(table.starts_with("HANDLE") && table.contains(&handle) && table.contains("bot"), "{}", table);
    assert!(ok(&home, &["tool", "list"]).contains("Pending approvals: 1"));
    let approved = ok(&home, &["tool", "approve", &handle, "--by", "ann", "toolapp"]);
    assert!(approved.contains("wiped A1"), "{}", approved);
    assert!(ok(&home, &["tool", "approvals"]).contains("No calls are waiting for approval"));
    assert!(fails(&home, &["tool", "deny", &handle, "--reason", "late"]).contains("No call is waiting"));

    // stop
    assert!(ok(&home, &["stop", "toolapp"]).contains("Stopping toolapp"));
    let status = wait_for("the integration to exit", || running.0.try_wait().unwrap());
    assert!(status.success(), "{:?}", status);
    let _ = std::fs::remove_dir_all(&home);
}

fn console_call(ctx: &CamelContext, method: &str, path: &str) -> (u16, Value) {
    let cfg = ConsoleConfig { enabled: true, dev_console: true, ..Default::default() };
    let r = handle_request(ctx, &cfg, &HttpRequest::new(method, path));
    (r.status, cameleer_core::json::parse(&String::from_utf8_lossy(&r.body)).unwrap())
}

#[test]
fn dev_console_lists_disables_and_enables_tools() {
    let ctx = CamelContext::new();
    cameleer_ai::register(&ctx);
    ctx.add_routes(|b| {
        b.from("ai-tool:lookup?readOnlyHint=true&tags=orders").set_body(constant("found"));
        b.from("ai-tool:wipe?destructiveHint=true").set_body(constant("wiped"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reg = cameleer_ai::AiRegistry::of(&ctx).unwrap();
    let changes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c2 = changes.clone();
    reg.on_change(move || {
        c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });

    let (status, consoles) = console_call(&ctx, "GET", "/q/dev");
    assert_eq!(status, 200);
    assert!(consoles.to_json().contains(r#""path":"/q/dev/tools""#), "{}", consoles.to_json());

    let (status, list) = console_call(&ctx, "GET", "/q/dev/tools");
    assert_eq!(status, 200);
    let j = list.to_json();
    assert!(j.contains(r#""name":"lookup""#) && j.contains(r#""readOnly":true"#) && j.contains(r#""pendingApprovals":0"#), "{}", j);

    let (status, r) = console_call(&ctx, "POST", "/q/dev/tools/lookup/disable");
    assert_eq!(status, 200, "{}", r.to_json());
    assert_eq!(r.get("disabled"), Some(&Value::Bool(true)));
    assert!(reg.is_disabled("lookup") && reg.tools(&[]).iter().all(|t| t.name != "lookup"));
    assert!(reg.call(cameleer_ai::ToolCall::new("lookup", Value::Null)).text.contains("disabled"));
    assert!(console_call(&ctx, "GET", "/q/dev/tools").1.to_json().contains(r#""disabled":true"#));
    assert_eq!(changes.load(std::sync::atomic::Ordering::SeqCst), 1, "listeners (MCP list_changed) are told");

    let (status, _) = console_call(&ctx, "POST", "/q/dev/tools/lookup/enable");
    assert_eq!(status, 200);
    assert_eq!(reg.call(cameleer_ai::ToolCall::new("lookup", Value::Null)).text, "found");

    let (status, r) = console_call(&ctx, "POST", "/q/dev/tools/nope/disable");
    assert_eq!(status, 404, "{}", r.to_json());
    assert!(r.to_json().contains("No tool named nope"));
    assert_eq!(console_call(&ctx, "POST", "/q/dev/tools/lookup/explode").0, 400);
    ctx.stop().unwrap();
}
