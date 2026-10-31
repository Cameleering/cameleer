//! # rust-camel messaging
//!
//! Message broker components built on `std::net` only (no TLS):
//!
//! | scheme | module | protocol |
//! |---|---|---|
//! | `spring-rabbitmq` | [`amqp::component`] | AMQP 0-9-1 (RabbitMQ), Camel `camel-spring-rabbitmq` options |
//! | `rabbitmq` | [`amqp::component`] | AMQP 0-9-1, legacy Camel `camel-rabbitmq` options |
//! | `stomp` | [`stomp::component`] | STOMP 1.2 |
//! | `jms`, `activemq`, `amqp-stomp` | [`stomp::component`] | JMS semantics (queues, topics, selectors, request/reply, transactions) over STOMP 1.2 |
//!
//! The `amqp` scheme (Camel's AMQP 1.0 component) is provided by `camel-amqp10`; the
//! STOMP flavour formerly registered as `amqp` is available as `amqp-stomp`.
//!
//! Protocol clients: [`amqp::client`] (AMQP 0-9-1) and [`stomp::client`] (STOMP 1.2);
//! JMS selectors: [`selector`]. Embedded brokers for tests: [`testing::AmqpBroker`] and
//! [`testing::StompBroker`].
//!
//! ```no_run
//! use camel_core::prelude::*;
//!
//! let ctx = CamelContext::new();
//! camel_amqp::register(&ctx);
//! ctx.properties().set("camel.component.jms.brokerUrl", "tcp://localhost:61613");
//! ctx.add_routes(|b| {
//!     b.from("spring-rabbitmq:orders?queues=eu-orders&routingKey=eu&host=localhost").to("jms:queue:orders");
//!     b.from("jms:queue:orders?concurrentConsumers=4&selector=region = 'EU'").log("${body}");
//!     b.from("stomp:topic:news?brokerURL=tcp://localhost:61613").to("rabbitmq:news?hostname=localhost&exchangeType=fanout");
//! }).unwrap();
//! ctx.start().unwrap();
//! ```

pub mod amqp;
pub mod common;
pub mod selector;
pub mod stomp;
pub mod testing;

use std::sync::Arc;

use camel_core::context::CamelContext;
use camel_core::spi::Component;

fn add(ctx: &CamelContext, scheme: &'static str, f: impl Fn() -> Arc<dyn Component> + Send + Sync + 'static) {
    if !ctx.has_component(scheme) {
        ctx.add_component_factory(scheme, f);
    }
}

/// Register the components of this crate on a context (lazily created):
/// `spring-rabbitmq`, `rabbitmq`, `stomp`, `jms`, `activemq` and `amqp-stomp`. Schemes that
/// already have a component are left untouched.
pub fn register(ctx: &CamelContext) {
    add(ctx, "spring-rabbitmq", || Arc::new(amqp::component::RabbitComponent::spring()));
    add(ctx, "rabbitmq", || Arc::new(amqp::component::RabbitComponent::legacy()));
    add(ctx, "stomp", || Arc::new(stomp::component::JmsComponent::stomp()));
    add(ctx, "jms", || Arc::new(stomp::component::JmsComponent::jms("jms")));
    add(ctx, "activemq", || Arc::new(stomp::component::JmsComponent::jms("activemq")));
    add(ctx, "amqp-stomp", || Arc::new(stomp::component::JmsComponent::jms("amqp-stomp")));
}
