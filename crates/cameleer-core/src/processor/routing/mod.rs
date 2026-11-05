//! Routing Enterprise Integration Patterns: filter, choice, split, aggregate,
//! multicast, recipientList, routingSlip, dynamicRouter, wireTap, enrich,
//! pollEnrich, loop, loadBalance, resequence, sort, pipeline and claimCheck.
//!
//! Each EIP has a model definition (`*Definition`, implementing
//! [`ProcessorDefinition`](crate::model::ProcessorDefinition)) and a runtime processor.
//! The fluent DSL methods live in [`crate::dsl::routing`].

pub mod aggregate;
pub mod choice;
pub mod claim_check;
pub mod enrich;
pub mod filter;
pub mod load_balance;
pub mod loop_eip;
pub mod multicast;
pub mod options;
pub mod recipient_list;
pub mod resequence;
pub mod sort;
pub mod split;
pub mod strategies;
pub mod support;
pub mod wire_tap;

pub use aggregate::{AggregateDefinition, AggregateProcessor, AggregationRepository, MemoryAggregationRepository};
pub use choice::{ChoiceDefinition, OtherwiseDefinition, WhenDefinition};
pub use claim_check::{ClaimCheckDefinition, ClaimCheckOperation};
pub use enrich::{EnrichDefinition, PollEnrichDefinition};
pub use filter::{FilterDefinition, PipelineDefinition};
pub use load_balance::{FailoverSettings, LoadBalanceDefinition, LoadBalancer, LoadBalancerPolicy};
pub use loop_eip::LoopDefinition;
pub use multicast::{MulticastDefinition, MulticastEngine};
pub use options::{Deferred, EipOption, EipOptions, MulticastSettings};
pub use recipient_list::{DynamicRouterDefinition, RecipientListDefinition, RoutingSlipDefinition};
pub use resequence::{ResequenceDefinition, ResequencerConfig};
pub use sort::SortDefinition;
pub use split::SplitDefinition;
pub use support::{aggregation_strategy_ref, StrategyDefinition};
pub use wire_tap::WireTapDefinition;
