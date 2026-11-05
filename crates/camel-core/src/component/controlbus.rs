//! `controlbus:` component - manage routes and evaluate scripts from messages.
//!
//! * `controlbus:route?routeId=foo&action=start|stop|suspend|resume|restart|status|stats|fail`
//!   (`routeId=current` = the route of the exchange; `async=true` runs the action in the
//!   background; `restartDelay` (default 1000ms) for restart; `loggingLevel`).
//!   `status` and `stats` set the result as the message body. Without `routeId`,
//!   `status` returns the context status and `stats` the statistics of all routes.
//! * `controlbus:language:simple` - evaluates the message body as a script in the given
//!   language and sets the result as the body.
//!
//! Stopping/suspending/restarting the route that is processing the exchange is always
//! done asynchronously (it could otherwise not complete gracefully).

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use crate::context::{CamelContext, WeakContext};
use crate::error::{CamelError, Result};
use crate::log::Level;
use crate::message::Exchange;
use crate::spi::{Component, Endpoint, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::Value;

#[derive(Default)]
pub struct ControlBusComponent;

#[derive(Clone, Debug)]
enum Command {
    Route { route_id: Option<String>, action: String, run_async: bool, restart_delay: Duration },
    Language(String),
}

impl Component for ControlBusComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let level = params.take("loggingLevel").and_then(|l| Level::parse(&l)).unwrap_or(Level::Info);
        let command = if let Some(lang) = remaining.strip_prefix("language:") {
            ctx.resolve_language(lang)?;
            Command::Language(lang.to_string())
        } else if remaining == "route" {
            let action = params.take_string("action", "status");
            if !["start", "stop", "suspend", "resume", "restart", "status", "stats", "fail"].contains(&action.as_str()) {
                return Err(CamelError::illegal_argument(format!("Unsupported controlbus action: {}", action)));
            }
            Command::Route {
                route_id: params.take("routeId").filter(|s| !s.is_empty()),
                action,
                run_async: params.take_bool("async", false)?,
                restart_delay: params.take_duration("restartDelay", Duration::from_millis(1000))?,
            }
        } else {
            return Err(CamelError::illegal_argument(format!("Unsupported controlbus command: {} (use route or language:name)", remaining)));
        };
        Ok(Arc::new(ControlBusEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), command, level }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct ControlBusEndpoint {
    uri: String,
    ctx: WeakContext,
    command: Command,
    level: Level,
}

impl Endpoint for ControlBusEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(ControlBusProducer { ctx: self.ctx.clone(), command: self.command.clone(), level: self.level }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct ControlBusProducer {
    ctx: WeakContext,
    command: Command,
    level: Level,
}

impl Service for ControlBusProducer {}

fn run_action(ctx: &CamelContext, id: &str, action: &str, restart_delay: Duration) -> Result<Value> {
    Ok(match action {
        "start" => {
            ctx.start_route(id)?;
            Value::Null
        }
        "stop" => {
            ctx.stop_route(id)?;
            Value::Null
        }
        "suspend" => {
            ctx.suspend_route(id)?;
            Value::Null
        }
        "resume" => {
            ctx.resume_route(id)?;
            Value::Null
        }
        "restart" => {
            ctx.stop_route(id)?;
            std::thread::sleep(restart_delay);
            ctx.start_route(id)?;
            Value::Null
        }
        "fail" => {
            ctx.stop_route(id)?;
            Value::Null
        }
        "status" => Value::String(ctx.route_status(id).map(|s| s.to_string()).ok_or_else(|| no_route(id))?),
        "stats" => ctx.route(id).ok_or_else(|| no_route(id))?.stats.snapshot(),
        other => return Err(CamelError::illegal_argument(format!("Unsupported action: {}", other))),
    })
}

fn no_route(id: &str) -> CamelError {
    CamelError::illegal_argument(format!("Cannot find route with id: {}", id))
}

impl Processor for ControlBusProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let ctx = exchange.context.clone().or_else(|| self.ctx.upgrade()).ok_or_else(|| CamelError::illegal_state("No CamelContext"))?;
        match &self.command {
            Command::Language(lang) => {
                let script = exchange.body_as::<String>()?;
                let expr = ctx.resolve_language(lang)?.create_expression(&script, &Default::default())?;
                let v = expr.evaluate(exchange)?;
                crate::log::log(self.level, "controlbus", format!("Evaluated {} script: {} -> {}", lang, script, v));
                exchange.set_body(v);
                Ok(())
            }
            Command::Route { route_id, action, run_async, restart_delay } => {
                let id = match route_id.as_deref() {
                    Some("current") => exchange.from_route_id.as_deref().map(str::to_string),
                    Some(id) => Some(id.to_string()),
                    None => None,
                };
                let Some(id) = id else {
                    let v = match action.as_str() {
                        "status" => Value::String(ctx.status().to_string()),
                        "stats" => Value::Object(ctx.routes().iter().map(|r| (r.id.clone(), r.stats.snapshot())).collect()),
                        _ => return Err(CamelError::illegal_argument(format!("routeId is required for action {}", action))),
                    };
                    exchange.set_body(v);
                    return Ok(());
                };
                if ctx.route(&id).is_none() {
                    return Err(no_route(&id));
                }
                let mutating = !matches!(action.as_str(), "status" | "stats");
                let self_route = exchange.from_route_id.as_deref() == Some(id.as_str());
                if mutating && (*run_async || self_route) {
                    let (ctx2, id2, action2, delay) = (ctx.downgrade(), id.clone(), action.clone(), *restart_delay);
                    let level = self.level;
                    std::thread::Builder::new()
                        .name(format!("Camel controlbus {} {}", action, id))
                        .spawn(move || {
                            if let Some(c) = ctx2.upgrade() {
                                if let Err(e) = run_action(&c, &id2, &action2, delay) {
                                    crate::camel_warn!("controlbus", "Error executing {} on route {}: {}", action2, id2, e);
                                } else {
                                    crate::log::log(level, "controlbus", format!("Route {} {} done", id2, action2));
                                }
                            }
                        })
                        .map_err(|e| CamelError::illegal_state(e.to_string()))?;
                    return Ok(());
                }
                crate::log::log(self.level, "controlbus", format!("ControlBus task: {} route {}", action, id));
                let v = run_action(&ctx, &id, action, *restart_delay)?;
                if !v.is_null() {
                    exchange.set_body(v);
                }
                Ok(())
            }
        }
    }
}
