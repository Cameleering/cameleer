//! Span decorators (Camel `SpanDecorator` of `camel-tracing`): the name, kind and attributes
//! of the span of an endpoint, per component.
//!
//! Names, kinds and attributes follow `camel-opentelemetry2` (Camel 4.11+, the module Camel
//! recommends; `camel-opentelemetry` is deprecated since 4.19):
//!
//! - every span: `component` = `camel-<scheme>`, `url.scheme`, `camel.uri` (secrets masked),
//!   `url.path`, `url.query`, `camel.route.id`, `op` (`EVENT_RECEIVED` or `EVENT_SENT`),
//!   `exchangeId`;
//! - HTTP components: name = the HTTP method; kinds `SERVER`/`CLIENT`; `http.method`,
//!   `http.url`, and after the call `http.status_code`;
//! - messaging components: name = the destination (topic, queue); kinds `CONSUMER`/`PRODUCER`;
//!   `messaging.destination.name`, `messaging.message.id`, and Kafka's `kafka.partition`,
//!   `kafka.offset`, `kafka.key`, `kafka.partition.key`;
//! - internal components (`direct`, `seda`, ...): name = the URI without scheme and options;
//! - any other component: name = the scheme. Both of these are `INTERNAL`.

use crate::message::Exchange;
use crate::value::Value;

use super::{AttrValue, OpenSpan, SpanKind, SpanStart};

/// Whether the span is for receiving (route input) or sending (a producer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Consumer,
    Producer,
}

/// The decorator family of a component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Http,
    Messaging,
    Internal,
    Other,
}

const HTTP: &[&str] = &["http", "https", "platform-http", "netty-http", "jetty", "undertow", "servlet", "vertx-http", "rest", "rest-openapi", "webhook", "knative"];
const MESSAGING: &[&str] = &[
    "kafka", "jms", "activemq", "activemq6", "amqp", "sjms", "sjms2", "rabbitmq", "spring-rabbitmq", "paho", "paho-mqtt5", "mqtt", "stomp", "nats", "pulsar", "aws2-sqs", "aws2-sns",
    "aws2-kinesis", "azure-servicebus", "azure-eventhubs", "azure-storage-queue", "google-pubsub", "ironmq", "nsq", "jt400", "cometd", "hazelcast-queue", "hazelcast-topic", "springrabbit",
];
const INTERNAL: &[&str] = &["direct", "direct-vm", "seda", "vm", "disruptor", "disruptor-vm", "controlbus", "stub"];

/// The scheme of an endpoint URI.
pub fn scheme(uri: &str) -> &str {
    uri.split(':').next().unwrap_or(uri)
}

pub fn family(scheme: &str) -> Family {
    if HTTP.contains(&scheme) {
        Family::Http
    } else if MESSAGING.contains(&scheme) {
        Family::Messaging
    } else if INTERNAL.contains(&scheme) {
        Family::Internal
    } else {
        Family::Other
    }
}

/// The URI without scheme and options (Camel `stripSchemeAndOptions`).
pub fn strip_scheme_and_options(uri: &str) -> &str {
    let rest = uri.split_once(':').map(|(_, r)| r).unwrap_or(uri);
    let rest = rest.trim_start_matches("//");
    rest.split('?').next().unwrap_or(rest)
}

fn header(ex: &Exchange, k: &str) -> Option<String> {
    match ex.header(k)? {
        Value::Null => None,
        Value::Bytes(b) => String::from_utf8(b.clone()).ok(),
        v => Some(v.to_string()),
    }
}

/// The messaging destination of an exchange at an endpoint.
fn destination(scheme: &str, uri: &str, ex: &Exchange) -> String {
    let from_header = match scheme {
        "kafka" => header(ex, "kafka.TOPIC").or_else(|| header(ex, "kafka.OVERRIDE_TOPIC")),
        "jms" | "activemq" | "activemq6" | "sjms" | "sjms2" | "amqp" => header(ex, "CamelJmsDestinationName"),
        "aws2-sqs" => None,
        _ => None,
    };
    let d = from_header.unwrap_or_else(|| strip_scheme_and_options(uri).to_string());
    // jms:queue:orders, amqp:topic:prices
    match d.split_once(':') {
        Some(("queue" | "topic" | "temp-queue" | "temp-topic", name)) if matches!(scheme, "jms" | "activemq" | "activemq6" | "sjms" | "sjms2" | "amqp") => name.to_string(),
        _ => d,
    }
}

/// Camel's rule: the `CamelHttpMethod` header; else GET with a query (`CamelHttpQuery` or `?`
/// in the URI); else POST when there is a body; else GET.
fn http_method(uri: &str, ex: &Exchange) -> String {
    match header(ex, "CamelHttpMethod") {
        Some(m) => m.to_ascii_uppercase(),
        None if header(ex, "CamelHttpQuery").is_some() || uri.contains('?') => "GET".into(),
        None if ex.body().is_null() => "GET".into(),
        None => "POST".into(),
    }
}

fn http_url(uri: &str, ex: &Exchange, role: Role) -> String {
    if let Some(u) = header(ex, "CamelHttpUrl") {
        return u;
    }
    if role == Role::Consumer {
        if let Some(u) = header(ex, "CamelHttpUri") {
            return u;
        }
    }
    let rest = strip_scheme_and_options(uri);
    if rest.starts_with("http://") || rest.starts_with("https://") {
        return rest.to_string();
    }
    match scheme(uri) {
        "http" | "https" => format!("{}://{}", scheme(uri), rest),
        _ => rest.to_string(),
    }
}

/// What is known about an endpoint's spans before any exchange: computed once per route or
/// producer, shared by all its spans.
pub struct Prepared {
    pub uri: String,
    pub role: Role,
    pub family: Family,
    /// `component`, `url.scheme`, `camel.uri`, `url.path`, `url.query`, `op` (and the route id
    /// of route spans).
    pub shared: std::sync::Arc<super::Attributes>,
    /// The span name when it does not depend on the exchange.
    pub fixed_name: Option<String>,
}

/// Prepares the decorator of `uri`.
pub fn prepare(uri: &str, role: Role, route_id: Option<&str>) -> Prepared {
    let s = scheme(uri);
    let fam = family(s);
    let masked = crate::component::log::mask_secrets(uri);
    let mut attributes: Vec<(String, AttrValue)> = vec![
        ("component".into(), AttrValue::String(format!("camel-{}", s))),
        ("url.scheme".into(), AttrValue::String(s.to_string())),
        ("camel.uri".into(), AttrValue::String(masked.clone())),
    ];
    let rest = masked.split_once(':').map(|(_, r)| r).unwrap_or("");
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    attributes.push(("url.path".into(), AttrValue::String(path.trim_start_matches("//").to_string())));
    if let Some(q) = query {
        attributes.push(("url.query".into(), AttrValue::String(q.to_string())));
    }
    attributes.push(("op".into(), AttrValue::from(if role == Role::Consumer { "EVENT_RECEIVED" } else { "EVENT_SENT" })));
    if let Some(r) = route_id {
        attributes.push(("camel.route.id".into(), AttrValue::String(r.to_string())));
    }
    let fixed_name = match fam {
        Family::Internal => Some(strip_scheme_and_options(uri).chars().take(250).collect()),
        Family::Other => Some(s.to_string()),
        _ => None,
    };
    Prepared { uri: uri.to_string(), role, family: fam, shared: std::sync::Arc::new(attributes), fixed_name }
}

/// The name, kind and per-exchange attributes of a new span.
pub fn dynamic(p: &Prepared, ex: &Exchange) -> (String, SpanKind, super::Attributes) {
    let (uri, role) = (p.uri.as_str(), p.role);
    let s = scheme(uri);
    let mut attributes: Vec<(String, AttrValue)> = Vec::with_capacity(4);
    attributes.push(("exchangeId".into(), AttrValue::String(ex.exchange_id.to_string())));
    let (name, kind) = match p.family {
        Family::Http => {
            let m = http_method(uri, ex);
            attributes.push(("http.method".into(), AttrValue::String(m.clone())));
            let url = http_url(uri, ex, role);
            let url = if url.contains('?') || url.contains('@') { crate::component::log::mask_secrets(&url) } else { url };
            attributes.push(("http.url".into(), AttrValue::String(url)));
            (m, if role == Role::Consumer { SpanKind::Server } else { SpanKind::Client })
        }
        Family::Messaging => {
            let d = destination(s, uri, ex);
            attributes.push(("messaging.destination.name".into(), AttrValue::String(d.clone())));
            (d, if role == Role::Consumer { SpanKind::Consumer } else { SpanKind::Producer })
        }
        Family::Internal | Family::Other => (p.fixed_name.clone().unwrap_or_default(), SpanKind::Internal),
    };
    // Camel's KafkaSpanDecorator reads these when the span starts
    if s == "kafka" {
        if let Some(k) = header(ex, "kafka.KEY") {
            attributes.push(("kafka.key".into(), AttrValue::String(k)));
        }
        if let Some(p) = ex.header("kafka.PARTITION_KEY").and_then(|v| v.as_i64()) {
            attributes.push(("kafka.partition.key".into(), AttrValue::Int(p)));
        }
        if role == Role::Consumer {
            for (h, a) in [("kafka.PARTITION", "kafka.partition"), ("kafka.OFFSET", "kafka.offset")] {
                if let Some(v) = ex.header(h).and_then(|v| v.as_i64()) {
                    attributes.push((a.into(), AttrValue::Int(v)));
                }
            }
        }
    }
    (name, kind, attributes)
}

/// Name, kind and all attributes of a new span for `uri` (prepared and per-exchange together).
pub fn start(uri: &str, role: Role, ex: &Exchange) -> SpanStart {
    let p = prepare(uri, role, None);
    let (name, kind, dynamic) = dynamic(&p, ex);
    let mut attributes = (*p.shared).clone();
    attributes.extend(dynamic);
    SpanStart { name, kind, attributes }
}

/// Attributes known once the exchange is done (status codes, offsets, message ids).
pub fn finish(p: &Prepared, ex: &Exchange, span: &mut OpenSpan) {
    let (uri, role) = (p.uri.as_str(), p.role);
    let s = scheme(uri);
    match p.family {
        Family::Http => {
            if let Some(c) = ex.header("CamelHttpResponseCode").and_then(|v| v.as_i64().or_else(|| v.to_string().parse().ok())) {
                span.set_attribute("http.status_code", c);
            }
        }
        Family::Messaging => {
            if s == "kafka" && role == Role::Producer {
                // the producer sets the location of the record it sent
                for (h, a) in [("kafka.PARTITION", "kafka.partition"), ("kafka.OFFSET", "kafka.offset")] {
                    if let Some(v) = ex.header(h).and_then(|v| v.as_i64()) {
                        span.set_attribute(a, v);
                    }
                }
            }
            let id = match s {
                "jms" | "activemq" | "activemq6" | "sjms" | "sjms2" | "amqp" => header(ex, "JMSMessageID").or_else(|| header(ex, "CamelJmsMessageId")),
                "aws2-sqs" => header(ex, "CamelAwsSqsMessageId"),
                "rabbitmq" | "spring-rabbitmq" => header(ex, "CamelRabbitmqMessageId"),
                "google-pubsub" => header(ex, "CamelGooglePubsubMessageId"),
                "azure-servicebus" => header(ex, "CamelAzureServiceBusMessageId"),
                _ => None,
            };
            if let Some(id) = id {
                span.set_attribute("messaging.message.id", id);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_kinds() {
        let mut ex = Exchange::default();
        let s = start("kafka:orders?brokers=localhost:9092", Role::Producer, &ex);
        assert_eq!((s.name.as_str(), s.kind), ("orders", SpanKind::Producer));
        let s = start("jms:queue:invoices", Role::Consumer, &ex);
        assert_eq!((s.name.as_str(), s.kind), ("invoices", SpanKind::Consumer));
        let s = start("http://example.com/x?password=secret", Role::Producer, &ex);
        assert_eq!((s.name.as_str(), s.kind), ("GET", SpanKind::Client));
        assert_eq!(s.attributes[2].1, AttrValue::from("http://example.com/x?password=xxxxxx"));
        assert_eq!(s.attributes[4].1, AttrValue::from("password=xxxxxx"));
        assert_eq!(start("http://example.com/x?a=1", Role::Producer, &Exchange::default()).name, "GET");
        let mut b = Exchange::default();
        b.message.set_body("x");
        assert_eq!(start("http://example.com/x", Role::Producer, &b).name, "POST");
        ex.set_header("CamelHttpMethod", "post");
        let s = start("platform-http:/orders", Role::Consumer, &ex);
        assert_eq!((s.name.as_str(), s.kind), ("POST", SpanKind::Server));
        let s = start("direct:a", Role::Producer, &ex);
        assert_eq!((s.name.as_str(), s.kind), ("a", SpanKind::Internal));
        let s = start("timer:tick?period=1000", Role::Consumer, &ex);
        assert_eq!((s.name.as_str(), s.kind), ("timer", SpanKind::Internal));
        assert_eq!(s.attributes[0].1, AttrValue::from("camel-timer"));
        assert_eq!(strip_scheme_and_options("http://h:1/p?q=1"), "h:1/p");
    }
}
