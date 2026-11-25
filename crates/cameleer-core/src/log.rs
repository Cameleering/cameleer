//! Minimal logging facility (no external crates). Log records go to stderr by
//! default; a custom sink can be installed (useful for tests and the CLI).
//! The level threshold comes from `CAMEL_LOG_LEVEL` (TRACE, DEBUG, INFO, WARN, ERROR, OFF).
//!
//! Lines are text by default, or one JSON object per line ([`LogFormat`], `CAMEL_LOG_FORMAT`):
//! `ecs` is the Elastic Common Schema layout Camel JBang's `--logging-json` uses, `logstash` the
//! field names of logstash-logback-encoder (Spring Boot's and Quarkus' `logstash` format). A
//! record logged inside a traced span ([`crate::telemetry`]) carries the trace and span id: in
//! text lines as `[trace_id,span_id]` after the thread, in `ecs` as `trace.id`/`span.id`, in
//! `logstash` as `trace_id`/`span_id` (the OpenTelemetry Java agent's MDC keys).

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
    /// The trace and span the record was logged in, if any.
    pub trace: Option<([u8; 16], [u8; 8])>,
}

/// The format of the lines written to stderr.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    /// `2026-11-25 10:00:00.000 INFO  [main] logger : message`.
    Text = 0,
    /// One JSON object per line with ECS field names (`@timestamp`, `log.level`, `message`,
    /// `process.thread.name`, `log.logger`, `ecs.version`, `trace.id`, `span.id`).
    Ecs = 1,
    /// One JSON object per line with logstash-logback-encoder field names (`@timestamp`,
    /// `@version`, `message`, `logger_name`, `thread_name`, `level`, `level_value`, `trace_id`,
    /// `span_id`).
    Logstash = 2,
}

impl LogFormat {
    /// `text`, `ecs` (or `json`, Camel JBang's `--logging-json`) or `logstash`.
    pub fn parse(s: &str) -> Option<LogFormat> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "text" | "plain" => LogFormat::Text,
            "ecs" | "json" => LogFormat::Ecs,
            "logstash" => LogFormat::Logstash,
            _ => return None,
        })
    }
}

/// `u8::MAX` until first read (then from `CAMEL_LOG_FORMAT`).
static FORMAT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// The current line format.
pub fn format() -> LogFormat {
    let f = FORMAT.load(std::sync::atomic::Ordering::Relaxed);
    let f = if f == u8::MAX {
        let f = std::env::var("CAMEL_LOG_FORMAT").ok().and_then(|s| LogFormat::parse(&s)).unwrap_or(LogFormat::Text);
        FORMAT.store(f as u8, std::sync::atomic::Ordering::Relaxed);
        f as u8
    } else {
        f
    };
    match f {
        1 => LogFormat::Ecs,
        2 => LogFormat::Logstash,
        _ => LogFormat::Text,
    }
}

/// Set the line format.
pub fn set_format(f: LogFormat) {
    FORMAT.store(f as u8, std::sync::atomic::Ordering::Relaxed);
}

fn level_value(l: Level) -> u32 {
    // logback's Level.toInt()
    match l {
        Level::Trace => 5000,
        Level::Debug => 10000,
        Level::Info => 20000,
        Level::Warn => 30000,
        Level::Error => 40000,
        Level::Off => i32::MAX as u32,
    }
}

/// Formats a record as one line (without the line break) in `format`.
pub fn format_line(rec: &LogRecord, thread: &str, format: LogFormat) -> String {
    use crate::telemetry::context::hex;
    match format {
        LogFormat::Text => {
            let ids = match rec.trace {
                Some((t, s)) => format!("[{},{}] ", hex(&t), hex(&s)),
                None => String::new(),
            };
            format!("{} {:5} [{}] {}{} : {}", format_timestamp(rec.timestamp_millis), rec.level.as_str(), thread, ids, rec.logger, rec.message)
        }
        LogFormat::Ecs | LogFormat::Logstash => {
            let ecs = format == LogFormat::Ecs;
            let ts = crate::util_time::format_utc_millis(rec.timestamp_millis as i64);
            let mut o = String::with_capacity(160 + rec.message.len());
            let field = |o: &mut String, k: &str, v: &str| {
                if o.is_empty() {
                    o.push('{');
                } else {
                    o.push(',');
                }
                crate::value::json_escape(k, o);
                o.push(':');
                crate::value::json_escape(v, o);
            };
            field(&mut o, "@timestamp", &ts);
            if ecs {
                field(&mut o, "log.level", rec.level.as_str());
                field(&mut o, "message", &rec.message);
                field(&mut o, "ecs.version", "1.2.0");
                field(&mut o, "process.thread.name", thread);
                field(&mut o, "log.logger", &rec.logger);
                if let Some((t, s)) = rec.trace {
                    field(&mut o, "trace.id", &hex(&t));
                    field(&mut o, "span.id", &hex(&s));
                }
                o.push('}');
            } else {
                field(&mut o, "@version", "1");
                field(&mut o, "message", &rec.message);
                field(&mut o, "logger_name", &rec.logger);
                field(&mut o, "thread_name", thread);
                field(&mut o, "level", rec.level.as_str());
                o.push_str(&format!(",\"level_value\":{}", level_value(rec.level)));
                if let Some((t, s)) = rec.trace {
                    field(&mut o, "trace_id", &hex(&t));
                    field(&mut o, "span_id", &hex(&s));
                }
                o.push('}');
            }
            o
        }
    }
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
        trace: crate::telemetry::current_ids(),
    };
    if *STDERR_ENABLED.read().unwrap() {
        let thread = std::thread::current();
        eprintln!("{}", format_line(&rec, thread.name().unwrap_or("main"), format()));
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
    #[test]
    fn line_formats() {
        use super::*;
        let mut rec = LogRecord { level: Level::Warn, logger: "route1".into(), message: "a \"b\"\n".into(), timestamp_millis: 1_764_064_800_123, trace: None };
        let t = format_line(&rec, "main", LogFormat::Text);
        assert!(t.ends_with(" WARN  [main] route1 : a \"b\"\n"), "{t}");
        assert_eq!(
            format_line(&rec, "main", LogFormat::Ecs),
            r#"{"@timestamp":"2025-11-25T10:00:00.123Z","log.level":"WARN","message":"a \"b\"\n","ecs.version":"1.2.0","process.thread.name":"main","log.logger":"route1"}"#
        );
        rec.trace = Some(([0xab; 16], [0xcd; 8]));
        let l = format_line(&rec, "w-1", LogFormat::Logstash);
        assert_eq!(
            l,
            format!(r#"{{"@timestamp":"2025-11-25T10:00:00.123Z","@version":"1","message":"a \"b\"\n","logger_name":"route1","thread_name":"w-1","level":"WARN","level_value":30000,"trace_id":"{}","span_id":"{}"}}"#, "ab".repeat(16), "cd".repeat(8))
        );
        assert!(format_line(&rec, "main", LogFormat::Text).contains(&format!("[main] [{},{}] route1", "ab".repeat(16), "cd".repeat(8))));
        assert!(format_line(&rec, "main", LogFormat::Ecs).ends_with(&format!(r#""trace.id":"{}","span.id":"{}"}}"#, "ab".repeat(16), "cd".repeat(8))));
        assert_eq!(LogFormat::parse("JSON"), Some(LogFormat::Ecs));
        assert_eq!(LogFormat::parse("xml"), None);
    }

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
