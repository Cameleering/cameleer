//! `camel` command line tool (inspired by Camel JBang).
//!
//! ```text
//! camel run routes.yaml [more files...] [--properties application.properties]
//!           [--max-seconds N] [--max-messages N] [--max-idle-seconds N] [--prop key=value]
//! camel catalog [components|languages]
//! camel version
//! ```

use std::process::ExitCode;


fn usage() -> &'static str {
    "Usage:
  camel run <files...> [--properties FILE] [--max-seconds N] [--max-messages N]
                       [--max-idle-seconds N] [--prop key=value] [--log-level LEVEL]
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
        other => Err(format!("Unknown command: {}\n{}", other, usage())),
    }
}
