//! The `lumberjack` consumer driven by a real shipper: Elastic's Filebeat (the Apache-2.0 OSS
//! build), installed by `scripts/official-clients.sh install` (pinned version, SHA-512 checked).
//!
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1`. The binary is `CAMEL_OFFICIAL_FILEBEAT`, default
//! `/opt/camel-official/filebeat/filebeat`. Each test writes a log file, starts Filebeat with a
//! `filestream` input and `output.logstash` pointed at the route, and checks what arrived.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;

fn enabled() -> bool {
    std::env::var("CAMEL_OFFICIAL_CLIENTS").map(|v| v == "1").unwrap_or(false)
}

fn filebeat_bin() -> PathBuf {
    let p = PathBuf::from(std::env::var("CAMEL_OFFICIAL_FILEBEAT").unwrap_or_else(|_| "/opt/camel-official/filebeat/filebeat".into()));
    assert!(p.exists(), "Filebeat not found at {} (scripts/official-clients.sh install, or set CAMEL_OFFICIAL_FILEBEAT)", p.display());
    p
}

fn slash(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cameleer-filebeat-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Filebeat(Child, PathBuf);

impl Drop for Filebeat {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

/// Starts Filebeat shipping `lines` (written to a file in `dir`) to `output` (YAML lines under
/// `output.logstash:`).
fn start_filebeat(dir: PathBuf, lines: &[String], output: &str) -> (Filebeat, String) {
    let log = dir.join("app.log");
    std::fs::write(&log, lines.iter().map(|l| format!("{}\n", l)).collect::<String>()).unwrap();
    let cfg = format!(
        "filebeat.inputs:\n  - type: filestream\n    id: cameleer-official\n    paths: [\"{}\"]\noutput.logstash:\n{}\nlogging.level: info\nlogging.to_stderr: true\n",
        slash(&log),
        output
    );
    let cfg_path = dir.join("filebeat.yml");
    std::fs::write(&cfg_path, cfg).unwrap();
    let out = std::fs::File::create(dir.join("filebeat.out")).unwrap();
    let child = Command::new(filebeat_bin())
        .arg("-e")
        .arg("--strict.perms=false")
        .arg("-c")
        .arg(&cfg_path)
        .arg("--path.home")
        .arg(&dir)
        .arg("--path.data")
        .arg(dir.join("data"))
        .arg("--path.logs")
        .arg(dir.join("logs"))
        .stdout(Stdio::null())
        .stderr(out)
        .spawn()
        .expect("start filebeat");
    (Filebeat(child, dir), slash(&log))
}

fn wait_for(fb: &Filebeat, what: &str, timeout: Duration, f: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while !f() {
        if Instant::now() > deadline {
            let out = std::fs::read_to_string(fb.1.join("filebeat.out")).unwrap_or_default();
            let tail: String = out.lines().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            panic!("timed out waiting for {}; Filebeat said:\n{}", what, tail);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn messages(m: &MockEndpoint) -> Vec<String> {
    m.received_exchanges().iter().map(|e| e.body().get("message").map(|v| v.to_string()).unwrap_or_default()).collect()
}

/// Log lines long enough that every file is over 1 KiB (filestream ignores smaller files).
fn lines(n: usize, prefix: &str) -> Vec<String> {
    (1..=n).map(|i| format!("{} line-{:03} 10.0.0.{} - - \"GET /catalog/items/{}?page=1 HTTP/1.1\" 200 5120 \"-\" \"curl/8.5.0\"", prefix, i, i % 250, i)).collect()
}

#[test]
fn official_filebeat_ships_a_log_file() {
    if !enabled() {
        return;
    }
    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    let uri = "lumberjack:127.0.0.1:0";
    ctx.add_routes(|b| {
        b.from(uri).to("mock:beats");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, uri).unwrap();
    let mock = ctx.mock_endpoint("mock:beats").unwrap();
    let sent = lines(120, "plain");
    // small batches so several windows (compressed, the default) go over one connection
    let (fb, path) = start_filebeat(temp_dir("plain"), &sent, &format!("  hosts: [\"127.0.0.1:{}\"]\n  bulk_max_size: 25\n  compression_level: 3", port));
    wait_for(&fb, "120 events", Duration::from_secs(60), || mock.received_counter() >= 120);
    let mut got = messages(&mock);
    got.sort();
    got.dedup();
    assert_eq!(got, sent);
    let first = mock.received_exchanges()[0].body().clone();
    assert_eq!(first.get("log").and_then(|l| l.get("file")).and_then(|f| f.get("path")).map(|p| p.to_string().replace('\\', "/")), Some(path));
    assert_eq!(first.get("@metadata").and_then(|m| m.get("beat")), Some(&Value::from("filebeat")));
    assert!(first.get("host").and_then(|h| h.get("name")).is_some(), "{}", first.to_json());
    assert!(first.get("@timestamp").is_some());
    drop(fb);
    ctx.stop().unwrap();
}

#[test]
fn official_filebeat_over_tls_verifies_the_server() {
    if !enabled() {
        return;
    }
    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    ctx.registry().bind("beatsTls", cameleer_tls::testing::server_params());
    let uri = "lumberjack:127.0.0.1:0?sslContextParameters=#beatsTls";
    ctx.add_routes(|b| {
        b.from(uri).to("mock:tls");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, uri).unwrap();
    let mock = ctx.mock_endpoint("mock:tls").unwrap();
    let dir = temp_dir("tls");
    let ca = dir.join("ca.pem");
    std::fs::write(&ca, &cameleer_tls::testing::pki().localhost.cert_pem).unwrap();
    let sent = lines(30, "tls");
    let (fb, _) = start_filebeat(
        dir,
        &sent,
        &format!("  hosts: [\"localhost:{}\"]\n  ssl.enabled: true\n  ssl.certificate_authorities: [\"{}\"]\n  ssl.verification_mode: full", port, slash(&ca)),
    );
    wait_for(&fb, "30 events over TLS", Duration::from_secs(60), || mock.received_counter() >= 30);
    let mut got = messages(&mock);
    got.sort();
    got.dedup();
    assert_eq!(got, sent);
    drop(fb);
    ctx.stop().unwrap();
}

#[test]
fn official_filebeat_resends_a_window_that_failed() {
    if !enabled() {
        return;
    }
    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    let failed_once = Arc::new(AtomicBool::new(false));
    let f = failed_once.clone();
    let uri = "lumberjack:127.0.0.1:0";
    ctx.add_routes(move |b| {
        let f = f.clone();
        b.from(uri)
            .process(move |ex: &mut Exchange| {
                let msg = ex.body().get("message").map(|m| m.to_string()).unwrap_or_default();
                if msg.contains("line-007") && !f.swap(true, Ordering::SeqCst) {
                    return Err(CamelError::illegal_argument("the index is not ready"));
                }
                Ok(())
            })
            .to("mock:retry");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, uri).unwrap();
    let mock = ctx.mock_endpoint("mock:retry").unwrap();
    let sent = lines(20, "retry");
    let (fb, _) = start_filebeat(temp_dir("retry"), &sent, &format!("  hosts: [\"127.0.0.1:{}\"]\n  bulk_max_size: 10\n  backoff.init: 200ms", port));
    wait_for(&fb, "every line after the failure", Duration::from_secs(60), || {
        let mut got = messages(&mock);
        got.sort();
        got.dedup();
        got == sent
    });
    assert!(failed_once.load(Ordering::SeqCst), "the failure path ran");
    drop(fb);
    ctx.stop().unwrap();
}
