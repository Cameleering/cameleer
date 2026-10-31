//! Message, headers and the Exchange.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::value::{FromValue, Value};

/// Case-insensitive header map (Camel headers are case-insensitive) that preserves
/// the original key spelling and insertion-independent (sorted) iteration order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Headers {
    map: BTreeMap<String, (String, Value)>,
}

impl Headers {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(&key.to_ascii_lowercase()).map(|(_, v)| v)
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.map.get_mut(&key.to_ascii_lowercase()).map(|(_, v)| v)
    }
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        let key = key.into();
        self.map.insert(key.to_ascii_lowercase(), (key, value.into()));
    }
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.map.remove(&key.to_ascii_lowercase()).map(|(_, v)| v)
    }
    pub fn contains(&self, key: &str) -> bool {
        self.map.contains_key(&key.to_ascii_lowercase())
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn clear(&mut self) {
        self.map.clear()
    }
    /// Iterate `(original_key, value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.map.values().map(|(k, v)| (k.as_str(), v))
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.map.values().map(|(k, _)| k.as_str())
    }
    /// Remove headers whose key matches a Camel-style pattern: exact (case-insensitive),
    /// wildcard (`Camel*`) or regex. Returns number removed.
    pub fn remove_matching(&mut self, pattern: &str, excludes: &[&str]) -> usize {
        let to_remove: Vec<String> = self
            .map
            .iter()
            .filter(|(_, (orig, _))| {
                crate::util::matches_pattern(orig, pattern) && !excludes.iter().any(|e| crate::util::matches_pattern(orig, e))
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in &to_remove {
            self.map.remove(k);
        }
        to_remove.len()
    }
    /// Convert to a plain object Value (original key casing).
    pub fn to_value(&self) -> Value {
        Value::Object(self.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }
}

impl<K: Into<String>, V: Into<Value>> FromIterator<(K, V)> for Headers {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut h = Headers::new();
        for (k, v) in iter {
            h.set(k, v);
        }
        h
    }
}

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn id_prefix() -> &'static str {
    use std::sync::OnceLock;
    static PREFIX: OnceLock<String> = OnceLock::new();
    PREFIX.get_or_init(|| {
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        format!("ID-rust-camel-{:x}-{}", t, std::process::id())
    })
}

/// Generate a new unique id (used for exchange and message ids).
pub fn generate_id() -> String {
    let n = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}", id_prefix(), n)
}

/// A message: headers + body.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Message {
    pub message_id: String,
    pub headers: Headers,
    pub body: Value,
}

impl Message {
    pub fn new(body: impl Into<Value>) -> Self {
        Message { message_id: generate_id(), headers: Headers::new(), body: body.into() }
    }
    pub fn body(&self) -> &Value {
        &self.body
    }
    pub fn set_body(&mut self, body: impl Into<Value>) {
        self.body = body.into();
    }
    pub fn body_as<T: FromValue>(&self) -> Result<T> {
        T::from_value(&self.body)
    }
    pub fn header(&self, key: &str) -> Option<&Value> {
        self.headers.get(key)
    }
    pub fn header_as<T: FromValue>(&self, key: &str) -> Result<Option<T>> {
        match self.headers.get(key) {
            None => Ok(None),
            Some(v) => T::from_value(v).map(Some),
        }
    }
    pub fn set_header(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.headers.set(key, value);
    }
    pub fn remove_header(&mut self, key: &str) -> Option<Value> {
        self.headers.remove(key)
    }
}

/// Message exchange pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExchangePattern {
    #[default]
    InOnly,
    InOut,
}

impl ExchangePattern {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "InOnly" | "inOnly" | "in-only" => Ok(ExchangePattern::InOnly),
            "InOut" | "inOut" | "in-out" => Ok(ExchangePattern::InOut),
            other => Err(CamelError::illegal_argument(format!("Unknown exchange pattern: {}", other))),
        }
    }
}

/// Well-known exchange property / header names (subset of Camel's `Exchange` constants).
pub mod props {
    pub const CORRELATION_ID: &str = "CamelCorrelationId";
    pub const SPLIT_INDEX: &str = "CamelSplitIndex";
    pub const SPLIT_SIZE: &str = "CamelSplitSize";
    pub const SPLIT_COMPLETE: &str = "CamelSplitComplete";
    pub const MULTICAST_INDEX: &str = "CamelMulticastIndex";
    pub const MULTICAST_COMPLETE: &str = "CamelMulticastComplete";
    pub const AGGREGATED_SIZE: &str = "CamelAggregatedSize";
    pub const AGGREGATED_COMPLETED_BY: &str = "CamelAggregatedCompletedBy";
    pub const AGGREGATED_CORRELATION_KEY: &str = "CamelAggregatedCorrelationKey";
    pub const LOOP_INDEX: &str = "CamelLoopIndex";
    pub const LOOP_SIZE: &str = "CamelLoopSize";
    pub const FILTER_MATCHED: &str = "CamelFilterMatched";
    pub const ROUTE_STOP: &str = "CamelRouteStop";
    pub const ERRORHANDLER_HANDLED: &str = "CamelErrorHandlerHandled";
    pub const EXCEPTION_CAUGHT: &str = "CamelExceptionCaught";
    pub const FAILURE_ENDPOINT: &str = "CamelFailureEndpoint";
    pub const FAILURE_ROUTE_ID: &str = "CamelFailureRouteId";
    pub const FATAL_FALLBACK_ERROR_HANDLER: &str = "CamelFatalFallbackErrorHandler";
    pub const REDELIVERED: &str = "CamelRedelivered";
    pub const REDELIVERY_COUNTER: &str = "CamelRedeliveryCounter";
    pub const REDELIVERY_MAX_COUNTER: &str = "CamelRedeliveryMaxCounter";
    pub const REDELIVERY_EXHAUSTED: &str = "CamelRedeliveryExhausted";
    pub const TO_ENDPOINT: &str = "CamelToEndpoint";
    pub const SLIP_ENDPOINT: &str = "CamelSlipEndpoint";
    pub const RECIPIENT_LIST_ENDPOINT: &str = "CamelRecipientListEndpoint";
    pub const TIMER_NAME: &str = "CamelTimerName";
    pub const TIMER_FIRED_TIME: &str = "CamelTimerFiredTime";
    pub const TIMER_COUNTER: &str = "CamelTimerCounter";
    pub const TIMER_PERIOD: &str = "CamelTimerPeriod";
    pub const CREATED_TIMESTAMP: &str = "CamelCreatedTimestamp";
    pub const BATCH_INDEX: &str = "CamelBatchIndex";
    pub const BATCH_SIZE: &str = "CamelBatchSize";
    pub const BATCH_COMPLETE: &str = "CamelBatchComplete";
    pub const DUPLICATE_MESSAGE: &str = "CamelDuplicateMessage";
    pub const FILE_NAME: &str = "CamelFileName";
    pub const FILE_NAME_ONLY: &str = "CamelFileNameOnly";
    pub const FILE_PATH: &str = "CamelFilePath";
    pub const FILE_LENGTH: &str = "CamelFileLength";
    pub const FILE_LAST_MODIFIED: &str = "CamelFileLastModified";
    pub const HTTP_RESPONSE_CODE: &str = "CamelHttpResponseCode";
    pub const HTTP_METHOD: &str = "CamelHttpMethod";
    pub const HTTP_URI: &str = "CamelHttpUri";
    pub const HTTP_PATH: &str = "CamelHttpPath";
    pub const HTTP_QUERY: &str = "CamelHttpQuery";
    pub const CONTENT_TYPE: &str = "Content-Type";
    pub const CHARSET_NAME: &str = "CamelCharsetName";
    pub const STEP_ID: &str = "CamelStepId";
    pub const ON_COMPLETION: &str = "CamelOnCompletion";
    pub const INTERCEPTED_ENDPOINT: &str = "CamelInterceptedEndpoint";
    pub const CIRCUIT_BREAKER_STATE: &str = "CamelCircuitBreakerState";
    pub const RESPONSE_SUCCESSFUL_EXECUTION: &str = "CamelCircuitBreakerSuccessfulExecution";
    pub const RESPONSE_FROM_FALLBACK: &str = "CamelCircuitBreakerResponseFromFallback";
    pub const RESPONSE_SHORT_CIRCUITED: &str = "CamelCircuitBreakerResponseShortCircuited";
    pub const RESPONSE_TIMED_OUT: &str = "CamelCircuitBreakerResponseTimedOut";
    pub const RESPONSE_REJECTED: &str = "CamelCircuitBreakerResponseRejected";
}

/// Callback executed when an exchange completes (Camel `Synchronization`).
pub type OnCompletionCallback = Box<dyn FnOnce(&Exchange) + Send>;

/// The Exchange: the container for a message flowing through a route.
pub struct Exchange {
    pub exchange_id: String,
    pub pattern: ExchangePattern,
    pub message: Message,
    pub properties: BTreeMap<String, Value>,
    pub exception: Option<CamelError>,
    pub context: Option<CamelContext>,
    pub from_endpoint: Option<String>,
    pub from_route_id: Option<String>,
    pub rollback_only: bool,
    pub created: SystemTime,
    /// Synchronizations run when the unit of work completes. Not copied on clone.
    pub on_completions: Vec<OnCompletionCallback>,
}

impl std::fmt::Debug for Exchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Exchange")
            .field("exchange_id", &self.exchange_id)
            .field("pattern", &self.pattern)
            .field("message", &self.message)
            .field("properties", &self.properties)
            .field("exception", &self.exception)
            .field("from_endpoint", &self.from_endpoint)
            .field("from_route_id", &self.from_route_id)
            .finish()
    }
}

impl Clone for Exchange {
    /// Copies everything except the on-completion synchronizations (same exchange id).
    fn clone(&self) -> Self {
        Exchange {
            exchange_id: self.exchange_id.clone(),
            pattern: self.pattern,
            message: self.message.clone(),
            properties: self.properties.clone(),
            exception: self.exception.clone(),
            context: self.context.clone(),
            from_endpoint: self.from_endpoint.clone(),
            from_route_id: self.from_route_id.clone(),
            rollback_only: self.rollback_only,
            created: self.created,
            on_completions: Vec::new(),
        }
    }
}

impl Default for Exchange {
    fn default() -> Self {
        Exchange::new(None)
    }
}

impl Exchange {
    pub fn new(context: Option<CamelContext>) -> Self {
        Exchange {
            exchange_id: generate_id(),
            pattern: ExchangePattern::InOnly,
            message: Message::new(Value::Null),
            properties: BTreeMap::new(),
            exception: None,
            context,
            from_endpoint: None,
            from_route_id: None,
            rollback_only: false,
            created: SystemTime::now(),
            on_completions: Vec::new(),
        }
    }

    /// Create an exchange with the given body.
    pub fn with_body(context: Option<CamelContext>, body: impl Into<Value>) -> Self {
        let mut ex = Exchange::new(context);
        ex.message.body = body.into();
        ex
    }

    /// A copy with a new exchange id and the correlation id property set to this exchange's id.
    pub fn correlated_copy(&self) -> Exchange {
        let mut c = self.clone();
        c.exchange_id = generate_id();
        c.message.message_id = generate_id();
        c.properties.insert(props::CORRELATION_ID.to_string(), Value::String(self.exchange_id.clone()));
        c
    }

    // --- body / header convenience ---
    pub fn body(&self) -> &Value {
        &self.message.body
    }
    pub fn set_body(&mut self, body: impl Into<Value>) {
        self.message.body = body.into();
    }
    pub fn body_as<T: FromValue>(&self) -> Result<T> {
        self.message.body_as()
    }
    pub fn header(&self, key: &str) -> Option<&Value> {
        self.message.headers.get(key)
    }
    pub fn set_header(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.message.headers.set(key, value)
    }
    pub fn property(&self, key: &str) -> Option<&Value> {
        self.properties.get(key)
    }
    pub fn set_property(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.properties.insert(key.into(), value.into());
    }
    pub fn remove_property(&mut self, key: &str) -> Option<Value> {
        self.properties.remove(key)
    }

    // --- failure state ---
    pub fn is_failed(&self) -> bool {
        self.exception.is_some()
    }
    pub fn set_exception(&mut self, e: CamelError) {
        self.exception = Some(e);
    }
    pub fn take_exception(&mut self) -> Option<CamelError> {
        self.exception.take()
    }
    /// True if the route has been told to stop routing this exchange (`stop()` EIP).
    pub fn is_route_stop(&self) -> bool {
        self.properties.get(props::ROUTE_STOP).map(|v| v.is_truthy()).unwrap_or(false)
    }
    pub fn set_route_stop(&mut self, stop: bool) {
        self.set_property(props::ROUTE_STOP, stop);
    }
    /// Whether routing should continue to the next processor in a pipeline.
    pub fn should_continue(&self) -> bool {
        !self.is_failed() && !self.is_route_stop() && !self.rollback_only
    }
    pub fn is_out_capable(&self) -> bool {
        self.pattern == ExchangePattern::InOut
    }

    /// Register a callback to run when this exchange's unit of work completes.
    pub fn add_on_completion(&mut self, f: impl FnOnce(&Exchange) + Send + 'static) {
        self.on_completions.push(Box::new(f));
    }

    /// Run and clear registered on-completion callbacks.
    pub fn run_on_completions(&mut self) {
        let cbs = std::mem::take(&mut self.on_completions);
        for cb in cbs {
            cb(self);
        }
    }

    /// The context, or an error if the exchange was created outside a context.
    pub fn context(&self) -> Result<&CamelContext> {
        self.context.as_ref().ok_or_else(|| CamelError::illegal_state("Exchange has no CamelContext"))
    }

    /// Created timestamp in epoch millis.
    pub fn created_millis(&self) -> i64 {
        self.created.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
    }
}
