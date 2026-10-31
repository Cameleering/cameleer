//! Built-in components shipped with camel-core.

use std::sync::Arc;

use crate::context::CamelContext;

pub mod direct;
pub mod log;
pub mod mock;

/// Register built-in components on a new context. Components are created lazily.
pub fn register_builtin_components(ctx: &CamelContext) {
    ctx.add_component_factory("direct", || Arc::new(direct::DirectComponent::new()));
    ctx.add_component_factory("mock", || Arc::new(mock::MockComponent::new()));
    ctx.add_component_factory("log", || Arc::new(log::LogComponent));
}
