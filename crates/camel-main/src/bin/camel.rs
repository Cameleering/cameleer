//! `camel` command line tool (inspired by Camel JBang).
//!
//! ```text
//! camel run routes.yaml [more files...] [--properties application.properties]
//!           [--max-seconds N] [--max-messages N] [--max-idle-seconds N] [--prop key=value]
//!           [--console] [--port N] [--trace]
//! camel get routes|context|health [--url http://localhost:8080]
//! camel catalog [components|languages]
//! camel version
//! ```

use std::process::ExitCode;


fn usage() -> &'static str {
    "Usage:
  camel run <files...> [--properties FILE] [--max-seconds N] [--max-messages N]
                       [--max-idle-seconds N] [--prop key=value] [--log-level LEVEL]
                       [--console] [--port N] [--trace]
  camel get routes|context|health [--url http://localhost:8080]
  camel catalog [components|languages]
  camel version
  camel help"
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    match cmd {
        "version" | "--version" | "-v" => {
            println!("rust-camel {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!("{}", usage());
            Ok(())
        }
        "catalog" => {
            let main = camel_main::new_main();
            let what = args.get(1).map(|s| s.as_str()).unwrap_or("components");
            let ctx = main.context();
            let items = match what {
                "components" => ctx.component_names(),
                "languages" => ctx.language_names(),
                other => return Err(format!("Unknown catalog kind: {}", other)),
            };
            for i in items {
                println!("{}", i);
            }
            Ok(())
        }
        "run" => {
            let mut main = camel_main::new_main();
            let mut i = 1;
            let mut files = 0;
            while i < args.len() {
                let a = &args[i];
                let mut next = || -> Result<String, String> {
                    i += 1;
                    args.get(i).cloned().ok_or_else(|| format!("Missing value for {}", a))
                };
                match a.as_str() {
                    "--properties" => {
                        let f = next()?;
                        main.load_properties_file(&f).map_err(|e| e.to_string())?;
                    }
                    "--max-seconds" => main.set_property("camel.main.duration-max-seconds", next()?),
                    "--max-messages" => main.set_property("camel.main.duration-max-messages", next()?),
                    "--max-idle-seconds" => main.set_property("camel.main.duration-max-idle-seconds", next()?),
                    "--log-level" => main.set_property("camel.main.log-level", next()?),
                    "--console" => {
                        for k in ["camel.server.enabled", "camel.server.devConsoleEnabled", "camel.server.healthCheckEnabled", "camel.server.metricsEnabled"] {
                            main.set_property(k, "true");
                        }
                    }
                    "--port" => {
                        let p = next()?;
                        p.parse::<u16>().map_err(|_| format!("Invalid port: {}", p))?;
                        main.set_property("camel.server.port", p);
                    }
                    "--trace" => main.set_property("camel.main.tracing", "true"),
                    "--prop" => {
                        let kv = next()?;
                        let (k, v) = kv.split_once('=').ok_or_else(|| format!("--prop expects key=value, got {}", kv))?;
                        main.set_property(k, v);
                    }
                    f if f.starts_with("--") => return Err(format!("Unknown option {}\n{}", f, usage())),
                    f => {
                        main.add_route_file(f);
                        files += 1;
                    }
                }
                i += 1;
            }
            if files == 0 && std::path::Path::new("application.properties").exists() {
                main.load_properties_file("application.properties").map_err(|e| e.to_string())?;
            }
            main.run().map_err(|e| e.to_string())
        }
        "get" => get(&args[1..]),
        other => Err(format!("Unknown command: {}\n{}", other, usage())),
    }
}

/// `camel get routes|context|health --url URL` (like Camel JBang `camel get`).
fn get(args: &[String]) -> Result<(), String> {
    let what = args.first().map(|s| s.as_str()).unwrap_or("routes");
    let mut url = "http://localhost:8080".to_string();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--url" => {
                i += 1;
                url = args.get(i).cloned().ok_or("Missing value for --url")?;
            }
            other => return Err(format!("Unknown option {}\n{}", other, usage())),
        }
        i += 1;
    }
    let url = url.trim_end_matches('/').to_string();
    let path = match what {
        "routes" | "route" => "/q/dev/routes",
        "context" => "/q/dev/context",
        "health" => "/q/health",
        other => return Err(format!("Unknown get kind: {} (use routes, context or health)", other)),
    };
    let client = camel_http::HttpClient { connect_timeout: std::time::Duration::from_secs(5), ..Default::default() };
    let resp = client.get(&format!("{}{}", url, path)).map_err(|e| e.to_string())?;
    if resp.status != 200 && !(what == "health" && resp.status == 503) {
        return Err(format!("{} returned HTTP {}: {}", path, resp.status, resp.body_text()));
    }
    let v = camel_core::json::parse(&resp.body_text()).map_err(|e| e.to_string())?;
    match what {
        "routes" | "route" => print!("{}", camel_main::console::format_routes_table(&v)),
        _ => println!("{}", v.to_json_pretty()),
    }
    Ok(())
}
