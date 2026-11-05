//! `cameleer` command line tool (inspired by Camel JBang; also installed as `camel`); see [`cameleer_main::cli`].

use std::process::ExitCode;

/// Cameleer's thread-caching allocator (see `cameleer_core::alloc`).
#[global_allocator]
static GLOBAL: cameleer_core::alloc::CamelAllocator = cameleer_core::alloc::CamelAllocator;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let code = cameleer_main::cli::run_status(&args, &mut out, &mut std::io::stderr());
    ExitCode::from(code as u8)
}
