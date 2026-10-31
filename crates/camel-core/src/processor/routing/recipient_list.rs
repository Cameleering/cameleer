//! Recipient List, Routing Slip and Dynamic Router EIPs.

use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::Result;
use crate::impl_definition_boilerplate;
use crate::message::{props, Exchange};
use crate::model::{ProcessorDefinition, RouteContext};
use crate::spi::{Expression, Processor, Producer};
use crate::value::Value;

use super::multicast::MulticastEngine;
use super::options::{EipOption, EipOptions, MulticastSettings};
use super::support::to_iterable;

/// Adapts a producer to a plain processor.
pub struct ProducerProcessor(pub Arc<dyn Producer>);

impl Processor for ProducerProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self.0.process(exchange)
    }
}

/// Parse an expression result into endpoint uris (array elements or a delimited string).
pub fn parse_uris(v: Value, delimiter: &str) -> Vec<String> {
    let delim = if delimiter.is_empty() || delimiter == "false" { None } else { Some(delimiter) };
    to_iterable(v, delim).into_iter().map(|u| u.to_string().trim().to_string()).filter(|u| !u.is_empty()).collect()
}

/// Resolve a producer for a uri; `Ok(None)` when invalid and `ignore_invalid` is set.
fn resolve_producer(exchange: &Exchange, uri: &str, ignore_invalid: bool, eip: &str) -> Result<Option<(String, Arc<dyn Producer>)>> {
    let ctx = exchange.context()?;
    let resolved = ctx.get_endpoint(uri).and_then(|e| {
        let u = e.uri().to_string();
        ctx.get_producer(uri).map(|p| (u, p))
    });
    match resolved {
        Ok(p) => Ok(Some(p)),
        Err(e) if ignore_invalid => {
            crate::camel_debug!(eip, "Endpoint uri is invalid: {}. This exception will be ignored. {}", uri, e);
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// recipientList
// ---------------------------------------------------------------------------

/// Recipient List: the expression gives a list of endpoint uris (array or delimited
/// string, default delimiter `,`); a copy of the exchange is sent to each.
/// Default result: the last reply (like multicast). Sets `CamelRecipientListEndpoint`.
pub struct RecipientListDefinition {
    pub id: Option<String>,
    pub expression: ExpressionDefinition,
    pub delimiter: String,
    pub ignore_invalid_endpoints: bool,
    pub settings: MulticastSettings,
}

impl RecipientListDefinition {
    pub fn new(expression: ExpressionDefinition) -> Self {
        RecipientListDefinition { id: None, expression, delimiter: ",".into(), ignore_invalid_endpoints: false, settings: Default::default() }
    }
}

impl EipOptions for RecipientListDefinition {
    fn apply_option(&mut self, option: &EipOption) -> bool {
        match option {
            EipOption::Delimiter(d) => self.delimiter = d.clone(),
            EipOption::IgnoreInvalidEndpoints(b) => self.ignore_invalid_endpoints = *b,
            other => return self.settings.apply(other),
        }
        true
    }
}

struct RecipientListProcessor {
    expression: Arc<dyn Expression>,
    delimiter: String,
    ignore_invalid: bool,
    engine: MulticastEngine,
}

impl Processor for RecipientListProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let uris = parse_uris(self.expression.evaluate(exchange)?, &self.delimiter);
        let mut targets = Vec::new();
        for uri in &uris {
            if let Some(t) = resolve_producer(exchange, uri, self.ignore_invalid, "recipientList")? {
                targets.push(t);
            }
        }
        let n = targets.len();
        let pairs = targets
            .into_iter()
            .enumerate()
            .map(|(i, (uri, producer))| {
                let mut sub = exchange.correlated_copy();
                sub.set_property(props::MULTICAST_INDEX, i);
                sub.set_property(props::MULTICAST_COMPLETE, i + 1 == n);
                sub.set_property(props::RECIPIENT_LIST_ENDPOINT, uri.clone());
                sub.set_property(props::TO_ENDPOINT, uri);
                (sub, Arc::new(ProducerProcessor(producer)) as Arc<dyn Processor>)
            })
            .collect();
        self.engine.execute(exchange, pairs)
    }
}

impl ProcessorDefinition for RecipientListDefinition {
    fn name(&self) -> &str {
        "recipientList"
    }
    fn label(&self) -> String {
        format!("recipientList[{:?}]", self.expression)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        Ok(Arc::new(RecipientListProcessor {
            expression: self.expression.resolve_expression(&rc.context)?,
            delimiter: self.delimiter.clone(),
            ignore_invalid: self.ignore_invalid_endpoints,
            engine: MulticastEngine::create(rc, &self.settings, "recipientList", true)?,
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// routingSlip / dynamicRouter
// ---------------------------------------------------------------------------

/// Send the exchange through the uris in sequence (pipeline style). Returns false
/// if routing should stop.
fn route_through(exchange: &mut Exchange, uris: &[String], ignore_invalid: bool, eip: &str) -> Result<bool> {
    for uri in uris {
        if !exchange.should_continue() {
            return Ok(false);
        }
        let (resolved, producer) = match resolve_producer(exchange, uri, ignore_invalid, eip)? {
            Some(p) => p,
            None => continue,
        };
        exchange.set_property(props::SLIP_ENDPOINT, resolved.clone());
        exchange.set_property(props::TO_ENDPOINT, resolved);
        if let Err(e) = producer.process(exchange) {
            exchange.set_exception(e);
            return Ok(false);
        }
    }
    Ok(exchange.should_continue())
}

/// Routing Slip: the expression (typically a header) is evaluated once and gives the
/// sequence of endpoints the exchange is routed through. Sets `CamelSlipEndpoint`.
pub struct RoutingSlipDefinition {
    pub id: Option<String>,
    pub expression: ExpressionDefinition,
    pub uri_delimiter: String,
    pub ignore_invalid_endpoints: bool,
}

impl RoutingSlipDefinition {
    pub fn new(expression: ExpressionDefinition) -> Self {
        RoutingSlipDefinition { id: None, expression, uri_delimiter: ",".into(), ignore_invalid_endpoints: false }
    }
}

impl EipOptions for RoutingSlipDefinition {
    fn apply_option(&mut self, option: &EipOption) -> bool {
        match option {
            EipOption::Delimiter(d) => self.uri_delimiter = d.clone(),
            EipOption::IgnoreInvalidEndpoints(b) => self.ignore_invalid_endpoints = *b,
            _ => return false,
        }
        true
    }
}

impl ProcessorDefinition for RoutingSlipDefinition {
    fn name(&self) -> &str {
        "routingSlip"
    }
    fn label(&self) -> String {
        format!("routingSlip[{:?}]", self.expression)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let expression = self.expression.resolve_expression(&rc.context)?;
        let delim = self.uri_delimiter.clone();
        let ignore = self.ignore_invalid_endpoints;
        Ok(Arc::new(move |ex: &mut Exchange| {
            let uris = parse_uris(expression.evaluate(ex)?, &delim);
            route_through(ex, &uris, ignore, "routingSlip")?;
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}

/// Dynamic Router: the expression is evaluated repeatedly (after each step) to get
/// the next endpoint(s), until it returns null / empty. The last endpoint is
/// available in the `CamelSlipEndpoint` property when the expression is evaluated.
pub struct DynamicRouterDefinition {
    pub id: Option<String>,
    pub expression: ExpressionDefinition,
    pub uri_delimiter: String,
    pub ignore_invalid_endpoints: bool,
}

impl DynamicRouterDefinition {
    pub fn new(expression: ExpressionDefinition) -> Self {
        DynamicRouterDefinition { id: None, expression, uri_delimiter: ",".into(), ignore_invalid_endpoints: false }
    }
}

impl EipOptions for DynamicRouterDefinition {
    fn apply_option(&mut self, option: &EipOption) -> bool {
        match option {
            EipOption::Delimiter(d) => self.uri_delimiter = d.clone(),
            EipOption::IgnoreInvalidEndpoints(b) => self.ignore_invalid_endpoints = *b,
            _ => return false,
        }
        true
    }
}

impl ProcessorDefinition for DynamicRouterDefinition {
    fn name(&self) -> &str {
        "dynamicRouter"
    }
    fn label(&self) -> String {
        format!("dynamicRouter[{:?}]", self.expression)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let expression = self.expression.resolve_expression(&rc.context)?;
        let delim = self.uri_delimiter.clone();
        let ignore = self.ignore_invalid_endpoints;
        Ok(Arc::new(move |ex: &mut Exchange| {
            loop {
                let uris = parse_uris(expression.evaluate(ex)?, &delim);
                if uris.is_empty() {
                    break;
                }
                if !route_through(ex, &uris, ignore, "dynamicRouter")? {
                    break;
                }
            }
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}
