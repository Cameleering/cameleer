"""azure-identity + azure-keyvault-secrets / -keys and azure-cosmos against rust-camel's
AzureMock (HTTPS, Entra ID client-credentials tokens, Key Vault challenge auth, Cosmos DB
master-key signatures).

usage: azure_services.py <https-url> <ca-pem> <check> [check...]
checks: keyvault-secrets, keyvault-keys, cosmos, identity-bad-secret
"""
import os
import sys
import traceback

TENANT = "00000000-0000-0000-0000-00000000aaaa"
CLIENT = "11111111-2222-3333-4444-555555555555"
SECRET = "mock~client~secret"
COSMOS_KEY = "C2y6yDjf5/R+ob0N8A7Cgv30VRDJIWEHLM+4QDU5DE2nQ9nDuVTqobD4b8mGGyPMbIZnqyMsEcaGQy67XIw/Jw=="
URL = CA = None


def credential(secret=SECRET):
    from azure.identity import ClientSecretCredential
    return ClientSecretCredential(TENANT, CLIENT, secret, authority=URL, connection_verify=CA, disable_instance_discovery=True)


def keyvault_secrets():
    from azure.keyvault.secrets import SecretClient
    from azure.core.exceptions import ResourceNotFoundError
    c = SecretClient(vault_url=URL, credential=credential(), connection_verify=CA, verify_challenge_resource=False)
    s = c.set_secret("db-password", "s3cr3t", content_type="text/plain", tags={"env": "test"})
    print("kv-set:", s.name, s.value, s.properties.content_type, s.properties.tags)
    print("kv-get:", c.get_secret("db-password").value)
    s2 = c.set_secret("db-password", "rotated")
    print("kv-version:", c.get_secret("db-password").value, s2.properties.version != s.properties.version)
    print("kv-get-version:", c.get_secret("db-password", s.properties.version).value)
    print("kv-versions:", len(list(c.list_properties_of_secret_versions("db-password"))))
    print("kv-list:", sorted(p.name for p in c.list_properties_of_secrets()))
    c.update_secret_properties("db-password", enabled=True, tags={"env": "prod"})
    print("kv-updated-tags:", c.get_secret("db-password").properties.tags)
    poller = c.begin_delete_secret("db-password")
    d = poller.result()
    print("kv-deleted:", d.name, d.recovery_id is not None)
    try:
        c.get_secret("db-password")
        print("kv-after-delete: found")
    except ResourceNotFoundError as e:
        print("kv-after-delete:", e.status_code)
    print("kv-deleted-list:", [x.name for x in c.list_deleted_secrets()])
    c.begin_recover_deleted_secret("db-password").result()
    print("kv-recovered:", c.get_secret("db-password").value)
    c.begin_delete_secret("db-password").result()
    c.purge_deleted_secret("db-password")
    print("kv-purged:", [x.name for x in c.list_deleted_secrets()])


def keyvault_keys():
    from azure.keyvault.keys import KeyClient
    from azure.keyvault.keys.crypto import CryptographyClient, EncryptionAlgorithm, SignatureAlgorithm, KeyWrapAlgorithm
    c = KeyClient(vault_url=URL, credential=credential(), connection_verify=CA, verify_challenge_resource=False)
    rsa = c.create_rsa_key("rsa-key", size=2048)
    print("kk-rsa:", rsa.name, rsa.key_type, len(rsa.key.n) * 8)
    ec = c.create_ec_key("ec-key", curve="P-256")
    print("kk-ec:", ec.key_type, ec.key.crv)
    print("kk-list:", sorted(k.name for k in c.list_properties_of_keys()))
    crypto = CryptographyClient(rsa, credential=credential(), connection_verify=CA, verify_challenge_resource=False)
    enc = crypto.encrypt(EncryptionAlgorithm.rsa_oaep, b"plaintext")
    print("kk-decrypt:", crypto.decrypt(EncryptionAlgorithm.rsa_oaep, enc.ciphertext).plaintext.decode())
    import hashlib
    digest = hashlib.sha256(b"message").digest()
    sig = crypto.sign(SignatureAlgorithm.rs256, digest)
    print("kk-verify:", crypto.verify(SignatureAlgorithm.rs256, digest, sig.signature).is_valid,
          crypto.verify(SignatureAlgorithm.rs256, hashlib.sha256(b"other").digest(), sig.signature).is_valid)
    wrapped = crypto.wrap_key(KeyWrapAlgorithm.rsa_oaep, b"\x01" * 32)
    print("kk-unwrap:", crypto.unwrap_key(KeyWrapAlgorithm.rsa_oaep, wrapped.encrypted_key).key == b"\x01" * 32)
    ecc = CryptographyClient(ec, credential=credential(), connection_verify=CA, verify_challenge_resource=False)
    s = ecc.sign(SignatureAlgorithm.es256, digest)
    print("kk-ec-verify:", ecc.verify(SignatureAlgorithm.es256, digest, s.signature).is_valid)
    # verify the RSA signature locally with the public key (independent of the emulator)
    from cryptography.hazmat.primitives.asymmetric import rsa as crsa, padding
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.asymmetric.utils import Prehashed
    pub = crsa.RSAPublicNumbers(int.from_bytes(rsa.key.e, "big"), int.from_bytes(rsa.key.n, "big")).public_key()
    pub.verify(sig.signature, digest, padding.PKCS1v15(), Prehashed(hashes.SHA256()))
    print("kk-local-verify: True")
    ct = pub.encrypt(b"local", padding.OAEP(mgf=padding.MGF1(hashes.SHA1()), algorithm=hashes.SHA1(), label=None))
    print("kk-local-encrypt:", crypto.decrypt(EncryptionAlgorithm.rsa_oaep, ct).plaintext.decode())
    c.begin_delete_key("ec-key").result()
    print("kk-deleted:", [k.name for k in c.list_deleted_keys()])


def cosmos():
    from azure.cosmos import cosmos_client, documents, errors
    import azure.cosmos
    policy = documents.ConnectionPolicy()
    policy.SSLConfiguration = documents.SSLConfiguration()
    policy.SSLConfiguration.SSLCaCerts = CA
    policy.EnableEndpointDiscovery = False
    c = cosmos_client.CosmosClient(URL, {"masterKey": COSMOS_KEY}, policy)
    db = c.CreateDatabase({"id": "official"})
    print("cosmos-db:", db["id"], "_self" in db)
    coll = c.CreateContainer(db["_self"] if False else "dbs/official", {"id": "people", "partitionKey": {"paths": ["/city"], "kind": "Hash"}})
    print("cosmos-coll:", coll["id"])
    link = "dbs/official/colls/people"
    for i, (name, city, age) in enumerate([("ada", "london", 36), ("alan", "london", 41), ("grace", "nyc", 85)]):
        c.CreateItem(link, {"id": str(i), "name": name, "city": city, "age": age})
    item = c.ReadItem(link + "/docs/1", {"partitionKey": "london"})
    print("cosmos-read:", item["name"], item["age"], "_etag" in item)
    q = list(c.QueryItems(link, {"query": "SELECT * FROM c WHERE c.city = @city ORDER BY c.age DESC", "parameters": [{"name": "@city", "value": "london"}]},
                          {"enableCrossPartitionQuery": True}))
    print("cosmos-query:", [x["name"] for x in q])
    item["age"] = 42
    c.ReplaceItem(link + "/docs/1", item, {"partitionKey": "london"})
    print("cosmos-replace:", c.ReadItem(link + "/docs/1", {"partitionKey": "london"})["age"])
    c.UpsertItem(link, {"id": "3", "name": "edsger", "city": "austin", "age": 72})
    print("cosmos-count:", len(list(c.ReadItems(link, {"enableCrossPartitionQuery": True}))))
    c.DeleteItem(link + "/docs/0", {"partitionKey": "london"})
    try:
        c.ReadItem(link + "/docs/0", {"partitionKey": "london"})
        print("cosmos-missing: found")
    except errors.HTTPFailure as e:
        print("cosmos-missing:", e.status_code)
    try:
        c.CreateItem(link, {"id": "1", "name": "dup", "city": "london"})
    except errors.HTTPFailure as e:
        print("cosmos-conflict:", e.status_code)
    bad = cosmos_client.CosmosClient(URL, {"masterKey": "d3Jvbmcga2V5IHdyb25nIGtleSB3cm9uZyBrZXk="}, policy)
    try:
        list(bad.ReadDatabases())
        print("cosmos-bad-key: accepted")
    except errors.HTTPFailure as e:
        print("cosmos-bad-key:", e.status_code)
    c.DeleteDatabase("dbs/official")


def identity_bad_secret():
    from azure.core.exceptions import ClientAuthenticationError
    try:
        credential("wrong").get_token("https://vault.azure.net/.default")
        print("identity: accepted")
    except ClientAuthenticationError as e:
        print("identity-refused:", "invalid_client" in str(e) or "AADSTS" in str(e))


CHECKS = {"keyvault-secrets": keyvault_secrets, "keyvault-keys": keyvault_keys, "cosmos": cosmos, "identity-bad-secret": identity_bad_secret}


def main():
    global URL, CA
    URL, CA = sys.argv[1], sys.argv[2]
    import azure.identity, azure.keyvault.secrets
    print("version: azure-identity %s azure-keyvault-secrets %s" % (azure.identity.__version__, azure.keyvault.secrets.__version__))
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
