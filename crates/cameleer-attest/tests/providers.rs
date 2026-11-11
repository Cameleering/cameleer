//! The evidence providers against a fake configfs-tsm directory and a fake dstack agent.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cameleer_attest::{check_binding, hex, report_data, unb64, Attestation, Dstack, EvidenceProvider, Manifest, Tsm};
use cameleer_core::attest::AttestationService;

fn temp_dir(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("cameleer-attest-{}-{}-{}", name, std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Emulates configfs-tsm: for every new report entry with an `inblob`, it bumps `generation`,
/// writes `provider` and a fake quote that embeds the REPORTDATA (`outblob`).
pub struct FakeTsm {
    pub root: PathBuf,
    stop: Arc<AtomicBool>,
    pub reports: Arc<AtomicUsize>,
}

impl FakeTsm {
    pub fn start(provider: &'static str) -> FakeTsm {
        let root = temp_dir("tsm");
        let stop = Arc::new(AtomicBool::new(false));
        let reports = Arc::new(AtomicUsize::new(0));
        let (r, s, n) = (root.clone(), stop.clone(), reports.clone());
        std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                if let Ok(dirs) = std::fs::read_dir(&r) {
                    for d in dirs.flatten() {
                        let p = d.path();
                        if let Ok(inblob) = std::fs::read(p.join("inblob")) {
                            if inblob.len() == 64 && !p.join("outblob").exists() {
                                let _ = std::fs::write(p.join("generation"), "1\n");
                                let _ = std::fs::write(p.join("provider"), format!("{}\n", provider));
                                let mut quote = b"FAKE-QUOTE:".to_vec();
                                quote.extend_from_slice(&inblob);
                                let _ = std::fs::write(p.join("outblob"), quote);
                                n.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        FakeTsm { root, stop, reports }
    }
}

impl Drop for FakeTsm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[test]
fn tsm_reports_carry_the_report_data_and_clean_up() {
    let fake = FakeTsm::start("tdx_guest");
    let tsm = Tsm::new(&fake.root);
    let rd = [0x5au8; 64];
    let ev = tsm.evidence(&rd).unwrap();
    assert_eq!(ev.platform, "tdx");
    assert_eq!(ev.format, "tdx-quote");
    assert!(ev.evidence.ends_with(&rd), "the quote embeds REPORTDATA");
    assert_eq!(std::fs::read_dir(&fake.root).unwrap().count(), 0, "the report entry is removed");
    let snp = FakeTsm::start("sev_guest");
    assert_eq!(Tsm::new(&snp.root).evidence(&rd).unwrap().platform, "sev-snp");
}

#[test]
fn a_missing_tsm_directory_is_an_error_not_a_hang() {
    let tsm = Tsm::new(Path::new("/definitely/not/here/tsm/report"));
    let err = tsm.evidence(&[0u8; 64]).unwrap_err();
    assert!(err.message.contains("Cannot create a report entry"), "{}", err.message);
}

/// A fake dstack agent over HTTP (the simulator's transport): `/GetQuote` echoes the
/// REPORTDATA inside a fake quote, `/Info` returns an app identity. Records each request.
fn fake_dstack(requests: Arc<std::sync::Mutex<Vec<(String, String)>>>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            requests.lock().unwrap().push((path.clone(), body.clone()));
            let (status, out) = match path.as_str() {
                "/GetQuote" => {
                    let v = cameleer_core::json::parse(&body).unwrap();
                    let rd = v.get("report_data").unwrap().to_string();
                    if rd.len() != 128 {
                        ("400 Bad Request", "report_data must be 64 bytes".to_string())
                    } else {
                        ("200 OK", format!(r#"{{"quote":"{}{}","event_log":"[{{\"event\":\"compose-hash\",\"digest\":\"ab\"}}]"}}"#, hex(b"DSTACK-QUOTE:"), rd))
                    }
                }
                "/Info" => ("200 OK", r#"{"app_id":"app-1","instance_id":"i-9","compose_hash":"c0ffee","os_image_hash":"0s","tcb_info":"big"}"#.to_string()),
                _ => ("404 Not Found", "no such method".to_string()),
            };
            // answer chunked, as some agents do
            let mut w = s;
            let resp = format!("HTTP/1.1 {}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n", status, out.len(), out);
            let _ = w.write_all(resp.as_bytes());
        }
    });
    format!("http://{}", addr)
}

#[test]
fn dstack_quotes_carry_the_report_data_and_the_app_identity() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let url = fake_dstack(requests.clone());
    let d = Dstack::new(url);
    let rd = [0x33u8; 64];
    let ev = d.evidence(&rd).unwrap();
    assert!(ev.evidence.starts_with(b"DSTACK-QUOTE:") && ev.evidence.ends_with(&rd), "{:?}", String::from_utf8_lossy(&ev.evidence));
    assert_eq!(ev.extra.get("dstack").and_then(|v| v.get("compose_hash")).map(|v| v.to_string()), Some("c0ffee".into()));
    assert!(ev.extra.get("dstack").and_then(|v| v.get("tcb_info")).is_none(), "only the identity fields are kept");
    assert_eq!(ev.extra.get("eventLog").and_then(|l| l.as_array()).map(|a| a.len()), Some(1), "the event log is parsed");
    let reqs = requests.lock().unwrap();
    let (path, body) = &reqs[0];
    assert_eq!(path, "/GetQuote");
    assert!(body.contains(&hex(&rd)), "raw 64-byte REPORTDATA as hex: {}", body);
}

#[test]
fn a_report_binds_the_manifest_tls_key_and_nonce() {
    let fake = FakeTsm::start("tdx_guest");
    let mut m = Manifest::default();
    m.version = "0.5.1".into();
    m.route_files.insert("orders.camel.yaml".into(), "aa".into());
    let spki = [4u8; 32];
    let a = Attestation::new(Box::new(Tsm::new(&fake.root)), m, Some(spki));
    let log = a.log();
    log.record("tool-disabled", "refund");
    let nonce = b"fresh-nonce-0123456789";
    let r = a.report(Some(nonce)).unwrap();
    // the client recomputes REPORTDATA from what it knows, and finds it in the quote
    let rd = check_binding(&r, Some(&spki), nonce).unwrap();
    let quote = unb64(&r.get("evidence").unwrap().to_string()).unwrap();
    assert!(quote.ends_with(&rd));
    assert_eq!(r.get("log").and_then(|l| l.as_array()).map(|a| a.len()), Some(2), "start and the change");
    // a different TLS key (a terminating proxy) or nonce fails the check
    assert!(check_binding(&r, Some(&[5u8; 32]), nonce).is_err());
    assert!(check_binding(&r, Some(&spki), b"another-nonce-0123456").is_err());
    assert!(check_binding(&r, None, nonce).is_err());
    // nonces outside 16..64 bytes are refused
    assert!(a.report(Some(b"short")).is_err());
    // without a nonce the server picks one and says so
    let r = a.report(None).unwrap();
    assert_eq!(r.get("nonceChosenBy").map(|v| v.to_string()), Some("server".into()));
    let _ = report_data(&[0; 48], None, b"x");
}

/// The real transport: dstack's agent on a unix socket (Linux and macOS only).
#[cfg(unix)]
#[test]
fn dstack_over_its_unix_socket() {
    use std::os::unix::net::UnixListener;
    let dir = temp_dir("dstack-sock");
    let sock = dir.join("dstack.sock");
    let l = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).unwrap();
            let out = if path == "/GetQuote" {
                let rd = cameleer_core::json::parse(&String::from_utf8(body).unwrap()).unwrap().get("report_data").unwrap().to_string();
                format!(r#"{{"quote":"{}"}}"#, rd)
            } else {
                "{}".to_string()
            };
            let mut w = s;
            let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}", out.len(), out).as_bytes());
        }
    });
    assert!(Dstack::available(&sock.to_string_lossy()));
    let rd = [0x77u8; 64];
    let ev = Dstack::new(sock.to_string_lossy().to_string()).evidence(&rd).unwrap();
    assert_eq!(ev.evidence, rd.to_vec());
}
