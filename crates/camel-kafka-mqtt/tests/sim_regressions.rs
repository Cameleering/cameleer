//! Hermetic regressions for the bugs found with the third-party simulators
//! (`tests/sim_kafka.rs` against franz-go's kfake): CreateTopics must go to the controller,
//! and group members join with JoinGroup v4 (KIP-394 MEMBER_ID_REQUIRED).

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use camel_core::prelude::*;
use camel_kafka_mqtt::kafka::client::{ClientConfig, KafkaClient};
use camel_kafka_mqtt::kafka::protocol::*;
use camel_kafka_mqtt::testing::KafkaBroker;

/// A two-node fake cluster: node 2 is the controller, node 1 answers CreateTopics with
/// NOT_CONTROLLER (as ZooKeeper-mode Kafka and kfake do).
fn fake_cluster() -> (u16, Arc<AtomicUsize>) {
    let l1 = TcpListener::bind("127.0.0.1:0").unwrap();
    let l2 = TcpListener::bind("127.0.0.1:0").unwrap();
    let ports = [l1.local_addr().unwrap().port(), l2.local_addr().unwrap().port()];
    let created = Arc::new(AtomicUsize::new(0));
    for (node, l) in [(1, l1), (2, l2)] {
        let created = created.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let created = created.clone();
                std::thread::spawn(move || serve(node, ports, s, &created));
            }
        });
    }
    (ports[0], created)
}

fn serve(node: i32, ports: [u16; 2], mut s: TcpStream, created: &AtomicUsize) {
    while let Ok(Some(frame)) = read_frame(&mut s, 1 << 20) {
        let (h, body) = decode_request_header(&frame).unwrap();
        let resp = match h.api_key {
            api::API_VERSIONS => ApiVersionsResponse { error_code: 0, apis: api::VERSIONS.iter().map(|(k, v)| (*k, 0, *v)).collect() }.encode(),
            api::METADATA => MetadataResponse {
                throttle_time_ms: 0,
                brokers: (0..2).map(|i| BrokerMeta { node_id: i + 1, host: "127.0.0.1".into(), port: ports[i as usize] as i32, rack: None }).collect(),
                cluster_id: Some("fake".into()),
                controller_id: 2,
                topics: Vec::new(),
            }
            .encode(),
            api::CREATE_TOPICS => {
                let req = CreateTopicsRequest::decode(body).unwrap();
                let code = if node == 2 {
                    created.fetch_add(1, Ordering::SeqCst);
                    errors::NONE
                } else {
                    errors::NOT_CONTROLLER
                };
                CreateTopicsResponse { throttle_time_ms: 0, topics: req.topics.iter().map(|t| (t.name.clone(), code, None)).collect() }.encode()
            }
            _ => return,
        };
        if s.write_all(&encode_response(h.correlation_id, &resp)).is_err() {
            return;
        }
    }
}

#[test]
fn create_topics_is_sent_to_the_controller() {
    let (bootstrap, created) = fake_cluster();
    let mut c = KafkaClient::new(ClientConfig::new(&format!("127.0.0.1:{}", bootstrap), "t").unwrap());
    c.create_topics(&[("orders".to_string(), 3)]).unwrap();
    assert_eq!(created.load(Ordering::SeqCst), 1);
    assert_eq!(errors::name(errors::NOT_CONTROLLER), "NOT_CONTROLLER");
}

#[test]
fn join_group_v4_member_id_required() {
    let broker = KafkaBroker::start().unwrap();
    let mut s = TcpStream::connect(("127.0.0.1", broker.port())).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut call = |corr: i32, body: &[u8]| {
        s.write_all(&encode_request(&RequestHeader { api_key: api::JOIN_GROUP, api_version: 4, correlation_id: corr, client_id: Some("raw".into()) }, body)).unwrap();
        let resp = read_frame(&mut s, 1 << 20).unwrap().unwrap();
        JoinGroupResponse::decode(&resp[4..]).unwrap()
    };
    let mut req = JoinGroupRequest {
        group_id: "g".into(),
        session_timeout_ms: 10_000,
        rebalance_timeout_ms: 1_000,
        member_id: String::new(),
        protocol_type: "consumer".into(),
        protocols: vec![("range".into(), encode_subscription(&["t".to_string()]))],
    };
    // a first join without member id gets MEMBER_ID_REQUIRED and the id to use
    let first = call(1, &req.encode());
    assert_eq!(first.error_code, errors::MEMBER_ID_REQUIRED);
    assert!(!first.member_id.is_empty());
    // rejoining with it makes the member the leader
    req.member_id = first.member_id.clone();
    let second = call(2, &req.encode());
    assert_eq!(second.error_code, errors::NONE);
    assert_eq!(second.member_id, first.member_id);
    assert_eq!(second.leader, first.member_id);
    assert_eq!(second.members.len(), 1);
    assert_eq!(api::version(api::JOIN_GROUP), 4);

    // the kafka consumer handles it
    let ctx = CamelContext::new();
    camel_kafka_mqtt::register(&ctx);
    broker.create_topic("joined", 1);
    ctx.add_routes(|r| {
        r.from(&format!("kafka:joined?brokers={}&groupId=v4&autoOffsetReset=earliest&pollTimeoutMs=100&heartbeatIntervalMs=50", broker.bootstrap())).to("mock:j");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:j").unwrap();
    mock.expected_bodies_received(vec!["hello"]);
    ctx.create_producer_template().send_body(&format!("kafka:joined?brokers={}", broker.bootstrap()), "hello").unwrap();
    mock.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    ctx.stop().unwrap();
}
