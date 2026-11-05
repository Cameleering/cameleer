//! Message history (Camel `MessageHistory`).
//!
//! When enabled with `ctx.set_message_history(true)`, every route input and every
//! processor an exchange passes through is recorded in the exchange property
//! [`MESSAGE_HISTORY`] (`CamelMessageHistory`) as an array of objects
//! `{routeId, nodeId, label, elapsed, timestamp}` (`elapsed` in milliseconds).
//! [`format_message_history`] renders it in the table format the default error handler
//! logs on failure.

use std::time::Duration;

use crate::component::log::ExchangeFormatter;
use crate::context::CamelContext;
use crate::message::Exchange;
use crate::value::Value;

/// Exchange property holding the message history.
pub const MESSAGE_HISTORY: &str = "CamelMessageHistory";

/// One recorded step.
#[derive(Clone, Debug, PartialEq)]
pub struct MessageHistoryEntry {
    pub route_id: String,
    pub node_id: String,
    pub label: String,
    /// Elapsed milliseconds (0 while the step is still running).
    pub elapsed: u64,
    /// Epoch millis when the step started.
    pub timestamp: i64,
}

impl CamelContext {
    /// Enable/disable message history (`camel.main.message-history`). Takes effect immediately.
    pub fn set_message_history(&self, enabled: bool) {
        crate::management::state(self).history_enabled.store(enabled, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn is_message_history(&self) -> bool {
        crate::management::state(self).history_enabled.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Record the start of a step; returns the entry index.
pub(crate) fn begin(ex: &mut Exchange, route_id: &str, node_id: &str, label: &str) -> usize {
    let entry = Value::object([
        ("routeId", Value::from(route_id)),
        ("nodeId", Value::from(node_id)),
        ("label", Value::from(label)),
        ("elapsed", Value::Int(0)),
        ("timestamp", Value::Int(crate::management::now_millis())),
    ]);
    let slot = ex.properties.entry(MESSAGE_HISTORY.to_string()).or_insert_with(|| Value::Array(Vec::new()));
    if !matches!(slot, Value::Array(_)) {
        *slot = Value::Array(Vec::new());
    }
    match slot {
        Value::Array(a) => {
            a.push(entry);
            a.len() - 1
        }
        _ => unreachable!(),
    }
}

/// Record the elapsed time of a step started with [`begin`].
pub(crate) fn end(ex: &mut Exchange, index: usize, node_id: &str, elapsed: Duration) {
    if let Some(Value::Array(a)) = ex.properties.get_mut(MESSAGE_HISTORY) {
        if let Some(Value::Object(o)) = a.get_mut(index) {
            if o.get("nodeId").and_then(|v| v.as_str()) == Some(node_id) {
                o.insert("elapsed".into(), Value::Int(elapsed.as_millis() as i64));
            }
        }
    }
}

/// The recorded history of an exchange (empty if message history is disabled).
pub fn message_history(ex: &Exchange) -> Vec<MessageHistoryEntry> {
    let Some(Value::Array(a)) = ex.property(MESSAGE_HISTORY) else { return Vec::new() };
    a.iter()
        .map(|v| {
            let s = |k: &str| v.get(k).map(|x| x.to_string()).unwrap_or_default();
            MessageHistoryEntry {
                route_id: s("routeId"),
                node_id: s("nodeId"),
                label: s("label"),
                elapsed: v.get("elapsed").and_then(|x| x.as_i64()).unwrap_or(0).max(0) as u64,
                timestamp: v.get("timestamp").and_then(|x| x.as_i64()).unwrap_or(0),
            }
        })
        .collect()
}

const LINE: &str = "---------------------------------------------------------------------------------------------------------------------------------------";

fn cell(s: &str, width: usize) -> String {
    let s: String = s.replace(['\n', '\r'], " ");
    if s.chars().count() > width {
        s.chars().take(width).collect()
    } else {
        format!("{:<width$}", s, width = width)
    }
}

/// Render the message history of an exchange like Camel's default error handler:
///
/// ```text
/// Message History
/// ---------------------------------------------------------------------------------------------------------------------------------------
/// RouteId              ProcessorId          Processor                                                                        Elapsed (ms)
/// [route1            ] [route1            ] [from[direct:start]                                                            ] [         3]
/// [route1            ] [to1               ] [to[mock:out]                                                                  ] [         0]
///
/// Exchange
/// ---------------------------------------------------------------------------------------------------------------------------------------
/// Exchange[Id: ..., ExchangePattern: InOnly, Headers: {}, BodyType: String, Body: hello]
///
/// Stacktrace
/// ---------------------------------------------------------------------------------------------------------------------------------------
/// IllegalArgumentException: boom
/// ```
pub fn format_message_history(ex: &Exchange) -> String {
    let mut out = String::new();
    let entries = message_history(ex);
    if ex.property(MESSAGE_HISTORY).is_none() {
        out.push_str("Message History (message history is disabled)\n");
        out.push_str(LINE);
        out.push('\n');
    } else {
        out.push_str("Message History\n");
        out.push_str(LINE);
        out.push('\n');
        out.push_str(&format!("{:<20} {:<20} {:<80} {}\n", "RouteId", "ProcessorId", "Processor", "Elapsed (ms)"));
        for e in &entries {
            out.push_str(&format!("[{}] [{}] [{}] [{:>10}]\n", cell(&e.route_id, 18), cell(&e.node_id, 18), cell(&e.label, 78), e.elapsed));
        }
    }
    out.push_str("\nExchange\n");
    out.push_str(LINE);
    out.push('\n');
    let fmt = ExchangeFormatter { show_exchange_id: true, show_headers: true, show_exception: false, max_chars: 1000, ..Default::default() };
    out.push_str(&fmt.format(ex));
    out.push('\n');
    let error = ex.exception.clone().or_else(|| {
        // a handled exception is kept as a property by the error handler
        ex.property(crate::message::props::EXCEPTION_CAUGHT).map(|v| crate::error::CamelError::new("Exception", v.to_string()))
    });
    if let Some(e) = error {
        out.push_str("\nStacktrace\n");
        out.push_str(LINE);
        out.push('\n');
        out.push_str(&e.to_string());
        out.push('\n');
    }
    out
}
