//! An embedded in-memory FHIR R4 server for tests ([`FhirMock`]).
//!
//! Base url `http://127.0.0.1:<port>/fhir`. Supported interactions: `metadata`, create
//! (`POST /T`), read / vread, update (`PUT /T/id`, update-as-create), delete (instance and
//! conditional), search (`GET /T?...`, `POST /T/_search`) with `_id`, `_count`, `_offset`
//! (paging with `next` / `previous` links), token (`system|code`), reference and
//! prefix-string matching on top-level elements (aliases `family`, `given`, `birthdate`,
//! `patient`, `subject`), history (instance, type, server) and `transaction` / `batch`
//! Bundles (with `urn:uuid:` reference rewriting). JSON and XML (by `Content-Type`,
//! `Accept`, `_format`). Optional basic or bearer authentication.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use camel_http::{HttpRequest, HttpResponse, HttpServer, ServerConfig};

use super::convert::{parse_json, tree_to_xml, xml_to_tree};
use super::json::{self, J};
use crate::support::now_utc;

#[derive(Clone)]
enum Auth {
    None,
    Basic(String),
    Bearer(String),
}

#[derive(Clone)]
struct Version {
    resource: Option<J>,
    version: u64,
    method: &'static str,
}

#[derive(Default)]
struct State {
    resources: BTreeMap<(String, String), Vec<Version>>,
    history: Vec<(String, String, u64)>,
    next_id: u64,
    requests: Vec<String>,
}

/// The mock FHIR server.
pub struct FhirMock {
    server: HttpServer,
    state: Arc<Mutex<State>>,
}

fn obj(items: Vec<(&str, J)>) -> J {
    J::Obj(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn s(v: impl Into<String>) -> J {
    J::Str(v.into())
}

fn outcome(severity: &str, code: &str, diag: &str) -> J {
    obj(vec![("resourceType", s("OperationOutcome")), ("issue", J::Arr(vec![obj(vec![("severity", s(severity)), ("code", s(code)), ("diagnostics", s(diag))])]))])
}

fn set(j: &mut J, key: &str, v: J) {
    if let J::Obj(m) = j {
        match m.iter_mut().find(|(k, _)| k == key) {
            Some(e) => e.1 = v,
            None => {
                let pos = if key == "id" { 1.min(m.len()) } else if key == "meta" { m.iter().position(|(k, _)| k == "id").map(|p| p + 1).unwrap_or(1.min(m.len())) } else { m.len() };
                m.insert(pos, (key.to_string(), v));
            }
        }
    }
}

struct Ctx {
    base: String,
    xml: bool,
}

impl Ctx {
    fn resp(&self, status: u16, j: &J) -> HttpResponse {
        let (body, ct) = if self.xml {
            (tree_to_xml(j, false).unwrap_or_default(), "application/fhir+xml;charset=utf-8")
        } else {
            (json::write(j, false), "application/fhir+json;charset=utf-8")
        };
        HttpResponse::new(status).with_header("Content-Type", ct).with_body(body.into_bytes())
    }
    fn err(&self, status: u16, code: &str, diag: &str) -> HttpResponse {
        self.resp(status, &outcome("error", code, diag))
    }
}

fn leaves(j: &J, out: &mut Vec<String>) {
    match j {
        J::Str(x) | J::Num(x) => out.push(x.clone()),
        J::Bool(b) => out.push(b.to_string()),
        J::Arr(a) => a.iter().for_each(|x| leaves(x, out)),
        J::Obj(m) => m.iter().filter(|(k, _)| !k.starts_with('_')).for_each(|(_, v)| leaves(v, out)),
        J::Null => {}
    }
}

fn token_match(j: &J, sys: &str, code: &str) -> bool {
    match j {
        J::Arr(a) => a.iter().any(|x| token_match(x, sys, code)),
        J::Obj(m) => {
            let get = |k: &str| m.iter().find(|(x, _)| x == k).and_then(|(_, v)| v.as_str());
            let direct = (sys.is_empty() || get("system") == Some(sys)) && (get("code") == Some(code) || get("value") == Some(code));
            direct || m.iter().any(|(_, v)| token_match(v, sys, code))
        }
        _ => false,
    }
}

fn matches(r: &J, id: &str, name: &str, value: &str) -> bool {
    let (name, modifier) = name.split_once(':').unwrap_or((name, ""));
    if name == "_id" {
        return value.split(',').any(|v| v == id);
    }
    let path: Vec<&str> = match name {
        "family" => vec!["name", "family"],
        "given" => vec!["name", "given"],
        "birthdate" => vec!["birthDate"],
        "patient" | "subject" => vec!["subject"],
        other => vec![other],
    };
    let mut cur: Vec<&J> = vec![r];
    for p in &path {
        let mut next = Vec::new();
        for c in cur {
            let v = match c {
                J::Arr(a) => a.iter().filter_map(|x| x.get(p)).collect::<Vec<_>>(),
                other => other.get(p).into_iter().collect(),
            };
            next.extend(v);
        }
        cur = next;
    }
    if cur.is_empty() {
        return false;
    }
    value.split(',').any(|v| {
        if let Some((sys, code)) = v.split_once('|') {
            return cur.iter().any(|c| token_match(c, sys, code));
        }
        let mut ls = Vec::new();
        cur.iter().for_each(|c| leaves(c, &mut ls));
        ls.iter().any(|l| match modifier {
            "exact" => l == v,
            "contains" => l.to_lowercase().contains(&v.to_lowercase()),
            _ => l.to_lowercase().starts_with(&v.to_lowercase()) || (l.contains('/') && l.ends_with(&format!("/{}", v))) || l == v,
        })
    })
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (camel_core::util::url_decode(&k.replace('+', " ")), camel_core::util::url_decode(&v.replace('+', " ")))
        })
        .collect()
}

impl State {
    fn current(&self, ty: &str, id: &str) -> Option<&Version> {
        self.resources.get(&(ty.to_string(), id.to_string())).and_then(|v| v.last())
    }

    fn store(&mut self, ty: &str, id: &str, mut r: Option<J>, method: &'static str) -> Version {
        let key = (ty.to_string(), id.to_string());
        let version = self.resources.get(&key).and_then(|v| v.last()).map(|v| v.version + 1).unwrap_or(1);
        if let Some(j) = &mut r {
            set(j, "id", s(id));
            set(j, "meta", obj(vec![("versionId", s(version.to_string())), ("lastUpdated", s(now_utc()))]));
        }
        let v = Version { resource: r, version, method };
        self.resources.entry(key).or_default().push(v.clone());
        self.history.push((ty.to_string(), id.to_string(), version));
        v
    }

    fn new_id(&mut self) -> String {
        self.next_id += 1;
        self.next_id.to_string()
    }
}

fn bundle(ty: &str, total: Option<usize>, links: Vec<(&str, String)>, entries: Vec<J>) -> J {
    let mut items = vec![("resourceType", s("Bundle")), ("id", s(camel_core::message::generate_id())), ("meta", obj(vec![("lastUpdated", s(now_utc()))])), ("type", s(ty))];
    if let Some(t) = total {
        items.push(("total", J::Num(t.to_string())));
    }
    if !links.is_empty() {
        items.push(("link", J::Arr(links.into_iter().map(|(r, u)| obj(vec![("relation", s(r)), ("url", s(u))])).collect())));
    }
    if !entries.is_empty() {
        items.push(("entry", J::Arr(entries)));
    }
    obj(items)
}

fn rewrite_refs(j: &mut J, map: &[(String, String)]) {
    match j {
        J::Obj(m) => {
            for (k, v) in m.iter_mut() {
                if k == "reference" {
                    if let J::Str(r) = v {
                        if let Some((_, to)) = map.iter().find(|(f, _)| f == r) {
                            *r = to.clone();
                        }
                    }
                } else {
                    rewrite_refs(v, map);
                }
            }
        }
        J::Arr(a) => a.iter_mut().for_each(|x| rewrite_refs(x, map)),
        _ => {}
    }
}

fn valid_type(t: &str) -> bool {
    t.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) && t.chars().all(|c| c.is_ascii_alphanumeric())
}

impl FhirMock {
    /// Start without authentication.
    pub fn start() -> FhirMock {
        Self::start_with(Auth::None)
    }

    /// Start requiring HTTP basic authentication.
    pub fn start_with_basic(user: &str, password: &str) -> FhirMock {
        Self::start_with(Auth::Basic(camel_tls::pem::base64_encode(format!("{}:{}", user, password).as_bytes())))
    }

    /// Start requiring a bearer token.
    pub fn start_with_bearer(token: &str) -> FhirMock {
        Self::start_with(Auth::Bearer(token.to_string()))
    }

    fn start_with(auth: Auth) -> FhirMock {
        let state = Arc::new(Mutex::new(State::default()));
        let st = state.clone();
        let cfg = ServerConfig { name: "fhir-mock".into(), pool_size: 8, ..Default::default() };
        let server = HttpServer::bind("127.0.0.1:0", cfg, move |req: &mut HttpRequest| handle(&st, &auth, req)).expect("bind FHIR mock");
        FhirMock { server, state }
    }

    /// The base url (`http://127.0.0.1:<port>/fhir`).
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/fhir", self.server.port())
    }

    /// Number of current (not deleted) resources of a type.
    pub fn count(&self, ty: &str) -> usize {
        let st = self.state.lock().unwrap();
        st.resources.iter().filter(|((t, _), v)| t == ty && v.last().map(|x| x.resource.is_some()).unwrap_or(false)).count()
    }

    /// The current version of a resource, as JSON text.
    pub fn resource_json(&self, ty: &str, id: &str) -> Option<String> {
        let st = self.state.lock().unwrap();
        st.current(ty, id).and_then(|v| v.resource.as_ref()).map(|j| json::write(j, false))
    }

    /// Requests received (`METHOD /path?query`).
    pub fn requests(&self) -> Vec<String> {
        self.state.lock().unwrap().requests.clone()
    }
}

impl Drop for FhirMock {
    fn drop(&mut self) {
        self.server.stop();
    }
}

fn handle(state: &Mutex<State>, auth: &Auth, req: &mut HttpRequest) -> HttpResponse {
    let query = parse_query(req.raw_query.as_deref().unwrap_or(""));
    let accept = req.headers.get("Accept").unwrap_or("").to_ascii_lowercase();
    let fmt = query.iter().find(|(k, _)| k == "_format").map(|(_, v)| v.to_ascii_lowercase());
    let xml = match &fmt {
        Some(f) => f.contains("xml"),
        None => accept.contains("xml") && !accept.contains("json"),
    };
    let base = format!("http://{}/fhir", req.headers.get("Host").unwrap_or("localhost"));
    let c = Ctx { base, xml };
    state.lock().unwrap().requests.push(format!("{} {}{}", req.method, req.path, req.raw_query.as_ref().map(|q| format!("?{}", q)).unwrap_or_default()));
    let authorized = match auth {
        Auth::None => true,
        Auth::Basic(cred) => req.headers.get("Authorization") == Some(format!("Basic {}", cred).as_str()),
        Auth::Bearer(t) => req.headers.get("Authorization") == Some(format!("Bearer {}", t).as_str()),
    };
    if !authorized {
        return c.err(401, "login", "Unauthorized").with_header("WWW-Authenticate", "Basic realm=\"fhir\"");
    }
    let Some(rest) = req.path.strip_prefix("/fhir") else { return c.err(404, "not-found", "Unknown path") };
    let segs: Vec<String> = rest.split('/').filter(|x| !x.is_empty()).map(str::to_string).collect();
    let body = if req.body.is_empty() {
        None
    } else {
        let text = req.body_text();
        let parsed = if req.content_type().map(|ct| ct.contains("xml")).unwrap_or(false) || text.trim_start().starts_with('<') { xml_to_tree(&text) } else { parse_json(&text) };
        match parsed {
            Ok(j) => Some(j),
            Err(e) => return c.err(400, "structure", &format!("Failed to parse request body: {}", e.message)),
        }
    };
    let minimal = req.headers.get("Prefer").map(|p| p.contains("return=minimal")).unwrap_or(false);
    let seg: Vec<&str> = segs.iter().map(String::as_str).collect();
    let mut st = state.lock().unwrap();
    match (req.method.as_str(), seg.as_slice()) {
        ("GET", ["metadata"]) => c.resp(200, &capability()),
        ("POST", []) => match body {
            Some(b) if b.get("resourceType").and_then(J::as_str) == Some("Bundle") => transaction(&mut st, &c, b),
            _ => c.err(400, "invalid", "Expected a transaction or batch Bundle"),
        },
        ("GET", ["_history"]) => history(&st, &c, None, None, &query),
        (_, [ty, ..]) if !valid_type(ty) => c.err(404, "not-supported", &format!("Unknown resource type '{}'", ty)),
        ("POST", [ty]) => match body {
            Some(b) => {
                if b.get("resourceType").and_then(J::as_str) != Some(*ty) {
                    return c.err(400, "invalid", &format!("Resource type in body does not match URL type {}", ty));
                }
                let id = st.new_id();
                let v = st.store(ty, &id, Some(b), "POST");
                created(&c, ty, &id, &v, 201, minimal)
            }
            None => c.err(400, "invalid", "No resource in request body"),
        },
        ("GET", [ty]) => search(&st, &c, ty, &query),
        ("POST", [ty, "_search"]) => {
            let mut q = query.clone();
            q.extend(parse_query(&req.body_text()));
            search(&st, &c, ty, &q)
        }
        ("DELETE", [ty]) => {
            let crit: Vec<(String, String)> = query.iter().filter(|(k, _)| !k.starts_with("_format")).cloned().collect();
            let ids: Vec<String> = st
                .resources
                .iter()
                .filter(|((t, _), v)| t == ty && v.last().and_then(|x| x.resource.as_ref()).is_some())
                .filter(|((_, id), v)| crit.iter().all(|(k, val)| matches(v.last().and_then(|x| x.resource.as_ref()).unwrap_or(&J::Null), id, k, val)))
                .map(|((_, id), _)| id.clone())
                .collect();
            for id in &ids {
                st.store(ty, id, None, "DELETE");
            }
            c.resp(200, &outcome("information", "informational", &format!("Successfully deleted {} resource(s)", ids.len())))
        }
        ("GET", [ty, "_history"]) => history(&st, &c, Some(ty), None, &query),
        ("GET", [ty, id]) => match st.current(ty, id) {
            Some(Version { resource: Some(r), version, .. }) => c.resp(200, r).with_header("ETag", &format!("W/\"{}\"", version)),
            Some(_) => c.err(410, "deleted", &format!("Resource {}/{} has been deleted", ty, id)),
            None => c.err(404, "not-found", &format!("Resource {}/{} is not known", ty, id)),
        },
        ("PUT", [ty, id]) => match body {
            Some(b) => {
                if b.get("resourceType").and_then(J::as_str) != Some(*ty) {
                    return c.err(400, "invalid", &format!("Resource type in body does not match URL type {}", ty));
                }
                if let Some(bid) = b.get("id").and_then(J::as_str) {
                    if bid != *id {
                        return c.err(400, "invalid", &format!("Resource body id {} does not match URL id {}", bid, id));
                    }
                }
                let existed = st.current(ty, id).map(|v| v.resource.is_some()).unwrap_or(false);
                let v = st.store(ty, id, Some(b), "PUT");
                created(&c, ty, id, &v, if existed { 200 } else { 201 }, minimal)
            }
            None => c.err(400, "invalid", "No resource in request body"),
        },
        ("DELETE", [ty, id]) => match st.current(ty, id) {
            Some(Version { resource: Some(_), .. }) => {
                st.store(ty, id, None, "DELETE");
                c.resp(200, &outcome("information", "informational", &format!("Successfully deleted {}/{}", ty, id)))
            }
            Some(_) => c.resp(200, &outcome("information", "informational", "Resource was already deleted")),
            None => c.err(404, "not-found", &format!("Resource {}/{} is not known", ty, id)),
        },
        ("GET", [ty, id, "_history"]) => {
            if st.current(ty, id).is_none() {
                return c.err(404, "not-found", &format!("Resource {}/{} is not known", ty, id));
            }
            history(&st, &c, Some(ty), Some(id), &query)
        }
        ("GET", [ty, id, "_history", vid]) => {
            let found = st.resources.get(&(ty.to_string(), id.to_string())).and_then(|vs| vs.iter().find(|v| v.version.to_string() == *vid));
            match found {
                Some(Version { resource: Some(r), .. }) => c.resp(200, r),
                Some(_) => c.err(410, "deleted", "Version is a deletion"),
                None => c.err(404, "not-found", &format!("Version {} of {}/{} is not known", vid, ty, id)),
            }
        }
        _ => c.err(405, "not-supported", &format!("{} {} is not supported", req.method, req.path)),
    }
}

fn created(c: &Ctx, ty: &str, id: &str, v: &Version, status: u16, minimal: bool) -> HttpResponse {
    let loc = format!("{}/{}/{}/_history/{}", c.base, ty, id, v.version);
    let resp = if minimal { HttpResponse::new(status) } else { c.resp(status, v.resource.as_ref().unwrap_or(&J::Null)) };
    resp.with_header("Location", &loc).with_header("ETag", &format!("W/\"{}\"", v.version))
}

fn search(st: &State, c: &Ctx, ty: &str, q: &[(String, String)]) -> HttpResponse {
    let get = |k: &str| q.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
    let count: usize = get("_count").and_then(|v| v.parse().ok()).unwrap_or(50);
    let offset: usize = get("_offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    let crit: Vec<&(String, String)> = q.iter().filter(|(k, _)| !matches!(k.as_str(), "_count" | "_offset" | "_format" | "_sort" | "_summary" | "_elements" | "_pretty")).collect();
    let hits: Vec<(&String, &J)> = st
        .resources
        .iter()
        .filter(|((t, _), _)| t == ty)
        .filter_map(|((_, id), v)| v.last().and_then(|x| x.resource.as_ref()).map(|r| (id, r)))
        .filter(|(id, r)| crit.iter().all(|(k, v)| matches(r, id, k, v)))
        .collect();
    let total = hits.len();
    let page: Vec<J> = hits
        .iter()
        .skip(offset)
        .take(count)
        .map(|(id, r)| obj(vec![("fullUrl", s(format!("{}/{}/{}", c.base, ty, id))), ("resource", (*r).clone()), ("search", obj(vec![("mode", s("match"))]))]))
        .collect();
    let plain: Vec<String> = q.iter().filter(|(k, _)| k != "_offset").map(|(k, v)| format!("{}={}", camel_core::util::url_encode(k), camel_core::util::url_encode(v))).collect();
    let page_url = |off: usize| {
        let mut p = plain.clone();
        p.push(format!("_offset={}", off));
        format!("{}/{}?{}", c.base, ty, p.join("&"))
    };
    let mut links = vec![("self", page_url(offset))];
    if offset + count < total {
        links.push(("next", page_url(offset + count)));
    }
    if offset > 0 {
        links.push(("previous", page_url(offset.saturating_sub(count))));
    }
    c.resp(200, &bundle("searchset", Some(total), links, page))
}

fn history(st: &State, c: &Ctx, ty: Option<&str>, id: Option<&str>, q: &[(String, String)]) -> HttpResponse {
    let count: usize = q.iter().find(|(k, _)| k == "_count").and_then(|(_, v)| v.parse().ok()).unwrap_or(100);
    let entries: Vec<J> = st
        .history
        .iter()
        .rev()
        .filter(|(t, i, _)| ty.map(|x| x == t).unwrap_or(true) && id.map(|x| x == i).unwrap_or(true))
        .take(count)
        .filter_map(|(t, i, ver)| {
            let v = st.resources.get(&(t.clone(), i.clone()))?.iter().find(|v| v.version == *ver)?;
            let url = format!("{}/{}", t, i);
            let mut e = vec![("fullUrl", s(format!("{}/{}", c.base, url)))];
            if let Some(r) = &v.resource {
                e.push(("resource", r.clone()));
            }
            e.push(("request", obj(vec![("method", s(v.method)), ("url", s(if v.method == "POST" { t.clone() } else { url.clone() }))])));
            let status = match v.method {
                "POST" => "201 Created",
                "DELETE" => "204 No Content",
                _ => "200 OK",
            };
            e.push(("response", obj(vec![("status", s(status)), ("etag", s(format!("W/\"{}\"", v.version)))])));
            Some(obj(e))
        })
        .collect();
    let total = entries.len();
    c.resp(200, &bundle("history", Some(total), vec![], entries))
}

fn transaction(st: &mut State, c: &Ctx, b: J) -> HttpResponse {
    let kind = b.get("type").and_then(J::as_str).unwrap_or("").to_string();
    if kind != "transaction" && kind != "batch" {
        return c.err(400, "invalid", &format!("Unable to process bundle of type '{}'", kind));
    }
    let mut entries: Vec<J> = match b.get("entry") {
        Some(J::Arr(a)) => a.clone(),
        _ => Vec::new(),
    };
    // assign ids to POSTed resources first, so urn:uuid references can be rewritten
    let mut map = Vec::new();
    let mut ids: Vec<Option<String>> = Vec::new();
    for e in &entries {
        let method = e.get("request").and_then(|r| r.get("method")).and_then(J::as_str).unwrap_or("");
        if method == "POST" {
            let id = st.new_id();
            if let (Some(full), Some(ty)) = (e.get("fullUrl").and_then(J::as_str), e.get("resource").and_then(|r| r.get("resourceType")).and_then(J::as_str)) {
                map.push((full.to_string(), format!("{}/{}", ty, id)));
            }
            ids.push(Some(id));
        } else {
            ids.push(None);
        }
    }
    for e in entries.iter_mut() {
        rewrite_refs(e, &map);
    }
    let mut out = Vec::new();
    for (e, id) in entries.into_iter().zip(ids) {
        let method = e.get("request").and_then(|r| r.get("method")).and_then(J::as_str).unwrap_or("").to_string();
        let url = e.get("request").and_then(|r| r.get("url")).and_then(J::as_str).unwrap_or("").to_string();
        let res = e.get("resource").cloned();
        let resp = |status: &str, loc: Option<String>, r: Option<J>| {
            let mut items = Vec::new();
            if let Some(r) = r {
                items.push(("resource", r));
            }
            let mut rr = vec![("status", s(status))];
            if let Some(l) = loc {
                rr.push(("location", s(l)));
            }
            items.push(("response", obj(rr)));
            obj(items)
        };
        let path: Vec<&str> = url.split('?').next().unwrap_or("").split('/').filter(|x| !x.is_empty()).collect();
        let entry = match (method.as_str(), path.as_slice(), res) {
            ("POST", [ty], Some(r)) => {
                let id = id.unwrap_or_default();
                let v = st.store(ty, &id, Some(r), "POST");
                resp("201 Created", Some(format!("{}/{}/_history/{}", ty, id, v.version)), v.resource)
            }
            ("PUT", [ty, rid], Some(r)) => {
                let existed = st.current(ty, rid).map(|v| v.resource.is_some()).unwrap_or(false);
                let v = st.store(ty, rid, Some(r), "PUT");
                resp(if existed { "200 OK" } else { "201 Created" }, Some(format!("{}/{}/_history/{}", ty, rid, v.version)), v.resource)
            }
            ("DELETE", [ty, rid], _) => {
                if st.current(ty, rid).and_then(|v| v.resource.as_ref()).is_some() {
                    st.store(ty, rid, None, "DELETE");
                    resp("204 No Content", None, None)
                } else {
                    resp("404 Not Found", None, None)
                }
            }
            ("GET", [ty, rid], _) => match st.current(ty, rid).and_then(|v| v.resource.clone()) {
                Some(r) => resp("200 OK", None, Some(r)),
                None => resp("404 Not Found", None, None),
            },
            _ => {
                if kind == "transaction" {
                    return c.err(400, "invalid", &format!("Unsupported transaction entry {} {}", method, url));
                }
                resp("400 Bad Request", None, Some(outcome("error", "invalid", "Unsupported entry")))
            }
        };
        out.push(entry);
    }
    c.resp(200, &bundle(&format!("{}-response", kind), None, vec![], out))
}

fn capability() -> J {
    let res: Vec<J> = ["Patient", "Observation", "Encounter", "Practitioner", "Organization", "Bundle"]
        .iter()
        .map(|t| {
            obj(vec![
                ("type", s(*t)),
                ("interaction", J::Arr(["read", "vread", "update", "delete", "history-instance", "history-type", "create", "search-type"].iter().map(|i| obj(vec![("code", s(*i))])).collect())),
                ("versioning", s("versioned")),
                ("readHistory", J::Bool(true)),
                ("updateCreate", J::Bool(true)),
            ])
        })
        .collect();
    obj(vec![
        ("resourceType", s("CapabilityStatement")),
        ("status", s("active")),
        ("date", s(now_utc())),
        ("publisher", s("rust-camel")),
        ("kind", s("instance")),
        ("software", obj(vec![("name", s("rust-camel FHIR mock")), ("version", s(env!("CARGO_PKG_VERSION")))])),
        ("fhirVersion", s("4.0.1")),
        ("format", J::Arr(vec![s("application/fhir+json"), s("application/fhir+xml")])),
        ("rest", J::Arr(vec![obj(vec![("mode", s("server")), ("resource", J::Arr(res)), ("interaction", J::Arr(vec![obj(vec![("code", s("transaction"))]), obj(vec![("code", s("batch"))]), obj(vec![("code", s("history-system"))])]))])])),
    ])
}
