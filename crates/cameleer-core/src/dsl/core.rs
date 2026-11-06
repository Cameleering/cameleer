//! Core DSL methods on [`RouteDefinition`].

use std::sync::Arc;

use crate::builder::{simple, ExpressionDefinition, IntoExpression};
use crate::log::Level;
use crate::message::ExchangePattern;
use crate::model::{ProcessorDefinition, RouteDefinition};
use crate::processor::basic::*;
use crate::spi::Processor;

impl RouteDefinition {
    // --- route level options ---

    /// Set the route id.
    pub fn route_id(&mut self, id: impl Into<String>) -> &mut Self {
        self.id = Some(id.into());
        self
    }

    pub fn route_description(&mut self, d: impl Into<String>) -> &mut Self {
        self.description = Some(d.into());
        self
    }

    pub fn route_group(&mut self, g: impl Into<String>) -> &mut Self {
        self.group = Some(g.into());
        self
    }

    pub fn auto_startup(&mut self, auto: bool) -> &mut Self {
        self.auto_startup = auto;
        self
    }

    pub fn startup_order(&mut self, order: i32) -> &mut Self {
        self.startup_order = Some(order);
        self
    }

    /// Set the id of the last added node (or of the currently open block).
    pub fn id(&mut self, id: impl Into<String>) -> &mut Self {
        let id = id.into();
        match self.last_mut() {
            Some(d) => d.set_id(id),
            None => self.id = Some(id),
        }
        self
    }

    /// Add a custom processor definition.
    pub fn add_definition(&mut self, def: impl ProcessorDefinition + 'static) -> &mut Self {
        self.add(Box::new(def))
    }

    // --- endpoints ---

    /// Send to a static endpoint.
    pub fn to(&mut self, uri: impl Into<String>) -> &mut Self {
        self.add(Box::new(ToDefinition::new(uri)))
    }

    /// Send to several endpoints in sequence (pipeline).
    pub fn to_all(&mut self, uris: &[&str]) -> &mut Self {
        for u in uris {
            self.to(*u);
        }
        self
    }

    /// Send to a static endpoint with the given exchange pattern.
    pub fn to_with_pattern(&mut self, pattern: ExchangePattern, uri: impl Into<String>) -> &mut Self {
        self.add(Box::new(ToDefinition { id: None, uri: uri.into(), pattern: Some(pattern) }))
    }

    /// Send InOnly (fire and forget) to an endpoint.
    pub fn in_only(&mut self, uri: impl Into<String>) -> &mut Self {
        self.to_with_pattern(ExchangePattern::InOnly, uri)
    }

    /// Send InOut (request/reply) to an endpoint.
    pub fn in_out(&mut self, uri: impl Into<String>) -> &mut Self {
        self.to_with_pattern(ExchangePattern::InOut, uri)
    }

    /// Send to a dynamic endpoint (uri is a simple expression).
    pub fn to_d(&mut self, uri: impl Into<String>) -> &mut Self {
        self.add(Box::new(ToDynamicDefinition { id: None, uri: simple(uri).def, pattern: None, ignore_invalid_endpoint: false }))
    }

    /// Send to a dynamic endpoint computed by any expression.
    pub fn to_d_expr(&mut self, uri: impl IntoExpression, ignore_invalid_endpoint: bool) -> &mut Self {
        self.add(Box::new(ToDynamicDefinition { id: None, uri: uri.into_expression(), pattern: None, ignore_invalid_endpoint }))
    }

    // --- message modification ---

    pub fn set_body(&mut self, e: impl IntoExpression) -> &mut Self {
        self.add(Box::new(SetDefinition::body(e.into_expression())))
    }

    pub fn transform(&mut self, e: impl IntoExpression) -> &mut Self {
        self.add(Box::new(SetDefinition::transform(e.into_expression())))
    }

    pub fn set_header(&mut self, name: impl Into<String>, e: impl IntoExpression) -> &mut Self {
        self.add(Box::new(SetDefinition::header(name, e.into_expression())))
    }

    /// Set several headers at once.
    pub fn set_headers(&mut self, headers: Vec<(&str, ExpressionDefinition)>) -> &mut Self {
        for (k, v) in headers {
            self.set_header(k, v);
        }
        self
    }

    pub fn set_property(&mut self, name: impl Into<String>, e: impl IntoExpression) -> &mut Self {
        self.add(Box::new(SetDefinition::property(name, e.into_expression())))
    }

    pub fn set_variable(&mut self, name: impl Into<String>, e: impl IntoExpression) -> &mut Self {
        self.add(Box::new(SetDefinition::variable(name, e.into_expression())))
    }

    pub fn remove_header(&mut self, name: impl Into<String>) -> &mut Self {
        self.add(Box::new(RemoveDefinition { id: None, target: RemoveTarget::Header(name.into()) }))
    }

    pub fn remove_headers(&mut self, pattern: impl Into<String>) -> &mut Self {
        self.remove_headers_excluding(pattern, &[])
    }

    pub fn remove_headers_excluding(&mut self, pattern: impl Into<String>, excludes: &[&str]) -> &mut Self {
        self.add(Box::new(RemoveDefinition {
            id: None,
            target: RemoveTarget::Headers { pattern: pattern.into(), excludes: excludes.iter().map(|s| s.to_string()).collect() },
        }))
    }

    pub fn remove_property(&mut self, name: impl Into<String>) -> &mut Self {
        self.add(Box::new(RemoveDefinition { id: None, target: RemoveTarget::Property(name.into()) }))
    }

    pub fn remove_properties(&mut self, pattern: impl Into<String>) -> &mut Self {
        self.add(Box::new(RemoveDefinition {
            id: None,
            target: RemoveTarget::Properties { pattern: pattern.into(), excludes: vec![] },
        }))
    }

    pub fn remove_variable(&mut self, name: impl Into<String>) -> &mut Self {
        self.add(Box::new(RemoveDefinition { id: None, target: RemoveTarget::Variable(name.into()) }))
    }

    pub fn convert_body_to(&mut self, type_name: impl Into<String>) -> &mut Self {
        self.add(Box::new(ConvertDefinition { id: None, type_name: type_name.into(), header: None, mandatory: false }))
    }

    pub fn convert_header_to(&mut self, header: impl Into<String>, type_name: impl Into<String>) -> &mut Self {
        self.add(Box::new(ConvertDefinition { id: None, type_name: type_name.into(), header: Some(header.into()), mandatory: false }))
    }

    pub fn set_exchange_pattern(&mut self, pattern: ExchangePattern) -> &mut Self {
        self.add(Box::new(SetExchangePatternDefinition { id: None, pattern }))
    }

    // --- processors ---

    /// Invoke a custom processor (closure or `impl Processor`).
    /// The `agent` step: a model and its tools in a loop (see
    /// [`crate::processor::agent::AgentDefinition`]).
    #[cfg(feature = "agent")]
    pub fn agent(&mut self, def: crate::processor::agent::AgentDefinition) -> &mut Self {
        self.add(Box::new(def))
    }

    pub fn process(&mut self, p: impl Processor + 'static) -> &mut Self {
        self.add(Box::new(ProcessDefinition { id: None, processor: Some(Arc::new(p)), reference: None }))
    }

    /// Invoke a processor bound in the registry.
    pub fn process_ref(&mut self, name: impl Into<String>) -> &mut Self {
        self.add(Box::new(ProcessDefinition { id: None, processor: None, reference: Some(name.into()) }))
    }

    /// Log a message (simple expression) at INFO level.
    pub fn log(&mut self, message: impl Into<String>) -> &mut Self {
        self.log_at(Level::Info, message)
    }

    pub fn log_at(&mut self, level: Level, message: impl Into<String>) -> &mut Self {
        self.add(Box::new(LogDefinition { id: None, message: simple(message).def, level, log_name: None, marker: None }))
    }

    pub fn log_full(&mut self, level: Level, log_name: impl Into<String>, message: impl Into<String>) -> &mut Self {
        self.add(Box::new(LogDefinition {
            id: None,
            message: simple(message).def,
            level,
            log_name: Some(log_name.into()),
            marker: None,
        }))
    }

    /// Stop routing the current exchange.
    pub fn stop(&mut self) -> &mut Self {
        self.add(Box::new(StopDefinition { id: None }))
    }

    /// Open a `step` block (groups processors; closed with `end()`).
    pub fn step(&mut self, id: impl Into<String>) -> &mut Self {
        self.push_block(Box::new(StepDefinition { id: Some(id.into()), outputs: Default::default() }))
    }
}
