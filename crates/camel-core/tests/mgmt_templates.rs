use std::collections::BTreeMap;

use camel_core::prelude::*;
use camel_core::template_routes::{TemplateParam, TemplatedRouteBuilder, ROUTE_TEMPLATE_ID};

fn register(ctx: &CamelContext) {
    ctx.add_route_template(
        "greeter",
        &[
            TemplateParam::required("name").with_description("who to greet"),
            TemplateParam::optional("greeting", "Hello"),
            TemplateParam::optional_no_default("suffix"),
        ],
        |b, p| {
            b.from(format!("direct:{}", p.get("name")))
                .set_body(constant(format!("{} {}{}", p.get("greeting"), p.get("name"), p.get_or("suffix", ""))))
                .to(p.resolve("mock:{{name}}"));
        },
    );
}

#[test]
fn routes_from_templates_with_defaults() {
    let ctx = CamelContext::new();
    register(&ctx);
    assert_eq!(ctx.route_template_ids(), vec!["greeter".to_string()]);
    let tpl = ctx.route_template("greeter").unwrap();
    assert_eq!(tpl.parameters.len(), 3);

    let mut p = BTreeMap::new();
    p.insert("name".to_string(), "bob".to_string());
    let id = ctx.add_route_from_template("bobRoute", "greeter", p).unwrap();
    assert_eq!(id, "bobRoute");
    let id2 = TemplatedRouteBuilder::new(&ctx, "greeter")
        .route_id("aliceRoute")
        .parameter("name", "alice")
        .parameter("greeting", "Hi")
        .parameter("suffix", "!")
        .configure(|d| d.description = Some("templated".into()))
        .add()
        .unwrap();
    assert_eq!(id2, "aliceRoute");
    ctx.start().unwrap();
    // also works on a started context, with a generated id
    let generated = TemplatedRouteBuilder::new(&ctx, "greeter").parameter("name", "carol").add().unwrap();
    assert!(generated.starts_with("route"), "{}", generated);
    assert_eq!(ctx.route_status(&generated), Some(ServiceStatus::Started));

    let t = ctx.create_producer_template();
    let bob = ctx.mock_endpoint("mock:bob").unwrap();
    bob.expected_bodies_received(vec!["Hello bob"]);
    let alice = ctx.mock_endpoint("mock:alice").unwrap();
    alice.expected_bodies_received(vec!["Hi alice!"]);
    let carol = ctx.mock_endpoint("mock:carol").unwrap();
    carol.expected_bodies_received(vec!["Hello carol"]);
    t.send_body("direct:bob", "x").unwrap();
    t.send_body("direct:alice", "x").unwrap();
    t.send_body("direct:carol", "x").unwrap();
    bob.assert_is_satisfied().unwrap();
    alice.assert_is_satisfied().unwrap();
    carol.assert_is_satisfied().unwrap();

    let r = ctx.route("aliceRoute").unwrap();
    assert_eq!(r.description.as_deref(), Some("templated"));
    let def = r.definition.lock().unwrap();
    assert_eq!(def.as_ref().unwrap().properties.get(ROUTE_TEMPLATE_ID).map(|s| s.as_str()), Some("greeter"));
    drop(def);
    ctx.stop().unwrap();
}

#[test]
fn template_errors() {
    let ctx = CamelContext::new();
    register(&ctx);
    // missing required parameter
    let err = TemplatedRouteBuilder::new(&ctx, "greeter").route_id("x").add().unwrap_err();
    assert!(err.message.contains("mandatory parameters") && err.message.contains("name"), "{}", err);
    // unknown template
    let err = ctx.add_route_from_template("x", "nope", BTreeMap::new()).unwrap_err();
    assert!(err.message.contains("nope"), "{}", err);
    // duplicate route id
    TemplatedRouteBuilder::new(&ctx, "greeter").route_id("dup").parameter("name", "a").add().unwrap();
    let err = TemplatedRouteBuilder::new(&ctx, "greeter").route_id("dup").parameter("name", "b").add().unwrap_err();
    assert!(err.message.contains("already exists"), "{}", err);
    // a template must define exactly one route
    ctx.add_route_template("two", &[], |b, _| {
        b.from("direct:t1").to("mock:t");
        b.from("direct:t2").to("mock:t");
    });
    assert!(ctx.add_route_from_template("t", "two", BTreeMap::new()).is_err());
    assert!(ctx.remove_route_template("two"));
    assert!(ctx.route_template("two").is_none());
}
