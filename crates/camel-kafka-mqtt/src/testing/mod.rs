//! Embedded brokers for self-contained tests: an in-memory single-node Kafka broker
//! ([`KafkaBroker`]) and an MQTT 3.1.1/5.0 broker ([`MqttBroker`]). Both bind to an
//! ephemeral port on 127.0.0.1 by default and stop when dropped.

pub mod kafka_broker;
pub mod mqtt_broker;

pub use kafka_broker::{KafkaBroker, KafkaBrokerConfig};
pub use mqtt_broker::{MqttBroker, MqttBrokerConfig};
