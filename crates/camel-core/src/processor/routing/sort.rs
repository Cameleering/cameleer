//! Sort EIP: sort the list given by an expression and set it as the body.

use std::cmp::Ordering;
use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::Result;
use crate::impl_definition_boilerplate;
use crate::message::Exchange;
use crate::model::{ProcessorDefinition, RouteContext};
use crate::spi::Processor;
use crate::value::{compare_values, Value};

use super::support::to_iterable;

/// A comparator for [`SortDefinition`].
pub type Comparator = Arc<dyn Fn(&Value, &Value) -> Ordering + Send + Sync>;

/// Sort: the expression (default: body) gives a list which is sorted (default:
/// natural ordering via `compare_values`, or a custom comparator) and set as the body.
pub struct SortDefinition {
    pub id: Option<String>,
    pub expression: ExpressionDefinition,
    pub comparator: Option<Comparator>,
}

/// Natural ordering of values (nulls first).
pub fn natural_order(a: &Value, b: &Value) -> Ordering {
    match compare_values(a, b) {
        Some(o) => o,
        None => match (a.is_null(), b.is_null()) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => a.to_string().cmp(&b.to_string()),
        },
    }
}

impl ProcessorDefinition for SortDefinition {
    fn name(&self) -> &str {
        "sort"
    }
    fn label(&self) -> String {
        format!("sort[{:?}]", self.expression)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let e = self.expression.resolve_expression(&rc.context)?;
        let cmp = self.comparator.clone();
        Ok(Arc::new(move |ex: &mut Exchange| {
            let mut list = to_iterable(e.evaluate(ex)?, None);
            match &cmp {
                Some(c) => list.sort_by(|a, b| c(a, b)),
                None => list.sort_by(natural_order),
            }
            ex.message.body = Value::Array(list);
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}
