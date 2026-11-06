//! `cameleer-bench coldstart`: cold start of any command, measured from the outside.
//!
//! The command is spawned `repeat` times. For each run it records the time from spawn to the
//! first output line (stdout or stderr) containing the ready marker, the time from spawn to
//! process exit, and the peak RSS. With `--kill` the process is killed once ready (for
//! servers that do not exit by themselves). Used to measure the `cameleer` CLI itself
//! (`cameleer run route.yaml`, ready marker `Routes startup`), which the in-process
//! `startup` scenario does not cover: argument parsing, component registration, route file
//! loading and the local status files.
//!
//! Output: `COLDSTART ready_ms=<median> exit_ms=<median> peak_kb=<max> ready_runs=a,b,...`.

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub struct Options {
    pub repeat: usize,
    pub ready: String,
    pub kill: bool,
    pub command: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Run {
    pub ready_ms: Option<f64>,
    pub exit_ms: f64,
    pub peak_kb: u64,
}

/// Median of a non-empty slice (upper median for even lengths).
pub fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[s.len() / 2]
}

fn forward<R: Read + Send + 'static>(r: R, marker: String, tx: mpsc::Sender<Instant>) {
    std::thread::spawn(move || {
        let mut sent = false;
        for line in BufReader::new(r).lines() {
            let Ok(line) = line else { break };
            if !sent && line.contains(&marker) {
                sent = true;
                let _ = tx.send(Instant::now());
            }
        }
    });
}

/// Run the command once.
pub fn run_once(opts: &Options) -> Result<Run, String> {
    let t0 = Instant::now();
    let mut child = Command::new(&opts.command[0])
        .args(&opts.command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", opts.command[0]))?;
    let (tx, rx) = mpsc::channel();
    forward(child.stdout.take().unwrap(), opts.ready.clone(), tx.clone());
    forward(child.stderr.take().unwrap(), opts.ready.clone(), tx);
    let ready = loop {
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(t) => break Some(t),
            Err(mpsc::RecvTimeoutError::Disconnected) => break None,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                    // exited: take a marker that may still be in flight
                    break rx.recv_timeout(Duration::from_millis(200)).ok();
                }
                if t0.elapsed() > Duration::from_secs(60) {
                    let _ = child.kill();
                    return Err("no ready line within 60 s".into());
                }
            }
        }
    };
    if opts.kill && ready.is_some() {
        let _ = child.kill();
    }
    child.wait().map_err(|e| e.to_string())?;
    let exit_ms = t0.elapsed().as_secs_f64() * 1000.0;
    Ok(Run {
        ready_ms: ready.map(|t| t.duration_since(t0).as_secs_f64() * 1000.0),
        exit_ms,
        peak_kb: crate::proc::peak_rss_kb(&child).unwrap_or(0),
    })
}

pub fn run(opts: &Options) -> Result<String, String> {
    if opts.command.is_empty() {
        return Err("coldstart: empty command".into());
    }
    let mut runs = Vec::new();
    for _ in 0..opts.repeat.max(1) {
        runs.push(run_once(opts)?);
    }
    let ready: Vec<f64> = runs.iter().filter_map(|r| r.ready_ms).collect();
    if ready.len() != runs.len() {
        return Err(format!("ready marker {:?} not seen in {} of {} runs", opts.ready, runs.len() - ready.len(), runs.len()));
    }
    let exits: Vec<f64> = runs.iter().map(|r| r.exit_ms).collect();
    let peak = runs.iter().map(|r| r.peak_kb).max().unwrap_or(0);
    let all: Vec<String> = ready.iter().map(|v| format!("{v:.2}")).collect();
    Ok(format!(
        "COLDSTART ready_ms={:.2} min_ready_ms={:.2} exit_ms={:.2} peak_kb={peak} ready_runs={}",
        median(&ready),
        ready.iter().cloned().fold(f64::MAX, f64::min),
        median(&exits),
        all.join(",")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_odd_and_even() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), 3.0);
    }

    #[test]
    fn measures_a_command_that_prints_the_marker() {
        // the test binary lists its tests when asked; the marker is in that listing
        let exe = std::env::current_exe().unwrap().to_string_lossy().to_string();
        let opts = Options { repeat: 2, ready: "median_of_odd_and_even".into(), kill: false, command: vec![exe, "--list".into()] };
        let line = run(&opts).unwrap();
        assert!(line.starts_with("COLDSTART ready_ms="), "{line}");
        assert!(line.contains("peak_kb="), "{line}");
    }

    #[test]
    fn a_missing_marker_is_an_error() {
        let exe = std::env::current_exe().unwrap().to_string_lossy().to_string();
        let opts = Options { repeat: 1, ready: "no such marker anywhere".into(), kill: false, command: vec![exe, "--list".into()] };
        assert!(run(&opts).is_err());
    }
}
