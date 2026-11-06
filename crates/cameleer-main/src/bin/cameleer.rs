//! `cameleer` command line tool (inspired by Camel JBang); see [`cameleer_main::cli`].

use std::process::ExitCode;

/// Cameleer's thread-caching allocator (see `cameleer_core::alloc`).
#[global_allocator]
static GLOBAL: cameleer_core::alloc::CamelAllocator = cameleer_core::alloc::CamelAllocator;

fn main() -> ExitCode {
    cameleer_core::startup::mark_process_start();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // AWS Lambda custom runtime: the executable is `bootstrap`, started without arguments in
    // $LAMBDA_TASK_ROOT; run the route files there (see cameleer_main::lambda)
    if args.is_empty() && cameleer_main::cli::invoked_as_bootstrap(std::env::args().next().as_deref()) {
        args.push("run".into());
    }
    // not `stdout().lock()`: `run --mcp-stdio` writes protocol messages from another thread
    let mut out = std::io::stdout();
    let code = cameleer_main::cli::run_status(&args, &mut out, &mut std::io::stderr());
    ExitCode::from(code as u8)
}
