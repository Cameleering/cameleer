//! Hermetic regressions for the AMQP 1.0 / Azure-emulation gaps found with the Azure SDK for
//! Python (`tests/official_azure_amqp.rs`): performatives with every field encoded, explicit
//! connection / session limits, SAS tokens put on `$cbs` with type `jwt`, absolute entity URLs as
//! link addresses, senders asking for receiver-settle-mode `second`, the Azure batch message
//! format, plain-valued session filters, session lock expiry as .NET ticks, the message header
//! on first deliveries, management replies without reply-to and detach echoes.

use std::time::Duration;

use cameleer_amqp10::client::{Connection, ConnectionOptions, LinkOptions};
use cameleer_amqp10::codec::AmqpValue;
use cameleer_amqp10::message::Message;
use cameleer_amqp10::testing::Amqp10Broker;
use cameleer_amqp10::types::{Frame, FrameBody, Performative, RcvSettleMode, SndSettleMode};

const KEY_NAME: &str = "RootManageSharedAccessKey";
const KEY: &str = "c2VjcmV0K2tleS8xMjM=";
const T: Duration = Duration::from_secs(5);

fn broker() -> Amqp10Broker {
    Amqp10Broker::builder().azure(KEY_NAME, KEY).session_entity("sessions").start().unwrap()
}

fn authorized_session(b: &Amqp10Broker, entity: &str) -> (Connection, cameleer_amqp10::client::Session) {
    let conn = Connection::open(&ConnectionOptions::new("127.0.0.1", b.port())).unwrap();
    let s = conn.session().unwrap();
    let audience = format!("sb://localhost/{}", entity);
    let token = cameleer_amqp10::util::sas_token(&audience, KEY_NAME, KEY, cameleer_amqp10::util::now_secs() + 60);
    // the Python SDK puts SAS tokens with type `jwt`: recognized by their content
    cameleer_amqp10::cbs::put_token(&s, &audience, &token, cameleer_amqp10::cbs::JWT_TOKEN_TYPE, T).unwrap();
    (conn, s)
}

#[test]
fn performatives_keep_all_fields_and_explicit_limits() {
    let open = Performative::Open(cameleer_amqp10::types::Open { container_id: "c".into(), max_frame_size: u32::MAX, channel_max: u16::MAX, ..Default::default() });
    let encoded = Frame::amqp(0, open).encode();
    match Frame::decode(&encoded).unwrap().body {
        FrameBody::Amqp(Performative::Open(o), _) => {
            assert_eq!(o.max_frame_size, u32::MAX);
            assert_eq!(o.channel_max, u16::MAX);
        }
        other => panic!("{:?}", other),
    }
    // the described list holds all 10 Open fields (positional readers index field 9)
    let body = &encoded[8..];
    let mut d = cameleer_amqp10::codec::Decoder::new(body);
    let v = d.read_value().unwrap();
    assert_eq!(v.undescribed().as_list().map(|l| l.len()), Some(10));
}

#[test]
fn cbs_jwt_typed_sas_tokens_absolute_addresses_and_settlement() {
    let b = broker();
    let (_c, s) = authorized_session(&b, "orders");
    // an absolute entity URL as target address, receiver-settle-mode second requested
    let mut opts = LinkOptions::new(&format!("amqps://localhost:{}/orders", b.port()));
    opts = opts.settle_mode(SndSettleMode::Unsettled, RcvSettleMode::Second);
    let tx = s.sender(opts).unwrap();
    assert_eq!(tx.remote_attach().rcv_settle_mode, RcvSettleMode::First, "Service Bus settles at once");
    tx.send_accepted(&Message::text("one"), T).unwrap();
    // an Azure batch delivery is split into its messages
    tx.send_azure_batch(&[Message::text("b1"), Message::text("b2")], T).unwrap();
    assert_eq!(b.queue_depth("orders"), 3);
    // first deliveries carry a header with delivery-count 0
    let rx = s.receiver(LinkOptions::new("orders").credit(10)).unwrap();
    let d = rx.receive(T).unwrap().unwrap();
    let raw = d.raw.clone();
    assert_eq!(raw[..3], [0x00, 0x53, 0x70], "header section first");
    assert_eq!(d.message.header.as_ref().map(|h| h.delivery_count), Some(0));
}

#[test]
fn plain_session_filters_and_tick_lock_expiry() {
    let b = broker();
    let (_c, s) = authorized_session(&b, "sessions");
    let tx = s.sender(LinkOptions::new("sessions")).unwrap();
    for sid in ["alpha", "beta"] {
        let mut m = Message::text(format!("{}-msg", sid));
        m.props_mut().group_id = Some(sid.into());
        tx.send_accepted(&m, T).unwrap();
    }
    // the Python SDK sends the session id as a plain value under the filter key
    let mut opts = LinkOptions::new("sessions").credit(5);
    opts.filter.push((AmqpValue::sym(cameleer_amqp10::azure::SESSION_FILTER), AmqpValue::str("beta")));
    let rx = s.receiver(opts).unwrap();
    let attach = rx.remote_attach().clone();
    let filter = &attach.source.as_ref().unwrap().filter;
    assert_eq!(filter[0].1, AmqpValue::str("beta"), "echoed in the form it was sent");
    let until = attach.properties.iter().find(|(k, _)| k.as_str() == Some("com.microsoft:locked-until-utc")).map(|(_, v)| v.clone()).unwrap();
    assert!(matches!(until, AmqpValue::Long(t) if t > 621_355_968_000_000_000), "{:?}", until);
    let ms = cameleer_amqp10::azure::locked_until_millis(&until);
    assert!((ms - cameleer_amqp10::util::now_secs() as i64 * 1000).abs() < 120_000);
    let d = rx.receive(T).unwrap().unwrap();
    assert_eq!(d.message.props().group_id.as_deref(), Some("beta"));
    assert_eq!(cameleer_amqp10::azure::locked_until_millis(&AmqpValue::Timestamp(42)), 42);
    assert_eq!(cameleer_amqp10::azure::millis_to_ticks(0), 621_355_968_000_000_000);
}

#[test]
fn management_replies_without_reply_to() {
    let b = Amqp10Broker::builder().azure(KEY_NAME, KEY).event_hub("hub", 2).start().unwrap();
    let (_c, s) = authorized_session(&b, "hub");
    // a CBS receiver and a management receiver on the same session; the request has no reply-to
    let _cbs_rx = s.receiver(LinkOptions::new("$cbs").local_address("$cbs").credit(10)).unwrap();
    let mgmt_rx = s.receiver(LinkOptions::new("$management").local_address("$management").credit(10)).unwrap();
    let mgmt_tx = s.sender(LinkOptions::new("$management")).unwrap();
    let mut req = Message::default();
    req.props_mut().message_id = Some(AmqpValue::str("m-1"));
    req.set_app_property("operation", AmqpValue::str("READ"));
    req.set_app_property("name", AmqpValue::str("hub"));
    req.set_app_property("type", AmqpValue::str("com.microsoft:eventhub"));
    mgmt_tx.send_accepted(&req, T).unwrap();
    let resp = mgmt_rx.receive(T).unwrap().expect("answered on the management receiver");
    assert_eq!(resp.message.app_property("status-code").and_then(|v| v.as_i64()), Some(200));
}
