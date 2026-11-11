//! Attestation end to end (`camel.attestation.*`): Main against a fake configfs-tsm directory,
//! reports over `GET /q/attestation` and through the MCP handshake and tool, the binding a
//! client checks, changes recorded after start-up, and failing closed.
#![cfg(all(feature = "attest", feature = "ai"))]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cameleer_attest::{b64, check_binding, unb64};
use cameleer_core::value::Value;
use cameleer_http::client::HttpClient;

const ROUTES: &str = r#"
- route:
    id: lookup
    from:
      uri: ai-tool:lookup
      parameters:
        description: Look something up
        readOnlyHint: true
      steps:
        - setBody:
            constant: found
"#;

fn temp_dir(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("cameleer-attest-main-{}-{}-{}", name, std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// configfs-tsm emulation: a quote that embeds the REPORTDATA for every new report entry.
struct FakeTsm {
    root: PathBuf,
    stop: Arc<AtomicBool>,
}

impl FakeTsm {
    fn start() -> FakeTsm {
        let root = temp_dir("tsm");
        let stop = Arc::new(AtomicBool::new(false));
        let (r, s) = (root.clone(), stop.clone());
        std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                if let Ok(dirs) = std::fs::read_dir(&r) {
                    for d in dirs.flatten() {
                        let p = d.path();
                        if let Ok(inblob) = std::fs::read(p.join("inblob")) {
                            if inblob.len() == 64 && !p.join("outblob").exists() {
                                let _ = std::fs::write(p.join("generation"), "1\n");
                                let _ = std::fs::write(p.join("provider"), "tdx_guest\n");
                                let mut q = b"FAKE-TDX-QUOTE:".to_vec();
                                q.extend_from_slice(&inblob);
                                let _ = std::fs::write(p.join("outblob"), q);
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        FakeTsm { root, stop }
    }
}

impl Drop for FakeTsm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn json(s: &str) -> Value {
    cameleer_core::json::parse(s).unwrap()
}

fn start(tsm: &FakeTsm, extra: &[(&str, &str)]) -> (cameleer_main::Main, u16) {
    let dir = temp_dir("routes");
    let f = dir.join("tools.camel.yaml");
    std::fs::write(&f, ROUTES).unwrap();
    let port = free_port();
    let mut main = cameleer_main::new_main();
    main.add_route_file(f.to_string_lossy().to_string());
    for (k, v) in [
        ("camel.server.host", "127.0.0.1"),
        ("camel.server.port", &port.to_string()[..]),
        ("camel.attestation.enabled", "true"),
        ("camel.attestation.provider", "tsm"),
        ("camel.attestation.tsm-path", &tsm.root.to_string_lossy()[..]),
        ("camel.attestation.exe-digest", "false"),
        ("camel.component.sql.password", "hunter2"),
    ] {
        main.set_property(k, v);
    }
    for (k, v) in extra {
        main.set_property(*k, *v);
    }
    main.start().unwrap();
    (main, port)
}

fn get(port: u16, path: &str) -> (u16, String) {
    let r = HttpClient::new().request("GET", &format!("http://127.0.0.1:{}{}", port, path), &[], None, Some(Duration::from_secs(10))).unwrap();
    (r.status, r.body_text())
}

#[test]
fn a_client_gets_evidence_bound_to_its_nonce_and_checks_it() {
    let tsm = FakeTsm::start();
    let (main, port) = start(&tsm, &[]);
    let nonce = b"client-nonce-0123456789abcdef";
    // base64url, as it would travel in a URL
    let enc = b64(nonce).replace('+', "-").replace('/', "_").trim_end_matches('=').to_string();
    let (status, body) = get(port, &format!("/q/attestation?nonce={}", enc));
    assert_eq!(status, 200, "{}", body);
    let r = json(&body);
    assert_eq!(r.get("platform").map(|v| v.to_string()), Some("tdx".into()));
    assert_eq!(r.get("format").map(|v| v.to_string()), Some("tdx-quote".into()));
    // the client recomputes REPORTDATA (no TLS here) and finds it in the quote
    let rd = check_binding(&r, None, nonce).unwrap();
    let quote = unb64(&r.get("evidence").unwrap().to_string()).unwrap();
    assert!(quote.starts_with(b"FAKE-TDX-QUOTE:") && quote.ends_with(&rd));
    // the manifest names what runs, without the secret
    let m = r.get("manifest").unwrap().to_json();
    assert!(m.contains("tools.camel.yaml") && m.contains("\"lookup\"") && m.contains(env!("CARGO_PKG_VERSION")), "{}", m);
    assert!(!m.contains("hunter2") && m.contains("<secret>"), "{}", m);
    // bad nonces are refused
    assert_eq!(get(port, "/q/attestation?nonce=c2hvcnQ").0, 400, "a 5-byte nonce");
    assert_eq!(get(port, "/q/attestation?nonce=%%%").0, 400);
    drop(main);
}

#[test]
fn changes_after_start_up_are_in_the_log() {
    let tsm = FakeTsm::start();
    let (main, port) = start(&tsm, &[]);
    let first = json(&get(port, "/q/attestation").1);
    let digest = |r: &Value| r.get("binding").and_then(|b| b.get("manifestDigest")).map(|d| d.to_string()).unwrap();
    // the operator disables a tool with the kill switch
    cameleer_ai::AiRegistry::of(main.context()).unwrap().disable("lookup");
    let second = json(&get(port, "/q/attestation").1);
    assert_ne!(digest(&first), digest(&second), "the digest moves when a tool is disabled");
    let log = second.get("log").and_then(|l| l.as_array()).unwrap();
    assert_eq!(log.last().and_then(|e| e.get("kind")).map(|k| k.to_string()), Some("tools-changed".into()), "{}", second.get("log").unwrap().to_json());
    // a route removed at runtime is recorded too
    main.context().stop_route("lookup").unwrap();
    main.context().remove_route("lookup").unwrap();
    let third = json(&get(port, "/q/attestation").1);
    let kinds: Vec<String> = third.get("log").and_then(|l| l.as_array()).unwrap().iter().map(|e| e.get("kind").unwrap().to_string()).collect();
    assert!(kinds.contains(&"routes-changed".to_string()), "{:?}", kinds);
    check_binding(&third, None, &unb64(&third.get("nonce").unwrap().to_string()).unwrap()).unwrap();
    drop(main);
}

#[test]
fn mcp_clients_get_the_summary_inline_evidence_and_the_tool() {
    let tsm = FakeTsm::start();
    let (main, port) = start(&tsm, &[("camel.server.mcp-enabled", "true"), ("camel.server.mcp-tags", "*,untagged"), ("camel.attestation.mcp-tool", "true")]);
    let url = format!("http://127.0.0.1:{}/mcp", port);
    // `meta`: more `_meta` entries; `params`: more params fields (each starting with a comma)
    let call = |method: &str, name: Option<&str>, meta: &str, params: &str| {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"{}","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}{}}}{}}}}}"#,
            method, meta, params
        );
        let mut h = vec![("Content-Type", "application/json"), ("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", method)];
        if let Some(n) = name {
            h.push(("Mcp-Name", n));
        }
        json(&HttpClient::new().request("POST", &url, &h, Some(body.as_bytes()), Some(Duration::from_secs(10))).unwrap().body_text())
    };
    // discover: the summary always, the evidence when a nonce is sent, in one round trip
    let nonce = b"mcp-verifier-nonce-0123456789";
    let d = call("server/discover", None, &format!(r#","ing.cameleer/attestation":{{"nonce":"{}"}}"#, b64(nonce)), "");
    let a = d.get("result").and_then(|r| r.get("_meta")).and_then(|m| m.get("ing.cameleer/attestation")).unwrap_or_else(|| panic!("{}", d.to_json()));
    assert_eq!(a.get("platform").map(|v| v.to_string()), Some("tdx".into()));
    assert_eq!(a.get("endpoint").map(|v| v.to_string()), Some("/q/attestation".into()));
    check_binding(a.get("report").unwrap(), None, nonce).unwrap();
    assert!(d.to_json().contains("io.modelcontextprotocol/serverInfo"), "serverInfo stays: {}", d.to_json());
    // the opt-in tool, with a description that says showing is not verifying
    let list = call("tools/list", None, "", "").to_json();
    assert!(list.contains("cameleer_attestation") && list.contains("not verifying"), "{}", list);
    let r = call("tools/call", Some("cameleer_attestation"), "", &format!(r#","name":"cameleer_attestation","arguments":{{"nonce":"{}"}}"#, b64(nonce)));
    let s = r.get("result").and_then(|r| r.get("structuredContent")).unwrap_or_else(|| panic!("{}", r.to_json()));
    check_binding(s, None, nonce).unwrap();
    drop(main);
}

#[test]
fn without_an_evidence_source_start_up_fails_closed() {
    let dir = temp_dir("none");
    let f = dir.join("tools.camel.yaml");
    std::fs::write(&f, ROUTES).unwrap();
    let mut main = cameleer_main::new_main();
    main.add_route_file(f.to_string_lossy().to_string());
    main.set_property("camel.server.port", free_port().to_string());
    main.set_property("camel.attestation.enabled", "true");
    main.set_property("camel.attestation.tsm-path", dir.join("no-tsm").to_string_lossy().to_string());
    main.set_property("camel.attestation.dstack-endpoint", dir.join("no-dstack.sock").to_string_lossy().to_string());
    let err = main.start().unwrap_err();
    assert!(err.message.contains("no evidence source"), "{}", err.message);
}
