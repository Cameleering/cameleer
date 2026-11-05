"""grpcio (the official gRPC C-core based Python implementation) against Cameleer's gRPC
stack: as a client of a Cameleer `grpc` consumer route, as a server called by the Cameleer
`grpc` producer, and as a Pub/Sub StreamingPull client of the GcpMock gRPC service.

usage:
  grpc_python.py client <host:port> <proto-dir> [--ca ca.pem] [--cert c.pem --key c.key] [--gzip] [--authority name]
  grpc_python.py server <proto-dir> <portfile> [cert.pem key.pem]    (serves until stdin closes)
  grpc_python.py pubsub <host:port> <subscription> <count> [service-account.json]

Protos are compiled at runtime with grpc_tools.protoc.
"""
import importlib
import os
import sys
import tempfile
import threading
import time

import grpc
from grpc_tools import protoc

PUBSUB_PROTO = """
syntax = "proto3";
package google.pubsub.v1;
message PubsubMessage { bytes data = 1; map<string, string> attributes = 2; string message_id = 3; string ordering_key = 5; }
message ReceivedMessage { string ack_id = 1; PubsubMessage message = 2; int32 delivery_attempt = 3; }
message StreamingPullRequest {
  string subscription = 1; repeated string ack_ids = 2; repeated int32 modify_deadline_seconds = 3;
  repeated string modify_deadline_ack_ids = 4; int32 stream_ack_deadline_seconds = 5; string client_id = 6;
  int64 max_outstanding_messages = 7; int64 max_outstanding_bytes = 8;
}
message AcknowledgeConfirmation { repeated string ack_ids = 1; repeated string invalid_ack_ids = 2; repeated string unordered_ack_ids = 3; }
message StreamingPullResponse { repeated ReceivedMessage received_messages = 1; AcknowledgeConfirmation acknowledge_confirmation = 5; }
service Subscriber { rpc StreamingPull(stream StreamingPullRequest) returns (stream StreamingPullResponse); }
"""


def compile_protos(proto_dir, files):
    out = tempfile.mkdtemp()
    args = ["protoc", "-I" + proto_dir, "--python_out=" + out, "--grpc_python_out=" + out] + files
    if protoc.main(args) != 0:
        raise SystemExit("protoc failed")
    sys.path.insert(0, out)
    return out


def greeter_modules(proto_dir):
    compile_protos(proto_dir, ["greeter.proto", "common.proto"])
    return importlib.import_module("greeter_pb2"), importlib.import_module("greeter_pb2_grpc"), importlib.import_module("common_pb2")


def flags(args):
    out, i = {}, 0
    while i < len(args):
        if args[i] == "--gzip":
            out["gzip"] = True
            i += 1
        else:
            out[args[i][2:]] = args[i + 1]
            i += 2
    return out


def client(target, proto_dir, *rest):
    f = flags(rest)
    pb, rpc, common = greeter_modules(proto_dir)
    opts = [("grpc.enable_http_proxy", 0)]
    if "authority" in f:
        opts.append(("grpc.ssl_target_name_override", f["authority"]))
    comp = grpc.Compression.Gzip if f.get("gzip") else None
    ca = f.get("ca")
    if ca:
        key = open(f["key"], "rb").read() if "key" in f else None
        cert = open(f["cert"], "rb").read() if "cert" in f else None
        creds = grpc.ssl_channel_credentials(root_certificates=open(ca, "rb").read(), private_key=key, certificate_chain=cert)
        channel = grpc.secure_channel(target, creds, options=opts, compression=comp)
    else:
        channel = grpc.insecure_channel(target, options=opts, compression=comp)
    stub = rpc.GreeterStub(channel)
    try:
        r = stub.SayHello(pb.HelloRequest(name="grpcio", mood=common.HAPPY), metadata=[("x-tenant", "python")], timeout=10)
    except grpc.RpcError as e:
        print("connect-error: %s" % e.code().name)
        return
    print("unary: %s|%d" % (r.message, r.index))
    replies = list(stub.SayHelloStream(pb.HelloRequest(name="S", times=3), timeout=10))
    print("server-stream: %s" % ",".join("%s/%d" % (x.message, x.index) for x in replies))
    r = stub.SayHelloToAll(iter([pb.HelloRequest(name=n) for n in ("a", "b", "c")]), timeout=10)
    print("client-stream: %s|%d" % (r.message, r.index))
    replies = list(stub.Chat(iter([pb.HelloRequest(name=n) for n in ("x", "y")]), timeout=10))
    print("bidi: %s" % ",".join(x.message for x in replies))
    try:
        stub.SayHello(pb.HelloRequest(name="missing"), timeout=10)
        print("error: none")
    except grpc.RpcError as e:
        print("error: %s|%s" % (e.code().name, e.details()))
    try:
        stub.SayHello(pb.HelloRequest(name="slow"), timeout=0.2)
        print("deadline: none")
    except grpc.RpcError as e:
        print("deadline: %s" % e.code().name)
    big = stub.SayHello(pb.HelloRequest(name="x" * 300000), timeout=10)
    print("large: %d" % len(big.message))
    channel.close()


def server(proto_dir, portfile, cert=None, key=None):
    pb, rpc, common = greeter_modules(proto_dir)
    from concurrent import futures

    class Greeter(rpc.GreeterServicer):
        def SayHello(self, request, context):
            md = dict(context.invocation_metadata())
            if request.name == "fail":
                context.abort(grpc.StatusCode.PERMISSION_DENIED, "no access for fail")
            return pb.HelloReply(message="Hi %s from grpcio (%s)" % (request.name, md.get("x-tenant", "-")), index=request.mood)

        def SayHelloStream(self, request, context):
            for i in range(request.times):
                yield pb.HelloReply(message="%s #%d" % (request.name, i), index=i)

        def SayHelloToAll(self, request_iterator, context):
            names = [r.name for r in request_iterator]
            return pb.HelloReply(message="all: " + ",".join(names), index=len(names))

        def Chat(self, request_iterator, context):
            for r in request_iterator:
                yield pb.HelloReply(message="chat " + r.name)

    s = grpc.server(futures.ThreadPoolExecutor(max_workers=8), compression=grpc.Compression.Gzip)
    rpc.add_GreeterServicer_to_server(Greeter(), s)
    if cert:
        creds = grpc.ssl_server_credentials([(open(key, "rb").read(), open(cert, "rb").read())])
        port = s.add_secure_port("127.0.0.1:0", creds)
    else:
        port = s.add_insecure_port("127.0.0.1:0")
    s.start()
    with open(portfile + ".tmp", "w") as f:
        f.write(str(port))
    os.rename(portfile + ".tmp", portfile)
    print("listening: %d" % port, flush=True)
    sys.stdin.read()
    s.stop(0)


def service_account_token(path):
    """OAuth 2 JWT bearer grant (RFC 7523) with the service account key, as google-auth does it."""
    import json
    import urllib.parse
    import urllib.request
    import jwt
    sa = json.load(open(path))
    now = int(time.time())
    claims = {"iss": sa["client_email"], "scope": "https://www.googleapis.com/auth/pubsub", "aud": sa["token_uri"], "iat": now, "exp": now + 3600}
    assertion = jwt.encode(claims, sa["private_key"], algorithm="RS256", headers={"kid": sa.get("private_key_id")})
    body = urllib.parse.urlencode({"grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer", "assertion": assertion}).encode()
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    return json.load(opener.open(urllib.request.Request(sa["token_uri"], data=body), timeout=10))["access_token"]


def pubsub(target, subscription, count, sa_file=None):
    token = service_account_token(sa_file) if sa_file else None
    d = tempfile.mkdtemp()
    with open(os.path.join(d, "pubsub_min.proto"), "w") as f:
        f.write(PUBSUB_PROTO)
    compile_protos(d, ["pubsub_min.proto"])
    pb = importlib.import_module("pubsub_min_pb2")
    rpc = importlib.import_module("pubsub_min_pb2_grpc")
    channel = grpc.insecure_channel(target, options=[("grpc.enable_http_proxy", 0)])
    stub = rpc.SubscriberStub(channel)
    md = [("authorization", "Bearer " + token)] if token else []
    acks = []
    cond = threading.Condition()
    received = []

    def requests():
        yield pb.StreamingPullRequest(subscription=subscription, stream_ack_deadline_seconds=30, client_id="grpcio")
        sent = 0
        deadline = time.time() + 20
        while time.time() < deadline:
            with cond:
                cond.wait(0.2)
                pending = acks[sent:]
                sent = len(acks)
            if pending:
                yield pb.StreamingPullRequest(ack_ids=pending)
            if len(received) >= count and sent >= count:
                time.sleep(0.5)
                return

    confirmations = []
    try:
        for resp in stub.StreamingPull(requests(), metadata=md, timeout=30):
            for m in resp.received_messages:
                received.append(m)
                with cond:
                    acks.append(m.ack_id)
                    cond.notify()
            if resp.HasField("acknowledge_confirmation"):
                confirmations.extend(resp.acknowledge_confirmation.ack_ids)
    except grpc.RpcError as e:
        if e.code() not in (grpc.StatusCode.CANCELLED, grpc.StatusCode.DEADLINE_EXCEEDED, grpc.StatusCode.OK):
            print("error: %s %s" % (e.code().name, e.details()))
    print("received: %s" % ",".join(sorted("%s/%s" % (m.message.data.decode(), m.message.attributes.get("k", "")) for m in received)))
    print("message-ids: %d" % len({m.message.message_id for m in received}))
    print("acked: %d" % len(acks))
    channel.close()


def main():
    print("version: grpcio %s" % grpc.__version__, flush=True)
    cmd = sys.argv[1]
    a = sys.argv[2:]
    if cmd == "client":
        client(*a)
    elif cmd == "server":
        server(a[0], a[1], *(a[2:4] if len(a) > 3 else []))
    elif cmd == "pubsub":
        pubsub(a[0], a[1], int(a[2]), a[3] if len(a) > 3 else None)
    else:
        raise SystemExit("unknown command")


if __name__ == "__main__":
    main()
