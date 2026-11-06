//! `cameleer-bench`: the Cameleer side of the benchmark suite, plus the shared tools.
//!
//! ```text
//! cameleer-bench run <scenario> <warmup_s> <iter_s> <iters>   one in-process scenario (or kafka-stream)
//! cameleer-bench http-server <port>                           serve the HTTP route until stdin closes
//! cameleer-bench serve <scenario> <port> <secs>               serve a real-use HTTP scenario on 0.0.0.0
//! cameleer-bench loadgen-remote <host> <port> <scenario> <conns> <warmup_s> <iter_s> <iters>
//!                     load a served scenario on another machine
//! cameleer-bench startup                                      start all routes, print READY, exit
//! cameleer-bench breakdown [n]                                per-stage cost of a direct send
//! cameleer-bench profile <scenario|http> <secs> [top]         sampling profile (Windows)
//! cameleer-bench loadgen <port> <conns> <secs>                load an already running HTTP server
//! cameleer-bench measure --side S --scenario X [--http CONNS] [--repeat N]
//!                     [--warmup s --iter s --iters n] -- <command...>
//! cameleer-bench coldstart [--repeat N] [--ready TEXT] [--kill] -- <command...>
//!                     spawn to ready line / exit, peak RSS of any command (the CLI)
//! cameleer-bench report --results F --machine F [--analysis F] --plan TEXT --out F
//! ```
//! The Java harness (`benchmarks/java-camel`) implements the same `run`, `http-server` and
//! `startup` commands and the same output records.

use std::io::Read;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use cameleer_bench::driver::Plan;
use cameleer_bench::{measure, realworld, report, scenarios};
use cameleer_core::prelude::*;

/// Cameleer's thread-caching allocator (see `cameleer_core::alloc`).
#[global_allocator]
static GLOBAL: cameleer_core::alloc::CamelAllocator = cameleer_core::alloc::CamelAllocator;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = dispatch(&args) {
        eprintln!("cameleer-bench: {e}");
        std::process::exit(1);
    }
}

fn version() {
    println!("VERSION impl=Cameleer camel={}", env!("CARGO_PKG_VERSION"));
}

fn secs(s: &str) -> std::result::Result<Duration, String> {
    s.parse::<f64>().map(Duration::from_secs_f64).map_err(|_| format!("bad seconds: {s}"))
}

fn context(sink: &Arc<AtomicU64>) -> Result<CamelContext> {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    scenarios::add_routes(&ctx, sink)?;
    Ok(ctx)
}

/// A context with the components of the real-use scenarios (HTTP, Kafka, SQL).
fn realworld_context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_kafka_mqtt::register(&ctx);
    cameleer_sql::register(&ctx);
    ctx
}

fn dispatch(args: &[String]) -> std::result::Result<(), String> {
    let s = |e: CamelError| e.to_string();
    match args.first().map(|s| s.as_str()) {
        Some("run") if args.len() == 5 && args[1] == "kafka-stream" => {
            let plan = Plan { warmup: secs(&args[2])?, iteration: secs(&args[3])?, iters: args[4].parse().map_err(|_| "bad iters")? };
            let sink = Arc::new(AtomicU64::new(0));
            let ctx = realworld_context();
            realworld::add_stream_route(&ctx, &realworld::Env::from_env(), &sink).map_err(s)?;
            ctx.start().map_err(s)?;
            version();
            println!("READY");
            let out = realworld::run_stream(&sink, plan);
            println!("{}", out.result_line("kafka-stream"));
            ctx.stop().map_err(s)
        }
        Some("serve") if args.len() == 4 => {
            let port: u16 = args[2].parse().map_err(|_| "bad port")?;
            let ctx = realworld_context();
            realworld::add_server_route(&ctx, &args[1], port, &realworld::Env::from_env()).map_err(s)?;
            ctx.start().map_err(s)?;
            version();
            println!("READY");
            std::thread::sleep(secs(&args[3])?);
            ctx.stop().map_err(s)
        }
        Some("loadgen-remote") if args.len() == 8 => {
            let port: u16 = args[2].parse().map_err(|_| "bad port")?;
            let (path, body) = realworld::served(&args[3]).ok_or("not a served scenario")?;
            let conns: usize = args[4].parse().map_err(|_| "bad conns")?;
            let plan = Plan { warmup: secs(&args[5])?, iteration: secs(&args[6])?, iters: args[7].parse().map_err(|_| "bad iters")? };
            let (out, connects) = cameleer_bench::loadgen::run_remote(&args[1], port, path, body, conns, plan);
            println!("{} connects={connects}", out.result_line(&args[3]));
            Ok(())
        }
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
            cameleer_http::register(&ctx);
            scenarios::add_http_route(&ctx, port).map_err(s)?;
            ctx.start().map_err(s)?;
            version();
            println!("READY");
            // serve until the parent closes stdin
            let _ = std::io::stdin().read_to_end(&mut Vec::new());
            ctx.stop().map_err(s)
        }
        Some("profile") if args.len() >= 3 => {
            // cameleer-bench profile <scenario|http> <secs> [top]
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
                        .aggregate(cameleer_core::builder::header("orderId"), cameleer_core::processor::routing::strategies::grouped_body())
                        .completion_size(scenarios::ITEMS)
                        .process(|_: &mut Exchange| Ok(()))
                        .end();
                })
                .map_err(s)?;
            }
            cameleer_bench::profile::run(secs_v.as_secs_f64() + 1.0, top, move || {
                if scenario == "agg-only" {
                    let t = c2.create_producer_template();
                    let item = cameleer_core::json::parse("{\"sku\":\"SKU-1\",\"qty\":2,\"price\":4.99}").unwrap();
                    let mut i = 0u64;
                    let _ = cameleer_bench::driver::run(plan, 1, None, move |_| {
                        let t = t.clone();
                        let item = item.clone();
                        move || {
                            i += 1;
                            t.send_body_and_headers("direct:agg-only", item.clone(), vec![("orderId", Value::from(i / 10))]).unwrap();
                        }
                    });
                } else if scenario == "http" {
                    let _ = cameleer_bench::loadgen::run(port, "/bench", 16, plan);
                } else {
                    let _ = scenarios::run(&c2, &sink, &scenario, plan);
                }
            })?;
            ctx.stop().map_err(s)
        }
        Some("raw-http") if args.len() == 3 => {
            let port: u16 = args[1].parse().map_err(|_| "bad port")?;
            let r = match args[2].as_str() {
                "poll" => cameleer_bench::rawhttp::poll(port),
                "iocp" => cameleer_bench::rawiocp::iocp(port, 4),
                "iocp2" => cameleer_bench::rawiocp::iocp(port, 2),
                "iocp8" => cameleer_bench::rawiocp::iocp(port, 8),
                "iocp16" => cameleer_bench::rawiocp::iocp(port, 16),
                _ => cameleer_bench::rawhttp::threads(port),
            };
            r.map_err(|e| e.to_string())
        }
        Some("loadgen") if args.len() == 4 => {
            // cameleer-bench loadgen <port> <conns> <secs>: drive an already running HTTP server
            let port: u16 = args[1].parse().map_err(|_| "bad port")?;
            let conns: usize = args[2].parse().map_err(|_| "bad conns")?;
            let plan = Plan { warmup: Duration::from_millis(500), iteration: secs(&args[3])?, iters: 1 };
            let (out, connects) = cameleer_bench::loadgen::run(port, "/bench", conns, plan);
            println!("{} connects={connects}", out.result_line("http"));
            Ok(())
        }
        Some("breakdown") => {
            let n = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(200_000);
            cameleer_bench::breakdown::run(n).map_err(s)
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
        Some("coldstart") => {
            let sep = args.iter().position(|a| a == "--").ok_or("coldstart: missing -- <command>")?;
            let mut opts = cameleer_bench::coldstart::Options { repeat: 10, ready: "READY".into(), kill: false, command: args[sep + 1..].to_vec() };
            let mut i = 1;
            while i < sep {
                match args[i].as_str() {
                    "--kill" => opts.kill = true,
                    "--repeat" => {
                        i += 1;
                        opts.repeat = args.get(i).and_then(|v| v.parse().ok()).ok_or("bad --repeat")?;
                    }
                    "--ready" => {
                        i += 1;
                        opts.ready = args.get(i).cloned().ok_or("missing --ready value")?;
                    }
                    k => return Err(format!("unknown option {k}")),
                }
                i += 1;
            }
            println!("{}", cameleer_bench::coldstart::run(&opts)?);
            Ok(())
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
        _ => Err("usage: cameleer-bench run|http-server|startup|measure|report ... (see src/main.rs)".into()),
    }
}
