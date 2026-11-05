//! Benchmark harness comparing rust-camel with Apache Camel (Java). See `docs/BENCHMARKS.md`
//! and `scripts/bench.sh`. Std-only: no criterion, so the warmup / timed-iteration driver,
//! the latency histogram, the HTTP load generator and the RSS probe live here.

pub mod breakdown;
pub mod driver;
pub mod hist;
pub mod loadgen;
pub mod measure;
pub mod proc;
pub mod profile;
pub mod rawhttp;
pub mod rawiocp;
pub mod report;
pub mod scenarios;

pub use measure::free_port;
