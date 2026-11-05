//! A REST service written in the Rust DSL, using only two crates:
//!
//! ```toml
//! [dependencies]
//! cameleer-core = "0.3"
//! cameleer-http = "0.3"
//! ```
//!
//! Run: `cargo run --release -p cameleer-main --example rest_service`, then
//! `curl localhost:8080/api/greet/Ada` or `curl -XPOST localhost:8080/api/echo -d hi`.

use cameleer_core::prelude::*;
use cameleer_http::rest::RestDslExt;

fn main() -> Result<()> {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    ctx.properties().set("camel.server.port", "8080");
    ctx.add_routes(|b| {
        b.rest("/api/greet").get("/{name}").to("direct:greet");
        b.rest("/api/echo").post("").to("direct:echo");

        b.from("direct:greet")
            .set_header("Content-Type", constant("application/json"))
            .set_body(simple(r#"{"greeting": "Hello, ${header.name}!"}"#));
        b.from("direct:echo").transform(simple("you sent: ${body}"));
    })?;
    ctx.start()?;
    println!("listening on http://localhost:8080/api (Ctrl+C to stop)");
    loop {
        std::thread::park();
    }
}
