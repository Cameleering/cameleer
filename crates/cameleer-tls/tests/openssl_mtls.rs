//! Mutual TLS and session resumption interop with `openssl s_server` / `s_client`
//! (`-Verify`, `-cert`, `-sess_out` / `-sess_in`, `-reconnect`-style double connects).
//! Every test is skipped (passes with a note) when `openssl` is not installed.

use cameleer_tls::testing::{mtls, mtls_anonymous_client_params, mtls_client_params, mtls_server_params};
use cameleer_tls::{ClientAuth, TlsClientConfig, TlsServerConfig, TlsStream, TlsVersion};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, OnceLock};
use std::thread;
use std::time::Duration;

fn openssl_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let ok = Command::new("openssl").arg("version").output().map(|o| o.status.success()).unwrap_or(false);
        if !ok {
            eprintln!("openssl binary not found: skipping interop tests");
        }
        ok
    })
}

struct Files {
    dir: PathBuf,
}

impl Files {
    fn new() -> Files {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("cameleer-tls-mtls-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        Files { dir }
    }
    fn write(&self, name: &str, content: &str) -> String {
        let p = self.dir.join(name);
        std::fs::write(&p, content).unwrap();
        p.to_string_lossy().into_owned()
    }
    fn path(&self, name: &str) -> String {
        self.dir.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn watchdog(child: &Child, secs: u64) {
    let pid = child.id();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(secs));
        let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
    });
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct SServer {
    child: Child,
    port: u16,
    _files: Files,
}

impl Drop for SServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `openssl s_server -www` with the mTLS server identity, trusting the mTLS CA.
fn start_s_server(naccept: u32, extra: &[&str]) -> SServer {
    let pki = mtls();
    let files = Files::new();
    let cert = files.write("cert.pem", &pki.server.cert_pem);
    let key = files.write("key.pem", &pki.server.key_pem);
    let ca = files.write("ca.pem", &pki.ca.cert_pem);
    let port = free_port();
    let mut cmd = Command::new("openssl");
    cmd.args(["s_server", "-accept", &format!("127.0.0.1:{port}"), "-cert", &cert, "-key", &key, "-CAfile", &ca, "-www"]);
    cmd.args(["-naccept", &naccept.to_string()]);
    cmd.args(extra);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    let mut child = cmd.spawn().expect("spawn openssl s_server");
    watchdog(&child, 60);
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut r = BufReader::new(stdout);
        let mut line = String::new();
        let mut signalled = false;
        while r.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
            if !signalled && line.contains("ACCEPT") {
                let _ = tx.send(());
                signalled = true;
            }
            line.clear();
        }
    });
    if rx.recv_timeout(Duration::from_secs(20)).is_err() {
        let mut err = String::new();
        let _ = child.stderr.take().unwrap().read_to_string(&mut err);
        panic!("openssl s_server did not start: {err}");
    }
    SServer { child, port, _files: files }
}

/// GET / against s_server; returns (page, resumed) or the error text.
fn get(port: u16, cfg: &TlsClientConfig) -> Result<(String, bool), String> {
    let sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut tls = TlsStream::connect(sock, "localhost", cfg).map_err(|e| e.to_string())?;
    tls.write_all(b"GET / HTTP/1.0\r\n\r\n").map_err(|e| e.to_string())?;
    let mut resp = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match tls.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => resp.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !resp.is_empty() => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    let text = String::from_utf8_lossy(&resp).into_owned();
    if !text.starts_with("HTTP/1.0 200 ok") {
        return Err(format!("unexpected s_server response: {text}"));
    }
    Ok((text, tls.session_resumed()))
}

fn versions() -> [(TlsVersion, &'static str); 2] {
    [(TlsVersion::Tls13, "-tls1_3"), (TlsVersion::Tls12, "-tls1_2")]
}

#[test]
fn our_client_certificate_vs_openssl_verify() {
    if !openssl_available() {
        return;
    }
    for (_, flag) in versions() {
        let srv = start_s_server(1, &["-Verify", "1", "-verify_return_error", flag]);
        let (page, _) = get(srv.port, &mtls_client_params().client_config().unwrap()).unwrap_or_else(|e| panic!("{flag}: {e}"));
        assert!(page.contains("camel-client") || page.contains("Client certificate"), "{flag}: s_server page lacks the client certificate:\n{page}");
        drop(srv);
        // without a certificate s_server aborts the handshake
        let srv = start_s_server(1, &["-Verify", "1", "-verify_return_error", flag]);
        let r = get(srv.port, &mtls_anonymous_client_params().client_config().unwrap());
        assert!(r.is_err(), "{flag}: s_server accepted a client without certificate");
    }
}

/// `openssl s_client` against our server; returns (server result: peer CN + resumed, s_client output).
fn s_client_vs_ours(cfg: &TlsServerConfig, extra: &[&str], files: &Files) -> (Result<(Option<String>, bool), String>, String) {
    let pki = mtls();
    let ca = files.write("ca.pem", &pki.ca.cert_pem);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let cfg = cfg.clone();
    let server = thread::spawn(move || -> Result<(Option<String>, bool), String> {
        let (sock, _) = listener.accept().map_err(|e| e.to_string())?;
        sock.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut tls = TlsStream::accept(sock, &cfg).map_err(|e| format!("our server handshake failed: {e}"))?;
        let mut line = String::new();
        BufReader::new(&mut tls).read_line(&mut line).map_err(|e| e.to_string())?;
        tls.write_all(format!("WORLD:{}\n", line.trim()).as_bytes()).map_err(|e| e.to_string())?;
        let peer = tls.peer_certificates().first().map(|c| cameleer_tls::Certificate::from_der(c).unwrap().subject.common_name().unwrap_or("").to_string());
        let resumed = tls.session_resumed();
        tls.close().map_err(|e| e.to_string())?;
        let mut rest = Vec::new();
        let _ = tls.read_to_end(&mut rest);
        Ok((peer, resumed))
    });
    let mut cmd = Command::new("openssl");
    cmd.args(["s_client", "-connect", &format!("127.0.0.1:{port}"), "-servername", "localhost", "-CAfile", &ca, "-verify_return_error"]);
    cmd.args(extra);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn openssl s_client");
    watchdog(&child, 60);
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(b"hello\n");
    let srv = server.join().unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (srv, text)
}

#[test]
fn openssl_client_certificate_vs_our_server() {
    if !openssl_available() {
        return;
    }
    let pki = mtls();
    for (v, flag) in versions() {
        let files = Files::new();
        let cert = files.write("client.pem", &pki.client.cert_pem);
        let key = files.write("client.key", &pki.client.key_pem);
        let mut p = mtls_server_params(ClientAuth::Need);
        p.min_version = Some(v);
        p.max_version = Some(v);
        let cfg = p.server_config().unwrap();
        let (srv, text) = s_client_vs_ours(&cfg, &[flag, "-cert", &cert, "-key", &key], &files);
        let (peer, _) = srv.unwrap_or_else(|e| panic!("{flag}: {e}\n{text}"));
        assert_eq!(peer.as_deref(), Some("camel-client"), "{flag}");
        assert!(text.contains("WORLD:hello"), "{flag}: {text}");
        // without a client certificate our server refuses
        let (srv, text) = s_client_vs_ours(&cfg, &[flag], &files);
        let e = srv.expect_err("our server accepted s_client without certificate");
        assert!(e.contains("certificate required"), "{flag}: {e}\n{text}");
    }
}

#[test]
fn openssl_sess_out_sess_in_resumes_with_our_server() {
    if !openssl_available() {
        return;
    }
    let pki = mtls();
    for (v, flag) in versions() {
        for no_ticket in [false, true] {
            if no_ticket && v == TlsVersion::Tls13 {
                continue; // TLS 1.3 resumption is ticket based
            }
            let files = Files::new();
            let sess = files.path("sess.pem");
            let cert = files.write("client.pem", &pki.client.cert_pem);
            let key = files.write("client.key", &pki.client.key_pem);
            let mut p = mtls_server_params(ClientAuth::Need);
            p.min_version = Some(v);
            p.max_version = Some(v);
            let cfg = p.server_config().unwrap();
            let mut extra = vec![flag, "-cert", &cert, "-key", &key, "-sess_out", &sess];
            if no_ticket {
                extra.push("-no_ticket");
            }
            let (srv, text) = s_client_vs_ours(&cfg, &extra, &files);
            let (peer, resumed) = srv.unwrap_or_else(|e| panic!("{flag}: {e}\n{text}"));
            assert!(!resumed && peer.is_some());
            assert!(std::fs::metadata(&sess).map(|m| m.len() > 0).unwrap_or(false), "{flag}: s_client saved no session\n{text}");
            let mut extra = vec![flag, "-sess_in", &sess];
            if no_ticket {
                extra.push("-no_ticket");
            }
            let (srv, text) = s_client_vs_ours(&cfg, &extra, &files);
            let (peer, resumed) = srv.unwrap_or_else(|e| panic!("{flag} resume: {e}\n{text}"));
            assert!(resumed, "{flag} no_ticket={no_ticket}: our server did not resume\n{text}");
            assert!(text.contains("Reused"), "{flag}: s_client did not report reuse\n{text}");
            assert_eq!(peer.as_deref(), Some("camel-client"), "client certificate restored from the session");
        }
    }
}

#[test]
fn our_client_resumes_with_openssl_s_server() {
    if !openssl_available() {
        return;
    }
    for (_, flag) in versions() {
        for no_ticket in [false, true] {
            let mut extra = vec![flag];
            if no_ticket {
                extra.push("-no_ticket");
            }
            let srv = start_s_server(2, &extra);
            let cfg: TlsClientConfig = mtls_anonymous_client_params().client_config().unwrap();
            let (_, r1) = get(srv.port, &cfg).unwrap_or_else(|e| panic!("{flag}: {e}"));
            let (page, r2) = get(srv.port, &cfg).unwrap_or_else(|e| panic!("{flag} second: {e}"));
            assert!(!r1, "{flag}");
            assert!(r2, "{flag} no_ticket={no_ticket}: second connection did not resume\n{page}");
            assert!(page.contains("Reused"), "{flag}: s_server reports no reuse\n{page}");
        }
    }
}
