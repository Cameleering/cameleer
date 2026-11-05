//! Official-client cross-verification of the AMQP 1.0 broker's Azure emulation: the Azure SDK
//! for Python (azure-servicebus 7 and azure-eventhub 5, both on the SDK's pure-Python AMQP
//! stack with CBS SAS tokens over TLS) against the embedded broker, and message exchange with
//! the `azure-servicebus` / `azure-eventhubs` components.
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1` (see `scripts/official-clients.sh`).

use cameleer_amqp10::testing::Amqp10Broker;
use cameleer_core::prelude::*;
use cameleer_tls::testing::official::{self, python_script, Run};

const KEY_NAME: &str = "RootManageSharedAccessKey";
const KEY: &str = "c2VjcmV0K2tleS8xMjM=";

fn broker() -> Amqp10Broker {
    Amqp10Broker::builder()
        .azure(KEY_NAME, KEY)
        .tls(official::server_params().server_config().unwrap())
        .max_delivery_count(3)
        .subscription("events", "audit")
        .subscription("events", "billing")
        .session_entity("sessions")
        .event_hub("telemetry", 3)
        .start()
        .unwrap()
}

fn sdk(b: &Amqp10Broker, ca: &std::path::Path, checks: &[&str]) -> Run {
    let cs = b.connection_string(KEY_NAME, KEY);
    let mut args = vec![cs.as_str(), ca.to_str().unwrap()];
    args.extend_from_slice(checks);
    python_script("azure_amqp.py", &args, &[])
}

#[test]
fn azure_sdk_service_bus_and_event_hubs() {
    if !official::enabled() {
        return;
    }
    let pem = official::pem_files("azure-amqp");
    let b = broker();
    let checks = ["servicebus-queue", "servicebus-topic", "servicebus-sessions", "eventhub", "bad-key"];
    let r = sdk(&b, &pem.ca, &checks).ok();
    assert_eq!(r.values("ok:"), checks.to_vec());
    let expect = [
        ("sb-receive:", "['batch-0', 'batch-1', 'batch-2', 'order-1']"),
        ("sb-props:", "acme 3 new text/plain c-1"),
        ("sb-delivery-count:", "0"),
        ("sb-binary:", "000162696e617279"),
        ("sb-redelivered:", "order-1 1"),
        ("sb-deadletter:", "order-1 bad"),
        ("sb-scheduled:", "1"),
        ("sb-scheduled-received:", "scheduled"),
        ("sb-peek:", "['peeked-0', 'peeked-1']"),
        ("sb-receive-and-delete:", "['peeked-0', 'peeked-1']"),
        ("sb-empty:", "0"),
        ("sb-subscription-audit:", "['event-0', 'event-1']"),
        ("sb-subscription-billing:", "['event-0', 'event-1']"),
        ("sb-session-b:", "['session-b-0', 'session-b-1'] ['session-b']"),
        ("sb-session-state:", "b'checkpoint'"),
        ("sb-session-next:", "['session-a-0', 'session-a-1'] session-a"),
        ("eh-partitions:", "['0', '1', '2']"),
        ("eh-name:", "telemetry"),
        ("eh-partition-props:", "True"),
        ("eh-received:", "['reading-0', 'reading-1', 'reading-2'] ['1']"),
        ("eh-properties:", "['s0', 's1', 's2']"),
    ];
    for (k, v) in expect {
        assert_eq!(r.value(k), v, "{}\n{}", k, r.stdout);
    }
    assert!(!r.value("bad-key-refused:").is_empty());
    let _ = std::fs::remove_dir_all(&pem.dir);
}

#[test]
fn azure_sdk_and_components_exchange_messages() {
    if !official::enabled() {
        return;
    }
    let pem = official::pem_files("azure-amqp-interop");
    let b = broker();
    sdk(&b, &pem.ca, &["interop-send"]).ok();
    let ctx = CamelContext::new();
    cameleer_amqp10::register(&ctx);
    ctx.registry().bind("ssl", official::server_params());
    let cs = b.connection_string(KEY_NAME, KEY);
    let sb = |entity: &str| format!("azure-servicebus:{}?connectionString=RAW({})&sslContextParameters=#ssl", entity, cs);
    let eh = format!("azure-eventhubs:?connectionString=RAW({};EntityPath=telemetry)&sslContextParameters=#ssl&partitionId=2&initialPosition=earliest", cs);
    let (from_sb, from_eh) = (sb("to-camel"), eh.clone());
    ctx.add_routes(move |r| {
        r.from(&from_sb).to("mock:sb");
        r.from(&from_eh).to("mock:eh");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:sb").unwrap();
    m.expected_bodies_received(vec!["from the Azure SDK"]);
    m.assert_is_satisfied_within(std::time::Duration::from_secs(20)).unwrap();
    let props = m.exchange(0).unwrap().header("CamelAzureServiceBusApplicationProperties").cloned().unwrap();
    assert_eq!(props.get("origin").map(|v| v.to_string()).as_deref(), Some("python"));
    assert_eq!(m.exchange(0).unwrap().header("CamelAzureServiceBusSubject").map(|v| v.to_string()).as_deref(), Some("interop"));
    let e = ctx.mock_endpoint("mock:eh").unwrap();
    e.expected_bodies_received(vec!["event from the Azure SDK"]);
    e.assert_is_satisfied_within(std::time::Duration::from_secs(20)).unwrap();
    ctx.create_producer_template()
        .send_with(&sb("from-camel"), ExchangePattern::InOnly, |ex| {
            ex.set_body("from Cameleer");
            ex.set_header("CamelAzureServiceBusApplicationProperties", Value::object([("origin", "camel")]));
        })
        .exception
        .map(|e| panic!("{:?}", e));
    ctx.stop().unwrap();
    let r = sdk(&b, &pem.ca, &["interop-receive"]).ok();
    assert_eq!(r.value("from-camel:"), "from Cameleer camel");
    let _ = std::fs::remove_dir_all(&pem.dir);
}
