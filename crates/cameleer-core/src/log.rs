//! Minimal logging facility (no external crates). Log records go to stderr by
//! default; a custom sink can be installed (useful for tests and the CLI).
//! The level threshold comes from `CAMEL_LOG_LEVEL` (TRACE, DEBUG, INFO, WARN, ERROR, OFF).

use std::sync::{OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Off = 5,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        Some(match s.trim().to_ascii_uppercase().as_str() {
            "TRACE" => Level::Trace,
            "DEBUG" => Level::Debug,
            "INFO" => Level::Info,
            "WARN" | "WARNING" => Level::Warn,
            "ERROR" => Level::Error,
            "OFF" => Level::Off,
            _ => return None,
        })
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Off => "OFF",
        }
    }
}

#[derive(Clone, Debug)]
pub struct LogRecord {
    pub level: Level,
    pub logger: String,
    pub message: String,
    pub timestamp_millis: u128,
}

type Sink = Box<dyn Fn(&LogRecord) + Send + Sync>;

static LEVEL: OnceLock<RwLock<Level>> = OnceLock::new();
static SINKS: RwLock<Vec<Sink>> = RwLock::new(Vec::new());
static STDERR_ENABLED: RwLock<bool> = RwLock::new(true);

fn level_lock() -> &'static RwLock<Level> {
    LEVEL.get_or_init(|| {
        let lvl = std::env::var("CAMEL_LOG_LEVEL").ok().and_then(|s| Level::parse(&s)).unwrap_or(Level::Info);
        RwLock::new(lvl)
    })
}

/// Current global threshold.
pub fn level() -> Level {
    *level_lock().read().unwrap()
}

/// Set the global threshold.
pub fn set_level(level: Level) {
    *level_lock().write().unwrap() = level;
    THRESHOLD.store(level as u8, std::sync::atomic::Ordering::Relaxed);
}

/// The threshold as a number, for the inline check of the `camel_*!` macros (`u8::MAX` until
/// first read).
static THRESHOLD: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// Whether records of `level` are logged: one relaxed atomic load (debug and trace
/// statements sit on hot paths).
#[inline]
pub fn enabled(level: Level) -> bool {
    let mut t = THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
    if t == u8::MAX {
        t = self::level() as u8;
        THRESHOLD.store(t, std::sync::atomic::Ordering::Relaxed);
    }
    level != Level::Off && level as u8 >= t
}

/// Install an additional log sink receiving every record that passes the threshold.
pub fn add_sink(f: impl Fn(&LogRecord) + Send + Sync + 'static) {
    SINKS.write().unwrap().push(Box::new(f));
}

/// Enable/disable writing to stderr.
pub fn set_stderr_enabled(enabled: bool) {
    *STDERR_ENABLED.write().unwrap() = enabled;
}

/// Format epoch millis as `YYYY-MM-DD HH:MM:SS.mmm` in the default time zone
/// ([`crate::tz::TimeZone::local`]: the `TZ` environment variable, `/etc/localtime`, else UTC).
pub fn format_timestamp(millis: u128) -> String {
    let secs = (millis / 1000) as i64;
    let ms = (millis % 1000) as u32;
    let local = crate::tz::TimeZone::local().to_local(secs);
    let (y, mo, d, h, mi, s) = crate::util_time::civil_from_epoch(local);
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}", y, mo, d, h, mi, s, ms)
}

/// Emit a log record.
pub fn log(level: Level, logger: &str, message: impl AsRef<str>) {
    if !enabled(level) {
        return;
    }
    emit(level, logger, message.as_ref().to_string());
}

/// Emit a log record from format arguments, formatting only when `level` is enabled. The
/// `camel_*!` macros expand to an inline level check and a call of this one out-of-line
/// function, so a log statement costs an atomic load when its level is off and no inlined
/// `format!` at its site.
#[inline(never)]
pub fn log_args(level: Level, logger: &str, args: std::fmt::Arguments<'_>) {
    if !enabled(level) {
        return;
    }
    // a message without arguments is copied, not formatted
    let message = match args.as_str() {
        Some(s) => s.to_string(),
        None => std::fmt::format(args),
    };
    emit(level, logger, message);
}

fn emit(level: Level, logger: &str, message: String) {
    let rec = LogRecord {
        level,
        logger: logger.to_string(),
        message,
        timestamp_millis: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
    };
    if *STDERR_ENABLED.read().unwrap() {
        let thread = std::thread::current();
        eprintln!(
            "{} {:5} [{}] {} : {}",
            format_timestamp(rec.timestamp_millis),
            level.as_str(),
            thread.name().unwrap_or("main"),
            logger,
            rec.message
        );
    }
    for sink in SINKS.read().unwrap().iter() {
        sink(&rec);
    }
}

#[macro_export]
macro_rules! camel_info { ($logger:expr, $($arg:tt)*) => { if $crate::log::enabled($crate::log::Level::Info) { $crate::log::log_args($crate::log::Level::Info, $logger, format_args!($($arg)*)) } }; }
#[macro_export]
macro_rules! camel_warn { ($logger:expr, $($arg:tt)*) => { if $crate::log::enabled($crate::log::Level::Warn) { $crate::log::log_args($crate::log::Level::Warn, $logger, format_args!($($arg)*)) } }; }
#[macro_export]
macro_rules! camel_error { ($logger:expr, $($arg:tt)*) => { if $crate::log::enabled($crate::log::Level::Error) { $crate::log::log_args($crate::log::Level::Error, $logger, format_args!($($arg)*)) } }; }
#[macro_export]
macro_rules! camel_debug { ($logger:expr, $($arg:tt)*) => { if $crate::log::enabled($crate::log::Level::Debug) { $crate::log::log_args($crate::log::Level::Debug, $logger, format_args!($($arg)*)) } }; }
#[macro_export]
macro_rules! camel_trace { ($logger:expr, $($arg:tt)*) => { if $crate::log::enabled($crate::log::Level::Trace) { $crate::log::log_args($crate::log::Level::Trace, $logger, format_args!($($arg)*)) } }; }

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counted<'a>(&'a AtomicUsize);

    impl std::fmt::Display for Counted<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fetch_add(1, Ordering::SeqCst);
            f.write_str("x")
        }
    }

    #[test]
    fn disabled_levels_do_not_format_their_arguments() {
        // the threshold is Info unless CAMEL_LOG_LEVEL says otherwise; no test lowers it
        if super::enabled(super::Level::Trace) {
            return;
        }
        let n = AtomicUsize::new(0);
        crate::camel_trace!("test", "value {}", Counted(&n));
        crate::camel_debug!("test", "value {}", Counted(&n));
        assert_eq!(n.load(Ordering::SeqCst), 0);
        // an enabled level formats once, and records go to the sinks
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let s2 = seen.clone();
        super::add_sink(move |r| {
            if r.logger == "log-args-test" {
                s2.lock().unwrap().push(r.message.clone());
            }
        });
        crate::camel_error!("log-args-test", "value {} and {}", Counted(&n), 7);
        crate::camel_error!("log-args-test", "plain");
        assert_eq!(n.load(Ordering::SeqCst), 1);
        assert_eq!(*seen.lock().unwrap(), vec!["value x and 7".to_string(), "plain".to_string()]);
    }
}
