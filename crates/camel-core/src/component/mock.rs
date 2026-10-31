//! `mock:` component - testing endpoint with expectations (Camel MockEndpoint).

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Component, Endpoint, Expression, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::{values_equal, Value};

#[derive(Default)]
pub struct MockComponent {
    endpoints: RwLock<HashMap<String, MockEndpoint>>,
}

impl MockComponent {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Component for MockComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let name = remaining.to_string();
        let mut eps = self.endpoints.write().unwrap();
        let ep = eps.entry(name.clone()).or_insert_with(|| MockEndpoint::new(uri, &name)).clone();
        if let Some(c) = params.take("expectedCount") {
            ep.expected_message_count(c.parse().map_err(|_| CamelError::illegal_argument("expectedCount must be a number"))?);
        }
        if let Some(n) = params.take("retainFirst") {
            ep.inner.state.lock().unwrap().retain_first = n.parse().ok();
        }
        if let Some(n) = params.take("retainLast") {
            ep.inner.state.lock().unwrap().retain_last = n.parse().ok();
        }
        if let Some(n) = params.take("assertPeriod") {
            ep.set_assert_period(crate::util::parse_duration(&n)?);
        }
        if let Some(n) = params.take("resultWaitTime") {
            ep.set_result_wait_time(crate::util::parse_duration(&n)?);
        }
        params.take("log");
        params.take("reportGroup");
        Ok(Arc::new(ep))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

type Check = Box<dyn Fn(&[Exchange]) -> std::result::Result<(), String> + Send + Sync>;

#[derive(Default)]
struct MockState {
    received: Vec<Exchange>,
    received_count: usize,
    expected_count: Option<usize>,
    expected_min_count: Option<usize>,
    checks: Vec<(String, Check)>,
    retain_first: Option<usize>,
    retain_last: Option<usize>,
    failures: Vec<String>,
}

struct MockInner {
    uri: String,
    name: String,
    state: Mutex<MockState>,
    cond: Condvar,
    result_wait_time: Mutex<Duration>,
    assert_period: Mutex<Duration>,
    reply: Mutex<Option<Arc<dyn Expression>>>,
    on_receive: Mutex<Option<Arc<dyn Processor>>>,
    on_receive_nth: Mutex<HashMap<usize, Arc<dyn Processor>>>,
}

/// A mock endpoint. Cheap to clone (shared handle).
#[derive(Clone)]
pub struct MockEndpoint {
    inner: Arc<MockInner>,
}

impl MockEndpoint {
    pub fn new(uri: &str, name: &str) -> Self {
        MockEndpoint {
            inner: Arc::new(MockInner {
                uri: uri.to_string(),
                name: name.to_string(),
                state: Mutex::new(MockState::default()),
                cond: Condvar::new(),
                result_wait_time: Mutex::new(Duration::from_secs(10)),
                assert_period: Mutex::new(Duration::ZERO),
                reply: Mutex::new(None),
                on_receive: Mutex::new(None),
                on_receive_nth: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    // --- configuration of expectations ---

    pub fn expected_message_count(&self, n: usize) -> &Self {
        self.inner.state.lock().unwrap().expected_count = Some(n);
        self
    }

    pub fn expected_minimum_message_count(&self, n: usize) -> &Self {
        self.inner.state.lock().unwrap().expected_min_count = Some(n);
        self
    }

    /// Bodies must be received in this order (also sets expected count).
    pub fn expected_bodies_received<V: Into<Value>>(&self, bodies: Vec<V>) -> &Self {
        let bodies: Vec<Value> = bodies.into_iter().map(Into::into).collect();
        self.expected_message_count(bodies.len());
        self.add_check("expectedBodiesReceived", move |exs| {
            let got: Vec<Value> = exs.iter().map(|e| e.message.body.clone()).collect();
            for (i, b) in bodies.iter().enumerate() {
                match got.get(i) {
                    Some(g) if values_equal(g, b) => {}
                    Some(g) => return Err(format!("Message #{} body expected <{}> but was <{}>", i, b, g)),
                    None => return Err(format!("Message #{} expected <{}> but was not received", i, b)),
                }
            }
            Ok(())
        });
        self
    }

    pub fn expected_bodies_received_in_any_order<V: Into<Value>>(&self, bodies: Vec<V>) -> &Self {
        let bodies: Vec<Value> = bodies.into_iter().map(Into::into).collect();
        self.expected_message_count(bodies.len());
        self.add_check("expectedBodiesReceivedInAnyOrder", move |exs| {
            let mut remaining: Vec<Value> = exs.iter().map(|e| e.message.body.clone()).collect();
            for b in &bodies {
                match remaining.iter().position(|g| values_equal(g, b)) {
                    Some(i) => {
                        remaining.remove(i);
                    }
                    None => return Err(format!("Expected body <{}> was not received. Received: {:?}", b, exs.iter().map(|e| e.message.body.to_string()).collect::<Vec<_>>())),
                }
            }
            Ok(())
        });
        self
    }

    /// Every received message has a header with this value (checked on the last received).
    pub fn expected_header_received(&self, name: &str, value: impl Into<Value>) -> &Self {
        let (name, value) = (name.to_string(), value.into());
        self.add_check("expectedHeaderReceived", move |exs| {
            for (i, e) in exs.iter().enumerate() {
                match e.message.headers.get(&name) {
                    Some(v) if values_equal(v, &value) => {}
                    other => {
                        return Err(format!("Message #{} header {} expected <{}> but was <{}>", i, name, value, other.cloned().unwrap_or_default()))
                    }
                }
            }
            Ok(())
        });
        self
    }

    pub fn expected_property_received(&self, name: &str, value: impl Into<Value>) -> &Self {
        let (name, value) = (name.to_string(), value.into());
        self.add_check("expectedPropertyReceived", move |exs| {
            for (i, e) in exs.iter().enumerate() {
                match e.properties.get(&name) {
                    Some(v) if values_equal(v, &value) => {}
                    other => {
                        return Err(format!("Message #{} property {} expected <{}> but was <{}>", i, name, value, other.cloned().unwrap_or_default()))
                    }
                }
            }
            Ok(())
        });
        self
    }

    /// Header values received in order.
    pub fn expected_header_values_received_in_any_order<V: Into<Value>>(&self, name: &str, values: Vec<V>) -> &Self {
        let name = name.to_string();
        let values: Vec<Value> = values.into_iter().map(Into::into).collect();
        self.expected_message_count(values.len());
        self.add_check("expectedHeaderValuesReceivedInAnyOrder", move |exs| {
            let mut got: Vec<Value> = exs.iter().map(|e| e.message.headers.get(&name).cloned().unwrap_or_default()).collect();
            for v in &values {
                match got.iter().position(|g| values_equal(g, v)) {
                    Some(i) => {
                        got.remove(i);
                    }
                    None => return Err(format!("Expected header {} value <{}> was not received", name, v)),
                }
            }
            Ok(())
        });
        self
    }

    /// Message bodies must be unique.
    pub fn expects_no_duplicate_bodies(&self) -> &Self {
        self.add_check("expectsNoDuplicates", |exs| {
            for i in 0..exs.len() {
                for j in (i + 1)..exs.len() {
                    if values_equal(&exs[i].message.body, &exs[j].message.body) {
                        return Err(format!("Duplicate body <{}> at #{} and #{}", exs[i].message.body, i, j));
                    }
                }
            }
            Ok(())
        });
        self
    }

    /// Custom assertion over all received exchanges.
    pub fn expects(&self, f: impl Fn(&[Exchange]) -> std::result::Result<(), String> + Send + Sync + 'static) -> &Self {
        self.add_check("expects", f);
        self
    }

    /// Assertion on the n-th received message (0-based).
    pub fn message(&self, index: usize, f: impl Fn(&Exchange) -> bool + Send + Sync + 'static) -> &Self {
        self.add_check("message", move |exs| match exs.get(index) {
            Some(e) if f(e) => Ok(()),
            Some(_) => Err(format!("Assertion on message #{} failed", index)),
            None => Err(format!("No message #{} received", index)),
        });
        self
    }

    /// Assertion applied to every received message.
    pub fn all_messages(&self, f: impl Fn(&Exchange) -> bool + Send + Sync + 'static) -> &Self {
        self.add_check("allMessages", move |exs| {
            for (i, e) in exs.iter().enumerate() {
                if !f(e) {
                    return Err(format!("Assertion on message #{} failed", i));
                }
            }
            Ok(())
        });
        self
    }

    fn add_check(&self, name: &str, f: impl Fn(&[Exchange]) -> std::result::Result<(), String> + Send + Sync + 'static) {
        self.inner.state.lock().unwrap().checks.push((name.to_string(), Box::new(f)));
    }

    pub fn set_result_wait_time(&self, d: Duration) {
        *self.inner.result_wait_time.lock().unwrap() = d;
    }

    /// After expectations are met, keep asserting for this period (to detect extra messages).
    pub fn set_assert_period(&self, d: Duration) {
        *self.inner.assert_period.lock().unwrap() = d;
    }

    /// Reply with this expression's value (sets the body) when receiving.
    pub fn return_reply_body(&self, e: impl Into<crate::builder::ExpressionDefinition>) {
        let def = e.into();
        let expr: Arc<dyn Expression> = match def {
            crate::builder::ExpressionDefinition::Expression(e) => e,
            other => Arc::new(move |ex: &Exchange| other.evaluate(ex)),
        };
        *self.inner.reply.lock().unwrap() = Some(expr);
    }

    /// Process every received exchange with this processor (e.g. to simulate errors).
    pub fn when_any_exchange_received(&self, p: impl Processor + 'static) {
        *self.inner.on_receive.lock().unwrap() = Some(Arc::new(p));
    }

    /// Process the n-th (0-based) received exchange with this processor.
    pub fn when_exchange_received(&self, index: usize, p: impl Processor + 'static) {
        self.inner.on_receive_nth.lock().unwrap().insert(index, Arc::new(p));
    }

    /// Reset expectations and received messages.
    pub fn reset(&self) {
        let mut s = self.inner.state.lock().unwrap();
        *s = MockState { retain_first: s.retain_first, retain_last: s.retain_last, ..Default::default() };
        *self.inner.reply.lock().unwrap() = None;
        *self.inner.on_receive.lock().unwrap() = None;
        self.inner.on_receive_nth.lock().unwrap().clear();
    }

    // --- inspection ---

    pub fn received_counter(&self) -> usize {
        self.inner.state.lock().unwrap().received_count
    }

    pub fn received_exchanges(&self) -> Vec<Exchange> {
        self.inner.state.lock().unwrap().received.clone()
    }

    pub fn received_bodies(&self) -> Vec<Value> {
        self.inner.state.lock().unwrap().received.iter().map(|e| e.message.body.clone()).collect()
    }

    pub fn exchange(&self, i: usize) -> Option<Exchange> {
        self.inner.state.lock().unwrap().received.get(i).cloned()
    }

    /// Wait until `n` messages were received or the timeout elapsed. Returns true on success.
    pub fn wait_for_messages(&self, n: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut s = self.inner.state.lock().unwrap();
        while s.received_count < n {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            s = self.inner.cond.wait_timeout(s, deadline - now).unwrap().0;
        }
        true
    }

    fn evaluate(&self) -> std::result::Result<(), String> {
        let s = self.inner.state.lock().unwrap();
        if !s.failures.is_empty() {
            return Err(s.failures.join("; "));
        }
        if let Some(c) = s.expected_count {
            if s.received_count != c {
                return Err(format!("mock://{} Received message count. Expected: <{}> but was: <{}>", self.inner.name, c, s.received_count));
            }
        }
        if let Some(c) = s.expected_min_count {
            if s.received_count < c {
                return Err(format!("mock://{} Received message count. Expected at least: <{}> but was: <{}>", self.inner.name, c, s.received_count));
            }
        }
        for (name, check) in &s.checks {
            check(&s.received).map_err(|e| format!("mock://{} {}: {}", self.inner.name, name, e))?;
        }
        Ok(())
    }

    /// Assert expectations, waiting up to the result wait time (default 10s) for messages.
    pub fn assert_is_satisfied(&self) -> Result<()> {
        let wait = *self.inner.result_wait_time.lock().unwrap();
        self.assert_is_satisfied_within(wait)
    }

    pub fn assert_is_satisfied_within(&self, timeout: Duration) -> Result<()> {
        let target = {
            let s = self.inner.state.lock().unwrap();
            s.expected_count.or(s.expected_min_count)
        };
        match target {
            Some(0) => {
                // wait a little to make sure nothing arrives
                std::thread::sleep(timeout.min(Duration::from_millis(200)));
            }
            Some(n) => {
                self.wait_for_messages(n, timeout);
            }
            None => {}
        }
        let period = *self.inner.assert_period.lock().unwrap();
        if period > Duration::ZERO {
            self.evaluate().map_err(|e| CamelError::new("AssertionError", e))?;
            std::thread::sleep(period);
        }
        self.evaluate().map_err(|e| CamelError::new("AssertionError", e))
    }

    /// Assert that the expectations are NOT satisfied.
    pub fn assert_is_not_satisfied(&self, timeout: Duration) -> Result<()> {
        match self.assert_is_satisfied_within(timeout) {
            Ok(()) => Err(CamelError::new("AssertionError", format!("mock://{} expectations were satisfied but should not be", self.inner.name))),
            Err(_) => Ok(()),
        }
    }

    /// Assert all given mocks.
    pub fn assert_all_satisfied(mocks: &[&MockEndpoint]) -> Result<()> {
        for m in mocks {
            m.assert_is_satisfied()?;
        }
        Ok(())
    }

    fn on_exchange(&self, ex: &mut Exchange) -> Result<()> {
        let index = self.inner.state.lock().unwrap().received_count;
        if let Some(r) = self.inner.reply.lock().unwrap().clone() {
            ex.message.body = r.evaluate(ex)?;
        }
        let nth = self.inner.on_receive_nth.lock().unwrap().get(&index).cloned();
        let any = self.inner.on_receive.lock().unwrap().clone();
        let mut result = Ok(());
        if let Some(p) = nth.or(any) {
            result = p.process(ex);
        }
        {
            let mut s = self.inner.state.lock().unwrap();
            s.received_count += 1;
            let keep = match (s.retain_first, s.retain_last) {
                (Some(f), _) if s.received.len() >= f && s.retain_last.is_none() => false,
                _ => true,
            };
            if keep {
                s.received.push(ex.clone());
                if let Some(l) = s.retain_last {
                    while s.received.len() > l {
                        s.received.remove(0);
                    }
                }
            }
        }
        self.inner.cond.notify_all();
        result
    }
}

impl Service for MockEndpoint {}

impl Processor for MockEndpoint {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self.on_exchange(exchange)
    }
}

impl Endpoint for MockEndpoint {
    fn uri(&self) -> &str {
        &self.inner.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(self.clone()))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
