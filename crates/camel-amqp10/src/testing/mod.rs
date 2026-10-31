//! Test support: the embedded AMQP 1.0 broker [`Amqp10Broker`] (queues, topics, credit,
//! dispositions, selectors, SASL, TLS and an Azure Service Bus / Event Hubs emulation).

mod broker;

pub use broker::{Amqp10Broker, AzureConfig, BrokerBuilder, SELECTOR_FILTER};
