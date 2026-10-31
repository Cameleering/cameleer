//! The `spring-rabbitmq` and `rabbitmq` components (AMQP 0-9-1).
//!
//! ```text
//! spring-rabbitmq:exchange?routingKey=&queues=a,b&autoDeclare=true&exchangeType=direct
//!     &concurrentConsumers=1&acknowledgeMode=AUTO|MANUAL|NONE&prefetchCount=250
//!     &deadLetterExchange=&deadLetterQueue=&deadLetterRoutingKey=&deadLetterExchangeType=direct
//!     &rejectAndDontRequeue=true&maximumRetryAttempts=5&retryDelay=1000&replyTimeout=30000
//!     &disableReplyTo=false&confirm=auto|enabled|disabled&confirmTimeout=5000
//!     &autoDeclareProducer=false&exclusive=false&allowNullBody=false&arg.queue.x-message-ttl=...
//! rabbitmq:exchange?hostname=&portNumber=5672&queue=&routingKey=&autoAck=true&username=&password=
//!     &vhost=/&exchangeType=direct&declare=true&durable=true&autoDelete=true&exclusive=false
//!     &concurrentConsumers=1&prefetchEnabled=false&prefetchCount=&prefetchGlobal=false
//!     &deadLetterExchange=&deadLetterQueue=&deadLetterRoutingKey=&requestTimeout=20000
//!     &publisherAcknowledgements=false&publisherAcknowledgementsTimeout=&mandatory=false
//!     &guaranteedDeliveries=false&bridgeEndpoint=false&requestedHeartbeat=60&skipQueueDeclare
//!     &skipQueueBind&skipExchangeDeclare&args=...
//! ```
//!
//! Connection settings of `spring-rabbitmq` come from the uri options `host`, `port`,
//! `addresses`, `username`, `password`, `vhost` (rust-camel additions; Camel uses a
//! `connectionFactory` bean), from `camel.component.spring-rabbitmq.<option>` or from the
//! Spring Boot properties `spring.rabbitmq.host|port|username|password|virtual-host|addresses`.
//! `rabbitmq` uses `hostname`, `portNumber`, `addresses`, `username`, `password`, `vhost`
//! (also as `camel.component.rabbitmq.<option>`). The exchange name `default` (spring) or
//! an empty name means the AMQP default exchange.
//!
//! **TLS** (`amqps`): enabled by an `amqps://` address, `sslProtocol=true|TLS|TLSv1.2|TLSv1.3`
//! (rabbitmq), `spring.rabbitmq.ssl.enabled=true`, or `sslContextParameters=#bean` /
//! `useGlobalSslContextParameters=true`. Trust comes from the parameters, the global
//! `camel.ssl.*` ones, else the system trust store. Client certificates are not supported.
//!
//! **Consumers** declare (when `autoDeclare` / `declare`) the exchange, the queue(s) and the
//! binding (plus the dead letter exchange/queue), set `basic.qos` and consume with
//! `concurrentConsumers` channels. Messages become exchanges with the AMQP headers as
//! Camel headers and the properties as `CamelRabbitmq*` headers, plus
//! `CamelRabbitmqRoutingKey`, `CamelRabbitmqExchangeName`, `CamelRabbitmqDeliveryTag`,
//! `CamelRabbitmqRedeliveryTag` (and `CamelSpringRabbitmqDeliveryTag` for spring). A
//! message with a `replyTo` property makes the exchange InOut and the result is published
//! to the default exchange with routing key = replyTo and the same correlation id (a failed
//! exchange replies with the `CamelExceptionKind`/`CamelExceptionMessage` headers).
//! Acknowledgement: `NONE`/`autoAck=true` uses broker auto-ack; otherwise Camel acks after
//! a successful exchange and rejects a failed one — spring: after `maximumRetryAttempts`
//! (AUTO mode, `retryDelay` apart), requeueing unless `rejectAndDontRequeue`; legacy:
//! requeue only when the exchange carries header `CamelRabbitmqRequeue=true`. Rejected
//! messages go to the dead letter exchange when the queue has one.
//!
//! **Producers** publish the body (bytes; strings as UTF-8 with `text/plain`, maps/lists as
//! JSON with `application/json` for spring) to the exchange with the routing key
//! (overridable by `CamelSpringRabbitmqRoutingOverrideKey` /
//! `CamelSpringRabbitmqExchangeOverrideName`, or for `rabbitmq` by `CamelRabbitmqRoutingKey`,
//! `CamelRabbitmqExchangeName` unless `bridgeEndpoint`, and `CamelRabbitmqExchangeOverrideName`).
//! Non-`Camel*` headers go into the AMQP header table, `CamelRabbitmq*` headers into the
//! message properties. InOut exchanges do request/reply over a temporary exclusive reply
//! queue with a correlation id (`replyTimeout` / `requestTimeout`, header
//! `CamelRabbitmqRequestTimeout`); the reply body and headers replace the message.
//! Publisher confirms are waited for with `confirm=enabled` / `publisherAcknowledgements`.

use std::any::Any;
use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::support::{BackgroundThreads, StopFlag};
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{kinds, CamelError, Result};
use camel_core::message::{Exchange, ExchangePattern};
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

use super::client::{AmqpConfig, Channel, Connection, Delivery, ExchangeOptions, QueueOptions};
use super::codec::{BasicProperties, Table};
use crate::common::{broker_url_is_tls, parse_broker_url, split_host_port, take_ignored, unique_id, COMMON_IGNORED};

/// Header names (Camel `RabbitMQConstants` / `SpringRabbitMQConstants`).
pub mod headers {
    pub const ROUTING_KEY: &str = "CamelRabbitmqRoutingKey";
    pub const EXCHANGE_OVERRIDE_NAME: &str = "CamelRabbitmqExchangeOverrideName";
    pub const EXCHANGE_NAME: &str = "CamelRabbitmqExchangeName";
    pub const CONTENT_TYPE: &str = "CamelRabbitmqContentType";
    pub const PRIORITY: &str = "CamelRabbitmqPriority";
    pub const DELIVERY_TAG: &str = "CamelRabbitmqDeliveryTag";
    pub const REDELIVERY_TAG: &str = "CamelRabbitmqRedeliveryTag";
    pub const CORRELATION_ID: &str = "CamelRabbitmqCorrelationId";
    pub const MESSAGE_ID: &str = "CamelRabbitmqMessageId";
    pub const DELIVERY_MODE: &str = "CamelRabbitmqDeliveryMode";
    pub const USER_ID: &str = "CamelRabbitmqUserId";
    pub const CLUSTER_ID: &str = "CamelRabbitmqClusterId";
    pub const REQUEST_TIMEOUT: &str = "CamelRabbitmqRequestTimeout";
    pub const REPLY_TO: &str = "CamelRabbitmqReplyTo";
    pub const CONTENT_ENCODING: &str = "CamelRabbitmqContentEncoding";
    pub const TYPE: &str = "CamelRabbitmqType";
    pub const EXPIRATION: &str = "CamelRabbitmqExpiration";
    pub const TIMESTAMP: &str = "CamelRabbitmqTimestamp";
    pub const APP_ID: &str = "CamelRabbitmqAppId";
    pub const REQUEUE: &str = "CamelRabbitmqRequeue";
    pub const MESSAGE_TIMESTAMP: &str = "CamelMessageTimestamp";
    pub const SPRING_DELIVERY_TAG: &str = "CamelSpringRabbitmqDeliveryTag";
    pub const SPRING_EXCHANGE_OVERRIDE_NAME: &str = "CamelSpringRabbitmqExchangeOverrideName";
    pub const SPRING_ROUTING_OVERRIDE_KEY: &str = "CamelSpringRabbitmqRoutingOverrideKey";
    /// Reply headers describing a failed InOut exchange on the consumer side.
    pub const EXCEPTION_KIND: &str = "CamelExceptionKind";
    pub const EXCEPTION_MESSAGE: &str = "CamelExceptionMessage";
}

/// Which Camel component flavour an endpoint follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// `spring-rabbitmq`
    Spring,
    /// `rabbitmq`
    Legacy,
}

/// Consumer acknowledge mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckMode {
    /// Camel acks after successful processing, rejects on failure (after retries).
    Auto,
    /// Camel acks / rejects explicitly after processing (no retries).
    Manual,
    /// Broker auto-ack (`no-ack`).
    None,
}

/// Resolved endpoint configuration.
#[derive(Clone, Debug)]
pub struct RabbitConfig {
    pub flavor: Flavor,
    pub conn: AmqpConfig,
    pub exchange: String,
    pub exchange_type: String,
    pub routing_key: String,
    pub queues: Vec<String>,
    pub auto_declare: bool,
    pub auto_declare_producer: bool,
    pub durable: bool,
    pub auto_delete: bool,
    pub exclusive: bool,
    pub exclusive_consumer: bool,
    pub concurrent_consumers: usize,
    pub ack_mode: AckMode,
    pub prefetch_count: u16,
    pub prefetch_global: bool,
    pub dead_letter_exchange: Option<String>,
    pub dead_letter_exchange_type: String,
    pub dead_letter_queue: Option<String>,
    pub dead_letter_routing_key: Option<String>,
    pub reject_and_dont_requeue: bool,
    pub maximum_retry_attempts: u32,
    pub retry_delay: Duration,
    pub reply_timeout: Duration,
    pub disable_reply_to: bool,
    pub confirm: bool,
    pub confirm_timeout: Duration,
    pub mandatory: bool,
    pub guaranteed_deliveries: bool,
    pub bridge_endpoint: bool,
    pub allow_null_body: bool,
    pub skip_queue_declare: bool,
    pub skip_queue_bind: bool,
    pub skip_exchange_declare: bool,
    pub queue_args: Table,
    pub exchange_args: Table,
    pub binding_args: Table,
    pub recovery_interval: Duration,
}

fn parse_arg_value(s: &str) -> Value {
    if let Ok(i) = s.parse::<i64>() {
        Value::Int(i)
    } else if s == "true" || s == "false" {
        Value::Bool(s == "true")
    } else {
        Value::String(s.to_string())
    }
}

/// Lookup an option from the uri, then `camel.component.<scheme>.<key>`.
fn opt(params: &mut Parameters, ctx: &CamelContext, scheme: &str, key: &str) -> Option<String> {
    params.take(key).or_else(|| ctx.properties().get(&format!("camel.component.{}.{}", scheme, key)))
}

fn parse_bool(key: &str, v: Option<String>, default: bool) -> Result<bool> {
    match v.as_deref().map(str::trim) {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(o) => Err(CamelError::illegal_argument(format!("Invalid boolean for {}: {}", key, o))),
    }
}

fn parse_num<T: std::str::FromStr>(key: &str, v: Option<String>, default: T) -> Result<T> {
    match v {
        None => Ok(default),
        Some(s) => s.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid number for {}: {}", key, s))),
    }
}

fn parse_ms(key: &str, v: Option<String>, default: Duration) -> Result<Duration> {
    match v {
        None => Ok(default),
        Some(s) => match s.trim().parse::<i64>() {
            Ok(n) => Ok(Duration::from_millis(n.max(0) as u64)),
            Err(_) => camel_core::util::parse_duration(s.trim()).map_err(|_| CamelError::illegal_argument(format!("Invalid duration for {}: {}", key, s))),
        },
    }
}

impl RabbitConfig {
    fn from_uri(flavor: Flavor, scheme: &str, ctx: &CamelContext, remaining: &str, params: &mut Parameters) -> Result<RabbitConfig> {
        let spring = flavor == Flavor::Spring;
        let mut conn = AmqpConfig::default();
        let mut exchange = remaining.to_string();
        // legacy Camel 2 style: rabbitmq://host:port/exchange
        if !spring {
            if let Some(i) = remaining.find('/') {
                let (h, p) = split_host_port(&remaining[..i], 5672)?;
                conn.host = h;
                conn.port = p;
                exchange = remaining[i + 1..].to_string();
            }
        }
        let props = ctx.properties();
        let mut tls_wanted = false;
        let mut tls_version = None;
        let mut o = |k: &str| opt(params, ctx, scheme, k);
        if let Some(v) = o("sslProtocol") {
            let v = v.trim();
            if !v.eq_ignore_ascii_case("false") && !v.is_empty() {
                tls_wanted = true;
                tls_version = camel_tls::params::parse_tls_version(v);
            }
        }
        if spring && props.get("spring.rabbitmq.ssl.enabled").map(|v| v.trim().eq_ignore_ascii_case("true")).unwrap_or(false) {
            tls_wanted = true;
        }
        let _ = o("trustManager");
        if spring {
            let host = o("host").or_else(|| props.get("spring.rabbitmq.host"));
            let port = o("port").or_else(|| props.get("spring.rabbitmq.port"));
            let addresses = o("addresses").or_else(|| props.get("spring.rabbitmq.addresses"));
            if let Some(a) = addresses {
                let first = a.split(',').next().unwrap_or("").trim().to_string();
                tls_wanted |= broker_url_is_tls(&first);
                let (h, p) = parse_broker_url(&first, if broker_url_is_tls(&first) { 5671 } else { 5672 })?;
                conn.host = h;
                conn.port = p;
            }
            if let Some(h) = host {
                conn.host = h;
            }
            conn.port = parse_num("port", port, conn.port)?;
            if let Some(u) = o("username").or_else(|| props.get("spring.rabbitmq.username")) {
                conn.username = u;
            }
            if let Some(p) = o("password").or_else(|| props.get("spring.rabbitmq.password")) {
                conn.password = p;
            }
            if let Some(v) = o("vhost").or_else(|| props.get("spring.rabbitmq.virtual-host")) {
                conn.vhost = v;
            }
        } else {
            if let Some(a) = o("addresses") {
                let first = a.split(',').next().unwrap_or("").trim().to_string();
                let (h, p) = split_host_port(&first, 5672)?;
                conn.host = h;
                conn.port = p;
            }
            if let Some(h) = o("hostname") {
                conn.host = h;
            }
            conn.port = parse_num("portNumber", o("portNumber"), conn.port)?;
            if let Some(u) = o("username") {
                conn.username = u;
            }
            if let Some(p) = o("password") {
                conn.password = p;
            }
            if let Some(v) = o("vhost") {
                conn.vhost = v;
            }
        }
        drop(o);
        if let Some(mut t) = camel_tls::camel_support::endpoint_tls(ctx, params, scheme, tls_wanted, &[])? {
            if let Some(v) = tls_version {
                let mut p = (*t.params).clone();
                p.min_version = Some(v);
                p.max_version = Some(v);
                t = camel_tls::camel_support::EndpointTls::new(std::sync::Arc::new(p));
            }
            conn.tls = Some(t);
        }
        let mut o = |k: &str| opt(params, ctx, scheme, k);
        conn.heartbeat = parse_num("requestedHeartbeat", o("requestedHeartbeat"), 60u16)?;
        conn.frame_max = parse_num("requestedFrameMax", o("requestedFrameMax"), conn.frame_max)?;
        conn.channel_max = parse_num("requestedChannelMax", o("requestedChannelMax"), conn.channel_max)?;
        conn.connect_timeout = parse_ms("connectionTimeout", o("connectionTimeout"), Duration::from_secs(30))?;
        if conn.connect_timeout.is_zero() {
            conn.connect_timeout = Duration::from_secs(30);
        }
        if spring && (exchange == "default" || exchange.is_empty()) {
            exchange = String::new();
        }
        let exchange_type = o("exchangeType").unwrap_or_else(|| "direct".into());
        if !["direct", "fanout", "topic", "headers"].contains(&exchange_type.as_str()) {
            return Err(CamelError::illegal_argument(format!("Invalid exchangeType: {}", exchange_type)));
        }
        let routing_key = o("routingKey").unwrap_or_default();
        let queues: Vec<String> = if spring { o("queues") } else { o("queue") }
            .map(|q| q.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        let auto_declare = if spring { parse_bool("autoDeclare", o("autoDeclare"), true)? } else { parse_bool("declare", o("declare"), true)? };
        let auto_declare_producer = if spring { parse_bool("autoDeclareProducer", o("autoDeclareProducer"), false)? } else { auto_declare };
        let durable = parse_bool("durable", o("durable"), true)?;
        let auto_delete = parse_bool("autoDelete", o("autoDelete"), !spring)?;
        let exclusive = parse_bool("exclusive", o("exclusive"), false)?;
        let exclusive_consumer = parse_bool("exclusiveConsumer", o("exclusiveConsumer"), false)?;
        let concurrent_consumers = parse_num("concurrentConsumers", o("concurrentConsumers"), 1usize)?.max(1);
        let ack_mode = if spring {
            match o("acknowledgeMode").map(|s| s.to_ascii_uppercase()).as_deref() {
                None | Some("AUTO") => AckMode::Auto,
                Some("MANUAL") => AckMode::Manual,
                Some("NONE") => AckMode::None,
                Some(other) => return Err(CamelError::illegal_argument(format!("Invalid acknowledgeMode: {}", other))),
            }
        } else if parse_bool("autoAck", o("autoAck"), true)? {
            AckMode::None
        } else {
            AckMode::Manual
        };
        let (prefetch_count, prefetch_global) = if spring {
            (parse_num("prefetchCount", o("prefetchCount"), 250u16)?, false)
        } else {
            let enabled = parse_bool("prefetchEnabled", o("prefetchEnabled"), false)?;
            let count = parse_num("prefetchCount", o("prefetchCount"), 0u16)?;
            let global = parse_bool("prefetchGlobal", o("prefetchGlobal"), false)?;
            o("prefetchSize");
            (if enabled { count } else { 0 }, global)
        };
        let dead_letter_exchange = o("deadLetterExchange").filter(|s| !s.is_empty());
        let dead_letter_exchange_type = o("deadLetterExchangeType").unwrap_or_else(|| "direct".into());
        let dead_letter_queue = o("deadLetterQueue").filter(|s| !s.is_empty());
        let dead_letter_routing_key = o("deadLetterRoutingKey").filter(|s| !s.is_empty());
        let reject_and_dont_requeue = parse_bool("rejectAndDontRequeue", o("rejectAndDontRequeue"), true)?;
        let maximum_retry_attempts = parse_num("maximumRetryAttempts", o("maximumRetryAttempts"), 5u32)?;
        let retry_delay = parse_ms("retryDelay", o("retryDelay"), Duration::from_millis(1000))?;
        let reply_timeout = if spring {
            parse_ms("replyTimeout", o("replyTimeout"), Duration::from_millis(30_000))?
        } else {
            parse_ms("requestTimeout", o("requestTimeout"), Duration::from_millis(20_000))?
        };
        let disable_reply_to = parse_bool("disableReplyTo", o("disableReplyTo"), false)?;
        let (confirm, confirm_timeout) = if spring {
            let c = match o("confirm").as_deref() {
                None | Some("auto") | Some("disabled") => false,
                Some("enabled") => true,
                Some(other) => return Err(CamelError::illegal_argument(format!("Invalid confirm: {}", other))),
            };
            (c, parse_ms("confirmTimeout", o("confirmTimeout"), Duration::from_millis(5000))?)
        } else {
            let c = parse_bool("publisherAcknowledgements", o("publisherAcknowledgements"), false)?;
            (c, parse_ms("publisherAcknowledgementsTimeout", o("publisherAcknowledgementsTimeout"), Duration::from_millis(5000))?)
        };
        let guaranteed_deliveries = parse_bool("guaranteedDeliveries", o("guaranteedDeliveries"), false)?;
        let mandatory = parse_bool("mandatory", o("mandatory"), false)?;
        let bridge_endpoint = parse_bool("bridgeEndpoint", o("bridgeEndpoint"), false)?;
        let allow_null_body = parse_bool("allowNullBody", o("allowNullBody"), false)?;
        let skip_queue_declare = parse_bool("skipQueueDeclare", o("skipQueueDeclare").or_else(|| o("skipDeclareQueue")), false)?;
        let skip_queue_bind = parse_bool("skipQueueBind", o("skipQueueBind").or_else(|| o("skipBindQueue")), false)?;
        let skip_exchange_declare = parse_bool("skipExchangeDeclare", o("skipExchangeDeclare").or_else(|| o("skipDeclareExchange")), false)?;
        let recovery_interval = parse_ms("recoveryInterval", o("recoveryInterval").or_else(|| o("networkRecoveryInterval")), Duration::from_millis(5000))?;
        let (mut queue_args, mut exchange_args, mut binding_args) = (Table::new(), Table::new(), Table::new());
        for (k, v) in params.take_prefixed("arg.") {
            let (target, name) = match k.split_once('.') {
                Some(("queue", n)) => (&mut queue_args, n),
                Some(("exchange", n)) => (&mut exchange_args, n),
                Some(("binding", n)) => (&mut binding_args, n),
                _ => return Err(CamelError::illegal_argument(format!("Invalid arg option: arg.{} (use arg.queue., arg.exchange. or arg.binding.)", k))),
            };
            target.insert(name.to_string(), parse_arg_value(&v));
        }
        take_ignored(params, COMMON_IGNORED);
        take_ignored(
            params,
            &[
                "connectionFactory",
                "maxConcurrentConsumers",
                "messageConverter",
                "messagePropertiesConverter",
                "headerFilterStrategy",
                "testConnectionOnStartup",
                "usePublisherConnection",
                "messageListenerContainerType",
                "noLocal",
                "retry",
                "replyToType",
                "automaticRecoveryEnabled",
                "topologyRecoveryEnabled",
                "threadPoolSize",
                "transferException",
                "allowCustomHeaders",
                "allowMessageBodySerialization",
                "clientProperties",
                "lazyCreateConnection",
                "channelPoolMaxSize",
                "channelPoolMaxWait",
                "passive",
                "inOut",
            ],
        );
        Ok(RabbitConfig {
            flavor,
            conn,
            exchange,
            exchange_type,
            routing_key,
            queues,
            auto_declare,
            auto_declare_producer,
            durable,
            auto_delete,
            exclusive,
            exclusive_consumer,
            concurrent_consumers,
            ack_mode,
            prefetch_count,
            prefetch_global,
            dead_letter_exchange,
            dead_letter_exchange_type,
            dead_letter_queue,
            dead_letter_routing_key,
            reject_and_dont_requeue,
            maximum_retry_attempts,
            retry_delay,
            reply_timeout,
            disable_reply_to,
            confirm: confirm || guaranteed_deliveries,
            confirm_timeout,
            mandatory: mandatory || guaranteed_deliveries,
            guaranteed_deliveries,
            bridge_endpoint,
            allow_null_body,
            skip_queue_declare,
            skip_queue_bind,
            skip_exchange_declare,
            queue_args,
            exchange_args,
            binding_args,
            recovery_interval,
        })
    }

    fn declare_exchange(&self, ch: &Channel) -> Result<()> {
        if self.exchange.is_empty() || self.skip_exchange_declare {
            return Ok(());
        }
        let auto_delete = self.flavor == Flavor::Legacy && self.auto_delete;
        ch.exchange_declare(&self.exchange, &self.exchange_type, &ExchangeOptions { durable: self.durable, auto_delete, arguments: self.exchange_args.clone(), ..Default::default() })
    }

    /// Declare exchange, dead letter topology, queues and bindings. Returns the queues to consume.
    fn declare_consumer_topology(&self, ch: &Channel) -> Result<Vec<String>> {
        let declare = self.auto_declare;
        if declare {
            self.declare_exchange(ch)?;
        }
        let mut qargs = self.queue_args.clone();
        if let Some(dlx) = &self.dead_letter_exchange {
            qargs.insert("x-dead-letter-exchange".into(), Value::from(dlx.as_str()));
            if let Some(k) = &self.dead_letter_routing_key {
                qargs.insert("x-dead-letter-routing-key".into(), Value::from(k.as_str()));
            }
            if declare {
                ch.exchange_declare(dlx, &self.dead_letter_exchange_type, &ExchangeOptions { durable: self.durable, ..Default::default() })?;
                if let Some(dlq) = &self.dead_letter_queue {
                    ch.queue_declare(dlq, &QueueOptions { durable: self.durable, ..Default::default() })?;
                    let key = self.dead_letter_routing_key.clone().unwrap_or_else(|| self.routing_key.clone());
                    ch.queue_bind(dlq, dlx, &key, &Table::new())?;
                }
            }
        }
        let mut names = self.queues.clone();
        if names.is_empty() {
            // anonymous queue (exclusive, auto-delete, server named)
            let info = ch.queue_declare("", &QueueOptions { exclusive: true, auto_delete: true, arguments: qargs.clone(), ..Default::default() })?;
            names.push(info.name);
            if !self.exchange.is_empty() {
                ch.queue_bind(&names[0], &self.exchange, &self.routing_key, &self.binding_args)?;
            }
            return Ok(names);
        }
        if declare {
            let auto_delete = self.flavor == Flavor::Legacy && self.auto_delete;
            for q in &names {
                if !self.skip_queue_declare {
                    ch.queue_declare(q, &QueueOptions { durable: self.durable, exclusive: self.exclusive, auto_delete, arguments: qargs.clone(), ..Default::default() })?;
                }
                if !self.skip_queue_bind && !self.exchange.is_empty() {
                    ch.queue_bind(q, &self.exchange, &self.routing_key, &self.binding_args)?;
                }
            }
        }
        Ok(names)
    }
}

/// The `spring-rabbitmq` / `rabbitmq` component.
pub struct RabbitComponent {
    flavor: Flavor,
}

impl RabbitComponent {
    /// The `spring-rabbitmq` component.
    pub fn spring() -> Self {
        RabbitComponent { flavor: Flavor::Spring }
    }
    /// The `rabbitmq` component.
    pub fn legacy() -> Self {
        RabbitComponent { flavor: Flavor::Legacy }
    }
}

impl Component for RabbitComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let scheme = match self.flavor {
            Flavor::Spring => "spring-rabbitmq",
            Flavor::Legacy => "rabbitmq",
        };
        let cfg = RabbitConfig::from_uri(self.flavor, scheme, ctx, remaining, params)?;
        Ok(Arc::new(RabbitEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg: Arc::new(cfg) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `spring-rabbitmq` / `rabbitmq` endpoint.
pub struct RabbitEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: Arc<RabbitConfig>,
}

impl RabbitEndpoint {
    pub fn config(&self) -> &RabbitConfig {
        &self.cfg
    }
}

impl Endpoint for RabbitEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(RabbitProducer { cfg: self.cfg.clone(), uri: self.uri.clone(), state: Mutex::new(None) }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(RabbitConsumer { cfg: self.cfg.clone(), uri: self.uri.clone(), ctx: self.ctx.clone(), processor, threads: BackgroundThreads::new() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// message mapping
// ---------------------------------------------------------------------------

fn is_textual(content_type: Option<&str>) -> bool {
    match content_type {
        Some(ct) => {
            let ct = ct.to_ascii_lowercase();
            ct.starts_with("text/") || ct.contains("json") || ct.contains("xml")
        }
        None => false,
    }
}

/// Body of a received message: text for textual content types (valid UTF-8), else bytes.
fn body_from_delivery(d: &Delivery) -> Value {
    if is_textual(d.properties.content_type.as_deref()) {
        if let Ok(s) = String::from_utf8(d.body.clone()) {
            return Value::String(s);
        }
    }
    Value::Bytes(d.body.clone())
}

/// Copy AMQP headers and properties of a delivery onto a Camel message.
fn delivery_to_exchange(cfg: &RabbitConfig, d: &Delivery, ex: &mut Exchange) {
    use headers::*;
    ex.set_body(body_from_delivery(d));
    if let Some(h) = &d.properties.headers {
        for (k, v) in h {
            ex.set_header(k.clone(), v.clone());
        }
    }
    let p = &d.properties;
    let mut set = |k: &str, v: &Option<String>| {
        if let Some(v) = v {
            ex.set_header(k, v.clone());
        }
    };
    set(CONTENT_TYPE, &p.content_type);
    set(CONTENT_ENCODING, &p.content_encoding);
    set(CORRELATION_ID, &p.correlation_id);
    set(REPLY_TO, &p.reply_to);
    set(EXPIRATION, &p.expiration);
    set(MESSAGE_ID, &p.message_id);
    set(TYPE, &p.kind);
    set(USER_ID, &p.user_id);
    set(APP_ID, &p.app_id);
    set(CLUSTER_ID, &p.cluster_id);
    if let Some(m) = p.delivery_mode {
        ex.set_header(DELIVERY_MODE, m as i64);
    }
    if let Some(pr) = p.priority {
        ex.set_header(PRIORITY, pr as i64);
    }
    if let Some(t) = p.timestamp {
        ex.set_header(TIMESTAMP, (t * 1000) as i64);
        ex.set_header(MESSAGE_TIMESTAMP, (t * 1000) as i64);
    }
    ex.set_header(ROUTING_KEY, d.routing_key.clone());
    ex.set_header(EXCHANGE_NAME, d.exchange.clone());
    ex.set_header(DELIVERY_TAG, d.delivery_tag as i64);
    ex.set_header(REDELIVERY_TAG, d.redelivered);
    if cfg.flavor == Flavor::Spring {
        ex.set_header(SPRING_DELIVERY_TAG, d.delivery_tag as i64);
    }
}

fn header_str(ex: &Exchange, k: &str) -> Option<String> {
    ex.header(k).filter(|v| !v.is_null()).map(|v| v.to_string()).filter(|s| !s.is_empty())
}

/// Build the body bytes and properties of an outgoing message from a Camel exchange.
fn exchange_to_message(cfg: &RabbitConfig, ex: &Exchange) -> Result<(BasicProperties, Vec<u8>)> {
    use headers::*;
    let spring = cfg.flavor == Flavor::Spring;
    let mut p = BasicProperties::default();
    let mut default_ct = None;
    let body = match ex.body() {
        Value::Null => {
            if !cfg.allow_null_body {
                return Err(CamelError::new("MessageConversionException", "Cannot send message as message body is null, and option allowNullBody is false."));
            }
            Vec::new()
        }
        Value::Bytes(b) => {
            default_ct = Some("application/octet-stream");
            b.clone()
        }
        Value::String(s) => {
            default_ct = Some("text/plain");
            s.as_bytes().to_vec()
        }
        v @ (Value::Array(_) | Value::Object(_)) => {
            default_ct = Some("application/json");
            v.to_json().into_bytes()
        }
        other => {
            default_ct = Some("text/plain");
            other.to_string().into_bytes()
        }
    };
    p.content_type = header_str(ex, CONTENT_TYPE).or_else(|| if spring { default_ct.map(String::from) } else { None });
    p.content_encoding = header_str(ex, CONTENT_ENCODING).or_else(|| if spring && p.content_type.as_deref() == Some("text/plain") { Some("UTF-8".into()) } else { None });
    p.correlation_id = header_str(ex, CORRELATION_ID);
    p.reply_to = header_str(ex, REPLY_TO);
    p.expiration = header_str(ex, EXPIRATION);
    p.message_id = header_str(ex, MESSAGE_ID);
    p.kind = header_str(ex, TYPE);
    p.user_id = header_str(ex, USER_ID);
    p.app_id = header_str(ex, APP_ID);
    p.cluster_id = header_str(ex, CLUSTER_ID);
    p.delivery_mode = match header_str(ex, DELIVERY_MODE).as_deref() {
        Some("2") | Some("PERSISTENT") => Some(2),
        Some("1") | Some("NON_PERSISTENT") => Some(1),
        Some(o) => return Err(CamelError::illegal_argument(format!("Invalid {} header: {}", DELIVERY_MODE, o))),
        None if spring => Some(2),
        None => None,
    };
    p.priority = ex.header(PRIORITY).and_then(|v| v.as_i64()).map(|v| v.clamp(0, 255) as u8);
    p.timestamp = ex.header(TIMESTAMP).and_then(|v| v.as_i64()).map(|ms| (ms / 1000).max(0) as u64);
    let mut table = Table::new();
    for (k, v) in ex.message.headers.iter() {
        let lower = k.to_ascii_lowercase();
        if lower.starts_with("camel") || lower.starts_with("org.apache.camel") || lower.starts_with("jms") {
            continue;
        }
        table.insert(k.to_string(), v.clone());
    }
    if !table.is_empty() {
        p.headers = Some(table);
    }
    Ok((p, body))
}

// ---------------------------------------------------------------------------
// producer
// ---------------------------------------------------------------------------

type Pending = Arc<Mutex<HashMap<String, Sender<Delivery>>>>;

struct ReplyManager {
    queue: String,
    pending: Pending,
    stop: Arc<StopFlag>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct ProducerState {
    conn: Connection,
    channel: Channel,
    declared: bool,
    reply: Option<ReplyManager>,
}

impl ProducerState {
    fn shutdown(mut self) {
        if let Some(mut r) = self.reply.take() {
            r.stop.stop();
            if let Some(t) = r.thread.take() {
                let _ = t.join();
            }
        }
        self.conn.close();
    }
}

struct RabbitProducer {
    cfg: Arc<RabbitConfig>,
    uri: String,
    state: Mutex<Option<ProducerState>>,
}

impl RabbitProducer {
    fn ensure<'a>(&self, g: &'a mut Option<ProducerState>) -> Result<&'a mut ProducerState> {
        if g.as_ref().is_some_and(|s| !s.channel.is_open()) {
            if let Some(old) = g.take() {
                old.shutdown();
            }
        }
        if g.is_none() {
            let conn = Connection::open(&self.cfg.conn)?;
            let channel = conn.channel()?;
            if self.cfg.confirm {
                channel.confirm_select()?;
            }
            *g = Some(ProducerState { conn, channel, declared: false, reply: None });
        }
        let s = g.as_mut().unwrap();
        if !s.declared {
            if self.cfg.auto_declare_producer {
                self.cfg.declare_exchange(&s.channel)?;
                if self.cfg.flavor == Flavor::Legacy {
                    for q in &self.cfg.queues {
                        let mut qargs = self.cfg.queue_args.clone();
                        if let Some(dlx) = &self.cfg.dead_letter_exchange {
                            qargs.insert("x-dead-letter-exchange".into(), Value::from(dlx.as_str()));
                        }
                        if !self.cfg.skip_queue_declare {
                            s.channel.queue_declare(q, &QueueOptions { durable: self.cfg.durable, auto_delete: self.cfg.auto_delete, exclusive: self.cfg.exclusive, arguments: qargs, ..Default::default() })?;
                        }
                        if !self.cfg.skip_queue_bind && !self.cfg.exchange.is_empty() {
                            s.channel.queue_bind(q, &self.cfg.exchange, &self.cfg.routing_key, &self.cfg.binding_args)?;
                        }
                    }
                }
            }
            s.declared = true;
        }
        Ok(s)
    }

    fn reply_manager<'a>(&self, s: &'a mut ProducerState) -> Result<&'a ReplyManager> {
        if s.reply.is_none() {
            let ch = s.conn.channel()?;
            let info = ch.queue_declare("", &QueueOptions { exclusive: true, auto_delete: true, ..Default::default() })?;
            let sub = ch.basic_consume(&info.name, "", true, true, &Table::new())?;
            let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
            let stop = StopFlag::new();
            let (p2, s2) = (pending.clone(), stop.clone());
            let thread = std::thread::Builder::new()
                .name(format!("Camel {} reply manager", self.uri))
                .spawn(move || {
                    let _keep = ch;
                    while !s2.is_stopped() {
                        match sub.deliveries.recv_timeout(Duration::from_millis(100)) {
                            Ok(d) => {
                                let id = d.properties.correlation_id.clone().unwrap_or_default();
                                if let Some(tx) = p2.lock().unwrap().remove(&id) {
                                    let _ = tx.send(d);
                                }
                            }
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .map_err(|e| CamelError::io(e.to_string()))?;
            s.reply = Some(ReplyManager { queue: info.name, pending, stop, thread: Some(thread) });
        }
        Ok(s.reply.as_ref().unwrap())
    }

    fn publish(&self, exchange: &mut Exchange) -> Result<()> {
        use headers::*;
        let cfg = &self.cfg;
        let spring = cfg.flavor == Flavor::Spring;
        let mut ex_name = cfg.exchange.clone();
        let mut rk = cfg.routing_key.clone();
        if spring {
            if let Some(o) = header_str(exchange, SPRING_EXCHANGE_OVERRIDE_NAME) {
                ex_name = if o == "default" { String::new() } else { o };
            }
            if let Some(k) = header_str(exchange, SPRING_ROUTING_OVERRIDE_KEY) {
                rk = k;
            }
        } else {
            if !cfg.bridge_endpoint {
                if let Some(k) = header_str(exchange, ROUTING_KEY) {
                    rk = k;
                }
                if let Some(e) = header_str(exchange, EXCHANGE_NAME) {
                    ex_name = e;
                }
            }
            if let Some(o) = header_str(exchange, EXCHANGE_OVERRIDE_NAME) {
                ex_name = o;
            }
        }
        let (mut props, body) = exchange_to_message(cfg, exchange)?;
        let in_out = exchange.is_out_capable() && !cfg.disable_reply_to;
        let mut g = self.state.lock().unwrap();
        let s = match self.ensure(&mut g) {
            Ok(s) => s,
            Err(e) => {
                if let Some(old) = g.take() {
                    old.shutdown();
                }
                return Err(e);
            }
        };
        let mandatory = cfg.mandatory || exchange.header("CamelRabbitmqMandatory").and_then(|v| v.as_bool()).unwrap_or(false);
        if !in_out {
            s.channel.basic_publish(&ex_name, &rk, mandatory, &props, &body)?;
            if cfg.confirm {
                let ok = s.channel.wait_for_confirms(cfg.confirm_timeout)?;
                if !ok {
                    return Err(CamelError::new("ConfirmNackException", format!("Message was nacked by the broker: exchange={} routingKey={}", ex_name, rk)));
                }
                let returned = s.channel.take_returned();
                if cfg.guaranteed_deliveries && !returned.is_empty() {
                    return Err(CamelError::new(
                        kinds::CAMEL_EXCHANGE,
                        format!("Message returned by the broker: {} {} (exchange={} routingKey={})", returned[0].reply_code, returned[0].reply_text, ex_name, rk),
                    ));
                }
            }
            return Ok(());
        }
        // request/reply
        let timeout = exchange.header(REQUEST_TIMEOUT).and_then(|v| v.as_i64()).map(|ms| Duration::from_millis(ms.max(1) as u64)).unwrap_or(cfg.reply_timeout);
        let correlation = props.correlation_id.clone().unwrap_or_else(unique_id);
        props.correlation_id = Some(correlation.clone());
        let (tx, rx) = mpsc::channel();
        let pending = {
            let rm = self.reply_manager(s)?;
            props.reply_to = Some(rm.queue.clone());
            rm.pending.lock().unwrap().insert(correlation.clone(), tx);
            rm.pending.clone()
        };
        if let Err(e) = s.channel.basic_publish(&ex_name, &rk, mandatory, &props, &body) {
            pending.lock().unwrap().remove(&correlation);
            return Err(e);
        }
        drop(g);
        let reply = match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(_) => {
                pending.lock().unwrap().remove(&correlation);
                return Err(CamelError::new(
                    kinds::EXCHANGE_TIMED_OUT,
                    format!("Exchange timed out after waiting: {} millis. Exchange[{}] (no reply received for correlation id {})", timeout.as_millis(), exchange.exchange_id, correlation),
                ));
            }
        };
        let failure = reply.properties.headers.as_ref().and_then(|h| h.get(EXCEPTION_KIND)).map(|v| v.to_string());
        if let Some(kind) = failure {
            let msg = reply.properties.headers.as_ref().and_then(|h| h.get(EXCEPTION_MESSAGE)).map(|v| v.to_string()).unwrap_or_default();
            return Err(CamelError::new(kind, msg));
        }
        exchange.message.headers.clear();
        delivery_to_exchange(cfg, &reply, exchange);
        Ok(())
    }
}

impl Service for RabbitProducer {
    fn stop(&self) -> Result<()> {
        if let Some(s) = self.state.lock().unwrap().take() {
            s.shutdown();
        }
        Ok(())
    }
}

impl Processor for RabbitProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self.publish(exchange)
    }
}

// ---------------------------------------------------------------------------
// consumer
// ---------------------------------------------------------------------------

struct RabbitConsumer {
    cfg: Arc<RabbitConfig>,
    uri: String,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    threads: BackgroundThreads,
}

struct Worker {
    cfg: Arc<RabbitConfig>,
    reply_cfg: RabbitConfig,
    uri: String,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
}

impl Worker {
    fn handle(&self, ch: &Channel, d: Delivery, stop: &StopFlag) {
        let cfg = &self.cfg;
        let mut ex = Exchange::new(self.ctx.upgrade());
        delivery_to_exchange(cfg, &d, &mut ex);
        let reply_to = d.properties.reply_to.clone().filter(|_| !cfg.disable_reply_to);
        if reply_to.is_some() {
            ex.pattern = ExchangePattern::InOut;
        }
        let original = ex.clone();
        let attempts = if cfg.flavor == Flavor::Spring && cfg.ack_mode == AckMode::Auto { cfg.maximum_retry_attempts.max(1) } else { 1 };
        let mut attempt = 1;
        loop {
            if let Err(e) = self.processor.process(&mut ex) {
                ex.set_exception(e);
            }
            if ex.exception.is_none() || attempt >= attempts || stop.is_stopped() {
                break;
            }
            attempt += 1;
            if stop.sleep(cfg.retry_delay) {
                break;
            }
            ex = original.clone();
        }
        let failed = ex.exception.is_some();
        if let Some(e) = &ex.exception {
            camel_core::camel_warn!(&self.uri, "Error processing exchange. Exchange[{}]. Caused by: [{} - {}]", ex.exchange_id, e.kind, e.message);
        }
        if let Some(rt) = reply_to {
            let mut out = ex.clone();
            out.exception = None;
            let result = exchange_to_message(&self.reply_cfg, &out);
            match result {
                Ok((mut props, body)) => {
                    props.correlation_id = d.properties.correlation_id.clone();
                    props.reply_to = None;
                    let mut h = props.headers.take().unwrap_or_default();
                    if let Some(e) = &ex.exception {
                        h.insert(headers::EXCEPTION_KIND.into(), Value::from(e.kind.as_str()));
                        h.insert(headers::EXCEPTION_MESSAGE.into(), Value::from(e.message.as_str()));
                    }
                    if !h.is_empty() {
                        props.headers = Some(h);
                    }
                    if let Err(e) = ch.basic_publish("", &rt, false, &props, &body) {
                        camel_core::camel_warn!(&self.uri, "Cannot send reply to {}: {}", rt, e.message);
                    }
                }
                Err(e) => camel_core::camel_warn!(&self.uri, "Cannot build reply message: {}", e.message),
            }
        }
        if cfg.ack_mode == AckMode::None {
            return;
        }
        let r = if !failed {
            ch.basic_ack(d.delivery_tag, false)
        } else {
            let requeue = match cfg.flavor {
                Flavor::Spring => !cfg.reject_and_dont_requeue,
                Flavor::Legacy => ex.header(headers::REQUEUE).and_then(|v| v.as_bool()).unwrap_or(false),
            };
            ch.basic_reject(d.delivery_tag, requeue)
        };
        if let Err(e) = r {
            camel_core::camel_warn!(&self.uri, "Cannot acknowledge delivery {}: {}", d.delivery_tag, e.message);
        }
    }
}

/// Connect and subscribe; returns the connection and `(channel, subscription)` pairs.
fn setup(cfg: &RabbitConfig) -> Result<(Connection, Vec<(Channel, super::client::Subscription)>)> {
    let conn = Connection::open(&cfg.conn)?;
    let first = conn.channel()?;
    let queues = cfg.declare_consumer_topology(&first)?;
    let mut subs = Vec::new();
    let no_ack = cfg.ack_mode == AckMode::None;
    for i in 0..cfg.concurrent_consumers {
        let ch = if i == 0 { first.clone() } else { conn.channel()? };
        if cfg.prefetch_count > 0 && !no_ack {
            ch.basic_qos(cfg.prefetch_count, cfg.prefetch_global)?;
        }
        for q in &queues {
            let sub = ch.basic_consume(q, "", no_ack, cfg.exclusive_consumer, &Table::new())?;
            subs.push((ch.clone(), sub));
        }
    }
    Ok((conn, subs))
}

impl Service for RabbitConsumer {
    fn start(&self) -> Result<()> {
        if self.threads.is_running() {
            return Ok(());
        }
        let worker = Arc::new(Worker { cfg: self.cfg.clone(), reply_cfg: RabbitConfig { allow_null_body: true, ..(*self.cfg).clone() }, uri: self.uri.clone(), ctx: self.ctx.clone(), processor: self.processor.clone() });
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let uri = self.uri.clone();
        self.threads.spawn(&format!("Camel {} supervisor", self.uri), move |stop| {
            let mut ready = Some(ready_tx);
            while !stop.is_stopped() {
                let (conn, subs) = match setup(&worker.cfg) {
                    Ok(x) => x,
                    Err(e) => {
                        camel_core::camel_warn!(&uri, "Cannot start consumer on {}: {} - retrying in {} ms", uri, e.message, worker.cfg.recovery_interval.as_millis());
                        if let Some(tx) = ready.take() {
                            let _ = tx.send(Err(e));
                        }
                        if stop.sleep(worker.cfg.recovery_interval) {
                            break;
                        }
                        continue;
                    }
                };
                if let Some(tx) = ready.take() {
                    let _ = tx.send(Ok(()));
                }
                let tags: Vec<(Channel, String)> = subs.iter().map(|(c, s)| (c.clone(), s.consumer_tag.clone())).collect();
                std::thread::scope(|scope| {
                    for (ch, sub) in subs {
                        let (w, st) = (worker.clone(), stop.clone());
                        scope.spawn(move || loop {
                            if st.is_stopped() {
                                break;
                            }
                            match sub.deliveries.recv_timeout(Duration::from_millis(50)) {
                                Ok(d) => w.handle(&ch, d, &st),
                                Err(RecvTimeoutError::Timeout) => {}
                                Err(RecvTimeoutError::Disconnected) => break,
                            }
                        });
                    }
                });
                let lost = !stop.is_stopped();
                for (ch, tag) in &tags {
                    if !lost {
                        let _ = ch.basic_cancel(tag);
                    }
                }
                conn.close();
                if lost {
                    camel_core::camel_warn!(&uri, "Connection lost on {} - reconnecting in {} ms", uri, worker.cfg.recovery_interval.as_millis());
                    if stop.sleep(worker.cfg.recovery_interval) {
                        break;
                    }
                }
            }
        })?;
        // wait for the first subscription so that messages sent right after start are consumed
        if let Ok(Err(e)) = ready_rx.recv_timeout(self.cfg.conn.connect_timeout + Duration::from_secs(5)) {
            camel_core::camel_warn!(&self.uri, "Consumer {} not connected yet: {}", self.uri, e.message);
        }
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        self.threads.stop();
        Ok(())
    }
}
