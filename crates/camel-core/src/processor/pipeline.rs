//! Pipeline: run processors in sequence, stopping when the exchange fails or is stopped.

use std::sync::Arc;

use crate::error::Result;
use crate::message::Exchange;
use crate::spi::Processor;

pub struct Pipeline {
    processors: Vec<Arc<dyn Processor>>,
}

impl Pipeline {
    pub fn new(processors: Vec<Arc<dyn Processor>>) -> Self {
        Pipeline { processors }
    }
    pub fn processors(&self) -> &[Arc<dyn Processor>] {
        &self.processors
    }
}

impl Processor for Pipeline {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        for p in &self.processors {
            if !exchange.should_continue() {
                break;
            }
            if let Err(e) = p.process(exchange) {
                exchange.set_exception(e);
                break;
            }
        }
        Ok(())
    }
}

/// Run a processor and convert an `Err` into the exchange's exception.
pub fn process_safely(p: &dyn Processor, exchange: &mut Exchange) {
    if let Err(e) = p.process(exchange) {
        exchange.set_exception(e);
    }
}
