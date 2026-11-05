//! `dataset:` and `dataset-test:` (alias `test:`) components - load testing and
//! soak testing with data sets.
//!
//! `dataset:name?produceDelay=3&consumeDelay=0&preloadSize=0&initialDelay=1000&dataSetIndex=lenient&minRate=0`
//!
//! * The consumer (`from("dataset:foo")`) sends `size()` messages created by the
//!   [`DataSet`] bound in the registry under `foo`, with the header `CamelDataSetIndex`.
//! * The producer (`to("dataset:foo")`) verifies the received messages against the data
//!   set like a mock endpoint expecting `size()` messages; use [`dataset_endpoint`] and
//!   [`DataSetEndpoint::assert_is_satisfied`].
//!
//! `dataset-test:uri?timeout=2000&anyOrder=false&split=false&delimiter=\n` expects the
//! messages that can be polled from `uri` (e.g. `seda:expected`): messages sent to it are
//! compared with the expected ones on [`DataSetTestEndpoint::assert_is_satisfied`].

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::component::mock::MockEndpoint;
use crate::context::{CamelContext, Registry, WeakContext};
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::{values_equal, Value};

use super::support::{new_exchange, process_consumer_exchange, BackgroundThreads};

pub const DATASET_INDEX: &str = "CamelDataSetIndex";

/// A data set: a fixed number of messages that can be generated and verified.
pub trait DataSet: Send + Sync {
    /// Number of messages in the data set.
    fn size(&self) -> u64;
    /// The body of message `index`.
    fn create_message_body(&self, index: u64) -> Result<Value>;
    /// Populate an exchange for message `index` (defaults to setting the body).
    fn populate_message(&self, exchange: &mut Exchange, index: u64) -> Result<()> {
        exchange.set_body(self.create_message_body(index)?);
        Ok(())
    }
    /// Verify that `actual` is the expected message `index`.
    fn assert_message_expected(&self, endpoint: &str, actual: &Exchange, index: u64) -> Result<()> {
        let expected = self.create_message_body(index)?;
        if !values_equal(&expected, actual.body()) {
            return Err(CamelError::new(
                "AssertionError",
                format!("{}: message #{} body expected <{}> but was <{}>", endpoint, index, expected, actual.body()),
            ));
        }
        Ok(())
    }
}

/// A data set that sends the same body (default `<hello>world!</hello>`) `size` times.
#[derive(Clone, Debug)]
pub struct SimpleDataSet {
    pub size: u64,
    pub default_body: Value,
    pub default_headers: Vec<(String, Value)>,
}

impl SimpleDataSet {
    pub fn new(size: u64) -> Self {
        SimpleDataSet { size, default_body: Value::from("<hello>world!</hello>"), default_headers: Vec::new() }
    }
    pub fn with_body(mut self, body: impl Into<Value>) -> Self {
        self.default_body = body.into();
        self
    }
    pub fn with_header(mut self, k: &str, v: impl Into<Value>) -> Self {
        self.default_headers.push((k.to_string(), v.into()));
        self
    }
}

impl DataSet for SimpleDataSet {
    fn size(&self) -> u64 {
        self.size
    }
    fn create_message_body(&self, _index: u64) -> Result<Value> {
        Ok(self.default_body.clone())
    }
    fn populate_message(&self, exchange: &mut Exchange, index: u64) -> Result<()> {
        exchange.set_body(self.create_message_body(index)?);
        for (k, v) in &self.default_headers {
            exchange.set_header(k.clone(), v.clone());
        }
        Ok(())
    }
}

/// A data set cycling through a list of bodies (`size` defaults to the list length).
#[derive(Clone, Debug)]
pub struct ListDataSet {
    pub bodies: Vec<Value>,
    pub size: u64,
}

impl ListDataSet {
    pub fn new<V: Into<Value>>(bodies: Vec<V>) -> Self {
        let bodies: Vec<Value> = bodies.into_iter().map(Into::into).collect();
        let size = bodies.len() as u64;
        ListDataSet { bodies, size }
    }
}

impl DataSet for ListDataSet {
    fn size(&self) -> u64 {
        self.size
    }
    fn create_message_body(&self, index: u64) -> Result<Value> {
        if self.bodies.is_empty() {
            return Ok(Value::Null);
        }
        Ok(self.bodies[(index % self.bodies.len() as u64) as usize].clone())
    }
}

/// Bind a data set in the registry.
pub fn bind_dataset(registry: &Registry, name: impl Into<String>, ds: impl DataSet + 'static) {
    let d: Arc<dyn DataSet> = Arc::new(ds);
    registry.bind(name, d);
}

fn lookup_dataset(registry: &Registry, name: &str) -> Option<Arc<dyn DataSet>> {
    if let Some(d) = registry.lookup::<Arc<dyn DataSet>>(name) {
        return Some((*d).clone());
    }
    if let Some(d) = registry.lookup::<SimpleDataSet>(name) {
        return Some(d);
    }
    registry.lookup::<ListDataSet>(name).map(|d| d as Arc<dyn DataSet>)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IndexMode {
    Strict,
    Lenient,
    Off,
}

#[derive(Default)]
struct DataSetState {
    received: u64,
    failures: Vec<String>,
    expected: Option<u64>,
}

struct DataSetShared {
    state: Mutex<DataSetState>,
    cond: Condvar,
}

#[derive(Default)]
pub struct DataSetComponent {
    shared: Mutex<HashMap<String, Arc<DataSetShared>>>,
}

#[derive(Clone)]
struct DataSetConfig {
    produce_delay: Duration,
    consume_delay: Duration,
    preload_size: u64,
    initial_delay: Duration,
    index_mode: IndexMode,
    min_rate: u64,
}

impl Component for DataSetComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let dataset = lookup_dataset(ctx.registry(), remaining).ok_or_else(|| CamelError::no_such_bean(remaining))?;
        let index_mode = match params.take("dataSetIndex").as_deref() {
            None | Some("lenient") => IndexMode::Lenient,
            Some("strict") => IndexMode::Strict,
            Some("off") => IndexMode::Off,
            Some(o) => return Err(CamelError::illegal_argument(format!("Invalid dataSetIndex: {}", o))),
        };
        let config = DataSetConfig {
            produce_delay: params.take_duration("produceDelay", Duration::from_millis(3))?,
            consume_delay: params.take_duration("consumeDelay", Duration::ZERO)?,
            preload_size: params.take_u64("preloadSize", 0)?,
            initial_delay: params.take_duration("initialDelay", Duration::from_millis(1000))?,
            index_mode,
            min_rate: params.take_u64("minRate", 0)?,
        };
        let shared = self
            .shared
            .lock()
            .unwrap()
            .entry(remaining.to_string())
            .or_insert_with(|| Arc::new(DataSetShared { state: Mutex::new(DataSetState::default()), cond: Condvar::new() }))
            .clone();
        if let Some(c) = params.take("expectedCount") {
            shared.state.lock().unwrap().expected = Some(c.parse().map_err(|_| CamelError::illegal_argument("expectedCount must be a number"))?);
        }
        for k in ["assertPeriod", "resultWaitTime", "retainFirst", "retainLast", "reportGroup", "log", "copyOnExchange", "failFast", "bridgeErrorHandler"] {
            params.take(k);
        }
        Ok(Arc::new(DataSetEndpoint { uri: uri.to_string(), name: remaining.to_string(), ctx: ctx.downgrade(), dataset, shared, config }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A dataset endpoint. Cheap to clone.
#[derive(Clone)]
pub struct DataSetEndpoint {
    uri: String,
    name: String,
    ctx: WeakContext,
    dataset: Arc<dyn DataSet>,
    shared: Arc<DataSetShared>,
    config: DataSetConfig,
}

impl DataSetEndpoint {
    pub fn dataset(&self) -> &Arc<dyn DataSet> {
        &self.dataset
    }
    /// Messages received (verified) by producers of this data set.
    pub fn received_counter(&self) -> u64 {
        self.shared.state.lock().unwrap().received
    }
    fn expected(&self) -> u64 {
        self.shared.state.lock().unwrap().expected.unwrap_or_else(|| self.dataset.size())
    }
    /// Wait (up to 10s) until the data set was received and verify it.
    pub fn assert_is_satisfied(&self) -> Result<()> {
        self.assert_is_satisfied_within(Duration::from_secs(10))
    }
    pub fn assert_is_satisfied_within(&self, timeout: Duration) -> Result<()> {
        let expected = self.expected();
        let deadline = Instant::now() + timeout;
        let mut st = self.shared.state.lock().unwrap();
        while st.received < expected && st.failures.is_empty() {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            st = self.shared.cond.wait_timeout(st, deadline - now).unwrap().0;
        }
        if !st.failures.is_empty() {
            return Err(CamelError::new("AssertionError", st.failures.join("; ")));
        }
        if st.received != expected {
            return Err(CamelError::new(
                "AssertionError",
                format!("dataset://{} Received message count. Expected: <{}> but was: <{}>", self.name, expected, st.received),
            ));
        }
        Ok(())
    }
    /// Reset received counters and failures.
    pub fn reset(&self) {
        *self.shared.state.lock().unwrap() = DataSetState::default();
    }
}

/// Typed access to the dataset endpoint behind a uri.
pub fn dataset_endpoint(ctx: &CamelContext, uri: &str) -> Result<DataSetEndpoint> {
    let e = ctx.get_endpoint(uri)?;
    e.as_any()
        .downcast_ref::<DataSetEndpoint>()
        .cloned()
        .ok_or_else(|| CamelError::illegal_argument(format!("{} is not a dataset endpoint", uri)))
}

impl Endpoint for DataSetEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(DataSetProducer { ep: self.clone(), started: Mutex::new(None) }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(DataSetConsumer { ep: self.clone(), processor, threads: BackgroundThreads::new() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct DataSetProducer {
    ep: DataSetEndpoint,
    started: Mutex<Option<Instant>>,
}

impl Service for DataSetProducer {}

impl Processor for DataSetProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let ep = &self.ep;
        if !ep.config.consume_delay.is_zero() {
            std::thread::sleep(ep.config.consume_delay);
        }
        self.started.lock().unwrap().get_or_insert_with(Instant::now);
        let mut st = ep.shared.state.lock().unwrap();
        let counter = st.received;
        let header = ex.header(DATASET_INDEX).and_then(|v| v.as_i64()).map(|v| v as u64);
        let index = header.unwrap_or(counter);
        let mut failure = None;
        match (ep.config.index_mode, header) {
            (IndexMode::Strict, None) => failure = Some(format!("dataset://{}: no {} header on message #{}", ep.name, DATASET_INDEX, counter)),
            (IndexMode::Strict, Some(h)) | (IndexMode::Lenient, Some(h)) if h != counter => {
                failure = Some(format!("dataset://{}: {} header expected <{}> but was <{}>", ep.name, DATASET_INDEX, counter, h))
            }
            _ => {}
        }
        if failure.is_none() {
            if let Err(e) = ep.dataset.assert_message_expected(&ep.uri, ex, index) {
                failure = Some(e.message);
            }
        }
        st.received += 1;
        if let Some(f) = failure {
            st.failures.push(f);
        }
        drop(st);
        ep.shared.cond.notify_all();
        if ep.config.min_rate > 0 {
            if let Some(start) = *self.started.lock().unwrap() {
                let secs = start.elapsed().as_secs_f64();
                let n = ep.received_counter() as f64;
                if secs > 1.0 && n / secs < ep.config.min_rate as f64 {
                    crate::camel_warn!("dataset", "dataset://{} rate {:.1} msg/s is below minRate {}", ep.name, n / secs, ep.config.min_rate);
                }
            }
        }
        Ok(())
    }
}

struct DataSetConsumer {
    ep: DataSetEndpoint,
    processor: Arc<dyn Processor>,
    threads: BackgroundThreads,
}

impl Service for DataSetConsumer {
    fn start(&self) -> Result<()> {
        let (ep, processor) = (self.ep.clone(), self.processor.clone());
        self.threads.spawn(&format!("Camel dataset://{}", self.ep.name), move |flag| {
            if flag.sleep(ep.config.initial_delay) {
                return;
            }
            let size = ep.dataset.size();
            for i in 0..size {
                if flag.is_stopped() {
                    return;
                }
                let mut ex = new_exchange(&ep.ctx);
                ex.set_header(DATASET_INDEX, Value::from(i));
                if let Err(e) = ep.dataset.populate_message(&mut ex, i) {
                    crate::camel_warn!("dataset", "Cannot create message #{} of dataset {}: {}", i, ep.name, e);
                    continue;
                }
                process_consumer_exchange(&ep.uri, processor.as_ref(), &mut ex);
                if i + 1 >= ep.config.preload_size && !ep.config.produce_delay.is_zero() && i + 1 < size && flag.sleep(ep.config.produce_delay) {
                    return;
                }
            }
        })
    }
    fn stop(&self) -> Result<()> {
        self.threads.stop();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// dataset-test
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct DataSetTestComponent {
    mocks: Mutex<HashMap<String, MockEndpoint>>,
}

impl Component for DataSetTestComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let timeout = params.take_duration("timeout", Duration::from_millis(2000))?;
        let any_order = params.take_bool("anyOrder", false)?;
        let split = params.take_bool("split", false)?;
        let delimiter = params.take("delimiter").map(|d| d.replace("\\n", "\n")).unwrap_or_else(|| "\n".to_string());
        // remaining options belong to the expected endpoint uri
        let rest = params.take_all();
        let mut expected_uri = remaining.to_string();
        if !rest.is_empty() {
            let q: Vec<String> = rest.iter().map(|(k, v)| format!("{}={}", k, crate::util::url_encode(v))).collect();
            expected_uri = format!("{}?{}", expected_uri, q.join("&"));
        }
        let mock = self.mocks.lock().unwrap().entry(expected_uri.clone()).or_insert_with(|| MockEndpoint::new(uri, &expected_uri)).clone();
        Ok(Arc::new(DataSetTestEndpoint {
            uri: uri.to_string(),
            expected_uri,
            ctx: ctx.downgrade(),
            mock,
            timeout,
            any_order,
            split,
            delimiter,
            loaded: Arc::new(Mutex::new(None)),
        }))
    }
    fn lenient_properties(&self) -> bool {
        true
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `dataset-test:` endpoint: a mock that expects the messages of another endpoint.
#[derive(Clone)]
pub struct DataSetTestEndpoint {
    uri: String,
    expected_uri: String,
    ctx: WeakContext,
    mock: MockEndpoint,
    timeout: Duration,
    any_order: bool,
    split: bool,
    delimiter: String,
    loaded: Arc<Mutex<Option<Vec<Value>>>>,
}

impl DataSetTestEndpoint {
    /// The underlying mock endpoint (received exchanges, extra expectations).
    pub fn mock(&self) -> &MockEndpoint {
        &self.mock
    }

    /// Poll all expected messages from the expected endpoint (once).
    pub fn load_expected(&self) -> Result<Vec<Value>> {
        let mut loaded = self.loaded.lock().unwrap();
        if let Some(v) = loaded.as_ref() {
            return Ok(v.clone());
        }
        let ctx = self.ctx.upgrade().ok_or_else(|| CamelError::illegal_state("CamelContext dropped"))?;
        let pc = ctx.get_endpoint(&self.expected_uri)?.create_polling_consumer()?;
        pc.start()?;
        let mut bodies = Vec::new();
        let mut wait = self.timeout;
        while let Some(ex) = pc.receive(Some(wait))? {
            if self.split {
                let s = ex.body_as::<String>()?;
                bodies.extend(s.split(self.delimiter.as_str()).filter(|x| !x.is_empty()).map(|x| Value::from(x.to_string())));
            } else {
                bodies.push(ex.message.body);
            }
            wait = Duration::from_millis(50);
        }
        pc.stop()?;
        if self.any_order {
            self.mock.expected_bodies_received_in_any_order(bodies.clone());
        } else {
            self.mock.expected_bodies_received(bodies.clone());
        }
        *loaded = Some(bodies.clone());
        Ok(bodies)
    }

    pub fn assert_is_satisfied(&self) -> Result<()> {
        self.load_expected()?;
        self.mock.assert_is_satisfied()
    }

    pub fn assert_is_satisfied_within(&self, timeout: Duration) -> Result<()> {
        self.load_expected()?;
        self.mock.assert_is_satisfied_within(timeout)
    }
}

/// Typed access to the dataset-test endpoint behind a uri.
pub fn dataset_test_endpoint(ctx: &CamelContext, uri: &str) -> Result<DataSetTestEndpoint> {
    let e = ctx.get_endpoint(uri)?;
    e.as_any()
        .downcast_ref::<DataSetTestEndpoint>()
        .cloned()
        .ok_or_else(|| CamelError::illegal_argument(format!("{} is not a dataset-test endpoint", uri)))
}

impl Endpoint for DataSetTestEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(self.mock.clone()))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
