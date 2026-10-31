//! Routing EIP DSL methods on [`RouteDefinition`].
//!
//! ## Block EIPs
//! `filter`, `choice` (`when`/`otherwise`), `split`, `aggregate`, `multicast`,
//! `loop_`/`loop_do_while`, `load_balance`, `resequence` and `pipeline` open a block:
//! subsequent DSL calls add outputs to the block until `.end()`.
//!
//! ```ignore
//! b.from("direct:in")
//!     .split(body()).parallel_processing().aggregation_strategy(grouped_body())
//!         .to("mock:part")
//!     .end()
//!     .to("mock:out");
//! ```
//!
//! ## Options
//! Options are chained right after the EIP and modify it:
//!
//! - **Generic options** (`parallel_processing`, `streaming`, `stop_on_exception`,
//!   `share_unit_of_work`, `timeout`, `aggregation_strategy(_ref)`, `delimiter`,
//!   `ignore_invalid_endpoints`, `copy`, `aggregate_on_exception`,
//!   `executor_pool_size`) apply to the most recently added *non-block* routing EIP
//!   (recipientList, routingSlip, dynamicRouter, wireTap, enrich, pollEnrich) if
//!   nothing was added after it, otherwise to the innermost open block
//!   (split, multicast, aggregate, loop, resequence).
//! - **EIP specific options** (e.g. `completion_size` for aggregate, `round_robin` for
//!   loadBalance, `batch_size` for resequence, `new_exchange_body` for wireTap,
//!   `precondition` for choice) act on that EIP.
//!
//! Using an option where it does not apply records a build error that is reported
//! when the routes are added to the context.
//!
//! `when()`/`otherwise()` close any nested blocks opened inside the previous clause
//! (like Camel's `endChoice()`), so `.choice().when(a).split(body()).to("x").when(b)`
//! works; `.end()` closes the choice.

use std::any::Any;
use std::cmp::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::builder::{constant, simple, ExpressionDefinition, IntoExpression};
use crate::error::CamelError;
use crate::model::{ProcessorDefinition, RouteDefinition};
use crate::processor::routing::aggregate::{AggregateDefinition, AggregationRepository};
use crate::processor::routing::choice::ChoiceDefinition;
use crate::processor::routing::claim_check::{ClaimCheckDefinition, ClaimCheckOperation};
use crate::processor::routing::enrich::{EnrichDefinition, PollEnrichDefinition};
use crate::processor::routing::filter::{FilterDefinition, PipelineDefinition};
use crate::processor::routing::load_balance::{FailoverSettings, LoadBalanceDefinition, LoadBalancer, LoadBalancerPolicy};
use crate::processor::routing::loop_eip::LoopDefinition;
use crate::processor::routing::multicast::MulticastDefinition;
use crate::processor::routing::options::{Deferred, EipOption, EipOptions, OptionTarget};
use crate::processor::routing::recipient_list::{DynamicRouterDefinition, RecipientListDefinition, RoutingSlipDefinition};
use crate::processor::routing::resequence::{ResequenceDefinition, ResequencerConfig};
use crate::processor::routing::sort::SortDefinition;
use crate::processor::routing::split::SplitDefinition;
use crate::processor::routing::support::StrategyDefinition;
use crate::processor::routing::wire_tap::WireTapDefinition;
use crate::spi::{AggregationStrategy, Processor};
use crate::value::Value;

/// Route extension key remembering the last added non-block routing EIP.
const LAST_NODE: &str = "camel.routing.lastNode";

struct LastNode {
    depth: usize,
    count: usize,
    target: Arc<dyn OptionTarget>,
}

fn deep_count(d: &dyn ProcessorDefinition) -> usize {
    d.outputs().into_iter().map(|c| 1 + deep_count(c)).sum()
}

/// Accessor for the generic options of the routing block definitions.
fn block_options(any: &mut dyn Any) -> Option<&mut dyn EipOptions> {
    if any.is::<SplitDefinition>() {
        return any.downcast_mut::<SplitDefinition>().map(|d| d as &mut dyn EipOptions);
    }
    if any.is::<MulticastDefinition>() {
        return any.downcast_mut::<MulticastDefinition>().map(|d| d as &mut dyn EipOptions);
    }
    if any.is::<AggregateDefinition>() {
        return any.downcast_mut::<AggregateDefinition>().map(|d| d as &mut dyn EipOptions);
    }
    if any.is::<LoopDefinition>() {
        return any.downcast_mut::<LoopDefinition>().map(|d| d as &mut dyn EipOptions);
    }
    if any.is::<ResequenceDefinition>() {
        return any.downcast_mut::<ResequenceDefinition>().map(|d| d as &mut dyn EipOptions);
    }
    None
}

/// Static uri or (when it contains `${`) a simple expression.
fn uri_expression(uri: String) -> ExpressionDefinition {
    if uri.contains("${") {
        simple(uri).def
    } else {
        constant(uri).def
    }
}

impl RouteDefinition {
    // -----------------------------------------------------------------------
    // internal helpers
    // -----------------------------------------------------------------------

    fn routing_container_count(&self) -> usize {
        match self.stack.last() {
            Some(b) => deep_count(b.as_ref()) + self.pending.as_ref().map(|p| 1 + deep_count(p.as_ref())).unwrap_or(0),
            None => self.outputs.iter().map(|o| 1 + deep_count(o.as_ref())).sum(),
        }
    }

    fn add_deferred<T: ProcessorDefinition + EipOptions>(&mut self, def: T) -> &mut Self {
        let d = Deferred::new(def);
        let target: Arc<dyn OptionTarget> = d.inner.clone();
        self.add(Box::new(d));
        let rec = LastNode { depth: self.stack.len(), count: self.routing_container_count(), target };
        self.extensions.insert(LAST_NODE.to_string(), Box::new(rec));
        self
    }

    fn last_routing_node(&self) -> Option<Arc<dyn OptionTarget>> {
        let rec = self.extension::<LastNode>(LAST_NODE)?;
        if rec.depth == self.stack.len() && rec.count == self.routing_container_count() {
            Some(rec.target.clone())
        } else {
            None
        }
    }

    /// Apply a generic routing option (see the module docs for the targeting rules).
    pub fn routing_option(&mut self, option: EipOption) -> &mut Self {
        if let Some(t) = self.last_routing_node() {
            if t.apply(&option) {
                return self;
            }
        }
        self.flush_pending();
        if let Some(top) = self.stack.last_mut() {
            if let Some(o) = block_options(top.as_any_mut()) {
                if o.apply_option(&option) {
                    return self;
                }
            }
        }
        let msg = format!("Option {} is not supported at this position in the route", option.name());
        self.add_build_error(CamelError::illegal_argument(msg));
        self
    }

    /// Modify the last added non-block routing EIP of type `T`.
    fn with_last_node<T: 'static>(&mut self, option: &str, f: impl FnOnce(&mut T)) -> &mut Self {
        let mut f = Some(f);
        let mut applied = false;
        if let Some(t) = self.last_routing_node() {
            t.with_any(&mut |a: &mut dyn Any| {
                if let Some(d) = a.downcast_mut::<T>() {
                    if let Some(f) = f.take() {
                        f(d);
                        applied = true;
                    }
                }
            });
        }
        if !applied {
            self.add_build_error(CamelError::illegal_argument(format!("Option {} is not supported at this position in the route", option)));
        }
        self
    }

    /// Modify the innermost open block of type `T`.
    fn with_block<T: 'static>(&mut self, option: &str, f: impl FnOnce(&mut T)) -> &mut Self {
        match self.current_block_mut::<T>() {
            Some(b) => f(b),
            None => {
                let msg = format!("Option {} must be used directly after its EIP", option);
                self.add_build_error(CamelError::illegal_argument(msg));
            }
        }
        self
    }

    // -----------------------------------------------------------------------
    // generic options
    // -----------------------------------------------------------------------

    /// Process sub-exchanges concurrently on a thread pool (split, multicast, recipientList).
    pub fn parallel_processing(&mut self) -> &mut Self {
        self.routing_option(EipOption::ParallelProcessing(true))
    }
    /// With parallel processing, aggregate replies in completion order instead of input order.
    pub fn streaming(&mut self) -> &mut Self {
        self.routing_option(EipOption::Streaming(true))
    }
    /// Stop processing further sub-exchanges as soon as one fails.
    pub fn stop_on_exception(&mut self) -> &mut Self {
        self.routing_option(EipOption::StopOnException(true))
    }
    /// Sub-exchanges share the parent's unit of work (on-completions run with the parent).
    pub fn share_unit_of_work(&mut self) -> &mut Self {
        self.routing_option(EipOption::ShareUnitOfWork(true))
    }
    /// Timeout: parallel processing total timeout (split/multicast/recipientList),
    /// completion timeout (aggregate), batch/stream timeout (resequence), poll timeout (pollEnrich).
    pub fn timeout(&mut self, d: Duration) -> &mut Self {
        self.routing_option(EipOption::Timeout(d))
    }
    /// Aggregation strategy (split, multicast, recipientList, aggregate, enrich, pollEnrich).
    pub fn aggregation_strategy(&mut self, s: impl AggregationStrategy + 'static) -> &mut Self {
        self.routing_option(EipOption::AggregationStrategy(StrategyDefinition::instance(s)))
    }
    /// Aggregation strategy from a shared instance.
    pub fn aggregation_strategy_arc(&mut self, s: Arc<dyn AggregationStrategy>) -> &mut Self {
        self.routing_option(EipOption::AggregationStrategy(StrategyDefinition::Instance(s)))
    }
    /// Aggregation strategy looked up in the registry by name.
    pub fn aggregation_strategy_ref(&mut self, name: impl Into<String>) -> &mut Self {
        self.routing_option(EipOption::AggregationStrategy(StrategyDefinition::Ref(name.into())))
    }
    /// Delimiter: split (string bodies; `"false"` disables), recipientList/routingSlip/dynamicRouter uris.
    pub fn delimiter(&mut self, d: impl Into<String>) -> &mut Self {
        self.routing_option(EipOption::Delimiter(d.into()))
    }
    /// Ignore endpoint uris that cannot be resolved (recipientList, routingSlip, dynamicRouter, enrich, wireTap).
    pub fn ignore_invalid_endpoints(&mut self) -> &mut Self {
        self.routing_option(EipOption::IgnoreInvalidEndpoints(true))
    }
    /// Copy option: loop (each iteration starts from a copy of the input), wireTap (copy the exchange, default true).
    pub fn copy(&mut self, copy: bool) -> &mut Self {
        self.routing_option(EipOption::Copy(copy))
    }
    /// Call the aggregation strategy even when the resource exchange failed (enrich, pollEnrich).
    pub fn aggregate_on_exception(&mut self) -> &mut Self {
        self.routing_option(EipOption::AggregateOnException(true))
    }
    /// Size of the thread pool used for parallel / asynchronous processing.
    pub fn executor_pool_size(&mut self, n: usize) -> &mut Self {
        self.routing_option(EipOption::PoolSize(n))
    }

    // -----------------------------------------------------------------------
    // filter / choice / pipeline
    // -----------------------------------------------------------------------

    /// Message Filter block: outputs only see exchanges matching the predicate.
    pub fn filter(&mut self, predicate: impl IntoExpression) -> &mut Self {
        self.push_block(Box::new(FilterDefinition::new(predicate.into_expression())))
    }

    /// Content Based Router block. Add clauses with `when()` / `otherwise()`, close with `end()`.
    pub fn choice(&mut self) -> &mut Self {
        self.push_block(Box::new(ChoiceDefinition::new()))
    }

    /// Start a `when` clause of the enclosing choice (closing nested blocks).
    pub fn when(&mut self, predicate: impl IntoExpression) -> &mut Self {
        let r = match self.unwind_to_block::<ChoiceDefinition>() {
            Some(c) => c.add_when(predicate.into_expression()),
            None => Err(CamelError::illegal_argument("when() must be used inside a choice()")),
        };
        if let Err(e) = r {
            self.add_build_error(e);
        }
        self
    }

    /// Start the `otherwise` clause of the enclosing choice (closing nested blocks).
    pub fn otherwise(&mut self) -> &mut Self {
        let r = match self.unwind_to_block::<ChoiceDefinition>() {
            Some(c) => c.add_otherwise(),
            None => Err(CamelError::illegal_argument("otherwise() must be used inside a choice()")),
        };
        if let Err(e) = r {
            self.add_build_error(e);
        }
        self
    }

    /// Go back to the enclosing choice (closing nested blocks), like Camel's `endChoice()`.
    pub fn end_choice(&mut self) -> &mut Self {
        if self.unwind_to_block::<ChoiceDefinition>().is_none() {
            self.add_build_error(CamelError::illegal_argument("endChoice() must be used inside a choice()"));
        }
        self
    }

    /// Precondition mode for the current choice: predicates are evaluated once when the
    /// route is created and only the matching branch is included.
    pub fn precondition(&mut self) -> &mut Self {
        let ok = match self.unwind_to_block::<ChoiceDefinition>() {
            Some(c) => {
                c.precondition = true;
                true
            }
            None => false,
        };
        if !ok {
            self.add_build_error(CamelError::illegal_argument("precondition() must be used on a choice()"));
        }
        self
    }

    /// Explicit pipeline block.
    pub fn pipeline(&mut self) -> &mut Self {
        self.push_block(Box::new(PipelineDefinition::default()))
    }

    // -----------------------------------------------------------------------
    // split / multicast / recipientList / routingSlip / dynamicRouter
    // -----------------------------------------------------------------------

    /// Splitter block: the expression result is split into parts routed through the block.
    pub fn split(&mut self, expression: impl IntoExpression) -> &mut Self {
        self.push_block(Box::new(SplitDefinition::new(expression.into_expression())))
    }

    /// Multicast block: every output receives a copy of the exchange.
    pub fn multicast(&mut self) -> &mut Self {
        self.push_block(Box::new(MulticastDefinition::new()))
    }

    /// Recipient list: send a copy to each endpoint uri given by the expression.
    pub fn recipient_list(&mut self, expression: impl IntoExpression) -> &mut Self {
        self.add_deferred(RecipientListDefinition::new(expression.into_expression()))
    }

    /// Routing slip: route the exchange through the uris given by the expression, in sequence.
    pub fn routing_slip(&mut self, expression: impl IntoExpression) -> &mut Self {
        self.add_deferred(RoutingSlipDefinition::new(expression.into_expression()))
    }

    /// Dynamic router: evaluate the expression repeatedly for the next uri(s) until it returns null.
    pub fn dynamic_router(&mut self, expression: impl IntoExpression) -> &mut Self {
        self.add_deferred(DynamicRouterDefinition::new(expression.into_expression()))
    }

    // -----------------------------------------------------------------------
    // aggregate
    // -----------------------------------------------------------------------

    /// Aggregator block correlating by `correlation` and merging with `strategy`.
    pub fn aggregate(&mut self, correlation: impl IntoExpression, strategy: impl AggregationStrategy + 'static) -> &mut Self {
        self.push_block(Box::new(AggregateDefinition::new(correlation.into_expression(), Some(StrategyDefinition::instance(strategy)))))
    }

    /// Aggregator block whose strategy is looked up in the registry.
    pub fn aggregate_ref(&mut self, correlation: impl IntoExpression, strategy_ref: impl Into<String>) -> &mut Self {
        self.push_block(Box::new(AggregateDefinition::new(correlation.into_expression(), Some(StrategyDefinition::Ref(strategy_ref.into())))))
    }

    /// Aggregator block; set the strategy with `aggregation_strategy(..)`.
    pub fn aggregate_correlation(&mut self, correlation: impl IntoExpression) -> &mut Self {
        self.push_block(Box::new(AggregateDefinition::new(correlation.into_expression(), None)))
    }

    /// Complete a group when it has `n` exchanges.
    pub fn completion_size(&mut self, n: usize) -> &mut Self {
        self.with_block::<AggregateDefinition>("completionSize", |a| a.completion_size = Some(n))
    }
    /// Complete a group when it has as many exchanges as the expression (evaluated on the aggregated exchange) says.
    pub fn completion_size_expr(&mut self, e: impl IntoExpression) -> &mut Self {
        let e = e.into_expression();
        self.with_block::<AggregateDefinition>("completionSize", |a| a.completion_size_expression = Some(e))
    }
    /// Complete a group when the predicate matches the aggregated (or, eager, incoming) exchange.
    pub fn completion_predicate(&mut self, p: impl IntoExpression) -> &mut Self {
        let p = p.into_expression();
        self.with_block::<AggregateDefinition>("completionPredicate", |a| a.completion_predicate = Some(p))
    }
    /// Complete a group after a period of inactivity.
    pub fn completion_timeout(&mut self, d: Duration) -> &mut Self {
        self.with_block::<AggregateDefinition>("completionTimeout", |a| a.completion_timeout = Some(d))
    }
    /// Inactivity timeout in millis computed per incoming exchange.
    pub fn completion_timeout_expr(&mut self, e: impl IntoExpression) -> &mut Self {
        let e = e.into_expression();
        self.with_block::<AggregateDefinition>("completionTimeout", |a| a.completion_timeout_expression = Some(e))
    }
    /// Complete all groups periodically.
    pub fn completion_interval(&mut self, d: Duration) -> &mut Self {
        self.with_block::<AggregateDefinition>("completionInterval", |a| a.completion_interval = Some(d))
    }
    /// Complete when the group size reaches the `CamelBatchSize` of the consumer batch.
    pub fn completion_from_batch_consumer(&mut self) -> &mut Self {
        self.with_block::<AggregateDefinition>("completionFromBatchConsumer", |a| a.completion_from_batch_consumer = true)
    }
    /// Complete the existing groups when an exchange with a new correlation key arrives.
    pub fn completion_on_new_correlation_group(&mut self) -> &mut Self {
        self.with_block::<AggregateDefinition>("completionOnNewCorrelationGroup", |a| a.completion_on_new_correlation_group = true)
    }
    /// Evaluate completion conditions on the incoming exchange (before aggregating).
    pub fn eager_check_completion(&mut self) -> &mut Self {
        self.with_block::<AggregateDefinition>("eagerCheckCompletion", |a| a.eager_check_completion = true)
    }
    /// Complete all groups when the route stops.
    pub fn force_completion_on_stop(&mut self) -> &mut Self {
        self.with_block::<AggregateDefinition>("forceCompletionOnStop", |a| a.force_completion_on_stop = true)
    }
    /// Discard groups that complete by timeout instead of routing them.
    pub fn discard_on_completion_timeout(&mut self) -> &mut Self {
        self.with_block::<AggregateDefinition>("discardOnCompletionTimeout", |a| a.discard_on_completion_timeout = true)
    }
    /// Silently ignore exchanges whose correlation key is null/empty.
    pub fn ignore_invalid_correlation_keys(&mut self) -> &mut Self {
        self.with_block::<AggregateDefinition>("ignoreInvalidCorrelationKeys", |a| a.ignore_invalid_correlation_keys = true)
    }
    /// Reject later exchanges for keys whose group already completed (`max_keys` remembered, 0 = unbounded).
    pub fn close_correlation_key_on_completion(&mut self, max_keys: usize) -> &mut Self {
        self.with_block::<AggregateDefinition>("closeCorrelationKeyOnCompletion", |a| a.close_correlation_key_on_completion = Some(max_keys))
    }
    /// Use a custom aggregation repository.
    pub fn aggregation_repository(&mut self, repo: Arc<dyn AggregationRepository>) -> &mut Self {
        self.with_block::<AggregateDefinition>("aggregationRepository", |a| a.repository = Some(repo))
    }

    // -----------------------------------------------------------------------
    // wireTap / enrich / pollEnrich
    // -----------------------------------------------------------------------

    /// Wire tap: asynchronously send a copy to `uri` (dynamic if it contains `${`).
    pub fn wire_tap(&mut self, uri: impl Into<String>) -> &mut Self {
        self.add_deferred(WireTapDefinition::new(uri))
    }
    /// Body of the wire tapped exchange.
    pub fn new_exchange_body(&mut self, e: impl IntoExpression) -> &mut Self {
        let e = e.into_expression();
        self.with_last_node::<WireTapDefinition>("newExchangeBody", |w| w.new_exchange_body = Some(e))
    }
    /// Header of the wire tapped exchange.
    pub fn new_exchange_header(&mut self, name: impl Into<String>, e: impl IntoExpression) -> &mut Self {
        let (n, e) = (name.into(), e.into_expression());
        self.with_last_node::<WireTapDefinition>("newExchangeHeader", |w| w.new_exchange_headers.push((n, e)))
    }
    /// Processor preparing the wire tapped exchange.
    pub fn on_prepare(&mut self, p: impl Processor + 'static) -> &mut Self {
        let p: Arc<dyn Processor> = Arc::new(p);
        self.with_last_node::<WireTapDefinition>("onPrepare", |w| w.on_prepare = Some(p))
    }

    /// Content enricher: request/reply to `uri` (dynamic if it contains `${`); reply replaces the message.
    pub fn enrich(&mut self, uri: impl Into<String>) -> &mut Self {
        self.add_deferred(EnrichDefinition::new(uri_expression(uri.into())))
    }
    /// Content enricher merging the reply with a strategy.
    pub fn enrich_with(&mut self, uri: impl Into<String>, strategy: impl AggregationStrategy + 'static) -> &mut Self {
        let mut d = EnrichDefinition::new(uri_expression(uri.into()));
        d.strategy = Some(StrategyDefinition::instance(strategy));
        self.add_deferred(d)
    }
    /// Content enricher with the uri computed by an expression.
    pub fn enrich_expr(&mut self, uri: impl IntoExpression) -> &mut Self {
        self.add_deferred(EnrichDefinition::new(uri.into_expression()))
    }

    /// Poll enricher: receive from `uri` with a polling consumer
    /// (`timeout_ms`: -1 wait forever, 0 no wait, >0 wait at most).
    pub fn poll_enrich(&mut self, uri: impl Into<String>, timeout_ms: i64) -> &mut Self {
        self.add_deferred(PollEnrichDefinition::new(uri_expression(uri.into()), timeout_ms))
    }
    /// Poll enricher merging with a strategy.
    pub fn poll_enrich_with(&mut self, uri: impl Into<String>, timeout_ms: i64, strategy: impl AggregationStrategy + 'static) -> &mut Self {
        let mut d = PollEnrichDefinition::new(uri_expression(uri.into()), timeout_ms);
        d.strategy = Some(StrategyDefinition::instance(strategy));
        self.add_deferred(d)
    }
    /// Poll enricher with the uri computed by an expression.
    pub fn poll_enrich_expr(&mut self, uri: impl IntoExpression, timeout_ms: i64) -> &mut Self {
        self.add_deferred(PollEnrichDefinition::new(uri.into_expression(), timeout_ms))
    }

    // -----------------------------------------------------------------------
    // loop / sort / claimCheck
    // -----------------------------------------------------------------------

    /// Loop block repeated `count` times (expression evaluated once).
    pub fn loop_(&mut self, count: impl IntoExpression) -> &mut Self {
        self.push_block(Box::new(LoopDefinition::count(count.into_expression())))
    }
    /// Loop block repeated while the predicate matches (checked before each iteration).
    pub fn loop_do_while(&mut self, predicate: impl IntoExpression) -> &mut Self {
        self.push_block(Box::new(LoopDefinition::do_while(predicate.into_expression())))
    }

    /// Sort the list given by the expression (natural order) and set it as the body.
    pub fn sort(&mut self, expression: impl IntoExpression) -> &mut Self {
        self.add(Box::new(SortDefinition { id: None, expression: expression.into_expression(), comparator: None }))
    }
    /// Sort with a custom comparator.
    pub fn sort_with<F>(&mut self, expression: impl IntoExpression, comparator: F) -> &mut Self
    where
        F: Fn(&Value, &Value) -> Ordering + Send + Sync + 'static,
    {
        self.add(Box::new(SortDefinition { id: None, expression: expression.into_expression(), comparator: Some(Arc::new(comparator)) }))
    }

    /// Claim check operation (`key` may be empty for Push/Pop).
    pub fn claim_check(&mut self, operation: ClaimCheckOperation, key: &str) -> &mut Self {
        self.claim_check_with_filter(operation, key, None)
    }
    /// Claim check with a merge filter (e.g. `"body,header:foo,-header:bar"`).
    pub fn claim_check_with_filter(&mut self, operation: ClaimCheckOperation, key: &str, filter: Option<&str>) -> &mut Self {
        let key = if key.is_empty() { None } else { Some(key.to_string()) };
        self.add(Box::new(ClaimCheckDefinition { id: None, operation, key, filter: filter.map(str::to_string) }))
    }

    // -----------------------------------------------------------------------
    // loadBalance
    // -----------------------------------------------------------------------

    /// Load balancer block (round robin by default); its outputs are the targets.
    pub fn load_balance(&mut self) -> &mut Self {
        self.push_block(Box::new(LoadBalanceDefinition::default()))
    }
    /// Load balancer block with an explicit policy.
    pub fn load_balance_with(&mut self, policy: LoadBalancerPolicy) -> &mut Self {
        self.push_block(Box::new(LoadBalanceDefinition { id: None, policy, outputs: Default::default() }))
    }
    fn lb_policy(&mut self, name: &str, policy: LoadBalancerPolicy) -> &mut Self {
        self.with_block::<LoadBalanceDefinition>(name, |l| l.policy = policy)
    }
    /// Round robin policy.
    pub fn round_robin(&mut self) -> &mut Self {
        self.lb_policy("roundRobin", LoadBalancerPolicy::RoundRobin)
    }
    /// Random policy.
    pub fn random(&mut self) -> &mut Self {
        self.lb_policy("random", LoadBalancerPolicy::Random)
    }
    /// Sticky policy: exchanges with the same key go to the same target.
    pub fn sticky(&mut self, key: impl IntoExpression) -> &mut Self {
        let k = key.into_expression();
        self.lb_policy("sticky", LoadBalancerPolicy::Sticky(k))
    }
    /// Weighted policy (round robin or random) with one distribution ratio per target.
    pub fn weighted(&mut self, round_robin: bool, distribution_ratio: &[usize]) -> &mut Self {
        self.lb_policy("weighted", LoadBalancerPolicy::Weighted { round_robin, ratios: distribution_ratio.to_vec() })
    }
    /// Topic policy: all targets receive a copy.
    pub fn topic(&mut self) -> &mut Self {
        self.lb_policy("topic", LoadBalancerPolicy::Topic)
    }
    /// Failover policy on the given exception kinds (empty = any), trying each target once.
    pub fn failover(&mut self, exceptions: &[&str]) -> &mut Self {
        let s = FailoverSettings { exceptions: exceptions.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        self.lb_policy("failover", LoadBalancerPolicy::Failover(s))
    }
    /// Failover policy with all options. With `round_robin` and `maximum_failover_attempts = -1`
    /// it keeps failing over until a target succeeds (as in Camel).
    pub fn failover_full(&mut self, maximum_failover_attempts: i32, round_robin: bool, sticky: bool, exceptions: &[&str]) -> &mut Self {
        let s = FailoverSettings {
            exceptions: exceptions.iter().map(|s| s.to_string()).collect(),
            maximum_failover_attempts,
            round_robin,
            sticky,
        };
        self.lb_policy("failover", LoadBalancerPolicy::Failover(s))
    }
    /// Custom load balancer.
    pub fn custom_load_balancer(&mut self, lb: impl LoadBalancer + 'static) -> &mut Self {
        self.lb_policy("custom", LoadBalancerPolicy::Custom(Arc::new(lb)))
    }

    // -----------------------------------------------------------------------
    // resequence
    // -----------------------------------------------------------------------

    /// Resequencer block (batch mode by default) ordering by the expression.
    pub fn resequence(&mut self, expression: impl IntoExpression) -> &mut Self {
        self.push_block(Box::new(ResequenceDefinition::new(expression.into_expression())))
    }
    /// Use batch mode (the default).
    pub fn batch(&mut self) -> &mut Self {
        self.with_block::<ResequenceDefinition>("batch", |r| {
            if !matches!(r.config, ResequencerConfig::Batch { .. }) {
                r.config = ResequencerConfig::batch()
            }
        })
    }
    /// Use stream mode.
    pub fn stream(&mut self) -> &mut Self {
        self.with_block::<ResequenceDefinition>("stream", |r| {
            if !matches!(r.config, ResequencerConfig::Stream { .. }) {
                r.config = ResequencerConfig::stream()
            }
        })
    }
    /// Batch mode: maximum batch size.
    pub fn batch_size(&mut self, n: usize) -> &mut Self {
        self.with_block::<ResequenceDefinition>("batchSize", |r| {
            if let ResequencerConfig::Batch { batch_size, .. } = &mut r.config {
                *batch_size = n.max(1)
            }
        })
    }
    /// Batch mode: timeout for collecting a batch.
    pub fn batch_timeout(&mut self, d: Duration) -> &mut Self {
        self.with_block::<ResequenceDefinition>("batchTimeout", |r| {
            if let ResequencerConfig::Batch { batch_timeout, .. } = &mut r.config {
                *batch_timeout = d
            }
        })
    }
    /// Batch mode: keep exchanges with equal keys.
    pub fn allow_duplicates(&mut self) -> &mut Self {
        self.with_block::<ResequenceDefinition>("allowDuplicates", |r| {
            if let ResequencerConfig::Batch { allow_duplicates, .. } = &mut r.config {
                *allow_duplicates = true
            }
        })
    }
    /// Batch mode: sort descending.
    pub fn reverse(&mut self) -> &mut Self {
        self.with_block::<ResequenceDefinition>("reverse", |r| {
            if let ResequencerConfig::Batch { reverse, .. } = &mut r.config {
                *reverse = true
            }
        })
    }
    /// Stream mode: buffer capacity.
    pub fn capacity(&mut self, n: usize) -> &mut Self {
        self.with_block::<ResequenceDefinition>("capacity", |r| {
            if let ResequencerConfig::Stream { capacity, .. } = &mut r.config {
                *capacity = n.max(1)
            }
        })
    }
    /// Stream mode: reject exchanges older than the last delivered one.
    pub fn reject_old(&mut self) -> &mut Self {
        self.with_block::<ResequenceDefinition>("rejectOld", |r| {
            if let ResequencerConfig::Stream { reject_old, .. } = &mut r.config {
                *reject_old = true
            }
        })
    }
    /// Silently drop exchanges whose sequence expression cannot be evaluated.
    pub fn ignore_invalid_exchanges(&mut self) -> &mut Self {
        self.with_block::<ResequenceDefinition>("ignoreInvalidExchanges", |r| match &mut r.config {
            ResequencerConfig::Batch { ignore_invalid_exchanges, .. } | ResequencerConfig::Stream { ignore_invalid_exchanges, .. } => {
                *ignore_invalid_exchanges = true
            }
        })
    }
}
