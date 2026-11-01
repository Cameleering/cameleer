//! Route-level tests: filter / choice / setBody / script with the script languages, from the
//! Rust DSL and from YAML; csimple and joor.

mod common;

use camel_core::prelude::*;
use camel_script::{datasonnet, groovy, js, mvel, ognl, python, spel};

fn send(ctx: &CamelContext, uri: &str, body: impl Into<Value>) {
    ctx.create_producer_template().send_body(uri, body).unwrap();
}

fn send_h(ctx: &CamelContext, uri: &str, body: impl Into<Value>, k: &str, v: impl Into<Value>) {
    ctx.create_producer_template().send_body_and_header(uri, body, k, v).unwrap();
}

#[test]
fn rust_dsl_filter_choice_set_body() {
    let ctx = common::ctx();
    ctx.add_routes(|rb| {
    rb.from("direct:filter").filter(groovy("request.headers.amount > 100")).to("mock:big").end().to("mock:all");
    rb.from("direct:choice")
        .choice()
        .when(js("headers.get('type') === 'gold'"))
        .to("mock:gold")
        .when(python("headers['type'] == 'silver'"))
        .to("mock:silver")
        .when(spel("#{request.headers.type == 'bronze'}"))
        .to("mock:bronze")
        .otherwise()
        .to("mock:other")
        .end();
    rb.from("direct:transform")
        .set_body(js("body.toUpperCase()"))
        .set_header("len", groovy("body.size()"))
        .set_header("ognl", ognl("request.body.length() * 2"))
        .set_header("mvel", mvel("request.headers.len + 1"))
        .to("mock:transformed");
    rb.from("direct:ds").set_body(datasonnet("{total: std.foldl(function(t, i) t + i, payload.values, 0)}")).to("mock:ds");
    rb.from("direct:script")
        .process(camel_script::script("groovy", "request.headers.seen = true\nrequest.body = request.body * 2").unwrap())
        .to("mock:script");
    })
    .unwrap();
    ctx.start().unwrap();

    let big = ctx.mock_endpoint("mock:big").unwrap();
    let all = ctx.mock_endpoint("mock:all").unwrap();
    big.expected_bodies_received(vec!["b"]);
    all.expected_message_count(2);
    send_h(&ctx, "direct:filter", "a", "amount", 50);
    send_h(&ctx, "direct:filter", "b", "amount", 500);
    big.assert_is_satisfied().unwrap();
    all.assert_is_satisfied().unwrap();

    for (t, m) in [("gold", "mock:gold"), ("silver", "mock:silver"), ("bronze", "mock:bronze"), ("none", "mock:other")] {
        ctx.mock_endpoint(m).unwrap().expected_bodies_received(vec![t]);
        send_h(&ctx, "direct:choice", t, "type", t);
    }
    for m in ["mock:gold", "mock:silver", "mock:bronze", "mock:other"] {
        ctx.mock_endpoint(m).unwrap().assert_is_satisfied().unwrap();
    }

    let tr = ctx.mock_endpoint("mock:transformed").unwrap();
    tr.expected_bodies_received(vec!["HELLO"]);
    tr.expected_header_received("len", 5);
    tr.expected_header_received("ognl", 10);
    tr.expected_header_received("mvel", 6);
    send(&ctx, "direct:transform", "hello");
    tr.assert_is_satisfied().unwrap();

    let ds = ctx.mock_endpoint("mock:ds").unwrap();
    ds.expected_bodies_received(vec!["{\"total\":6}"]);
    send(&ctx, "direct:ds", r#"{"values": [1, 2, 3]}"#);
    ds.assert_is_satisfied().unwrap();

    let sc = ctx.mock_endpoint("mock:script").unwrap();
    sc.expected_bodies_received(vec!["abab"]);
    sc.expected_header_received("seen", true);
    send(&ctx, "direct:script", "ab");
    sc.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn yaml_routes_with_script_languages() {
    let ctx = common::ctx();
    camel_yaml::add_routes_from_yaml(
        &ctx,
        r##"
- from:
    uri: direct:yfilter
    steps:
      - filter:
          groovy: "request.body.contains('camel')"
          steps:
            - to: mock:yfiltered
- from:
    uri: direct:ychoice
    steps:
      - choice:
          when:
            - js: "headers.get('n') > 10"
              steps:
                - to: mock:ylarge
            - python: "headers['n'] > 5"
              steps:
                - to: mock:ymedium
            - ognl: "request.headers.n > 2"
              steps:
                - to: mock:ysmall
          otherwise:
            steps:
              - to: mock:ytiny
- from:
    uri: direct:ytransform
    steps:
      - setBody:
          spel: "Hello #{request.body}"
      - setHeader:
          name: up
          mvel: "request.body.toUpperCase()"
      - setHeader:
          name: py
          python: "len(body)"
      - to: mock:ytransformed
- from:
    uri: direct:yscript
    steps:
      - script:
          groovy: |
            def items = request.body.split(',')
            request.headers.count = items.size()
            request.body = items.collect { it.trim().toUpperCase() }.join('|')
      - script:
          js: "exchange.getIn().setHeader('js', 'ran'); exchange.setProperty('p', 1)"
      - to: mock:yscript
- from:
    uri: direct:yds
    steps:
      - transform:
          datasonnet:
            expression: "{name: payload.user, upper: std.asciiUpper(payload.user)}"
            outputMediaType: application/x-java-object
      - to: mock:yds
- from:
    uri: direct:ycsimple
    steps:
      - setBody:
          csimple: "Hi ${body}"
      - to: mock:ycsimple
"##,
    )
    .unwrap();
    ctx.start().unwrap();

    let f = ctx.mock_endpoint("mock:yfiltered").unwrap();
    f.expected_bodies_received(vec!["apache camel"]);
    send(&ctx, "direct:yfilter", "apache camel");
    send(&ctx, "direct:yfilter", "other");
    f.assert_is_satisfied().unwrap();

    for (n, m) in [(20, "mock:ylarge"), (7, "mock:ymedium"), (3, "mock:ysmall"), (1, "mock:ytiny")] {
        ctx.mock_endpoint(m).unwrap().expected_message_count(1);
        send_h(&ctx, "direct:ychoice", "x", "n", n);
    }
    for m in ["mock:ylarge", "mock:ymedium", "mock:ysmall", "mock:ytiny"] {
        ctx.mock_endpoint(m).unwrap().assert_is_satisfied().unwrap();
    }

    let t = ctx.mock_endpoint("mock:ytransformed").unwrap();
    t.expected_bodies_received(vec!["Hello World"]);
    t.expected_header_received("up", "HELLO WORLD");
    t.expected_header_received("py", 11);
    send(&ctx, "direct:ytransform", "World");
    t.assert_is_satisfied().unwrap();

    let s = ctx.mock_endpoint("mock:yscript").unwrap();
    s.expected_bodies_received(vec!["A|B|C"]);
    s.expected_header_received("count", 3);
    s.expected_header_received("js", "ran");
    send(&ctx, "direct:yscript", "a, b ,c");
    s.assert_is_satisfied().unwrap();
    let ex = &s.received_exchanges()[0];
    assert_eq!(ex.property("p").unwrap().to_json(), "1");

    let d = ctx.mock_endpoint("mock:yds").unwrap();
    send(&ctx, "direct:yds", r#"{"user": "ann"}"#);
    d.expected_message_count(1);
    d.assert_is_satisfied().unwrap();
    assert_eq!(d.received_exchanges()[0].body().to_json(), r#"{"name":"ann","upper":"ANN"}"#);

    let c = ctx.mock_endpoint("mock:ycsimple").unwrap();
    c.expected_bodies_received(vec!["Hi there"]);
    send(&ctx, "direct:ycsimple", "there");
    c.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn script_errors_fail_the_exchange_and_can_be_caught() {
    let ctx = common::ctx();
    camel_yaml::add_routes_from_yaml(
        &ctx,
        r##"
- from:
    uri: direct:err
    steps:
      - doTry:
          steps:
            - script:
                groovy: "throw new IllegalArgumentException('bad input ' + body)"
            - to: mock:notreached
          doCatch:
            - exception:
                - java.lang.IllegalArgumentException
              steps:
                - setBody:
                    simple: "caught: ${exception.message}"
                - to: mock:caught
"##,
    )
    .unwrap();
    ctx.start().unwrap();
    let c = ctx.mock_endpoint("mock:caught").unwrap();
    c.expected_message_count(1);
    send(&ctx, "direct:err", "x");
    c.assert_is_satisfied().unwrap();
    let body = c.received_exchanges()[0].body().to_string();
    assert!(body.contains("IllegalArgumentException: bad input x"), "{}", body);
    ctx.stop().unwrap();
}

#[test]
fn compile_errors_fail_route_creation() {
    let ctx = common::ctx();
    let r = ctx.add_routes(|rb| {
        rb.from("direct:bad").set_body(js("var = 1")).to("mock:x");
    });
    if let Err(e) = r {
        assert!(e.to_string().contains("JavaScript syntax error at line 1, column 5"), "{}", e);
        return;
    }
    let e = ctx.start().err().map(|e| format!("{}: {}", e.kind, e)).unwrap_or_default();
    assert!(e.contains("JavaScript syntax error at line 1, column 5"), "{}", e);
}

#[test]
fn csimple_and_joor() {
    let ctx = common::ctx();
    let ex = common::exchange(&ctx);
    let v = camel_core::builder::ExpressionDefinition::language("csimple", "${body} / ${header.foo}").evaluate(&ex).unwrap();
    assert_eq!(v.to_string(), "Hello World / bar");
    let p = ctx.resolve_language("csimple").unwrap().create_predicate("${header.num} > 3", &Default::default()).unwrap();
    assert!(p.matches(&ex).unwrap());
    for name in ["joor", "java"] {
        let e = ctx.resolve_language(name).unwrap().create_expression("return \"Hi \" + body;", &Default::default()).unwrap();
        assert_eq!(e.evaluate(&ex).unwrap().to_string(), "Hi Hello World");
    }
    // all names are registered
    let names = ctx.language_names();
    for n in ["js", "javascript", "javaScript", "groovy", "python", "ognl", "mvel", "spel", "datasonnet", "csimple", "joor", "java"] {
        assert!(names.contains(&n.to_string()), "missing language {}", n);
    }
}
