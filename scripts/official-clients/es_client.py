"""The official elasticsearch-py client against rust-camel's embedded Elasticsearch server.

usage: es_client.py <url> <command> [user password ca]

commands:
  api        info, index create/exists/delete, index/get/update/delete documents, refresh,
             search (match, term, bool, range, sort, from/size, aggregations), count, bulk
             (helpers.bulk), mget, scroll (helpers.scan), delete_by_query, error mapping
  seed       indexes documents for rust-camel to read (index from-python)
  check      reads documents rust-camel indexed (index from-camel)
  auth-fail  expects a 401
Prints `key: value` lines.
"""
import sys

import elasticsearch
from elasticsearch import Elasticsearch, helpers, NotFoundError, AuthenticationException, ConflictError


def client(url, user=None, password=None, ca=None):
    kw = {"request_timeout": 20}
    if user:
        kw["http_auth"] = (user, password)
    if ca:
        kw["ca_certs"] = ca
        kw["verify_certs"] = True
    return Elasticsearch([url], **kw)


def api(es):
    info = es.info()
    print("info:", info["version"]["number"], info.get("tagline", ""))
    if es.indices.exists(index="books"):
        es.indices.delete(index="books")
    r = es.indices.create(index="books", body={"settings": {"number_of_shards": 1},
                                               "mappings": {"properties": {"title": {"type": "text"}, "year": {"type": "integer"},
                                                                           "genre": {"type": "keyword"}}}})
    print("created:", r["acknowledged"], r["index"])
    print("exists:", es.indices.exists(index="books"))
    docs = [
        {"title": "The Rust Programming Language", "year": 2018, "genre": "tech", "pages": 552},
        {"title": "Enterprise Integration Patterns", "year": 2003, "genre": "tech", "pages": 736},
        {"title": "Camel in Action", "year": 2018, "genre": "tech", "pages": 912},
        {"title": "Dune", "year": 1965, "genre": "scifi", "pages": 412},
        {"title": "Hyperion", "year": 1989, "genre": "scifi", "pages": 482},
    ]
    for i, d in enumerate(docs):
        r = es.index(index="books", id=str(i + 1), body=d)
    print("indexed:", r["result"], r["_version"])
    r = es.index(index="books", body={"title": "No id", "year": 2020, "genre": "misc"})
    print("auto-id:", len(r["_id"]) > 0, r["result"])
    es.indices.refresh(index="books")
    g = es.get(index="books", id="4")
    print("get:", g["found"], g["_source"]["title"])
    try:
        es.get(index="books", id="404")
        print("missing: found")
    except NotFoundError as e:
        print("missing:", e.status_code)
    r = es.update(index="books", id="4", body={"doc": {"pages": 500}})
    print("updated:", r["result"], r["_version"])
    try:
        es.create(index="books", id="4", body={"title": "dup"})
        print("conflict: none")
    except ConflictError as e:
        print("conflict:", e.status_code)
    r = es.search(index="books", body={"query": {"match": {"title": "camel action"}}})
    print("match:", r["hits"]["total"]["value"], r["hits"]["hits"][0]["_source"]["title"])
    r = es.search(index="books", body={"query": {"term": {"genre": "scifi"}}, "sort": [{"year": "asc"}]})
    print("term-sorted:", ",".join(h["_source"]["title"] for h in r["hits"]["hits"]))
    r = es.search(index="books", body={"query": {"bool": {"must": [{"term": {"genre": "tech"}}],
                                                           "filter": [{"range": {"year": {"gte": 2010}}}]}},
                                         "sort": [{"pages": "desc"}], "from": 0, "size": 1})
    print("bool-range:", r["hits"]["total"]["value"], r["hits"]["hits"][0]["_source"]["title"])
    r = es.search(index="books", body={"size": 0, "aggs": {"genres": {"terms": {"field": "genre"}},
                                                           "avg_pages": {"avg": {"field": "pages"}}}})
    buckets = {b["key"]: b["doc_count"] for b in r["aggregations"]["genres"]["buckets"]}
    print("aggs: tech=%d scifi=%d avg=%.1f" % (buckets.get("tech", 0), buckets.get("scifi", 0), r["aggregations"]["avg_pages"]["value"]))
    print("count:", es.count(index="books", body={"query": {"term": {"genre": "tech"}}})["count"])
    actions = [{"_index": "books", "_id": "b%d" % i, "_source": {"title": "Bulk %d" % i, "year": 2000 + i, "genre": "bulk"}} for i in range(50)]
    ok, errors = helpers.bulk(es, actions, refresh=True)
    print("bulk:", ok, len(errors))
    r = es.mget(index="books", body={"ids": ["1", "b3", "nope"]})
    print("mget:", ",".join(str(d["found"]) for d in r["docs"]))
    n = sum(1 for _ in helpers.scan(es, index="books", query={"query": {"term": {"genre": "bulk"}}}, size=10, scroll="1m"))
    print("scan:", n)
    r = es.delete_by_query(index="books", body={"query": {"term": {"genre": "bulk"}}}, refresh=True)
    print("delete-by-query:", r["deleted"])
    r = es.delete(index="books", id="5")
    print("deleted:", r["result"])
    es.indices.refresh(index="books")
    print("final-count:", es.count(index="books")["count"])
    r = es.indices.delete(index="books")
    print("index-deleted:", r["acknowledged"])


def seed(es):
    for i in range(3):
        es.index(index="from-python", id="py%d" % i, body={"name": "python-%d" % i, "n": i, "nested": {"ok": True}})
    es.indices.refresh(index="from-python")
    print("seeded: 3")


def check(es):
    es.indices.refresh(index="from-camel")
    r = es.search(index="from-camel", body={"query": {"match_all": {}}, "sort": [{"n": "asc"}]})
    for h in r["hits"]["hits"]:
        print("doc: %s %s" % (h["_id"], h["_source"]["name"]))


def main():
    url, cmd = sys.argv[1], sys.argv[2]
    user, password, ca = (sys.argv[3:] + [None, None, None])[:3]
    print("version: elasticsearch-py", elasticsearch.__versionstr__)
    es = client(url, user, password, ca)
    if cmd == "auth-fail":
        try:
            es.info()
            print("auth: ok")
        except AuthenticationException as e:
            print("auth-failed:", e.status_code)
        return
    {"api": api, "seed": seed, "check": check}[cmd](es)


if __name__ == "__main__":
    main()
