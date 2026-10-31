//! Content Enricher EIPs: enrich (request/reply to an endpoint) and pollEnrich
//! (consume from an endpoint with a polling consumer).
//!
//! Default merge (no strategy): the resource reply replaces the message
//! (enrich: message and properties of the reply, like Camel's copy strategy;
//! pollEnrich: the polled message; when nothing was polled the body becomes null).

use std::sync::Arc;
use std::time::Duration;

use crate::builder::ExpressionDefinition;
use crate::error::Result;
use crate::impl_definition_boilerplate;
use crate::message::{props, Exchange, ExchangePattern};
use crate::model::{ProcessorDefinition, RouteContext};
use crate::spi::{AggregationStrategy, Expression, Processor};

use super::options::{EipOption, EipOptions};
use super::support::{copy_results, run_processor, SavedProperties, StrategyDefinition};

/// Merge `resource` into `exchange` with the strategy (or the default replace).
fn merge(exchange: &mut Exchange, resource: Exchange, strategy: &Option<Arc<dyn AggregationStrategy>>, message_only: bool) -> Result<()> {
    let saved = SavedProperties::save(exchange, &[props::CORRELATION_ID]);
    match strategy {
        Some(s) => {
            let old = exchange.clone();
            let mut r = s.aggregate(Some(old), resource)?;
            s.on_completion(&mut r);
            copy_results(exchange, r);
        }
        None => {
            if message_only {
                exchange.message = resource.message;
                if resource.exception.is_some() {
                    exchange.exception = resource.exception;
                }
            } else {
                copy_results(exchange, resource);
            }
        }
    }
    saved.restore(exchange);
    Ok(())
}

// ---------------------------------------------------------------------------
// enrich
// ---------------------------------------------------------------------------

/// Content Enricher: sends a copy of the exchange (InOut) to the resource endpoint and
/// merges the reply with the aggregation strategy (default: reply replaces the message).
pub struct EnrichDefinition {
    pub id: Option<String>,
    pub uri: ExpressionDefinition,
    pub strategy: Option<StrategyDefinition>,
    /// Call the strategy even if the resource exchange failed (else the failure propagates).
    pub aggregate_on_exception: bool,
    pub ignore_invalid_endpoint: bool,
    pub share_unit_of_work: bool,
}

impl EnrichDefinition {
    pub fn new(uri: ExpressionDefinition) -> Self {
        EnrichDefinition { id: None, uri, strategy: None, aggregate_on_exception: false, ignore_invalid_endpoint: false, share_unit_of_work: false }
    }
}

impl EipOptions for EnrichDefinition {
    fn apply_option(&mut self, option: &EipOption) -> bool {
        match option {
            EipOption::AggregationStrategy(s) => self.strategy = Some(s.clone()),
            EipOption::AggregateOnException(b) => self.aggregate_on_exception = *b,
            EipOption::IgnoreInvalidEndpoints(b) => self.ignore_invalid_endpoint = *b,
            EipOption::ShareUnitOfWork(b) => self.share_unit_of_work = *b,
            _ => return false,
        }
        true
    }
}

struct EnrichProcessor {
    uri: Arc<dyn Expression>,
    strategy: Option<Arc<dyn AggregationStrategy>>,
    aggregate_on_exception: bool,
    ignore_invalid: bool,
    share_unit_of_work: bool,
}

impl Processor for EnrichProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let uri = self.uri.evaluate(exchange)?.to_string();
        let uri = uri.trim();
        let producer = match exchange.context()?.get_producer(uri) {
            Ok(p) => p,
            Err(e) if self.ignore_invalid => {
                crate::camel_debug!("enrich", "Ignoring invalid endpoint {}: {}", uri, e);
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let mut resource = exchange.correlated_copy();
        resource.pattern = ExchangePattern::InOut;
        resource.set_property(props::TO_ENDPOINT, uri.to_string());
        run_processor(&super::recipient_list::ProducerProcessor(producer), &mut resource);
        if self.share_unit_of_work {
            exchange.on_completions.extend(std::mem::take(&mut resource.on_completions));
        } else {
            resource.run_on_completions();
        }
        if resource.is_failed() && !self.aggregate_on_exception {
            exchange.exception = resource.exception.take();
            return Ok(());
        }
        merge(exchange, resource, &self.strategy, false)
    }
}

impl ProcessorDefinition for EnrichDefinition {
    fn name(&self) -> &str {
        "enrich"
    }
    fn label(&self) -> String {
        format!("enrich[{:?}]", self.uri)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        Ok(Arc::new(EnrichProcessor {
            uri: self.uri.resolve_expression(&rc.context)?,
            strategy: match &self.strategy {
                Some(s) => Some(s.resolve(&rc.context)?),
                None => None,
            },
            aggregate_on_exception: self.aggregate_on_exception,
            ignore_invalid: self.ignore_invalid_endpoint,
            share_unit_of_work: self.share_unit_of_work,
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// pollEnrich
// ---------------------------------------------------------------------------

/// Poll Enricher: receives a message from the resource endpoint using a polling
/// consumer and merges it with the strategy (default: the polled message replaces
/// the message; null body if nothing was received).
/// `timeout_ms`: -1 waits until a message arrives, 0 does not wait, >0 waits at most that long.
/// When nothing is received and a strategy is set, the strategy gets a copy of the
/// original exchange with a null body as the "new" exchange.
pub struct PollEnrichDefinition {
    pub id: Option<String>,
    pub uri: ExpressionDefinition,
    pub timeout_ms: i64,
    pub strategy: Option<StrategyDefinition>,
    pub aggregate_on_exception: bool,
    pub ignore_invalid_endpoint: bool,
}

impl PollEnrichDefinition {
    pub fn new(uri: ExpressionDefinition, timeout_ms: i64) -> Self {
        PollEnrichDefinition { id: None, uri, timeout_ms, strategy: None, aggregate_on_exception: false, ignore_invalid_endpoint: false }
    }
}

impl EipOptions for PollEnrichDefinition {
    fn apply_option(&mut self, option: &EipOption) -> bool {
        match option {
            EipOption::AggregationStrategy(s) => self.strategy = Some(s.clone()),
            EipOption::AggregateOnException(b) => self.aggregate_on_exception = *b,
            EipOption::IgnoreInvalidEndpoints(b) => self.ignore_invalid_endpoint = *b,
            EipOption::Timeout(d) => self.timeout_ms = d.as_millis() as i64,
            _ => return false,
        }
        true
    }
}

struct PollEnrichProcessor {
    uri: Arc<dyn Expression>,
    timeout_ms: i64,
    strategy: Option<Arc<dyn AggregationStrategy>>,
    aggregate_on_exception: bool,
    ignore_invalid: bool,
}

impl Processor for PollEnrichProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let uri = self.uri.evaluate(exchange)?.to_string();
        let uri = uri.trim();
        let pc = match exchange.context()?.get_endpoint(uri).and_then(|e| e.create_polling_consumer()) {
            Ok(pc) => pc,
            Err(e) if self.ignore_invalid => {
                crate::camel_debug!("pollEnrich", "Ignoring invalid endpoint {}: {}", uri, e);
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let timeout = match self.timeout_ms {
            t if t < 0 => Some(Duration::from_secs(u64::MAX / 4)),
            0 => None,
            t => Some(Duration::from_millis(t as u64)),
        };
        pc.start()?;
        let received = pc.receive(timeout);
        let _ = pc.stop();
        let resource = match received {
            Ok(r) => r,
            Err(e) => {
                if !self.aggregate_on_exception || self.strategy.is_none() {
                    return Err(e);
                }
                let mut r = exchange.correlated_copy();
                r.message.body = crate::value::Value::Null;
                r.set_exception(e);
                Some(r)
            }
        };
        match resource {
            Some(r) => {
                if r.is_failed() && !self.aggregate_on_exception {
                    exchange.exception = r.exception;
                    return Ok(());
                }
                merge(exchange, r, &self.strategy, true)
            }
            None => match &self.strategy {
                Some(_) => {
                    let mut empty = exchange.correlated_copy();
                    empty.message.body = crate::value::Value::Null;
                    merge(exchange, empty, &self.strategy, true)
                }
                None => {
                    exchange.message.body = crate::value::Value::Null;
                    Ok(())
                }
            },
        }
    }
}

impl ProcessorDefinition for PollEnrichDefinition {
    fn name(&self) -> &str {
        "pollEnrich"
    }
    fn label(&self) -> String {
        format!("pollEnrich[{:?}]", self.uri)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        Ok(Arc::new(PollEnrichProcessor {
            uri: self.uri.resolve_expression(&rc.context)?,
            timeout_ms: self.timeout_ms,
            strategy: match &self.strategy {
                Some(s) => Some(s.resolve(&rc.context)?),
                None => None,
            },
            aggregate_on_exception: self.aggregate_on_exception,
            ignore_invalid: self.ignore_invalid_endpoint,
        }))
    }
    impl_definition_boilerplate!();
}
