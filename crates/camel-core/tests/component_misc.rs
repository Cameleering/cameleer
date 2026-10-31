use std::sync::Arc;
use std::time::Duration;

use camel_core::component::bean::{bind_bean, Bean, BeanMethods};
use camel_core::component::browse::browse_exchanges;
use camel_core::component::dataset::{bind_dataset, dataset_endpoint, dataset_test_endpoint, ListDataSet, SimpleDataSet};
use camel_core::prelude::*;

fn temp_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rust-camel-misc-{}-{}-{}", tag, std::process::id(), camel_core::util_time::now_millis()))
}

struct Greeter;
impl Bean for Greeter {
    fn invoke(&self, method: Option<&str>, ex: &mut Exchange) -> Result<()> {
        let name: String = ex.body_as()?;
        match method.unwrap_or("hello") {
            "hello" => ex.set_body(format!("Hello {}", name)),
            "bye" => ex.set_body(format!("Bye {}", name)),
            m => return Err(CamelError::new("MethodNotFoundException", m)),
        }
        Ok(())
    }
}

#[test]
fn bean_component_invokes_registry_beans() {
    let ctx = CamelContext::new();
    bind_bean(ctx.registry(), "greeter", Greeter);
    ctx.registry().bind("calc", BeanMethods::new().method_value("double", |ex| Ok(Value::from(ex.body().as_i64().unwrap_or(0) * 2))));
    ctx.registry().bind_processor("upper", |ex: &mut Exchange| {
        let s: String = ex.body_as()?;
        ex.set_body(s.to_uppercase());
        Ok(())
    });
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("bean:greeter", "Camel").unwrap(), Value::from("Hello Camel"));
    assert_eq!(t.request_body("bean:greeter?method=bye", "Camel").unwrap(), Value::from("Bye Camel"));
    assert_eq!(t.request_body_and_header("bean:greeter", "X", "CamelBeanMethodName", "bye").unwrap(), Value::from("Bye X"));
    assert_eq!(t.request_body("bean:calc?method=double(${body})", 21).unwrap(), Value::from(42));
    assert_eq!(t.request_body("class:upper", "abc").unwrap(), Value::from("ABC"));
    let err = t.request_body("bean:missing", "x").unwrap_err();
    assert!(err.cause.unwrap().is_a("NoSuchBeanException"));
    ctx.stop().unwrap();
}

#[test]
fn ref_component_resolves_registry_endpoint() {
    let ctx = CamelContext::new();
    let ep: Arc<dyn Endpoint> = ctx.get_endpoint("mock:target").unwrap();
    ctx.registry().bind("myEndpoint", ep);
    ctx.registry().bind("byUri", "direct:viaUri".to_string());
    ctx.add_routes(|b| {
        b.from("direct:start").to("ref:myEndpoint");
        b.from("ref:byUri").to("mock:viaUri");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:target").unwrap();
    mock.expected_bodies_received(vec!["via ref"]);
    ctx.create_producer_template().send_body("direct:start", "via ref").unwrap();
    mock.assert_is_satisfied().unwrap();
    let m2 = ctx.mock_endpoint("mock:viaUri").unwrap();
    m2.expected_message_count(1);
    ctx.create_producer_template().send_body("direct:viaUri", "x").unwrap();
    m2.assert_is_satisfied().unwrap();
    assert!(ctx.get_endpoint("ref:nothing").is_err());
    ctx.stop().unwrap();
}

#[test]
fn controlbus_route_actions() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:managed").route_id("managed").to("mock:managed");
        b.from("direct:self").route_id("selfStop").to("controlbus:route?routeId=current&action=stop");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("controlbus:route?routeId=managed&action=status", "").unwrap(), Value::from("Started"));
    t.send_body("controlbus:route?routeId=managed&action=stop", "").unwrap();
    assert_eq!(ctx.route_status("managed"), Some(ServiceStatus::Stopped));
    assert_eq!(t.request_body("controlbus:route?routeId=managed&action=status", "").unwrap(), Value::from("Stopped"));
    t.send_body("controlbus:route?routeId=managed&action=start", "").unwrap();
    assert_eq!(ctx.route_status("managed"), Some(ServiceStatus::Started));
    t.send_body("controlbus:route?routeId=managed&action=suspend", "").unwrap();
    assert_eq!(ctx.route_status("managed"), Some(ServiceStatus::Suspended));
    t.send_body("controlbus:route?routeId=managed&action=resume", "").unwrap();
    assert_eq!(ctx.route_status("managed"), Some(ServiceStatus::Started));
    t.send_body("direct:managed", "x").unwrap();
    let stats = t.request_body("controlbus:route?routeId=managed&action=stats", "").unwrap();
    assert_eq!(stats.get("exchangesTotal").and_then(|v| v.as_i64()), Some(1));
    t.send_body("controlbus:route?routeId=managed&action=restart&restartDelay=0", "").unwrap();
    assert_eq!(ctx.route_status("managed"), Some(ServiceStatus::Started));
    assert_eq!(t.request_body("controlbus:route?action=status", "").unwrap(), Value::from("Started"));
    // a route stopping itself is done asynchronously
    t.send_body("direct:self", "x").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while ctx.route_status("selfStop") != Some(ServiceStatus::Stopped) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(ctx.route_status("selfStop"), Some(ServiceStatus::Stopped));
    assert!(t.send_body("controlbus:route?routeId=nope&action=stop", "").is_err());
    assert!(ctx.get_endpoint("controlbus:route?action=explode").is_err());
    ctx.stop().unwrap();
}

#[test]
fn controlbus_language() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let v = t.request_body_and_header("controlbus:language:simple", "Hi ${header.who}", "who", "there").unwrap();
    assert_eq!(v, Value::from("Hi there"));
    ctx.stop().unwrap();
}

#[test]
fn browse_component_stores_exchanges() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:in").to("browse:seen");
        b.from("browse:seen").to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:in", "one").unwrap();
    t.send_body("direct:in", "two").unwrap();
    let bodies: Vec<Value> = browse_exchanges(&ctx, "browse:seen").unwrap().into_iter().map(|e| e.message.body).collect();
    assert_eq!(bodies, vec![Value::from("one"), Value::from("two")]);
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 2);
    ctx.stop().unwrap();
}

#[test]
fn exec_runs_echo_and_reports_exit_value() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let ex = t.request("exec:echo?args=hello world", |_| {}).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "hello world\n");
    assert_eq!(ex.header("CamelExecExitValue").unwrap().as_i64(), Some(0));
    // stdin from body
    assert_eq!(t.request_body("exec:cat", "piped").unwrap(), Value::from("piped"));
    // exit code and stderr
    let ex = t.request("exec:sh?args=RAW(-c \"echo oops >&2; exit 3\")&useStderrOnEmptyStdout=true", |_| {}).unwrap();
    assert_eq!(ex.header("CamelExecExitValue").unwrap().as_i64(), Some(3));
    assert_eq!(ex.header("CamelExecStderr").unwrap().to_string(), "oops\n");
    assert_eq!(ex.body_as::<String>().unwrap(), "oops\n");
    // header overrides args
    let v = t.request_body_and_header("exec:echo", Value::Null, "CamelExecCommandArgs", "from header").unwrap();
    assert_eq!(v, Value::from("from header\n"));
    // timeout
    let start = std::time::Instant::now();
    assert!(t.request_body("exec:sleep?args=5&timeout=100", Value::Null).is_err());
    assert!(start.elapsed() < Duration::from_secs(3));
    // exitValues
    assert!(t.request_body("exec:false?exitValues=0", Value::Null).is_err());
    assert!(t.request_body("exec:/definitely/not/here", Value::Null).is_err());
    ctx.stop().unwrap();
}

#[test]
fn stream_file_producer_and_consumer() {
    let out = temp_path("stream-out.txt");
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let uri = format!("stream:file?fileName={}", out.display());
    t.send_body(&uri, "line1").unwrap();
    t.send_body(&uri, "line2").unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "line1\nline2\n");
    t.send_body("stream:out", "written to stdout").unwrap();
    ctx.stop().unwrap();

    let ctx = CamelContext::new();
    let uri = format!("stream:file?fileName={}", out.display());
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:lines");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:lines").unwrap();
    mock.expected_bodies_received(vec!["line1", "line2"]);
    mock.assert_is_satisfied().unwrap();
    assert_eq!(mock.exchange(1).unwrap().header("CamelStreamComplete").unwrap(), &Value::Bool(true));
    assert_eq!(mock.exchange(1).unwrap().header("CamelStreamIndex").unwrap().as_i64(), Some(1));
    ctx.stop().unwrap();
    let _ = std::fs::remove_file(&out);
}

#[test]
fn stream_file_scan_stream_tails() {
    let f = temp_path("stream-tail.txt");
    std::fs::write(&f, "a\n").unwrap();
    let ctx = CamelContext::new();
    let uri = format!("stream:file?fileName={}&scanStream=true&scanStreamDelay=5", f.display());
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:tail");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:tail").unwrap();
    assert!(mock.wait_for_messages(1, Duration::from_secs(5)));
    use std::io::Write;
    std::fs::OpenOptions::new().append(true).open(&f).unwrap().write_all(b"b\n").unwrap();
    assert!(mock.wait_for_messages(2, Duration::from_secs(5)));
    assert_eq!(mock.received_bodies(), vec![Value::from("a"), Value::from("b")]);
    let start = std::time::Instant::now();
    ctx.stop().unwrap();
    assert!(start.elapsed() < Duration::from_millis(500));
    let _ = std::fs::remove_file(&f);
}

#[test]
fn language_component_evaluates_script() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("language:simple:Hello ${body}", "World").unwrap(), Value::from("Hello World"));
    assert_eq!(t.request_body("language:simple:Hello ${body}?transform=false", "World").unwrap(), Value::from("World"));
    assert_eq!(t.request_body("language:constant:fixed", "x").unwrap(), Value::from("fixed"));
    // script from the body
    assert_eq!(t.request_body_and_header("language:simple", "v=${header.v}", "v", 7).unwrap(), Value::from("v=7"));
    assert!(ctx.get_endpoint("language:nolang:x").is_err());
    ctx.stop().unwrap();
}

#[test]
fn dataset_consumer_and_producer_verify() {
    let ctx = CamelContext::new();
    bind_dataset(ctx.registry(), "foo", SimpleDataSet::new(20).with_body("payload"));
    ctx.add_routes(|b| {
        b.from("dataset:foo?initialDelay=0&produceDelay=0").to("seda:through");
        b.from("seda:through").to("dataset:foo");
    })
    .unwrap();
    ctx.start().unwrap();
    let ds = dataset_endpoint(&ctx, "dataset:foo").unwrap();
    ds.assert_is_satisfied().unwrap();
    assert_eq!(ds.received_counter(), 20);
    ctx.stop().unwrap();
}

#[test]
fn dataset_detects_wrong_messages() {
    let ctx = CamelContext::new();
    bind_dataset(ctx.registry(), "letters", ListDataSet::new(vec!["a", "b", "c"]));
    ctx.add_routes(|b| {
        b.from("dataset:letters?initialDelay=0&produceDelay=0")
            .process(|ex: &mut Exchange| {
                if ex.body_as::<String>()? == "b" {
                    ex.set_body("tampered");
                }
                Ok(())
            })
            .to("dataset:letters");
    })
    .unwrap();
    ctx.start().unwrap();
    let ds = dataset_endpoint(&ctx, "dataset:letters").unwrap();
    let err = ds.assert_is_satisfied_within(Duration::from_secs(5)).unwrap_err();
    assert!(err.message.contains("tampered"), "{}", err.message);
    ctx.stop().unwrap();
}

#[test]
fn dataset_test_compares_with_expected_endpoint() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:in").transform(simple("out-${body}")).to("dataset-test:seda:expected?timeout=200");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("seda:expected", "out-1").unwrap();
    t.send_body("seda:expected", "out-2").unwrap();
    let dt = dataset_test_endpoint(&ctx, "dataset-test:seda:expected?timeout=200").unwrap();
    assert_eq!(dt.load_expected().unwrap().len(), 2);
    t.send_body("direct:in", "1").unwrap();
    t.send_body("direct:in", "2").unwrap();
    dt.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}
