"""pymongo against Cameleer's embedded MongoDB server.

usage: mongo_pymongo.py <uri> <command>

commands:
  crud        insert/find/update/replace/delete, indexes, sort/skip/limit/projection, cursors
              (batching + getMore), bulk writes, distinct, count, findAndModify, binary/date types
  aggregate   $match/$group/$sort/$project/$unwind/$limit/$count pipelines
  watch       a change stream (insert / update / delete events)
  seed        inserts documents for Cameleer to read (db camel, collection fromPymongo)
  check       reads the documents Cameleer wrote (db camel, collection fromCamel)
  auth-fail   expects an authentication failure
Prints `key: value` lines.
"""
import datetime
import sys
import threading
import time

import bson
import pymongo
from pymongo import MongoClient, ReturnDocument, InsertOne, UpdateOne, DeleteOne, ReplaceOne
from pymongo.errors import OperationFailure, DuplicateKeyError


def client(uri):
    return MongoClient(uri, serverSelectionTimeoutMS=10000, connectTimeoutMS=10000)


def crud(uri):
    c = client(uri)
    print("server-version:", c.server_info().get("version"))
    db = c.official
    db.drop_collection("people")
    people = db.people
    r = people.insert_one({"_id": 1, "name": "ada", "age": 36, "tags": ["math", "code"], "born": datetime.datetime(1815, 12, 10),
                           "blob": bson.Binary(b"\x00\x01\xff"), "nested": {"x": 1.5, "y": None}})
    print("inserted-one:", r.inserted_id)
    r = people.insert_many([{"name": "alan", "age": 41, "tags": ["code"]}, {"name": "grace", "age": 85, "tags": ["navy", "code"]},
                            {"name": "edsger", "age": 72, "tags": []}])
    print("inserted-many:", len(r.inserted_ids))
    try:
        people.insert_one({"_id": 1, "name": "dup"})
        print("duplicate: not-raised")
    except DuplicateKeyError as e:
        print("duplicate: E%d" % e.code)
    doc = people.find_one({"_id": 1})
    print("find-one: %s %d %s %s %s" % (doc["name"], doc["age"], doc["born"].year, bytes(doc["blob"]).hex(), doc["nested"]["x"]))
    names = [d["name"] for d in people.find({"tags": "code"}, {"name": 1, "_id": 0}).sort("age", pymongo.DESCENDING)]
    print("find-sorted:", ",".join(names))
    names = [d["name"] for d in people.find({"age": {"$gte": 40, "$lt": 80}}).sort([("name", 1)]).skip(1).limit(5)]
    print("find-range:", ",".join(names))
    print("count:", people.count_documents({"tags": {"$size": 0}}))
    print("estimated:", people.estimated_document_count())
    print("distinct:", ",".join(sorted(people.distinct("tags"))))
    r = people.update_one({"name": "alan"}, {"$set": {"age": 42}, "$push": {"tags": "crypto"}, "$inc": {"visits": 1}})
    print("update-one: %d %d" % (r.matched_count, r.modified_count))
    r = people.update_many({"age": {"$gt": 50}}, {"$set": {"senior": True}})
    print("update-many: %d %d" % (r.matched_count, r.modified_count))
    r = people.update_one({"name": "linus"}, {"$set": {"age": 30}}, upsert=True)
    print("upsert:", r.upserted_id is not None)
    r = people.replace_one({"name": "edsger"}, {"name": "edsger", "age": 72, "quote": "goto"})
    print("replace: %d" % r.modified_count)
    d = people.find_one_and_update({"name": "grace"}, {"$inc": {"age": 1}}, return_document=ReturnDocument.AFTER)
    print("find-and-modify:", d["age"])
    r = people.bulk_write([InsertOne({"name": "barbara", "age": 80}), UpdateOne({"name": "ada"}, {"$set": {"x": 1}}),
                           ReplaceOne({"name": "linus"}, {"name": "linus", "age": 31}), DeleteOne({"name": "barbara"})])
    print("bulk: %d %d %d %d" % (r.inserted_count, r.matched_count, r.modified_count, r.deleted_count))
    idx = people.create_index([("name", pymongo.ASCENDING)], unique=True)
    print("index:", idx)
    print("indexes:", ",".join(sorted(i["name"] for i in people.list_indexes())))
    r = people.delete_many({"senior": True})
    print("delete-many:", r.deleted_count)
    # cursor batching: 250 documents read with batch size 50 (getMore round trips)
    db.drop_collection("many")
    db.many.insert_many([{"i": i, "pad": "x" * 100} for i in range(250)])
    total = sum(1 for _ in db.many.find({}, batch_size=50))
    print("cursor:", total)
    print("collections:", ",".join(sorted(db.list_collection_names())))
    print("databases-has-official:", "official" in c.list_database_names())
    db.drop_collection("many")
    print("dropped:", "many" not in db.list_collection_names())


def aggregate(uri):
    c = client(uri)
    coll = c.official.sales
    coll.drop()
    coll.insert_many([
        {"item": "apple", "qty": 5, "price": 1.5, "tags": ["fruit", "red"]},
        {"item": "pear", "qty": 2, "price": 2.0, "tags": ["fruit"]},
        {"item": "apple", "qty": 3, "price": 1.5, "tags": ["fruit", "red"]},
        {"item": "carrot", "qty": 10, "price": 0.5, "tags": ["veg"]},
    ])
    out = list(coll.aggregate([
        {"$match": {"qty": {"$gte": 2}}},
        {"$group": {"_id": "$item", "total": {"$sum": "$qty"}, "revenue": {"$sum": {"$multiply": ["$qty", "$price"]}}, "n": {"$sum": 1}}},
        {"$sort": {"total": -1}},
    ]))
    print("group:", ";".join("%s=%d/%.1f/%d" % (d["_id"], d["total"], d["revenue"], d["n"]) for d in out))
    out = list(coll.aggregate([{"$unwind": "$tags"}, {"$group": {"_id": "$tags", "c": {"$sum": 1}}}, {"$sort": {"_id": 1}}]))
    print("unwind:", ";".join("%s=%d" % (d["_id"], d["c"]) for d in out))
    out = list(coll.aggregate([{"$project": {"_id": 0, "item": 1, "big": {"$gt": ["$qty", 4]}}}, {"$sort": {"item": 1}}, {"$limit": 2}]))
    print("project:", ";".join("%s=%s" % (d["item"], d["big"]) for d in out))
    out = list(coll.aggregate([{"$match": {"tags": "fruit"}}, {"$count": "fruits"}]))
    print("count:", out[0]["fruits"])


def watch(uri):
    c = client(uri)
    coll = c.official.events
    coll.drop()
    coll.insert_one({"seed": True})
    events = []
    ready = threading.Event()

    def listen():
        with coll.watch(full_document="updateLookup", max_await_time_ms=500) as stream:
            ready.set()
            for ch in stream:
                events.append(ch)
                if len(events) >= 3:
                    return

    t = threading.Thread(target=listen)
    t.start()
    ready.wait(10)
    time.sleep(0.5)
    coll.insert_one({"_id": "e1", "v": 1})
    coll.update_one({"_id": "e1"}, {"$set": {"v": 2}})
    deadline = time.time() + 15
    while len(events) < 2 and time.time() < deadline:
        time.sleep(0.05)
    coll.delete_one({"_id": "e1"})
    t.join(30)
    print("events:", ",".join(e["operationType"] for e in events))
    for e in events:
        if e["operationType"] == "update":
            print("update-fields:", e["updateDescription"]["updatedFields"])
            print("full-document:", (e.get("fullDocument") or {}).get("v"))
    print("resume-token:", "_data" in events[-1]["_id"] if events else False)


def seed(uri):
    c = client(uri)
    coll = c.camel.fromPymongo
    coll.drop()
    coll.insert_many([{"_id": "p%d" % i, "n": i, "when": datetime.datetime(2024, 1, 1 + i), "bin": bson.Binary(bytes([i, 255])),
                       "oid": bson.ObjectId(), "dec": bson.Decimal128("1.%d" % i), "i64": bson.Int64(2 ** 40 + i)} for i in range(3)])
    print("seeded:", coll.count_documents({}))


def check(uri):
    c = client(uri)
    for d in c.camel.fromCamel.find().sort("n", 1):
        print("doc: %s %s %s %s" % (d.get("name"), d.get("n"), type(d.get("n")).__name__, d.get("nested", {}).get("ok")))


def auth_fail(uri):
    try:
        client(uri).official.command("ping")
        print("auth: ok")
    except OperationFailure as e:
        print("auth-failed: %s %s" % (e.code, e))


def whoami(uri):
    c = client(uri)
    c.official.x509.insert_one({"via": "x509"})
    print("x509-insert:", c.official.x509.count_documents({}))


def main():
    uri, cmd = sys.argv[1], sys.argv[2]
    print("version: pymongo", pymongo.version)
    {"crud": crud, "aggregate": aggregate, "watch": watch, "seed": seed, "check": check, "auth-fail": auth_fail, "whoami": whoami}[cmd](uri)


if __name__ == "__main__":
    main()
