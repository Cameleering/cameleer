//! HTTPS routes, the management endpoints, attestation and MCP on one port
//! (`camel.server.sslEnabled=true`), and Camel's separate management port
//! (`camel.management.*`). They used to fail at start-up: the `/q/*` registration was plain
//! HTTP, and a port can't serve both.
#![cfg(all(feature = "attest", feature = "ai"))]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cameleer_attest::{b64, check_binding, spki_sha256};
use cameleer_http::client::HttpClient;
use cameleer_tls::testing;

const ROUTES: &str = r#"
- route:
    id: hello
    from:
      uri: platform-http:/hello
      steps:
        - setBody:
            constant: secure hello
- route:
    from:
      uri: ai-tool:ping
      parameters:
        description: Ping
        readOnlyHint: true
      steps:
        - setBody:
            constant: pong
"#;

fn temp_dir(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("cameleer-https-shared-{}-{}-{}", name, std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// configfs-tsm emulation, as in the attestation tests.
fn fake_tsm(stop: Arc<AtomicBool>) -> PathBuf {
    let root = temp_dir("tsm");
    let r = root.clone();
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            if let Ok(dirs) = std::fs::read_dir(&r) {
                for d in dirs.flatten() {
                    let p = d.path();
                    if let Ok(inblob) = std::fs::read(p.join("inblob")) {
                        if inblob.len() == 64 && !p.join("outblob").exists() {
                            let _ = std::fs::write(p.join("generation"), "1\n");
                            let _ = std::fs::write(p.join("provider"), "tdx_guest\n");
                            let _ = std::fs::write(p.join("outblob"), &inblob);
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    root
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn tls_client() -> HttpClient {
    HttpClient { read_timeout: Duration::from_secs(20), ..HttpClient::new() }.with_tls(testing::client_params().client_config().unwrap())
}

#[test]
fn https_routes_management_attestation_and_mcp_share_one_port() {
    let (cert, key) = testing::write_localhost_pem_files("shared");
    let stop = Arc::new(AtomicBool::new(false));
    let tsm = fake_tsm(stop.clone());
    let dir = temp_dir("routes");
    let f = dir.join("server.camel.yaml");
    std::fs::write(&f, ROUTES).unwrap();
    let port = free_port();
    let mut main = cameleer_main::new_main();
    main.add_route_file(f.to_string_lossy().to_string());
    for (k, v) in [
        ("camel.server.host", "127.0.0.1".to_string()),
        ("camel.server.port", port.to_string()),
        ("camel.server.sslEnabled", "true".into()),
        ("camel.server.ssl.certFile", cert.display().to_string()),
        ("camel.server.ssl.keyFile", key.display().to_string()),
        ("camel.server.enabled", "true".into()),
        ("camel.server.healthCheckEnabled", "true".into()),
        ("camel.server.metricsEnabled", "true".into()),
        ("camel.attestation.enabled", "true".into()),
        ("camel.attestation.provider", "tsm".into()),
        ("camel.attestation.tsm-path", tsm.to_string_lossy().to_string()),
        ("camel.attestation.exe-digest", "false".into()),
        ("camel.server.mcp-enabled", "true".into()),
        ("camel.server.mcp-tags", "*,untagged".into()),
    ] {
        main.set_property(k, v);
    }
    main.start().expect("HTTPS routes, management, attestation and MCP start together");
    let base = format!("https://localhost:{}", port);
    let c = tls_client();
    assert_eq!(c.get(&format!("{}/hello", base)).unwrap().body_text(), "secure hello");
    let h = c.get(&format!("{}/q/health", base)).unwrap();
    assert_eq!(h.status, 200, "{}", h.body_text());
    assert_eq!(c.get(&format!("{}/q/metrics", base)).unwrap().status, 200);
    // the attestation report is bound to the certificate this client sees on the connection
    let der = cameleer_tls::pem::parse_pem(&std::fs::read(&cert).unwrap()).unwrap().into_iter().find(|b| b.label == "CERTIFICATE").unwrap().der;
    let observed = spki_sha256(&der).unwrap();
    let nonce = b"https-client-nonce-0123456789";
    let enc = b64(nonce).replace('+', "-").replace('/', "_").trim_end_matches('=').to_string();
    let r = c.get(&format!("{}/q/attestation?nonce={}", base, enc)).unwrap();
    assert_eq!(r.status, 200, "{}", r.body_text());
    let report = cameleer_core::json::parse(&r.body_text()).unwrap();
    check_binding(&report, Some(&observed), nonce).unwrap();
    // MCP over the same HTTPS port
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let m = c
        .request("POST", &format!("{}/mcp", base), &[("Content-Type", "application/json"), ("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", "server/discover")], Some(body.as_bytes()), Some(Duration::from_secs(10)))
        .unwrap();
    assert_eq!(m.status, 200, "{}", m.body_text());
    assert!(m.body_text().contains("ing.cameleer/attestation"), "{}", m.body_text());
    // plain HTTP on that port is refused, as for any HTTPS port
    assert!(HttpClient { read_timeout: Duration::from_secs(5), ..HttpClient::new() }.get(&format!("http://127.0.0.1:{}/q/health", port)).map(|r| r.status != 200).unwrap_or(true));
    stop.store(true, Ordering::Relaxed);
    drop(main);
}

#[test]
fn the_separate_management_port_serves_plain_http_next_to_https_routes() {
    let (cert, key) = testing::write_localhost_pem_files("mgmt");
    let dir = temp_dir("mgmt");
    let f = dir.join("server.camel.yaml");
    std::fs::write(&f, ROUTES).unwrap();
    let (port, mgmt) = (free_port(), free_port());
    let mut main = cameleer_main::new_main();
    main.add_route_file(f.to_string_lossy().to_string());
    for (k, v) in [
        ("camel.server.host", "127.0.0.1".to_string()),
        ("camel.server.port", port.to_string()),
        ("camel.server.sslEnabled", "true".into()),
        ("camel.server.ssl.certFile", cert.display().to_string()),
        ("camel.server.ssl.keyFile", key.display().to_string()),
        ("camel.management.enabled", "true".into()),
        ("camel.management.host", "127.0.0.1".into()),
        ("camel.management.port", mgmt.to_string()),
        ("camel.management.healthCheckEnabled", "true".into()),
    ] {
        main.set_property(k, v);
    }
    main.start().unwrap();
    assert_eq!(tls_client().get(&format!("https://localhost:{}/hello", port)).unwrap().body_text(), "secure hello");
    let h = HttpClient::new().get(&format!("http://127.0.0.1:{}/q/health", mgmt)).unwrap();
    assert_eq!(h.status, 200, "{}", h.body_text());
    drop(main);
}
