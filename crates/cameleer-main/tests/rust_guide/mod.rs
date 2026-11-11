//! docs/guides/rust.md, "A worked example": the code between the `page:` markers is the
//! page's Rust block, character for character (checked by `rust_guide_matches_the_page`).

// page: rust.md begin
use std::any::Any;
use std::path::Path;
use std::sync::Arc;

use cameleer_core::prelude::*;

/// `greet:NAME?punctuation=!` replaces the body with "Hello, BODY!".
struct GreetComponent;

struct GreetEndpoint {
    uri: String,
    punctuation: String,
}

struct GreetProducer {
    punctuation: String,
}

impl Component for GreetComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, _remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        // take the options you know; an option left over fails, as in Camel
        let punctuation = params.take_string("punctuation", ".");
        Ok(Arc::new(GreetEndpoint { uri: uri.to_string(), punctuation }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Endpoint for GreetEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(GreetProducer { punctuation: self.punctuation.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Service for GreetProducer {}

impl Processor for GreetProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let greeting = format!("Hello, {}{}", exchange.body(), self.punctuation);
        exchange.set_body(greeting);
        Ok(())
    }
}

/// Every built-in component, plus `greet:`, the `upper` processor and the `joinLines`
/// aggregation strategy that the YAML routes in `routes` refer to by name.
fn start(routes: &Path) -> Result<cameleer_main::Main> {
    let mut main = cameleer_main::new_main();
    let ctx = main.context().clone();
    ctx.add_component("greet", GreetComponent);
    ctx.registry().bind_processor("upper", |ex: &mut Exchange| -> Result<()> {
        let upper = ex.body().to_string().to_uppercase();
        ex.set_body(upper);
        Ok(())
    });
    ctx.registry().bind_aggregation_strategy("joinLines", |old: Option<Exchange>, mut new: Exchange| -> Result<Exchange> {
        if let Some(old) = old {
            let joined = format!("{}\n{}", old.body(), new.body());
            new.set_body(joined);
        }
        Ok(new)
    });
    // a Rust DSL route next to the YAML ones
    ctx.add_routes(|b| {
        b.from("direct:hello").to("greet:hello?punctuation=!");
    })?;
    main.add_route_file(routes);
    main.start()?;
    Ok(main)
}
// page: rust.md end

#[test]
fn rust_guide_worked_example() {
    let dir = super::temp_dir("rust-guide");
    let file = dir.join("greetings.camel.yaml");
    std::fs::write(&file, super::block_after(&super::page("rust.md"), "<!-- example: rust-greetings -->")).unwrap();
    let main = start(&file).unwrap();
    let template = main.context().create_producer_template();
    let reply = template.request_body("direct:greetings", vec!["ann", "bob"]).unwrap();
    assert_eq!(reply.to_string(), "Hello, ANN!\nHello, BOB!");
    // the page: an option the component does not take fails, as in Camel
    let e = main.context().get_endpoint("greet:x?colour=red").err().expect("unknown option accepted");
    assert!(e.to_string().contains("colour"), "{}", e);
    main.stop().unwrap();
}

#[test]
fn rust_guide_matches_the_page() {
    let src = std::fs::read_to_string(super::repo().join("crates/cameleer-main/tests/rust_guide/mod.rs")).unwrap().replace("\r\n", "\n");
    let start = src.find("// page: rust.md begin\n").unwrap() + "// page: rust.md begin\n".len();
    let end = src.find("// page: rust.md end").unwrap();
    let text = super::page("rust.md");
    let i = text.find("```rust\nuse std::any::Any;").unwrap() + "```rust\n".len();
    let j = i + text[i..].find("```").unwrap();
    assert_eq!(&text[i..j], &src[start..end], "the Rust example on rust.md differs from tests/rust_guide/mod.rs");
}
