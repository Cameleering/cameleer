"""The DataStax python-cassandra driver against Cameleer's embedded Cassandra server.

usage: cassandra_driver.py <port> <command> [user password ca]

commands:
  cql        protocol v4 session: keyspace/table DDL, simple and prepared statements, batches,
             paging (fetch_size), type round trips, lightweight errors
  seed       creates camel.from_driver rows for Cameleer to read
  check      reads camel.from_camel rows written by Cameleer
  auth-fail  expects an authentication failure
Prints `key: value` lines.
"""
import datetime
import decimal
import logging
import os
import ssl
import sys
import uuid

import cassandra
from cassandra import ProtocolVersion, ConsistencyLevel
from cassandra.auth import PlainTextAuthProvider
from cassandra.cluster import Cluster, NoHostAvailable
from cassandra.policies import WhiteListRoundRobinPolicy
from cassandra.query import BatchStatement, SimpleStatement


def connect(port, user=None, password=None, ca=None, compression=False):
    kw = dict(contact_points=["127.0.0.1"], port=int(port), protocol_version=ProtocolVersion.V4,
              load_balancing_policy=WhiteListRoundRobinPolicy(["127.0.0.1"]), connect_timeout=10, control_connection_timeout=10,
              compression=compression)
    if user:
        kw["auth_provider"] = PlainTextAuthProvider(username=user, password=password)
    if ca:
        ctx = ssl.create_default_context(cafile=ca)
        ctx.check_hostname = False
        kw["ssl_context"] = ctx
    cluster = Cluster(**kw)
    return cluster, cluster.connect()


def cql(session):
    session.execute("CREATE KEYSPACE IF NOT EXISTS official WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1}")
    session.set_keyspace("official")
    session.execute("DROP TABLE IF EXISTS users")
    session.execute("CREATE TABLE users (id int PRIMARY KEY, name text, email text, score double, active boolean, "
                    "created timestamp, uid uuid, tags list<text>, attrs map<text, int>, big bigint, data blob, amount decimal)")
    ins = session.prepare("INSERT INTO users (id, name, email, score, active, created, uid, tags, attrs, big, data, amount) "
                          "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
    print("prepared-columns:", len(ins.column_metadata))
    u = uuid.UUID("12345678-1234-5678-1234-567812345678")
    ts = datetime.datetime(2024, 5, 17, 10, 30, 0)
    for i in range(25):
        session.execute(ins, (i, "user%d" % i, "u%d@example.com" % i, i * 1.5, i % 2 == 0, ts, u, ["a", "b%d" % i], {"k": i},
                              2 ** 40 + i, bytes([i, 255]), decimal.Decimal("12.%02d" % i)))
    row = session.execute("SELECT * FROM users WHERE id = %s", (7,)).one()
    print("row: %s %s %.1f %s %s %s %s %s %d %s %s" % (row.name, row.email, row.score, row.active, row.created.isoformat(),
                                                     row.uid, ",".join(row.tags), row.attrs["k"], row.big, row.data.hex(), row.amount))
    sel = session.prepare("SELECT name FROM users WHERE id = ?")
    print("prepared-select:", session.execute(sel, [3]).one().name)
    # paging: 25 rows with fetch_size 10 -> 3 pages
    stmt = SimpleStatement("SELECT id FROM users", fetch_size=10)
    rs = session.execute(stmt)
    pages = 1
    ids = [r.id for r in rs.current_rows]
    while rs.has_more_pages:
        rs.fetch_next_page()
        pages += 1
        ids.extend(r.id for r in rs.current_rows)
    print("paging: %d rows %d pages" % (len(set(ids)), pages))
    print("auto-paging:", sum(1 for _ in session.execute(SimpleStatement("SELECT id FROM users", fetch_size=7))))
    b = BatchStatement(consistency_level=ConsistencyLevel.QUORUM)
    upd = session.prepare("UPDATE users SET score = ? WHERE id = ?")
    for i in range(3):
        b.add(upd, (100.0 + i, i))
    b.add(SimpleStatement("DELETE FROM users WHERE id = %s"), (24,))
    session.execute(b)
    print("batch:", ",".join("%.0f" % session.execute(sel.query_string.replace("name", "score"), [i]).one().score for i in range(3)) if False else
          ",".join("%.0f" % session.execute("SELECT score FROM users WHERE id = %s", (i,)).one().score for i in range(3)))
    print("count:", session.execute("SELECT count(*) FROM users").one()[0])
    try:
        session.execute("SELECT * FROM nosuchtable")
        print("invalid: not-raised")
    except cassandra.InvalidRequest as e:
        print("invalid: %s" % type(e).__name__)
    ks = session.cluster.metadata.keyspaces
    print("metadata-keyspace:", "official" in ks)
    conns = [pool._connection for pool in session._pools.values()]
    print("compressor:", ",".join(sorted({getattr(c.compressor, "__name__", "none") if c.compressor else "none" for c in conns})))
    print("release-version:", session.execute("SELECT release_version FROM system.local").one().release_version)


def seed(session):
    session.execute("CREATE KEYSPACE IF NOT EXISTS camel WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1}")
    session.execute("CREATE TABLE IF NOT EXISTS camel.from_driver (id int PRIMARY KEY, name text, n bigint)")
    ins = session.prepare("INSERT INTO camel.from_driver (id, name, n) VALUES (?, ?, ?)")
    for i in range(3):
        session.execute(ins, (i, "driver-%d" % i, 10 ** 12 + i))
    print("seeded: 3")


def check(session):
    for r in sorted(session.execute("SELECT id, name FROM camel.from_camel"), key=lambda r: r.id):
        print("row: %d %s" % (r.id, r.name))


def main():
    if os.environ.get("CASS_DEBUG"):
        logging.basicConfig(level=logging.DEBUG)
    port, cmd = sys.argv[1], sys.argv[2]
    user, password, ca, comp = (sys.argv[3:] + [None, None, None, None])[:4]
    print("version: cassandra-driver", cassandra.__version__)
    if cmd == "auth-fail":
        try:
            connect(port, user, password, ca)
            print("auth: ok")
        except NoHostAvailable as e:
            print("auth-failed:", list(e.errors.values())[0].__class__.__name__, list(e.errors.values())[0])
        return
    cluster, session = connect(port, user, password, ca, comp if comp else False)
    {"cql": cql, "seed": seed, "check": check}[cmd](session)
    cluster.shutdown()


if __name__ == "__main__":
    main()
