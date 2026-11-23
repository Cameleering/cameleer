//! A `platform-http` route that sends to Kafka runs as an asynchronous task (docs/design/async-routing.md):
//! the exchange waits for the broker without a thread, and the route continues on the producer's sender
//! thread. Answers and records are the same as on the synchronous path.
#![cfg(feature = "kafka-mqtt")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_core::spi::Processor;
use cameleer_http::{HttpClient, ServerConfig};
use cameleer_kafka_mqtt::testing::KafkaBroker;

struct Probe(Arc<Mutex<Vec<bool>>>);
impl Processor for Probe {
    fn process(&self, _: &mut Exchange) -> Result<()> {
        self.0.lock().unwrap().push(cameleer_core::reactive::on_loop());
        Ok(())
    }
    fn blocking(&self) -> cameleer_core::spi::Blocking {
        cameleer_core::spi::Blocking::Never
    }
}

fn post(url: &str, body: &str) -> (u16, String) {
    let r = HttpClient::new().request("POST", url, &[("Content-Type", "text/plain")], Some(body.as_bytes()), Some(Duration::from_secs(20))).unwrap();
    (r.status, r.body_text())
}

/// 16 clients x 10 requests through a server with 2 offload threads; returns where the step after the
/// Kafka send ran and the number of records.
fn run(clients: usize, each: usize) -> (Vec<bool>, usize) {
    let broker = KafkaBroker::start_with_partitions(2).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_kafka_mqtt::register(&ctx);
    ctx.add_component(
        "platform-http",
        cameleer_http::platform::HttpConsumerComponent::new("platform-http").with_port(0).with_server_config(ServerConfig { pool_size: 2, ..ServerConfig::default() }),
    );
    let probe = seen.clone();
    let uri = format!("kafka:ingest?brokers={}", broker.bootstrap());
    ctx.add_routes(move |b| {
        b.from("platform-http:/ingest").route_id("ingest").to(uri.clone()).process(Probe(probe.clone())).set_body(constant("queued"));
    })
    .unwrap();
    ctx.start().unwrap();
    let base = format!("http://127.0.0.1:{}", cameleer_http::platform_http_port(&ctx).unwrap());
    let hs: Vec<_> = (0..clients)
        .map(|c| {
            let url = format!("{base}/ingest");
            std::thread::spawn(move || {
                for i in 0..each {
                    assert_eq!(post(&url, &format!("c{c}-{i}")), (200, "queued".to_string()));
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    ctx.stop().unwrap();
    let n: usize = (0..2).map(|p| broker.records("ingest", p).len()).sum();
    let loops = seen.lock().unwrap().clone();
    (loops, n)
}

#[test]
fn a_route_that_sends_to_kafka_waits_without_a_thread_and_loses_nothing() {
    let (loops, records) = run(16, 10);
    assert_eq!(records, 160);
    assert_eq!(loops.len(), 160);
    assert!(loops.iter().all(|l| *l), "the step after the send continues on the producer's sender thread, marked as an event loop");

    cameleer_core::reactive::set_enabled(false);
    let (loops, records) = run(16, 10);
    cameleer_core::reactive::set_enabled(true);
    assert_eq!(records, 160);
    assert!(loops.iter().all(|l| !*l), "the synchronous path runs on the server's worker threads");
}
