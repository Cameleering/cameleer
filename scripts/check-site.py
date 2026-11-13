#!/usr/bin/env python3
"""Check the built site before it deploys: links, anchors, JSON-LD, redirects and the sitemap.

Run after `python3 scripts/build-docs.py` (CI does both, before merge and before deploy):

    python3 scripts/check-site.py [--site site]

Fails (exit 1) on:
  - a relative link or #fragment that does not resolve, on any HTML page under site/;
  - an internal link to a redirect stub (link to the page it points at instead);
  - an absolute https://cameleer.ing/ link whose path does not exist;
  - a JSON-LD block that does not parse or has no @context;
  - a designed page (hand-written index.html outside docs/ and the project microsites) whose
    canonical is not its own URL, or that has no H1 or more than one;
  - a sitemap URL that is missing or a stub, or a designed page missing from the sitemap;
  - a redirect table (scripts/redirects.txt) whose stub is missing or points at a missing page.
Warns on titles over 65 characters and meta descriptions over 165 on designed pages.

Std-only Python, like build-docs.py.
"""
import argparse
import html.parser
import json
import os
import re
import sys
import urllib.parse

SITE_URL = "https://cameleer.ing/"
# Copied in from their own repositories (they check their own pages when they build).
MICROSITES = ("simoom",)
# Not pages: never in the sitemap, never "designed".
NOT_PAGES = ("docs", "assets", "brand") + MICROSITES


class Page(html.parser.HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.ids = set()
        self.links = []  # (attr, value)
        self.ldjson = []
        self.canonical = None
        self.refresh = None
        self.h1 = 0
        self.title = ""
        self.description = None
        self._in_ld = False
        self._in_title = False
        self._buf = []

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        for k in ("id", "name"):
            if a.get(k) and (k == "id" or tag == "a"):
                self.ids.add(a[k])
        if tag == "a" and a.get("href"):
            self.links.append(a["href"])
        elif tag in ("img", "script", "source") and a.get("src"):
            self.links.append(a["src"])
        elif tag == "link" and a.get("href"):
            rel = (a.get("rel") or "").lower()
            if rel == "canonical":
                self.canonical = a["href"]
            elif rel in ("stylesheet", "icon", "apple-touch-icon", "alternate") and "fonts.googleapis" not in a["href"]:
                self.links.append(a["href"])
        elif tag == "meta":
            if (a.get("http-equiv") or "").lower() == "refresh":
                self.refresh = a.get("content", "")
            elif (a.get("name") or "").lower() == "description":
                self.description = a.get("content", "")
        elif tag == "h1":
            self.h1 += 1
        elif tag == "title":
            self._in_title = True
        if tag == "script" and (a.get("type") or "") == "application/ld+json":
            self._in_ld = True
            self._buf = []

    def handle_endtag(self, tag):
        if tag == "script" and self._in_ld:
            self.ldjson.append("".join(self._buf))
            self._in_ld = False
        elif tag == "title":
            self._in_title = False

    def handle_data(self, data):
        if self._in_ld:
            self._buf.append(data)
        elif self._in_title:
            self.title += data


def url_of(rel):
    """Site-relative file path -> its public URL."""
    if rel == "index.html":
        return SITE_URL
    if rel.endswith("/index.html"):
        return SITE_URL + rel[: -len("index.html")]
    return SITE_URL + rel


def target_file(site, path):
    """A site-relative URL path (no fragment) -> the file it serves, or None."""
    path = urllib.parse.unquote(path).lstrip("/")
    full = os.path.join(site, path)
    if path == "" or path.endswith("/"):
        full = os.path.join(full, "index.html")
    elif os.path.isdir(full):
        full = os.path.join(full, "index.html")
    return full if os.path.isfile(full) else None


def read_redirects(repo):
    table = {}
    p = os.path.join(repo, "scripts", "redirects.txt")
    if os.path.exists(p):
        for n, line in enumerate(open(p, encoding="utf-8"), 1):
            line = line.split("#", 1)[0].strip()
            if not line:
                continue
            parts = line.split()
            if len(parts) != 2:
                raise SystemExit("scripts/redirects.txt:%d: expected `old new`" % n)
            table[parts[0]] = parts[1]
    return table


def main(argv=None):
    here = os.path.dirname(os.path.abspath(__file__))
    repo = os.path.dirname(here)
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--site", default=os.path.join(repo, "site"))
    args = ap.parse_args(argv)
    site = os.path.abspath(args.site)

    errors, warnings = [], []
    pages = {}
    for d, dirs, files in os.walk(site):
        dirs.sort()
        for f in sorted(files):
            if f.endswith(".html"):
                full = os.path.join(d, f)
                rel = os.path.relpath(full, site).replace("\\", "/")
                p = Page()
                p.feed(open(full, encoding="utf-8").read())
                pages[rel] = p

    def is_stub(rel):
        return rel in pages and pages[rel].refresh is not None

    def designed(rel):
        top = rel.split("/", 1)[0]
        return rel.endswith("index.html") and top not in NOT_PAGES and not is_stub(rel)

    for rel, p in pages.items():
        here_dir = os.path.dirname(rel)
        if is_stub(rel):
            continue
        for href in p.links:
            if href.startswith(("mailto:", "tel:", "javascript:", "data:")):
                continue
            parsed = urllib.parse.urlsplit(href)
            if parsed.scheme in ("http", "https"):
                if parsed.netloc != "cameleer.ing":
                    continue
                path = parsed.path
            elif parsed.scheme or parsed.netloc:
                continue
            elif href.startswith("#"):
                path = "/" + rel
            else:
                if parsed.path.startswith("/"):
                    path = parsed.path
                else:
                    path = "/" + os.path.normpath(os.path.join(here_dir, parsed.path)).replace("\\", "/")
                    if parsed.path.endswith("/") and not path.endswith("/"):
                        path += "/"
                    if path == "/.":
                        path = "/"
            tf = target_file(site, path)
            if tf is None:
                errors.append("%s: link `%s` does not resolve" % (rel, href))
                continue
            trel = os.path.relpath(tf, site).replace("\\", "/")
            if is_stub(trel):
                errors.append("%s: link `%s` goes through the redirect stub %s; link its target" % (rel, href, trel))
                continue
            frag = urllib.parse.unquote(parsed.fragment)
            if frag and trel.endswith(".html") and trel in pages and frag not in pages[trel].ids:
                errors.append("%s: link `%s`: no id `%s` in %s" % (rel, href, frag, trel))
        for block in p.ldjson:
            try:
                data = json.loads(block)
            except ValueError as e:
                errors.append("%s: JSON-LD does not parse: %s" % (rel, e))
                continue
            items = data if isinstance(data, list) else [data]
            for item in items:
                if isinstance(item, dict) and "@context" not in item:
                    errors.append("%s: JSON-LD block without @context" % rel)
        if designed(rel):
            want = url_of(rel)
            if p.canonical != want:
                errors.append("%s: canonical `%s`, expected `%s`" % (rel, p.canonical, want))
            if p.h1 != 1:
                errors.append("%s: %d H1 elements, expected 1" % (rel, p.h1))
            title = re.sub(r"\s+", " ", p.title).strip()
            if len(title) > 65:
                warnings.append("%s: title is %d characters (over 65)" % (rel, len(title)))
            if p.description is not None and len(p.description) > 165:
                warnings.append("%s: meta description is %d characters (over 165)" % (rel, len(p.description)))

    # sitemap: every URL is a real page; every designed page is listed
    sm = os.path.join(site, "sitemap.xml")
    listed = set()
    if os.path.exists(sm):
        for loc in re.findall(r"<loc>([^<]+)</loc>", open(sm, encoding="utf-8").read()):
            listed.add(loc)
            if not loc.startswith(SITE_URL):
                errors.append("sitemap: `%s` is not on %s" % (loc, SITE_URL))
                continue
            tf = target_file(site, loc[len(SITE_URL) - 1:])
            if tf is None:
                errors.append("sitemap: `%s` does not exist" % loc)
            elif is_stub(os.path.relpath(tf, site).replace("\\", "/")):
                errors.append("sitemap: `%s` is a redirect stub" % loc)
        for rel in pages:
            if designed(rel) and url_of(rel) not in listed:
                errors.append("sitemap: designed page %s is not listed" % url_of(rel))
    else:
        errors.append("site/sitemap.xml is missing (run scripts/build-docs.py first)")

    # redirect table: every old path is a stub to its target, and the target is a real page
    for old, new in read_redirects(repo).items():
        oldf = target_file(site, old)
        newf = target_file(site, new)
        if newf is None:
            errors.append("redirects.txt: target `%s` does not exist" % new)
        elif is_stub(os.path.relpath(newf, site).replace("\\", "/")):
            errors.append("redirects.txt: target `%s` is itself a stub (a chain)" % new)
        if oldf is None:
            errors.append("redirects.txt: no stub at `%s`" % old)
        elif not is_stub(os.path.relpath(oldf, site).replace("\\", "/")):
            errors.append("redirects.txt: `%s` is a real page, not a stub" % old)

    for w in warnings:
        print("warning: " + w)
    if errors:
        print("site check failed (%d):" % len(errors), file=sys.stderr)
        for e in errors:
            print("  " + e, file=sys.stderr)
        return 1
    print("site check: %d HTML pages, %d sitemap URLs, %d warnings, no errors" % (len(pages), len(listed), len(warnings)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
