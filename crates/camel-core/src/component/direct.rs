//! `direct:` component - synchronous in-JVM (in-process) call to another route.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::context::CamelContext;
use crate::error::{kinds, CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use crate::util::Parameters;

type ConsumerSlot = Arc<RwLock<Option<Arc<dyn Processor>>>>;

#[derive(Default)]
pub struct DirectComponent {
    slots: RwLock<HashMap<String, ConsumerSlot>>,
}

impl DirectComponent {
    pub fn new() -> Self {
        Self::default()
    }
    fn slot(&self, name: &str) -> ConsumerSlot {
        self.slots.write().unwrap().entry(name.to_string()).or_default().clone()
    }
}

impl Component for DirectComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let block = params.take_bool("block", true)?;
        let timeout = params.take_duration("timeout", Duration::from_secs(30))?;
        let fail_if_no_consumers = params.take_bool("failIfNoConsumers", true)?;
        params.take("synchronous");
        Ok(Arc::new(DirectEndpoint {
            uri: uri.to_string(),
            name: remaining.to_string(),
            slot: self.slot(remaining),
            block,
            timeout,
            fail_if_no_consumers,
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct DirectEndpoint {
    uri: String,
    name: String,
    slot: ConsumerSlot,
    block: bool,
    timeout: Duration,
    fail_if_no_consumers: bool,
}

struct DirectProducer {
    uri: String,
    name: String,
    slot: ConsumerSlot,
    block: bool,
    timeout: Duration,
    fail_if_no_consumers: bool,
}

impl Service for DirectProducer {}

impl Processor for DirectProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let mut consumer = self.slot.read().unwrap().clone();
        if consumer.is_none() && self.block {
            // wait for a consumer to become available (e.g. route starting)
            let deadline = Instant::now() + self.timeout.min(Duration::from_millis(if self.fail_if_no_consumers { 100 } else { 0 }));
            while consumer.is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
                consumer = self.slot.read().unwrap().clone();
            }
        }
        match consumer {
            Some(p) => {
                // Route stop inside the called route must not stop the caller.
                let r = p.process(exchange);
                if exchange.is_route_stop() {
                    // keep stop semantics: Camel's stop() ends the whole exchange routing
                }
                r
            }
            None => {
                if self.fail_if_no_consumers {
                    Err(CamelError::new(
                        kinds::DIRECT_CONSUMER_NOT_AVAILABLE,
                        format!("No consumers available on endpoint: {}. Exchange[{}]", self.uri, exchange.exchange_id),
                    ))
                } else {
                    crate::camel_warn!("direct", "No consumers available on endpoint: direct://{}", self.name);
                    Ok(())
                }
            }
        }
    }
}

struct DirectConsumer {
    uri: String,
    slot: ConsumerSlot,
    processor: Arc<dyn Processor>,
}

impl Service for DirectConsumer {
    fn start(&self) -> Result<()> {
        let mut s = self.slot.write().unwrap();
        if s.is_some() {
            return Err(CamelError::illegal_state(format!(
                "Cannot add a 2nd consumer to the same endpoint: {}. Endpoint only allows one consumer.",
                self.uri
            )));
        }
        *s = Some(self.processor.clone());
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        *self.slot.write().unwrap() = None;
        Ok(())
    }
}

impl Endpoint for DirectEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(DirectProducer {
            uri: self.uri.clone(),
            name: self.name.clone(),
            slot: self.slot.clone(),
            block: self.block,
            timeout: self.timeout,
            fail_if_no_consumers: self.fail_if_no_consumers,
        }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(DirectConsumer { uri: self.uri.clone(), slot: self.slot.clone(), processor }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
