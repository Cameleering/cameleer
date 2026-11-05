"""The Azure SDK for Python (azure-storage-blob / -queue / -file-datalake / -file-share) against
Cameleer's AzuriteMock, authorized with Shared Key and with SAS tokens the SDK generates.

usage: azure_storage.py <blob-url> <queue-url> <dfs-url> <file-url> <check> [check...]

URLs are the emulator's service roots (`http://127.0.0.1:port`); the account is Azurite's
development account. Prints `key: value` lines, `ok: <check>` per passing check, and exits 1
if any check failed.
"""
import datetime
import os
import sys
import traceback

import azure.core
from azure.core.exceptions import ResourceNotFoundError, ResourceExistsError, HttpResponseError, ClientAuthenticationError

ACCOUNT = "devstoreaccount1"
KEY = "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw=="
URLS = {}


def conn_str():
    return ("DefaultEndpointsProtocol=http;AccountName=%s;AccountKey=%s;BlobEndpoint=%s/%s;QueueEndpoint=%s/%s;FileEndpoint=%s/%s"
            % (ACCOUNT, KEY, URLS["blob"], ACCOUNT, URLS["queue"], ACCOUNT, URLS["file"], ACCOUNT))


def blob():
    from azure.storage.blob import BlobServiceClient, ContentSettings, BlobBlock, generate_blob_sas, generate_account_sas, \
        BlobSasPermissions, ResourceTypes, AccountSasPermissions, BlobClient, ContainerClient
    svc = BlobServiceClient.from_connection_string(conn_str())
    c = svc.create_container("official")
    try:
        svc.create_container("official")
        print("blob-dup: created")
    except ResourceExistsError as e:
        print("blob-dup:", e.error_code)
    print("blob-containers:", [x.name for x in svc.list_containers(name_starts_with="off")])
    b = c.get_blob_client("dir/hello.txt")
    b.upload_blob(b"hello azure", content_settings=ContentSettings(content_type="text/plain"), metadata={"owner": "sdk", "Mixed": "Case"})
    print("blob-download:", b.download_blob().readall().decode())
    p = b.get_blob_properties()
    print("blob-properties:", p.size, p.content_settings.content_type, sorted(p.metadata.items()), p.blob_type)
    print("blob-range:", b.download_blob(offset=6, length=5).readall().decode())
    try:
        b.upload_blob(b"again")
        print("blob-overwrite: allowed")
    except ResourceExistsError as e:
        print("blob-overwrite:", e.error_code)
    b.upload_blob(b"replaced", overwrite=True)
    for i in range(5):
        c.upload_blob("list/k%d" % i, b"%d" % i)
    names = [x.name for x in c.list_blobs(name_starts_with="list/", results_per_page=2)]
    print("blob-list:", len(names))
    print("blob-walk:", sorted(x.name for x in c.walk_blobs(delimiter="/")))
    # staged blocks (put block + put block list) and a large upload the SDK chunks
    bb = c.get_blob_client("blocks.bin")
    ids = []
    for n in range(3):
        bid = "block-%03d" % n
        bb.stage_block(bid, bytes([65 + n]) * 1000)
        ids.append(BlobBlock(block_id=bid))
    bb.commit_block_list(ids)
    committed, _ = bb.get_block_list()
    print("blob-blocks:", len(committed), bb.download_blob().readall()[::1000].decode())
    big = os.urandom(9 * 1024 * 1024)
    big_client = BlobClient.from_connection_string(conn_str(), "official", "big.bin", max_block_size=4 * 1024 * 1024, max_single_put_size=4 * 1024 * 1024)
    big_client.upload_blob(big)
    print("blob-chunked:", big_client.download_blob(max_concurrency=2).readall() == big, len(big_client.get_block_list()[0]))
    ab = c.get_blob_client("append.log")
    ab.create_append_blob()
    ab.append_block(b"line1\n")
    ab.append_block(b"line2\n")
    print("blob-append:", ab.download_blob().readall().decode().split())
    # copy, snapshot, lease, tags
    dst = c.get_blob_client("copy.txt")
    dst.start_copy_from_url(b.url)
    print("blob-copy:", dst.download_blob().readall().decode())
    lease = b.acquire_lease(lease_duration=15)
    try:
        b.delete_blob()
        print("blob-lease: deleted")
    except HttpResponseError as e:
        print("blob-lease:", e.error_code)
    lease.release()
    snap = b.create_snapshot()
    print("blob-snapshot:", "snapshot" in snap)
    # SAS: blob-level read SAS used anonymously, account SAS for listing, expired SAS refused
    exp = datetime.datetime.utcnow() + datetime.timedelta(hours=1)
    sas = generate_blob_sas(ACCOUNT, "official", "dir/hello.txt", account_key=KEY, permission=BlobSasPermissions(read=True), expiry=exp)
    print("blob-sas-read:", BlobClient.from_blob_url(b.url + "?" + sas).download_blob().readall().decode())
    try:
        BlobClient.from_blob_url(c.get_blob_client("sas-write.txt").url + "?" + sas).upload_blob(b"x")
        print("blob-sas-write: allowed")
    except HttpResponseError as e:
        print("blob-sas-write:", e.status_code)
    acc = generate_account_sas(ACCOUNT, KEY, resource_types=ResourceTypes(container=True, object=True, service=True),
                               permission=AccountSasPermissions(read=True, list=True, write=True, create=True), expiry=exp)
    sas_svc = BlobServiceClient(account_url="%s/%s" % (URLS["blob"], ACCOUNT), credential=acc)
    print("blob-account-sas:", len(list(sas_svc.get_container_client("official").list_blobs(name_starts_with="list/"))))
    sas_svc.get_blob_client("official", "via-sas.txt").upload_blob(b"via account sas")
    print("blob-account-sas-write:", c.get_blob_client("via-sas.txt").download_blob().readall().decode())
    old = generate_blob_sas(ACCOUNT, "official", "dir/hello.txt", account_key=KEY, permission=BlobSasPermissions(read=True),
                            expiry=datetime.datetime.utcnow() - datetime.timedelta(minutes=5))
    try:
        BlobClient.from_blob_url(b.url + "?" + old).download_blob().readall()
        print("blob-sas-expired: allowed")
    except HttpResponseError as e:
        print("blob-sas-expired:", e.status_code, e.error_code)
    bad = BlobServiceClient(account_url="%s/%s" % (URLS["blob"], ACCOUNT), credential={"account_name": ACCOUNT, "account_key": "d3Jvbmcga2V5"})
    try:
        list(bad.list_containers())
        print("blob-bad-key: allowed")
    except HttpResponseError as e:
        print("blob-bad-key:", e.status_code, e.error_code)
    c.delete_blob("list/k0")
    try:
        c.get_blob_client("list/k0").download_blob()
    except ResourceNotFoundError as e:
        print("blob-missing:", e.error_code)
    print("blob-batch-delete:", len(list(c.delete_blobs("list/k1", "list/k2"))))
    svc.delete_container("official")


def queue():
    from azure.storage.queue import QueueServiceClient, QueueClient, generate_queue_sas, QueueSasPermissions, BinaryBase64EncodePolicy, \
        BinaryBase64DecodePolicy
    svc = QueueServiceClient.from_connection_string(conn_str())
    q = svc.create_queue("official", metadata={"purpose": "test"})
    print("queue-list:", [x.name for x in svc.list_queues(name_starts_with="off")])
    q.send_message("hello queue")
    q.send_message("second", visibility_timeout=0, time_to_live=3600)
    peek = q.peek_messages(max_messages=5)
    print("queue-peek:", [m.content for m in peek])
    msgs = list(q.receive_messages(messages_per_page=5, visibility_timeout=30))
    print("queue-receive:", sorted(m.content for m in msgs), [m.dequeue_count for m in msgs])
    m0 = msgs[0]
    upd = q.update_message(m0, content="updated", visibility_timeout=0)
    again = [m for m in q.receive_messages(visibility_timeout=30)]
    print("queue-update:", [m.content for m in again], upd.pop_receipt is not None)
    for m in again + msgs[1:]:
        q.delete_message(m)
    print("queue-props:", q.get_queue_properties().metadata.get("purpose"))
    bq = QueueClient.from_connection_string(conn_str(), "binary", message_encode_policy=BinaryBase64EncodePolicy(),
                                            message_decode_policy=BinaryBase64DecodePolicy())
    bq.create_queue()
    bq.send_message(b"\x00\x01\xff")
    print("queue-binary:", next(iter(bq.receive_messages())).content.hex())
    exp = datetime.datetime.utcnow() + datetime.timedelta(hours=1)
    sas = generate_queue_sas(ACCOUNT, "official", KEY, permission=QueueSasPermissions(add=True, read=True, process=True), expiry=exp)
    sq = QueueClient.from_queue_url(q.url, credential=sas)
    sq.send_message("via sas")
    print("queue-sas:", [m.content for m in sq.receive_messages()])
    q.clear_messages()
    svc.delete_queue("official")
    try:
        svc.get_queue_client("official").get_queue_properties()
    except ResourceNotFoundError as e:
        print("queue-missing:", e.error_code)


def datalake():
    from azure.storage.filedatalake import DataLakeServiceClient
    svc = DataLakeServiceClient(account_url="%s/%s" % (URLS["dfs"], ACCOUNT), credential={"account_name": ACCOUNT, "account_key": KEY})
    fs = svc.create_file_system("lake")
    d = fs.create_directory("raw/2024")
    f = d.create_file("data.csv")
    f.append_data(b"a,b\n", offset=0, length=4)
    f.append_data(b"1,2\n", offset=4, length=4)
    f.flush_data(8)
    print("dfs-read:", f.download_file().readall().decode().split())
    print("dfs-paths:", sorted(p.name for p in fs.get_paths(recursive=True)))
    f.rename_file("lake/raw/2024/renamed.csv")
    print("dfs-renamed:", sorted(p.name for p in fs.get_paths(path="raw/2024")))
    print("dfs-props:", fs.get_file_client("raw/2024/renamed.csv").get_file_properties().size)
    fs.get_directory_client("raw").delete_directory()
    print("dfs-deleted:", list(p.name for p in fs.get_paths()))
    svc.delete_file_system("lake")


def fileshare():
    from azure.storage.fileshare import ShareServiceClient
    svc = ShareServiceClient.from_connection_string(conn_str())
    share = svc.create_share("docs")
    share.create_directory("reports")
    fc = share.get_file_client("reports/q1.txt")
    fc.upload_file(b"quarterly report")
    print("file-read:", fc.download_file().readall().decode())
    print("file-list:", [x["name"] for x in share.get_directory_client("reports").list_directories_and_files()])
    print("file-props:", fc.get_file_properties().size)
    fc.delete_file()
    print("file-after-delete:", [x["name"] for x in share.get_directory_client("reports").list_directories_and_files()])
    svc.delete_share("docs")


def interop_seed():
    from azure.storage.blob import BlobServiceClient, ContentSettings
    svc = BlobServiceClient.from_connection_string(conn_str())
    c = svc.create_container("interop")
    c.upload_blob("from sdk/ä b.txt", b"written by the Azure SDK", content_settings=ContentSettings(content_type="text/plain"), metadata={"origin": "sdk"})
    from azure.storage.queue import QueueClient
    q = QueueClient.from_connection_string(conn_str(), "interop-in")
    q.create_queue()
    q.send_message("queued by the Azure SDK")
    print("seeded: ok")


def interop_check():
    from azure.storage.blob import BlobServiceClient
    svc = BlobServiceClient.from_connection_string(conn_str())
    b = svc.get_blob_client("interop", "from camel/ü.txt")
    p = b.get_blob_properties()
    print("check-blob:", b.download_blob().readall().decode(), p.content_settings.content_type, p.metadata.get("origin"))
    from azure.storage.queue import QueueClient
    msgs = list(QueueClient.from_connection_string(conn_str(), "interop-out").receive_messages())
    print("check-queue:", [m.content for m in msgs])


CHECKS = {"interop-seed": interop_seed, "interop-check": interop_check, "blob": blob, "queue": queue, "datalake": datalake, "fileshare": fileshare}


def main():
    URLS["blob"], URLS["queue"], URLS["dfs"], URLS["file"] = sys.argv[1:5]
    import azure.storage.blob, azure.storage.queue
    print("version: azure-core %s azure-storage-blob %s azure-storage-queue %s" % (azure.core.__version__, azure.storage.blob.__version__, azure.storage.queue.__version__))
    failed = False
    for name in sys.argv[5:]:
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
