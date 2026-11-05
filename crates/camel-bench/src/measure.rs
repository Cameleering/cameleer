//! `camel-bench measure`: runs one benchmark process (Rust or Java) and measures it from the
//! outside the same way for both: time from spawn to its `READY` line (startup), peak RSS,
//! and for the HTTP scenario the load generated against it once it is ready.
//!
//! Protocol spoken by the benchmark processes on stdout (one record per line):
//! `READY` once the CamelContext is started, `RESULT scenario=... k=v...` per scenario,
//! `VERSION k=v...` with runtime versions. The HTTP server mode serves until stdin closes.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::driver::Plan;
use crate::loadgen;

pub struct Options {
    pub side: String,
    pub scenario: String,
    /// Run the HTTP load generator with this many connections once the child is ready.
    pub http_conns: Option<usize>,
    pub plan: Plan,
    /// How many times to run the child (startup is measured several times).
    pub repeat: usize,
    pub command: Vec<String>,
}

/// Prefix a child's record with the side: `RESULT x=1` -> `RESULT side=rust x=1`.
pub fn tag(line: &str, side: &str) -> String {
    match line.split_once(' ') {
        Some((kind, rest)) => format!("{kind} side={side} {rest}"),
        None => format!("{line} side={side}"),
    }
}

/// A free loopback port (bound and released; good enough for a benchmark run).
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).expect("free port")
}

pub fn run(opts: &Options) -> Result<(), String> {
    let mut startups = Vec::new();
    let mut peak = 0u64;
    for _ in 0..opts.repeat.max(1) {
        let port = free_port();
        let args: Vec<String> = opts.command.iter().map(|a| a.replace("{port}", &port.to_string())).collect();
        let t0 = Instant::now();
        let mut child = Command::new(&args[0])
            .args(&args[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", args[0]))?;
        let mut stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut ready = false;
        for line in stdout.lines() {
            let line = line.map_err(|e| e.to_string())?;
            let line = line.trim();
            if line == "READY" || line.starts_with("READY ") {
                ready = true;
                startups.push(t0.elapsed().as_secs_f64() * 1000.0);
                if let Some(conns) = opts.http_conns {
                    // give the server's acceptor a moment, then load it and let it exit
                    std::thread::sleep(Duration::from_millis(200));
                    let (out, connects) = loadgen::run(port, "/bench", conns, opts.plan);
                    println!("{} connects={connects}", tag(&out.result_line(&opts.scenario), &opts.side));
                    stdin = None;
                }
            } else if line.starts_with("RESULT ") || line.starts_with("VERSION ") {
                println!("{}", tag(line, &opts.side));
            }
        }
        drop(stdin);
        let status = child.wait().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("{} {} exited with {status}", opts.side, opts.scenario));
        }
        if !ready {
            return Err(format!("{} {} never printed READY", opts.side, opts.scenario));
        }
        peak = peak.max(crate::proc::peak_rss_kb(&child).unwrap_or(0));
    }
    if peak > 0 {
        println!("RSS side={} scenario={} peak_kb={peak}", opts.side, opts.scenario);
    }
    if opts.scenario == "startup" {
        let mut s = startups.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = s[s.len() / 2];
        let all: Vec<String> = startups.iter().map(|v| format!("{v:.0}")).collect();
        println!("STARTUP side={} ms={median:.1} runs={}", opts.side, all.join(","));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_records_with_the_side() {
        assert_eq!(tag("RESULT scenario=x tput=1", "java"), "RESULT side=java scenario=x tput=1");
        assert_eq!(tag("READY", "rust"), "READY side=rust");
    }
}
