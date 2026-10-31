//! `paho:` (MQTT 3.1.1, Camel `camel-paho`) and `paho-mqtt5:` (MQTT 5.0, Camel
//! `camel-paho-mqtt5`) components over the built-in MQTT client.
//!
//! `paho:topic?brokerUrl=tcp://host:port&qos=2&retained=false&clientId=...&userName=...&password=...&cleanSession=true`
//!
//! - **Producer**: publishes the body (bytes as is, other values as UTF-8 text) to the
//!   endpoint topic, or to header `CamelPahoOverrideTopic` (`paho-mqtt5` also reads
//!   `CamelPahoMqtt5OverrideTopic`). Headers `CamelPahoQos` / `CamelPahoRetained` override
//!   `qos` / `retained`. QoS 1/2 sends wait for the PUBACK / PUBCOMP.
//! - **Consumer**: subscribes to the topic (wildcards `+` and `#` allowed) and routes each
//!   message with body = payload bytes and headers `CamelMqttTopic`, `CamelMqttQoS`,
//!   `CamelMqttRetained`, `CamelPahoDuplicate` and `CamelPahoMessageId` (MQTT 5 user
//!   properties become headers too). The message is acknowledged after the route
//!   completes.
//!
//! Options: `brokerUrl` (tcp://localhost:1883), `serverURIs` (first entry used),
//! `clientId` (generated), `qos` (2), `retained` (false), `userName`, `password`,
//! `cleanSession` (true; `cleanStart` for paho-mqtt5), `keepAliveInterval` (60 s),
//! `connectionTimeout` (30 s), `automaticReconnect` (true), `maxReconnectDelay` (128000 ms),
//! `willTopic`, `willPayload`, `willQos`, `willRetained`, `sessionExpiryInterval`
//! (paho-mqtt5, -1 = not sent), plus accepted-and-ignored options (`persistence`,
//! `filePersistenceDirectory`, `maxInflight`, `executorServiceTimeout`, ...).
//!
//! TLS: `ssl://host:port` (also `mqtts://`, `tls://`; default port 8883) broker urls
//! connect with TLS; trust from `sslContextParameters=#bean`,
//! `useGlobalSslContextParameters=true`, the global `camel.ssl.*` parameters, else the
//! system trust store (`httpsHostnameVerificationEnabled=false` skips the host name
//! check). `wss://` and `ws://` broker urls fail with `UnsupportedOperationException`.

pub mod client;
pub mod packet;

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::support::process_consumer_exchange;
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{kinds, CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

pub use client::{MessageHandler, MqttClient, MqttMessage, MqttOptions, MqttVersion};
use packet::{PropValue, Properties, Will};

use crate::net::{percent_decode, split_host_port, unique_id};

/// Header names (Camel `PahoConstants` / `PahoMqtt5Constants`).
pub mod headers {
    pub const MQTT_TOPIC: &str = "CamelMqttTopic";
    pub const MQTT_QOS: &str = "CamelMqttQoS";
    pub const MQTT_RETAINED: &str = "CamelMqttRetained";
    pub const PAHO_DUPLICATE: &str = "CamelPahoDuplicate";
    pub const PAHO_MESSAGE_ID: &str = "CamelPahoMessageId";
    pub const PAHO_OVERRIDE_TOPIC: &str = "CamelPahoOverrideTopic";
    pub const PAHO_MQTT5_OVERRIDE_TOPIC: &str = "CamelPahoMqtt5OverrideTopic";
    pub const PAHO_QOS: &str = "CamelPahoQos";
    pub const PAHO_RETAINED: &str = "CamelPahoRetained";
}

/// Endpoint configuration.
#[derive(Clone, Debug)]
pub struct PahoConfig {
    pub topic: String,
    pub version: MqttVersion,
    pub host: String,
    pub port: u16,
    pub client_id: Option<String>,
    pub qos: u8,
    pub retained: bool,
    pub user_name: Option<String>,
    pub password: Option<String>,
    pub clean_session: bool,
    pub keep_alive: Duration,
    pub connection_timeout: Duration,
    pub automatic_reconnect: bool,
    pub max_reconnect_delay: Duration,
    pub will: Option<Will>,
    pub session_expiry_interval: Option<u32>,
    /// TLS settings for `ssl://` broker urls.
    pub tls: Option<camel_tls::camel_support::EndpointTls>,
}

impl PahoConfig {
    /// Client options; `suffix` distinguishes generated client ids.
    pub fn options(&self, role: &str) -> MqttOptions {
        let client_id = self.client_id.clone().unwrap_or_else(|| format!("camel-{}-{}", role, unique_id()));
        let mut o = MqttOptions::new(&self.host, self.port, &client_id);
        o.version = self.version;
        o.clean_session = self.clean_session;
        o.keep_alive = self.keep_alive;
        o.username = self.user_name.clone();
        o.password = self.password.clone();
        o.will = self.will.clone();
        o.connect_timeout = self.connection_timeout;
        o.auto_reconnect = self.automatic_reconnect;
        o.max_reconnect_delay = self.max_reconnect_delay;
        o.session_expiry_interval = self.session_expiry_interval;
        o.tls = self.tls.clone();
        o
    }
}

/// The `paho` / `paho-mqtt5` component.
pub struct PahoComponent {
    version: MqttVersion,
    broker_url: Option<String>,
}

impl PahoComponent {
    pub fn new(version: MqttVersion) -> Self {
        PahoComponent { version, broker_url: None }
    }
    /// Default broker url for endpoints without `brokerUrl`.
    pub fn with_broker_url(version: MqttVersion, url: &str) -> Self {
        PahoComponent { version, broker_url: Some(url.to_string()) }
    }
}

const IGNORED: &[&str] = &[
    "lazyStartProducer",
    "bridgeErrorHandler",
    "exceptionHandler",
    "exchangePattern",
    "synchronous",
    "persistence",
    "filePersistenceDirectory",
    "maxInflight",
    "executorServiceTimeout",
    "mqttVersion",
    "client",
    "socketFactory",
    "sslClientProps",
    "sslHostnameVerifier",
    "customWebSocketHeaders",
    "receiveMaximum",
    "maximumPacketSize",
    "topicAliasMaximum",
    "willMqttProperties",
    "userProperties",
    "requestProblemInfo",
    "requestResponseInfo",
    "authMethod",
    "authData",
    "manualAcksEnabled",
];

/// Parse `tcp://host:port` or `ssl://host:port`; websocket schemes are not supported.
/// Returns `(host, port, tls)`.
pub fn parse_broker_url(url: &str) -> Result<(String, u16, bool)> {
    let (scheme, rest) = match url.find("://") {
        Some(i) => (url[..i].to_ascii_lowercase(), &url[i + 3..]),
        None => ("tcp".to_string(), url),
    };
    match scheme.as_str() {
        "tcp" | "mqtt" => {
            let (h, p) = split_host_port(rest.trim_end_matches('/'), 1883)?;
            Ok((h, p, false))
        }
        "ssl" | "mqtts" | "tls" => {
            let (h, p) = split_host_port(rest.trim_end_matches('/'), 8883)?;
            Ok((h, p, true))
        }
        "wss" | "ws" => Err(CamelError::new(
            kinds::UNSUPPORTED_OPERATION,
            format!("MQTT broker url scheme {}:// is not supported by rust-camel (no WebSocket transport; use tcp:// or ssl://): {}", scheme, url),
        )),
        other => Err(CamelError::illegal_argument(format!("Invalid MQTT broker url scheme {}: {}", other, url))),
    }
}

impl Component for PahoComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let topic = percent_decode(remaining);
        let url = match params.take("serverURIs").filter(|s| !s.is_empty()) {
            Some(list) => {
                params.take("brokerUrl");
                list.split(',').next().unwrap_or("").trim().to_string()
            }
            None => params.take("brokerUrl").or_else(|| self.broker_url.clone()).unwrap_or_else(|| "tcp://localhost:1883".to_string()),
        };
        let (host, port, secure) = parse_broker_url(&url)?;
        let hostname_verification = params.take_bool("httpsHostnameVerificationEnabled", true)?;
        let scheme = if self.version.is_v5() { "paho-mqtt5" } else { "paho" };
        let tls = match camel_tls::camel_support::endpoint_tls(ctx, params, scheme, secure, &[])? {
            Some(t) if secure => {
                let mut p = (*t.params).clone();
                if !hostname_verification {
                    p.hostname_verification = Some(false);
                }
                Some(camel_tls::camel_support::EndpointTls::new(Arc::new(p)))
            }
            _ => None,
        };
        let qos = params.take_u64("qos", 2)?;
        if qos > 2 {
            return Err(CamelError::illegal_argument(format!("Invalid qos: {} (0, 1 or 2)", qos)));
        }
        let clean = if self.version.is_v5() {
            let s = params.take_bool("cleanSession", true)?;
            params.take_bool("cleanStart", s)?
        } else {
            params.take_bool("cleanSession", true)?
        };
        let will = match params.take("willTopic").filter(|t| !t.is_empty()) {
            None => {
                params.take("willPayload");
                params.take("willQos");
                params.take("willRetained");
                None
            }
            Some(t) => Some(Will {
                topic: t,
                payload: params.take("willPayload").unwrap_or_default().into_bytes(),
                qos: params.take_u64("willQos", 1)?.min(2) as u8,
                retain: params.take_bool("willRetained", false)?,
                props: Properties::new(),
            }),
        };
        let expiry = params.take_i64("sessionExpiryInterval", -1)?;
        let cfg = PahoConfig {
            topic,
            version: self.version,
            host,
            port,
            client_id: params.take("clientId").filter(|c| !c.is_empty()),
            qos: qos as u8,
            retained: params.take_bool("retained", false)?,
            user_name: params.take("userName").filter(|c| !c.is_empty()),
            password: params.take("password"),
            clean_session: clean,
            keep_alive: Duration::from_secs(params.take_u64("keepAliveInterval", 60)?),
            connection_timeout: Duration::from_secs(params.take_u64("connectionTimeout", 30)?),
            automatic_reconnect: params.take_bool("automaticReconnect", true)?,
            max_reconnect_delay: Duration::from_millis(params.take_u64("maxReconnectDelay", 128_000)?),
            will,
            session_expiry_interval: if expiry >= 0 { Some(expiry.min(u32::MAX as i64) as u32) } else { None },
            tls,
        };
        for k in IGNORED {
            params.take(k);
        }
        Ok(Arc::new(PahoEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `paho` / `paho-mqtt5` endpoint.
pub struct PahoEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: PahoConfig,
}

impl PahoEndpoint {
    pub fn config(&self) -> &PahoConfig {
        &self.cfg
    }
}

impl Endpoint for PahoEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(PahoProducer { cfg: self.cfg.clone(), client: Mutex::new(None) }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        if self.cfg.topic.is_empty() {
            return Err(CamelError::illegal_argument("A topic is required for an MQTT consumer"));
        }
        Ok(Arc::new(PahoConsumer { cfg: self.cfg.clone(), ctx: self.ctx.clone(), processor, logger: self.uri.clone(), client: Mutex::new(None) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn header_u8(ex: &Exchange, name: &str) -> Option<u8> {
    ex.header(name).filter(|v| !v.is_null()).and_then(|v| v.as_i64().or_else(|| v.to_string().trim().parse().ok())).map(|q| q.clamp(0, 2) as u8)
}

fn header_bool(ex: &Exchange, name: &str) -> Option<bool> {
    ex.header(name).filter(|v| !v.is_null()).map(|v| v.as_bool().unwrap_or_else(|| v.to_string().eq_ignore_ascii_case("true")))
}

struct PahoProducer {
    cfg: PahoConfig,
    client: Mutex<Option<Arc<MqttClient>>>,
}

impl PahoProducer {
    fn client(&self) -> Result<Arc<MqttClient>> {
        let mut g = self.client.lock().unwrap();
        if let Some(c) = g.as_ref() {
            return Ok(c.clone());
        }
        let c = Arc::new(MqttClient::connect(self.cfg.options("producer"), None)?);
        *g = Some(c.clone());
        Ok(c)
    }
}

impl Service for PahoProducer {
    fn stop(&self) -> Result<()> {
        if let Some(c) = self.client.lock().unwrap().take() {
            c.disconnect();
        }
        Ok(())
    }
}

impl Processor for PahoProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let mut topic = ex.header(headers::PAHO_OVERRIDE_TOPIC).filter(|v| !v.is_null()).map(|v| v.to_string());
        if self.cfg.version.is_v5() {
            if let Some(t) = ex.header(headers::PAHO_MQTT5_OVERRIDE_TOPIC).filter(|v| !v.is_null()) {
                topic = Some(t.to_string());
            }
        }
        let topic = topic.unwrap_or_else(|| self.cfg.topic.clone());
        let qos = header_u8(ex, headers::PAHO_QOS).unwrap_or(self.cfg.qos);
        let retained = header_bool(ex, headers::PAHO_RETAINED).unwrap_or(self.cfg.retained);
        let payload = ex.body().to_bytes();
        let client = self.client()?;
        client.publish(&topic, &payload, qos, retained)
    }
}

struct PahoConsumer {
    cfg: PahoConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    logger: String,
    client: Mutex<Option<MqttClient>>,
}

impl Service for PahoConsumer {
    fn start(&self) -> Result<()> {
        let mut g = self.client.lock().unwrap();
        if g.is_some() {
            return Ok(());
        }
        let (ctx, processor, logger) = (self.ctx.clone(), self.processor.clone(), self.logger.clone());
        let handler: MessageHandler = Arc::new(move |m: MqttMessage| {
            let mut ex = Exchange::new(ctx.upgrade());
            ex.set_body(Value::Bytes(m.payload));
            ex.set_header(headers::MQTT_TOPIC, m.topic);
            ex.set_header(headers::MQTT_QOS, m.qos as i64);
            ex.set_header(headers::MQTT_RETAINED, m.retain);
            ex.set_header(headers::PAHO_DUPLICATE, m.dup);
            ex.set_header(headers::PAHO_MESSAGE_ID, m.pkid as i64);
            for (id, v) in &m.props.0 {
                match (*id, v) {
                    (packet::props::USER_PROPERTY, PropValue::Pair(k, v)) => ex.set_header(k.clone(), v.clone()),
                    (packet::props::CONTENT_TYPE, PropValue::Str(s)) => ex.set_header("CamelPahoMqtt5ContentType", s.clone()),
                    _ => {}
                }
            }
            process_consumer_exchange(&logger, processor.as_ref(), &mut ex);
        });
        let client = MqttClient::connect(self.cfg.options("consumer"), Some(handler))?;
        client.subscribe(&[(self.cfg.topic.clone(), self.cfg.qos)])?;
        camel_core::camel_info!("paho", "Subscribed to {} on tcp://{}:{}", self.cfg.topic, self.cfg.host, self.cfg.port);
        *g = Some(client);
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        if let Some(c) = self.client.lock().unwrap().take() {
            c.disconnect();
        }
        Ok(())
    }
}
