//! # rust-camel AMQP 1.0
//!
//! A std-only implementation of OASIS AMQP 1.0 and the Camel components built on it:
//!
//! | scheme | module | notes |
//! |---|---|---|
//! | `amqp` | [`jms`] | Camel's AMQP component: JMS semantics over AMQP 1.0 (Artemis, ActiveMQ, Qpid...) |
//! | `azure-servicebus` | [`servicebus`] | Azure Service Bus queues / topics (SAS or Azure AD + CBS, peek-lock, sessions, lock renewal, deferral, transactions, dead-letter, scheduling) |
//! | `azure-eventhubs` | [`eventhubs`] | Azure Event Hubs (partitions, offsets, checkpoint stores, load balancing, epochs) |
//!
//! Protocol layers: [`codec`] (type system), [`types`] (frames, performatives, SASL),
//! [`message`] (message sections), [`client`] (connection / session / links, SASL, TLS,
//! heartbeats, credit, settlement incl. `rcv-settle-mode = second`, multi-frame
//! transfers, local transactions through a [`client::Coordinator`]), [`ws`] (AMQP over
//! WebSockets), [`cbs`] (claims-based security and management request/response),
//! [`identity`] (Azure AD token credentials), [`azure`] (namespaces, CBS token refresh).
//! [`testing::Amqp10Broker`] is an embedded broker with an Azure emulation mode.
//!
//! [`register`] takes over the `amqp` scheme; the JMS-over-STOMP flavour of `camel-amqp`
//! stays available as `amqp-stomp` (and as `jms` / `activemq`).
//!
//! ```no_run
//! use camel_core::prelude::*;
//!
//! let ctx = CamelContext::new();
//! camel_amqp10::register(&ctx);
//! ctx.properties().set("camel.component.amqp.remoteAddress", "amqp://localhost:5672");
//! ctx.add_routes(|b| {
//!     b.from("amqp:queue:orders?concurrentConsumers=2").to("azure-servicebus:orders?connectionString=RAW(Endpoint=sb://ns.servicebus.windows.net/;SharedAccessKeyName=k;SharedAccessKey=v)");
//!     b.from("azure-eventhubs:ns/telemetry?sharedAccessName=k&sharedAccessKey=RAW(v)").log("${body}");
//! }).unwrap();
//! ctx.start().unwrap();
//! ```

pub mod azure;
pub mod cbs;
pub mod client;
pub mod codec;
pub mod eventhubs;
pub mod identity;
pub mod jms;
pub mod message;
pub mod servicebus;
pub mod testing;
pub mod transport;
pub mod types;
pub mod util;
pub mod ws;

use std::sync::Arc;

use camel_core::context::CamelContext;

/// Register `amqp` (replacing any existing `amqp` component, e.g. camel-amqp's STOMP
/// flavour), `azure-servicebus` and `azure-eventhubs` (left alone when already present).
pub fn register(ctx: &CamelContext) {
    util::register_kinds();
    ctx.add_component_factory("amqp", || Arc::new(jms::AmqpComponent::new("amqp")));
    if !ctx.has_component("azure-servicebus") {
        ctx.add_component_factory("azure-servicebus", || Arc::new(servicebus::ServiceBusComponent));
    }
    if !ctx.has_component("azure-eventhubs") {
        ctx.add_component_factory("azure-eventhubs", || Arc::new(eventhubs::EventHubsComponent));
    }
}
