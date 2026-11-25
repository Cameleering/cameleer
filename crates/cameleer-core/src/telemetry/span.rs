//! The span model handed to span processors and exporters (the OpenTelemetry data model).

use std::time::Duration;

/// Span kind (OTLP `Span.SpanKind`; the discriminants are the OTLP enum values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanKind {
    Internal = 1,
    Server = 2,
    Client = 3,
    Producer = 4,
    Consumer = 5,
}

impl SpanKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SpanKind::Internal => "INTERNAL",
            SpanKind::Server => "SERVER",
            SpanKind::Client => "CLIENT",
            SpanKind::Producer => "PRODUCER",
            SpanKind::Consumer => "CONSUMER",
        }
    }
}

/// An attribute value (OTLP `AnyValue`, the kinds spans use).
#[derive(Clone, Debug, PartialEq)]
pub enum AttrValue {
    String(String),
    Bool(bool),
    Int(i64),
    Double(f64),
    StringArray(Vec<String>),
}

impl AttrValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            AttrValue::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            AttrValue::Int(i) => Some(*i),
            _ => None,
        }
    }
}

impl std::fmt::Display for AttrValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttrValue::String(s) => f.write_str(s),
            AttrValue::Bool(b) => write!(f, "{b}"),
            AttrValue::Int(i) => write!(f, "{i}"),
            AttrValue::Double(d) => write!(f, "{d}"),
            AttrValue::StringArray(a) => write!(f, "[{}]", a.join(", ")),
        }
    }
}

impl From<&str> for AttrValue {
    fn from(s: &str) -> Self {
        AttrValue::String(s.to_string())
    }
}
impl From<String> for AttrValue {
    fn from(s: String) -> Self {
        AttrValue::String(s)
    }
}
impl From<bool> for AttrValue {
    fn from(b: bool) -> Self {
        AttrValue::Bool(b)
    }
}
impl From<i64> for AttrValue {
    fn from(i: i64) -> Self {
        AttrValue::Int(i)
    }
}
impl From<f64> for AttrValue {
    fn from(d: f64) -> Self {
        AttrValue::Double(d)
    }
}

/// Key/value attributes in insertion order.
pub type Attributes = Vec<(String, AttrValue)>;

/// A timed event of a span (OTLP `Span.Event`), such as a recorded exception.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanEvent {
    pub name: String,
    pub time_unix_nano: u64,
    pub attributes: Attributes,
}

/// Span status code (OTLP `Status.StatusCode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StatusCode {
    #[default]
    Unset = 0,
    Ok = 1,
    Error = 2,
}

/// A finished span.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanData {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    /// All zero for a root span.
    pub parent_span_id: [u8; 8],
    /// Whether the parent came from another process.
    pub parent_remote: bool,
    pub trace_state: String,
    pub flags: u8,
    pub name: String,
    pub kind: SpanKind,
    pub start_unix_nano: u64,
    pub end_unix_nano: u64,
    pub attributes: Attributes,
    /// Attributes shared by every span of one route or endpoint (`component`, `camel.uri`, ...),
    /// computed once; exporters write them before [`SpanData::attributes`].
    pub shared_attributes: std::sync::Arc<Attributes>,
    /// Attributes dropped by the attribute count limit.
    pub dropped_attributes: u32,
    pub events: Vec<SpanEvent>,
    pub status: StatusCode,
    pub status_message: String,
    /// The instrumentation scope (`camel.opentelemetry.instrumentationName`, default `camel`).
    pub scope_name: std::sync::Arc<str>,
}

impl SpanData {
    /// The value of attribute `key`.
    pub fn attribute(&self, key: &str) -> Option<&AttrValue> {
        self.attributes.iter().chain(self.shared_attributes.iter()).find(|(k, _)| k == key).map(|(_, v)| v)
    }
    /// Every attribute: the shared ones, then the span's own.
    pub fn all_attributes(&self) -> impl Iterator<Item = &(String, AttrValue)> {
        self.shared_attributes.iter().chain(self.attributes.iter())
    }
    pub fn duration(&self) -> Duration {
        Duration::from_nanos(self.end_unix_nano.saturating_sub(self.start_unix_nano))
    }
    pub fn is_root(&self) -> bool {
        self.parent_span_id == [0; 8]
    }
}

/// Receives finished, sampled spans (OpenTelemetry `SpanProcessor`). `on_end` runs on the
/// routing thread: it must not block; exporters queue the span and send batches from their
/// own thread.
pub trait SpanProcessor: Send + Sync {
    fn on_end(&self, span: SpanData);
    /// Export what is queued; true when everything was exported within `timeout`.
    fn force_flush(&self, _timeout: Duration) -> bool {
        true
    }
    /// Flush and stop; later spans are dropped.
    fn shutdown(&self, _timeout: Duration) {}
}

/// Keeps finished spans in memory (tests, the dev console).
#[derive(Default)]
pub struct InMemorySpanProcessor {
    spans: std::sync::Mutex<Vec<SpanData>>,
}

impl InMemorySpanProcessor {
    pub fn new() -> Self {
        Self::default()
    }
    /// The finished spans, in end order.
    pub fn spans(&self) -> Vec<SpanData> {
        self.spans.lock().unwrap().clone()
    }
    pub fn clear(&self) {
        self.spans.lock().unwrap().clear();
    }
    /// Waits until at least `n` spans ended, at most `timeout`.
    pub fn wait_for(&self, n: usize, timeout: Duration) -> Vec<SpanData> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let s = self.spans();
            if s.len() >= n || std::time::Instant::now() >= deadline {
                return s;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl SpanProcessor for InMemorySpanProcessor {
    fn on_end(&self, span: SpanData) {
        self.spans.lock().unwrap().push(span);
    }
}

impl<T: SpanProcessor + ?Sized> SpanProcessor for std::sync::Arc<T> {
    fn on_end(&self, span: SpanData) {
        (**self).on_end(span)
    }
    fn force_flush(&self, timeout: Duration) -> bool {
        (**self).force_flush(timeout)
    }
    fn shutdown(&self, timeout: Duration) {
        (**self).shutdown(timeout)
    }
}
