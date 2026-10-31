//! Kamelets: source/sink/action usage from the Rust DSL and YAML, Pipes, loading.

use std::time::Duration;

use camel_core::prelude::*;
use camel_yaml::{add_kamelet, add_routes_from_yaml, load_kamelets_from_dir, register_kamelet_component};

#[test]
fn builtin_timer_source_from_rust_dsl() {
    let ctx = CamelContext::new();
    register_kamelet_component(&ctx);
    ctx.add_routes(|b| {
        b.from("kamelet:timer-source?message=hi&period=20").to("mock:ticks");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:ticks").unwrap();
    assert!(mock.wait_for_messages(2, Duration::from_secs(5)));
    let ex = mock.exchange(0).unwrap();
    assert_eq!(ex.body(), &Value::from("hi"));
    assert_eq!(ex.header("Content-Type").unwrap(), &Value::from("text/plain"));
    let kamelet_routes: Vec<String> = ctx.route_ids().into_iter().filter(|r| r.starts_with("timer-source-")).collect();
    assert_eq!(kamelet_routes.len(), 1, "{:?}", ctx.route_ids());
    let route = ctx.route(&kamelet_routes[0]).unwrap();
    assert!(route.endpoint_uri.starts_with("timer:tick"), "{}", route.endpoint_uri);
    ctx.stop().unwrap();
}

#[test]
fn builtin_actions_and_sink_from_rust_dsl() {
    let ctx = CamelContext::new();
    register_kamelet_component(&ctx);
    ctx.add_routes(|b| {
        b.from("direct:in")
            .to("kamelet:insert-header-action?name=greeting&value=hello ${body}")
            .to("kamelet:set-body-action?value=${header.greeting}!")
            .to("kamelet:log-action?showHeaders=true&loggerName=test-action")
            .to("kamelet:log-sink")
            .to("mock:out");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let out = t.request_body("direct:in", "bob").unwrap();
    assert_eq!(out, Value::from("hello bob!"));
    let mock = ctx.mock_endpoint("mock:out").unwrap();
    assert_eq!(mock.received_counter(), 1);
    assert_eq!(mock.exchange(0).unwrap().header("greeting").unwrap(), &Value::from("hello bob"));
    // one generated route per kamelet usage
    let generated = ctx.routes().iter().filter(|r| r.id.contains("-action-") || r.id.starts_with("log-sink-")).count();
    assert_eq!(generated, 4);
    ctx.stop().unwrap();
}

#[test]
fn kamelet_properties_from_application_properties_and_validation() {
    let ctx = CamelContext::new();
    register_kamelet_component(&ctx);
    // required property missing
    let err = ctx.get_endpoint("kamelet:set-body-action").err().unwrap();
    assert!(err.message.contains("mandatory properties are missing: value"), "{}", err.message);
    let err = ctx.get_endpoint("kamelet:no-such-kamelet").err().unwrap();
    assert!(err.message.contains("Kamelet with id no-such-kamelet not found"), "{}", err.message);
    assert!(ctx.get_endpoint("kamelet:source").is_err());
    ctx.properties().set("camel.kamelet.set-body-action.value", "from-properties");
    ctx.add_routes(|b| {
        b.from("direct:p").to("kamelet:set-body-action/myBody").to("mock:p");
    })
    .unwrap();
    ctx.start().unwrap();
    let out = ctx.create_producer_template().request_body("direct:p", "x").unwrap();
    assert_eq!(out, Value::from("from-properties"));
    assert!(ctx.route("myBody").is_some(), "route id from kamelet:name/routeId");
    // stopping the calling route removes the kamelet route
    ctx.stop_route("route1").ok();
    ctx.stop().unwrap();
}

const GREETER: &str = r#"
apiVersion: camel.apache.org/v1
kind: Kamelet
metadata:
  name: greeter-source
  labels:
    camel.apache.org/kamelet.type: source
spec:
  definition:
    properties:
      who:
        type: string
        required: true
      period:
        type: integer
        default: 20
      greeting:
        type: string
        default: Hello
  template:
    from:
      uri: "timer:greeter?period={{period}}&repeatCount={{?count}}"
      steps:
        - setBody:
            simple: "{{greeting}} {{who}} #${exchangeProperty.CamelTimerCounter}"
        - to: kamelet:sink
"#;

const UPPER: &str = r#"
apiVersion: camel.apache.org/v1
kind: Kamelet
metadata:
  name: upper-action
spec:
  definition:
    properties: {}
  flow:
    from:
      uri: kamelet:source
      steps:
        - setBody:
            simple: "${body.toUpperCase()}"
"#;

#[test]
fn custom_kamelets_in_yaml_routes() {
    let ctx = CamelContext::new();
    assert_eq!(add_kamelet(&ctx, GREETER).unwrap(), "greeter-source");
    assert_eq!(add_kamelet(&ctx, UPPER).unwrap(), "upper-action");
    add_routes_from_yaml(
        &ctx,
        r#"
- route:
    id: greet
    from:
      uri: kamelet:greeter-source
      parameters:
        who: World
        count: 2
      steps:
        - to: kamelet:upper-action
        - to: mock:greet
"#,
    )
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:greet").unwrap();
    mock.expected_bodies_received(vec!["HELLO WORLD #1", "HELLO WORLD #2"]);
    mock.set_assert_period(Duration::from_millis(100));
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn kamelet_and_routes_in_one_multi_document_yaml() {
    let ctx = CamelContext::new();
    let yaml = format!(
        "{}\n---\n{}",
        UPPER.trim(),
        r#"
- from:
    uri: direct:start
    steps:
      - to: "kamelet:upper-action"
      - to: "kamelet:log-sink?showHeaders=true"
      - to: mock:end
"#
    );
    add_routes_from_yaml(&ctx, &yaml).unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:start", "abc").unwrap(), Value::from("ABC"));
    ctx.stop().unwrap();
}

#[test]
fn yaml_from_builtin_timer_source() {
    let ctx = CamelContext::new();
    add_routes_from_yaml(
        &ctx,
        r#"
- from:
    uri: "kamelet:timer-source?message=hi&period=20&repeatCount=3"
    steps:
      - to: mock:yaml-ticks
"#,
    )
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:yaml-ticks").unwrap();
    mock.expected_bodies_received(vec!["hi", "hi", "hi"]);
    mock.set_assert_period(Duration::from_millis(100));
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn pipe_resource_becomes_route() {
    let ctx = CamelContext::new();
    add_routes_from_yaml(
        &ctx,
        r#"
apiVersion: camel.apache.org/v1
kind: Pipe
metadata:
  name: timer-to-mock
spec:
  source:
    ref:
      kind: Kamelet
      apiVersion: camel.apache.org/v1
      name: timer-source
    properties:
      message: "Hello Pipe"
      period: 20
  steps:
    - ref:
        kind: Kamelet
        apiVersion: camel.apache.org/v1
        name: insert-header-action
      properties:
        name: source
        value: pipe
  sink:
    uri: mock:pipe
"#,
    )
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.route("timer-to-mock").is_some());
    let mock = ctx.mock_endpoint("mock:pipe").unwrap();
    assert!(mock.wait_for_messages(1, Duration::from_secs(5)));
    let ex = mock.exchange(0).unwrap();
    assert_eq!(ex.body(), &Value::from("Hello Pipe"));
    assert_eq!(ex.header("source").unwrap(), &Value::from("pipe"));
    ctx.stop().unwrap();
}

#[test]
fn kamelet_binding_legacy_kind() {
    let ctx = CamelContext::new();
    add_routes_from_yaml(
        &ctx,
        r#"
apiVersion: camel.apache.org/v1alpha1
kind: KameletBinding
metadata:
  name: legacy
spec:
  source:
    uri: direct:legacy
  sink:
    ref: {kind: Kamelet, name: set-body-action}
    properties: {value: bound}
"#,
    )
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:legacy", "x").unwrap(), Value::from("bound"));
    ctx.stop().unwrap();
}

#[test]
fn load_kamelets_from_directory() {
    let dir = std::env::temp_dir().join(format!("camel-kamelets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("greeter-source.kamelet.yaml"), GREETER).unwrap();
    std::fs::write(dir.join("upper.yaml"), UPPER).unwrap();
    std::fs::write(dir.join("routes.yaml"), "- from:\n    uri: direct:x\n    steps:\n      - to: mock:x\n").unwrap();
    std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
    let ctx = CamelContext::new();
    let names = load_kamelets_from_dir(&ctx, &dir).unwrap();
    assert_eq!(names, vec!["greeter-source".to_string(), "upper-action".to_string()]);
    assert!(camel_yaml::kamelet_registry(&ctx).get("upper-action").is_some());
    std::fs::write(dir.join("broken.kamelet.yaml"), "kind: Kamelet\nmetadata: {}\n").unwrap();
    assert!(load_kamelets_from_dir(&ctx, &dir).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
