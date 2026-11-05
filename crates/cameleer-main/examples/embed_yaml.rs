//! Embed Cameleer in your own Rust program and run Camel YAML routes with every component
//! (Kafka, HTTP, SQL, AWS, OpenAI, ...):
//!
//! ```toml
//! [dependencies]
//! cameleer-main = "0.3"
//! ```
//!
//! Run: `cargo run --release -p cameleer-main --example embed_yaml -- examples/rest-api.camel.yaml`

use std::process::ExitCode;

fn main() -> ExitCode {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: embed_yaml <route.camel.yaml>...");
        return ExitCode::FAILURE;
    }
    let mut main = cameleer_main::new_main();
    // configuration as in application.properties: camel.main.*, camel.component.*, ...
    main.set_property("camel.main.name", "my-integration");
    for f in files {
        main.add_route_file(f);
    }
    // runs until Ctrl+C (or camel.main.duration-max-seconds and friends)
    match main.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::FAILURE
        }
    }
}
