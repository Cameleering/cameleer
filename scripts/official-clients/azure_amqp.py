"""azure-servicebus and azure-eventhub (pure-Python AMQP 1.0 stack, CBS SAS tokens) against
Cameleer's AMQP 1.0 broker in Azure mode (TLS).

usage: azure_amqp.py <connection-string> <ca-pem> <check> [check...]
checks: servicebus-queue, servicebus-topic, servicebus-sessions, eventhub, bad-key
"""
import datetime
import sys
import time
import traceback

CONN = CA = None


def endpoint():
    host = CONN.split("Endpoint=sb://", 1)[1].split("/", 1)[0]
    return "sb://" + host


def sb_client(conn=None):
    from azure.servicebus import ServiceBusClient
    import os
    return ServiceBusClient.from_connection_string(conn or CONN, custom_endpoint_address=endpoint(), connection_verify=CA, logging_enable=bool(os.environ.get("AZ_DEBUG")))


def servicebus_queue():
    from azure.servicebus import ServiceBusMessage, ServiceBusReceiveMode
    with sb_client() as c:
        with c.get_queue_sender("orders") as s:
            s.send_messages(ServiceBusMessage("order-1", application_properties={"customer": "acme", "qty": 3}, subject="new", message_id="m-1",
                                              content_type="text/plain", correlation_id="c-1"))
            batch = s.create_message_batch()
            for i in range(3):
                batch.add_message(ServiceBusMessage("batch-%d" % i))
            s.send_messages(batch)
            s.send_messages(ServiceBusMessage(b"\x00\x01binary"))
        with c.get_queue_receiver("orders", max_wait_time=5) as r:
            msgs = []
            deadline = time.time() + 20
            while len(msgs) < 5 and time.time() < deadline:
                msgs += r.receive_messages(max_message_count=10, max_wait_time=2)
            first = [m for m in msgs if m.message_id == "m-1"][0]
            print("sb-receive:", sorted(str(m) for m in msgs if not str(m).startswith("\x00")))
            props = {k.decode() if isinstance(k, bytes) else k: (v.decode() if isinstance(v, bytes) else v) for k, v in first.application_properties.items()}
            print("sb-props:", props.get("customer"), props.get("qty"), first.subject, first.content_type, first.correlation_id)
            print("sb-delivery-count:", first.delivery_count)
            binary = [m for m in msgs if b"".join(m.body).startswith(b"\x00")]
            print("sb-binary:", b"".join(binary[0].body).hex() if binary else None)
            # abandon one (redelivered), complete the rest, dead-letter one
            r.abandon_message(first)
            for m in msgs:
                if m is not first:
                    r.complete_message(m)
            again = r.receive_messages(max_message_count=1, max_wait_time=5)
            print("sb-redelivered:", str(again[0]) if again else None, again[0].delivery_count if again else None)
            r.dead_letter_message(again[0], reason="bad", error_description="testing")
        with c.get_queue_receiver("orders", sub_queue="deadletter", max_wait_time=5) as d:
            dl = d.receive_messages(max_message_count=1, max_wait_time=5)
            print("sb-deadletter:", str(dl[0]) if dl else None, dl[0].dead_letter_reason if dl else None)
            for m in dl:
                d.complete_message(m)
        with c.get_queue_sender("orders") as s:
            seq = s.schedule_messages(ServiceBusMessage("scheduled"), datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=2))
            print("sb-scheduled:", len(seq))
        with c.get_queue_receiver("orders", max_wait_time=8) as r:
            got = r.receive_messages(max_message_count=1, max_wait_time=8)
            print("sb-scheduled-received:", str(got[0]) if got else None)
            for m in got:
                r.complete_message(m)
        with c.get_queue_sender("orders") as s:
            s.send_messages([ServiceBusMessage("peeked-%d" % i) for i in range(2)])
        with c.get_queue_receiver("orders", max_wait_time=3) as r:
            print("sb-peek:", [str(m) for m in r.peek_messages(max_message_count=5)])
        with c.get_queue_receiver("orders", receive_mode=ServiceBusReceiveMode.RECEIVE_AND_DELETE, max_wait_time=3) as r:
            got = []
            deadline = time.time() + 10
            while len(got) < 2 and time.time() < deadline:
                got += r.receive_messages(max_message_count=5, max_wait_time=2)
            print("sb-receive-and-delete:", sorted(str(m) for m in got))
        with c.get_queue_receiver("orders", max_wait_time=2) as r:
            print("sb-empty:", len(r.receive_messages(max_message_count=5, max_wait_time=2)))


def servicebus_topic():
    from azure.servicebus import ServiceBusMessage
    with sb_client() as c:
        with c.get_topic_sender("events") as s:
            s.send_messages([ServiceBusMessage("event-%d" % i) for i in range(2)])
        for sub in ("audit", "billing"):
            with c.get_subscription_receiver("events", sub, max_wait_time=5) as r:
                got = []
                deadline = time.time() + 10
                while len(got) < 2 and time.time() < deadline:
                    got += r.receive_messages(max_message_count=10, max_wait_time=2)
                print("sb-subscription-%s:" % sub, sorted(str(m) for m in got))
                for m in got:
                    r.complete_message(m)


def servicebus_sessions():
    from azure.servicebus import ServiceBusMessage, NEXT_AVAILABLE_SESSION
    with sb_client() as c:
        with c.get_queue_sender("sessions") as s:
            # (session ids of exactly two characters are mis-encoded by pyamqp's filter-set encoder)
            for sid in ("session-a", "session-b"):
                for i in range(2):
                    s.send_messages(ServiceBusMessage("%s-%d" % (sid, i), session_id=sid))
        with c.get_queue_receiver("sessions", session_id="session-b", max_wait_time=5) as r:
            got = r.receive_messages(max_message_count=10, max_wait_time=3)
            print("sb-session-b:", [str(m) for m in got], sorted({m.session_id for m in got}))
            r.session.set_state(b"checkpoint")
            print("sb-session-state:", r.session.get_state())
            for m in got:
                r.complete_message(m)
        with c.get_queue_receiver("sessions", session_id=NEXT_AVAILABLE_SESSION, max_wait_time=5) as r:
            got = r.receive_messages(max_message_count=10, max_wait_time=3)
            print("sb-session-next:", [str(m) for m in got], r.session.session_id)
            for m in got:
                r.complete_message(m)


def eventhub():
    from azure.eventhub import EventHubProducerClient, EventHubConsumerClient, EventData
    conn = CONN + ";EntityPath=telemetry"
    import os
    p = EventHubProducerClient.from_connection_string(conn, custom_endpoint_address=endpoint(), connection_verify=CA, logging_enable=bool(os.environ.get("AZ_DEBUG")))
    with p:
        print("eh-partitions:", p.get_partition_ids())
        props = p.get_eventhub_properties()
        print("eh-name:", props["eventhub_name"])
        batch = p.create_batch(partition_id="1")
        for i in range(3):
            e = EventData("reading-%d" % i)
            e.properties = {"sensor": "s%d" % i}
            batch.add(e)
        p.send_batch(batch)
        b2 = p.create_batch(partition_key="device-7")
        b2.add(EventData("keyed"))
        p.send_batch(b2)
        print("eh-partition-props:", p.get_partition_properties("1")["last_enqueued_sequence_number"] >= 2)
    received = []
    c = EventHubConsumerClient.from_connection_string(conn, consumer_group="$Default", custom_endpoint_address=endpoint(), connection_verify=CA)

    def on_event(ctx, event):
        if event is not None:
            received.append((ctx.partition_id, event.body_as_str(), event.properties.get(b"sensor", event.properties.get("sensor")), event.sequence_number))
        if len(received) >= 3:
            c.close()

    import threading
    t = threading.Thread(target=lambda: c.receive(on_event=on_event, partition_id="1", starting_position="-1", max_wait_time=3))
    t.start()
    t.join(30)
    if t.is_alive():
        c.close()
    print("eh-received:", [r[1] for r in received], sorted({r[0] for r in received}))
    print("eh-properties:", [r[2].decode() if isinstance(r[2], bytes) else r[2] for r in received])


def bad_key():
    from azure.servicebus import ServiceBusMessage
    from azure.servicebus.exceptions import ServiceBusError
    bad = CONN.replace(CONN.split("SharedAccessKey=")[1].split(";")[0], "d3Jvbmc=")
    try:
        with sb_client(bad) as c:
            with c.get_queue_sender("orders") as s:
                s.send_messages(ServiceBusMessage("x"))
        print("bad-key: accepted")
    except ServiceBusError as e:
        print("bad-key-refused:", type(e).__name__)


def interop_send():
    from azure.servicebus import ServiceBusMessage
    with sb_client() as c:
        with c.get_queue_sender("to-camel") as s:
            s.send_messages(ServiceBusMessage("from the Azure SDK", application_properties={"origin": "python"}, subject="interop"))
    from azure.eventhub import EventHubProducerClient, EventData
    p = EventHubProducerClient.from_connection_string(CONN + ";EntityPath=telemetry", custom_endpoint_address=endpoint(), connection_verify=CA)
    with p:
        b = p.create_batch(partition_id="2")
        b.add(EventData("event from the Azure SDK"))
        p.send_batch(b)
    print("sent: ok")


def interop_receive():
    with sb_client() as c:
        with c.get_queue_receiver("from-camel", max_wait_time=5) as r:
            got = []
            deadline = time.time() + 15
            while not got and time.time() < deadline:
                got = r.receive_messages(max_message_count=5, max_wait_time=3)
            for m in got:
                props = {(k.decode() if isinstance(k, bytes) else k): (v.decode() if isinstance(v, bytes) else v) for k, v in (m.application_properties or {}).items()}
                print("from-camel:", str(m), props.get("origin"))
                r.complete_message(m)


CHECKS = {"interop-send": interop_send, "interop-receive": interop_receive, "servicebus-queue": servicebus_queue, "servicebus-topic": servicebus_topic, "servicebus-sessions": servicebus_sessions,
          "eventhub": eventhub, "bad-key": bad_key}


def main():
    global CONN, CA
    import os
    if os.environ.get("AZ_DEBUG"):
        import logging
        logging.basicConfig(level=logging.DEBUG, stream=sys.stdout)
        import faulthandler
        faulthandler.dump_traceback_later(int(os.environ.get("AZ_DUMP", "40")), exit=True)
    CONN, CA = sys.argv[1], sys.argv[2]
    import azure.servicebus, azure.eventhub
    print("version: azure-servicebus %s azure-eventhub %s" % (azure.servicebus.__version__, azure.eventhub.__version__))
    failed = False
    for name in sys.argv[3:]:
        try:
            CHECKS[name]()
            print("ok:", name)
        except Exception as e:
            failed = True
            print("FAIL %s: %s" % (name, e))
            traceback.print_exc()
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
