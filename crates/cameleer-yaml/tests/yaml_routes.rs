use std::collections::BTreeMap;
use std::sync::Arc;

use cameleer_core::prelude::*;
use cameleer_core::processor::basic::ToDefinition;
use cameleer_yaml::*;

fn started(yaml: &str) -> CamelContext {
    let ctx = CamelContext::new();
    add_routes_from_yaml(&ctx, yaml).unwrap_or_else(|e| panic!("load failed: {}", e));
    ctx.start().unwrap();
    ctx
}

#[test]
fn basic_route_with_set_body_and_headers() {
    let ctx = started(
        r#"
- route:
    id: hello
    description: greets people
    group: demo
    from:
      uri: direct:start
      steps:
        - setHeader:
            name: greeting
            constant: Hello
        - setBody:
            simple: "${header.greeting} ${body}"
        - to: mock:result
"#,
    );
    let mock = ctx.mock_endpoint("mock:result").unwrap();
    mock.expected_bodies_received(vec!["Hello World"]);
    mock.expected_header_received("greeting", "Hello");
    ctx.create_producer_template().send_body("direct:start", "World").unwrap();
    mock.assert_is_satisfied().unwrap();
    let route = ctx.route("hello").unwrap();
    assert_eq!(route.description.as_deref(), Some("greets people"));
    assert_eq!(route.group.as_deref(), Some("demo"));
}

#[test]
fn short_from_form_and_flow_style() {
    let ctx = started(
        r#"
- from:
    uri: "direct:short"
    steps:
      - transform: {simple: "short:${body}"}
      - to: {uri: "mock:short"}
"#,
    );
    let mock = ctx.mock_endpoint("mock:short").unwrap();
    mock.expected_bodies_received(vec!["short:x"]);
    ctx.create_producer_template().send_body("direct:short", "x").unwrap();
    mock.assert_is_satisfied().unwrap();
}

#[test]
fn expression_forms() {
    let ctx = started(
        r#"
- route:
    from:
      uri: direct:expr
      steps:
        - setProperty:
            name: prop
            expression:
              constant: P
        - setHeader:
            name: fromProp
            exchangeProperty: prop
        - setHeader:
            name: copy
            header: fromProp
        - setHeader:
            name: nested
            expression:
              simple:
                expression: "${header.copy}-${body}"
        - setBody:
            header: nested
        - to: mock:expr
"#,
    );
    let mock = ctx.mock_endpoint("mock:expr").unwrap();
    mock.expected_bodies_received(vec!["P-b"]);
    mock.expected_header_received("copy", "P");
    mock.expected_property_received("prop", "P");
    ctx.create_producer_template().send_body("direct:expr", "b").unwrap();
    mock.assert_is_satisfied().unwrap();
}

#[test]
fn set_headers_variables_and_removals() {
    let ctx = started(
        r#"
- route:
    from:
      uri: direct:rm
      steps:
        - setHeaders:
            headers:
              - name: a
                constant: "1"
              - name: b
                simple: "${body}"
              - name: keepMe
                constant: k
        - setVariable:
            name: v
            constant: var
        - setProperty:
            name: p
            constant: prop
        - setProperty:
            name: q
            constant: prop
        - setHeader:
            name: fromVar
            simple: "${exchangeProperty.CamelVariable.v}"
        - removeHeader:
            name: a
        - removeHeaders:
            pattern: "*"
            excludePattern: keep*,fromVar
        - removeProperty: p
        - removeProperties:
            pattern: "q*"
        - removeVariable:
            name: v
        - to: mock:rm
"#,
    );
    let mock = ctx.mock_endpoint("mock:rm").unwrap();
    mock.expected_message_count(1);
    mock.expected_header_received("keepMe", "k");
    mock.expected_header_received("fromVar", "var");
    ctx.create_producer_template().send_body("direct:rm", "body").unwrap();
    mock.assert_is_satisfied().unwrap();
    let ex = mock.exchange(0).unwrap();
    assert!(ex.message.headers.get("a").is_none());
    assert!(ex.message.headers.get("b").is_none());
    assert!(!ex.properties.contains_key("p"));
    assert!(!ex.properties.contains_key("q"));
    assert!(!ex.properties.contains_key("CamelVariable.v"));
}

#[test]
fn convert_log_stop_pattern_and_step() {
    let ctx = started(
        r#"
- route:
    from:
      uri: direct:misc
      steps:
        - setHeader:
            name: num
            constant: "42"
        - convertHeaderTo:
            name: num
            type: int
        - convertBodyTo: Integer
        - log: "got ${body}"
        - log:
            message: "detail ${body}"
            loggingLevel: DEBUG
            logName: my.logger
        - setExchangePattern: InOut
        - step:
            id: inner
            steps:
              - setHeader:
                  name: stepped
                  constant: yes
              - to: mock:misc
        - stop: {}
        - to: mock:never
"#,
    );
    let mock = ctx.mock_endpoint("mock:misc").unwrap();
    let never = ctx.mock_endpoint("mock:never").unwrap();
    mock.expected_bodies_received(vec![Value::Int(7)]);
    mock.expected_header_received("num", Value::Int(42));
    mock.expected_header_received("stepped", "yes");
    never.expected_message_count(0);
    ctx.create_producer_template().send_body("direct:misc", "7").unwrap();
    mock.assert_is_satisfied().unwrap();
    assert_eq!(mock.exchange(0).unwrap().pattern, ExchangePattern::InOut);
    never.assert_is_satisfied_within(std::time::Duration::from_millis(20)).unwrap();
}

#[test]
fn to_d_in_only_and_disabled_steps() {
    let ctx = started(
        r#"
- route:
    from:
      uri: direct:dyn
      steps:
        - setBody:
            constant: should-not-happen
            disabled: true
        - toD: "mock:${header.target}"
        - inOnly: mock:inonly
        - inOut:
            uri: mock:inout
"#,
    );
    let a = ctx.mock_endpoint("mock:alpha").unwrap();
    a.expected_bodies_received(vec!["payload"]);
    let io = ctx.mock_endpoint("mock:inonly").unwrap();
    io.expected_message_count(1);
    ctx.create_producer_template().send_body_and_header("direct:dyn", "payload", "target", "alpha").unwrap();
    a.assert_is_satisfied().unwrap();
    io.assert_is_satisfied().unwrap();
}

#[test]
fn step_ids_are_applied() {
    let ctx = CamelContext::new();
    let routes = load_routes(
        &ctx,
        r#"
- route:
    id: ids
    from:
      uri: direct:ids
      steps:
        - to:
            id: myTo
            uri: mock:ids
        - step:
            id: blk
            steps:
              - to: {id: innerTo, uri: mock:inner}
"#,
    )
    .unwrap();
    assert_eq!(routes.len(), 1);
    let r = &routes[0];
    assert_eq!(r.outputs[0].id(), Some("myTo"));
    assert_eq!(r.outputs[1].id(), Some("blk"));
    assert_eq!(r.outputs[1].outputs()[0].id(), Some("innerTo"));
}

#[test]
fn uri_parameters_are_merged() {
    let ctx = CamelContext::new();
    let routes = load_routes(
        &ctx,
        r#"
- route:
    id: params
    from:
      uri: direct:params
      parameters:
        timeout: 1000
      steps:
        - to:
            uri: log:params
            parameters:
              showHeaders: true
              level: DEBUG
        - to:
            uri: "log:other?showBody=false"
            parameters:
              marker: "a b"
        - to: mock:params
"#,
    )
    .unwrap();
    let r = &routes[0];
    assert_eq!(r.input, "direct:params?timeout=1000");
    let uri = |i: usize| r.outputs[i].as_any().downcast_ref::<ToDefinition>().unwrap().uri.clone();
    assert_eq!(uri(0), "log:params?showHeaders=true&level=DEBUG");
    assert_eq!(uri(1), "log:other?showBody=false&marker=a%20b");

    // the merged uris are valid endpoint uris
    let ctx = started(
        r#"
- from:
    uri: direct:p2
    steps:
      - to:
          uri: log:params
          parameters:
            showHeaders: true
      - to: mock:p2
"#,
    );
    let m = ctx.mock_endpoint("mock:p2").unwrap();
    m.expected_message_count(1);
    ctx.create_producer_template().send_body("direct:p2", "x").unwrap();
    m.assert_is_satisfied().unwrap();
}

#[test]
fn route_templates_and_templated_routes() {
    let ctx = started(
        r#"
- templatedRoute:
    routeTemplateRef: greeter
    routeId: greet-a
    parameters:
      - name: name
        value: a
      - name: greeting
        value: Hi
- routeTemplate:
    id: greeter
    parameters:
      - name: name
      - name: greeting
        defaultValue: Hello
    route:
      from:
        uri: "direct:{{name}}"
        steps:
          - setBody:
              simple: "{{greeting}} ${body}"
          - to: "mock:{{name}}"
- templatedRoute:
    routeTemplateRef: greeter
    routeId: greet-b
    parameters:
      - name: name
        value: b
"#,
    );
    let ma = ctx.mock_endpoint("mock:a").unwrap();
    let mb = ctx.mock_endpoint("mock:b").unwrap();
    ma.expected_bodies_received(vec!["Hi x"]);
    mb.expected_bodies_received(vec!["Hello y"]);
    let t = ctx.create_producer_template();
    t.send_body("direct:a", "x").unwrap();
    t.send_body("direct:b", "y").unwrap();
    ma.assert_is_satisfied().unwrap();
    mb.assert_is_satisfied().unwrap();
    assert!(ctx.route("greet-a").is_some());
    assert!(ctx.route("greet-b").is_some());

    // instantiate from Rust
    let mut params = BTreeMap::new();
    params.insert("name".to_string(), "c".to_string());
    add_route_from_template(&ctx, "greeter", Some("greet-c"), &params).unwrap();
    let mc = ctx.mock_endpoint("mock:c").unwrap();
    mc.expected_bodies_received(vec!["Hello z"]);
    t.send_body("direct:c", "z").unwrap();
    mc.assert_is_satisfied().unwrap();

    // missing mandatory parameter
    let e = add_route_from_template(&ctx, "greeter", None, &BTreeMap::new()).unwrap_err();
    assert!(e.message.contains("mandatory parameters are missing: name"), "{}", e);
    let e = load_routes(&ctx, "- templatedRoute:\n    routeTemplateRef: nope\n").unwrap_err();
    assert!(e.message.contains("Cannot find route template with id 'nope'"), "{}", e);
}

#[test]
fn template_with_short_from_form() {
    let ctx = started(
        r#"
- routeTemplate:
    id: simpleTpl
    parameters:
      - name: in
    from:
      uri: "direct:{{in}}"
      steps:
        - to: mock:tpl
- templatedRoute:
    routeTemplateRef: simpleTpl
    parameters:
      - name: in
        value: tplIn
"#,
    );
    let m = ctx.mock_endpoint("mock:tpl").unwrap();
    m.expected_message_count(1);
    ctx.create_producer_template().send_body("direct:tplIn", "x").unwrap();
    m.assert_is_satisfied().unwrap();
}

#[test]
fn beans_with_factory_hook() {
    register_bean_factory("com.example.Greeter", |ctx: &CamelContext, def: &BeanDefinition| {
        let prefix = def.properties.get("prefix").map(|v| v.to_string()).unwrap_or_default();
        ctx.registry().bind_processor(def.name.clone(), move |ex: &mut Exchange| {
            let b: String = ex.body_as()?;
            ex.set_body(format!("{}{}", prefix, b));
            Ok(())
        });
        Ok(())
    });
    let ctx = started(
        r#"
- beans:
    - name: greeter
      type: com.example.Greeter
      properties:
        prefix: "Hey "
    - name: unknown
      type: com.example.NotAvailable
- route:
    from:
      uri: direct:bean
      steps:
        - process:
            ref: greeter
        - to: mock:bean
"#,
    );
    let m = ctx.mock_endpoint("mock:bean").unwrap();
    m.expected_bodies_received(vec!["Hey you"]);
    ctx.create_producer_template().send_body("direct:bean", "you").unwrap();
    m.assert_is_satisfied().unwrap();
    let state = yaml_state(&ctx);
    assert!(state.bean("greeter").unwrap().resolved);
    let unknown = state.bean("unknown").unwrap();
    assert!(!unknown.resolved);
    assert_eq!(unknown.type_name.as_deref(), Some("com.example.NotAvailable"));
}

#[test]
fn bean_factory_on_loader() {
    let ctx = CamelContext::new();
    let mut loader = YamlLoader::new();
    loader.register_bean_factory("my.Type", |ctx: &CamelContext, def: &BeanDefinition| {
        ctx.registry().bind(def.name.clone(), def.properties.get("v").cloned().unwrap_or_default());
        Ok(())
    });
    loader.load_routes(&ctx, "- beans:\n    - name: b1\n      type: my.Type\n      properties: {v: 5}\n").unwrap();
    assert_eq!(*ctx.registry().lookup::<Value>("b1").unwrap(), Value::Int(5));
}

#[test]
fn invalid_yaml_reports_position() {
    let ctx = CamelContext::new();
    let e = load_routes(&ctx, "- route:\n    from:\n      uri: direct:a\n     steps: []\n").unwrap_err();
    assert_eq!(e.kind, kinds::PARSE);
    assert!(e.message.contains("line 4"), "{}", e);
    let e = load_routes(&ctx, "- route:\n    id: \"unterminated\n").unwrap_err();
    assert!(e.message.contains("unterminated double-quoted scalar at line 2, column 9"), "{}", e);
}

#[test]
fn unknown_step_reports_name_and_location() {
    let ctx = CamelContext::new();
    let e = load_routes(
        &ctx,
        r#"- route:
    id: bad
    from:
      uri: direct:a
      steps:
        - to: mock:a
        - frobnicate:
            x: 1
"#,
    )
    .unwrap_err();
    assert!(e.is_a("IllegalArgumentException"), "{}", e.kind);
    assert!(e.message.contains("Unknown step 'frobnicate'"), "{}", e);
    assert!(e.message.contains("[0].route.from.steps[1].frobnicate"), "{}", e);
    assert!(e.message.contains("line 7, column 11"), "{}", e);
    assert!(e.message.contains("setBody"), "should list supported steps: {}", e);
}

#[test]
fn unknown_nested_step_location() {
    let ctx = CamelContext::new();
    let e = load_routes(
        &ctx,
        r#"
- from:
    uri: direct:a
    steps:
      - step:
          steps:
            - setBody:
                constant: x
            - nope: {}
"#,
    )
    .unwrap_err();
    assert!(e.message.contains("from.steps[0].step.steps[1].nope"), "{}", e);
    assert!(e.message.contains("line 9"), "{}", e);
}

#[test]
fn other_dsl_errors() {
    let ctx = CamelContext::new();
    let e = load_routes(&ctx, "- route:\n    id: x\n").unwrap_err();
    assert!(e.message.contains("requires 'from'"), "{}", e);
    let e = load_routes(&ctx, "- whatever: {}\n").unwrap_err();
    assert!(e.message.contains("Unknown top-level element 'whatever'"), "{}", e);
    let e = load_routes(&ctx, "- from:\n    uri: direct:a\n    steps:\n      - setHeader:\n          constant: x\n").unwrap_err();
    assert!(e.message.contains("missing required option 'name'"), "{}", e);
    assert!(e.message.contains("setHeader"), "{}", e);
    let e = load_routes(&ctx, "- from:\n    uri: direct:a\n    steps:\n      - setBody: {}\n").unwrap_err();
    assert!(e.message.contains("missing expression"), "{}", e);
    let e = load_routes(&ctx, "- from:\n    uri: direct:a\n    steps:\n      - to: a\n        id: x\n").unwrap_err();
    assert!(e.message.contains("exactly one key"), "{}", e);
    let e = load_routes(&ctx, "- from:\n    uri: direct:a\n    steps:\n      - log:\n          message: x\n          loggingLevel: LOUD\n").unwrap_err();
    assert!(e.message.contains("invalid loggingLevel 'LOUD'"), "{}", e);
    let e = load_routes(&ctx, "- from:\n    uri: direct:a\n    steps: {to: x}\n").unwrap_err();
    assert!(e.message.contains("Expected a list of steps"), "{}", e);
}

#[test]
fn multi_document_input() {
    let ctx = started(
        r#"
- from:
    uri: direct:doc1
    steps:
      - to: mock:doc1
---
# second document
- route:
    id: doc2
    from:
      uri: direct:doc2
      steps:
        - to: mock:doc2
---
"#,
    );
    let m1 = ctx.mock_endpoint("mock:doc1").unwrap();
    let m2 = ctx.mock_endpoint("mock:doc2").unwrap();
    m1.expected_message_count(1);
    m2.expected_message_count(1);
    let t = ctx.create_producer_template();
    t.send_body("direct:doc1", "a").unwrap();
    t.send_body("direct:doc2", "b").unwrap();
    m1.assert_is_satisfied().unwrap();
    m2.assert_is_satisfied().unwrap();

    let e = load_routes(&CamelContext::new(), "- from:\n    uri: direct:a\n---\n- from:\n    uri: direct:b\n    steps:\n      - bogus: 1\n").unwrap_err();
    assert!(e.message.contains("document[1]"), "{}", e);
    assert!(e.message.contains("line 7"), "{}", e);
}

#[test]
fn auto_startup_and_route_options() {
    let ctx = CamelContext::new();
    ctx.properties().set("start.it", "false");
    add_routes_from_yaml(
        &ctx,
        r#"
- routeConfiguration:
    id: myConfig
- routeConfiguration:
    id: cfg
- route:
    id: lazy
    autoStartup: "{{start.it}}"
    startupOrder: 5
    routeConfigurationId: myConfig
    from:
      uri: direct:lazy
      steps:
        - to: mock:lazy
"#,
    )
    .unwrap();
    ctx.start().unwrap();
    assert_ne!(ctx.route_status("lazy"), Some(ServiceStatus::Started));
    ctx.start_route("lazy").unwrap();
    assert_eq!(ctx.route_status("lazy"), Some(ServiceStatus::Started));
    let routes = load_routes(&ctx, "- route:\n    routeConfigurationId: cfg\n    from: {uri: direct:x}\n").unwrap();
    assert_eq!(routes[0].properties.get("routeConfigurationId").map(|s| s.as_str()), Some("cfg"));
    assert!(routes[0].extension::<Value>("yaml").is_some());
    let e = load_routes(&ctx, "- route:\n    routeConfigurationId: nope\n    from: {uri: direct:y}\n").unwrap_err();
    assert!(e.message.contains("Cannot find routeConfiguration with id 'nope'"), "{}", e);
}

#[test]
fn custom_steps_global_and_per_loader() {
    // global registration (e.g. by another crate after merge)
    register_step("upperCaseTest", |ctx: &mut StepContext<'_>, _cfg: &Value| {
        ctx.route().process(|ex: &mut Exchange| {
            let b: String = ex.body_as()?;
            ex.set_body(b.to_uppercase());
            Ok(())
        });
        Ok(())
    });
    let ctx = started(
        r#"
- from:
    uri: direct:custom
    steps:
      - upperCaseTest: {}
      - to: mock:custom
"#,
    );
    let m = ctx.mock_endpoint("mock:custom").unwrap();
    m.expected_bodies_received(vec!["ABC"]);
    ctx.create_producer_template().send_body("direct:custom", "abc").unwrap();
    m.assert_is_satisfied().unwrap();

    // per-loader block step with nested steps and an expression
    let ctx = CamelContext::new();
    let mut loader = YamlLoader::new();
    loader.register_step("wrapTest", |ctx: &mut StepContext<'_>, cfg: &Value| {
        let e = ctx.expression(cfg)?;
        ctx.add(Box::new(cameleer_core::processor::basic::SetDefinition::header("wrapped", e)));
        ctx.add_block(Box::new(cameleer_core::processor::basic::StepDefinition { id: None, outputs: Default::default() }), cfg.get("steps"))
    });
    loader
        .add_routes(
            &ctx,
            r#"
- from:
    uri: direct:wrap
    steps:
      - wrapTest:
          constant: W
          steps:
            - to: mock:wrap
"#,
        )
        .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:wrap").unwrap();
    m.expected_header_received("wrapped", "W");
    ctx.create_producer_template().send_body("direct:wrap", "x").unwrap();
    m.assert_is_satisfied().unwrap();

    // errors raised inside nested steps of a custom step keep their location
    let e = loader
        .load_routes(&ctx, "- from:\n    uri: direct:w2\n    steps:\n      - wrapTest:\n          constant: W\n          steps:\n            - nope: 1\n")
        .unwrap_err();
    assert!(e.message.contains("wrapTest.steps[0].nope"), "{}", e);
    assert!(e.message.contains("line 7"), "{}", e);
}

#[test]
fn stored_configurations_and_top_level_hook() {
    let ctx = CamelContext::new();
    let mut loader = YamlLoader::new();
    // a rest-like extension generating routes from the raw element
    loader.register_top_level("rest", |tl: &mut TopLevelContext<'_>, v: &Value| {
        let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
        if let Some(Value::Array(gets)) = v.get("get") {
            for g in gets {
                let to = g.get("to").and_then(|t| t.as_str()).ok_or_else(|| tl.error("missing to"))?;
                let sub = g.get("path").and_then(|p| p.as_str()).unwrap_or("");
                let mut def = tl.build_route_from(&format!("direct:rest{}{}", path.replace('/', "-"), sub.replace('/', "-")), &Value::Null)?;
                def.to(to);
                tl.add_route(def);
            }
        }
        Ok(())
    });
    let routes = loader
        .load_routes(
            &ctx,
            r#"
- restConfiguration:
    component: platform-http
    port: 8080
- routeConfiguration:
    id: myConfig
    onException:
      - onException:
          exception: [java.lang.Exception]
          handled: {constant: true}
- errorHandler:
    deadLetterChannel:
      deadLetterUri: mock:dead
- rest:
    path: /api
    get:
      - path: /hello
        to: direct:hello
"#,
        )
        .unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].input, "direct:rest-api-hello");
    let state = yaml_state(&ctx);
    assert_eq!(state.rest_configurations()[0].get("port"), Some(&Value::Int(8080)));
    assert_eq!(state.route_configurations()[0].get("id"), Some(&Value::from("myConfig")));
    assert_eq!(state.error_handlers().len(), 1);
    assert_eq!(state.rests().len(), 1);
}

#[test]
fn xml_routes_end_to_end() {
    let ctx = CamelContext::new();
    add_routes_from_xml(
        &ctx,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<routes xmlns="http://camel.apache.org/schema/spring">
  <route id="xml1">
    <from uri="direct:xml"/>
    <setHeader name="h"><constant>H</constant></setHeader>
    <setBody><simple>${header.h}-${body}</simple></setBody>
    <log message="xml ${body}"/>
    <to uri="mock:xml"/>
  </route>
  <routeTemplate id="xt">
    <templateParameter name="n"/>
    <route>
      <from uri="direct:{{n}}"/>
      <to uri="mock:{{n}}"/>
    </route>
  </routeTemplate>
  <templatedRoute routeTemplateRef="xt" routeId="xtr">
    <parameter name="n" value="xmltpl"/>
  </templatedRoute>
</routes>"#,
    )
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:xml").unwrap();
    m.expected_bodies_received(vec!["H-b"]);
    let t = ctx.create_producer_template();
    t.send_body("direct:xml", "b").unwrap();
    m.assert_is_satisfied().unwrap();
    let mt = ctx.mock_endpoint("mock:xmltpl").unwrap();
    mt.expected_message_count(1);
    t.send_body("direct:xmltpl", "z").unwrap();
    mt.assert_is_satisfied().unwrap();

    let e = load_routes_from_xml(&ctx, "<routes><route><from uri=\"direct:q\"/><bogus/></route></routes>").unwrap_err();
    assert!(e.message.contains("Unknown step 'bogus'"), "{}", e);
}

#[test]
fn dump_routes_round_trip() {
    let yaml = r#"
- route:
    id: dumped
    description: d
    from:
      uri: direct:dump
      steps:
        - setHeader:
            name: h
            constant: x
        - setBody:
            simple: "${body}!"
        - log:
            message: hi
            loggingLevel: WARN
        - step:
            id: s1
            steps:
              - to: mock:dump
        - removeHeaders:
            pattern: "*"
        - convertBodyTo: String
        - stop: {}
"#;
    let ctx = started(yaml);
    let dumped = dump_routes_as_yaml(&ctx);
    assert!(dumped.contains("id: dumped"), "{}", dumped);
    assert!(dumped.contains("uri: direct:dump"), "{}", dumped);
    // dumped YAML loads again and dumps identically
    let ctx2 = CamelContext::new();
    let defs = load_routes(&ctx2, &dumped).unwrap_or_else(|e| panic!("{}\n{}", e, dumped));
    assert_eq!(dump_route_definitions_as_yaml(&defs), dumped);
    let v = parse_yaml(&dumped).unwrap();
    let steps = v.get("0").unwrap().get("route").unwrap().get("from").unwrap().get("steps").unwrap();
    assert_eq!(steps.get("3").unwrap().get("step").unwrap().get("id"), Some(&Value::from("s1")));
}

#[test]
fn load_from_file() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join("camel_yaml_file_test.yaml");
    std::fs::write(&path, "- from:\n    uri: direct:file\n    steps:\n      - to: mock:file\n").unwrap();
    let ctx = CamelContext::new();
    add_routes_from_file(&ctx, &path).unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:file").unwrap();
    m.expected_message_count(1);
    ctx.create_producer_template().send_body("direct:file", "x").unwrap();
    m.assert_is_satisfied().unwrap();

    let bad = dir.join("camel_yaml_bad.yaml");
    std::fs::write(&bad, "- from:\n    uri: direct:x\n    steps:\n      - unknownThing: 1\n").unwrap();
    let e = load_routes_from_file(&CamelContext::new(), &bad).unwrap_err();
    assert!(e.message.contains("camel_yaml_bad.yaml"), "{}", e);

    let xml = dir.join("camel_yaml_routes.xml");
    std::fs::write(&xml, "<routes><route id=\"fx\"><from uri=\"direct:fx\"/><to uri=\"mock:fx\"/></route></routes>").unwrap();
    let defs = load_routes_from_file(&CamelContext::new(), &xml).unwrap();
    assert_eq!(defs[0].id.as_deref(), Some("fx"));
    assert!(load_routes_from_file(&CamelContext::new(), dir.join("does-not-exist.yaml")).is_err());
}

#[test]
fn anchors_and_merge_keys_in_routes() {
    let ctx = started(
        r#"
- route:
    id: anchored
    from:
      uri: direct:anchor
      steps:
        - setHeader: &hdr
            name: common
            constant: shared
        - setHeader:
            <<: *hdr
            name: other
        - to: mock:anchor
"#,
    );
    let m = ctx.mock_endpoint("mock:anchor").unwrap();
    m.expected_header_received("common", "shared");
    m.expected_header_received("other", "shared");
    ctx.create_producer_template().send_body("direct:anchor", "x").unwrap();
    m.assert_is_satisfied().unwrap();
}

#[test]
fn loader_built_route_api() {
    let ctx = CamelContext::new();
    let loader = YamlLoader::new();
    let v = parse_yaml("id: api\nfrom:\n  uri: direct:api\n  steps:\n    - to: mock:api\n").unwrap();
    let def = loader.build_route(&ctx, &v).unwrap();
    assert_eq!(def.id.as_deref(), Some("api"));
    let mut def2 = RouteDefinition::new("direct:api2");
    loader.build_steps(&ctx, &mut def2, &parse_yaml("- to: mock:a\n- to: mock:b\n").unwrap()).unwrap();
    assert_eq!(def2.outputs.len(), 2);
    let names = Arc::new(StepRegistry::core().names());
    for s in ["to", "toD", "setBody", "transform", "setHeader", "setHeaders", "setProperty", "setVariable", "removeHeader", "removeHeaders",
        "removeProperty", "removeProperties", "removeVariable", "convertBodyTo", "convertHeaderTo", "log", "process", "stop",
        "setExchangePattern", "step", "inOnly", "inOut"]
    {
        assert!(names.contains(&s.to_string()), "missing {}", s);
    }
}
