use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use camel_core::component::file::{GenericFileFilter, FileInfo};
use camel_core::prelude::*;

/// A unique temp directory removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "rust-camel-file-{}-{}-{}-{}",
            tag,
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst),
            camel_core::util_time::now_millis()
        ));
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn uri(&self, sub: &str) -> String {
        self.0.join(sub).display().to_string()
    }
    fn write(&self, rel: &str, content: &str) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.0.join(rel)).unwrap()
    }
    fn exists(&self, rel: &str) -> bool {
        self.0.join(rel).exists()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const FAST: &str = "initialDelay=0&delay=10";

fn wait_until(f: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    f()
}

#[test]
fn consumer_moves_to_dot_camel_by_default_and_sets_headers() {
    let dir = TempDir::new("move");
    dir.write("in/hello.txt", "Hello World");
    let ctx = CamelContext::new();
    let uri = format!("file:{}?{}", dir.uri("in"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:files");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:files").unwrap();
    mock.expected_bodies_received(vec!["Hello World"]);
    mock.assert_is_satisfied().unwrap();
    let ex = mock.exchange(0).unwrap();
    assert!(matches!(ex.body(), Value::Bytes(_)));
    assert_eq!(ex.body_as::<String>().unwrap(), "Hello World");
    assert_eq!(ex.header("CamelFileName").unwrap(), &Value::from("hello.txt"));
    assert_eq!(ex.header("CamelFileNameOnly").unwrap(), &Value::from("hello.txt"));
    assert_eq!(ex.header("CamelFileLength").unwrap().as_i64(), Some(11));
    assert_eq!(ex.header("CamelFileAbsolute").unwrap(), &Value::Bool(true));
    assert!(ex.header("CamelFileAbsolutePath").unwrap().to_string().ends_with("hello.txt"));
    assert!(ex.header("CamelFileParent").unwrap().to_string().ends_with("in"));
    assert!(ex.header("CamelFileLastModified").unwrap().as_i64().unwrap() > 0);
    assert!(wait_until(|| dir.exists("in/.camel/hello.txt")));
    assert!(!dir.exists("in/hello.txt"));
    ctx.stop().unwrap();
}

#[test]
fn consumer_noop_is_idempotent() {
    let dir = TempDir::new("noop");
    dir.write("in/a.txt", "A");
    let ctx = CamelContext::new();
    let uri = format!("file:{}?noop=true&{}", dir.uri("in"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:noop");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:noop").unwrap();
    mock.expected_message_count(1);
    mock.set_assert_period(Duration::from_millis(80));
    mock.assert_is_satisfied().unwrap();
    assert!(dir.exists("in/a.txt"));
    assert!(!dir.exists("in/.camel"));
    ctx.stop().unwrap();
}

#[test]
fn consumer_delete() {
    let dir = TempDir::new("delete");
    dir.write("in/a.txt", "A");
    dir.write("in/b.txt", "B");
    let ctx = CamelContext::new();
    let uri = format!("file:{}?delete=true&sortBy=file:name&{}", dir.uri("in"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).convert_body_to_string_for_test().to("mock:del");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:del").unwrap();
    mock.expected_bodies_received(vec!["A", "B"]);
    mock.assert_is_satisfied().unwrap();
    assert!(wait_until(|| !dir.exists("in/a.txt") && !dir.exists("in/b.txt")));
    assert!(!dir.exists("in/.camel"));
    ctx.stop().unwrap();
}

trait ConvertForTest {
    fn convert_body_to_string_for_test(&mut self) -> &mut Self;
}
impl ConvertForTest for RouteDefinition {
    fn convert_body_to_string_for_test(&mut self) -> &mut Self {
        self.process(|ex: &mut Exchange| {
            let s: String = ex.body_as()?;
            ex.set_body(s);
            Ok(())
        })
    }
}

#[test]
fn consumer_include_exclude_and_ant() {
    let dir = TempDir::new("include");
    for f in ["a.txt", "b.xml", "c.txt", "skip.txt", ".hidden.txt"] {
        dir.write(&format!("in/{}", f), f);
    }
    let ctx = CamelContext::new();
    let uri = format!("file:{}?noop=true&include=.*\\.txt&exclude=skip.*&sortBy=file:name&{}", dir.uri("in"), FAST);
    let uri2 = format!("file:{}?noop=true&antInclude=**/*.xml&{}", dir.uri("in"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).convert_body_to_string_for_test().to("mock:inc");
        b.from(&uri2).convert_body_to_string_for_test().to("mock:ant");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:inc").unwrap();
    mock.expected_bodies_received(vec!["a.txt", "c.txt"]);
    let ant = ctx.mock_endpoint("mock:ant").unwrap();
    ant.expected_bodies_received(vec!["b.xml"]);
    mock.set_assert_period(Duration::from_millis(50));
    MockEndpoint::assert_all_satisfied(&[&mock, &ant]).unwrap();
    ctx.stop().unwrap();
}

#[test]
fn consumer_recursive_with_depth_and_relative_names() {
    let dir = TempDir::new("recursive");
    dir.write("in/top.txt", "top");
    dir.write("in/sub/mid.txt", "mid");
    dir.write("in/sub/deep/low.txt", "low");
    let ctx = CamelContext::new();
    let all = format!("file:{}?noop=true&recursive=true&sortBy=file:name&{}", dir.uri("in"), FAST);
    let depth = format!("file:{}?noop=true&recursive=true&minDepth=2&maxDepth=2&{}", dir.uri("in"), FAST);
    let flat = format!("file:{}?noop=true&{}", dir.uri("in"), FAST);
    ctx.add_routes(move |b| {
        b.from(&all).to("mock:all");
        b.from(&depth).to("mock:depth");
        b.from(&flat).to("mock:flat");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:all").unwrap();
    m.expected_header_values_received_in_any_order("CamelFileName", vec!["top.txt", "sub/mid.txt", "sub/deep/low.txt"]);
    let d = ctx.mock_endpoint("mock:depth").unwrap();
    d.expected_header_values_received_in_any_order("CamelFileName", vec!["sub/mid.txt"]);
    let f = ctx.mock_endpoint("mock:flat").unwrap();
    f.expected_header_values_received_in_any_order("CamelFileName", vec!["top.txt"]);
    d.set_assert_period(Duration::from_millis(50));
    MockEndpoint::assert_all_satisfied(&[&m, &d, &f]).unwrap();
    let mid = m.received_exchanges().into_iter().find(|e| e.header("CamelFileName").unwrap().to_string() == "sub/mid.txt").unwrap();
    assert_eq!(mid.header("CamelFileNameOnly").unwrap(), &Value::from("mid.txt"));
    ctx.stop().unwrap();
}

#[test]
fn consumer_move_expression_and_move_failed() {
    let dir = TempDir::new("movefailed");
    dir.write("in/good.txt", "good");
    dir.write("in/bad.txt", "bad");
    let ctx = CamelContext::new();
    let uri = format!("file:{}?move=done/${{file:name.noext}}.bak&moveFailed=error&{}", dir.uri("in"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri)
            .process(|ex: &mut Exchange| {
                if ex.body_as::<String>()? == "bad" {
                    return Err(CamelError::illegal_argument("bad file"));
                }
                Ok(())
            })
            .to("mock:mf");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(wait_until(|| dir.exists("in/done/good.bak") && dir.exists("in/error/bad.txt")));
    assert!(!dir.exists("in/good.txt") && !dir.exists("in/bad.txt"));
    assert_eq!(dir.read("in/done/good.bak"), "good");
    ctx.stop().unwrap();
}

#[test]
fn consumer_failure_without_move_failed_keeps_file() {
    let dir = TempDir::new("rollback");
    dir.write("in/x.txt", "x");
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    let uri = format!("file:{}?{}", dir.uri("in"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).process(move |_: &mut Exchange| {
            a.fetch_add(1, Ordering::SeqCst);
            Err(CamelError::io("boom"))
        });
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(wait_until(|| attempts.load(Ordering::SeqCst) >= 2), "file retried on next poll");
    ctx.stop().unwrap();
    assert!(dir.exists("in/x.txt"));
}

#[test]
fn consumer_pre_move_max_messages_and_file_name() {
    let dir = TempDir::new("premove");
    for i in 0..5 {
        dir.write(&format!("in/f{}.txt", i), &format!("{}", i));
    }
    let ctx = CamelContext::new();
    let uri = format!("file:{}?preMove=inprogress&move=../done&maxMessagesPerPoll=2&delay=200&initialDelay=0&sortBy=file:name", dir.uri("in"));
    let fixed_dir = TempDir::new("fixed");
    fixed_dir.write("a.txt", "a");
    fixed_dir.write("wanted.txt", "wanted");
    let uri2 = format!("file:{}?fileName=wanted.txt&noop=true&{}", fixed_dir.path().display(), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:pre");
        b.from(&uri2).to("mock:fixed");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:pre").unwrap();
    // only 2 per poll: after the first poll there are exactly 2
    assert!(mock.wait_for_messages(2, Duration::from_secs(5)));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(mock.received_counter(), 2);
    let ex = mock.exchange(0).unwrap();
    assert!(ex.header("CamelFileAbsolutePath").unwrap().to_string().contains("inprogress"));
    assert!(mock.wait_for_messages(5, Duration::from_secs(5)));
    assert!(wait_until(|| (0..5).all(|i| dir.exists(&format!("done/f{}.txt", i)))));
    let fixed = ctx.mock_endpoint("mock:fixed").unwrap();
    fixed.expected_header_received("CamelFileName", "wanted.txt");
    fixed.expected_message_count(1);
    fixed.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn consumer_done_file_and_marker_read_lock() {
    let dir = TempDir::new("done");
    dir.write("in/data.csv", "1,2");
    dir.write("in/other.csv", "3,4");
    dir.write("in/data.csv.done", "");
    let ctx = CamelContext::new();
    let uri = format!("file:{}?doneFileName=${{file:name}}.done&readLock=markerFile&{}", dir.uri("in"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:done");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:done").unwrap();
    mock.expected_header_received("CamelFileName", "data.csv");
    mock.expected_message_count(1);
    mock.set_assert_period(Duration::from_millis(60));
    mock.assert_is_satisfied().unwrap();
    assert!(wait_until(|| !dir.exists("in/data.csv.done")));
    assert!(!dir.exists("in/data.csv.camelLock"));
    assert!(dir.exists("in/other.csv"));
    // now signal the other file
    dir.write("in/other.csv.done", "");
    assert!(mock.wait_for_messages(2, Duration::from_secs(5)));
    ctx.stop().unwrap();
}

#[test]
fn consumer_sort_reverse_and_changed_read_lock() {
    let dir = TempDir::new("sort");
    for n in ["a", "b", "c"] {
        dir.write(&format!("in/{}.txt", n), n);
    }
    let ctx = CamelContext::new();
    let uri = format!("file:{}?sortBy=reverse:file:name&readLock=changed&readLockCheckInterval=5&{}", dir.uri("in"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).convert_body_to_string_for_test().to("mock:sort");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:sort").unwrap();
    mock.expected_bodies_received(vec!["c", "b", "a"]);
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn consumer_custom_filter_and_idempotent_key() {
    let dir = TempDir::new("filter");
    dir.write("in/keep-1.txt", "1");
    dir.write("in/drop-1.txt", "2");
    let ctx = CamelContext::new();
    let filter: Arc<dyn GenericFileFilter> = Arc::new(|f: &FileInfo, _dir: bool| f.only_name().starts_with("keep"));
    ctx.registry().bind("myFilter", filter);
    let uri = format!("file:{}?filter=#myFilter&noop=true&idempotentKey=${{file:onlyname}}&{}", dir.uri("in"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:filter");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:filter").unwrap();
    mock.expected_header_received("CamelFileName", "keep-1.txt");
    mock.expected_message_count(1);
    mock.set_assert_period(Duration::from_millis(50));
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn consumer_starting_directory_must_exist_and_auto_create() {
    let dir = TempDir::new("mustexist");
    let ctx = CamelContext::new();
    let uri = format!("file:{}?startingDirectoryMustExist=true&{}", dir.uri("missing"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:x");
    })
    .unwrap();
    assert!(ctx.start().is_err());
    let ctx = CamelContext::new();
    let uri = format!("file:{}?{}", dir.uri("auto"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:x");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(dir.exists("auto"));
    ctx.stop().unwrap();
}

#[test]
fn consumer_stops_promptly() {
    let dir = TempDir::new("stop");
    let ctx = CamelContext::new();
    let uri = format!("file:{}?initialDelay=60000", dir.uri("in"));
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:x");
    })
    .unwrap();
    ctx.start().unwrap();
    let start = std::time::Instant::now();
    ctx.stop().unwrap();
    assert!(start.elapsed() < Duration::from_millis(500));
}

#[test]
fn producer_writes_with_header_and_file_name_option() {
    let dir = TempDir::new("produce");
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let out = format!("file:{}", dir.uri("out"));
    t.send_body_and_header(&out, "hello", "CamelFileName", "greeting.txt").unwrap();
    assert_eq!(dir.read("out/greeting.txt"), "hello");
    t.send_body_and_header(&out, "nested", "CamelFileName", "a/b/c.txt").unwrap();
    assert_eq!(dir.read("out/a/b/c.txt"), "nested");
    // fileName option with file language based on the incoming name
    let renamed = format!("file:{}?fileName=${{file:name.noext}}.out", dir.uri("out"));
    let ex = t.send_with(&renamed, ExchangePattern::InOnly, |e| {
        e.set_body("x");
        e.set_header("CamelFileName", "input.csv");
    });
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(dir.read("out/input.out"), "x");
    assert!(ex.header("CamelFileNameProduced").unwrap().to_string().ends_with("input.out"));
    // no name: generated message id
    let ex = t.send_with(&out, ExchangePattern::InOnly, |e| e.set_body("anon"));
    let produced = ex.header("CamelFileNameProduced").unwrap().to_string();
    assert_eq!(fs::read_to_string(produced).unwrap(), "anon");
    // jail
    let err = t.send_body_and_header(&out, "x", "CamelFileName", "../escape.txt").unwrap_err();
    assert!(err.cause.unwrap().message.contains("jailed"));
    ctx.stop().unwrap();
}

#[test]
fn producer_file_exist_modes() {
    let dir = TempDir::new("exist");
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = dir.uri("out");
    let send = |opts: &str, body: &str| t.send_body_and_header(&format!("file:{}?{}", base, opts), body, "CamelFileName", "f.txt");
    send("fileExist=Override", "one").unwrap();
    send("fileExist=Override", "two").unwrap();
    assert_eq!(dir.read("out/f.txt"), "two");
    send("fileExist=Append", "+three").unwrap();
    assert_eq!(dir.read("out/f.txt"), "two+three");
    send("fileExist=Ignore", "ignored").unwrap();
    assert_eq!(dir.read("out/f.txt"), "two+three");
    let err = send("fileExist=Fail", "fail").unwrap_err();
    assert!(err.cause.unwrap().message.contains("File already exist"));
    send("fileExist=Move&moveExisting=old", "new").unwrap();
    assert_eq!(dir.read("out/f.txt"), "new");
    assert_eq!(dir.read("out/old/f.txt"), "two+three");
    send("fileExist=TryRename&tempPrefix=tmp-", "renamed").unwrap();
    assert_eq!(dir.read("out/f.txt"), "renamed");
    assert!(!dir.exists("out/tmp-f.txt"));
    // missing moveExisting is a configuration error
    assert!(ctx.get_endpoint(&format!("file:{}?fileExist=Move", base)).is_err());
    ctx.stop().unwrap();
}

#[test]
fn producer_temp_file_append_chars_done_file_and_keep_last_modified() {
    let dir = TempDir::new("temp");
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let uri = format!(
        "file:{}?tempFileName=${{file:onlyname}}.tmp&appendChars=\\n&doneFileName=${{file:name.noext}}.ready&keepLastModified=true",
        dir.uri("out")
    );
    let ex = t.send_with(&uri, ExchangePattern::InOnly, |e| {
        e.set_body("line");
        e.set_header("CamelFileName", "data.txt");
        e.set_header("CamelFileLastModified", 1_000_000_000_000i64);
    });
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(dir.read("out/data.txt"), "line\n");
    assert!(!dir.exists("out/data.txt.tmp"));
    assert!(dir.exists("out/data.ready"));
    let m = fs::metadata(dir.path().join("out/data.txt")).unwrap().modified().unwrap();
    assert_eq!(m.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(), 1_000_000_000_000);
    // null body fails unless allowNullBody
    let err = t.send_body_and_header(&format!("file:{}", dir.uri("out")), Value::Null, "CamelFileName", "n.txt").unwrap_err();
    assert!(err.cause.unwrap().message.contains("null body"));
    t.send_body_and_header(&format!("file:{}?allowNullBody=true", dir.uri("out")), Value::Null, "CamelFileName", "n.txt").unwrap();
    assert_eq!(dir.read("out/n.txt"), "");
    ctx.stop().unwrap();
}

#[test]
fn file_to_file_route_and_polling_consumer() {
    let dir = TempDir::new("route");
    dir.write("in/order.txt", "order-1");
    let ctx = CamelContext::new();
    let (from, to) = (format!("file:{}?{}", dir.uri("in"), FAST), format!("file:{}", dir.uri("out")));
    ctx.add_routes(move |b| {
        b.from(&from).to(&to).to("mock:copied");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.mock_endpoint("mock:copied").unwrap().expected_message_count(1);
    ctx.mock_endpoint("mock:copied").unwrap().assert_is_satisfied().unwrap();
    assert_eq!(dir.read("out/order.txt"), "order-1");
    // ConsumerTemplate polling
    dir.write("poll/p.txt", "polled");
    let ct = ctx.create_consumer_template();
    let ex = ct.receive(&format!("file:{}?noop=true", dir.uri("poll")), Some(Duration::from_secs(2))).unwrap().unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "polled");
    assert!(ct.receive_no_wait(&format!("file:{}?noop=true", dir.uri("empty"))).unwrap().is_none());
    ctx.stop().unwrap();
}

#[test]
fn changed_read_lock_min_age_marker_file_and_growing_files() {
    let dir = TempDir::new("changed");
    dir.write("in/old.txt", "old");
    // another consumer holds the marker file of this one
    dir.write("in/locked.txt", "locked");
    dir.write("in/locked.txt.camelLock", "");
    std::thread::sleep(Duration::from_millis(120));
    let ctx = CamelContext::new();
    // with readLockMinAge an old file is taken at once (no second check 2s later)
    let uri = format!("file:{}?readLock=changed&readLockMinAge=100&readLockCheckInterval=2000&{}", dir.uri("in"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).convert_body_to_string_for_test().to("mock:changed");
    })
    .unwrap();
    let start = std::time::Instant::now();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:changed").unwrap();
    assert!(mock.wait_for_messages(1, Duration::from_millis(1500)));
    assert!(start.elapsed() < Duration::from_millis(1500), "{:?}", start.elapsed());
    assert_eq!(mock.received_bodies(), vec![Value::from("old")]);
    // the marker file of the consumed file is released
    std::thread::sleep(Duration::from_millis(50));
    assert!(!dir.exists("in/old.txt.camelLock"));
    assert!(dir.exists("in/locked.txt"), "a file locked by someone else is left alone");
    ctx.stop().unwrap();

    // without readLockMinAge a file still being written is only taken once it stopped changing
    let dir = TempDir::new("growing");
    dir.write("in/grow.txt", "a");
    let ctx = CamelContext::new();
    let uri = format!("file:{}?readLock=changed&readLockCheckInterval=150&{}", dir.uri("in"), FAST);
    ctx.add_routes(move |b| {
        b.from(&uri).convert_body_to_string_for_test().to("mock:grow");
    })
    .unwrap();
    ctx.start().unwrap();
    for chunk in ["b", "c"] {
        std::thread::sleep(Duration::from_millis(60));
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(dir.path().join("in/grow.txt")).unwrap();
        f.write_all(chunk.as_bytes()).unwrap();
    }
    let mock = ctx.mock_endpoint("mock:grow").unwrap();
    assert!(mock.wait_for_messages(1, Duration::from_secs(5)));
    assert_eq!(mock.received_bodies(), vec![Value::from("abc")]);
    ctx.stop().unwrap();
}
