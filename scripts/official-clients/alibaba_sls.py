"""Alibaba Cloud's own SLS client (aliyun-log-python-sdk) against Cameleer's SlsMock.

Usage: alibaba_sls.py SDK_DIR ENDPOINT ACCESS_KEY_ID ACCESS_KEY_SECRET

SDK_DIR is the unpacked aliyun-log-python-sdk source (scripts/official-clients.sh install puts it in
/opt/camel-official/aliyun-log-python-sdk-<version>, checksum-verified). The script runs with
python -I, so the SDK directory is added to sys.path here. Prints `ok:<step>` lines and the values
the Rust test checks.
"""
import sys
import time

sdk, endpoint, ak, secret = sys.argv[1:5]
sys.path.insert(0, sdk)

from aliyun.log import LogClient, LogItem, PutLogsRequest  # noqa: E402
from aliyun.log.auth import AUTH_VERSION_4  # noqa: E402

PROJECT = "official"


def client(version, key_secret=secret):
    if version == "v4":
        c = LogClient(endpoint, ak, key_secret, auth_version=AUTH_VERSION_4, region="cn-hangzhou")
    else:
        c = LogClient(endpoint, ak, key_secret)
    # the SDK takes an IP address as a direct endpoint only without a port; the mock runs on
    # 127.0.0.1:<port>, so connect to it directly (the Host header still names the project)
    c._isRowIp = True
    return c


now = int(time.time())
print("sdk-version:", __import__("aliyun.log.version", fromlist=["__version__"]).__version__)
for version in ("v1", "v4"):
    c = client(version)
    c.create_logstore(PROJECT, "store-" + version, ttl=1, shard_count=1)
    print("ok:create-" + version)
    for compress in ("lz4", None):
        items = []
        for i in range(3):
            item = LogItem(now - 10 + i, [("message", "official %s %s %d" % (version, compress or "raw", i)), ("level", "ERROR" if i == 1 else "INFO")])
            items.append(item)
        req = PutLogsRequest(PROJECT, "store-" + version, "sdk-topic", "10.1.2.3", items, compress=compress is not None, compress_type=compress, logtags=[("team", "integration")])
        c.put_logs(req)
        print("ok:put-%s-%s" % (version, compress or "raw"))
    res = c.get_log(PROJECT, "store-" + version, now - 60, now + 60, query="level: ERROR")
    print("get-%s-count: %d" % (version, res.get_count()))
    print("get-%s-messages: %s" % (version, ",".join(sorted(l.get_contents()["message"] for l in res.get_logs()))))
    print("ok:get-" + version)
    ls = c.list_logstore(PROJECT, "store", offset=0, size=10)
    print("list-%s: %d %s" % (version, ls.get_total(), ",".join(ls.get_logstores())))
    print("ok:list-" + version)

# a wrong secret is refused
try:
    client("v1", "wrong-secret").list_logstore(PROJECT, "store")
    print("wrong-secret: accepted")
except Exception as e:  # LogException
    print("wrong-secret:", getattr(e, "get_error_code", lambda: str(e))())
