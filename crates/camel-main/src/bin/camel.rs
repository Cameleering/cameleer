//! `camel` command line tool (inspired by Camel JBang); see [`camel_main::cli`].

use std::process::ExitCode;

/// rust-camel's thread-caching allocator (see `camel_core::alloc`).
#[global_allocator]
static GLOBAL: camel_core::alloc::CamelAllocator = camel_core::alloc::CamelAllocator;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match camel_main::cli::run(&args, &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::FAILURE
        }
    }
}
