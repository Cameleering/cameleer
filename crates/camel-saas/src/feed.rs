//! `rss:` and `atom:` polling feed consumers (Camel `camel-rss` / `camel-atom`), parsing
//! RSS 2.0, RSS 1.0 (RDF) and Atom 1.0 with the camel-xml parser.
//!
//! `rss:https://example.com/feed.xml?splitEntries=true&delay=60000`
//!
//! The remaining part is the feed url (`http`, `https` or `file:`); options the component
//! does not know are appended to the feed url's query (as in Camel).
//!
//! Feeds and entries are maps. Feed: `title`, `link`, `description`, `author`,
//! `publishedDate`, `type` (`rss_2.0`, `rss_1.0`, `atom_1.0`), `entries`. Entry: `title`,
//! `link`, `uri` (guid / id), `description` (description / summary), `contents`
//! (content:encoded / content), `author`, `publishedDate`, `updatedDate`, `categories`.
//!
//! Options: `splitEntries` (default true: one exchange per entry; `rss` bodies are the feed
//! with only that entry, `atom` bodies the entry), `filter` (default true: only entries
//! newer than the last delivered one; entries without a date are delivered once),
//! `lastUpdate` (`yyyy-MM-ddTHH:mm:ss`, UTC: initial filter date), `sortEntries` (default
//! false: sort by date), `throttleEntries` (default true: one entry per poll),
//! `feedHeader` (default true: the whole feed in the `CamelRssFeed` / `CamelAtomFeed`
//! header) and the scheduled poll options (`delay`, default 60 s).

use std::any::Any;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::spi::{Component, Consumer, Endpoint, Processor};
use camel_core::util::{url_encode, Parameters};
use camel_core::value::{ObjectMap, Value};
use camel_xml::dom::{Document, NodeId};

use crate::rest::{emit, poll_config, poll_consumer, Req, Rest};

pub const RSS_FEED: &str = "CamelRssFeed";
pub const ATOM_FEED: &str = "CamelAtomFeed";
const ATOM_NS: &str = "http://www.w3.org/2005/Atom";

/// The `rss` / `atom` component.
pub struct FeedComponent {
    atom: bool,
}

impl FeedComponent {
    pub fn rss() -> FeedComponent {
        FeedComponent { atom: false }
    }
    pub fn atom() -> FeedComponent {
        FeedComponent { atom: true }
    }
}

#[derive(Clone, Debug)]
struct Config {
    atom: bool,
    url: String,
    split: bool,
    filter: bool,
    last_update: Option<i64>,
    sort: bool,
    throttle: bool,
    feed_header: bool,
}

impl Component for FeedComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let r = remaining.trim();
        if r.is_empty() {
            return Err(CamelError::illegal_argument("rss/atom: a feed url is required"));
        }
        let last_update = match params.take("lastUpdate") {
            Some(s) => Some(parse_date(&s).ok_or_else(|| CamelError::illegal_argument(format!("Invalid lastUpdate date: {}", s)))?),
            None => None,
        };
        let mut cfg = Config {
            atom: self.atom,
            url: r.to_string(),
            split: params.take_bool("splitEntries", true)?,
            filter: params.take_bool("filter", true)?,
            last_update,
            sort: params.take_bool("sortEntries", false)?,
            throttle: params.take_bool("throttleEntries", true)?,
            feed_header: params.take_bool("feedHeader", true)?,
        };
        let poll = poll_config(params, Duration::from_secs(60))?;
        let rest = params.take_all();
        if !rest.is_empty() {
            let q: Vec<String> = rest.iter().map(|(k, v)| format!("{}={}", k, url_encode(v))).collect();
            cfg.url.push(if cfg.url.contains('?') { '&' } else { '?' });
            cfg.url.push_str(&q.join("&"));
        }
        Ok(Arc::new(FeedEndpoint { uri: uri.to_string(), cfg, poll, ctx: ctx.downgrade() }))
    }
    fn lenient_properties(&self) -> bool {
        true
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A feed endpoint (consumer only).
pub struct FeedEndpoint {
    uri: String,
    cfg: Config,
    poll: camel_core::component::scheduled::ScheduledPollConfig,
    ctx: WeakContext,
}

#[derive(Default)]
struct State {
    last: Option<i64>,
    seen: HashSet<String>,
    queue: VecDeque<Value>,
}

fn entry_date(e: &Value) -> Option<i64> {
    e.get("publishedDate").or_else(|| e.get("updatedDate")).and_then(|d| parse_date(&d.to_string()))
}

fn entry_key(e: &Value) -> String {
    e.get("uri").or_else(|| e.get("link")).or_else(|| e.get("title")).map(|v| v.to_string()).unwrap_or_default()
}

impl Endpoint for FeedEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let cfg = self.cfg.clone();
        let ctx = self.ctx.clone();
        let rest = Rest::default();
        let state = Arc::new(Mutex::new(State { last: cfg.last_update, ..Default::default() }));
        let logger = if cfg.atom { "atom" } else { "rss" };
        Ok(Arc::new(poll_consumer(logger, self.poll.clone(), move || {
            let feed = fetch_feed(&rest, &cfg.url)?;
            let mut entries: Vec<Value> = feed.get("entries").and_then(|e| e.as_array()).cloned().unwrap_or_default();
            if cfg.sort {
                entries.sort_by_key(|e| entry_date(e).unwrap_or(i64::MIN));
            }
            let mut st = state.lock().unwrap();
            if cfg.filter {
                let last = st.last;
                let mut fresh = Vec::new();
                for e in entries {
                    let keep = match (entry_date(&e), last) {
                        (Some(d), Some(l)) => d > l,
                        (Some(_), None) => !st.seen.contains(&entry_key(&e)),
                        (None, _) => !st.seen.contains(&entry_key(&e)),
                    };
                    if keep {
                        fresh.push(e);
                    }
                }
                for e in &fresh {
                    st.seen.insert(entry_key(e));
                    if let Some(d) = entry_date(e) {
                        st.last = Some(st.last.map(|l| l.max(d)).unwrap_or(d));
                    }
                }
                entries = fresh;
            }
            let header_name = if cfg.atom { ATOM_FEED } else { RSS_FEED };
            let headers = |f: &Value| if cfg.feed_header { vec![(header_name, f.clone())] } else { vec![] };
            if !cfg.split {
                if entries.is_empty() && cfg.filter {
                    return Ok(0);
                }
                let mut f = feed.clone();
                if let Some(m) = f.as_object_mut() {
                    m.insert("entries".into(), Value::Array(entries));
                }
                drop(st);
                emit(&ctx, processor.as_ref(), logger, f, headers(&feed));
                return Ok(1);
            }
            st.queue.extend(entries);
            let batch: Vec<Value> = if cfg.throttle { st.queue.pop_front().into_iter().collect() } else { st.queue.drain(..).collect() };
            drop(st);
            let n = batch.len();
            for e in batch {
                let body = if cfg.atom {
                    e
                } else {
                    let mut f = feed.clone();
                    if let Some(m) = f.as_object_mut() {
                        m.insert("entries".into(), Value::Array(vec![e]));
                    }
                    f
                };
                emit(&ctx, processor.as_ref(), logger, body, headers(&feed));
            }
            Ok(n)
        })))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn fetch_feed(rest: &Rest, url: &str) -> Result<Value> {
    let bytes = if let Some(p) = url.strip_prefix("file://").or_else(|| url.strip_prefix("file:")) {
        std::fs::read(p).map_err(|e| CamelError::new(camel_core::error::kinds::IO, format!("Cannot read feed {}: {}", url, e)))?
    } else {
        rest.send_ok(Req::get(url.to_string()).header("Accept", "application/rss+xml, application/atom+xml, application/xml, text/xml"))?.body
    };
    let doc = Document::parse_bytes(&bytes)?;
    parse_feed(&doc)
}

fn child<'a>(doc: &'a Document, el: NodeId, local: &str) -> Option<NodeId> {
    doc.element_children(el).into_iter().find(|&c| doc.local_name(c) == local)
}

fn child_text(doc: &Document, el: NodeId, local: &str) -> Option<String> {
    child(doc, el, local).map(|c| doc.string_value(c).trim().to_string())
}

fn opt(v: Option<String>) -> Value {
    v.map(Value::String).unwrap_or(Value::Null)
}

/// Parse an RSS 2.0 / RSS 1.0 / Atom 1.0 document into a feed map.
pub fn parse_feed(doc: &Document) -> Result<Value> {
    let root = doc.document_element().ok_or_else(|| CamelError::new("FeedException", "Empty feed document"))?;
    let rl = doc.local_name(root);
    let mut feed = ObjectMap::new();
    let mut entries = Vec::new();
    if rl == "feed" && doc.namespace_uri(root) == ATOM_NS || rl == "feed" {
        feed.insert("type".to_string(), Value::from("atom_1.0"));
        feed.insert("title".into(), opt(child_text(doc, root, "title")));
        feed.insert("description".into(), opt(child_text(doc, root, "subtitle")));
        feed.insert("link".into(), opt(atom_link(doc, root)));
        feed.insert("author".into(), opt(child(doc, root, "author").and_then(|a| child_text(doc, a, "name"))));
        feed.insert("publishedDate".into(), opt(child_text(doc, root, "updated")));
        for e in doc.element_children(root).into_iter().filter(|&c| doc.local_name(c) == "entry") {
            let mut m = ObjectMap::new();
            m.insert("title".to_string(), opt(child_text(doc, e, "title")));
            m.insert("link".into(), opt(atom_link(doc, e)));
            m.insert("uri".into(), opt(child_text(doc, e, "id")));
            m.insert("description".into(), opt(child_text(doc, e, "summary")));
            m.insert("contents".into(), opt(child_text(doc, e, "content")));
            m.insert("author".into(), opt(child(doc, e, "author").and_then(|a| child_text(doc, a, "name"))));
            m.insert("publishedDate".into(), opt(child_text(doc, e, "published")));
            m.insert("updatedDate".into(), opt(child_text(doc, e, "updated")));
            let cats: Vec<Value> = doc.element_children(e).into_iter().filter(|&c| doc.local_name(c) == "category").filter_map(|c| doc.attribute(c, "term").map(Value::from)).collect();
            m.insert("categories".into(), Value::Array(cats));
            entries.push(Value::Object(m));
        }
    } else if rl == "rss" || rl == "RDF" {
        let rss1 = rl == "RDF";
        feed.insert("type".to_string(), Value::from(if rss1 { "rss_1.0" } else { "rss_2.0" }));
        let channel = child(doc, root, "channel").ok_or_else(|| CamelError::new("FeedException", "RSS feed without <channel>"))?;
        feed.insert("title".into(), opt(child_text(doc, channel, "title")));
        feed.insert("link".into(), opt(child_text(doc, channel, "link")));
        feed.insert("description".into(), opt(child_text(doc, channel, "description")));
        feed.insert("author".into(), opt(child_text(doc, channel, "managingEditor").or_else(|| child_text(doc, channel, "creator"))));
        feed.insert("publishedDate".into(), opt(child_text(doc, channel, "pubDate").or_else(|| child_text(doc, channel, "date"))));
        let items: Vec<NodeId> = if rss1 { doc.element_children(root) } else { doc.element_children(channel) };
        for it in items.into_iter().filter(|&c| doc.local_name(c) == "item") {
            let mut m = ObjectMap::new();
            m.insert("title".to_string(), opt(child_text(doc, it, "title")));
            m.insert("link".into(), opt(child_text(doc, it, "link")));
            m.insert("uri".into(), opt(child_text(doc, it, "guid").or_else(|| doc.attribute_ns(it, "http://www.w3.org/1999/02/22-rdf-syntax-ns#", "about").map(String::from))));
            m.insert("description".into(), opt(child_text(doc, it, "description")));
            m.insert("contents".into(), opt(child(doc, it, "encoded").map(|c| doc.string_value(c))));
            m.insert("author".into(), opt(child_text(doc, it, "author").or_else(|| child_text(doc, it, "creator"))));
            m.insert("publishedDate".into(), opt(child_text(doc, it, "pubDate").or_else(|| child_text(doc, it, "date"))));
            m.insert("updatedDate".into(), Value::Null);
            let cats: Vec<Value> = doc.element_children(it).into_iter().filter(|&c| doc.local_name(c) == "category" || doc.local_name(c) == "subject").map(|c| Value::String(doc.string_value(c).trim().to_string())).collect();
            m.insert("categories".into(), Value::Array(cats));
            entries.push(Value::Object(m));
        }
    } else {
        return Err(CamelError::new("FeedException", format!("Unsupported feed document: <{}>", doc.qualified_name(root))));
    }
    feed.insert("entries".into(), Value::Array(entries));
    Ok(Value::Object(feed))
}

fn atom_link(doc: &Document, el: NodeId) -> Option<String> {
    let links: Vec<NodeId> = doc.element_children(el).into_iter().filter(|&c| doc.local_name(c) == "link").collect();
    links
        .iter()
        .find(|&&l| matches!(doc.attribute(l, "rel"), None | Some("alternate")))
        .or(links.first())
        .and_then(|&l| doc.attribute(l, "href").map(String::from))
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse an RFC 3339 / ISO 8601 or RFC 822 date to epoch milliseconds (UTC when no zone).
pub fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() >= 10 && s.as_bytes()[4] == b'-' {
        // ISO 8601
        let y: i64 = s.get(0..4)?.parse().ok()?;
        let mo: i64 = s.get(5..7)?.parse().ok()?;
        let d: i64 = s.get(8..10)?.parse().ok()?;
        let (mut h, mut mi, mut se, mut ms, mut off) = (0i64, 0i64, 0i64, 0i64, 0i64);
        if s.len() > 10 {
            let t = &s[11..];
            h = t.get(0..2)?.parse().ok()?;
            mi = t.get(3..5)?.parse().ok()?;
            let mut rest = &t[5..];
            if rest.starts_with(':') {
                se = rest.get(1..3)?.parse().ok()?;
                rest = &rest[3..];
            }
            if let Some(fr) = rest.strip_prefix('.') {
                let digits: String = fr.chars().take_while(|c| c.is_ascii_digit()).collect();
                ms = format!("{:0<3}", &digits[..digits.len().min(3)]).parse().ok()?;
                rest = &fr[digits.len()..];
            }
            if rest.starts_with('+') || rest.starts_with('-') {
                let sign = if rest.starts_with('-') { -1 } else { 1 };
                let z = rest[1..].replace(':', "");
                let oh: i64 = z.get(0..2)?.parse().ok()?;
                let om: i64 = z.get(2..4).and_then(|x| x.parse().ok()).unwrap_or(0);
                off = sign * (oh * 60 + om);
            }
        }
        return Some(((days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - off * 60) * 1000) + ms);
    }
    // RFC 822: [Day, ]DD Mon YYYY HH:MM[:SS] ZONE
    let s = match s.split_once(',') {
        Some((_, r)) => r.trim(),
        None => s,
    };
    let p: Vec<&str> = s.split_whitespace().collect();
    if p.len() < 4 {
        return None;
    }
    let d: i64 = p[0].parse().ok()?;
    let months = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let mo = months.iter().position(|m| p[1].to_ascii_lowercase().starts_with(m))? as i64 + 1;
    let mut y: i64 = p[2].parse().ok()?;
    if y < 100 {
        y += if y < 50 { 2000 } else { 1900 };
    }
    let t: Vec<i64> = p[3].split(':').filter_map(|x| x.parse().ok()).collect();
    let (h, mi, se) = (*t.first()?, *t.get(1)?, *t.get(2).unwrap_or(&0));
    let off = match p.get(4).copied().unwrap_or("GMT") {
        "GMT" | "UT" | "UTC" | "Z" => 0,
        "EST" => -300,
        "EDT" => -240,
        "CST" => -360,
        "CDT" => -300,
        "MST" => -420,
        "MDT" => -360,
        "PST" => -480,
        "PDT" => -420,
        z if (z.starts_with('+') || z.starts_with('-')) && z.len() >= 5 => {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let oh: i64 = z[1..3].parse().ok()?;
            let om: i64 = z[3..5].parse().ok()?;
            sign * (oh * 60 + om)
        }
        _ => 0,
    };
    Some((days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - off * 60) * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(parse_date("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_date("2024-01-02T03:04:05.250+01:00"), Some(1704161045250));
        assert_eq!(parse_date("Tue, 02 Jan 2024 02:04:05 GMT"), Some(1704161045000));
        assert_eq!(parse_date("02 Jan 2024 03:04:05 +0100"), Some(1704161045000));
        assert_eq!(parse_date("2024-01-02"), Some(1704153600000));
        assert_eq!(parse_date("garbage"), None);
    }
}
