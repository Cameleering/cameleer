//! # Cameleer messaging: Kafka and MQTT
//!
//! Messaging components built on `std::net` only (no TLS, no SASL):
//!
//! | scheme | module | role |
//! |---|---|---|
//! | `kafka` | [`kafka`] | Kafka producer and consumer (consumer groups, committed offsets) over a built-in wire protocol client |
//! | `paho` | [`mqtt`] | MQTT 3.1.1 producer and consumer (Camel `camel-paho`) |
//! | `paho-mqtt5` | [`mqtt`] | MQTT 5.0 producer and consumer (Camel `camel-paho-mqtt5`) |
//!
//! Embedded test brokers live in [`testing`]: [`testing::KafkaBroker`] (in-memory,
//! single node, multiple partitions, consumer groups, offsets) and
//! [`testing::MqttBroker`] (topic wildcards, retained messages, QoS 0/1/2, wills,
//! persistent sessions).
//!
//! ```no_run
//! use cameleer_core::prelude::*;
//!
//! let ctx = CamelContext::new();
//! cameleer_kafka_mqtt::register(&ctx);
//! ctx.add_routes(|b| {
//!     b.from("kafka:orders?brokers=localhost:9092&groupId=billing&autoOffsetReset=earliest").to("log:orders");
//!     b.from("paho:sensors/+/temperature?brokerUrl=tcp://localhost:1883&qos=1").to("kafka:temperatures?brokers=localhost:9092");
//! }).unwrap();
//! ctx.start().unwrap();
//! ```

pub mod kafka;
pub mod mqtt;
pub mod net;
pub mod testing;

use std::sync::Arc;

use cameleer_core::context::CamelContext;
use cameleer_core::spi::Component;

fn add(ctx: &CamelContext, scheme: &'static str, f: impl Fn() -> Arc<dyn Component> + Send + Sync + 'static) {
    if !ctx.has_component(scheme) {
        ctx.add_component_factory(scheme, f);
    }
}

/// Register the components of this crate on a context (lazily created): `kafka`, `paho`
/// and `paho-mqtt5`. Schemes that already have a component are left untouched.
pub fn register(ctx: &CamelContext) {
    add(ctx, "kafka", || Arc::new(kafka::KafkaComponent::new()));
    add(ctx, "paho", || Arc::new(mqtt::PahoComponent::new(mqtt::MqttVersion::V311)));
    add(ctx, "paho-mqtt5", || Arc::new(mqtt::PahoComponent::new(mqtt::MqttVersion::V5)));
}
