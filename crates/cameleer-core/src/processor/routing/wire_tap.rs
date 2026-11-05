//! Wire Tap EIP: send a copy (or a new exchange) to an endpoint asynchronously;
//! the original exchange continues immediately.

use std::sync::Arc;

use crate::builder::{simple, ExpressionDefinition};
use crate::error::Result;
use crate::impl_definition_boilerplate;
use crate::message::{Exchange, ExchangePattern};
use crate::model::{ProcessorDefinition, RouteContext};
use crate::spi::{Expression, Processor, Producer};

use super::options::{EipOption, EipOptions};
use super::support::{pool_name, LazyPool, DEFAULT_POOL_SIZE};

/// Wire Tap definition. The uri is dynamic (like `toD`) when it contains `${`.
pub struct WireTapDefinition {
    pub id: Option<String>,
    pub uri: String,
    /// Copy the original exchange (default true); false starts from a new, empty exchange.
    pub copy: bool,
    /// Body of the tapped exchange (evaluated on the tapped exchange).
    pub new_exchange_body: Option<ExpressionDefinition>,
    /// Headers set on the tapped exchange (evaluated on the tapped exchange).
    pub new_exchange_headers: Vec<(String, ExpressionDefinition)>,
    /// Processor invoked to prepare the tapped exchange before it is sent.
    pub on_prepare: Option<Arc<dyn Processor>>,
    pub pool_size: Option<usize>,
    pub ignore_invalid_endpoint: bool,
}

impl WireTapDefinition {
    pub fn new(uri: impl Into<String>) -> Self {
        WireTapDefinition {
            id: None,
            uri: uri.into(),
            copy: true,
            new_exchange_body: None,
            new_exchange_headers: Vec::new(),
            on_prepare: None,
            pool_size: None,
            ignore_invalid_endpoint: false,
        }
    }
}

impl EipOptions for WireTapDefinition {
    fn apply_option(&mut self, option: &EipOption) -> bool {
        match option {
            EipOption::Copy(b) => self.copy = *b,
            EipOption::PoolSize(n) => self.pool_size = Some(*n),
            EipOption::IgnoreInvalidEndpoints(b) => self.ignore_invalid_endpoint = *b,
            _ => return false,
        }
        true
    }
}

enum TapTarget {
    Static(Arc<dyn Producer>),
    Dynamic(Arc<dyn Expression>),
}

struct WireTapProcessor {
    target: TapTarget,
    copy: bool,
    body: Option<Arc<dyn Expression>>,
    headers: Vec<(String, Arc<dyn Expression>)>,
    on_prepare: Option<Arc<dyn Processor>>,
    pool: Arc<LazyPool>,
    ignore_invalid: bool,
}

impl Processor for WireTapProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let producer = match &self.target {
            TapTarget::Static(p) => p.clone(),
            TapTarget::Dynamic(e) => {
                let uri = e.evaluate(exchange)?.to_string();
                match exchange.context()?.get_producer(uri.trim()) {
                    Ok(p) => p,
                    Err(e) if self.ignore_invalid => {
                        crate::camel_debug!("wireTap", "Ignoring invalid endpoint {}: {}", uri, e);
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                }
            }
        };
        let mut tap = if self.copy {
            exchange.correlated_copy()
        } else {
            let mut t = Exchange::new(exchange.context.clone());
            t.from_endpoint = exchange.from_endpoint.clone();
            t
        };
        tap.pattern = ExchangePattern::InOnly;
        if let Some(b) = &self.body {
            tap.message.body = b.evaluate(&tap)?;
        }
        for (k, e) in &self.headers {
            let v = e.evaluate(&tap)?;
            tap.message.headers.set(k.clone(), v);
        }
        if let Some(p) = &self.on_prepare {
            p.process(&mut tap)?;
        }
        self.pool.execute(move || {
            if let Err(e) = producer.process(&mut tap) {
                tap.set_exception(e);
            }
            if let Some(e) = &tap.exception {
                crate::camel_warn!("wireTap", "Error processing wire tapped exchange {}: {}", tap.exchange_id, e);
            }
            tap.run_on_completions();
        });
        Ok(())
    }
}

impl ProcessorDefinition for WireTapDefinition {
    fn name(&self) -> &str {
        "wireTap"
    }
    fn label(&self) -> String {
        format!("wireTap[{}]", self.uri)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let target = if self.uri.contains("${") {
            TapTarget::Dynamic(simple(self.uri.clone()).def.resolve_expression(&rc.context)?)
        } else {
            let endpoint = rc.context.get_endpoint(&self.uri)?;
            let producer = endpoint.create_producer()?;
            rc.add_service(producer.clone());
            TapTarget::Static(producer)
        };
        let pool = LazyPool::new(pool_name(&rc.route_id, "WireTap"), self.pool_size.unwrap_or(DEFAULT_POOL_SIZE));
        rc.add_service(pool.clone());
        let body = match &self.new_exchange_body {
            Some(b) => Some(b.resolve_expression(&rc.context)?),
            None => None,
        };
        let mut headers = Vec::new();
        for (k, e) in &self.new_exchange_headers {
            headers.push((k.clone(), e.resolve_expression(&rc.context)?));
        }
        Ok(Arc::new(WireTapProcessor {
            target,
            copy: self.copy,
            body,
            headers,
            on_prepare: self.on_prepare.clone(),
            pool,
            ignore_invalid: self.ignore_invalid_endpoint,
        }))
    }
    impl_definition_boilerplate!();
}
