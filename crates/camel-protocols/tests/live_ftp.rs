//! Live interop: the `ftp` / `ftps` components and the FTP client against a real vsftpd
//! (`scripts/live-interop.sh`). Runs only with `CAMEL_LIVE_INTEROP=1`.

use std::sync::Arc;
use std::time::Duration;

use camel_core::prelude::*;
use camel_protocols::ftp::{FtpClient, FtpSecurity};
use camel_tls::testing::live;

const PORT: u16 = 2121;
const IMPLICIT_PORT: u16 = 2990;
const REUSE_PORT: u16 = 2991;
const USER: &str = "camelftp";

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_protocols::register(&ctx);
    live::bind_beans(&ctx);
    ctx
}

fn dir(name: &str) -> String {
    format!("live-{}-{}", std::process::id(), name)
}

fn client() -> FtpClient {
    let mut c = FtpClient::connect("127.0.0.1", PORT, Duration::from_secs(5), Duration::from_secs(10)).unwrap();
    c.login(USER, live::PASSWORD).unwrap();
    c.set_passive(true);
    c.set_binary(true).unwrap();
    c
}

fn secure_client(port: u16, implicit: bool, resumption: bool) -> Result<FtpClient> {
    let mut p = live::client_params();
    p.session_resumption = Some(resumption);
    let tls = Arc::new(p.client_config().unwrap());
    let sec = if implicit { FtpSecurity::Implicit { tls, server_name: "localhost".into() } } else { FtpSecurity::Explicit { tls, server_name: "localhost".into() } };
    let mut c = FtpClient::connect_secure("127.0.0.1", port, Duration::from_secs(5), Duration::from_secs(10), &sec)?;
    c.login(USER, live::PASSWORD)?;
    c.protect_data(0, "P")?;
    c.set_passive(true);
    c.set_binary(true)?;
    Ok(c)
}

/// Remove a directory tree through FTP (the server's files belong to the ftp user).
fn remove_tree(c: &mut FtpClient, path: &str) {
    if let Ok(entries) = c.list(path) {
        for e in entries {
            let p = format!("{}/{}", path, e.name);
            if e.is_dir {
                remove_tree(c, &p);
            } else {
                let _ = c.dele(&p);
            }
        }
    }
    let _ = c.rmd(path);
}

fn wait_until(f: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn live_vsftpd_client_operations_passive_and_active() {
    if !live::enabled() {
        return;
    }
    let d = dir("client");
    for passive in [true, false] {
        let mut c = FtpClient::connect("127.0.0.1", PORT, Duration::from_secs(5), Duration::from_secs(10)).unwrap();
        assert!(c.login(USER, "wrong").is_err());
        let mut c = FtpClient::connect("127.0.0.1", PORT, Duration::from_secs(5), Duration::from_secs(10)).unwrap();
        c.login(USER, live::PASSWORD).unwrap();
        c.set_passive(passive);
        c.set_binary(true).unwrap();
        assert_eq!(c.pwd().unwrap(), "/");
        c.mkdirs(&format!("{}/a/b", d)).unwrap();
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        c.stor(&format!("{}/a/b/x.bin", d), &data, false).unwrap();
        c.stor(&format!("{}/a/b/x.bin", d), b"!", true).unwrap();
        let back = c.retr(&format!("{}/a/b/x.bin", d)).unwrap();
        assert_eq!(back.len(), data.len() + 1);
        assert_eq!(&back[..data.len()], &data[..]);
        assert_eq!(c.size(&format!("{}/a/b/x.bin", d)).unwrap(), Some(data.len() as u64 + 1));
        assert!(c.mdtm(&format!("{}/a/b/x.bin", d)).unwrap().is_some());
        let l = c.list(&format!("{}/a/b", d)).unwrap();
        assert_eq!(l.len(), 1, "{:?}", l.iter().map(|f| f.name.clone()).collect::<Vec<_>>());
        assert_eq!((l[0].name.as_str(), l[0].size, l[0].is_dir), ("x.bin", data.len() as u64 + 1, false));
        assert_eq!(c.nlst(&format!("{}/a", d)).unwrap().iter().map(|n| n.rsplit('/').next().unwrap().to_string()).collect::<Vec<_>>(), vec!["b".to_string()]);
        c.cwd(&format!("{}/a", d)).unwrap();
        c.rename("b/x.bin", "y.bin").unwrap();
        assert!(c.exists("y.bin").unwrap());
        assert!(!c.exists("nope.bin").unwrap());
        // ASCII mode: vsftpd converts line endings (ascii_upload/download are off by default,
        // so the bytes come back as stored)
        c.set_binary(false).unwrap();
        c.stor("text.txt", b"l1\nl2\n", false).unwrap();
        assert_eq!(c.retr("text.txt").unwrap(), b"l1\nl2\n");
        c.dele("text.txt").unwrap();
        c.dele("y.bin").unwrap();
        c.cdup().unwrap();
        c.rmd("a/b").unwrap();
        c.rmd("a").unwrap();
        assert!(c.dele("missing").is_err());
        c.cdup().unwrap();
        c.rmd(&d).unwrap();
        c.quit();
    }
}

#[test]
fn live_vsftpd_consumer_move_delete_include_and_noop() {
    if !live::enabled() {
        return;
    }
    let d = dir("consume");
    let mut c = client();
    c.mkdirs(&format!("{}/inbox", d)).unwrap();
    c.mkdirs(&format!("{}/del/sub", d)).unwrap();
    c.stor(&format!("{}/inbox/a.txt", d), b"Hello A", false).unwrap();
    c.stor(&format!("{}/inbox/b.txt", d), b"Hello B", false).unwrap();
    c.stor(&format!("{}/inbox/c.log", d), b"skip me", false).unwrap();
    c.stor(&format!("{}/del/one.csv", d), b"1\n", false).unwrap();
    c.stor(&format!("{}/del/sub/two.csv", d), b"2\n", false).unwrap();
    let ctx = context();
    let base = format!("ftp://{}@127.0.0.1:{}", USER, PORT);
    let mv = format!("{}/{}/inbox?password={}&passiveMode=true&binary=true&antInclude=*.txt&move=done&initialDelay=0&delay=100&sortBy=file:name", base, d, live::PASSWORD);
    let del = format!("{}/{}/del?password={}&recursive=true&delete=true&stepwise=false&initialDelay=0&delay=100&passiveMode=false", base, d, live::PASSWORD);
    ctx.add_routes(move |b| {
        b.from(&mv).convert_body_to("String").to("mock:moved");
        b.from(&del).convert_body_to("String").to("mock:deleted");
    })
    .unwrap();
    let moved = ctx.mock_endpoint("mock:moved").unwrap();
    moved.expected_bodies_received(vec!["Hello A", "Hello B"]);
    moved.expected_header_received("CamelFileHost", "127.0.0.1");
    let deleted = ctx.mock_endpoint("mock:deleted").unwrap();
    deleted.expected_bodies_received_in_any_order(vec!["1\n", "2\n"]);
    ctx.start().unwrap();
    MockEndpoint::assert_all_satisfied(&[&moved, &deleted]).unwrap();
    let ex = moved.exchange(0).unwrap();
    assert_eq!(ex.header("CamelFileName").unwrap().to_string(), "a.txt");
    assert_eq!(ex.header("CamelFileLength").unwrap().as_i64(), Some(7));
    assert!(ex.header("CamelFileLastModified").unwrap().as_i64().unwrap() > 0);
    let mut chk = client();
    assert!(wait_until(|| {
        let mut c = client();
        c.exists(&format!("{}/inbox/done/b.txt", d)).unwrap_or(false) && !c.exists(&format!("{}/inbox/b.txt", d)).unwrap_or(true)
    }));
    assert_eq!(chk.retr(&format!("{}/inbox/done/a.txt", d)).unwrap(), b"Hello A");
    assert!(chk.exists(&format!("{}/inbox/c.log", d)).unwrap(), "excluded by antInclude");
    assert!(wait_until(|| {
        let mut c = client();
        !c.exists(&format!("{}/del/one.csv", d)).unwrap_or(true) && !c.exists(&format!("{}/del/sub/two.csv", d)).unwrap_or(true)
    }));
    ctx.stop().unwrap();

    // noop + idempotent: files stay and are consumed once
    chk.mkdirs(&format!("{}/noop", d)).unwrap();
    chk.stor(&format!("{}/noop/keep.txt", d), b"keep", false).unwrap();
    let ctx = context();
    let noop = format!("{}/{}/noop?password={}&passiveMode=true&noop=true&initialDelay=0&delay=50", base, d, live::PASSWORD);
    ctx.add_routes(move |b| {
        b.from(&noop).to("mock:noop");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:noop").unwrap();
    m.expected_message_count(1);
    ctx.start().unwrap();
    m.assert_is_satisfied().unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(m.received_counter(), 1, "idempotent");
    assert!(chk.exists(&format!("{}/noop/keep.txt", d)).unwrap());
    ctx.stop().unwrap();
    remove_tree(&mut chk, &d);
    chk.quit();
}

#[test]
fn live_vsftpd_producer_file_exist_strategies() {
    if !live::enabled() {
        return;
    }
    let d = dir("produce");
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = format!("ftp://{}@127.0.0.1:{}/{}/outbox?password={}&passiveMode=true", USER, PORT, d, live::PASSWORD);
    let mut c = client();
    let target = format!("{}/outbox/sub/greeting.txt", d);
    let ex = t
        .request(&base, |ex| {
            ex.set_body("Hello");
            ex.set_header("CamelFileName", "sub/greeting.txt");
        })
        .unwrap();
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(c.retr(&target).unwrap(), b"Hello");
    assert_eq!(ex.header("CamelFileNameProduced").unwrap().to_string(), format!("{}/outbox/sub/greeting.txt", d));
    assert_eq!(ex.header("CamelFtpReplyCode").unwrap().as_i64(), Some(226));
    let err = t.send_body_and_header(&format!("{}&fileExist=Fail", base), "Again", "CamelFileName", "sub/greeting.txt").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "GenericFileOperationFailedException");
    t.send_body_and_header(&format!("{}&fileExist=Ignore", base), "Ignored", "CamelFileName", "sub/greeting.txt").unwrap();
    assert_eq!(c.retr(&target).unwrap(), b"Hello");
    t.send_body_and_header(&format!("{}&fileExist=Append&binary=true", base), " World", "CamelFileName", "sub/greeting.txt").unwrap();
    assert_eq!(c.retr(&target).unwrap(), b"Hello World");
    t.send_body_and_header(&format!("{}&tempPrefix=.inprogress-", base), "Replaced", "CamelFileName", "sub/greeting.txt").unwrap();
    assert_eq!(c.retr(&target).unwrap(), b"Replaced");
    assert!(!c.exists(&format!("{}/outbox/sub/.inprogress-greeting.txt", d)).unwrap());
    t.send_body_and_header(&format!("{}&fileExist=Move&moveExisting=old", base), "Newest", "CamelFileName", "sub/greeting.txt").unwrap();
    assert_eq!(c.retr(&target).unwrap(), b"Newest");
    assert_eq!(c.retr(&format!("{}/outbox/old/sub/greeting.txt", d)).unwrap(), b"Replaced");
    t.send_body(&format!("{}&fileName=report-${{exchangeId}}.txt&stepwise=false&disconnect=true", base), "R").unwrap();
    assert_eq!(c.list(&format!("{}/outbox", d)).unwrap().iter().filter(|f| f.name.starts_with("report-")).count(), 1);
    // vsftpd stores what the producer wrote (owner: the ftp user)
    let local = std::path::Path::new("/home/camelftp/ftp").join(&target);
    if local.exists() {
        assert_eq!(std::fs::read(&local).unwrap(), b"Newest");
    }
    ctx.stop().unwrap();
    remove_tree(&mut c, &d);
    c.quit();
}

#[test]
fn live_vsftpd_ftps_explicit_implicit_and_session_reuse() {
    if !live::enabled() {
        return;
    }
    let d = dir("ftps");
    client().mkdirs(&d).unwrap();
    for (port, implicit) in [(PORT, false), (IMPLICIT_PORT, true), (REUSE_PORT, false)] {
        let mut c = secure_client(port, implicit, true).unwrap_or_else(|e| panic!("{}: {}", port, e));
        assert!(c.is_secure());
        let p = format!("{}/s-{}.bin", d, port);
        c.stor(&p, &[1, 2, 3, 0, 255], false).unwrap_or_else(|e| panic!("{}: {}", port, e));
        for _ in 0..3 {
            assert_eq!(c.retr(&p).unwrap_or_else(|e| panic!("{}: {}", port, e)), vec![1, 2, 3, 0, 255]);
            assert!(c.list(&d).unwrap().iter().any(|f| f.name == format!("s-{}.bin", port)));
        }
        c.dele(&p).unwrap();
        c.quit();
    }
    // require_ssl_reuse=YES refuses data connections that do not resume the control session
    let mut c = secure_client(REUSE_PORT, false, false).unwrap();
    let e = c.stor(&format!("{}/no-reuse.bin", d), b"x", false).expect_err("vsftpd requires TLS session reuse");
    assert!(e.to_string().contains("522") || e.to_string().to_ascii_lowercase().contains("reuse"), "{}", e);
    c.quit();

    // the ftps component: producer over explicit, consumer over implicit, and the reuse server
    let ctx = context();
    let producer = format!("ftps://{}@localhost:{}/{}/out?password={}&passiveMode=true&sslContextParameters=#liveSsl", USER, PORT, d, live::PASSWORD);
    let consumer = format!("ftps://{}@localhost:{}/{}/out?password={}&passiveMode=true&isImplicit=true&sslContextParameters=#liveSsl&delete=true&initialDelay=0&delay=100", USER, IMPLICIT_PORT, d, live::PASSWORD);
    ctx.add_routes(move |b| {
        b.from(&consumer).convert_body_to("String").to("mock:ftps");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:ftps").unwrap();
    mock.expected_bodies_received(vec!["secret payload"]);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header(&producer, "secret payload", "CamelFileName", "doc.txt").unwrap();
    mock.assert_is_satisfied().unwrap();
    let reuse = format!("ftps://{}@localhost:{}/{}/reuse?password={}&passiveMode=true&sslContextParameters=#liveSsl", USER, REUSE_PORT, d, live::PASSWORD);
    t.send_body_and_header(&reuse, "one", "CamelFileName", "1.txt").unwrap();
    t.send_body_and_header(&reuse, "two", "CamelFileName", "2.txt").unwrap();
    let mut chk = client();
    assert_eq!(chk.retr(&format!("{}/reuse/2.txt", d)).unwrap(), b"two");
    // untrusted server certificate
    let bad = format!("ftps://{}@localhost:{}/{}/out?password={}&passiveMode=true&sslContextParameters=#liveUntrusted&maximumReconnectAttempts=0", USER, PORT, d, live::PASSWORD);
    assert!(t.send_body_and_header(&bad, "x", "CamelFileName", "x.txt").is_err());
    ctx.stop().unwrap();
    remove_tree(&mut chk, &d);
    chk.quit();
}
