//! Interoperability against the `openssl` command-line tool (s_server / s_client).
//! Every test is skipped (passes trivially with a note) when `openssl` is not installed.

use camel_tls::{self_signed, CipherSuite, NamedGroup, TlsClientConfig, TlsServerConfig, TlsStream, TlsVersion};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

const RSA_CERT: &str = include_str!("../testdata/rsa_selfsigned.pem");
const RSA_KEY: &str = include_str!("../testdata/rsa2048.pkcs8.pem");
const CA: &str = include_str!("../testdata/ca.pem");
const INT: &str = include_str!("../testdata/int.pem");
const LEAF: &str = include_str!("../testdata/leaf.pem");
const LEAF_KEY: &str = include_str!("../testdata/leaf.key");

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

fn temp_dir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("camel-tls-interop-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Files {
    dir: PathBuf,
}

impl Files {
    fn new() -> Files {
        Files { dir: temp_dir() }
    }
    fn write(&self, name: &str, content: &str) -> String {
        let p = self.dir.join(name);
        std::fs::write(&p, content).unwrap();
        p.to_string_lossy().into_owned()
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn ec_identity() -> &'static camel_tls::SelfSigned {
    static ID: OnceLock<camel_tls::SelfSigned> = OnceLock::new();
    ID.get_or_init(|| self_signed("localhost"))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Kills the child if it is still running after `secs`.
fn watchdog(child: &Child, secs: u64) {
    let pid = child.id();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(secs));
        let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
    });
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

/// Starts `openssl s_server -www` with the given certificate/key and extra args.
fn start_s_server(cert_pem: &str, key_pem: &str, chain_pem: Option<&str>, extra: &[&str]) -> SServer {
    let files = Files::new();
    let cert = files.write("cert.pem", cert_pem);
    let key = files.write("key.pem", key_pem);
    let port = free_port();
    let mut cmd = Command::new("openssl");
    cmd.args(["s_server", "-accept", &format!("127.0.0.1:{port}"), "-cert", &cert, "-key", &key, "-www", "-naccept", "1"]);
    if let Some(chain) = chain_pem {
        let c = files.write("chain.pem", chain);
        cmd.args(["-cert_chain", &c]);
    }
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

/// Connects our client to s_server, issues GET / and returns the response text.
fn client_get(port: u16, name: &str, cfg: &TlsClientConfig, key_update: bool) -> (String, TlsVersion, CipherSuite, NamedGroup) {
    let sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut tls = TlsStream::connect(sock, name, cfg).unwrap_or_else(|e| panic!("handshake with openssl s_server failed: {e}"));
    if key_update {
        tls.send_key_update(true).unwrap();
    }
    tls.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
    let mut resp = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match tls.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => resp.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break, // s_server -www may skip close_notify
            Err(e) => panic!("read from s_server failed: {e}"),
        }
    }
    let text = String::from_utf8_lossy(&resp).into_owned();
    assert!(text.starts_with("HTTP/1.0 200 ok"), "unexpected s_server response: {text}");
    (text, tls.protocol_version(), tls.cipher_suite(), tls.named_group().unwrap())
}

fn ec_client(builder: camel_tls::TlsClientConfigBuilder) -> TlsClientConfig {
    builder.system_roots(false).add_root_pem(ec_identity().cert_pem.as_bytes()).unwrap().build()
}

const SUITES13: [(CipherSuite, &str); 3] = [
    (CipherSuite::Tls13Aes128GcmSha256, "TLS_AES_128_GCM_SHA256"),
    (CipherSuite::Tls13Aes256GcmSha384, "TLS_AES_256_GCM_SHA384"),
    (CipherSuite::Tls13Chacha20Poly1305Sha256, "TLS_CHACHA20_POLY1305_SHA256"),
];

const GROUPS: [(NamedGroup, &str); 3] = [(NamedGroup::X25519, "X25519"), (NamedGroup::Secp256r1, "P-256"), (NamedGroup::Secp384r1, "P-384")];

#[test]
fn client_vs_openssl_tls13_matrix() {
    if !openssl_available() {
        return;
    }
    let id = ec_identity();
    for (suite, oname) in SUITES13 {
        for (group, gname) in GROUPS {
            let srv = start_s_server(&id.cert_pem, &id.key_pem, None, &["-tls1_3", "-ciphersuites", oname, "-groups", gname]);
            let cfg = ec_client(TlsClientConfig::builder().groups(&[group]));
            let (text, v, s, g) = client_get(srv.port, "localhost", &cfg, false);
            assert_eq!((v, s, g), (TlsVersion::Tls13, suite, group));
            assert!(text.contains(oname), "s_server status page should mention {oname}");
        }
    }
}

#[test]
fn client_vs_openssl_hrr_and_key_update() {
    if !openssl_available() {
        return;
    }
    let id = ec_identity();
    // we send an X25519 share; s_server only accepts P-256 -> HelloRetryRequest
    let srv = start_s_server(&id.cert_pem, &id.key_pem, None, &["-tls1_3", "-groups", "P-256"]);
    let cfg = ec_client(TlsClientConfig::builder().groups(&[NamedGroup::X25519, NamedGroup::Secp256r1]));
    let (_, v, _, g) = client_get(srv.port, "localhost", &cfg, true);
    assert_eq!((v, g), (TlsVersion::Tls13, NamedGroup::Secp256r1));
}

#[test]
fn client_vs_openssl_tls12_matrix() {
    if !openssl_available() {
        return;
    }
    let id = ec_identity();
    let ec_suites = [
        (CipherSuite::EcdheEcdsaAes128GcmSha256, "ECDHE-ECDSA-AES128-GCM-SHA256"),
        (CipherSuite::EcdheEcdsaAes256GcmSha384, "ECDHE-ECDSA-AES256-GCM-SHA384"),
        (CipherSuite::EcdheEcdsaChacha20Poly1305, "ECDHE-ECDSA-CHACHA20-POLY1305"),
    ];
    for (suite, oname) in ec_suites {
        for (group, gname) in GROUPS {
            // TLS 1.2: the P-256 certificate curve must also be in supported_groups (RFC 8422)
            let srv = start_s_server(&id.cert_pem, &id.key_pem, None, &["-tls1_2", "-cipher", oname, "-groups", gname]);
            let mut groups = vec![group];
            if group != NamedGroup::Secp256r1 {
                groups.push(NamedGroup::Secp256r1);
            }
            let cfg = ec_client(TlsClientConfig::builder().groups(&groups));
            let (_, v, s, g) = client_get(srv.port, "localhost", &cfg, false);
            assert_eq!((v, s, g), (TlsVersion::Tls12, suite, group));
        }
    }
    let rsa_suites = [
        (CipherSuite::EcdheRsaAes128GcmSha256, "ECDHE-RSA-AES128-GCM-SHA256"),
        (CipherSuite::EcdheRsaAes256GcmSha384, "ECDHE-RSA-AES256-GCM-SHA384"),
        (CipherSuite::EcdheRsaChacha20Poly1305, "ECDHE-RSA-CHACHA20-POLY1305"),
    ];
    for (suite, oname) in rsa_suites {
        for sigalgs in ["RSA-PSS+SHA256", "RSA+SHA256", "RSA+SHA384"] {
            let srv = start_s_server(RSA_CERT, RSA_KEY, None, &["-tls1_2", "-cipher", oname, "-sigalgs", sigalgs]);
            let cfg = TlsClientConfig::builder().system_roots(false).add_root_pem(RSA_CERT.as_bytes()).unwrap().build();
            let (_, v, s, _) = client_get(srv.port, "rsa.test", &cfg, false);
            assert_eq!((v, s), (TlsVersion::Tls12, suite));
        }
    }
}

#[test]
fn client_vs_openssl_rsa_and_chain_tls13() {
    if !openssl_available() {
        return;
    }
    for sigalgs in ["RSA-PSS+SHA256", "RSA-PSS+SHA384", "RSA-PSS+SHA512"] {
        let srv = start_s_server(RSA_CERT, RSA_KEY, None, &["-tls1_3", "-sigalgs", sigalgs]);
        let cfg = TlsClientConfig::builder().system_roots(false).add_root_pem(RSA_CERT.as_bytes()).unwrap().build();
        let (_, v, _, _) = client_get(srv.port, "rsa.test", &cfg, false);
        assert_eq!(v, TlsVersion::Tls13);
    }
    let srv = start_s_server(LEAF, LEAF_KEY, Some(INT), &["-tls1_3"]);
    let cfg = TlsClientConfig::builder().system_roots(false).add_root_pem(CA.as_bytes()).unwrap().build();
    let (_, v, _, _) = client_get(srv.port, "leaf.camel.test", &cfg, false);
    assert_eq!(v, TlsVersion::Tls13);
}

/// Runs `openssl s_client` against our server; returns (s_client stdout, stderr).
fn s_client_vs_ours(server_cfg: TlsServerConfig, ca_pem: &str, name: &str, extra: &[&str]) -> (String, String) {
    let files = Files::new();
    let ca = files.write("ca.pem", ca_pem);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || -> Result<String, String> {
        let (sock, _) = listener.accept().map_err(|e| e.to_string())?;
        sock.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut tls = TlsStream::accept(sock, &server_cfg).map_err(|e| format!("our server handshake failed: {e}"))?;
        let info = format!("{:?} {:?} {:?}", tls.protocol_version(), tls.cipher_suite(), tls.named_group().unwrap());
        let mut line = String::new();
        {
            let mut r = BufReader::new(&mut tls);
            r.read_line(&mut line).map_err(|e| e.to_string())?;
        }
        tls.write_all(format!("WORLD:{}", line.trim()).as_bytes()).map_err(|e| e.to_string())?;
        tls.write_all(b"\n").map_err(|e| e.to_string())?;
        tls.close().map_err(|e| e.to_string())?;
        let mut rest = Vec::new();
        let _ = tls.read_to_end(&mut rest);
        Ok(info)
    });
    let mut cmd = Command::new("openssl");
    cmd.args([
        "s_client",
        "-connect",
        &format!("127.0.0.1:{port}"),
        "-servername",
        name,
        "-CAfile",
        &ca,
        "-verify_return_error",
        "-verify_hostname",
        name,
        "-brief",
    ]);
    cmd.args(extra);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn openssl s_client");
    watchdog(&child, 60);
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"hello\n").unwrap();
    // Keep stdin open until our server has read the line and answered: closing it
    // early (e.g. after a fixed sleep) makes s_client quit before the handshake
    // completes on a loaded machine. Our server sends close_notify, so s_client exits.
    let srv = server.join().unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(srv.is_ok(), "server error: {:?}\ns_client stderr:\n{stderr}", srv);
    assert!(stdout.contains("WORLD:hello"), "s_client did not receive our reply.\nstdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.contains("Verification: OK"), "s_client verification failed:\n{stderr}");
    (stdout, stderr)
}

fn ec_server_cfg() -> camel_tls::TlsServerConfigBuilder {
    let id = ec_identity();
    TlsServerConfig::builder().cert_chain_pem(id.cert_pem.as_bytes()).private_key_pem(id.key_pem.as_bytes())
}

#[test]
fn openssl_client_vs_server_tls13_matrix() {
    if !openssl_available() {
        return;
    }
    let ca = ec_identity().cert_pem.clone();
    for (_, oname) in SUITES13 {
        for (_, gname) in GROUPS {
            let (_, err) = s_client_vs_ours(ec_server_cfg().build().unwrap(), &ca, "localhost", &["-tls1_3", "-ciphersuites", oname, "-groups", gname]);
            assert!(err.contains("Protocol version: TLSv1.3"), "{err}");
            assert!(err.contains(&format!("Ciphersuite: {oname}")), "{err}");
        }
    }
    // HelloRetryRequest from our server: client shares X25519, we only accept P-384
    let cfg = ec_server_cfg().groups(&[NamedGroup::Secp384r1]).build().unwrap();
    let (_, err) = s_client_vs_ours(cfg, &ca, "localhost", &["-tls1_3", "-groups", "X25519:P-384"]);
    assert!(err.contains("TLSv1.3"), "{err}");
    // RSA-PSS signatures from our server
    for sigalgs in ["RSA-PSS+SHA256", "RSA-PSS+SHA384", "RSA-PSS+SHA512"] {
        let cfg = TlsServerConfig::from_pem(RSA_CERT.as_bytes(), RSA_KEY.as_bytes()).unwrap();
        s_client_vs_ours(cfg, RSA_CERT, "rsa.test", &["-tls1_3", "-sigalgs", sigalgs]);
    }
    // chain with intermediate
    let chain = format!("{LEAF}{INT}");
    let cfg = TlsServerConfig::from_pem(chain.as_bytes(), LEAF_KEY.as_bytes()).unwrap();
    s_client_vs_ours(cfg, CA, "leaf.camel.test", &["-tls1_3"]);
}

#[test]
fn openssl_client_vs_server_tls12_matrix() {
    if !openssl_available() {
        return;
    }
    let ca = ec_identity().cert_pem.clone();
    for oname in ["ECDHE-ECDSA-AES128-GCM-SHA256", "ECDHE-ECDSA-AES256-GCM-SHA384", "ECDHE-ECDSA-CHACHA20-POLY1305"] {
        for (group, gname) in GROUPS {
            let groups = if group == NamedGroup::Secp256r1 { gname.to_string() } else { format!("{gname}:P-256") };
            let cfg = ec_server_cfg().groups(&[group]).build().unwrap();
            let (_, err) = s_client_vs_ours(cfg, &ca, "localhost", &["-tls1_2", "-cipher", oname, "-groups", &groups]);
            assert!(err.contains("Protocol version: TLSv1.2"), "{err}");
            assert!(err.contains(&format!("Ciphersuite: {oname}")), "{err}");
        }
    }
    for oname in ["ECDHE-RSA-AES128-GCM-SHA256", "ECDHE-RSA-AES256-GCM-SHA384", "ECDHE-RSA-CHACHA20-POLY1305"] {
        for sigalgs in ["RSA-PSS+SHA256", "RSA+SHA256", "RSA+SHA512"] {
            let cfg = TlsServerConfig::from_pem(RSA_CERT.as_bytes(), RSA_KEY.as_bytes()).unwrap();
            let (_, err) = s_client_vs_ours(cfg, RSA_CERT, "rsa.test", &["-tls1_2", "-cipher", oname, "-sigalgs", sigalgs]);
            assert!(err.contains(&format!("Ciphersuite: {oname}")), "{err}");
        }
    }
}


#[test]
fn ocsp_stapling_both_ways() {
    use camel_tls::certgen::{issue, CertParams};
    use camel_tls::ocsp::{build_response, CertStatus, OcspResponse};
    use camel_tls::x509::Certificate;
    use camel_tls::{OcspMode, PrivateKey};
    if !openssl_available() {
        return;
    }
    let ca = issue(&CertParams::ca("OCSP Test CA"), PrivateKey::generate_ec(camel_tls::crypto::ec::EcCurve::P256), None);
    let leaf = issue(&CertParams::server(&["localhost"]), PrivateKey::generate_ec(camel_tls::crypto::ec::EcCurve::P256), Some(&ca));
    let files = Files::new();
    let ca_pem = files.write("ca.pem", &ca.cert_pem);
    let ca_key = files.write("ca.key", &ca.key_pem);
    let leaf_pem = files.write("leaf.pem", &leaf.cert_pem);
    let leafc = Certificate::from_der(&leaf.cert_der).unwrap();
    let cac = Certificate::from_der(&ca.cert_der).unwrap();
    let serial: String = leafc.serial.iter().map(|b| format!("{:02X}", b)).collect();
    files.write("index.txt", &format!("V\t351231235959Z\t\t{}\tunknown\t/CN=localhost\n", serial));
    // an OCSP response made by `openssl ocsp` (third-party vector)
    let d = files.dir.clone();
    let st = Command::new("openssl").args(["ocsp", "-issuer", &ca_pem, "-cert", &leaf_pem, "-no_nonce", "-reqout"]).arg(d.join("req.der")).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert!(st.success());
    let st = Command::new("openssl")
        .args(["ocsp", "-index"])
        .arg(d.join("index.txt"))
        .args(["-CA", &ca_pem, "-rsigner", &ca_pem, "-rkey", &ca_key, "-ndays", "1", "-reqin"])
        .arg(d.join("req.der"))
        .arg("-respout")
        .arg(d.join("resp.der"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success());
    let openssl_resp = std::fs::read(d.join("resp.der")).unwrap();
    let parsed = OcspResponse::parse(&openssl_resp).unwrap();
    assert_eq!(parsed.validate(&leafc, &cac, camel_tls::x509::now_unix()), Ok(CertStatus::Good));
    let resp_file = d.join("resp.der").to_string_lossy().into_owned();
    let chain = format!("{}{}", leaf.cert_pem, ca.cert_pem);

    // our client (Require) against s_server -status_file, TLS 1.3 and 1.2
    for version in ["-tls1_3", "-tls1_2"] {
        let srv = start_s_server(&leaf.cert_pem, &leaf.key_pem, None, &["-status_file", &resp_file, version]);
        let cfg = TlsClientConfig::builder().system_roots(false).add_root_pem(ca.cert_pem.as_bytes()).unwrap().ocsp_stapling(OcspMode::Require).build();
        let sock = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let tls = TlsStream::connect(sock, "localhost", &cfg).unwrap_or_else(|e| panic!("{}: {}", version, e));
        assert_eq!(tls.ocsp_response(), Some(&openssl_resp[..]), "{}", version);
    }
    // a server that does not staple fails under Require, passes under Request
    let srv = start_s_server(&leaf.cert_pem, &leaf.key_pem, None, &[]);
    let cfg = TlsClientConfig::builder().system_roots(false).add_root_pem(ca.cert_pem.as_bytes()).unwrap().ocsp_stapling(OcspMode::Require).build();
    let sock = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
    assert!(TlsStream::connect(sock, "localhost", &cfg).is_err());
    drop(srv);

    // openssl s_client -status against our stapling server
    let now = camel_tls::x509::now_unix();
    let ours = build_response(&leafc, &cac, &ca.key, &CertStatus::Good, now - 60, Some(now + 3600));
    for version in ["-tls1_3", "-tls1_2"] {
        let server_cfg = TlsServerConfig::builder().cert_chain_pem(chain.as_bytes()).private_key_pem(leaf.key_pem.as_bytes()).ocsp_response(ours.clone()).build().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            sock.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
            if let Ok(mut tls) = TlsStream::accept(sock, &server_cfg) {
                let mut rest = Vec::new();
                let _ = tls.read_to_end(&mut rest);
            }
        });
        let out = Command::new("openssl")
            .args(["s_client", "-connect", &format!("127.0.0.1:{port}"), "-servername", "localhost", "-CAfile", &ca_pem, "-status", version])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        h.join().unwrap();
        let all = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(all.contains("OCSP Response Status: successful"), "{}: {}", version, all);
        assert!(all.contains("Cert Status: good"), "{}: {}", version, all);
    }
    // our client against our server with a revoked status fails
    let revoked = build_response(&leafc, &cac, &ca.key, &CertStatus::Revoked { time: now - 30, reason: None }, now - 60, Some(now + 3600));
    let server_cfg = TlsServerConfig::builder().cert_chain_pem(chain.as_bytes()).private_key_pem(leaf.key_pem.as_bytes()).ocsp_response(revoked).build().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = thread::spawn(move || {
        let (sock, _) = listener.accept().unwrap();
        let _ = TlsStream::accept(sock, &server_cfg);
    });
    let cfg = TlsClientConfig::builder().system_roots(false).add_root_pem(ca.cert_pem.as_bytes()).unwrap().ocsp_stapling(OcspMode::Request).build();
    let err = TlsStream::connect(TcpStream::connect(("127.0.0.1", port)).unwrap(), "localhost", &cfg).unwrap_err();
    assert!(err.to_string().contains("revoked"), "{}", err);
    h.join().unwrap();
}
