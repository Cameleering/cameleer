//! Embedded brokers for tests.
pub mod amqp_broker;
pub mod stomp_broker;
pub use amqp_broker::AmqpBroker;
pub use stomp_broker::StompBroker;
