//! `browse:` component - stores the exchanges sent to it so they can be browsed,
//! and forwards them synchronously to a consumer route if one exists.
//!
//! Options: `browseLimit` (max kept exchanges, default unlimited).

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use crate::util::Parameters;

#[derive(Default)]
struct BrowseState {
    exchanges: Mutex<Vec<Exchange>>,
    consumer: RwLock<Option<Arc<dyn Processor>>>,
}

#[derive(Default)]
pub struct BrowseComponent {
    states: Mutex<HashMap<String, Arc<BrowseState>>>,
}

impl Component for BrowseComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let limit = params.take_usize("browseLimit", 0)?;
        params.take("synchronous");
        let state = self.states.lock().unwrap().entry(remaining.to_string()).or_default().clone();
        Ok(Arc::new(BrowseEndpoint { uri: uri.to_string(), state, limit }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A browsable endpoint.
pub struct BrowseEndpoint {
    uri: String,
    state: Arc<BrowseState>,
    limit: usize,
}

impl BrowseEndpoint {
    /// The exchanges received so far.
    pub fn exchanges(&self) -> Vec<Exchange> {
        self.state.exchanges.lock().unwrap().clone()
    }
    pub fn clear(&self) {
        self.state.exchanges.lock().unwrap().clear();
    }
}

/// Exchanges received by the browse endpoint behind `uri`.
pub fn browse_exchanges(ctx: &CamelContext, uri: &str) -> Result<Vec<Exchange>> {
    let e = ctx.get_endpoint(uri)?;
    e.as_any()
        .downcast_ref::<BrowseEndpoint>()
        .map(|b| b.exchanges())
        .ok_or_else(|| CamelError::illegal_argument(format!("{} is not a browse endpoint", uri)))
}

struct BrowseProducer {
    state: Arc<BrowseState>,
    limit: usize,
}

impl Service for BrowseProducer {}

impl Processor for BrowseProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        {
            let mut ex = self.state.exchanges.lock().unwrap();
            ex.push(exchange.clone());
            if self.limit > 0 && ex.len() > self.limit {
                let excess = ex.len() - self.limit;
                ex.drain(0..excess);
            }
        }
        let consumer = self.state.consumer.read().unwrap().clone();
        match consumer {
            Some(p) => p.process(exchange),
            None => Ok(()),
        }
    }
}

struct BrowseConsumer {
    state: Arc<BrowseState>,
    processor: Arc<dyn Processor>,
}

impl Service for BrowseConsumer {
    fn start(&self) -> Result<()> {
        *self.state.consumer.write().unwrap() = Some(self.processor.clone());
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        *self.state.consumer.write().unwrap() = None;
        Ok(())
    }
}

impl Endpoint for BrowseEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(BrowseProducer { state: self.state.clone(), limit: self.limit }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(BrowseConsumer { state: self.state.clone(), processor }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
