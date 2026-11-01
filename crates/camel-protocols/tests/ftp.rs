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
fn sftp_is_not_supported_and_ftps_resolves() {
    let ctx = context();
    let err = ctx.get_endpoint("sftp://host/dir?password=x").err().expect("must fail");
    assert!(err.to_string().contains("not supported"), "{}", err);
    // ftps is supported (end-to-end tests in tests/tls.rs); options are validated
    assert!(ctx.get_endpoint("ftps://host/dir?isImplicit=true&execProt=P").is_ok());
    assert!(ctx.get_endpoint("ftps://host/dir?execProt=X").is_err());
}

#[test]
fn read_lock_strategies() {
    let root = temp_dir("readlock");
    let server = FtpServer::start_with_users(&root, &[("admin", "secret")]).unwrap();
    fs::create_dir_all(root.join("in")).unwrap();
    fs::write(root.join("in/stable.txt"), "done").unwrap();
    let ctx = context();
    let base = format!("ftp://admin@127.0.0.1:{}/in?password=secret&passiveMode=true&delete=true&initialDelay=0&delay=50", server.port());
    let changed = format!("{}&readLock=changed&readLockCheckInterval=100&readLockTimeout=1500", base);
    let c2 = changed.clone();
    ctx.add_routes(move |b| {
        b.from(&c2).to("mock:changed");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:changed").unwrap();
    assert!(wait_until(|| mock.received_counter() == 1));
    assert!(server.commands().iter().any(|c| c.starts_with("MDTM")));
    // a growing file is only picked up once it stops changing
    let growing = root.join("in/growing.txt");
    fs::write(&growing, "a").unwrap();
    let g = growing.clone();
    let writer = std::thread::spawn(move || {
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(60));
            let mut d = fs::read(&g).unwrap_or_default();
            d.push(b'a');
            let _ = fs::write(&g, d);
        }
    });
    writer.join().unwrap();
    assert!(wait_until(|| mock.received_counter() == 2));
    let body = mock.received_exchanges()[1].body().to_bytes();
    assert_eq!(body, b"aaaaaaaaa".to_vec(), "the whole file is read once stable");
    ctx.stop().unwrap();

    // rename strategy
    fs::write(root.join("in/r.txt"), "r").unwrap();
    let ctx = context();
    let rename = format!("{}&readLock=rename", base);
    ctx.add_routes(move |b| {
        b.from(&rename).to("mock:rename");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:rename").unwrap();
    assert!(wait_until(|| mock.received_counter() == 1));
    assert!(server.commands().iter().any(|c| c.contains("camelExclusiveReadLock")));
    ctx.stop().unwrap();
    assert!(context().get_endpoint(&format!("{}&readLock=fileLock", base)).is_err());
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resume_download_local_work_directory_and_stream_download() {
    let root = temp_dir("resume");
    let work = temp_dir("resume-work");
    fs::create_dir_all(root.join("big")).unwrap();
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    fs::write(root.join("big/data.bin"), &data).unwrap();
    // a previous, interrupted transfer left the first 100000 bytes
    fs::write(work.join("data.bin.inprogress"), &data[..100_000]).unwrap();
    let server = FtpServer::start_with_users(&root, &[("admin", "secret")]).unwrap();

    // client level: REST + streamed RETR
    let mut c = FtpClient::connect("127.0.0.1", server.port(), Duration::from_secs(5), Duration::from_secs(5)).unwrap();
    c.login("admin", "secret").unwrap();
    c.set_passive(true);
    c.set_binary(true).unwrap();
    let mut out = Vec::new();
    assert_eq!(c.retr_to("big/data.bin", &mut out, 299_990).unwrap(), 10);
    assert_eq!(out, &data[299_990..]);
    c.quit();

    let ctx = context();
    let uri = format!(
        "ftp://admin@127.0.0.1:{}/big?password=secret&passiveMode=true&binary=true&noop=true&localWorkDirectory={}&resumeDownload=true&initialDelay=0&delay=50",
        server.port(),
        work.display()
    );
    let sdir = temp_dir("stream-src");
    fs::write(sdir.join("s.txt"), "streamed body").unwrap();
    let server2 = FtpServer::start_with_users(&sdir, &[("admin", "secret")]).unwrap();
    let suri = format!("ftp://admin@127.0.0.1:{}/?password=secret&passiveMode=true&binary=true&noop=true&streamDownload=true&initialDelay=0&delay=50", server2.port());
    ctx.add_routes(|b| {
        b.from(&uri).to("mock:resumed");
        b.from(&suri)
            .process(|ex: &mut Exchange| {
                assert!(camel_tls::stream_body::is_available(&ex.message.body));
                let bytes = camel_tls::stream_body::body_bytes(&ex.message.body).unwrap();
                ex.set_body(Value::Bytes(bytes));
                Ok(())
            })
            .convert_body_to("String")
            .to("mock:streamed");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:resumed").unwrap();
    mock.expected_message_count(1);
    let smock = ctx.mock_endpoint("mock:streamed").unwrap();
    smock.expected_bodies_received(vec!["streamed body"]);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    smock.assert_is_satisfied().unwrap();
    let ex = mock.exchange(0).unwrap();
    assert_eq!(ex.body().to_bytes(), data);
    assert_eq!(ex.header("CamelFtpResumedFrom").unwrap().as_i64(), Some(100_000));
    assert!(server.commands().iter().any(|c| c == "REST 100000"), "{:?}", server.commands());
    let local = ex.header("CamelFileLocalWorkPath").unwrap().to_string();
    assert!(local.ends_with("data.bin"));
    // local work files are removed after the exchange
    assert!(wait_until(|| !std::path::Path::new(&local).exists() && !work.join("data.bin.inprogress").exists()));
    // the stream's temp file is gone too
    let sex = smock.exchange(0).unwrap();
    let tmp = sex.header("CamelFileLocalWorkPath").unwrap().to_string();
    assert!(wait_until(|| !std::path::Path::new(&tmp).exists()));
    ctx.stop().unwrap();
    server.stop();
    server2.stop();
    // resumeDownload requires localWorkDirectory
    let ctx = context();
    assert!(ctx.get_endpoint("ftp://admin@127.0.0.1:21/big?resumeDownload=true").is_err());
    for d in [root, work, sdir] {
        let _ = fs::remove_dir_all(d);
    }
}
