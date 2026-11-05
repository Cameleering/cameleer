//! `camel`: the `cameleer` CLI under the name Camel JBang users type; see [`cameleer_main::cli`].

use std::process::ExitCode;

/// Cameleer's thread-caching allocator (see `cameleer_core::alloc`).
#[global_allocator]
static GLOBAL: cameleer_core::alloc::CamelAllocator = cameleer_core::alloc::CamelAllocator;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match cameleer_main::cli::run(&args, &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::FAILURE
        }
    }
}
