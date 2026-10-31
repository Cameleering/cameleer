//! Built-in aggregation strategies (Camel `org.apache.camel.processor.aggregate.*`
//! and `AggregationStrategies` builder).
//!
//! Strategies are used by aggregate, split, multicast, recipientList and enrich.
//! The list based strategies (grouped body/exchange, flexible) collect values in an
//! exchange property while aggregating and move the result into the message on
//! completion (`AggregationStrategy::on_completion`), like Camel does.

use std::sync::Arc;

use crate::builder::IntoExpression;
use crate::context::CamelContext;
use crate::error::Result;
use crate::message::Exchange;
use crate::spi::{AggregationStrategy, Expression, Predicate};
use crate::value::{ObjectMap, Value};

use super::support::{LazyExpr, LazyPred};

/// Property holding the list collected by the grouped strategies.
pub const GROUPED_EXCHANGE: &str = "CamelGroupedExchange";
/// Property holding the values collected by [`FlexibleAggregationStrategy`].
pub const FLEXIBLE_COLLECTION: &str = "CamelFlexAggrStrategy";
/// Set this property (to true) on the aggregated exchange from a strategy to
/// complete the current group (completed by `strategy`).
pub const AGGREGATION_COMPLETE_CURRENT_GROUP: &str = "CamelAggregationCompleteCurrentGroup";
/// Set on an incoming exchange to force completion of all groups (the exchange itself is not aggregated).
pub const AGGREGATION_COMPLETE_ALL_GROUPS: &str = "CamelAggregationCompleteAllGroups";
/// Set on an incoming exchange to aggregate it and then force completion of all groups.
pub const AGGREGATION_COMPLETE_ALL_GROUPS_INCLUSIVE: &str = "CamelAggregationCompleteAllGroupsInclusive";

/// Keep the latest exchange (the new one). A failure of an earlier exchange is kept.
#[derive(Clone, Debug, Default)]
pub struct UseLatestAggregationStrategy;

impl AggregationStrategy for UseLatestAggregationStrategy {
    fn aggregate(&self, old: Option<Exchange>, mut new: Exchange) -> Result<Exchange> {
        if let Some(old) = old {
            if new.exception.is_none() && old.exception.is_some() {
                new.exception = old.exception;
            }
        }
        Ok(new)
    }
}

/// Keep the original (first) exchange; failures of later exchanges are propagated
/// onto it unless `propagate_exception` is false.
#[derive(Clone, Debug)]
pub struct UseOriginalAggregationStrategy {
    pub propagate_exception: bool,
}

impl Default for UseOriginalAggregationStrategy {
    fn default() -> Self {
        UseOriginalAggregationStrategy { propagate_exception: true }
    }
}

impl UseOriginalAggregationStrategy {
    pub fn new(propagate_exception: bool) -> Self {
        UseOriginalAggregationStrategy { propagate_exception }
    }
}

impl AggregationStrategy for UseOriginalAggregationStrategy {
    fn aggregate(&self, old: Option<Exchange>, new: Exchange) -> Result<Exchange> {
        match old {
            None => Ok(new),
            Some(mut old) => {
                if self.propagate_exception && old.exception.is_none() {
                    if let Some(e) = new.exception {
                        old.exception = Some(e);
                    }
                }
                Ok(old)
            }
        }
    }
}

fn push_to_list(ex: &mut Exchange, key: &str, v: Value) {
    match ex.properties.get_mut(key) {
        Some(Value::Array(a)) => a.push(v),
        _ => {
            ex.properties.insert(key.to_string(), Value::Array(vec![v]));
        }
    }
}

/// Group the message bodies into an array, set as the body on completion.
#[derive(Clone, Debug, Default)]
pub struct GroupedBodyAggregationStrategy;

impl AggregationStrategy for GroupedBodyAggregationStrategy {
    fn aggregate(&self, old: Option<Exchange>, new: Exchange) -> Result<Exchange> {
        let body = new.message.body.clone();
        let mut result = old.unwrap_or(new);
        push_to_list(&mut result, GROUPED_EXCHANGE, body);
        Ok(result)
    }
    fn on_completion(&self, exchange: &mut Exchange) {
        if let Some(list) = exchange.properties.remove(GROUPED_EXCHANGE) {
            exchange.message.body = list;
        }
    }
}

/// Snapshot of an exchange as a value: `{exchangeId, body, headers, properties}`.
pub fn exchange_to_value(ex: &Exchange) -> Value {
    let props: ObjectMap =
        ex.properties.iter().filter(|(k, _)| k.as_str() != GROUPED_EXCHANGE).map(|(k, v)| (k.clone(), v.clone())).collect();
    Value::object([
        ("exchangeId", Value::String(ex.exchange_id.clone())),
        ("body", ex.message.body.clone()),
        ("headers", ex.message.headers.to_value()),
        ("properties", Value::Object(props)),
    ])
}

/// Group the exchanges into a list (`CamelGroupedExchange` property); on completion
/// the body is set to the list. Since bodies are values, each exchange is represented
/// by [`exchange_to_value`].
#[derive(Clone, Debug, Default)]
pub struct GroupedExchangeAggregationStrategy;

impl AggregationStrategy for GroupedExchangeAggregationStrategy {
    fn aggregate(&self, old: Option<Exchange>, new: Exchange) -> Result<Exchange> {
        let snap = exchange_to_value(&new);
        let mut result = old.unwrap_or(new);
        push_to_list(&mut result, GROUPED_EXCHANGE, snap);
        Ok(result)
    }
    fn on_completion(&self, exchange: &mut Exchange) {
        if let Some(list) = exchange.properties.get(GROUPED_EXCHANGE).cloned() {
            exchange.message.body = list;
        }
    }
}

/// Concatenate the (picked) values as strings with a delimiter into the body.
pub struct StringAggregationStrategy {
    pub delimiter: String,
    pick: Option<Arc<dyn Expression>>,
}

impl StringAggregationStrategy {
    pub fn new(delimiter: impl Into<String>) -> Self {
        StringAggregationStrategy { delimiter: delimiter.into(), pick: None }
    }
    /// Pick the value to concatenate (default: the body).
    pub fn pick(mut self, e: impl IntoExpression) -> Self {
        self.pick = Some(Arc::new(LazyExpr::new(e.into_expression())));
        self
    }
}

impl AggregationStrategy for StringAggregationStrategy {
    fn aggregate(&self, old: Option<Exchange>, mut new: Exchange) -> Result<Exchange> {
        let v = match &self.pick {
            Some(p) => p.evaluate(&new)?,
            None => new.message.body.clone(),
        };
        match old {
            None => {
                new.message.body = Value::String(v.to_string());
                Ok(new)
            }
            Some(mut old) => {
                let s = format!("{}{}{}", old.message.body, self.delimiter, v);
                old.message.body = Value::String(s);
                Ok(old)
            }
        }
    }
}

/// Where [`FlexibleAggregationStrategy`] stores its result.
#[derive(Clone, Debug, PartialEq)]
pub enum FlexibleTarget {
    Body,
    Header(String),
    Property(String),
}

/// A lightweight `FlexibleAggregationStrategy`: picks a value from each exchange
/// (default: body), optionally only when a condition matches, and collects the
/// values into an array (or keeps only the last one) stored in the body, a header
/// or a property on completion.
pub struct FlexibleAggregationStrategy {
    pick: Option<Arc<dyn Expression>>,
    condition: Option<Arc<dyn Predicate>>,
    target: FlexibleTarget,
    accumulate: bool,
    ignore_nulls: bool,
}

impl Default for FlexibleAggregationStrategy {
    fn default() -> Self {
        FlexibleAggregationStrategy { pick: None, condition: None, target: FlexibleTarget::Body, accumulate: true, ignore_nulls: false }
    }
}

impl FlexibleAggregationStrategy {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn pick(mut self, e: impl IntoExpression) -> Self {
        self.pick = Some(Arc::new(LazyExpr::new(e.into_expression())));
        self
    }
    pub fn condition(mut self, p: impl IntoExpression) -> Self {
        self.condition = Some(Arc::new(LazyPred::new(p.into_expression())));
        self
    }
    pub fn store_in_body(mut self) -> Self {
        self.target = FlexibleTarget::Body;
        self
    }
    pub fn store_in_header(mut self, name: impl Into<String>) -> Self {
        self.target = FlexibleTarget::Header(name.into());
        self
    }
    pub fn store_in_property(mut self, name: impl Into<String>) -> Self {
        self.target = FlexibleTarget::Property(name.into());
        self
    }
    /// Collect into an array (default true); false keeps only the last picked value.
    pub fn accumulate_in_collection(mut self, b: bool) -> Self {
        self.accumulate = b;
        self
    }
    /// Skip null picked values.
    pub fn ignore_nulls(mut self) -> Self {
        self.ignore_nulls = true;
        self
    }
}

impl AggregationStrategy for FlexibleAggregationStrategy {
    fn aggregate(&self, old: Option<Exchange>, new: Exchange) -> Result<Exchange> {
        let include = match &self.condition {
            Some(c) => c.matches(&new)?,
            None => true,
        };
        let v = if include {
            Some(match &self.pick {
                Some(p) => p.evaluate(&new)?,
                None => new.message.body.clone(),
            })
        } else {
            None
        };
        let mut result = old.unwrap_or(new);
        if !result.properties.contains_key(FLEXIBLE_COLLECTION) {
            result.properties.insert(FLEXIBLE_COLLECTION.to_string(), Value::Array(vec![]));
        }
        if let Some(v) = v {
            if !(self.ignore_nulls && v.is_null()) {
                push_to_list(&mut result, FLEXIBLE_COLLECTION, v);
            }
        }
        Ok(result)
    }
    fn on_completion(&self, exchange: &mut Exchange) {
        let list = match exchange.properties.remove(FLEXIBLE_COLLECTION) {
            Some(Value::Array(a)) => a,
            _ => return,
        };
        let v = if self.accumulate { Value::Array(list) } else { list.into_iter().last().unwrap_or(Value::Null) };
        match &self.target {
            FlexibleTarget::Body => exchange.message.body = v,
            FlexibleTarget::Header(h) => exchange.message.headers.set(h.clone(), v),
            FlexibleTarget::Property(p) => {
                exchange.properties.insert(p.clone(), v);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// AggregationStrategies-like factory functions
// ---------------------------------------------------------------------------

/// `AggregationStrategies.groupedBody()`.
pub fn grouped_body() -> GroupedBodyAggregationStrategy {
    GroupedBodyAggregationStrategy
}
/// `AggregationStrategies.groupedExchange()`.
pub fn grouped_exchange() -> GroupedExchangeAggregationStrategy {
    GroupedExchangeAggregationStrategy
}
/// `AggregationStrategies.useLatest()`.
pub fn use_latest() -> UseLatestAggregationStrategy {
    UseLatestAggregationStrategy
}
/// `AggregationStrategies.useOriginal()`.
pub fn use_original() -> UseOriginalAggregationStrategy {
    UseOriginalAggregationStrategy::default()
}
/// `AggregationStrategies.string(delimiter)`.
pub fn string(delimiter: &str) -> StringAggregationStrategy {
    StringAggregationStrategy::new(delimiter)
}
/// `AggregationStrategies.flexible()`.
pub fn flexible() -> FlexibleAggregationStrategy {
    FlexibleAggregationStrategy::new()
}

/// Look up an aggregation strategy from the registry by name.
pub fn aggregation_strategy_ref(ctx: &CamelContext, name: &str) -> Result<Arc<dyn AggregationStrategy>> {
    super::support::aggregation_strategy_ref(ctx, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(body: &str) -> Exchange {
        Exchange::with_body(None, body)
    }

    #[test]
    fn grouped_body_collects() {
        let s = GroupedBodyAggregationStrategy;
        let a = s.aggregate(None, ex("a")).unwrap();
        let mut b = s.aggregate(Some(a), ex("b")).unwrap();
        s.on_completion(&mut b);
        assert_eq!(b.message.body, Value::from(vec!["a", "b"]));
    }

    #[test]
    fn string_concat() {
        let s = string("+");
        let a = s.aggregate(None, ex("a")).unwrap();
        let b = s.aggregate(Some(a), ex("b")).unwrap();
        assert_eq!(b.message.body, Value::from("a+b"));
    }

    #[test]
    fn use_latest_keeps_failure() {
        let s = use_latest();
        let mut a = ex("a");
        a.set_exception(crate::error::CamelError::io("x"));
        let b = s.aggregate(Some(a), ex("b")).unwrap();
        assert!(b.is_failed());
        assert_eq!(b.message.body, Value::from("b"));
    }

    #[test]
    fn flexible_condition() {
        let s = flexible().condition(crate::builder::body().is_not_equal_to("skip")).store_in_header("all");
        let a = s.aggregate(None, ex("a")).unwrap();
        let b = s.aggregate(Some(a), ex("skip")).unwrap();
        let mut c = s.aggregate(Some(b), ex("c")).unwrap();
        s.on_completion(&mut c);
        assert_eq!(c.header("all").unwrap(), &Value::from(vec!["a", "c"]));
    }
}
