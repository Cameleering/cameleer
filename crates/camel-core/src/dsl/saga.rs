//! Saga EIP DSL methods.
//!
//! ```ignore
//! b.from("direct:buy")
//!     .saga()
//!         .propagation(SagaPropagation::Required)
//!         .compensation("direct:cancelOrder")
//!         .completion("direct:orderDone")
//!         .option("id", header("orderId"))
//!         .saga_timeout(Duration::from_secs(30))
//!         .to("direct:reserveCredit")
//!     .end();
//! ```

use std::time::Duration;

use crate::builder::IntoExpression;
use crate::error::CamelError;
use crate::model::RouteDefinition;
use crate::processor::saga::{SagaCompletionMode, SagaDefinition, SagaPropagation};

impl RouteDefinition {
    /// Open a `saga` block (closed with `end()`).
    pub fn saga(&mut self) -> &mut Self {
        self.push_block(Box::new(SagaDefinition::new()))
    }

    fn with_saga(&mut self, option: &str, f: impl FnOnce(&mut SagaDefinition)) -> &mut Self {
        match self.current_block_mut::<SagaDefinition>() {
            Some(s) => f(s),
            None => self.add_build_error(CamelError::illegal_argument(format!("{} must be used inside a saga block", option))),
        }
        self
    }

    /// Saga propagation (default `REQUIRED`).
    pub fn propagation(&mut self, p: SagaPropagation) -> &mut Self {
        self.with_saga("propagation", |s| s.propagation = p)
    }

    /// Saga completion mode (default `AUTO`).
    pub fn completion_mode(&mut self, m: SagaCompletionMode) -> &mut Self {
        self.with_saga("completionMode", |s| s.completion_mode = m)
    }

    /// Compensate the saga if it did not complete within this time.
    pub fn saga_timeout(&mut self, d: Duration) -> &mut Self {
        self.with_saga("timeout", |s| s.timeout = Some(d))
    }

    /// Endpoint called when the saga is compensated.
    pub fn compensation(&mut self, uri: impl Into<String>) -> &mut Self {
        let uri = uri.into();
        self.with_saga("compensation", |s| s.compensation = Some(uri))
    }

    /// Endpoint called when the saga completes.
    pub fn completion(&mut self, uri: impl Into<String>) -> &mut Self {
        let uri = uri.into();
        self.with_saga("completion", |s| s.completion = Some(uri))
    }

    /// An option evaluated when the step begins, passed as a header to the
    /// compensation/completion endpoints.
    pub fn option(&mut self, name: impl Into<String>, e: impl IntoExpression) -> &mut Self {
        let (name, e) = (name.into(), e.into_expression());
        self.with_saga("option", |s| s.options.push((name, e)))
    }

    /// Use the saga service bound in the registry under `name`.
    pub fn saga_service(&mut self, name: impl Into<String>) -> &mut Self {
        let name = name.into();
        self.with_saga("sagaService", |s| s.saga_service = Some(name))
    }
}
