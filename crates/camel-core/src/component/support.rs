//! Shared building blocks for components: promptly stoppable background threads,
//! deadline helpers and consumer-side exchange processing.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::context::WeakContext;
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::Processor;

/// A stop signal that sleeping threads can wait on; `stop()` wakes them immediately.
#[derive(Default)]
pub struct StopFlag {
    stopped: Mutex<bool>,
    cond: Condvar,
}

impl StopFlag {
    pub fn new() -> Arc<StopFlag> {
        Arc::new(StopFlag::default())
    }
    /// Signal stop and wake up all waiters.
    pub fn stop(&self) {
        *self.stopped.lock().unwrap() = true;
        self.cond.notify_all();
    }
    /// Clear the stop signal (before restarting).
    pub fn reset(&self) {
        *self.stopped.lock().unwrap() = false;
    }
    pub fn is_stopped(&self) -> bool {
        *self.stopped.lock().unwrap()
    }
    /// Sleep for `d` or until stopped. Returns true if stopped.
    pub fn sleep(&self, d: Duration) -> bool {
        match deadline_after(d) {
            Some(deadline) => self.sleep_until(deadline),
            None => {
                let mut s = self.stopped.lock().unwrap();
                while !*s {
                    s = self.cond.wait(s).unwrap();
                }
                true
            }
        }
    }
    /// Sleep until `deadline` or until stopped. Returns true if stopped.
    pub fn sleep_until(&self, deadline: Instant) -> bool {
        let mut s = self.stopped.lock().unwrap();
        loop {
            if *s {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            s = self.cond.wait_timeout(s, deadline - now).unwrap().0;
        }
    }
}

/// `Instant::now() + d`, or None when that would overflow (i.e. "forever").
pub fn deadline_after(d: Duration) -> Option<Instant> {
    Instant::now().checked_add(d)
}

/// A set of background threads sharing one stop flag. `stop()` signals and joins them
/// (unless called from one of the threads itself, which would deadlock).
#[derive(Default)]
pub struct BackgroundThreads {
    flag: Mutex<Option<Arc<StopFlag>>>,
    handles: Mutex<Vec<JoinHandle<()>>>,
}

impl BackgroundThreads {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current stop flag (a fresh one is created on first use / after stop).
    pub fn flag(&self) -> Arc<StopFlag> {
        self.flag.lock().unwrap().get_or_insert_with(StopFlag::new).clone()
    }

    /// Spawn a named thread running `f(stop_flag)`.
    pub fn spawn(&self, name: &str, f: impl FnOnce(Arc<StopFlag>) + Send + 'static) -> Result<()> {
        let flag = self.flag();
        let h = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || f(flag))
            .map_err(|e| CamelError::illegal_state(format!("Cannot spawn thread {}: {}", name, e)))?;
        self.handles.lock().unwrap().push(h);
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.handles.lock().unwrap().iter().any(|h| !h.is_finished())
    }

    /// Signal stop and join all threads.
    pub fn stop(&self) {
        let flag = self.flag.lock().unwrap().take();
        if let Some(f) = &flag {
            f.stop();
        }
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *self.handles.lock().unwrap());
        let me = std::thread::current().id();
        for h in handles {
            if h.thread().id() == me {
                continue; // stopping from inside the consumer thread: let it finish on its own
            }
            let _ = h.join();
        }
    }
}

impl Drop for BackgroundThreads {
    fn drop(&mut self) {
        if let Ok(f) = self.flag.lock() {
            if let Some(f) = f.as_ref() {
                f.stop();
            }
        }
    }
}

/// Create a new exchange for a consumer, bound to the context if still alive.
pub fn new_exchange(ctx: &WeakContext) -> Exchange {
    Exchange::new(ctx.upgrade())
}

/// Feed an exchange created by a consumer into the route processor; failures that
/// were not handled are logged (Camel's `LoggingExceptionHandler`). Returns true if
/// the exchange completed without exception.
pub fn process_consumer_exchange(logger: &str, processor: &dyn Processor, exchange: &mut Exchange) -> bool {
    if let Err(e) = processor.process(exchange) {
        exchange.set_exception(e);
    }
    if let Some(e) = &exchange.exception {
        crate::camel_warn!(
            logger,
            "Error processing exchange. Exchange[{}]. Caused by: [{} - {}]",
            exchange.exchange_id,
            e.kind,
            e.message
        );
        false
    } else {
        true
    }
}

/// Copy the result of a processed exchange back to the original (Camel `ExchangeHelper.copyResults`).
pub fn copy_results(target: &mut Exchange, source: &Exchange) {
    target.message = source.message.clone();
    target.properties = source.properties.clone();
    target.exception = source.exception.clone();
    target.rollback_only = source.rollback_only;
}

/// Parse a Java `TimeUnit` name into a multiplier in milliseconds (fractional for sub-millis).
pub fn time_unit_millis(unit: &str) -> Result<f64> {
    Ok(match unit.to_ascii_uppercase().as_str() {
        "NANOSECONDS" => 0.000_001,
        "MICROSECONDS" => 0.001,
        "MILLISECONDS" => 1.0,
        "SECONDS" => 1000.0,
        "MINUTES" => 60_000.0,
        "HOURS" => 3_600_000.0,
        "DAYS" => 86_400_000.0,
        other => return Err(CamelError::illegal_argument(format!("Invalid timeUnit: {}", other))),
    })
}

/// Parse a duration option value: a plain (possibly negative) number is interpreted in
/// `unit_ms` milliseconds, otherwise the value may carry a unit suffix (`5s`). Negative
/// numbers are clamped to zero.
pub fn parse_duration_with_unit(value: &str, unit_ms: f64) -> Result<Duration> {
    let v = value.trim();
    if let Ok(n) = v.parse::<f64>() {
        if n <= 0.0 {
            return Ok(Duration::ZERO);
        }
        return Ok(Duration::from_micros((n * unit_ms * 1000.0) as u64));
    }
    crate::util::parse_duration(v)
}

/// Split a command line style string into arguments, honouring double and single quotes.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                has = true;
            }
            None if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            None => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_flag_wakes_sleepers() {
        let threads = BackgroundThreads::new();
        threads
            .spawn("t", |flag| {
                flag.sleep(Duration::from_secs(60));
            })
            .unwrap();
        let start = Instant::now();
        threads.stop();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(!threads.is_running());
    }

    #[test]
    fn args_split() {
        assert_eq!(split_args("a \"b c\" d"), vec!["a", "b c", "d"]);
        assert_eq!(split_args("  x  ''  "), vec!["x", ""]);
    }

    #[test]
    fn durations_with_unit() {
        assert_eq!(parse_duration_with_unit("2", 1000.0).unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration_with_unit("-1", 1.0).unwrap(), Duration::ZERO);
        assert_eq!(parse_duration_with_unit("3s", 1.0).unwrap(), Duration::from_secs(3));
    }
}
