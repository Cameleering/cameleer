"""SigV4 reference signatures from botocore for a list of request vectors (stdin, JSON).

Each vector: {"method", "url", "headers": [[k, v]...], "body_b64", "service", "region",
"time": epoch seconds, "access_key", "secret_key", "token"?, "presign": expires seconds?}.
Prints one JSON line per vector: {"authorization": ..., "headers": {...}} for header
signing, or {"url": ...} for presigned URLs (botocore's S3SigV4QueryAuth).
"""
import base64
import datetime
import json
import sys

import botocore
import botocore.auth
from botocore.awsrequest import AWSRequest
from botocore.credentials import Credentials


class FixedClock:
    """Stands in for the datetime module inside botocore.auth."""

    def __init__(self, ts):
        self.ts = ts
        outer = self

        class _DT(datetime.datetime):
            @classmethod
            def utcnow(cls):
                return datetime.datetime.utcfromtimestamp(outer.ts)

            @classmethod
            def now(cls, tz=None):
                return datetime.datetime.fromtimestamp(outer.ts, tz)

        self.datetime = _DT
        self.timezone = datetime.timezone
        self.timedelta = datetime.timedelta


def sign(v):
    botocore.auth.datetime = FixedClock(v["time"])
    creds = Credentials(v["access_key"], v["secret_key"], v.get("token"))
    body = base64.b64decode(v.get("body_b64", ""))
    req = AWSRequest(method=v["method"], url=v["url"], headers=dict(v.get("headers", [])), data=body)
    if "presign" in v:
        botocore.auth.S3SigV4QueryAuth(creds, v["service"], v["region"], expires=v["presign"]).add_auth(req)
        return {"url": req.url}
    cls = botocore.auth.S3SigV4Auth if v["service"] == "s3" else botocore.auth.SigV4Auth
    cls(creds, v["service"], v["region"]).add_auth(req)
    return {"authorization": req.headers["Authorization"], "headers": {k: v for k, v in req.headers.items()}}


def main():
    print(json.dumps({"version": botocore.__version__}))
    for v in json.load(sys.stdin):
        print(json.dumps(sign(v)))


if __name__ == "__main__":
    main()
