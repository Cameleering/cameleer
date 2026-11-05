"""boto3 / botocore (the official AWS SDK for Python) against Cameleer's AwsMock.

usage: aws_boto3.py <endpoint> <service> [service...]

Every request is signed with SigV4 by botocore and must be verified by the emulator.
Credentials come from AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY (and AWS_SESSION_TOKEN).
Each service check prints `key: value` lines; a failing check prints `FAIL <service>: ...`
and the script exits 1 after running all requested services.
"""
import base64
import hashlib
import io
import json
import os
import sys
import time
import traceback
import urllib.request
import zlib

import boto3
import botocore
from botocore.config import Config
from botocore.exceptions import ClientError

ENDPOINT = None
REGION = "us-east-1"


def client(name, **kw):
    cfg = Config(retries={"max_attempts": 1}, s3={"addressing_style": "path"}, **kw)
    return boto3.client(name, endpoint_url=ENDPOINT, region_name=REGION, config=cfg)


def urlopen(req):
    """urllib with the same CA bundle botocore uses (AWS_CA_BUNDLE)."""
    import ssl
    ctx = ssl.create_default_context(cafile=os.environ["AWS_CA_BUNDLE"]) if os.environ.get("AWS_CA_BUNDLE") else None
    return urllib.request.urlopen(req, timeout=10, context=ctx)


def code(e):
    return e.response["Error"]["Code"]


def s3():
    c = client("s3", signature_version="s3v4")
    c.create_bucket(Bucket="official")
    print("s3-buckets:", ",".join(b["Name"] for b in c.list_buckets()["Buckets"] if b["Name"] == "official"))
    r = c.put_object(Bucket="official", Key="dir/hello.txt", Body=b"hello world", ContentType="text/plain",
                     Metadata={"owner": "boto3", "mixed-Case": "v"})
    print("s3-put-etag:", r["ETag"].strip('"') == hashlib.md5(b"hello world").hexdigest())
    g = c.get_object(Bucket="official", Key="dir/hello.txt")
    print("s3-get:", g["Body"].read().decode(), g["ContentType"], g["ContentLength"], sorted(g["Metadata"].items()))
    h = c.head_object(Bucket="official", Key="dir/hello.txt")
    print("s3-head:", h["ContentLength"], h["ETag"] == r["ETag"])
    print("s3-range:", c.get_object(Bucket="official", Key="dir/hello.txt", Range="bytes=6-10")["Body"].read().decode())
    # unicode / special characters in keys
    key = "spécial dir/a+b=c&d?.txt"
    c.put_object(Bucket="official", Key=key, Body=b"x")
    print("s3-special-key:", c.get_object(Bucket="official", Key=key)["Body"].read().decode())
    for i in range(5):
        c.put_object(Bucket="official", Key="list/k%d" % i, Body=b"%d" % i)
    l = c.list_objects_v2(Bucket="official", Prefix="list/", MaxKeys=2)
    keys = [o["Key"] for o in l["Contents"]]
    pages = 1
    while l.get("IsTruncated"):
        l = c.list_objects_v2(Bucket="official", Prefix="list/", MaxKeys=2, ContinuationToken=l["NextContinuationToken"])
        keys += [o["Key"] for o in l["Contents"]]
        pages += 1
    print("s3-list: %d keys %d pages" % (len(keys), pages))
    d = c.list_objects_v2(Bucket="official", Delimiter="/")
    print("s3-delimiter:", ",".join(p["Prefix"] for p in d.get("CommonPrefixes", [])))
    print("s3-list-v1:", len(c.list_objects(Bucket="official", Prefix="list/")["Contents"]))
    c.copy_object(Bucket="official", Key="copy.txt", CopySource={"Bucket": "official", "Key": "dir/hello.txt"})
    print("s3-copy:", c.get_object(Bucket="official", Key="copy.txt")["Body"].read().decode())
    r = c.delete_objects(Bucket="official", Delete={"Objects": [{"Key": "list/k0"}, {"Key": "list/k1"}]})
    print("s3-delete-objects:", len(r.get("Deleted", [])))
    c.delete_object(Bucket="official", Key="copy.txt")
    try:
        c.get_object(Bucket="official", Key="copy.txt")
        print("s3-missing: found")
    except ClientError as e:
        print("s3-missing:", code(e), e.response["ResponseMetadata"]["HTTPStatusCode"])
    try:
        c.head_object(Bucket="official", Key="nope")
    except ClientError as e:
        print("s3-head-missing:", e.response["ResponseMetadata"]["HTTPStatusCode"])
    # multipart upload (3 parts) and the managed transfer (upload_fileobj with small thresholds)
    mp = c.create_multipart_upload(Bucket="official", Key="big.bin", ContentType="application/octet-stream")
    parts = []
    data = b""
    for n in range(1, 4):
        chunk = bytes([n]) * (5 * 1024 * 1024 if n < 3 else 1000)
        data += chunk
        r = c.upload_part(Bucket="official", Key="big.bin", PartNumber=n, UploadId=mp["UploadId"], Body=chunk)
        parts.append({"ETag": r["ETag"], "PartNumber": n})
    print("s3-list-parts:", len(c.list_parts(Bucket="official", Key="big.bin", UploadId=mp["UploadId"])["Parts"]))
    r = c.complete_multipart_upload(Bucket="official", Key="big.bin", UploadId=mp["UploadId"], MultipartUpload={"Parts": parts})
    print("s3-multipart-etag:", r["ETag"].strip('"').endswith("-3"))
    body = c.get_object(Bucket="official", Key="big.bin")["Body"].read()
    print("s3-multipart:", len(body), body == data)
    mp = c.create_multipart_upload(Bucket="official", Key="aborted.bin")
    c.abort_multipart_upload(Bucket="official", Key="aborted.bin", UploadId=mp["UploadId"])
    print("s3-abort:", "aborted.bin" not in [o["Key"] for o in c.list_objects_v2(Bucket="official").get("Contents", [])])
    from boto3.s3.transfer import TransferConfig
    payload = os.urandom(12 * 1024 * 1024)
    c.upload_fileobj(io.BytesIO(payload), "official", "managed.bin", Config=TransferConfig(multipart_threshold=5 * 1024 * 1024, multipart_chunksize=5 * 1024 * 1024))
    out = io.BytesIO()
    c.download_fileobj("official", "managed.bin", out, Config=TransferConfig(multipart_threshold=5 * 1024 * 1024, multipart_chunksize=5 * 1024 * 1024))
    print("s3-managed-transfer:", out.getvalue() == payload)
    # presigned GET and PUT used through plain urllib (query-string SigV4)
    url = c.generate_presigned_url("get_object", Params={"Bucket": "official", "Key": "dir/hello.txt"}, ExpiresIn=300)
    print("s3-presigned-get:", urlopen(url).read().decode())
    url = c.generate_presigned_url("put_object", Params={"Bucket": "official", "Key": "presigned.txt"}, ExpiresIn=300)
    req = urllib.request.Request(url, data=b"via presigned put", method="PUT")
    urlopen(req).read()
    print("s3-presigned-put:", c.get_object(Bucket="official", Key="presigned.txt")["Body"].read().decode())
    bad = url.replace("X-Amz-Signature=", "X-Amz-Signature=0")
    try:
        urlopen(urllib.request.Request(bad, data=b"x", method="PUT"))
        print("s3-presigned-tampered: accepted")
    except urllib.error.HTTPError as e:
        print("s3-presigned-tampered:", e.code)
    post = c.generate_presigned_post("official", "post.txt", ExpiresIn=300)
    print("s3-presigned-post-fields:", "x-amz-signature" in post["fields"])
    c.put_object_tagging(Bucket="official", Key="dir/hello.txt", Tagging={"TagSet": [{"Key": "env", "Value": "test"}]})
    print("s3-tagging:", c.get_object_tagging(Bucket="official", Key="dir/hello.txt")["TagSet"])


def attributes_md5(attrs):
    """MD5OfMessageAttributes per the SQS developer guide (independent of the emulator)."""
    buf = b""
    for name in sorted(attrs):
        dtype, value = attrs[name]
        for part in (name.encode(), dtype.encode()):
            buf += len(part).to_bytes(4, "big") + part
        buf += (b"\x02" if dtype.startswith("Binary") else b"\x01") + len(value).to_bytes(4, "big") + value
    return hashlib.md5(buf).hexdigest()


def sqs():
    c = client("sqs")
    url = c.create_queue(QueueName="official", Attributes={"VisibilityTimeout": "30"})["QueueUrl"]
    print("sqs-url:", url.endswith("/official"))
    print("sqs-get-url:", c.get_queue_url(QueueName="official")["QueueUrl"] == url)
    r = c.send_message(QueueUrl=url, MessageBody="hello sqs", MessageAttributes={"kind": {"DataType": "String", "StringValue": "greeting"},
                                                                                 "n": {"DataType": "Number", "StringValue": "42"},
                                                                                 "b": {"DataType": "Binary", "BinaryValue": b"\x00\x01"}})
    print("sqs-md5:", r["MD5OfMessageBody"] == hashlib.md5(b"hello sqs").hexdigest(), r.get("MD5OfMessageAttributes") == attributes_md5({
        "kind": ("String", b"greeting"), "n": ("Number", b"42"), "b": ("Binary", b"\x00\x01")}))
    r = c.send_message_batch(QueueUrl=url, Entries=[{"Id": "a", "MessageBody": "batch-a"}, {"Id": "b", "MessageBody": "batch-b", "DelaySeconds": 0}])
    print("sqs-batch:", len(r["Successful"]), len(r.get("Failed", [])))
    got = []
    deadline = time.time() + 10
    while len(got) < 3 and time.time() < deadline:
        r = c.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=1, MessageAttributeNames=["All"], AttributeNames=["All"])
        got += r.get("Messages", [])
    first = [m for m in got if m["Body"] == "hello sqs"][0]
    attrs = first["MessageAttributes"]
    print("sqs-receive:", sorted(m["Body"] for m in got))
    print("sqs-attributes:", attrs["kind"]["StringValue"], attrs["n"]["StringValue"], attrs["b"]["BinaryValue"].hex(),
          first["Attributes"].get("ApproximateReceiveCount"))
    c.change_message_visibility(QueueUrl=url, ReceiptHandle=got[1]["ReceiptHandle"], VisibilityTimeout=0)
    r = c.delete_message_batch(QueueUrl=url, Entries=[{"Id": str(i), "ReceiptHandle": m["ReceiptHandle"]} for i, m in enumerate(got)])
    print("sqs-delete-batch:", len(r["Successful"]))
    a = c.get_queue_attributes(QueueUrl=url, AttributeNames=["All"])["Attributes"]
    print("sqs-queue-attributes:", a.get("VisibilityTimeout"), "QueueArn" in a)
    c.set_queue_attributes(QueueUrl=url, Attributes={"DelaySeconds": "1"})
    print("sqs-list:", any(u.endswith("/official") for u in c.list_queues(QueueNamePrefix="off").get("QueueUrls", [])))
    fifo = c.create_queue(QueueName="official.fifo", Attributes={"FifoQueue": "true", "ContentBasedDeduplication": "true"})["QueueUrl"]
    for i in range(3):
        c.send_message(QueueUrl=fifo, MessageBody="f%d" % i, MessageGroupId="g")
    c.send_message(QueueUrl=fifo, MessageBody="f0", MessageGroupId="g")
    bodies = [m["Body"] for m in c.receive_message(QueueUrl=fifo, MaxNumberOfMessages=10).get("Messages", [])]
    print("sqs-fifo:", ",".join(bodies))
    c.purge_queue(QueueUrl=url)
    try:
        c.get_queue_url(QueueName="nope")
    except ClientError as e:
        print("sqs-missing:", code(e))
    c.delete_queue(QueueUrl=url)


def sqs_json():
    """SQS over the AWS JSON 1.0 protocol (what botocore >= 1.34.90 and the Java SDK v2 use):
    the packaged model is the Query one, so a copy with protocol json is loaded."""
    import tempfile
    src = os.path.join(os.path.dirname(botocore.__file__), "data", "sqs", "2012-11-05", "service-2.json")
    model = json.load(open(src))
    model["metadata"].update({"protocol": "json", "jsonVersion": "1.0", "targetPrefix": "AmazonSQS", "awsQueryCompatible": {}})
    for shape in model["shapes"].values():
        # JSON uses member names; flattened Query lists become plain lists
        shape.pop("flattened", None)
        shape.pop("locationName", None)
        for part in ("member", "key", "value"):
            if isinstance(shape.get(part), dict):
                shape[part].pop("locationName", None)
        for m in shape.get("members", {}).values():
            m.pop("locationName", None)
            m.pop("flattened", None)
    d = tempfile.mkdtemp()
    os.makedirs(os.path.join(d, "sqs", "2012-11-05"))
    json.dump(model, open(os.path.join(d, "sqs", "2012-11-05", "service-2.json"), "w"))
    sess = botocore.session.get_session()
    sess.set_config_variable("data_path", d)
    loader = sess.get_component("data_loader")
    loader.search_paths.insert(0, d)
    c = boto3.session.Session(botocore_session=sess, region_name=REGION).client("sqs", endpoint_url=ENDPOINT, config=Config(retries={"max_attempts": 1}))
    print("sqsjson-protocol:", c.meta.service_model.protocol)
    url = c.create_queue(QueueName="json-queue")["QueueUrl"]
    c.send_message(QueueUrl=url, MessageBody="over json", MessageAttributes={"k": {"DataType": "String", "StringValue": "v"}})
    r = c.send_message_batch(QueueUrl=url, Entries=[{"Id": "1", "MessageBody": "b1"}, {"Id": "2", "MessageBody": "b2"}])
    print("sqsjson-batch:", len(r["Successful"]))
    msgs = c.receive_message(QueueUrl=url, MaxNumberOfMessages=10, MessageAttributeNames=["All"], WaitTimeSeconds=1).get("Messages", [])
    print("sqsjson-receive:", sorted(m["Body"] for m in msgs), [m.get("MessageAttributes", {}).get("k", {}).get("StringValue") for m in msgs if m["Body"] == "over json"])
    r = c.delete_message_batch(QueueUrl=url, Entries=[{"Id": str(i), "ReceiptHandle": m["ReceiptHandle"]} for i, m in enumerate(msgs)])
    print("sqsjson-delete:", len(r["Successful"]))
    print("sqsjson-attributes:", "QueueArn" in c.get_queue_attributes(QueueUrl=url, AttributeNames=["All"])["Attributes"])
    try:
        c.get_queue_url(QueueName="nope-json")
    except ClientError as e:
        print("sqsjson-missing:", code(e))


def sns():
    c = client("sns")
    arn = c.create_topic(Name="official", Attributes={"DisplayName": "Official"})["TopicArn"]
    print("sns-arn:", arn.endswith(":official"))
    print("sns-list:", arn in [t["TopicArn"] for t in c.list_topics()["Topics"]])
    sq = client("sqs")
    qurl = sq.create_queue(QueueName="sns-target")["QueueUrl"]
    qarn = sq.get_queue_attributes(QueueUrl=qurl, AttributeNames=["QueueArn"])["Attributes"]["QueueArn"]
    sub = c.subscribe(TopicArn=arn, Protocol="sqs", Endpoint=qarn, ReturnSubscriptionArn=True)["SubscriptionArn"]
    print("sns-subscribe:", sub.startswith(arn))
    r = c.publish(TopicArn=arn, Message="hello sns", Subject="greeting", MessageAttributes={"k": {"DataType": "String", "StringValue": "v"}})
    print("sns-publish:", len(r["MessageId"]) > 0)
    r = c.publish(TopicArn=arn, Message=json.dumps({"default": "d", "sqs": "s"}), MessageStructure="json")
    msgs = []
    deadline = time.time() + 10
    while len(msgs) < 2 and time.time() < deadline:
        msgs += sq.receive_message(QueueUrl=qurl, MaxNumberOfMessages=10, WaitTimeSeconds=1).get("Messages", [])
    envelopes = [json.loads(m["Body"]) for m in msgs]
    print("sns-delivered:", sorted(e.get("Message") for e in envelopes))
    print("sns-subscriptions:", len(c.list_subscriptions_by_topic(TopicArn=arn)["Subscriptions"]))
    attrs = c.get_topic_attributes(TopicArn=arn)["Attributes"]
    print("sns-attributes:", attrs.get("DisplayName"))
    c.unsubscribe(SubscriptionArn=sub)
    try:
        c.publish(TopicArn=arn + "-missing", Message="x")
    except ClientError as e:
        print("sns-missing:", code(e))
    c.delete_topic(TopicArn=arn)


def dynamodb():
    c = client("dynamodb")
    c.create_table(TableName="official", AttributeDefinitions=[{"AttributeName": "pk", "AttributeType": "S"}, {"AttributeName": "sk", "AttributeType": "N"}],
                   KeySchema=[{"AttributeName": "pk", "KeyType": "HASH"}, {"AttributeName": "sk", "KeyType": "RANGE"}], BillingMode="PAY_PER_REQUEST")
    c.get_waiter("table_exists").wait(TableName="official", WaiterConfig={"Delay": 1, "MaxAttempts": 5})
    d = c.describe_table(TableName="official")["Table"]
    print("ddb-describe:", d["TableStatus"], [k["AttributeName"] for k in d["KeySchema"]])
    t = boto3.resource("dynamodb", endpoint_url=ENDPOINT, region_name=REGION).Table("official")
    from decimal import Decimal
    for i in range(6):
        t.put_item(Item={"pk": "user", "sk": i, "name": "n%d" % i, "tags": {"a", "b"}, "score": Decimal("1.5") * i, "nested": {"x": [1, "two", None, True]},
                         "blob": b"\x00\xff"})
    item = t.get_item(Key={"pk": "user", "sk": 3})["Item"]
    print("ddb-get:", item["name"], sorted(item["tags"]), item["score"], item["nested"]["x"], bytes(item["blob"]).hex())
    from boto3.dynamodb.conditions import Key, Attr
    r = t.query(KeyConditionExpression=Key("pk").eq("user") & Key("sk").between(1, 4), FilterExpression=Attr("score").gt(2), ScanIndexForward=False)
    print("ddb-query:", [int(i["sk"]) for i in r["Items"]], r["Count"], r["ScannedCount"])
    r = t.query(KeyConditionExpression=Key("pk").eq("user"), Limit=2)
    print("ddb-paging:", len(r["Items"]), "LastEvaluatedKey" in r)
    r = t.update_item(Key={"pk": "user", "sk": 1}, UpdateExpression="SET #n = :n, visits = if_not_exists(visits, :zero) + :one REMOVE tags",
                      ExpressionAttributeNames={"#n": "name"}, ExpressionAttributeValues={":n": "renamed", ":zero": 0, ":one": 1}, ReturnValues="ALL_NEW")
    print("ddb-update:", r["Attributes"]["name"], r["Attributes"]["visits"], "tags" in r["Attributes"])
    try:
        t.put_item(Item={"pk": "user", "sk": 1}, ConditionExpression="attribute_not_exists(pk)")
        print("ddb-condition: accepted")
    except ClientError as e:
        print("ddb-condition:", code(e))
    r = t.scan(FilterExpression=Attr("name").begins_with("n") & Attr("sk").is_in([0, 2, 5]), ProjectionExpression="sk, #nm", ExpressionAttributeNames={"#nm": "name"})
    print("ddb-scan:", sorted(int(i["sk"]) for i in r["Items"]), sorted(r["Items"][0].keys()))
    c.batch_write_item(RequestItems={"official": [{"PutRequest": {"Item": {"pk": {"S": "b"}, "sk": {"N": str(i)}}}} for i in range(3)]})
    r = c.batch_get_item(RequestItems={"official": {"Keys": [{"pk": {"S": "b"}, "sk": {"N": "0"}}, {"pk": {"S": "b"}, "sk": {"N": "2"}}]}})
    print("ddb-batch-get:", len(r["Responses"]["official"]))
    c.transact_write_items(TransactItems=[{"Put": {"TableName": "official", "Item": {"pk": {"S": "tx"}, "sk": {"N": "1"}}}},
                                          {"Delete": {"TableName": "official", "Key": {"pk": {"S": "b"}, "sk": {"N": "0"}}}}])
    print("ddb-transact:", "Item" in c.get_item(TableName="official", Key={"pk": {"S": "tx"}, "sk": {"N": "1"}}))
    try:
        c.transact_write_items(TransactItems=[{"Put": {"TableName": "official", "Item": {"pk": {"S": "tx"}, "sk": {"N": "2"}}}},
                                              {"ConditionCheck": {"TableName": "official", "Key": {"pk": {"S": "tx"}, "sk": {"N": "1"}},
                                                                  "ConditionExpression": "attribute_not_exists(pk)"}}])
        print("ddb-transact-cancel: committed")
    except ClientError as e:
        print("ddb-transact-cancel:", code(e), [r["Code"] for r in e.response.get("CancellationReasons", [])],
              "Item" in c.get_item(TableName="official", Key={"pk": {"S": "tx"}, "sk": {"N": "2"}}))
    t.delete_item(Key={"pk": "user", "sk": 0})
    print("ddb-list:", "official" in c.list_tables()["TableNames"])
    try:
        c.describe_table(TableName="nope")
    except ClientError as e:
        print("ddb-missing:", code(e))
    c.delete_table(TableName="official")


def kinesis():
    c = client("kinesis")
    c.create_stream(StreamName="official", ShardCount=1)
    c.get_waiter("stream_exists").wait(StreamName="official", WaiterConfig={"Delay": 1, "MaxAttempts": 5})
    d = c.describe_stream(StreamName="official")["StreamDescription"]
    shard = d["Shards"][0]["ShardId"]
    print("kinesis-describe:", d["StreamStatus"], len(d["Shards"]))
    r = c.put_record(StreamName="official", Data=b"record-1", PartitionKey="p1")
    print("kinesis-put:", r["ShardId"] == shard, len(r["SequenceNumber"]) > 0)
    r = c.put_records(StreamName="official", Records=[{"Data": b"record-%d" % i, "PartitionKey": "p%d" % i} for i in range(2, 5)])
    print("kinesis-put-records:", r["FailedRecordCount"], len(r["Records"]))
    it = c.get_shard_iterator(StreamName="official", ShardId=shard, ShardIteratorType="TRIM_HORIZON")["ShardIterator"]
    r = c.get_records(ShardIterator=it, Limit=10)
    print("kinesis-get:", [x["Data"].decode() for x in r["Records"]], "NextShardIterator" in r)
    print("kinesis-list:", "official" in c.list_streams()["StreamNames"])
    print("kinesis-shards:", len(c.list_shards(StreamName="official")["Shards"]))
    c.delete_stream(StreamName="official")


def lambda_():
    c = client("lambda")
    r = c.invoke(FunctionName="echo", Payload=json.dumps({"hello": "lambda"}).encode())
    print("lambda-invoke:", r["StatusCode"], json.loads(r["Payload"].read()))
    r = c.invoke(FunctionName="fail", Payload=b"{}")
    print("lambda-function-error:", r.get("FunctionError"), r["Payload"].read().decode()[:60] != "")
    r = c.invoke(FunctionName="echo", InvocationType="Event", Payload=b"{}")
    print("lambda-async:", r["StatusCode"])
    try:
        c.invoke(FunctionName="missing", Payload=b"{}")
    except ClientError as e:
        print("lambda-missing:", code(e))
    names = [f["FunctionName"] for f in c.list_functions().get("Functions", [])]
    print("lambda-list:", "echo" in names)


def kms():
    c = client("kms")
    k = c.create_key(Description="official", KeyUsage="ENCRYPT_DECRYPT")["KeyMetadata"]
    print("kms-key:", k["KeyState"], k["Enabled"])
    c.create_alias(AliasName="alias/official", TargetKeyId=k["KeyId"])
    r = c.encrypt(KeyId="alias/official", Plaintext=b"secret data", EncryptionContext={"app": "camel"})
    blob = r["CiphertextBlob"]
    d = c.decrypt(CiphertextBlob=blob, EncryptionContext={"app": "camel"})
    print("kms-roundtrip:", d["Plaintext"].decode(), d["KeyId"] == k["Arn"] or d["KeyId"].endswith(k["KeyId"]))
    try:
        c.decrypt(CiphertextBlob=blob, EncryptionContext={"app": "other"})
        print("kms-context: accepted")
    except ClientError as e:
        print("kms-context:", code(e))
    dk = c.generate_data_key(KeyId=k["KeyId"], KeySpec="AES_256")
    print("kms-data-key:", len(dk["Plaintext"]), c.decrypt(CiphertextBlob=dk["CiphertextBlob"])["Plaintext"] == dk["Plaintext"])
    print("kms-describe:", c.describe_key(KeyId="alias/official")["KeyMetadata"]["KeyId"] == k["KeyId"])
    print("kms-list:", k["KeyId"] in [x["KeyId"] for x in c.list_keys()["Keys"]])
    c.disable_key(KeyId=k["KeyId"])
    try:
        c.encrypt(KeyId=k["KeyId"], Plaintext=b"x")
    except ClientError as e:
        print("kms-disabled:", code(e))


def secretsmanager():
    c = client("secretsmanager")
    r = c.create_secret(Name="official/db", SecretString=json.dumps({"user": "u", "password": "p"}), Description="d")
    print("sm-create:", r["Name"], r["ARN"].startswith("arn:aws:secretsmanager:"))
    print("sm-get:", json.loads(c.get_secret_value(SecretId="official/db")["SecretString"])["password"])
    c.put_secret_value(SecretId="official/db", SecretString="v2")
    v = c.get_secret_value(SecretId="official/db")
    print("sm-version:", v["SecretString"], "AWSCURRENT" in v["VersionStages"])
    c.create_secret(Name="official/bin", SecretBinary=b"\x00\x01\x02")
    print("sm-binary:", c.get_secret_value(SecretId="official/bin")["SecretBinary"].hex())
    print("sm-describe:", c.describe_secret(SecretId="official/db")["Name"])
    print("sm-list:", sorted(s["Name"] for s in c.list_secrets()["SecretList"] if s["Name"].startswith("official/")))
    c.update_secret(SecretId="official/db", Description="new")
    c.delete_secret(SecretId="official/bin", ForceDeleteWithoutRecovery=True)
    try:
        c.get_secret_value(SecretId="official/bin")
    except ClientError as e:
        print("sm-missing:", code(e))


def sts():
    c = client("sts")
    ident = c.get_caller_identity()
    print("sts-identity:", ident["Account"], "Arn" in ident)
    r = c.assume_role(RoleArn="arn:aws:iam::000000000000:role/official", RoleSessionName="boto3")
    creds = r["Credentials"]
    print("sts-assume:", creds["AccessKeyId"][:4], "SessionToken" in creds, r["AssumedRoleUser"]["Arn"].endswith("/boto3"))
    # the temporary credentials (with the session token) are accepted by other services
    s = boto3.session.Session(aws_access_key_id=creds["AccessKeyId"], aws_secret_access_key=creds["SecretAccessKey"],
                              aws_session_token=creds["SessionToken"], region_name=REGION)
    sq = s.client("sqs", endpoint_url=ENDPOINT, config=Config(retries={"max_attempts": 1}))
    print("sts-temp-creds:", sq.create_queue(QueueName="via-sts")["QueueUrl"].endswith("/via-sts"))
    s2 = boto3.session.Session(aws_access_key_id=creds["AccessKeyId"], aws_secret_access_key=creds["SecretAccessKey"], region_name=REGION)
    try:
        s2.client("sqs", endpoint_url=ENDPOINT, config=Config(retries={"max_attempts": 1})).list_queues()
        print("sts-missing-token: accepted")
    except ClientError as e:
        print("sts-missing-token:", code(e))
    r = c.get_session_token(DurationSeconds=900)
    print("sts-session-token:", "SessionToken" in r["Credentials"])


def badsig():
    s = boto3.session.Session(aws_access_key_id=os.environ["AWS_ACCESS_KEY_ID"], aws_secret_access_key="wrong-secret", region_name=REGION)
    try:
        s.client("sqs", endpoint_url=ENDPOINT, config=Config(retries={"max_attempts": 1})).list_queues()
        print("badsig-sqs: accepted")
    except ClientError as e:
        print("badsig-sqs:", code(e))
    try:
        s.client("s3", endpoint_url=ENDPOINT, config=Config(retries={"max_attempts": 1}, s3={"addressing_style": "path"})).list_buckets()
        print("badsig-s3: accepted")
    except ClientError as e:
        print("badsig-s3:", code(e))
    s = boto3.session.Session(aws_access_key_id="AKIAUNKNOWN", aws_secret_access_key="x", region_name=REGION)
    try:
        s.client("dynamodb", endpoint_url=ENDPOINT, config=Config(retries={"max_attempts": 1})).list_tables()
    except ClientError as e:
        print("badsig-unknown-key:", code(e))


def interop_seed():
    """Data written with boto3 for Cameleer's components to read."""
    s3c = client("s3")
    s3c.create_bucket(Bucket="interop")
    s3c.put_object(Bucket="interop", Key="from boto3/ä+b.txt", Body=b"written by boto3", ContentType="text/plain; charset=utf-8", Metadata={"origin": "boto3"})
    sq = client("sqs")
    url = sq.create_queue(QueueName="interop-in")["QueueUrl"]
    sq.send_message(QueueUrl=url, MessageBody="sqs from boto3", MessageAttributes={"origin": {"DataType": "String", "StringValue": "boto3"},
                                                                                     "count": {"DataType": "Number", "StringValue": "7"}})
    ddb = client("dynamodb")
    ddb.create_table(TableName="interop", AttributeDefinitions=[{"AttributeName": "id", "AttributeType": "S"}],
                     KeySchema=[{"AttributeName": "id", "KeyType": "HASH"}], BillingMode="PAY_PER_REQUEST")
    from decimal import Decimal
    boto3.resource("dynamodb", endpoint_url=ENDPOINT, region_name=REGION).Table("interop").put_item(
        Item={"id": "boto3", "n": 42, "f": Decimal("1.25"), "s": "text", "b": True, "l": ["x", 1], "m": {"k": "v"}, "ss": {"a", "b"}})
    sm = client("secretsmanager")
    sm.create_secret(Name="interop/secret", SecretString="from-boto3")
    print("seeded: ok")


def interop_check():
    """Reads what Cameleer's components wrote."""
    s3c = client("s3")
    o = s3c.get_object(Bucket="interop", Key="from camel/ü b.txt")
    print("check-s3:", o["Body"].read().decode(), o["ContentType"], o["Metadata"].get("origin"))
    sq = client("sqs")
    url = sq.get_queue_url(QueueName="interop-out")["QueueUrl"]
    msgs = sq.receive_message(QueueUrl=url, MaxNumberOfMessages=10, MessageAttributeNames=["All"], WaitTimeSeconds=2).get("Messages", [])
    m = msgs[0]
    attrs = {k: (v["DataType"], v.get("StringValue")) for k, v in m.get("MessageAttributes", {}).items()}
    print("check-sqs:", m["Body"], sorted(attrs.items()), m["MD5OfBody"] == hashlib.md5(m["Body"].encode()).hexdigest())
    t = boto3.resource("dynamodb", endpoint_url=ENDPOINT, region_name=REGION).Table("interop")
    item = t.get_item(Key={"id": "camel"})["Item"]
    print("check-ddb:", item["name"], item["n"], item["flag"], item.get("nested", {}).get("k"))
    print("check-secret:", client("secretsmanager").get_secret_value(SecretId="interop/camel")["SecretString"])


SERVICES = {"interop-seed": interop_seed, "interop-check": interop_check,"s3": s3, "sqs": sqs, "sqs-json": sqs_json, "sns": sns, "dynamodb": dynamodb, "kinesis": kinesis, "lambda": lambda_, "kms": kms,
            "secretsmanager": secretsmanager, "sts": sts, "badsig": badsig}


def main():
    global ENDPOINT
    ENDPOINT = sys.argv[1]
    print("version: boto3 %s botocore %s" % (boto3.__version__, botocore.__version__))
    failed = False
    for name in sys.argv[2:]:
        try:
            SERVICES[name]()
            print("ok:", name)
        except Exception as e:
            failed = True
            print("FAIL %s: %s" % (name, e))
            traceback.print_exc()
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
