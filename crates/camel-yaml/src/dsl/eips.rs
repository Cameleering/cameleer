//! Built-in step handlers for the Enterprise Integration Patterns (filter, choice,
//! split, aggregate, multicast, ..., doTry, circuitBreaker, throttle, ...).
//!
//! Property names follow the Camel 4 YAML DSL schema. Expressions may be given
//! inline (`split: {simple: "${body}", steps: [...]}`) or with the `expression` key
//! (`split: {expression: {simple: "${body}"}, steps: [...]}`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::builder::ExpressionDefinition;
use camel_core::message::Exchange;
use camel_core::model::CustomDefinition;
use camel_core::processor::basic::ToDefinition;
use camel_core::processor::errors::circuit_breaker::{CircuitBreakerConfig, CircuitBreakerDefinition};
use camel_core::processor::errors::do_try::DoTryDefinition;
use camel_core::processor::errors::handler::RedeliveryOverrides;
use camel_core::processor::errors::idempotent::{memory_idempotent_repository, IdempotentConsumerDefinition, IdempotentRepository, MemoryIdempotentRepository};
use camel_core::processor::errors::intercept::{InterceptDefinition, InterceptKind};
use camel_core::processor::errors::misc::{
    PolicyDefinition, RollbackDefinition, RollbackMode, ThreadsDefinition, ThrowExceptionDefinition, TransactedDefinition, ValidateDefinition,
};
use camel_core::processor::errors::on_completion::{OnCompletionDefinition, OnCompletionMode};
use camel_core::processor::errors::on_exception::OnExceptionDefinition;
use camel_core::processor::errors::throttle::{DelayDefinition, SampleDefinition, ThrottleDefinition};
use camel_core::processor::errors::BoolOrPredicate;
use camel_core::processor::routing::sort::Comparator;
use camel_core::processor::routing::strategies;
use camel_core::processor::routing::{
    AggregateDefinition, AggregationRepository, ChoiceDefinition, ClaimCheckDefinition, ClaimCheckOperation, DynamicRouterDefinition, EnrichDefinition,
    FailoverSettings, FilterDefinition, LoadBalanceDefinition, LoadBalancer, LoadBalancerPolicy, LoopDefinition, MulticastDefinition, MulticastSettings,
    PipelineDefinition, PollEnrichDefinition, RecipientListDefinition, ResequenceDefinition, ResequencerConfig, RoutingSlipDefinition, SortDefinition,
    SplitDefinition, StrategyDefinition, WireTapDefinition,
};
use camel_core::spi::Processor;
use camel_core::dataformat::DataFormatDefinition;
use camel_core::processor::transform::MarshalDefinition;
use camel_core::util::parse_duration;

use super::*;

// ---------------------------------------------------------------------------
// Value helpers (shared with the configuration loader)
// ---------------------------------------------------------------------------

/// Strip registry reference prefixes: `#bean:name`, `#class:a.b.C`, `#type:x`, `#name`, `bean:name`, `ref:name`.
pub fn ref_name(s: &str) -> String {
    let s = s.trim();
    for p in ["#bean:", "#class:", "#type:", "#", "bean:", "ref:"] {
        if let Some(r) = s.strip_prefix(p) {
            return r.trim().to_string();
        }
    }
    s.to_string()
}

/// Normalize a Java exception class name to a rust-camel error kind
/// (`java.io.IOException` -> `IOException`).
pub fn exception_kind(s: &str) -> String {
    let s = s.trim();
    s.rsplit('.').next().unwrap_or(s).to_string()
}

/// Scalar option as a string with property placeholders resolved.
pub(super) fn val_str(camel: &CamelContext, v: &Value) -> Result<String> {
    let s = scalar_string(v).ok_or_else(|| dsl_error(format!("expected a scalar value but found {}", v.type_name())))?;
    if s.contains("{{") {
        camel.resolve_placeholders(&s)
    } else {
        Ok(s)
    }
}

pub(super) fn val_i64(camel: &CamelContext, v: &Value) -> Result<i64> {
    match v {
        Value::Int(i) => Ok(*i),
        Value::Float(f) if f.fract() == 0.0 => Ok(*f as i64),
        _ => {
            let s = val_str(camel, v)?;
            s.trim().parse::<i64>().map_err(|_| dsl_error(format!("'{}' is not an integer", s)))
        }
    }
}

pub(super) fn val_f64(camel: &CamelContext, v: &Value) -> Result<f64> {
    match v {
        Value::Int(i) => Ok(*i as f64),
        Value::Float(f) => Ok(*f),
        _ => {
            let s = val_str(camel, v)?;
            s.trim().parse::<f64>().map_err(|_| dsl_error(format!("'{}' is not a number", s)))
        }
    }
}

/// A duration: a number is taken in `unit_millis` milliseconds (1 for millis, 1000
/// for seconds), a string may carry a unit (`5s`, `250ms`, `PT1M`).
pub(super) fn val_duration(camel: &CamelContext, v: &Value, unit_millis: u64) -> Result<Duration> {
    match v {
        Value::Int(i) if *i >= 0 => Ok(Duration::from_millis(*i as u64 * unit_millis)),
        Value::Float(f) if *f >= 0.0 => Ok(Duration::from_millis((*f * unit_millis as f64) as u64)),
        _ => {
            let s = val_str(camel, v)?;
            let t = s.trim();
            if let Ok(n) = t.parse::<u64>() {
                return Ok(Duration::from_millis(n * unit_millis));
            }
            parse_duration(t).map_err(|e| dsl_error(e.message))
        }
    }
}

pub(super) fn val_bool(camel: &CamelContext, v: &Value) -> Result<bool> {
    resolve_bool(camel, v)
}

/// A list option given as a YAML sequence or a comma separated string.
pub(super) fn string_list(v: Option<&Value>) -> Vec<String> {
    match v {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.iter().filter_map(scalar_string).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        Some(other) => scalar_string(other)
            .map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
            .unwrap_or_default(),
    }
}

/// Exception kinds of an `exception:` list.
pub(super) fn exception_list(v: Option<&Value>) -> Vec<String> {
    string_list(v).iter().map(|s| exception_kind(s)).collect()
}

/// An optional expression stored under `key` (an XML `<onWhen>` arrives as a one element list).
pub(super) fn opt_expression_value(v: Option<&Value>) -> Result<Option<ExpressionDefinition>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) if a.len() == 1 => parse_expression(&a[0]).map(Some),
        Some(v) => parse_expression(v).map(Some),
    }
}

/// `handled` / `continued`: a boolean, `{constant: true}` or a predicate.
pub(super) fn bool_or_predicate(camel: &CamelContext, v: Option<&Value>) -> Result<Option<BoolOrPredicate>> {
    let v = match v {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(a)) if a.len() == 1 => &a[0],
        Some(v) => v,
    };
    match v {
        Value::Bool(b) => return Ok(Some(BoolOrPredicate::Bool(*b))),
        Value::String(s) if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") => {
            return Ok(Some(BoolOrPredicate::Bool(s.eq_ignore_ascii_case("true"))))
        }
        _ => {}
    }
    let e = parse_expression(v)?;
    if let ExpressionDefinition::Language { language, expression, .. } = &e {
        if language == "constant" {
            let t = camel.resolve_placeholders(expression)?;
            if let Some(b) = Value::String(t.trim().to_string()).as_bool() {
                return Ok(Some(BoolOrPredicate::Bool(b)));
            }
        }
    }
    Ok(Some(BoolOrPredicate::Predicate(e)))
}

/// Look up a processor bound in the registry.
pub(super) fn lookup_processor(camel: &CamelContext, raw: &str) -> Result<Arc<dyn Processor>> {
    let n = ref_name(raw);
    camel
        .registry()
        .lookup_processor(&n)
        .ok_or_else(|| dsl_error(format!("No processor named '{}' found in the registry (bind it with Registry::bind_processor)", n)))
}

/// Resolve an `aggregationStrategy` reference: a strategy bound in the registry
/// (`Registry::bind_aggregation_strategy`) or one of Camel's built-in strategies by
/// class name (`GroupedBodyAggregationStrategy`, `UseLatestAggregationStrategy`, ...).
pub fn resolve_aggregation_strategy(camel: &CamelContext, raw: &str) -> Result<StrategyDefinition> {
    let n = ref_name(raw);
    if camel.registry().lookup_aggregation_strategy(&n).is_some() {
        return Ok(StrategyDefinition::Ref(n));
    }
    let simple = n.rsplit('.').next().unwrap_or(&n);
    let builtin = match simple {
        "GroupedBodyAggregationStrategy" | "groupedBody" => StrategyDefinition::instance(strategies::grouped_body()),
        "GroupedExchangeAggregationStrategy" | "GroupedMessageAggregationStrategy" | "groupedExchange" => {
            StrategyDefinition::instance(strategies::grouped_exchange())
        }
        "UseLatestAggregationStrategy" | "useLatest" => StrategyDefinition::instance(strategies::use_latest()),
        "UseOriginalAggregationStrategy" | "useOriginal" => StrategyDefinition::instance(strategies::use_original()),
        "StringAggregationStrategy" | "string" => StrategyDefinition::instance(strategies::string("")),
        _ => {
            return Err(dsl_error(format!(
                "No AggregationStrategy named '{}' found in the registry (bind it with Registry::bind_aggregation_strategy)",
                n
            )))
        }
    };
    Ok(builtin)
}

// ---------------------------------------------------------------------------
// StepContext option helpers
// ---------------------------------------------------------------------------

impl<'a> StepContext<'a> {
    fn opt_i64(&self, cfg: &Value, key: &str) -> Result<Option<i64>> {
        match cfg.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => val_i64(self.camel, v).map(Some).map_err(|e| self.error(format!("option '{}': {}", key, e.message))),
        }
    }

    fn opt_usize(&self, cfg: &Value, key: &str) -> Result<Option<usize>> {
        match self.opt_i64(cfg, key)? {
            Some(n) if n < 0 => Err(self.error(format!("option '{}' must not be negative", key))),
            other => Ok(other.map(|n| n as usize)),
        }
    }

    fn opt_f64(&self, cfg: &Value, key: &str) -> Result<Option<f64>> {
        match cfg.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => val_f64(self.camel, v).map(Some).map_err(|e| self.error(format!("option '{}': {}", key, e.message))),
        }
    }

    /// Duration option (plain numbers are millis).
    fn opt_duration(&self, cfg: &Value, key: &str) -> Result<Option<Duration>> {
        self.opt_duration_unit(cfg, key, 1)
    }

    fn opt_duration_unit(&self, cfg: &Value, key: &str, unit: u64) -> Result<Option<Duration>> {
        match cfg.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => val_duration(self.camel, v, unit).map(Some).map_err(|e| self.error(format!("option '{}': {}", key, e.message))),
        }
    }

    /// An optional nested expression (`completionPredicate: {simple: ...}`).
    fn opt_expression(&self, cfg: &Value, key: &str) -> Result<Option<ExpressionDefinition>> {
        opt_expression_value(cfg.get(key)).map_err(|e| self.error(format!("option '{}': {}", key, e.message)))
    }

    fn flag(&self, cfg: &Value, key: &str, default: bool) -> Result<bool> {
        Ok(self.opt_bool(cfg, key)?.unwrap_or(default))
    }

    fn strategy(&self, cfg: &Value) -> Result<Option<StrategyDefinition>> {
        let raw = ["aggregationStrategy", "aggregationStrategyRef", "strategyRef"].iter().find_map(|k| self.opt_str(cfg, k));
        match raw {
            None => Ok(None),
            Some(r) => resolve_aggregation_strategy(self.camel, &r).map(Some).map_err(|e| self.error(e.message)),
        }
    }

    fn multicast_settings(&self, cfg: &Value, s: &mut MulticastSettings) -> Result<()> {
        if let Some(b) = self.opt_bool(cfg, "parallelProcessing")? {
            s.parallel_processing = b;
        }
        if let Some(b) = self.opt_bool(cfg, "streaming")? {
            s.streaming = b;
        }
        if let Some(b) = self.opt_bool(cfg, "stopOnException")? {
            s.stop_on_exception = b;
        }
        if let Some(b) = self.opt_bool(cfg, "shareUnitOfWork")? {
            s.share_unit_of_work = b;
        }
        if let Some(d) = self.opt_duration(cfg, "timeout")? {
            if !d.is_zero() {
                s.timeout = Some(d);
            }
        }
        if let Some(st) = self.strategy(cfg)? {
            s.strategy = Some(st);
        }
        Ok(())
    }

    /// The innermost open block, downcast to `T` (used by multi-section EIPs).
    fn block<T: 'static>(&mut self, what: &str) -> Result<&mut T> {
        let err = self.error(format!("internal error: the {} block is not open", what));
        self.route.current_block_mut::<T>().ok_or(err)
    }

    /// Load `steps` given in a section mapping (`otherwise: {steps: [...]}`) or as the list itself.
    fn load_section(&mut self, section: &Value) -> Result<()> {
        match section {
            Value::Array(_) => self.load_steps(section),
            Value::Object(_) => match section.get("steps") {
                Some(s) => self.load_steps(s),
                None => Ok(()),
            },
            Value::Null => Ok(()),
            other => Err(self.error(format!("expected a mapping with 'steps' but found {}", other.type_name()))),
        }
    }
}

/// `{key: {...}}` wrapper around a list item (`- onException: {...}`, `- when: {...}`).
pub(super) fn unwrap_item<'v>(v: &'v Value, key: &str) -> &'v Value {
    match v {
        Value::Object(m) if m.len() == 1 && m.contains_key(key) => &m[key],
        other => other,
    }
}

/// Items of an option that may be a list or a single mapping.
fn items_of(v: Option<&Value>) -> Vec<&Value> {
    match v {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.iter().collect(),
        Some(other) => vec![other],
    }
}

fn uri_expression(uri: String) -> ExpressionDefinition {
    if uri.contains("${") {
        ExpressionDefinition::language("simple", uri)
    } else {
        ExpressionDefinition::language("constant", uri)
    }
}

/// Endpoint of enrich / pollEnrich: `uri` (+ `parameters`), a plain string, or an expression.
fn enrich_endpoint(ctx: &StepContext<'_>, cfg: &Value) -> Result<ExpressionDefinition> {
    match cfg {
        Value::String(s) => Ok(uri_expression(s.clone())),
        _ if cfg.get("uri").is_some() => Ok(uri_expression(ctx.uri(cfg)?)),
        _ => ctx.expression(cfg),
    }
}

/// Build an `onException` definition (without its outputs) from its YAML configuration.
pub(super) fn on_exception_definition(ctx: &StepContext<'_>, cfg: &Value) -> Result<OnExceptionDefinition> {
    let camel = ctx.camel;
    let wrap = |e: CamelError| ctx.error(e.message);
    let exceptions = exception_list(cfg.get("exception"));
    let refs: Vec<&str> = exceptions.iter().map(|s| s.as_str()).collect();
    let mut d = OnExceptionDefinition::new(&refs);
    d.handled = bool_or_predicate(camel, cfg.get("handled")).map_err(wrap)?;
    d.continued = bool_or_predicate(camel, cfg.get("continued")).map_err(wrap)?;
    d.on_when = ctx.opt_expression(cfg, "onWhen")?;
    d.retry_while = ctx.opt_expression(cfg, "retryWhile")?;
    if let Some(rp) = cfg.get("redeliveryPolicy") {
        d.policy = super::config::parse_redelivery_overrides(camel, unwrap_item(rp, "redeliveryPolicy")).map_err(wrap)?.0;
    }
    if let Some(r) = ctx.opt_str(cfg, "redeliveryPolicyRef") {
        let n = ref_name(&r);
        let p = camel
            .registry()
            .lookup::<camel_core::processor::errors::RedeliveryPolicy>(&n)
            .ok_or_else(|| ctx.error(format!("No RedeliveryPolicy named '{}' found in the registry", n)))?;
        d.policy = RedeliveryOverrides {
            maximum_redeliveries: Some(p.maximum_redeliveries),
            redelivery_delay: Some(p.redelivery_delay),
            back_off_multiplier: Some(p.back_off_multiplier),
            use_exponential_back_off: Some(p.use_exponential_back_off),
            maximum_redelivery_delay: Some(p.maximum_redelivery_delay),
            use_collision_avoidance: Some(p.use_collision_avoidance),
            retry_attempted_log_level: Some(p.retry_attempted_log_level),
            retries_exhausted_log_level: Some(p.retries_exhausted_log_level),
            log_stack_trace: Some(p.log_stack_trace),
            log_exhausted: Some(p.log_exhausted),
            delay_pattern: p.delay_pattern.clone(),
            async_delayed_redelivery: Some(p.async_delayed_redelivery),
        };
    }
    d.use_original_message = ctx.flag(cfg, "useOriginalMessage", false)?;
    d.use_original_body = ctx.flag(cfg, "useOriginalBody", false)?;
    if let Some(r) = ctx.opt_str(cfg, "onRedeliveryRef").or_else(|| ctx.opt_str(cfg, "onRedelivery")) {
        d.on_redelivery = Some(lookup_processor(camel, &r).map_err(wrap)?);
    }
    if let Some(r) = ctx.opt_str(cfg, "onExceptionOccurredRef").or_else(|| ctx.opt_str(cfg, "onExceptionOccurred")) {
        d.on_exception_occurred = Some(lookup_processor(camel, &r).map_err(wrap)?);
    }
    Ok(d)
}

fn on_completion_definition(ctx: &StepContext<'_>, cfg: &Value) -> Result<OnCompletionDefinition> {
    let mut d = OnCompletionDefinition::default();
    let complete = ctx.flag(cfg, "onCompleteOnly", false)?;
    let failure = ctx.flag(cfg, "onFailureOnly", false)?;
    if complete && failure {
        return Err(ctx.error("onCompleteOnly and onFailureOnly cannot both be true"));
    }
    if complete {
        d.mode = OnCompletionMode::OnCompleteOnly;
    } else if failure {
        d.mode = OnCompletionMode::OnFailureOnly;
    }
    if let Some(m) = ctx.opt_str(cfg, "mode") {
        d.before_consumer = match m.as_str() {
            "BeforeConsumer" | "beforeConsumer" => true,
            "AfterConsumer" | "afterConsumer" => false,
            other => return Err(ctx.error(format!("invalid mode '{}' (AfterConsumer, BeforeConsumer)", other))),
        };
    }
    d.on_when = ctx.opt_expression(cfg, "onWhen")?;
    Ok(d)
}

fn resilience_config(ctx: &StepContext<'_>, cfg: &Value) -> Result<CircuitBreakerConfig> {
    let mut c = CircuitBreakerConfig::default();
    if let Some(r) = ctx.opt_str(cfg, "configuration").or_else(|| ctx.opt_str(cfg, "configurationRef")) {
        let n = ref_name(&r);
        c = ctx
            .camel
            .registry()
            .lookup::<CircuitBreakerConfig>(&n)
            .map(|c| (*c).clone())
            .ok_or_else(|| ctx.error(format!("No CircuitBreakerConfig named '{}' found in the registry", n)))?;
    }
    if let Some(r4j) = cfg.get("resilience4jConfiguration") {
        if let Some(v) = ctx.opt_f64(r4j, "failureRateThreshold")? {
            c.failure_rate_threshold = v;
        }
        if let Some(v) = ctx.opt_usize(r4j, "minimumNumberOfCalls")? {
            c.minimum_number_of_calls = v;
        }
        if let Some(v) = ctx.opt_usize(r4j, "slidingWindowSize")? {
            c.sliding_window_size = v;
        }
        // seconds in Camel's resilience4j configuration
        if let Some(v) = ctx.opt_duration_unit(r4j, "waitDurationInOpenState", 1000)? {
            c.wait_duration_in_open_state = v;
        }
        if let Some(v) = ctx.opt_usize(r4j, "permittedNumberOfCallsInHalfOpenState")? {
            c.permitted_number_of_calls_in_half_open_state = v;
        }
        if let Some(v) = ctx.opt_bool(r4j, "timeoutEnabled")? {
            c.timeout_enabled = v;
        }
        if let Some(v) = ctx.opt_duration(r4j, "timeoutDuration")? {
            c.timeout_duration = v;
        }
    }
    if let Some(ft) = cfg.get("faultToleranceConfiguration") {
        if let Some(v) = ctx.opt_usize(ft, "requestVolumeThreshold")? {
            c.sliding_window_size = v;
            c.minimum_number_of_calls = v;
        }
        if let Some(v) = ctx.opt_f64(ft, "failureRatio")? {
            c.failure_rate_threshold = v;
        }
        if let Some(v) = ctx.opt_duration(ft, "delay")? {
            c.wait_duration_in_open_state = v;
        }
        if let Some(v) = ctx.opt_usize(ft, "successThreshold")? {
            c.permitted_number_of_calls_in_half_open_state = v;
        }
        if let Some(v) = ctx.opt_bool(ft, "timeoutEnabled")? {
            c.timeout_enabled = v;
        }
        if let Some(v) = ctx.opt_duration(ft, "timeoutDuration")? {
            c.timeout_duration = v;
        }
    }
    Ok(c)
}

fn load_balancer_policy(ctx: &StepContext<'_>, cfg: &Value) -> Result<LoadBalancerPolicy> {
    let mut policy: Option<LoadBalancerPolicy> = None;
    let map = match cfg {
        Value::Object(m) => m.clone(),
        Value::Null => Default::default(),
        other => return Err(ctx.error(format!("expected a mapping but found {}", other.type_name()))),
    };
    for (k, v) in &map {
        let name = k.strip_suffix("LoadBalancer").unwrap_or(k);
        let p = match name {
            "roundRobin" => LoadBalancerPolicy::RoundRobin,
            "random" => LoadBalancerPolicy::Random,
            "topic" => LoadBalancerPolicy::Topic,
            "sticky" => {
                let e = match v.get("correlationExpression") {
                    Some(c) => parse_expression(c).map_err(|e| ctx.error(format!("sticky correlationExpression: {}", e.message)))?,
                    None => ctx.expression(v).map_err(|_| ctx.error("sticky load balancer requires a 'correlationExpression'"))?,
                };
                LoadBalancerPolicy::Sticky(e)
            }
            "weighted" => {
                let delim = ctx.opt_str(v, "distributionRatioDelimiter").unwrap_or_else(|| ",".into());
                let ratios: Vec<usize> = match v.get("distributionRatio") {
                    Some(Value::Array(a)) => a.iter().map(|x| val_i64(ctx.camel, x).map(|n| n.max(0) as usize)).collect::<Result<_>>().map_err(|e| ctx.error(e.message))?,
                    Some(x) => {
                        let s = val_str(ctx.camel, x).map_err(|e| ctx.error(e.message))?;
                        s.split(delim.as_str())
                            .map(|p| p.trim().parse::<usize>().map_err(|_| ctx.error(format!("invalid distributionRatio '{}'", s))))
                            .collect::<Result<_>>()?
                    }
                    None => return Err(ctx.error("weighted load balancer requires 'distributionRatio'")),
                };
                LoadBalancerPolicy::Weighted { round_robin: ctx.flag(v, "roundRobin", false)?, ratios }
            }
            "failover" => {
                let mut s = FailoverSettings { exceptions: exception_list(v.get("exception")), ..Default::default() };
                if let Some(n) = ctx.opt_i64(v, "maximumFailoverAttempts")? {
                    s.maximum_failover_attempts = n as i32;
                }
                s.round_robin = ctx.flag(v, "roundRobin", false)?;
                s.sticky = ctx.flag(v, "sticky", false)?;
                LoadBalancerPolicy::Failover(s)
            }
            "custom" => {
                let r = match v {
                    Value::String(s) => s.clone(),
                    _ => ctx.required_str(v, &["ref"])?,
                };
                let n = ref_name(&r);
                let lb = ctx
                    .camel
                    .registry()
                    .lookup::<Arc<dyn LoadBalancer>>(&n)
                    .map(|l| (*l).clone())
                    .ok_or_else(|| ctx.error(format!("No LoadBalancer named '{}' found in the registry", n)))?;
                LoadBalancerPolicy::Custom(lb)
            }
            "steps" | "id" | "description" | "disabled" | "inheritErrorHandler" => continue,
            other => return Err(ctx.error(format!("unknown load balancer '{}'", other))),
        };
        if policy.is_some() {
            return Err(ctx.error("only one load balancer policy may be configured"));
        }
        policy = Some(p);
    }
    Ok(policy.unwrap_or(LoadBalancerPolicy::RoundRobin))
}

fn idempotent_repository(ctx: &StepContext<'_>, cfg: &Value) -> Result<Arc<dyn IdempotentRepository>> {
    let raw = match cfg.get("idempotentRepository").or_else(|| cfg.get("idempotentRepositoryRef")) {
        None | Some(Value::Null) => return Ok(memory_idempotent_repository(1000)),
        Some(v) => scalar_string(v).ok_or_else(|| ctx.error("'idempotentRepository' must be a bean reference"))?,
    };
    let n = ref_name(&raw);
    let reg = ctx.camel.registry();
    if let Some(r) = reg.lookup::<Arc<dyn IdempotentRepository>>(&n) {
        return Ok((*r).clone());
    }
    if let Some(r) = reg.lookup::<Arc<MemoryIdempotentRepository>>(&n) {
        return Ok((*r).clone());
    }
    if let Some(r) = reg.lookup::<MemoryIdempotentRepository>(&n) {
        return Ok(r);
    }
    Err(ctx.error(format!("No IdempotentRepository named '{}' found in the registry", n)))
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the EIP step handlers.
pub(super) fn register_eip_steps(r: &mut StepRegistry) {
    // --- routing ---
    r.register("filter", |ctx, cfg| {
        let p = ctx.expression(cfg)?;
        ctx.add_block(Box::new(FilterDefinition::new(p)), cfg.get("steps"))
    });
    r.register("choice", |ctx, cfg| {
        let mut def = ChoiceDefinition::new();
        def.precondition = ctx.flag(cfg, "precondition", false)?;
        ctx.push_block(Box::new(def));
        let r = load_choice(ctx, cfg);
        ctx.end();
        r
    });
    r.register("pipeline", |ctx, cfg| ctx.add_block(Box::new(PipelineDefinition::default()), cfg.get("steps")));
    r.register("split", |ctx, cfg| {
        let e = ctx.expression(cfg)?;
        let mut d = SplitDefinition::new(e);
        ctx.multicast_settings(cfg, &mut d.settings)?;
        if let Some(delim) = ctx.opt_str(cfg, "delimiter") {
            d.delimiter = if delim == "false" { None } else { Some(delim) };
        }
        ctx.add_block(Box::new(d), cfg.get("steps"))
    });
    r.register("multicast", |ctx, cfg| {
        let mut d = MulticastDefinition::new();
        ctx.multicast_settings(cfg, &mut d.settings)?;
        ctx.add_block(Box::new(d), cfg.get("steps"))
    });
    r.register("recipientList", |ctx, cfg| {
        let mut d = RecipientListDefinition::new(ctx.expression(cfg)?);
        if let Some(delim) = ctx.opt_str(cfg, "delimiter") {
            d.delimiter = delim;
        }
        d.ignore_invalid_endpoints = ctx.flag(cfg, "ignoreInvalidEndpoints", false)?;
        ctx.multicast_settings(cfg, &mut d.settings)?;
        ctx.add(Box::new(d));
        Ok(())
    });
    r.register("routingSlip", |ctx, cfg| {
        let mut d = RoutingSlipDefinition::new(ctx.expression(cfg)?);
        if let Some(delim) = ctx.opt_str(cfg, "uriDelimiter") {
            d.uri_delimiter = delim;
        }
        d.ignore_invalid_endpoints = ctx.flag(cfg, "ignoreInvalidEndpoints", false)?;
        ctx.add(Box::new(d));
        Ok(())
    });
    r.register("dynamicRouter", |ctx, cfg| {
        let mut d = DynamicRouterDefinition::new(ctx.expression(cfg)?);
        if let Some(delim) = ctx.opt_str(cfg, "uriDelimiter") {
            d.uri_delimiter = delim;
        }
        d.ignore_invalid_endpoints = ctx.flag(cfg, "ignoreInvalidEndpoints", false)?;
        ctx.add(Box::new(d));
        Ok(())
    });
    r.register("aggregate", |ctx, cfg| {
        let correlation = match cfg.get("correlationExpression") {
            Some(c) => parse_expression(unwrap_item(c, "expression")).map_err(|e| ctx.error(format!("correlationExpression: {}", e.message)))?,
            None => ctx.expression(cfg).map_err(|_| ctx.error("missing required option 'correlationExpression'"))?,
        };
        let strategy = ctx.strategy(cfg)?.ok_or_else(|| ctx.error("missing required option 'aggregationStrategy'"))?;
        let mut d = AggregateDefinition::new(correlation, Some(strategy));
        d.completion_size = ctx.opt_usize(cfg, "completionSize")?;
        d.completion_size_expression = ctx.opt_expression(cfg, "completionSizeExpression")?;
        d.completion_predicate = ctx.opt_expression(cfg, "completionPredicate")?;
        d.completion_timeout = ctx.opt_duration(cfg, "completionTimeout")?.filter(|d| !d.is_zero());
        d.completion_timeout_expression = ctx.opt_expression(cfg, "completionTimeoutExpression")?;
        d.completion_interval = ctx.opt_duration(cfg, "completionInterval")?.filter(|d| !d.is_zero());
        d.completion_from_batch_consumer = ctx.flag(cfg, "completionFromBatchConsumer", false)?;
        d.completion_on_new_correlation_group = ctx.flag(cfg, "completionOnNewCorrelationGroup", false)?;
        d.eager_check_completion = ctx.flag(cfg, "eagerCheckCompletion", false)?;
        d.force_completion_on_stop = ctx.flag(cfg, "forceCompletionOnStop", false)?;
        d.discard_on_completion_timeout = ctx.flag(cfg, "discardOnCompletionTimeout", false)?;
        d.ignore_invalid_correlation_keys = ctx.flag(cfg, "ignoreInvalidCorrelationKeys", false)?;
        d.close_correlation_key_on_completion = ctx.opt_usize(cfg, "closeCorrelationKeyOnCompletion")?;
        if let Some(r) = ctx.opt_str(cfg, "aggregationRepository").or_else(|| ctx.opt_str(cfg, "aggregationRepositoryRef")) {
            let n = ref_name(&r);
            let repo = ctx
                .camel
                .registry()
                .lookup::<Arc<dyn AggregationRepository>>(&n)
                .map(|r| (*r).clone())
                .ok_or_else(|| ctx.error(format!("No AggregationRepository named '{}' found in the registry", n)))?;
            d.repository = Some(repo);
        }
        ctx.add_block(Box::new(d), cfg.get("steps"))
    });
    r.register("wireTap", |ctx, cfg| {
        let uri = ctx.uri(cfg)?;
        let mut d = WireTapDefinition::new(uri);
        d.copy = ctx.flag(cfg, "copy", true)?;
        d.ignore_invalid_endpoint = ctx.flag(cfg, "ignoreInvalidEndpoint", false)?;
        for key in ["body", "newExchangeBody", "newExchangeExpression"] {
            if let Some(e) = ctx.opt_expression(cfg, key)? {
                d.new_exchange_body = Some(e);
            }
        }
        for key in ["setHeader", "setHeaders", "headers"] {
            for h in items_of(cfg.get(key)) {
                let h = unwrap_item(h, "setHeader");
                let name = ctx.required_str(h, &["name"])?;
                d.new_exchange_headers.push((name, ctx.expression(h)?));
            }
        }
        if let Some(r) = ctx.opt_str(cfg, "onPrepare").or_else(|| ctx.opt_str(cfg, "onPrepareRef")) {
            d.on_prepare = Some(lookup_processor(ctx.camel, &r).map_err(|e| ctx.error(e.message))?);
        }
        ctx.add(Box::new(d));
        Ok(())
    });
    r.register("enrich", |ctx, cfg| {
        let mut d = EnrichDefinition::new(enrich_endpoint(ctx, cfg)?);
        d.strategy = ctx.strategy(cfg)?;
        d.aggregate_on_exception = ctx.flag(cfg, "aggregateOnException", false)?;
        d.ignore_invalid_endpoint = ctx.flag(cfg, "ignoreInvalidEndpoint", false)?;
        d.share_unit_of_work = ctx.flag(cfg, "shareUnitOfWork", false)?;
        ctx.add(Box::new(d));
        Ok(())
    });
    r.register("pollEnrich", |ctx, cfg| {
        let timeout = match cfg.get("timeout") {
            None | Some(Value::Null) => -1,
            Some(Value::Int(n)) => *n,
            Some(v) => {
                let s = val_str(ctx.camel, v).map_err(|e| ctx.error(e.message))?;
                match s.trim().parse::<i64>() {
                    Ok(n) => n,
                    Err(_) => parse_duration(&s).map_err(|e| ctx.error(e.message))?.as_millis() as i64,
                }
            }
        };
        let mut d = PollEnrichDefinition::new(enrich_endpoint(ctx, cfg)?, timeout);
        d.strategy = ctx.strategy(cfg)?;
        d.aggregate_on_exception = ctx.flag(cfg, "aggregateOnException", false)?;
        d.ignore_invalid_endpoint = ctx.flag(cfg, "ignoreInvalidEndpoint", false)?;
        ctx.add(Box::new(d));
        Ok(())
    });
    r.register("loop", |ctx, cfg| {
        let e = ctx.expression(cfg)?;
        let mut d = if ctx.flag(cfg, "doWhile", false)? { LoopDefinition::do_while(e) } else { LoopDefinition::count(e) };
        d.copy = ctx.flag(cfg, "copy", false)?;
        ctx.add_block(Box::new(d), cfg.get("steps"))
    });
    r.register("loadBalance", |ctx, cfg| {
        let policy = load_balancer_policy(ctx, cfg)?;
        ctx.add_block(Box::new(LoadBalanceDefinition { id: None, policy, outputs: Default::default() }), cfg.get("steps"))
    });
    r.register("resequence", |ctx, cfg| {
        let mut d = ResequenceDefinition::new(ctx.expression(cfg)?);
        if let Some(b) = cfg.get("batchConfig").or_else(|| cfg.get("batch-config")) {
            let mut c = ResequencerConfig::batch();
            if let ResequencerConfig::Batch { batch_size, batch_timeout, allow_duplicates, reverse, ignore_invalid_exchanges } = &mut c {
                if let Some(n) = ctx.opt_usize(b, "batchSize")? {
                    *batch_size = n.max(1);
                }
                if let Some(t) = ctx.opt_duration(b, "batchTimeout")? {
                    *batch_timeout = t;
                }
                *allow_duplicates = ctx.flag(b, "allowDuplicates", false)?;
                *reverse = ctx.flag(b, "reverse", false)?;
                *ignore_invalid_exchanges = ctx.flag(b, "ignoreInvalidExchanges", false)?;
            }
            d.config = c;
        } else if let Some(s) = cfg.get("streamConfig").or_else(|| cfg.get("stream-config")) {
            let mut c = ResequencerConfig::stream();
            if let ResequencerConfig::Stream { capacity, timeout, reject_old, ignore_invalid_exchanges } = &mut c {
                if let Some(n) = ctx.opt_usize(s, "capacity")? {
                    *capacity = n.max(1);
                }
                if let Some(t) = ctx.opt_duration(s, "timeout")? {
                    *timeout = t;
                }
                *reject_old = ctx.flag(s, "rejectOld", false)?;
                *ignore_invalid_exchanges = ctx.flag(s, "ignoreInvalidExchanges", false)?;
            }
            d.config = c;
        }
        ctx.add_block(Box::new(d), cfg.get("steps"))
    });
    r.register("sort", |ctx, cfg| {
        let expression = ctx.expression(cfg)?;
        let comparator = match ctx.opt_str(cfg, "comparator").or_else(|| ctx.opt_str(cfg, "comparatorRef")) {
            None => None,
            Some(r) => {
                let n = ref_name(&r);
                let c = ctx
                    .camel
                    .registry()
                    .lookup::<Comparator>(&n)
                    .map(|c| (*c).clone())
                    .ok_or_else(|| ctx.error(format!("No comparator named '{}' found in the registry", n)))?;
                Some(c)
            }
        };
        ctx.add(Box::new(SortDefinition { id: None, expression, comparator }));
        Ok(())
    });
    r.register("claimCheck", |ctx, cfg| {
        let op = ctx.required_str(cfg, &["operation"])?;
        let operation = match op.to_ascii_lowercase().as_str() {
            "set" => ClaimCheckOperation::Set,
            "get" => ClaimCheckOperation::Get,
            "getandremove" => ClaimCheckOperation::GetAndRemove,
            "push" => ClaimCheckOperation::Push,
            "pop" => ClaimCheckOperation::Pop,
            other => return Err(ctx.error(format!("invalid claimCheck operation '{}' (Set, Get, GetAndRemove, Push, Pop)", other))),
        };
        ctx.add(Box::new(ClaimCheckDefinition { id: None, operation, key: ctx.opt_str(cfg, "key"), filter: ctx.opt_str(cfg, "filter") }));
        Ok(())
    });

    // --- error handling ---
    r.register("doTry", |ctx, cfg| {
        ctx.push_block(Box::new(DoTryDefinition::default()));
        let r = load_do_try(ctx, cfg);
        ctx.end();
        r
    });
    r.register("onException", |ctx, cfg| {
        let d = on_exception_definition(ctx, cfg)?;
        ctx.add_block(Box::new(d), cfg.get("steps"))
    });
    r.register("onCompletion", |ctx, cfg| {
        let d = on_completion_definition(ctx, cfg)?;
        ctx.add_block(Box::new(d), cfg.get("steps"))
    });
    r.register("intercept", |ctx, cfg| {
        let mut d = InterceptDefinition::new(InterceptKind::All);
        d.on_when = ctx.opt_expression(cfg, "onWhen")?;
        ctx.add_block(Box::new(d), cfg.get("steps"))
    });
    r.register("interceptFrom", |ctx, cfg| {
        let uri = match cfg {
            Value::String(s) => s.clone(),
            _ => ctx.opt_str(cfg, "uri").unwrap_or_else(|| "*".into()),
        };
        let mut d = InterceptDefinition::new(InterceptKind::From(uri));
        d.on_when = ctx.opt_expression(cfg, "onWhen")?;
        ctx.add_block(Box::new(d), cfg.get("steps"))
    });
    r.register("interceptSendToEndpoint", |ctx, cfg| {
        let pattern = match cfg {
            Value::String(s) => s.clone(),
            _ => ctx.required_str(cfg, &["uri"])?,
        };
        let skip = ctx.flag(cfg, "skipSendToOriginalEndpoint", false)?;
        let mut d = InterceptDefinition::new(InterceptKind::SendToEndpoint { pattern, skip_send_to_original_endpoint: skip });
        d.on_when = ctx.opt_expression(cfg, "onWhen")?;
        if let Some(after) = ctx.opt_str(cfg, "afterUri") {
            camel_warn!("camel-yaml", "interceptSendToEndpoint afterUri '{}' is not supported and is ignored ({})", after, ctx.location());
        }
        ctx.add_block(Box::new(d), cfg.get("steps"))
    });
    r.register("circuitBreaker", |ctx, cfg| {
        let config = resilience_config(ctx, cfg)?;
        ctx.push_block(Box::new(CircuitBreakerDefinition { config, ..Default::default() }));
        let r = (|| -> Result<()> {
            if let Some(s) = cfg.get("steps") {
                ctx.load_steps(s)?;
            }
            if let Some(fb) = cfg.get("onFallback") {
                let cb = ctx.block::<CircuitBreakerDefinition>("circuitBreaker")?;
                cb.in_fallback = true;
                cb.fallback.get_or_insert_with(Default::default);
                ctx.load_section(fb)?;
            }
            Ok(())
        })();
        ctx.end();
        r
    });
    r.register("throttle", |ctx, cfg| {
        let max = throttle_maximum(ctx, cfg)?;
        let period = ctx.opt_duration(cfg, "timePeriodMillis")?.unwrap_or(Duration::from_millis(1000));
        let d = ThrottleDefinition::new(max, period);
        {
            let mut c = d.config.lock().unwrap();
            c.reject_execution = ctx.flag(cfg, "rejectExecution", false)?;
            c.async_delayed = ctx.flag(cfg, "asyncDelayed", false)?;
            c.correlation = ctx.opt_expression(cfg, "correlationExpression")?;
        }
        ctx.add(Box::new(d));
        Ok(())
    });
    r.register("delay", |ctx, cfg| {
        let expression = ctx.expression(cfg)?;
        let async_delayed = Arc::new(Mutex::new(ctx.flag(cfg, "asyncDelayed", true)?));
        ctx.add(Box::new(DelayDefinition { id: None, expression, async_delayed }));
        Ok(())
    });
    r.register("sample", |ctx, cfg| {
        let (period, freq) = match cfg {
            Value::Object(_) => (ctx.opt_duration(cfg, "samplePeriod")?, ctx.opt_i64(cfg, "messageFrequency")?),
            Value::Null => (None, None),
            other => (Some(val_duration(ctx.camel, other, 1).map_err(|e| ctx.error(e.message))?), None),
        };
        let def = match freq {
            Some(n) if n > 0 => SampleDefinition { id: None, period: None, message_frequency: Some(n as u64) },
            _ => SampleDefinition { id: None, period: Some(period.unwrap_or(Duration::from_secs(1))), message_frequency: None },
        };
        ctx.add(Box::new(def));
        Ok(())
    });
    r.register("idempotentConsumer", |ctx, cfg| {
        let e = ctx.expression(cfg)?;
        let mut d = IdempotentConsumerDefinition::new(e, idempotent_repository(ctx, cfg)?);
        d.eager = ctx.flag(cfg, "eager", true)?;
        d.skip_duplicate = ctx.flag(cfg, "skipDuplicate", true)?;
        d.remove_on_failure = ctx.flag(cfg, "removeOnFailure", true)?;
        d.completion_eager = ctx.flag(cfg, "completionEager", false)?;
        ctx.add_block(Box::new(d), cfg.get("steps"))
    });
    r.register("validate", |ctx, cfg| {
        let predicate = ctx.expression(cfg)?;
        ctx.add(Box::new(ValidateDefinition { id: None, predicate }));
        Ok(())
    });
    r.register("rollback", |ctx, cfg| {
        let (mode, message) = match cfg {
            Value::String(s) => (RollbackMode::Throw, Some(s.clone())),
            _ if ctx.flag(cfg, "markRollbackOnlyLast", false)? => (RollbackMode::MarkRollbackOnlyLast, None),
            _ if ctx.flag(cfg, "markRollbackOnly", false)? => (RollbackMode::MarkRollbackOnly, None),
            _ => (RollbackMode::Throw, ctx.opt_str(cfg, "message")),
        };
        ctx.add(Box::new(RollbackDefinition { id: None, message, mode }));
        Ok(())
    });
    r.register("throwException", |ctx, cfg| {
        let kind = ctx
            .opt_str(cfg, "exceptionType")
            .map(|t| exception_kind(&t))
            .ok_or_else(|| ctx.error("missing required option 'exceptionType'"))?;
        let message = ctx.opt_str(cfg, "message").unwrap_or_default();
        ctx.add(Box::new(ThrowExceptionDefinition { id: None, kind, message }));
        Ok(())
    });
    r.register("threads", |ctx, cfg| {
        let pool_size = ctx.opt_usize(cfg, "poolSize")?.or(ctx.opt_usize(cfg, "maxPoolSize")?).unwrap_or(10).max(1);
        let thread_name = ctx.opt_str(cfg, "threadName").unwrap_or_else(|| "Threads".into());
        let def = Box::new(ThreadsDefinition { id: None, pool_size, thread_name, outputs: Default::default() });
        match cfg.get("steps") {
            Some(s) => ctx.add_block(def, Some(s)),
            None => {
                // like Camel: the remaining steps of the enclosing list run on the thread pool
                ctx.push_open_block(def);
                Ok(())
            }
        }
    });
    r.register("transacted", |ctx, cfg| {
        let reference = match cfg {
            Value::String(s) => Some(ref_name(s)),
            _ => ctx.opt_str(cfg, "ref").map(|r| ref_name(&r)),
        };
        ctx.add(Box::new(TransactedDefinition { id: None, reference }));
        Ok(())
    });
    r.register("policy", |ctx, cfg| {
        let reference = match cfg {
            Value::String(s) => ref_name(s),
            _ => ref_name(&ctx.required_str(cfg, &["ref"])?),
        };
        let def = Box::new(PolicyDefinition { id: None, policy: None, reference: Some(reference), outputs: Default::default() });
        match cfg.get("steps") {
            Some(s) => ctx.add_block(def, Some(s)),
            None => {
                ctx.push_open_block(def);
                Ok(())
            }
        }
    });

    // --- misc ---
    r.register("bean", |ctx, cfg| {
        let (scheme, name) = match cfg {
            Value::String(s) => ("bean", ref_name(s)),
            _ => match ctx.opt_str(cfg, "ref") {
                Some(r) => ("bean", ref_name(&r)),
                None => match ctx.opt_str(cfg, "beanType").or_else(|| ctx.opt_str(cfg, "type")) {
                    Some(t) => ("class", ref_name(&t)),
                    None => return Err(ctx.error("missing required option 'ref' (or 'beanType')")),
                },
            },
        };
        let mut params = std::collections::BTreeMap::new();
        if let Some(m) = ctx.opt_str(cfg, "method") {
            params.insert("method".to_string(), Value::String(m));
        }
        if let Some(s) = ctx.opt_str(cfg, "scope") {
            params.insert("scope".to_string(), Value::String(s));
        }
        let uri = build_uri(&format!("{}:{}", scheme, name), Some(&Value::Object(params))).map_err(|e| ctx.error(e.message))?;
        ctx.add(Box::new(ToDefinition::new(uri)));
        Ok(())
    });
    for (step, marshal) in [("marshal", true), ("unmarshal", false)] {
        r.register(step, move |ctx, cfg| {
            let df = data_format_definition(ctx, cfg)?;
            ctx.add(Box::new(if marshal { MarshalDefinition::marshal(df) } else { MarshalDefinition::unmarshal(df) }));
            Ok(())
        });
    }
    r.register("script", |ctx, cfg| {
        let expression = ctx.expression(cfg)?;
        ctx.add(Box::new(CustomDefinition {
            id: None,
            name: "script".into(),
            factory: Box::new(move |rc| {
                // script languages (camel-script) run as processors so their exchange changes are kept
                if let ExpressionDefinition::Language { language, expression: text, options } = &expression {
                    type ScriptFactory = Box<dyn Fn(&str, &str, &std::collections::BTreeMap<String, String>) -> Option<Result<Arc<dyn Processor>>> + Send + Sync>;
                    if let Some(f) = rc.context.registry().lookup::<ScriptFactory>("camelScriptProcessorFactory") {
                        if let Some(p) = f(language, &rc.context.resolve_placeholders(text)?, options) {
                            return p;
                        }
                    }
                }
                let e = expression.resolve_expression(&rc.context)?;
                Ok(Arc::new(move |ex: &mut Exchange| {
                    e.evaluate(ex)?;
                    Ok(())
                }) as Arc<dyn Processor>)
            }),
        }));
        Ok(())
    });
}

fn throttle_maximum(ctx: &StepContext<'_>, cfg: &Value) -> Result<usize> {
    let raw = match cfg.get("maximumRequests") {
        Some(v) => v.clone(),
        None => match ctx.expression(cfg)? {
            ExpressionDefinition::Language { language, expression, .. } if language == "constant" || language == "simple" => Value::String(expression),
            other => return Err(ctx.error(format!("throttle supports only a constant number of maximum requests, not {:?}", other))),
        },
    };
    let n = val_i64(ctx.camel, &raw).map_err(|e| ctx.error(format!("throttle maximum requests: {}", e.message)))?;
    if n <= 0 {
        return Err(ctx.error("throttle maximum requests must be > 0"));
    }
    Ok(n as usize)
}

fn load_choice(ctx: &mut StepContext<'_>, cfg: &Value) -> Result<()> {
    let whens = items_of(cfg.get("when"));
    if whens.is_empty() {
        return Err(ctx.error("choice requires at least one 'when'"));
    }
    for w in whens {
        let w = unwrap_item(w, "when");
        let pred = ctx.expression(w)?;
        let id = w.get("id").and_then(scalar_string);
        let err = ctx.error("invalid when");
        let c = ctx.block::<ChoiceDefinition>("choice")?;
        c.add_when(pred).map_err(|e| err.with_cause(e))?;
        if let Some(id) = id {
            c.whens.last_mut().unwrap().id = Some(id);
        }
        if let Some(s) = w.get("steps") {
            ctx.load_steps(s)?;
        }
    }
    if let Some(o) = cfg.get("otherwise") {
        let id = o.get("id").and_then(scalar_string);
        let c = ctx.block::<ChoiceDefinition>("choice")?;
        c.add_otherwise().map_err(|e| dsl_error(e.message))?;
        if let (Some(id), Some(ow)) = (id, c.otherwise.as_mut()) {
            ow.id = Some(id);
        }
        ctx.load_section(o)?;
    }
    Ok(())
}

fn load_do_try(ctx: &mut StepContext<'_>, cfg: &Value) -> Result<()> {
    if let Some(s) = cfg.get("steps") {
        ctx.load_steps(s)?;
    }
    for c in items_of(cfg.get("doCatch")) {
        let c = unwrap_item(c, "doCatch");
        let exceptions = exception_list(c.get("exception"));
        let on_when = ctx.opt_expression(c, "onWhen")?;
        let refs: Vec<&str> = exceptions.iter().map(|s| s.as_str()).collect();
        let t = ctx.block::<DoTryDefinition>("doTry")?;
        t.add_catch(&refs);
        if let Some(p) = on_when {
            t.set_on_when(p);
        }
        if let Some(s) = c.get("steps") {
            ctx.load_steps(s)?;
        }
    }
    if let Some(f) = cfg.get("doFinally") {
        ctx.block::<DoTryDefinition>("doTry")?.start_finally();
        ctx.load_section(f)?;
    }
    Ok(())
}

/// Parse a YAML data format reference: `{json: {prettyPrint: true}}`, `{csv: {delimiter: ";"}}`,
/// `{custom: {ref: myFormat}}`, or a plain string name (`json`).
fn data_format_definition(ctx: &StepContext<'_>, cfg: &Value) -> Result<DataFormatDefinition> {
    if let Some(name) = cfg.as_str() {
        return Ok(DataFormatDefinition::new(name));
    }
    let obj = cfg.as_object().ok_or_else(|| ctx.error("marshal/unmarshal expects a data format"))?;
    let mut formats = obj.iter().filter(|(k, _)| !matches!(k.as_str(), "id" | "description" | "disabled" | "variableSend" | "variableReceive" | "allowNullBody"));
    let (name, opts) = formats.next().ok_or_else(|| ctx.error("marshal/unmarshal requires a data format, e.g. json: {}"))?;
    if formats.next().is_some() {
        return Err(ctx.error("marshal/unmarshal must specify exactly one data format"));
    }
    if name == "custom" {
        let r = opts.as_str().map(String::from).or_else(|| opts.get("ref").map(|v| v.to_string()));
        let r = r.ok_or_else(|| ctx.error("custom data format requires a ref"))?;
        return Ok(DataFormatDefinition::named(r.trim_start_matches('#').trim_start_matches("bean:")));
    }
    let mut df = DataFormatDefinition::new(name.clone());
    if let Some(o) = opts.as_object() {
        for (k, v) in o {
            if k == "id" {
                continue;
            }
            df.options.insert(k.clone(), v.to_string());
        }
    }
    Ok(df)
}
