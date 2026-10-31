//! Conformance tests for core components (timer, seda, file, log, mock, controlbus,
//! direct, ProducerTemplate / ConsumerTemplate) against Apache Camel 4 documentation.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_core::prelude::*;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "rust-camel-conf-{}-{}-{}-{}",
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
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wait_until(f: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    f()
}

fn today() -> String {
    camel_core::util_time::format_millis(camel_core::util_time::now_millis(), "yyyyMMdd")
}

// ---------------------------------------------------------------------------
// timer
// ---------------------------------------------------------------------------

#[test]
fn timer_repeat_count_and_counter_property() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:tick?period=10&delay=0&repeatCount=3").to("mock:tick");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:tick").unwrap();
    m.expected_message_count(3);
    m.set_assert_period(Duration::from_millis(100));
    m.assert_is_satisfied_within(Duration::from_secs(5)).unwrap();
    let ex = m.received_exchanges();
    let counters: Vec<Value> = ex.iter().map(|e| e.property(props::TIMER_COUNTER).cloned().unwrap()).collect();
    assert_eq!(counters, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    assert_eq!(ex[0].body(), &Value::Null, "timer fires with an empty body");
    assert_eq!(ex[0].pattern, ExchangePattern::InOnly);
    ctx.stop().unwrap();
}

#[test]
fn timer_stops_firing_when_route_stopped() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:t?period=10&delay=0").route_id("timer").to("mock:t");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:t").unwrap();
    assert!(m.wait_for_messages(2, Duration::from_secs(5)));
    ctx.stop_route("timer").unwrap();
    let n = m.received_counter();
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(m.received_counter(), n);
    assert_eq!(ctx.route_status("timer"), Some(ServiceStatus::Stopped));
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------
// seda
// ---------------------------------------------------------------------------

#[test]
fn seda_concurrent_consumers() {
    let ctx = CamelContext::new();
    let threads = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let t2 = threads.clone();
    ctx.add_routes(move |b| {
        let t3 = t2.clone();
        b.from("seda:work?concurrentConsumers=3")
            .process(move |_ex: &mut Exchange| {
                t3.lock().unwrap().insert(format!("{:?}", std::thread::current().id()));
                std::thread::sleep(Duration::from_millis(30));
                Ok(())
            })
            .to("mock:done");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 0..6 {
        t.send_body("seda:work", i).unwrap();
    }
    let m = ctx.mock_endpoint("mock:done").unwrap();
    assert!(m.wait_for_messages(6, Duration::from_secs(5)));
    assert!(threads.lock().unwrap().len() > 1);
    ctx.stop().unwrap();
}

#[test]
fn seda_in_out_request_from_template_times_out() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:slow").delay(constant(500)).set_body(constant("late"));
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().request_body("seda:slow?timeout=50", "x").unwrap_err();
    assert!(e.cause.unwrap().is_a("ExchangeTimedOutException"));
    ctx.stop().unwrap();
}

#[test]
fn seda_without_consumer_queues_messages_for_consumer_template() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("seda:queue", "one").unwrap();
    t.send_body("seda:queue", "two").unwrap();
    let c = ctx.create_consumer_template();
    assert_eq!(c.receive_body("seda:queue", Duration::from_secs(1)).unwrap(), Some(Value::from("one")));
    assert_eq!(c.receive_body("seda:queue", Duration::from_secs(1)).unwrap(), Some(Value::from("two")));
    assert_eq!(c.receive_body("seda:queue", Duration::from_millis(20)).unwrap(), None);
    assert!(c.receive_no_wait("seda:queue").unwrap().is_none());
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------
// direct
// ---------------------------------------------------------------------------

#[test]
fn direct_without_consumer_fails() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body("direct:nobody", "x").unwrap_err();
    assert_eq!(e.cause.unwrap().kind, kinds::DIRECT_CONSUMER_NOT_AVAILABLE);
}

#[test]
fn unknown_component_fails_to_resolve() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body("nosuchcomponent:foo", "x").unwrap_err();
    let cause = e.cause.unwrap();
    assert!(cause.is_a("ResolveEndpointFailedException") || cause.is_a("NoSuchEndpointException"), "{}", cause);
}

#[test]
fn unknown_endpoint_option_fails_at_route_creation() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").to("seda:x?noSuchOption=true");
    })
    .unwrap();
    let e = ctx.start().unwrap_err();
    let text = format!("{} {:?}", e, e.cause);
    assert!(text.contains("noSuchOption"), "{}", text);
}

// ---------------------------------------------------------------------------
// file
// ---------------------------------------------------------------------------

#[test]
fn file_consume_process_move_with_simple_file_name_expression() {
    let dir = TempDir::new("move");
    fs::create_dir_all(dir.path().join("inbox")).unwrap();
    fs::write(dir.path().join("inbox/order.xml"), "<order/>").unwrap();
    let ctx = CamelContext::new();
    let inbox = dir.uri("inbox");
    ctx.add_routes(move |b| {
        b.from(format!("file:{}?initialDelay=0&delay=10&move=done/${{file:name.noext}}-${{date:now:yyyyMMdd}}.txt", inbox))
            .convert_body_to("String")
            .set_header("onlyName", simple("${file:onlyname.noext}"))
            .to("mock:files");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:files").unwrap();
    assert!(m.wait_for_messages(1, Duration::from_secs(5)));
    let ex = m.exchange(0).unwrap();
    assert_eq!(ex.body(), &Value::from("<order/>"));
    assert_eq!(ex.header("CamelFileName"), Some(&Value::from("order.xml")));
    assert_eq!(ex.header("CamelFileNameOnly"), Some(&Value::from("order.xml")));
    assert_eq!(ex.header("onlyName"), Some(&Value::from("order")));
    let moved = dir.path().join(format!("inbox/done/order-{}.txt", today()));
    assert!(wait_until(|| moved.exists()), "expected {:?}", moved);
    assert!(!dir.path().join("inbox/order.xml").exists());
    ctx.stop().unwrap();
}

#[test]
fn file_consume_default_moves_to_dot_camel() {
    let dir = TempDir::new("dotcamel");
    fs::create_dir_all(dir.path().join("in")).unwrap();
    fs::write(dir.path().join("in/a.txt"), "A").unwrap();
    let ctx = CamelContext::new();
    let uri = format!("file:{}?initialDelay=0&delay=10", dir.uri("in"));
    ctx.add_routes(move |b| {
        b.from(uri.as_str()).to("mock:a");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.mock_endpoint("mock:a").unwrap().wait_for_messages(1, Duration::from_secs(5)));
    assert!(wait_until(|| dir.path().join("in/.camel/a.txt").exists()));
    ctx.stop().unwrap();
}

#[test]
fn file_consume_failure_with_move_failed() {
    let dir = TempDir::new("failed");
    fs::create_dir_all(dir.path().join("in")).unwrap();
    fs::write(dir.path().join("in/bad.txt"), "B").unwrap();
    let ctx = CamelContext::new();
    let uri = format!("file:{}?initialDelay=0&delay=10&moveFailed=error", dir.uri("in"));
    ctx.add_routes(move |b| {
        b.from(uri.as_str()).error_handler(default_error_handler().log_exhausted(false)).throw_exception("IOException", "cannot process");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(wait_until(|| dir.path().join("in/error/bad.txt").exists()));
    ctx.stop().unwrap();
}

#[test]
fn file_producer_file_name_with_simple_and_header() {
    let dir = TempDir::new("produce");
    let ctx = CamelContext::new();
    let out = dir.uri("out");
    ctx.add_routes(move |b| {
        b.from("direct:option").to(format!("file:{}?fileName=${{header.id}}-report.txt", out));
        b.from("direct:header").to(format!("file:{}", out));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:option", "R1", "id", "42").unwrap();
    t.send_body_and_header("direct:header", "R2", "CamelFileName", "named.txt").unwrap();
    assert_eq!(fs::read_to_string(dir.path().join("out/42-report.txt")).unwrap(), "R1");
    assert_eq!(fs::read_to_string(dir.path().join("out/named.txt")).unwrap(), "R2");
    // without a file name the exchange id (message id) is used
    t.send_body("direct:header", "R3").unwrap();
    let count = fs::read_dir(dir.path().join("out")).unwrap().count();
    assert_eq!(count, 3);
}

#[test]
fn file_producer_file_exist_append_and_fail() {
    let dir = TempDir::new("exist");
    let ctx = CamelContext::new();
    let out = dir.uri("out");
    ctx.add_routes(move |b| {
        b.from("direct:append").to(format!("file:{}?fileName=a.txt&fileExist=Append", out));
        b.from("direct:fail").to(format!("file:{}?fileName=a.txt&fileExist=Fail", out));
        b.from("direct:override").to(format!("file:{}?fileName=b.txt", out));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:append", "1").unwrap();
    t.send_body("direct:append", "2").unwrap();
    assert_eq!(fs::read_to_string(dir.path().join("out/a.txt")).unwrap(), "12");
    let e = t.send_body("direct:fail", "3").unwrap_err();
    assert!(e.cause.unwrap().is_a("GenericFileOperationFailedException"));
    t.send_body("direct:override", "x").unwrap();
    t.send_body("direct:override", "y").unwrap();
    assert_eq!(fs::read_to_string(dir.path().join("out/b.txt")).unwrap(), "y", "default fileExist=Override");
}

#[test]
fn file_producer_sets_file_name_produced_header() {
    let dir = TempDir::new("produced");
    let ctx = CamelContext::new();
    let out = dir.uri("out");
    ctx.add_routes(move |b| {
        b.from("direct:start").to(format!("file:{}?fileName=x.txt", out));
    })
    .unwrap();
    ctx.start().unwrap();
    let ex = ctx.create_producer_template().send_with("direct:start", ExchangePattern::InOnly, |e| e.set_body("x"));
    let produced = ex.header("CamelFileNameProduced").map(|v| v.to_string()).unwrap_or_default();
    assert!(produced.ends_with("x.txt"), "{}", produced);
}

// ---------------------------------------------------------------------------
// log
// ---------------------------------------------------------------------------

#[test]
fn log_component_options_format() {
    let records = Arc::new(Mutex::new(Vec::new()));
    let r2 = records.clone();
    camel_core::log::add_sink(move |rec| {
        if rec.logger == "conformance.log" {
            r2.lock().unwrap().push(rec.message.clone());
        }
    });
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:default").to("log:conformance.log");
        b.from("direct:headers").to("log:conformance.log?showHeaders=true&showBodyType=false&showExchangePattern=false");
        b.from("direct:nobody").to("log:conformance.log?showBody=false&showExchangePattern=false&showBodyType=false&showHeaders=true");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:default", "Hello").unwrap();
    t.send_body_and_header("direct:headers", "Hi", "foo", "bar").unwrap();
    t.send_body_and_header("direct:nobody", "Secret", "a", 1).unwrap();
    let recs = records.lock().unwrap().clone();
    assert_eq!(recs.len(), 3, "{:?}", recs);
    assert!(recs[0].starts_with("Exchange[ExchangePattern: InOnly, BodyType: "), "{}", recs[0]);
    assert!(recs[0].ends_with("Body: Hello]"), "{}", recs[0]);
    assert_eq!(recs[1], "Exchange[Headers: {foo=bar}, Body: Hi]");
    assert_eq!(recs[2], "Exchange[Headers: {a=1}]");
}

#[test]
fn log_eip_with_simple_message() {
    let records = Arc::new(Mutex::new(Vec::new()));
    let r2 = records.clone();
    camel_core::log::add_sink(move |rec| {
        if rec.message.starts_with("conformance-log-eip") {
            r2.lock().unwrap().push(rec.message.clone());
        }
    });
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").log("conformance-log-eip ${body} from ${header.who}");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body_and_header("direct:start", "msg", "who", "me").unwrap();
    assert_eq!(*records.lock().unwrap(), vec!["conformance-log-eip msg from me".to_string()]);
}

// ---------------------------------------------------------------------------
// mock
// ---------------------------------------------------------------------------

#[test]
fn mock_expected_message_count_zero_detects_late_message() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:in").delay(constant(50)).to("mock:out");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:out").unwrap();
    m.expected_message_count(0);
    m.set_assert_period(Duration::from_millis(300));
    ctx.create_producer_template().send_body("seda:in", "late").unwrap();
    assert!(m.assert_is_satisfied_within(Duration::from_millis(10)).is_err(), "message arriving within the assert period must fail");
    ctx.stop().unwrap();
}

#[test]
fn mock_assert_period_detects_extra_messages() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").to("mock:out").to("seda:again");
        b.from("seda:again").delay(constant(50)).to("mock:out");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:out").unwrap();
    m.expected_message_count(1);
    m.set_assert_period(Duration::from_millis(300));
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert!(m.assert_is_satisfied().is_err());
    ctx.stop().unwrap();
}

#[test]
fn mock_expected_bodies_and_headers() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").set_header("type", constant("t")).to("mock:out");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:out").unwrap();
    m.expected_bodies_received(vec!["a", "b"]);
    m.expected_header_received("type", "t");
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "a").unwrap();
    t.send_body("direct:start", "b").unwrap();
    m.assert_is_satisfied().unwrap();
    m.reset();
    m.expected_bodies_received(vec!["b", "a"]);
    t.send_body("direct:start", "a").unwrap();
    t.send_body("direct:start", "b").unwrap();
    assert!(m.assert_is_satisfied().is_err(), "order matters for expectedBodiesReceived");
}

#[test]
fn mock_when_any_exchange_received_can_fail() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").to("mock:out");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:out").unwrap();
    m.when_any_exchange_received(|_ex: &mut Exchange| Err(CamelError::io("simulated")));
    let e = ctx.create_producer_template().send_body("direct:start", "x").unwrap_err();
    assert!(e.cause.unwrap().is_a("IOException"));
    assert_eq!(m.received_counter(), 1);
}

// ---------------------------------------------------------------------------
// controlbus
// ---------------------------------------------------------------------------

#[test]
fn controlbus_route_actions() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:managed").route_id("managed").to("mock:managed");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("controlbus:route?routeId=managed&action=status", Value::Null).unwrap(), Value::from("Started"));
    t.send_body("controlbus:route?routeId=managed&action=stop", Value::Null).unwrap();
    assert_eq!(ctx.route_status("managed"), Some(ServiceStatus::Stopped));
    assert_eq!(t.request_body("controlbus:route?routeId=managed&action=status", Value::Null).unwrap(), Value::from("Stopped"));
    t.send_body("controlbus:route?routeId=managed&action=start", Value::Null).unwrap();
    assert_eq!(ctx.route_status("managed"), Some(ServiceStatus::Started));
    ctx.stop().unwrap();
}

#[test]
fn controlbus_language_simple() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body("controlbus:language:simple", "${camelId}").unwrap();
    assert_eq!(r, Value::from(ctx.name().to_string()));
}

// ---------------------------------------------------------------------------
// templates
// ---------------------------------------------------------------------------

#[test]
fn producer_template_request_returns_out_and_send_returns_nothing() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").transform(simple("Bye ${body}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("direct:start", "World").unwrap(), Value::from("Bye World"));
    assert_eq!(t.request_body_and_header("direct:start", "A", "h", 1).unwrap(), Value::from("Bye A"));
    t.send_body("direct:start", "World").unwrap();
    let dt = ctx.create_producer_template().with_default_endpoint("direct:start");
    dt.send_body_default("x").unwrap();
}

#[test]
fn producer_template_async_request() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:slow").delay(constant(30)).transform(simple("${body}!"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let f1 = t.async_request_body("direct:slow", "a");
    let f2 = t.async_request_body("direct:slow", "b");
    assert_eq!(f1.join().unwrap().unwrap(), Value::from("a!"));
    assert_eq!(f2.join().unwrap().unwrap(), Value::from("b!"));
}

#[test]
fn consumer_template_receive_from_file() {
    let dir = TempDir::new("ct");
    fs::create_dir_all(dir.path().join("in")).unwrap();
    fs::write(dir.path().join("in/hello.txt"), "Hello file").unwrap();
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let c = ctx.create_consumer_template();
    let ex = c.receive(&format!("file:{}?noop=true", dir.uri("in")), Some(Duration::from_secs(2))).unwrap().unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "Hello file");
    assert_eq!(ex.header("CamelFileName"), Some(&Value::from("hello.txt")));
}

#[test]
fn file_noop_is_idempotent_and_keeps_file() {
    let dir = TempDir::new("noop");
    fs::create_dir_all(dir.path().join("in")).unwrap();
    fs::write(dir.path().join("in/a.txt"), "A").unwrap();
    let ctx = CamelContext::new();
    let uri = format!("file:{}?initialDelay=0&delay=10&noop=true", dir.uri("in"));
    ctx.add_routes(move |b| {
        b.from(uri.as_str()).to("mock:a");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:a").unwrap();
    assert!(m.wait_for_messages(1, Duration::from_secs(5)));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(m.received_counter(), 1, "noop=true implies idempotent=true");
    assert!(dir.path().join("in/a.txt").exists());
    ctx.stop().unwrap();
}

#[test]
fn file_consumer_headers_recursive_and_include() {
    let dir = TempDir::new("headers");
    fs::create_dir_all(dir.path().join("in/sub")).unwrap();
    fs::write(dir.path().join("in/sub/data.csv"), "1,2,3").unwrap();
    fs::write(dir.path().join("in/skip.txt"), "x").unwrap();
    let ctx = CamelContext::new();
    let uri = format!("file:{}?initialDelay=0&delay=10&recursive=true&include=.*\\.csv&delete=true", dir.uri("in"));
    ctx.add_routes(move |b| {
        b.from(uri.as_str()).to("mock:a");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:a").unwrap();
    assert!(m.wait_for_messages(1, Duration::from_secs(5)));
    let ex = m.exchange(0).unwrap();
    assert_eq!(ex.header("CamelFileName"), Some(&Value::from("sub/data.csv")));
    assert_eq!(ex.header("CamelFileNameOnly"), Some(&Value::from("data.csv")));
    assert_eq!(ex.header("CamelFileLength"), Some(&Value::Int(5)));
    assert!(ex.header("CamelFileLastModified").is_some());
    assert!(ex.header("CamelFileAbsolutePath").is_some());
    assert!(wait_until(|| !dir.path().join("in/sub/data.csv").exists()), "delete=true");
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(m.received_counter(), 1, "include filter");
    assert!(dir.path().join("in/skip.txt").exists());
    ctx.stop().unwrap();
}

#[test]
fn file_split_lines_with_tokenize() {
    // classic Camel example: read a file and split it line by line
    let dir = TempDir::new("split");
    fs::create_dir_all(dir.path().join("in")).unwrap();
    fs::write(dir.path().join("in/lines.txt"), "l1\nl2\nl3").unwrap();
    let ctx = CamelContext::new();
    let uri = format!("file:{}?initialDelay=0&delay=10&noop=true", dir.uri("in"));
    ctx.add_routes(move |b| {
        b.from(uri.as_str()).split(body().tokenize("\n")).streaming().to("mock:line").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:line").unwrap();
    assert!(m.wait_for_messages(3, Duration::from_secs(5)));
    let lines: Vec<String> = m.received_bodies().iter().map(|v| v.to_string()).collect();
    assert_eq!(lines, vec!["l1", "l2", "l3"]);
    ctx.stop().unwrap();
}

#[test]
fn seda_in_out_headers_flow_back_to_caller() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:svc").set_header("fromSeda", constant("yes")).transform(constant("done"));
    })
    .unwrap();
    ctx.start().unwrap();
    let ex = ctx.create_producer_template().request("seda:svc", |e| e.set_body("x")).unwrap();
    assert_eq!(ex.body(), &Value::from("done"));
    assert_eq!(ex.header("fromSeda"), Some(&Value::from("yes")));
    ctx.stop().unwrap();
}

#[test]
fn mock_expected_minimum_message_count() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").to("mock:out");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:out").unwrap();
    m.expected_minimum_message_count(2);
    let t = ctx.create_producer_template();
    for i in 0..3 {
        t.send_body("direct:start", i).unwrap();
    }
    m.assert_is_satisfied().unwrap();
}
