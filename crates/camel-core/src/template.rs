//! ProducerTemplate and ConsumerTemplate.

use std::time::Duration;

use crate::context::CamelContext;
use crate::error::{kinds, CamelError, Result};
use crate::message::{Exchange, ExchangePattern};
use crate::value::Value;

/// Send messages to endpoints from user code (Camel `ProducerTemplate`).
#[derive(Clone)]
pub struct ProducerTemplate {
    ctx: CamelContext,
    default_endpoint: Option<String>,
}

impl ProducerTemplate {
    pub fn new(ctx: CamelContext) -> Self {
        ProducerTemplate { ctx, default_endpoint: None }
    }

    pub fn with_default_endpoint(mut self, uri: impl Into<String>) -> Self {
        self.default_endpoint = Some(uri.into());
        self
    }

    pub fn context(&self) -> &CamelContext {
        &self.ctx
    }

    /// Send an exchange; returns the exchange after processing (check `exception`).
    pub fn send(&self, uri: &str, mut exchange: Exchange) -> Exchange {
        if exchange.context.is_none() {
            exchange.context = Some(self.ctx.clone());
        }
        let start = std::time::Instant::now();
        self.ctx.notify(&crate::context::CamelEvent::ExchangeSending { exchange_id: exchange.exchange_id.clone(), endpoint: uri.to_string() });
        match self.ctx.get_producer(uri) {
            Ok(p) => {
                if let Err(e) = p.process(&mut exchange) {
                    exchange.set_exception(e);
                }
            }
            Err(e) => exchange.set_exception(e),
        }
        self.ctx.notify(&crate::context::CamelEvent::ExchangeSent {
            exchange_id: exchange.exchange_id.clone(),
            endpoint: uri.to_string(),
            elapsed_ms: start.elapsed().as_millis() as u64,
        });
        if exchange.from_route_id.is_none() {
            // not routed through a route processor (e.g. direct producer runs inline) - complete unit of work
            exchange.run_on_completions();
        }
        exchange
    }

    /// Build and send an exchange using a closure to populate it.
    pub fn send_with(&self, uri: &str, pattern: ExchangePattern, f: impl FnOnce(&mut Exchange)) -> Exchange {
        let mut ex = self.ctx.create_exchange();
        ex.pattern = pattern;
        f(&mut ex);
        self.send(uri, ex)
    }

    fn to_result(ex: Exchange) -> Result<Exchange> {
        match ex.exception {
            Some(ref e) => Err(CamelError::new(kinds::CAMEL_EXECUTION, format!("Exception occurred during execution on the exchange: {}", e))
                .with_cause(e.clone())),
            None => Ok(ex),
        }
    }

    /// Send a body (InOnly). Returns an error if the exchange failed.
    pub fn send_body(&self, uri: &str, body: impl Into<Value>) -> Result<()> {
        let body = body.into();
        let ex = self.send_with(uri, ExchangePattern::InOnly, |ex| ex.message.body = body);
        Self::to_result(ex).map(|_| ())
    }

    /// Send a body to the default endpoint.
    pub fn send_body_default(&self, body: impl Into<Value>) -> Result<()> {
        let uri = self.default_endpoint.clone().ok_or_else(|| CamelError::illegal_state("No default endpoint configured"))?;
        self.send_body(&uri, body)
    }

    pub fn send_body_and_header(&self, uri: &str, body: impl Into<Value>, key: &str, value: impl Into<Value>) -> Result<()> {
        let (body, value) = (body.into(), value.into());
        let ex = self.send_with(uri, ExchangePattern::InOnly, |ex| {
            ex.message.body = body;
            ex.message.headers.set(key, value);
        });
        Self::to_result(ex).map(|_| ())
    }

    pub fn send_body_and_headers(&self, uri: &str, body: impl Into<Value>, headers: Vec<(&str, Value)>) -> Result<()> {
        let body = body.into();
        let ex = self.send_with(uri, ExchangePattern::InOnly, |ex| {
            ex.message.body = body;
            for (k, v) in headers {
                ex.message.headers.set(k, v);
            }
        });
        Self::to_result(ex).map(|_| ())
    }

    pub fn send_body_and_property(&self, uri: &str, body: impl Into<Value>, key: &str, value: impl Into<Value>) -> Result<()> {
        let (body, value) = (body.into(), value.into());
        let ex = self.send_with(uri, ExchangePattern::InOnly, |ex| {
            ex.message.body = body;
            ex.set_property(key, value);
        });
        Self::to_result(ex).map(|_| ())
    }

    /// Request/reply (InOut): returns the reply body.
    pub fn request_body(&self, uri: &str, body: impl Into<Value>) -> Result<Value> {
        let body = body.into();
        let ex = self.send_with(uri, ExchangePattern::InOut, |ex| ex.message.body = body);
        Self::to_result(ex).map(|ex| ex.message.body)
    }

    pub fn request_body_and_header(&self, uri: &str, body: impl Into<Value>, key: &str, value: impl Into<Value>) -> Result<Value> {
        let (body, value) = (body.into(), value.into());
        let ex = self.send_with(uri, ExchangePattern::InOut, |ex| {
            ex.message.body = body;
            ex.message.headers.set(key, value);
        });
        Self::to_result(ex).map(|ex| ex.message.body)
    }

    pub fn request_body_and_headers(&self, uri: &str, body: impl Into<Value>, headers: Vec<(&str, Value)>) -> Result<Value> {
        let body = body.into();
        let ex = self.send_with(uri, ExchangePattern::InOut, |ex| {
            ex.message.body = body;
            for (k, v) in headers {
                ex.message.headers.set(k, v);
            }
        });
        Self::to_result(ex).map(|ex| ex.message.body)
    }

    /// Request/reply returning the whole exchange (Err if it failed).
    pub fn request(&self, uri: &str, f: impl FnOnce(&mut Exchange)) -> Result<Exchange> {
        Self::to_result(self.send_with(uri, ExchangePattern::InOut, f))
    }

    /// Asynchronous send on a background thread; join the handle for the result.
    pub fn async_request_body(&self, uri: &str, body: impl Into<Value>) -> std::thread::JoinHandle<Result<Value>> {
        let t = self.clone();
        let uri = uri.to_string();
        let body = body.into();
        std::thread::spawn(move || t.request_body(&uri, body))
    }

    pub fn async_send_body(&self, uri: &str, body: impl Into<Value>) -> std::thread::JoinHandle<Result<()>> {
        let t = self.clone();
        let uri = uri.to_string();
        let body = body.into();
        std::thread::spawn(move || t.send_body(&uri, body))
    }
}

/// Poll messages from endpoints from user code (Camel `ConsumerTemplate`).
#[derive(Clone)]
pub struct ConsumerTemplate {
    ctx: CamelContext,
}

impl ConsumerTemplate {
    pub fn new(ctx: CamelContext) -> Self {
        ConsumerTemplate { ctx }
    }

    /// Receive an exchange, waiting up to `timeout` (None = wait forever).
    pub fn receive(&self, uri: &str, timeout: Option<Duration>) -> Result<Option<Exchange>> {
        let endpoint = self.ctx.get_endpoint(uri)?;
        let pc = endpoint.create_polling_consumer()?;
        pc.start()?;
        let r = pc.receive(timeout.or(Some(Duration::from_secs(u64::MAX / 4))));
        pc.stop()?;
        r
    }

    /// Receive without waiting.
    pub fn receive_no_wait(&self, uri: &str) -> Result<Option<Exchange>> {
        let endpoint = self.ctx.get_endpoint(uri)?;
        let pc = endpoint.create_polling_consumer()?;
        pc.start()?;
        let r = pc.receive(None);
        pc.stop()?;
        r
    }

    /// Receive a body, waiting up to `timeout`.
    pub fn receive_body(&self, uri: &str, timeout: Duration) -> Result<Option<Value>> {
        Ok(self.receive(uri, Some(timeout))?.map(|ex| ex.message.body))
    }
}
