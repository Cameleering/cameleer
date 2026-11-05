//! `camel-bench`: the rust-camel side of the benchmark suite, plus the shared tools.
//!
//! ```text
//! camel-bench run <scenario> <warmup_s> <iter_s> <iters>   one in-process scenario
//! camel-bench http-server <port>                           serve the HTTP route until stdin closes
//! camel-bench startup                                      start all routes, print READY, exit
//! camel-bench breakdown [n]                                per-stage cost of a direct send
//! camel-bench profile <scenario|http> <secs> [top]         sampling profile (Windows)
//! camel-bench loadgen <port> <conns> <secs>                load an already running HTTP server
//! camel-bench measure --side S --scenario X [--http CONNS] [--repeat N]
//!                     [--warmup s --iter s --iters n] -- <command...>
//! camel-bench report --results F --machine F [--analysis F] --plan TEXT --out F
//! ```
//! The Java harness (`benchmarks/java-camel`) implements the same `run`, `http-server` and
//! `startup` commands and the same output records.

use std::io::Read;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use camel_bench::driver::Plan;
use camel_bench::{measure, report, scenarios};
use camel_core::prelude::*;

/// rust-camel's thread-caching allocator (see `camel_core::alloc`).
#[global_allocator]
static GLOBAL: camel_core::alloc::CamelAllocator = camel_core::alloc::CamelAllocator;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = dispatch(&args) {
        eprintln!("camel-bench: {e}");
        std::process::exit(1);
    }
}

fn version() {
    println!("VERSION impl=rust-camel camel={}", env!("CARGO_PKG_VERSION"));
}

fn secs(s: &str) -> std::result::Result<Duration, String> {
    s.parse::<f64>().map(Duration::from_secs_f64).map_err(|_| format!("bad seconds: {s}"))
}

fn context(sink: &Arc<AtomicU64>) -> Result<CamelContext> {
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    scenarios::add_routes(&ctx, sink)?;
    Ok(ctx)
}

fn dispatch(args: &[String]) -> std::result::Result<(), String> {
    let s = |e: CamelError| e.to_string();
    match args.first().map(|s| s.as_str()) {
        Some("run") if args.len() == 5 => {
            let plan = Plan { warmup: secs(&args[2])?, iteration: secs(&args[3])?, iters: args[4].parse().map_err(|_| "bad iters")? };
            let sink = Arc::new(AtomicU64::new(0));
            let ctx = context(&sink).map_err(s)?;
            ctx.start().map_err(s)?;
            version();
            println!("READY");
            let out = scenarios::run(&ctx, &sink, &args[1], plan).map_err(s)?;
            println!("{}", out.result_line(&args[1]));
            ctx.stop().map_err(s)
        }
        Some("http-server") if args.len() == 2 => {
            let port: u16 = args[1].parse().map_err(|_| "bad port")?;
            let ctx = CamelContext::new();
            camel_http::register(&ctx);
            scenarios::add_http_route(&ctx, port).map_err(s)?;
            ctx.start().map_err(s)?;
            version();
            println!("READY");
            // serve until the parent closes stdin
            let _ = std::io::stdin().read_to_end(&mut Vec::new());
            ctx.stop().map_err(s)
        }
        Some("profile") if args.len() >= 3 => {
            // camel-bench profile <scenario|http> <secs> [top]
            let secs_v = secs(&args[2])?;
            let top = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(40);
            let scenario = args[1].clone();
            let sink = Arc::new(AtomicU64::new(0));
            let ctx = context(&sink).map_err(s)?;
            let port = measure::free_port();
            if scenario == "http" {
                scenarios::add_http_route(&ctx, port).map_err(s)?;
            }
            ctx.start().map_err(s)?;
            let plan = Plan { warmup: Duration::from_millis(500), iteration: secs_v, iters: 1 };
            let c2 = ctx.clone();
            if scenario == "agg-only" {
                // profiling aid: the aggregator alone, fed one item at a time
                ctx.add_routes(|b| {
                    b.from("direct:agg-only")
                        .aggregate(camel_core::builder::header("orderId"), camel_core::processor::routing::strategies::grouped_body())
                        .completion_size(scenarios::ITEMS)
                        .process(|_: &mut Exchange| Ok(()))
                        .end();
                })
                .map_err(s)?;
            }
            camel_bench::profile::run(secs_v.as_secs_f64() + 1.0, top, move || {
                if scenario == "agg-only" {
                    let t = c2.create_producer_template();
                    let item = camel_core::json::parse("{\"sku\":\"SKU-1\",\"qty\":2,\"price\":4.99}").unwrap();
                    let mut i = 0u64;
                    let _ = camel_bench::driver::run(plan, 1, None, move |_| {
                        let t = t.clone();
                        let item = item.clone();
                        move || {
                            i += 1;
                            t.send_body_and_headers("direct:agg-only", item.clone(), vec![("orderId", Value::from(i / 10))]).unwrap();
                        }
                    });
                } else if scenario == "http" {
                    let _ = camel_bench::loadgen::run(port, "/bench", 16, plan);
                } else {
                    let _ = scenarios::run(&c2, &sink, &scenario, plan);
                }
            })?;
            ctx.stop().map_err(s)
        }
        Some("raw-http") if args.len() == 3 => {
            let port: u16 = args[1].parse().map_err(|_| "bad port")?;
            let r = match args[2].as_str() {
                "poll" => camel_bench::rawhttp::poll(port),
                "iocp" => camel_bench::rawiocp::iocp(port, 4),
                "iocp2" => camel_bench::rawiocp::iocp(port, 2),
                "iocp8" => camel_bench::rawiocp::iocp(port, 8),
                "iocp16" => camel_bench::rawiocp::iocp(port, 16),
                _ => camel_bench::rawhttp::threads(port),
            };
            r.map_err(|e| e.to_string())
        }
        Some("loadgen") if args.len() == 4 => {
            // camel-bench loadgen <port> <conns> <secs>: drive an already running HTTP server
            let port: u16 = args[1].parse().map_err(|_| "bad port")?;
            let conns: usize = args[2].parse().map_err(|_| "bad conns")?;
            let plan = Plan { warmup: Duration::from_millis(500), iteration: secs(&args[3])?, iters: 1 };
            let (out, connects) = camel_bench::loadgen::run(port, "/bench", conns, plan);
            println!("{} connects={connects}", out.result_line("http"));
            Ok(())
        }
        Some("breakdown") => {
            let n = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(200_000);
            camel_bench::breakdown::run(n).map_err(s)
        }
        Some("startup") => {
            let sink = Arc::new(AtomicU64::new(0));
            let ctx = context(&sink).map_err(s)?;
            scenarios::add_http_route(&ctx, measure::free_port()).map_err(s)?;
            ctx.start().map_err(s)?;
            println!("READY");
            version();
            ctx.stop().map_err(s)
        }
        Some("measure") => {
            let sep = args.iter().position(|a| a == "--").ok_or("measure: missing -- <command>")?;
            let mut opts = measure::Options {
                side: String::new(),
                scenario: String::new(),
                http_conns: None,
                plan: Plan { warmup: Duration::from_secs(10), iteration: Duration::from_secs(5), iters: 5 },
                repeat: 1,
                command: args[sep + 1..].to_vec(),
            };
            let mut it = args[1..sep].iter();
            while let Some(k) = it.next() {
                let v = it.next().ok_or_else(|| format!("missing value for {k}"))?;
                match k.as_str() {
                    "--side" => opts.side = v.clone(),
                    "--scenario" => opts.scenario = v.clone(),
                    "--http" => opts.http_conns = Some(v.parse().map_err(|_| "bad --http")?),
                    "--repeat" => opts.repeat = v.parse().map_err(|_| "bad --repeat")?,
                    "--warmup" => opts.plan.warmup = secs(v)?,
                    "--iter" => opts.plan.iteration = secs(v)?,
                    "--iters" => opts.plan.iters = v.parse().map_err(|_| "bad --iters")?,
                    _ => return Err(format!("unknown option {k}")),
                }
            }
            if opts.command.is_empty() {
                return Err("measure: empty command".into());
            }
            measure::run(&opts)
        }
        Some("report") => {
            let get = |name: &str| -> Option<String> {
                args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
            };
            let results = get("--results").ok_or("report: --results required")?;
            let machine = get("--machine").ok_or("report: --machine required")?;
            let out = get("--out").ok_or("report: --out required")?;
            let read = |p: &str| std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"));
            let records: Vec<report::Record> = read(&results)?.lines().filter_map(report::Record::parse).collect();
            let machine = read(&machine)?
                .lines()
                .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
                .filter(|(_, v)| !v.is_empty())
                .collect();
            let analysis = match get("--analysis") {
                Some(p) if std::path::Path::new(&p).exists() => read(&p)?,
                _ => String::new(),
            };
            let ctx = report::Context { machine, plan: get("--plan").unwrap_or_default(), analysis };
            std::fs::write(&out, report::render(&records, &ctx)).map_err(|e| format!("{out}: {e}"))
        }
        _ => Err("usage: camel-bench run|http-server|startup|measure|report ... (see src/main.rs)".into()),
    }
}
