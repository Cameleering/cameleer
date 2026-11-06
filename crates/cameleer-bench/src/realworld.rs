//! Real-use scenarios that need other machines: an HTTP API driven over the network, HTTP into
//! Kafka, HTTP into PostgreSQL, and Kafka stream processing. `Bench.java` defines the same
//! routes with the Java DSL; keep the two in step.
//!
//! The servers listen on all interfaces and serve for a fixed time (`serve`), so a run on one
//! machine can be loaded from another without a stop signal. Where the external systems are
//! comes from the environment, the same variables on both sides:
//!
//! | variable | default | used by |
//! |---|---|---|
//! | `BENCH_KAFKA` | `127.0.0.1:9092` | `http-kafka`, `kafka-stream` (bootstrap servers) |
//! | `BENCH_PG` | `127.0.0.1` | `http-sql` (PostgreSQL host; database `bench`, user `camel`, password `camelpw`) |
//! | `BENCH_SIDE` | `rust` | topic and consumer group names, so the two sides never share them |
//! | `BENCH_RUN` | `0` | appended to consumer group names, so every run starts from the first record |

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cameleer_core::prelude::*;

use crate::driver::{self, Outcome, Plan};

/// Scenarios served over HTTP, with the path the load generator calls and its request body.
pub const SERVED: &[(&str, &str, &str)] =
    &[("http", "/bench", "small"), ("rest-json", "/orders", "order"), ("http-kafka", "/ingest", "order"), ("http-sql", "/persist", "order"), ("mcp", "/mcp", "mcp"), ("mcp-session", "/mcp", "mcp-session")];

/// PostgreSQL pool size on both sides (Cameleer `poolSize`, HikariCP `maximumPoolSize`).
pub const PG_POOL: usize = 16;
/// Consumer threads of the stream scenario (the input topic has as many partitions).
pub const STREAM_CONSUMERS: usize = 4;

/// Where the external systems are (see the module docs).
#[derive(Clone, Debug)]
pub struct Env {
    pub kafka: String,
    pub pg: String,
    pub side: String,
    pub run: String,
}

impl Env {
    pub fn from_env() -> Env {
        let get = |k: &str, d: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string());
        Env { kafka: get("BENCH_KAFKA", "127.0.0.1:9092"), pg: get("BENCH_PG", "127.0.0.1"), side: get("BENCH_SIDE", "rust"), run: get("BENCH_RUN", "0") }
    }

    pub fn pg_url(&self) -> String {
        format!("jdbc:postgresql://camel:camelpw@{}:5432/bench?poolSize={PG_POOL}", self.pg)
    }
}

/// The path and body kind the load generator uses for a served scenario.
pub fn served(scenario: &str) -> Option<(&'static str, &'static str)> {
    SERVED.iter().find(|(s, _, _)| *s == scenario).map(|(_, p, b)| (*p, *b))
}

/// Add the route of a served scenario on `0.0.0.0:port`.
pub fn add_server_route(ctx: &CamelContext, scenario: &str, port: u16, env: &Env) -> Result<()> {
    let (path, _) = served(scenario).ok_or_else(|| CamelError::illegal_argument(format!("not a served scenario: {scenario}")))?;
    let from = format!("netty-http:http://0.0.0.0:{port}{path}");
    let env = env.clone();
    let scenario = scenario.to_string();
    ctx.add_routes(move |b| match scenario.as_str() {
        // a fixed reply: the cost of the HTTP stack itself
        "http" => {
            b.from(&from)
                .route_id("http")
                .set_header("Content-Type", constant("application/json"))
                .set_body(constant("{\"status\":\"ok\"}"));
        }
        // an order API: read fields from the JSON request, answer with a JSON document
        "rest-json" => {
            b.from(&from)
                .route_id("rest-json")
                .convert_body_to("String")
                .set_header("orderId", jsonpath("$.orderId"))
                .set_header("customer", jsonpath("$.customer"))
                .set_header("Content-Type", constant("application/json"))
                .set_body(simple("{\"orderId\":${header.orderId},\"customer\":\"${header.customer}\",\"status\":\"accepted\"}"));
        }
        // order ingestion: publish the order to Kafka, keyed by order id, then acknowledge
        "http-kafka" => {
            b.from(&from)
                .route_id("http-kafka")
                .convert_body_to("String")
                .set_header("orderId", jsonpath("$.orderId"))
                .set_header("kafka.KEY", simple("${header.orderId}"))
                .to(&format!("kafka:orders-{}?brokers={}", env.side, env.kafka))
                .set_header("Content-Type", constant("application/json"))
                .set_body(constant("{\"status\":\"queued\"}"));
        }
        // persistence: insert one row per order into PostgreSQL, then acknowledge
        "http-sql" => {
            b.from(&from)
                .route_id("http-sql")
                .convert_body_to("String")
                .set_header("orderId", jsonpath("$.orderId"))
                .set_header("customer", jsonpath("$.customer"))
                .to(&format!("sql:insert into orders (order_id, customer, side) values (:#orderId, :#customer, '{}')?dataSource={}", env.side, env.pg_url()))
                .set_header("Content-Type", constant("application/json"))
                .set_body(constant("{\"status\":\"stored\"}"));
        }
        _ => unreachable!(),
    })
}

/// The stream processing route: consume orders from Kafka, keep the valid ones, reshape them
/// and publish them to another topic. `sink` counts records that reached the output topic.
pub fn add_stream_route(ctx: &CamelContext, env: &Env, sink: &Arc<AtomicU64>) -> Result<()> {
    let sink = sink.clone();
    let env = env.clone();
    ctx.add_routes(move |b| {
        b.from(&format!(
            "kafka:stream-in?brokers={}&groupId=stream-{}-{}&autoOffsetReset=earliest&consumersCount={STREAM_CONSUMERS}",
            env.kafka, env.side, env.run
        ))
        .route_id("kafka-stream")
        .convert_body_to("String")
        .set_header("orderId", jsonpath("$.orderId"))
        .set_header("amount", jsonpath("$.amount"))
        .filter(simple("${header.amount} > 10"))
        .transform(simple("{\"orderId\":${header.orderId},\"amount\":${header.amount},\"checked\":true}"))
        .to(&format!("kafka:stream-out-{}?brokers={}", env.side, env.kafka))
        .process(move |_ex: &mut Exchange| {
            sink.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .end();
    })
}

/// Measure the stream route: throughput is records published per second, taken from `sink`
/// (the input topic is filled before the run, so the consumers never wait for data).
pub fn run_stream(sink: &Arc<AtomicU64>, plan: Plan) -> Outcome {
    driver::run(plan, 1, Some(sink.clone()), |_| || std::thread::sleep(std::time::Duration::from_millis(20)))
}

/// One input record of the stream scenario (`amount` is above the filter's 10 for 9 in 10).
pub fn stream_record(id: u64) -> String {
    format!("{{\"orderId\":{id},\"customer\":\"ACME\",\"amount\":{}.50}}", 5 + (id % 10) * 20)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn served_scenarios_have_paths_and_bodies() {
        assert_eq!(served("http-sql"), Some(("/persist", "order")));
        assert_eq!(served("http"), Some(("/bench", "small")));
        assert!(served("direct").is_none());
    }

    #[test]
    fn rest_json_answers_with_fields_from_the_request() {
        let ctx = CamelContext::new();
        cameleer_http::register(&ctx);
        let port = crate::free_port();
        add_server_route(&ctx, "rest-json", port, &Env::from_env()).unwrap();
        ctx.start().unwrap();
        let mut conn = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = crate::loadgen::order_request("127.0.0.1", port, "/orders", 77);
        std::io::Write::write_all(&mut conn, &req).unwrap();
        let mut r = std::io::BufReader::new(conn);
        let mut line = String::new();
        let (status, body) = crate::loadgen::read_response_body(&mut r, &mut line).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "{\"orderId\":77,\"customer\":\"ACME\",\"status\":\"accepted\"}");
        ctx.stop().unwrap();
    }

    fn kafka_env(broker: &cameleer_kafka_mqtt::testing::kafka_broker::KafkaBroker, run: &str) -> Env {
        Env { kafka: broker.bootstrap(), pg: "127.0.0.1".into(), side: "test".into(), run: run.into() }
    }

    fn wait_for(what: &str, f: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !f() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn http_kafka_publishes_orders_keyed_by_id() {
        use cameleer_kafka_mqtt::testing::kafka_broker::KafkaBroker;
        let broker = KafkaBroker::start_with_partitions(1).unwrap();
        let ctx = CamelContext::new();
        cameleer_http::register(&ctx);
        cameleer_kafka_mqtt::register(&ctx);
        let port = crate::free_port();
        add_server_route(&ctx, "http-kafka", port, &kafka_env(&broker, "1")).unwrap();
        ctx.start().unwrap();
        let mut r = std::io::BufReader::new(std::net::TcpStream::connect(("127.0.0.1", port)).unwrap());
        let mut line = String::new();
        for id in [5u64, 6] {
            std::io::Write::write_all(r.get_mut(), &crate::loadgen::order_request("127.0.0.1", port, "/ingest", id)).unwrap();
            let (status, body) = crate::loadgen::read_response_body(&mut r, &mut line).unwrap();
            assert_eq!((status, body.as_str()), (200, "{\"status\":\"queued\"}"));
        }
        let recs = broker.records("orders-test", 0);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[1].key.as_deref(), Some(&b"6"[..]));
        assert!(String::from_utf8_lossy(recs[0].value.as_deref().unwrap()).contains("\"orderId\":5"));
        ctx.stop().unwrap();
    }

    #[test]
    fn kafka_stream_filters_and_reshapes_records() {
        use cameleer_kafka_mqtt::testing::kafka_broker::KafkaBroker;
        let broker = KafkaBroker::start_with_partitions(STREAM_CONSUMERS).unwrap();
        broker.create_topic("stream-in", STREAM_CONSUMERS);
        for id in 0..40u64 {
            broker.append("stream-in", (id % STREAM_CONSUMERS as u64) as i32, None, stream_record(id).as_bytes());
        }
        let ctx = CamelContext::new();
        cameleer_kafka_mqtt::register(&ctx);
        let sink = Arc::new(AtomicU64::new(0));
        add_stream_route(&ctx, &kafka_env(&broker, "1"), &sink).unwrap();
        ctx.start().unwrap();
        // 9 in 10 records pass the amount filter
        wait_for("36 records", || sink.load(Ordering::Relaxed) == 36);
        let out: Vec<String> = (0..STREAM_CONSUMERS as i32)
            .flat_map(|p| broker.records("stream-out-test", p))
            .map(|r| String::from_utf8_lossy(r.value.as_deref().unwrap()).into_owned())
            .collect();
        assert_eq!(out.len(), 36);
        assert!(out.contains(&"{\"orderId\":1,\"amount\":25.5,\"checked\":true}".to_string()), "{out:?}");
        ctx.stop().unwrap();
    }

    #[test]
    fn stream_records_are_json_and_mostly_pass_the_filter() {
        let v = cameleer_core::json::parse(&stream_record(3)).unwrap();
        assert_eq!(v.get("orderId"), Some(&Value::Int(3)));
        let passing = (0..10).filter(|i| 5 + (i % 10) * 20 > 10).count();
        assert_eq!(passing, 9);
    }
}
