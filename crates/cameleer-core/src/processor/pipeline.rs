//! Pipeline: run processors in sequence, stopping when the exchange fails or is stopped.

use std::sync::Arc;

use crate::error::Result;
use crate::message::Exchange;
use crate::spi::{AsyncProcessor, Blocking, ProcessFuture, Processor};

pub struct Pipeline {
    processors: Vec<Arc<dyn Processor>>,
    /// A step has an asynchronous form (decided once, here).
    is_async: bool,
    blocking: Blocking,
}

impl Pipeline {
    pub fn new(processors: Vec<Arc<dyn Processor>>) -> Self {
        let is_async = processors.iter().any(|p| p.as_async().is_some());
        let blocking = if processors.iter().all(|p| p.as_async().is_some() || p.blocking() == Blocking::Never) { Blocking::Never } else { Blocking::Maybe };
        Pipeline { processors, is_async, blocking }
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

    fn as_async(&self) -> Option<&dyn AsyncProcessor> {
        if self.is_async {
            Some(self)
        } else {
            None
        }
    }

    fn blocking(&self) -> Blocking {
        self.blocking
    }
}

impl AsyncProcessor for Pipeline {
    fn process_async<'a>(&'a self, exchange: &'a mut Exchange) -> ProcessFuture<'a> {
        Box::pin(async move {
            for p in &self.processors {
                if !exchange.should_continue() {
                    break;
                }
                let r = match p.as_async() {
                    Some(a) => a.process_async(exchange).await,
                    None => {
                        if p.blocking() == Blocking::Maybe {
                            crate::reactive::offload().await;
                        }
                        p.process_timed(exchange, 0).0
                    }
                };
                if let Err(e) = r {
                    exchange.set_exception(e);
                    break;
                }
            }
            Ok(())
        })
    }
}

/// Run a processor and convert an `Err` into the exchange's exception.
pub fn process_safely(p: &dyn Processor, exchange: &mut Exchange) {
    if let Err(e) = p.process(exchange) {
        exchange.set_exception(e);
    }
}
