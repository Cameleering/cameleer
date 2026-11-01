//! # rust-camel core
//!
//! A Rust implementation of the Apache Camel integration framework: exchanges,
//! messages, endpoints, components, routes, Enterprise Integration Patterns,
//! expression languages and a fluent route DSL.

pub mod async_delay;
pub mod builder;
pub mod component;
pub mod context;
pub mod dataformat;
pub mod dsl;
pub mod error;
pub mod executor;
pub mod extensions;
pub mod health;
pub mod history;
pub mod json;
pub mod language;
pub mod log;
pub mod management;
pub mod message;
pub mod model;
pub mod processor;
pub mod regex;
pub mod spi;
pub mod supervising;
pub mod template;
pub mod template_routes;
pub mod tracer;
pub mod transaction;
pub mod tz;
pub mod util;
pub mod util_time;
pub mod value;

/// Convenient imports for applications and components.
pub mod prelude {
    pub use crate::builder::{
        and, body, constant, exchange_property, expr, header, headers, language, not, or, pred, simple, ExpressionDefinition,
        IntoExpression, ValueBuilder,
    };
    pub use crate::component::mock::MockEndpoint;
    pub use crate::context::{CamelContext, ServiceStatus};
    pub use crate::dsl::RouteBuilder;
    pub use crate::error::{kinds, CamelError, Result};
    pub use crate::log::Level as LoggingLevel;
    pub use crate::message::{props, Exchange, ExchangePattern, Headers, Message};
    pub use crate::model::{ProcessorDefinition, RouteContext, RouteDefinition};
    pub use crate::spi::{AggregationStrategy, Component, Consumer, Endpoint, Expression, Language, Predicate, Processor, Producer, Service};
    pub use crate::template::{ConsumerTemplate, ProducerTemplate};
    pub use crate::util::Parameters;
    pub use crate::value::{FromValue, Value};

    pub use crate::dataformat::DataFormatDefinition;
    pub use crate::language::{
        bean_method, jq, jsonpath, jsonpath_as, method, ref_expression, tokenize, tokenize_pair, tokenize_regex, tokenize_xml, xpath,
        xpath_as, LanguageOptions,
    };
    pub use crate::processor::errors::{
        caught_exception, dead_letter_channel, default_error_handler, memory_idempotent_repository, no_error_handler, CamelContextErrorExt,
        RedeliveryPolicy,
    };
    pub use crate::processor::routing::strategies;
    pub use crate::processor::routing::ClaimCheckOperation;
}

pub use prelude::*;
