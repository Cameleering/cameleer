"""redis-py against rust-camel's embedded Redis server.

usage: redis_py.py <host> <port> <command> [password ca]

commands:
  api        strings/expiry/hashes/lists/sets/sorted sets/keys/scan, pipelines (MULTI/EXEC),
             WATCH, pub/sub, binary values, error replies, RESP3-less client handshake
  seed       writes keys for rust-camel to read
  check      reads keys rust-camel wrote
  auth-fail  expects an authentication error
Prints `key: value` lines.
"""
import sys
import threading
import time

import redis


def client(host, port, password=None, ca=None, username=None):
    kw = dict(host=host, port=int(port), socket_timeout=10, decode_responses=False)
    if password:
        kw["password"] = password
    if username:
        kw["username"] = username
    if ca:
        kw.update(ssl=True, ssl_ca_certs=ca, ssl_cert_reqs="required")
    return redis.Redis(**kw)


def api(r):
    r.flushdb()
    print("ping:", r.ping())
    print("info-version:", r.info("server").get("redis_version"))
    r.set("s", "v1")
    print("get:", r.get("s").decode())
    print("setnx:", r.set("s", "v2", nx=True))
    r.set("t", "x", ex=100)
    print("ttl:", 0 < r.ttl("t") <= 100)
    r.psetex("p", 1500, "y")
    print("pttl:", 0 < r.pttl("p") <= 1500)
    print("incr:", r.incr("n"), r.incrby("n", 10), r.incrbyfloat("f", 1.5))
    print("mget:", [x.decode() if x else None for x in r.mget("s", "nope", "t")])
    r.set("bin", bytes([0, 1, 2, 255, 13, 10]))
    print("binary:", r.get("bin").hex())
    print("append:", r.append("s", "-more"), r.getrange("s", 0, 3).decode(), r.strlen("s"))
    r.hset("h", mapping={"a": "1", "b": "2"})
    print("hash:", sorted((k.decode(), v.decode()) for k, v in r.hgetall("h").items()), r.hincrby("h", "a", 5), r.hexists("h", "z"))
    r.rpush("l", "a", "b", "c")
    r.lpush("l", "z")
    print("list:", [x.decode() for x in r.lrange("l", 0, -1)], r.lpop("l").decode(), r.rpop("l").decode(), r.llen("l"))
    print("blpop:", r.blpop(["l"], timeout=1))
    r.sadd("set", "x", "y", "z")
    print("set:", sorted(x.decode() for x in r.smembers("set")), r.sismember("set", "y"), r.scard("set"))
    r.sadd("set2", "y", "q")
    print("sinter:", sorted(x.decode() for x in r.sinter("set", "set2")))
    r.zadd("z", {"alice": 3, "bob": 1, "carol": 2})
    print("zset:", [(m.decode(), s) for m, s in r.zrange("z", 0, -1, withscores=True)], r.zscore("z", "alice"), r.zrank("z", "carol"))
    print("zrangebyscore:", [m.decode() for m in r.zrangebyscore("z", 2, "+inf")], r.zincrby("z", 10, "bob"))
    print("exists:", r.exists("s", "h", "nope"), r.type("h").decode(), r.type("z").decode())
    keys = sorted(k.decode() for k in r.scan_iter(match="*", count=2))
    print("scan:", ",".join(keys))
    print("keys-pattern:", sorted(k.decode() for k in r.keys("s*")))
    print("rename:", r.rename("s", "s2"), r.exists("s"))
    print("delete:", r.delete("s2", "nope"))
    try:
        r.lpush("h", "x")
        print("wrongtype: none")
    except redis.ResponseError as e:
        print("wrongtype:", str(e).split()[0])
    p = r.pipeline(transaction=True)
    p.set("tx1", "a").incr("txn").get("tx1")
    print("multi:", [x.decode() if isinstance(x, bytes) else x for x in p.execute()])
    p = r.pipeline(transaction=False)
    for i in range(100):
        p.set("pk%d" % i, i)
    print("pipeline:", sum(1 for x in p.execute() if x))
    # WATCH: a concurrent change aborts the transaction
    with r.pipeline() as p:
        p.watch("w")
        pool = r.connection_pool
        other = redis.Redis(connection_pool=redis.ConnectionPool(connection_class=pool.connection_class, **pool.connection_kwargs))
        other.set("w", "changed")
        p.multi()
        p.set("w", "mine")
        try:
            p.execute()
            print("watch: committed")
        except redis.WatchError:
            print("watch: aborted")
    # pub/sub
    ps = r.pubsub()
    ps.subscribe("chan")
    ps.psubscribe("news.*")
    got = []
    deadline = time.time() + 5
    while time.time() < deadline and len(got) < 2:
        m = ps.get_message(timeout=0.2)
        if m and m["type"] in ("subscribe", "psubscribe"):
            got.append(m["type"])
    n1 = r.publish("chan", "hello")
    n2 = r.publish("news.tech", "rust")
    msgs = []
    deadline = time.time() + 5
    while time.time() < deadline and len(msgs) < 2:
        m = ps.get_message(timeout=0.2)
        if m and m["type"] in ("message", "pmessage"):
            msgs.append("%s:%s" % (m["channel"].decode(), m["data"].decode()))
    print("pubsub: %d %d %s" % (n1, n2, ",".join(msgs)))
    ps.close()
    print("dbsize:", r.dbsize())


def seed(r):
    r.set("py:string", "from-python")
    r.hset("py:hash", mapping={"k1": "v1", "k2": "v2"})
    r.rpush("py:list", "a", "b", "c")
    print("seeded: 3")


def check(r):
    print("camel-string:", (r.get("camel:string") or b"").decode())
    print("camel-list:", [x.decode() for x in r.lrange("camel:list", 0, -1)])


def main():
    host, port, cmd = sys.argv[1], sys.argv[2], sys.argv[3]
    password, ca, username = (sys.argv[4:] + [None, None, None])[:3]
    password = password or None
    ca = ca or None
    print("version: redis-py", redis.__version__)
    r = client(host, port, password, ca, username)
    if cmd == "auth-fail":
        try:
            r.ping()
            print("auth: ok")
        except (redis.AuthenticationError, redis.ResponseError) as e:
            print("auth-failed:", type(e).__name__, e)
        return
    {"api": api, "seed": seed, "check": check}[cmd](r)


if __name__ == "__main__":
    main()
