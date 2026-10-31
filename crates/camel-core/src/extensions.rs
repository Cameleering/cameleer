//! Registration of built-in extensions (error handling, interceptors, ...)
//! that hook into route creation.

use crate::context::CamelContext;

pub fn register_builtin_extensions(ctx: &CamelContext) {
    // error handlers, onException, interceptors, onCompletion, route policies
    crate::processor::errors::hook::register(ctx);
    // statistics, tracer, message history, health checks (management)
    crate::management::register(ctx);
}
