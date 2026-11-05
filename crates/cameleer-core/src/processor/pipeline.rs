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
        self.process_timed(exchange, 0).0
    }

    fn process_timed(&self, exchange: &mut Exchange, start: u64) -> (Result<()>, u64) {
        // the end reading of one step is the start of the next
        let mut t = start;
        for p in &self.processors {
            if !exchange.should_continue() {
                break;
            }
            let (r, end) = p.process_timed(exchange, t);
            t = end;
            if let Err(e) = r {
                exchange.set_exception(e);
                break;
            }
        }
        (Ok(()), t)
    }
}

/// Run a processor and convert an `Err` into the exchange's exception.
pub fn process_safely(p: &dyn Processor, exchange: &mut Exchange) {
    if let Err(e) = p.process(exchange) {
        exchange.set_exception(e);
    }
}
