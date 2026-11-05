//! Route-level configuration: `routeConfiguration` (onException, onCompletion,
//! intercept, interceptFrom, interceptSendToEndpoint, errorHandler), top-level
//! `errorHandler` and the redelivery policy.
//!
//! A `routeConfiguration` is mapped to cameleer-core's builder scope
//! ([`BuilderScope`]): routes referencing it with `routeConfigurationId` (or every
//! route without `routeConfigurationId` for configurations without `id`) get a
//! [`BuilderScopeHandle`] in their extensions, exactly like routes created by a
//! `RouteBuilder` with builder-scoped `on_exception(..)` clauses.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use cameleer_core::log::Level;
use cameleer_core::processor::errors::handler::{dead_letter_channel, default_error_handler, no_error_handler, ErrorHandlerBuilder, RedeliveryOverrides};
use cameleer_core::processor::errors::hook::{BuilderScope, BuilderScopeHandle, BUILDER_SCOPE};
use cameleer_core::processor::errors::intercept::InterceptDefinition;
use cameleer_core::processor::errors::on_completion::OnCompletionDefinition;
use cameleer_core::processor::errors::on_exception::OnExceptionDefinition;
use cameleer_core::processor::errors::RedeliveryPolicy;
use cameleer_core::util::matches_pattern;

use super::eips::{lookup_processor, ref_name, unwrap_item, val_bool, val_duration, val_f64, val_i64, val_str};
use super::*;

/// Elements allowed inside a `routeConfiguration` (and accepted at top level as an
/// anonymous route configuration).
pub const ROUTE_CONFIGURATION_ITEMS: &[&str] = &["onException", "onCompletion", "intercept", "interceptFrom", "interceptSendToEndpoint"];

/// Redelivery options not covered by [`RedeliveryOverrides`].
#[derive(Clone, Debug, Default)]
pub struct RedeliveryExtras {
    pub collision_avoidance_factor: Option<f64>,
    pub log_retry_attempted: Option<bool>,
}

fn level(camel: &CamelContext, v: &Value) -> Result<Level> {
    let s = val_str(camel, v)?;
    Level::parse(&s).ok_or_else(|| dsl_error(format!("invalid logging level '{}'", s)))
}

/// Parse a `redeliveryPolicy` mapping (`maximumRedeliveries`, `redeliveryDelay`,
/// `backOffMultiplier`, `useExponentialBackOff`, `maximumRedeliveryDelay`,
/// `useCollisionAvoidance`, `collisionAvoidanceFactor`, `retryAttemptedLogLevel`,
/// `retriesExhaustedLogLevel`, `logStackTrace`, `logExhausted`, `logRetryAttempted`,
/// `delayPattern`, `asyncDelayedRedelivery`). Other Camel logging options are accepted and ignored.
pub fn parse_redelivery_overrides(camel: &CamelContext, v: &Value) -> Result<(RedeliveryOverrides, RedeliveryExtras)> {
    let mut o = RedeliveryOverrides::default();
    let mut x = RedeliveryExtras::default();
    let map = match v {
        Value::Object(m) => m,
        Value::Null => return Ok((o, x)),
        other => return Err(dsl_error(format!("'redeliveryPolicy' must be a mapping but found {}", other.type_name()))),
    };
    for (k, val) in map {
        let wrap = |e: CamelError| dsl_error(format!("redeliveryPolicy option '{}': {}", k, e.message));
        match k.as_str() {
            "maximumRedeliveries" => o.maximum_redeliveries = Some(val_i64(camel, val).map_err(wrap)? as i32),
            "redeliveryDelay" => o.redelivery_delay = Some(val_duration(camel, val, 1).map_err(wrap)?.as_millis() as u64),
            "backOffMultiplier" => o.back_off_multiplier = Some(val_f64(camel, val).map_err(wrap)?),
            "useExponentialBackOff" => o.use_exponential_back_off = Some(val_bool(camel, val).map_err(wrap)?),
            "maximumRedeliveryDelay" => o.maximum_redelivery_delay = Some(val_duration(camel, val, 1).map_err(wrap)?.as_millis() as u64),
            "useCollisionAvoidance" => o.use_collision_avoidance = Some(val_bool(camel, val).map_err(wrap)?),
            "collisionAvoidanceFactor" => x.collision_avoidance_factor = Some(val_f64(camel, val).map_err(wrap)?),
            "retryAttemptedLogLevel" => o.retry_attempted_log_level = Some(level(camel, val).map_err(wrap)?),
            "retriesExhaustedLogLevel" => o.retries_exhausted_log_level = Some(level(camel, val).map_err(wrap)?),
            "logStackTrace" => o.log_stack_trace = Some(val_bool(camel, val).map_err(wrap)?),
            "logExhausted" => o.log_exhausted = Some(val_bool(camel, val).map_err(wrap)?),
            "logRetryAttempted" => x.log_retry_attempted = Some(val_bool(camel, val).map_err(wrap)?),
            "delayPattern" => o.delay_pattern = Some(val_str(camel, val).map_err(wrap)?),
            "asyncDelayedRedelivery" => o.async_delayed_redelivery = Some(val_bool(camel, val).map_err(wrap)?),
            "id" => {}
            other => cameleer_core::camel_debug!("cameleer-yaml", "redeliveryPolicy option '{}' is not supported and is ignored", other),
        }
    }
    Ok((o, x))
}

/// Apply a `redeliveryPolicy` mapping on a policy.
pub fn apply_redelivery_policy(camel: &CamelContext, base: &RedeliveryPolicy, v: &Value) -> Result<RedeliveryPolicy> {
    let (o, x) = parse_redelivery_overrides(camel, v)?;
    let mut p = o.apply(base);
    if let Some(f) = x.collision_avoidance_factor {
        p.collision_avoidance_factor = f;
    }
    if let Some(b) = x.log_retry_attempted {
        p.log_retry_attempted = b;
    }
    Ok(p)
}

/// Parse an error handler definition:
///
/// ```yaml
/// deadLetterChannel:
///   deadLetterUri: mock:dead
///   useOriginalMessage: true
///   redeliveryPolicy: {maximumRedeliveries: 3, redeliveryDelay: 0}
/// ```
///
/// Also `defaultErrorHandler: {redeliveryPolicy: ...}`, `noErrorHandler: {}` and
/// `refErrorHandler: myHandler` (an [`ErrorHandlerBuilder`] bound in the registry).
pub fn parse_error_handler(camel: &CamelContext, v: &Value) -> Result<ErrorHandlerBuilder> {
    let lookup = |r: &str| -> Result<ErrorHandlerBuilder> {
        let n = ref_name(r);
        camel
            .registry()
            .lookup::<ErrorHandlerBuilder>(&n)
            .map(|e| (*e).clone())
            .ok_or_else(|| dsl_error(format!("No error handler named '{}' found in the registry", n)))
    };
    let (kind, cfg): (&str, &Value) = match v {
        Value::String(s) => match s.as_str() {
            "noErrorHandler" | "defaultErrorHandler" => (s.as_str(), &Value::Null),
            other => return lookup(other),
        },
        Value::Object(m) => {
            let keys: Vec<&String> = m.keys().filter(|k| k.as_str() != "id").collect();
            if keys.len() != 1 {
                return Err(dsl_error(format!(
                    "an errorHandler must contain exactly one of deadLetterChannel, defaultErrorHandler, noErrorHandler, refErrorHandler but found [{}]",
                    keys.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")
                )));
            }
            (keys[0].as_str(), &m[keys[0]])
        }
        other => return Err(dsl_error(format!("an errorHandler must be a mapping but found {}", other.type_name()))),
    };
    let mut eh = match kind {
        "deadLetterChannel" => {
            let uri = cfg
                .get("deadLetterUri")
                .map(endpoint_uri)
                .transpose()?
                .ok_or_else(|| dsl_error("deadLetterChannel requires 'deadLetterUri'"))?;
            dead_letter_channel(uri)
        }
        "defaultErrorHandler" | "jtaTransactionErrorHandler" | "springTransactionErrorHandler" => default_error_handler(),
        "noErrorHandler" => return Ok(no_error_handler()),
        "refErrorHandler" => {
            let r = match cfg {
                Value::Object(_) => cfg.get("ref").and_then(scalar_string).ok_or_else(|| dsl_error("refErrorHandler requires 'ref'"))?,
                other => scalar_string(other).ok_or_else(|| dsl_error("refErrorHandler requires a reference"))?,
            };
            return lookup(&r);
        }
        other => {
            return Err(dsl_error(format!(
                "unknown error handler '{}' (deadLetterChannel, defaultErrorHandler, noErrorHandler, refErrorHandler)",
                other
            )))
        }
    };
    let map = match cfg {
        Value::Object(m) => m.clone(),
        _ => cameleer_core::value::ObjectMap::new(),
    };
    for (k, val) in &map {
        let wrap = |e: CamelError| dsl_error(format!("{} option '{}': {}", kind, k, e.message));
        match k.as_str() {
            "deadLetterUri" | "id" | "level" | "logName" | "loggerRef" | "executorServiceRef" | "transactionTemplateRef" | "transactionManagerRef"
            | "rollbackLoggingLevel" => {}
            "redeliveryPolicy" => eh.policy = apply_redelivery_policy(camel, &eh.policy, unwrap_item(val, "redeliveryPolicy")).map_err(wrap)?,
            "redeliveryPolicyRef" => {
                let n = ref_name(&val_str(camel, val).map_err(wrap)?);
                let p = camel.registry().lookup::<RedeliveryPolicy>(&n).ok_or_else(|| wrap(dsl_error(format!("No RedeliveryPolicy named '{}' found in the registry", n))))?;
                eh.policy = (*p).clone();
            }
            "useOriginalMessage" => eh.use_original_message = val_bool(camel, val).map_err(wrap)?,
            "useOriginalBody" => eh.use_original_body = val_bool(camel, val).map_err(wrap)?,
            "deadLetterHandleNewException" => eh.dead_letter_handle_new_exception = val_bool(camel, val).map_err(wrap)?,
            "onRedeliveryRef" | "onRedelivery" => eh.on_redelivery = Some(lookup_processor(camel, &val_str(camel, val).map_err(wrap)?).map_err(wrap)?),
            "onExceptionOccurredRef" | "onExceptionOccurred" => {
                eh.on_exception_occurred = Some(lookup_processor(camel, &val_str(camel, val).map_err(wrap)?).map_err(wrap)?)
            }
            "onPrepareFailureRef" | "onPrepareFailure" => {
                eh.on_prepare_failure = Some(lookup_processor(camel, &val_str(camel, val).map_err(wrap)?).map_err(wrap)?)
            }
            "retryWhile" => eh.retry_while = Some(parse_expression(val).map_err(wrap)?),
            other => return Err(wrap(dsl_error(format!("unknown option '{}'", other)))),
        }
    }
    Ok(eh)
}

/// A route configuration source collected from the YAML state.
struct ConfigEntry {
    id: Option<String>,
    value: Value,
}

fn collect_configurations(ctx: &CamelContext) -> Vec<ConfigEntry> {
    let mut out = Vec::new();
    for (kind, v) in yaml_state(ctx).all_configurations() {
        if kind == "routeConfiguration" {
            out.push(ConfigEntry { id: v.get("id").and_then(scalar_string), value: v });
        } else if ROUTE_CONFIGURATION_ITEMS.contains(&kind.as_str()) {
            out.push(ConfigEntry { id: None, value: Value::object([(kind, v)]) });
        }
    }
    out
}

impl YamlLoader {
    /// Build the builder scope of one `routeConfiguration` value.
    pub(super) fn build_route_configuration(&self, camel: &CamelContext, source: &Source<'_>, cfg: &Value, path: Vec<PathSegment>) -> Result<BuilderScope> {
        let map = match cfg {
            Value::Object(m) => m.clone(),
            Value::Null => cameleer_core::value::ObjectMap::new(),
            other => {
                return Err(dsl_error(format!("A routeConfiguration must be a mapping but found {} {}", other.type_name(), source.location(&path))))
            }
        };
        let mut scope = BuilderScope::default();
        let mut scratch = RouteDefinition::new("routeConfiguration");
        {
            let mut sc = StepContext {
                route: &mut scratch,
                camel,
                loader: self,
                source,
                path: path.clone(),
                step_name: "routeConfiguration".into(),
                pending_id: None,
                dangling: 0,
            };
            for (k, v) in &map {
                let mut kp = path.clone();
                kp.push(PathSegment::Key(k.clone()));
                match k.as_str() {
                    "id" | "description" | "precondition" => {}
                    "errorHandler" => {
                        let eh = parse_error_handler(camel, v).map_err(|e| dsl_error(format!("Invalid errorHandler {}: {}", source.location(&kp), e.message)))?;
                        scope.error_handler = Some(eh);
                    }
                    "steps" => sc.load_steps(v)?,
                    item if ROUTE_CONFIGURATION_ITEMS.contains(&item) => match v {
                        Value::Null => {}
                        Value::Array(a) => {
                            for (i, it) in a.iter().enumerate() {
                                let mut ip = kp.clone();
                                ip.push(PathSegment::Index(i));
                                if matches!(it, Value::Object(m) if m.len() == 1 && m.contains_key(item)) {
                                    sc.load_step_at(it, ip)?;
                                } else {
                                    sc.load_step_at(&Value::object([(item, it.clone())]), ip)?;
                                }
                            }
                        }
                        Value::Object(m) if m.len() == 1 && m.contains_key(item) => sc.load_step_at(v, kp)?,
                        _ => sc.load_step_at(&Value::object([(item, v.clone())]), path.clone())?,
                    },
                    other => {
                        return Err(dsl_error(format!(
                            "Unknown routeConfiguration option '{}' {}. Supported: id, errorHandler, {}",
                            other,
                            source.location(&kp),
                            ROUTE_CONFIGURATION_ITEMS.join(", ")
                        )))
                    }
                }
            }
        }
        scratch.finish().map_err(|e| dsl_error(format!("Invalid routeConfiguration {}: {}", source.location(&path), e.message)))?;
        for d in &scratch.outputs {
            let a = d.as_any();
            if !(a.is::<OnExceptionDefinition>() || a.is::<OnCompletionDefinition>() || a.is::<InterceptDefinition>()) {
                return Err(dsl_error(format!(
                    "A routeConfiguration may only contain {} but found '{}' {}",
                    ROUTE_CONFIGURATION_ITEMS.join(", "),
                    d.name(),
                    source.location(&path)
                )));
            }
        }
        scope.outputs = scratch.outputs;
        Ok(scope)
    }

    /// Link routes to the route configurations that apply to them (see the module docs).
    pub(super) fn apply_route_configurations(&self, ctx: &CamelContext, routes: &mut [RouteDefinition]) -> Result<()> {
        let configs = collect_configurations(ctx);
        if routes.is_empty() {
            return Ok(());
        }
        let mut cache: BTreeMap<Vec<usize>, BuilderScopeHandle> = BTreeMap::new();
        for r in routes.iter_mut() {
            if r.extensions.contains_key(BUILDER_SCOPE) {
                continue;
            }
            let ids: Vec<String> = r
                .properties
                .get("routeConfigurationId")
                .map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
                .unwrap_or_default();
            let mut selected: Vec<usize> = Vec::new();
            if ids.is_empty() {
                selected.extend(configs.iter().enumerate().filter(|(_, c)| c.id.is_none()).map(|(i, _)| i));
            } else {
                for id in &ids {
                    let matching: Vec<usize> = configs
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.id.as_deref().map(|cid| cid == id || matches_pattern(cid, id)).unwrap_or(false))
                        .map(|(i, _)| i)
                        .collect();
                    if matching.is_empty() {
                        return Err(dsl_error(format!(
                            "Cannot find routeConfiguration with id '{}' (referenced by route {})",
                            id,
                            r.id.clone().unwrap_or_else(|| r.input.clone())
                        )));
                    }
                    for i in matching {
                        if !selected.contains(&i) {
                            selected.push(i);
                        }
                    }
                }
            }
            if selected.is_empty() {
                continue;
            }
            let handle = match cache.get(&selected) {
                Some(h) => h.clone(),
                None => {
                    let mut merged = BuilderScope::default();
                    for &i in &selected {
                        let c = &configs[i];
                        let source = Source::synthetic(&c.value);
                        let label = c.id.clone().map(|id| format!("routeConfiguration[{}]", id)).unwrap_or_else(|| "routeConfiguration".into());
                        let mut s = self.build_route_configuration(ctx, &source, &c.value, vec![PathSegment::Key(label)])?;
                        if s.error_handler.is_some() {
                            merged.error_handler = s.error_handler.take();
                        }
                        merged.outputs.append(&mut s.outputs);
                    }
                    let h = BuilderScopeHandle(Arc::new(Mutex::new(merged)));
                    cache.insert(selected.clone(), h.clone());
                    h
                }
            };
            r.extensions.insert(BUILDER_SCOPE.to_string(), Box::new(handle));
        }
        Ok(())
    }
}
