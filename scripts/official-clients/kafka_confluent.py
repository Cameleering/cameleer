"""confluent-kafka (librdkafka) against rust-camel's embedded Kafka broker.

usage: kafka_confluent.py <bootstrap> <command> [args...]

Prints `key: value` lines the Rust test asserts on. Security settings come from the
environment: KAFKA_SECURITY_PROTOCOL, KAFKA_SASL_MECHANISM, KAFKA_SASL_USERNAME,
KAFKA_SASL_PASSWORD, KAFKA_CA (PEM), KAFKA_CERT / KAFKA_KEY (client certificate).
"""
import os
import sys
import time

import confluent_kafka
from confluent_kafka import Consumer, Producer, TopicPartition, KafkaError
from confluent_kafka.admin import AdminClient, NewTopic


def base_conf(bootstrap):
    conf = {"bootstrap.servers": bootstrap, "client.id": "official-librdkafka", "broker.address.family": "v4"}
    env = os.environ
    if env.get("KAFKA_SECURITY_PROTOCOL"):
        conf["security.protocol"] = env["KAFKA_SECURITY_PROTOCOL"]
    if env.get("KAFKA_SASL_MECHANISM"):
        conf["sasl.mechanism"] = env["KAFKA_SASL_MECHANISM"]
        conf["sasl.username"] = env["KAFKA_SASL_USERNAME"]
        conf["sasl.password"] = env["KAFKA_SASL_PASSWORD"]
    if env.get("KAFKA_CA"):
        conf["ssl.ca.location"] = env["KAFKA_CA"]
        conf["ssl.endpoint.identification.algorithm"] = "https"
    if env.get("KAFKA_CERT"):
        conf["ssl.certificate.location"] = env["KAFKA_CERT"]
        conf["ssl.key.location"] = env["KAFKA_KEY"]
    if env.get("KAFKA_DEBUG"):
        conf["debug"] = env["KAFKA_DEBUG"]
    return conf


def produce(bootstrap, topic, count, codec, prefix="msg"):
    conf = base_conf(bootstrap)
    conf.update({"compression.codec": codec, "linger.ms": 50, "acks": "all", "partitioner": os.environ.get("KAFKA_PARTITIONER", "consistent_random")})
    p = Producer(conf)
    delivered = []
    errors = []

    def cb(err, msg):
        if err is not None:
            errors.append(str(err))
        else:
            delivered.append((msg.partition(), msg.offset()))

    for i in range(count):
        p.produce(topic, value=("%s-%s-%d " % (prefix, codec, i)) * 20, key="key-%d" % (i % 4), on_delivery=cb,
                  headers=[("codec", codec.encode()), ("index", str(i).encode()), ("empty", b""), ("bin", bytes([0, 1, 255]))])
    p.flush(30)
    if errors:
        print("error:", errors[0])
        sys.exit(1)
    print("delivered:", len(delivered))
    for part, off in sorted(delivered):
        print("offset: %d %d" % (part, off))


def consume(bootstrap, topic, group, expected, commit_mode="sync"):
    conf = base_conf(bootstrap)
    conf.update({"group.id": group, "auto.offset.reset": "earliest", "enable.auto.commit": commit_mode == "auto",
                 "session.timeout.ms": 6000, "heartbeat.interval.ms": 500})
    c = Consumer(conf)
    c.subscribe([topic])
    got = []
    deadline = time.time() + 60
    while len(got) < expected and time.time() < deadline:
        m = c.poll(0.5)
        if m is None:
            continue
        if m.error():
            if m.error().code() == KafkaError._PARTITION_EOF:
                continue
            print("error:", m.error())
            sys.exit(1)
        got.append(m)
        hdrs = [(k, v) for k, v in (m.headers() or [])]
        print("record: %d %d %s %s %s" % (m.partition(), m.offset(), (m.key() or b"").decode(),
                                           (m.value() or b"")[:40].decode(errors="replace").split(" ")[0],
                                           ",".join("%s=%s" % (k, (v or b"").hex()) for k, v in hdrs)))
        if commit_mode == "sync":
            c.commit(message=m, asynchronous=False)
    if commit_mode == "auto":
        c.commit(asynchronous=False)
    committed = c.committed([TopicPartition(topic, p) for p in sorted({m.partition() for m in got})], timeout=10)
    for tp in committed:
        print("committed: %d %d" % (tp.partition, tp.offset))
    c.close()
    print("consumed:", len(got))
    if len(got) < expected:
        sys.exit(2)


def admin(bootstrap, topic, partitions):
    a = AdminClient(base_conf(bootstrap))
    fs = a.create_topics([NewTopic(topic, num_partitions=int(partitions), replication_factor=1)])
    for t, f in fs.items():
        f.result(15)
        print("created:", t)
    md = a.list_topics(timeout=10)
    print("partitions: %d" % len(md.topics[topic].partitions))
    print("brokers: %d" % len(md.brokers))


def auth_error(bootstrap):
    """Connects with (bad) credentials and reports the first error librdkafka raises."""
    errors = []
    conf = base_conf(bootstrap)
    conf["error_cb"] = lambda e: errors.append(e)
    p = Producer(conf)
    deadline = time.time() + 20
    while not errors and time.time() < deadline:
        p.poll(0.2)
    if not errors:
        print("no-error")
        sys.exit(1)
    print("auth-error: %s %s" % (errors[0].name(), errors[0].str()))


def group(bootstrap, topic, group_id, produce_count, idle):
    """Joins a consumer group shared with a rust-camel consumer, produces `produce_count`
    records round-robin over all partitions once it has an assignment, then consumes until
    `idle` seconds pass without records."""
    conf = base_conf(bootstrap)
    conf.update({"group.id": group_id, "auto.offset.reset": "earliest", "session.timeout.ms": 6000,
                 "heartbeat.interval.ms": 500, "partition.assignment.strategy": "range"})
    c = Consumer(conf)
    assigned = []
    c.subscribe([topic], on_assign=lambda cons, parts: assigned.append(sorted(p.partition for p in parts)))
    deadline = time.time() + 60
    while (not assigned or not assigned[-1]) and time.time() < deadline:
        c.poll(0.2)
    print("assigned: %s" % ",".join(str(p) for p in assigned[-1]))
    md = c.list_topics(topic, timeout=10)
    nparts = len(md.topics[topic].partitions)
    pconf = base_conf(bootstrap)
    p = Producer(pconf)
    for i in range(produce_count):
        p.produce(topic, value=("group-%d" % i).encode(), key=("k%d" % i).encode(), partition=i % nparts)
    p.flush(30)
    got = []
    last = time.time()
    while time.time() - last < idle:
        m = c.poll(0.2)
        if m is None or m.error():
            continue
        last = time.time()
        got.append(m)
        c.commit(message=m, asynchronous=False)
    print("partitions: %s" % ",".join(str(x) for x in sorted({m.partition() for m in got})))
    print("received: %d" % len(got))
    c.close()


def main():
    bootstrap, cmd = sys.argv[1], sys.argv[2]
    args = sys.argv[3:]
    print("version: confluent-kafka %s librdkafka %s" % (confluent_kafka.version()[0], confluent_kafka.libversion()[0]))
    if cmd == "produce":
        produce(bootstrap, args[0], int(args[1]), args[2] if len(args) > 2 else "none")
    elif cmd == "consume":
        consume(bootstrap, args[0], args[1], int(args[2]), args[3] if len(args) > 3 else "sync")
    elif cmd == "admin":
        admin(bootstrap, args[0], args[1])
    elif cmd == "auth-error":
        auth_error(bootstrap)
    elif cmd == "group":
        group(bootstrap, args[0], args[1], int(args[2]), float(args[3]))
    else:
        raise SystemExit("unknown command " + cmd)


if __name__ == "__main__":
    main()
