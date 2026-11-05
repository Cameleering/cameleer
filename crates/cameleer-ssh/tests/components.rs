//! `sftp` and `ssh` components against the embedded server.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_ssh::testing::{temp_dir, SshServer, SshServerConfig};
use cameleer_ssh::SshPrivateKey;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_ssh::register(&ctx);
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

struct Fixture {
    root: PathBuf,
    dir: PathBuf,
    server: SshServer,
    key_file: PathBuf,
    known_hosts: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = temp_dir(name);
        let root = dir.join("root");
        fs::create_dir_all(&root).unwrap();
        let key = SshPrivateKey::generate_ed25519();
        let key_file = dir.join("id_ed25519");
        fs::write(&key_file, key.to_openssh_pem("test").unwrap()).unwrap();
        let server = SshServer::start(SshServerConfig::default().user("alice", "wonderland").authorized_key("alice", key.public_key()).root(&root)).unwrap();
        let known_hosts = dir.join("known_hosts");
        fs::write(&known_hosts, server.known_hosts_text("127.0.0.1")).unwrap();
        Fixture { root, dir, server, key_file, known_hosts }
    }

    /// Base URI with strict host key checking against the server's known_hosts.
    fn uri(&self, scheme: &str, path: &str, auth: &str) -> String {
        format!(
            "{}://alice@127.0.0.1:{}{}?{}&knownHostsFile={}&strictHostKeyChecking=yes&useUserKnownHostsFile=false",
            scheme,
            self.server.port(),
            path,
            auth,
            self.known_hosts.display()
        )
    }

    fn password_uri(&self, scheme: &str, path: &str) -> String {
        self.uri(scheme, path, "password=wonderland")
    }

    fn key_uri(&self, scheme: &str, path: &str) -> String {
        self.uri(scheme, path, &format!("privateKeyFile={}", self.key_file.display()))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.stop();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

#[test]
fn sftp_consumer_move_and_include() {
    let f = Fixture::new("consume-move");
    write(&f.root, "inbox/a.txt", "Hello A");
    write(&f.root, "inbox/b.txt", "Hello B");
    write(&f.root, "inbox/c.log", "skip me");
    let ctx = context();
    let uri = format!("{}&antInclude=*.txt&move=done&initialDelay=0&delay=50", f.password_uri("sftp", "/inbox"));
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
    assert_eq!(ex.header("CamelFileNameConsumed").unwrap().to_string(), "a.txt");
    assert_eq!(ex.header("CamelFileLength").unwrap().as_i64(), Some(7));
    assert_eq!(ex.header("CamelFileAbsolutePath").unwrap().to_string(), "inbox/a.txt");
    assert_eq!(ex.header("CamelFileParent").unwrap().to_string(), "inbox");
    assert!(ex.header("CamelFileLastModified").unwrap().as_i64().unwrap() > 0);
    assert!(wait_until(|| f.root.join("inbox/done/b.txt").exists() && !f.root.join("inbox/b.txt").exists()));
    assert_eq!(fs::read_to_string(f.root.join("inbox/done/a.txt")).unwrap(), "Hello A");
    assert!(f.root.join("inbox/c.log").exists());
    ctx.stop().unwrap();
}

#[test]
fn sftp_consumer_delete_recursive_with_private_key() {
    let f = Fixture::new("consume-delete");
    write(&f.root, "in/one.csv", "1\n");
    write(&f.root, "in/sub/two.csv", "2\n");
    write(&f.root, "in/sub/deeper/three.csv", "3\n");
    let ctx = context();
    let uri = format!("{}&recursive=true&maxDepth=2&delete=true&initialDelay=0&delay=50", f.key_uri("sftp", "/in"));
    ctx.add_routes(|b| {
        b.from(&uri).convert_body_to("String").to("mock:result");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:result").unwrap();
    mock.expected_bodies_received_in_any_order(vec!["1\n", "2\n"]);
    mock.expected_header_values_received_in_any_order("CamelFileName", vec!["one.csv", "sub/two.csv"]);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    assert!(wait_until(|| !f.root.join("in/one.csv").exists() && !f.root.join("in/sub/two.csv").exists()));
    assert!(f.root.join("in/sub/deeper/three.csv").exists(), "beyond maxDepth");
    assert!(f.server.auth_log().iter().all(|l| l.starts_with("alice:publickey")), "{:?}", f.server.auth_log());
    ctx.stop().unwrap();
}

#[test]
fn sftp_consumer_noop_idempotent_sort_and_failures() {
    let f = Fixture::new("consume-noop");
    write(&f.root, "box/a.txt", "a");
    write(&f.root, "box/b.txt", "b");
    write(&f.root, "box/c.txt", "c");
    write(&f.root, "bad/x.txt", "x");
    write(&f.root, "pre/p.txt", "p");
    let ctx = context();
    let noop = format!("{}&noop=true&sortBy=reverse:file:name&maxMessagesPerPoll=2&initialDelay=0&delay=20", f.password_uri("sftp", "/box"));
    let failing = format!("{}&moveFailed=.error&move=.ok&initialDelay=0&delay=50", f.password_uri("sftp", "/bad"));
    let pre = format!("{}&preMove=work&delete=true&initialDelay=0&delay=50", f.password_uri("sftp", "/pre"));
    ctx.add_routes(|b| {
        b.from(&noop).convert_body_to("String").to("mock:noop");
        b.from(&failing).process(|_ex: &mut Exchange| Err(CamelError::illegal_state("boom")));
        b.from(&pre).convert_body_to("String").to("mock:pre");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:noop").unwrap();
    mock.expected_bodies_received(vec!["c", "b", "a"]);
    let pre_mock = ctx.mock_endpoint("mock:pre").unwrap();
    pre_mock.expected_bodies_received(vec!["p"]);
    pre_mock.expected_header_received("CamelFileAbsolutePath", "pre/work/p.txt");
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    pre_mock.assert_is_satisfied().unwrap();
    // idempotent: no duplicates on later polls, and the files stay in place
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(mock.received_counter(), 3);
    assert!(f.root.join("box/a.txt").exists() && f.root.join("box/c.txt").exists());
    assert!(wait_until(|| f.root.join("bad/.error/x.txt").exists()));
    assert!(!f.root.join("bad/.ok/x.txt").exists());
    assert!(wait_until(|| !f.root.join("pre/work/p.txt").exists()));
    ctx.stop().unwrap();
}

#[test]
fn sftp_producer_file_exist_strategies() {
    let f = Fixture::new("produce");
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = f.password_uri("sftp", "/outbox");
    let target = f.root.join("outbox/sub/greeting.txt");

    let ex = t
        .request(&base, |ex| {
            ex.set_body("Hello");
            ex.set_header("CamelFileName", "sub/greeting.txt");
        })
        .unwrap();
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(fs::read_to_string(&target).unwrap(), "Hello");
    assert_eq!(ex.header("CamelFileNameProduced").unwrap().to_string(), "outbox/sub/greeting.txt");
    // Fail
    let err = t.send_body_and_header(&format!("{}&fileExist=Fail", base), "Again", "CamelFileName", "sub/greeting.txt").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "GenericFileOperationFailedException");
    assert!(cause.message.contains("File already exist"), "{}", cause.message);
    // Ignore
    t.send_body_and_header(&format!("{}&fileExist=Ignore", base), "Ignored", "CamelFileName", "sub/greeting.txt").unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "Hello");
    // Append
    t.send_body_and_header(&format!("{}&fileExist=Append", base), " World", "CamelFileName", "sub/greeting.txt").unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "Hello World");
    // Override through a temporary file
    t.send_body_and_header(&format!("{}&tempPrefix=.inprogress-", base), "Replaced", "CamelFileName", "sub/greeting.txt").unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "Replaced");
    assert!(!f.root.join("outbox/sub/.inprogress-greeting.txt").exists());
    // TryRename with tempFileName onto an existing target fails (SFTP rename does not overwrite)
    assert!(t.send_body_and_header(&format!("{}&fileExist=TryRename&tempFileName=${{file:name}}.tmp", base), "T", "CamelFileName", "sub/greeting.txt").is_err());
    // Move the existing file away
    t.send_body_and_header(&format!("{}&fileExist=Move&moveExisting=old", base), "Newest", "CamelFileName", "sub/greeting.txt").unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "Newest");
    assert_eq!(fs::read_to_string(f.root.join("outbox/old/sub/greeting.txt")).unwrap(), "Replaced");
    // fileName option, doneFileName, chmod, disconnect
    t.send_body(&format!("{}&fileName=report-${{exchangeId}}.txt&doneFileName=${{file:name}}.done&chmod=640&disconnect=true", base), "R").unwrap();
    let reports: Vec<_> = fs::read_dir(f.root.join("outbox")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.starts_with("report-")).collect();
    assert_eq!(reports.len(), 2, "{:?}", reports);
    let report = reports.iter().find(|n| !n.ends_with(".done")).unwrap();
    assert!(f.root.join("outbox").join(format!("{}.done", report)).exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(f.root.join("outbox").join(report)).unwrap().permissions().mode() & 0o777, 0o640);
    }
    // binary bodies and the message id as file name
    t.send_body(&format!("{}&fileName=bin.dat", base), Value::Bytes(vec![0, 1, 2, 255])).unwrap();
    assert_eq!(fs::read(f.root.join("outbox/bin.dat")).unwrap(), vec![0, 1, 2, 255]);
    t.send_body(&base, "anonymous").unwrap();
    assert_eq!(fs::read_dir(f.root.join("outbox")).unwrap().count(), 6);
    // null body
    assert!(t.send_body(&format!("{}&fileName=null.txt", base), Value::Null).is_err());
    ctx.stop().unwrap();
}

#[test]
fn sftp_round_trip_route_and_errors() {
    let f = Fixture::new("roundtrip");
    let ctx = context();
    let from = format!("{}&delete=true&initialDelay=0&delay=50", f.password_uri("sftp", "/in"));
    let to = f.password_uri("sftp", "/out");
    ctx.add_routes(|b| {
        b.from("direct:start").to(&to);
        b.from(&from).convert_body_to("String").to("mock:in");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:in").unwrap();
    mock.expected_bodies_received(vec!["ping"]);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", "pong", "CamelFileName", "x.txt").unwrap();
    assert_eq!(fs::read_to_string(f.root.join("out/x.txt")).unwrap(), "pong");
    write(&f.root, "in/y.txt", "ping");
    mock.assert_is_satisfied().unwrap();
    // wrong password / unknown host key give clear errors
    let bad = format!("sftp://alice@127.0.0.1:{}/out?password=nope&useUserKnownHostsFile=false&maximumReconnectAttempts=0", f.server.port());
    let err = t.send_body_and_header(&bad, "x", "CamelFileName", "z.txt").unwrap_err();
    let msg = format!("{} {:?}", err, err.cause);
    assert!(msg.contains("Authentication failed"), "{}", msg);
    let strict = format!("sftp://alice@127.0.0.1:{}/out?password=wonderland&strictHostKeyChecking=yes&useUserKnownHostsFile=false", f.server.port());
    let err = t.send_body_and_header(&strict, "x", "CamelFileName", "z.txt").unwrap_err();
    let msg = format!("{} {:?}", err, err.cause);
    assert!(msg.contains("Host key verification failed"), "{}", msg);
    assert!(!f.root.join("out/z.txt").exists());
    ctx.stop().unwrap();
}

#[test]
fn ssh_producer_and_consumer() {
    let f = Fixture::new("ssh");
    write(&f.root, "data.txt", "remote data\n");
    let ctx = context();
    let poll = format!("{}&pollCommand=echo tick&initialDelay=0&delay=50", f.password_uri("ssh", ""));
    ctx.add_routes(|b| {
        b.from(&poll).to("mock:poll");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:poll").unwrap();
    mock.expected_minimum_message_count(2);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    let first = mock.exchange(0).unwrap();
    assert_eq!(first.message.body.to_string(), "tick\n");
    assert_eq!(first.header("CamelSshExitValue").unwrap().as_i64(), Some(0));

    let t = ctx.create_producer_template();
    let uri = f.key_uri("ssh", "");
    let ex = t.request(&uri, |ex| ex.set_body("echo hello world")).unwrap();
    assert_eq!(ex.message.body, Value::String("hello world\n".into()));
    assert_eq!(ex.header("CamelSshExitValue").unwrap().as_i64(), Some(0));
    assert_eq!(ex.header("CamelSshStderr").unwrap().to_string(), "");
    let ex = t.request(&uri, |ex| ex.set_body("cat data.txt nope.txt")).unwrap();
    assert_eq!(ex.message.body.to_string(), "remote data\n");
    assert_eq!(ex.header("CamelSshStderr").unwrap().to_string(), "cat: nope.txt: No such file or directory\n");
    assert_eq!(ex.header("CamelSshExitValue").unwrap().as_i64(), Some(1));
    // the pooled connection is reused
    let before = f.server.auth_log().len();
    for i in 0..3 {
        assert_eq!(t.request_body(&uri, format!("exit {}", i)).unwrap().to_string(), "");
    }
    assert_eq!(f.server.auth_log().len(), before);
    // shell channel
    let shell = format!("{}&channelType=shell", f.password_uri("ssh", ""));
    assert_eq!(t.request_body(&shell, "echo from shell\npwd").unwrap().to_string(), "from shell\n/\n");
    // credentials from headers
    let anon = format!("ssh://127.0.0.1:{}?useUserKnownHostsFile=false", f.server.port());
    let r = t.request_body_and_headers(&anon, "echo hdr", vec![("CamelSshUsername", Value::from("alice")), ("CamelSshPassword", Value::from("wonderland"))]).unwrap();
    assert_eq!(r.to_string(), "hdr\n");
    assert!(t.request_body_and_headers(&anon, "echo hdr", vec![("CamelSshUsername", Value::from("alice")), ("CamelSshPassword", Value::from("bad"))]).is_err());
    // failOnUnknownHost
    let unknown = format!("ssh://alice:wonderland@127.0.0.1:{}?failOnUnknownHost=true", f.server.port());
    assert!(t.request_body(&unknown, "echo x").is_err());
    let known = format!("ssh://alice:wonderland@127.0.0.1:{}?failOnUnknownHost=true&knownHostsResource=file:{}", f.server.port(), f.known_hosts.display());
    assert_eq!(t.request_body(&known, "echo x").unwrap().to_string(), "x\n");
    ctx.stop().unwrap();
    assert!(f.server.commands().contains(&"echo tick".to_string()));
    // a consumer without pollCommand is rejected
    let ctx2 = context();
    let no_cmd = f.password_uri("ssh", "");
    let r = ctx2.add_routes(|b| {
        b.from(&no_cmd).to("mock:x");
    });
    assert!(r.is_err() || ctx2.start().is_err());
}

#[test]
fn register_does_not_override_and_override_does() {
    let ctx = CamelContext::new();
    cameleer_ssh::register(&ctx);
    assert!(ctx.has_component("sftp") && ctx.has_component("ssh"));
    let ctx = CamelContext::new();
    cameleer_ssh::register_override(&ctx);
    assert!(ctx.component::<cameleer_ssh::component::sftp::SftpComponent>("sftp").is_some());
}
