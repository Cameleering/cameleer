use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use camel_core::prelude::*;
use camel_protocols::ftp::FtpClient;
use camel_protocols::testing::FtpServer;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("camel-ftp-{}-{}-{}", name, std::process::id(), camel_core::util_time::now_millis()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_protocols::register(&ctx);
    ctx
}

fn wait_until(f: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn client_basic_operations_passive_and_active() {
    let root = temp_dir("client");
    let server = FtpServer::start_with_users(&root, &[("admin", "secret")]).unwrap();
    for passive in [true, false] {
        let mut c = FtpClient::connect("127.0.0.1", server.port(), Duration::from_secs(5), Duration::from_secs(5)).unwrap();
        assert!(c.login("admin", "wrong").is_err());
        c.login("admin", "secret").unwrap();
        c.set_passive(passive);
        c.set_binary(true).unwrap();
        assert_eq!(c.pwd().unwrap(), "/");
        c.mkdirs("a/b").unwrap();
        c.stor("a/b/x.bin", &[0, 1, 2, 13, 10, 255], false).unwrap();
        c.stor("a/b/x.bin", b"!", true).unwrap();
        assert_eq!(c.retr("a/b/x.bin").unwrap(), vec![0, 1, 2, 13, 10, 255, b'!']);
        assert_eq!(c.size("a/b/x.bin").unwrap(), Some(7));
        assert!(c.mdtm("a/b/x.bin").unwrap().is_some());
        let l = c.list("a/b").unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].name.as_str(), l[0].size, l[0].is_dir), ("x.bin", 7, false));
        assert_eq!(c.nlst("a").unwrap(), vec!["b".to_string()]);
        c.cwd("a").unwrap();
        c.rename("b/x.bin", "y.bin").unwrap();
        assert!(c.exists("y.bin").unwrap());
        // ASCII mode converts line endings on the wire and back
        c.set_binary(false).unwrap();
        c.stor("text.txt", b"l1\nl2\n", false).unwrap();
        assert_eq!(c.retr("text.txt").unwrap(), b"l1\nl2\n");
        c.dele("text.txt").unwrap();
        c.dele("y.bin").unwrap();
        c.cdup().unwrap();
        c.rmd("a/b").unwrap();
        c.rmd("a").unwrap();
        assert!(c.dele("missing").is_err());
        c.quit();
    }
    assert!(!root.join("a").exists());
    assert!(server.commands().iter().any(|c| c.starts_with("PORT ")));
    assert!(server.commands().iter().any(|c| c == "PASV" || c == "EPSV"));
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn consumer_with_move_and_include() {
    let root = temp_dir("move");
    fs::create_dir_all(root.join("inbox")).unwrap();
    fs::write(root.join("inbox/a.txt"), "Hello A").unwrap();
    fs::write(root.join("inbox/b.txt"), "Hello B").unwrap();
    fs::write(root.join("inbox/c.log"), "skip me").unwrap();
    let server = FtpServer::start_with_users(&root, &[("admin", "secret")]).unwrap();
    let ctx = context();
    let uri = format!(
        "ftp://admin@127.0.0.1:{}/inbox?password=secret&passiveMode=true&binary=true&antInclude=*.txt&move=done&initialDelay=0&delay=50",
        server.port()
    );
    ctx.add_routes(|b| {
        b.from(&uri).convert_body_to("String").to("mock:result");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:result").unwrap();
    mock.expected_bodies_received(vec!["Hello A", "Hello B"]);
    mock.expected_header_received("CamelFileHost", "127.0.0.1");
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    let ex = mock.exchange(0).unwrap();
    assert_eq!(ex.header("CamelFileName").unwrap().to_string(), "a.txt");
    assert_eq!(ex.header("CamelFileNameOnly").unwrap().to_string(), "a.txt");
    assert_eq!(ex.header("CamelFileLength").unwrap().as_i64(), Some(7));
    assert_eq!(ex.header("CamelFileAbsolutePath").unwrap().to_string(), "inbox/a.txt");
    assert!(ex.header("CamelFileLastModified").unwrap().as_i64().unwrap() > 0);
    assert!(wait_until(|| root.join("inbox/done/b.txt").exists() && !root.join("inbox/b.txt").exists()));
    assert_eq!(fs::read_to_string(root.join("inbox/done/a.txt")).unwrap(), "Hello A");
    assert!(root.join("inbox/c.log").exists());
    ctx.stop().unwrap();
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn consumer_delete_active_mode_and_recursive() {
    let root = temp_dir("delete");
    fs::create_dir_all(root.join("in/sub")).unwrap();
    fs::write(root.join("in/one.csv"), "1\n").unwrap();
    fs::write(root.join("in/sub/two.csv"), "2\n").unwrap();
    let server = FtpServer::start(&root).unwrap();
    let ctx = context();
    let uri = format!("ftp://bob@127.0.0.1:{}/in?password=x&recursive=true&delete=true&stepwise=false&initialDelay=0&delay=50", server.port());
    ctx.add_routes(|b| {
        b.from(&uri).convert_body_to("String").to("mock:result");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:result").unwrap();
    mock.expected_bodies_received_in_any_order(vec!["1\n", "2\n"]);
    mock.expected_header_values_received_in_any_order("CamelFileName", vec!["one.csv", "sub/two.csv"]);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    assert!(wait_until(|| !root.join("in/one.csv").exists() && !root.join("in/sub/two.csv").exists()));
    // active mode was used (passiveMode defaults to false)
    assert!(server.commands().iter().any(|c| c.starts_with("PORT ")));
    ctx.stop().unwrap();
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn consumer_noop_is_idempotent() {
    let root = temp_dir("noop");
    fs::write(root.join("keep.txt"), "keep").unwrap();
    let server = FtpServer::start(&root).unwrap();
    let ctx = context();
    let uri = format!("ftp://127.0.0.1:{}?noop=true&passiveMode=true&initialDelay=0&delay=20", server.port());
    ctx.add_routes(|b| {
        b.from(&uri).to("mock:result");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:result").unwrap();
    mock.expected_message_count(1);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    // several more polls happen; the file must not be consumed again
    let n = server.commands().iter().filter(|c| c.starts_with("MLSD")).count();
    assert!(wait_until(|| server.commands().iter().filter(|c| c.starts_with("MLSD")).count() >= n + 3));
    assert_eq!(mock.received_counter(), 1);
    assert!(root.join("keep.txt").exists());
    ctx.stop().unwrap();
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn producer_file_exist_strategies() {
    let root = temp_dir("produce");
    let server = FtpServer::start(&root).unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = format!("ftp://user@127.0.0.1:{}/outbox?password=pw&passiveMode=true", server.port());
    let target = root.join("outbox/sub/greeting.txt");

    let ex = t.request(&base, |ex| {
        ex.set_body("Hello");
        ex.set_header("CamelFileName", "sub/greeting.txt");
    })
    .unwrap();
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(fs::read_to_string(&target).unwrap(), "Hello");
    assert_eq!(ex.header("CamelFileNameProduced").unwrap().to_string(), "outbox/sub/greeting.txt");
    assert_eq!(ex.header("CamelFtpReplyCode").unwrap().as_i64(), Some(226));

    // Fail
    let err = t.send_body_and_header(&format!("{}&fileExist=Fail", base), "Again", "CamelFileName", "sub/greeting.txt").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "GenericFileOperationFailedException");
    assert!(cause.message.contains("File already exist"), "{}", cause.message);
    // Ignore
    t.send_body_and_header(&format!("{}&fileExist=Ignore", base), "Ignored", "CamelFileName", "sub/greeting.txt").unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "Hello");
    // Append
    t.send_body_and_header(&format!("{}&fileExist=Append&binary=true", base), " World", "CamelFileName", "sub/greeting.txt").unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "Hello World");
    // Override through a temporary file
    t.send_body_and_header(&format!("{}&tempPrefix=.inprogress-", base), "Replaced", "CamelFileName", "sub/greeting.txt").unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "Replaced");
    assert!(!root.join("outbox/sub/.inprogress-greeting.txt").exists());
    // Move the existing file away
    t.send_body_and_header(&format!("{}&fileExist=Move&moveExisting=old", base), "Newest", "CamelFileName", "sub/greeting.txt").unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "Newest");
    assert_eq!(fs::read_to_string(root.join("outbox/old/sub/greeting.txt")).unwrap(), "Replaced");
    // fileName option with the file language, and stepwise=false + disconnect
    t.send_body(&format!("{}&fileName=report-${{exchangeId}}.txt&stepwise=false&disconnect=true", base), "R").unwrap();
    let reports: Vec<_> = fs::read_dir(root.join("outbox")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("report-")).collect();
    assert_eq!(reports.len(), 1);
    // no file name: the message id is used
    t.send_body(&base, "anonymous").unwrap();
    assert_eq!(fs::read_dir(root.join("outbox")).unwrap().count(), 4);
    ctx.stop().unwrap();
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ftps_and_sftp_are_not_supported() {
    let ctx = context();
    for uri in ["sftp://host/dir?password=x", "ftps://host/dir"] {
        let err = ctx.get_endpoint(uri).err().expect("must fail");
        assert!(err.to_string().contains("not supported"), "{}", err);
    }
}
