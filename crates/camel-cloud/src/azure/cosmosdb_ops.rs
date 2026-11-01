//! `azure-cosmosdb` throughput operations, Entra ID authentication, request options and the
//! lease-based change feed processor.
//!
//! * Throughput: `throughputProperties` (option) or `CamelAzureCosmosDbThroughputProperties`
//!   (header: manual RU/s as a number, or a map `{throughput}` / `{autoscaleMaxThroughput}`) on
//!   createDatabase / createContainer (`x-ms-offer-throughput` /
//!   `x-ms-cosmos-offer-autopilot-settings`); `replaceDatabaseThroughput` /
//!   `replaceContainerThroughput` replace the resource's offer (Offers API: the offer is found by
//!   `offerResourceId`, then `PUT /offers/{id}`; body = the new offer), and the rust-camel
//!   `readDatabaseThroughput` / `readContainerThroughput` return it.
//! * `credentialType=AZURE_IDENTITY`: Entra ID tokens (client secret, workload identity,
//!   managed identity; scope `https://{account}.documents.azure.com/.default` for an account
//!   endpoint, `https://cosmos.azure.com/.default` otherwise) sent as
//!   `authorization: type=aad&ver=1.0&sig=<token>`.
//! * `consistencyLevel` (`x-ms-consistency-level`: STRONG, BOUNDED_STALENESS, SESSION,
//!   CONSISTENT_PREFIX, EVENTUAL), `contentResponseOnWriteEnabled=false` (`Prefer:
//!   return=minimal`, writes return no item). rust-camel always talks to the REST gateway: the
//!   SDK's direct (TCP) connection mode options do not apply.
//! * Change feed processor (`leaseContainerName`, `leaseDatabaseName` default = the monitored
//!   database, `createLeaseContainerIfNotExists`, `createLeaseDatabaseIfNotExists`, `hostName`
//!   default random, rust-camel `leaseExpirationInterval` ms default 60000): one lease item per
//!   partition key range (`id` = `{db}_{container}..{range}`, `LeaseToken`, `ContinuationToken`,
//!   `Owner`, `timestamp`) in the lease container (partition key `/id`). Each poll the host
//!   creates missing leases (409 = another host was faster), renews its own (etag-checked
//!   replace; a 412 means the lease was taken), takes unowned or expired leases up to its fair
//!   share (ranges / live hosts), steals one lease per round from a host above the fair share,
//!   gives back leases above its own share, reads the change feed of its ranges from their continuation
//!   and checkpoints the continuation in the lease after each processed batch. Leases are
//!   released when the consumer stops.

use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::Processor;
use camel_core::value::{ObjectMap, Value};

use super::cosmosdb::{headers as ch, CosmosService};
use crate::endpoint::{process, set_batch};
use crate::util::{now_millis, obj, parse_json, Opts};

/// Extra header names.
pub mod headers {
    pub const THROUGHPUT_PROPERTIES: &str = "CamelAzureCosmosDbThroughputProperties";
}

/// Lease container configuration of the change feed processor.
#[derive(Clone, Debug)]
pub struct LeaseConfig {
    pub database: String,
    pub container: String,
    pub create_database: bool,
    pub create_container: bool,
    pub host: String,
    pub expiration_millis: i64,
}

impl LeaseConfig {
    /// From the endpoint options; None without `leaseContainerName`.
    pub fn from_opts(o: &mut Opts, db: Option<&str>) -> Result<Option<LeaseConfig>> {
        let Some(container) = o.get("leaseContainerName") else {
            o.ignore(&["leaseDatabaseName", "createLeaseContainerIfNotExists", "createLeaseDatabaseIfNotExists", "hostName", "leaseExpirationInterval"]);
            return Ok(None);
        };
        Ok(Some(LeaseConfig {
            database: o.get("leaseDatabaseName").or_else(|| db.map(|d| d.to_string())).ok_or_else(|| CamelError::illegal_argument("leaseDatabaseName (or a database in the uri) is required"))?,
            container,
            create_database: o.bool("createLeaseDatabaseIfNotExists", false)?,
            create_container: o.bool("createLeaseContainerIfNotExists", false)?,
            host: o.get("hostName").unwrap_or_else(|| format!("host-{}", crate::util::random_hex(6))),
            expiration_millis: o.i64("leaseExpirationInterval", 60_000)?,
        }))
    }
}

/// A lease owned by this host: (etag, continuation).
#[derive(Clone, Debug)]
pub struct Lease {
    pub etag: String,
    pub continuation: Option<String>,
    pub doc: Value,
}

fn throughput_value(v: &Value) -> Result<(Option<i64>, Option<i64>)> {
    let num = |x: &Value| x.as_i64().or_else(|| x.to_string().trim().parse().ok());
    match v {
        Value::Object(m) => Ok((m.get("throughput").or_else(|| m.get("offerThroughput")).and_then(num), m.get("autoscaleMaxThroughput").or_else(|| m.get("maxThroughput")).and_then(num))),
        Value::String(s) if s.trim_start().starts_with('{') => throughput_value(&parse_json(s.as_bytes())?),
        Value::String(s) if s.to_ascii_lowercase().starts_with("autoscale:") => Ok((None, s[10..].trim().parse().ok())),
        other => Ok((Some(num(other).ok_or_else(|| CamelError::illegal_argument(format!("Invalid throughput: {}", other)))?), None)),
    }
}

impl CosmosService {
    /// The Entra ID scope of the account.
    pub fn aad_scope(&self) -> String {
        let host = crate::http::Url::parse(&self.endpoint).map(|u| u.host).unwrap_or_default();
        if host.ends_with(".documents.azure.com") {
            format!("https://{}/.default", host)
        } else {
            "https://cosmos.azure.com/.default".to_string()
        }
    }

    /// Offer headers of a create request (the header value, else the option).
    pub fn throughput_headers(&self, v: Option<&Value>) -> Result<Vec<(&'static str, String)>> {
        let v = match v.cloned().or_else(|| self.throughput.clone().map(Value::from)) {
            Some(v) => v,
            None => return Ok(Vec::new()),
        };
        Ok(match throughput_value(&v)? {
            (_, Some(max)) => vec![("x-ms-cosmos-offer-autopilot-settings", obj([("maxThroughput", Value::Int(max))]).to_json())],
            (Some(t), None) => vec![("x-ms-offer-throughput", t.to_string())],
            _ => Vec::new(),
        })
    }

    fn offer_of(&self, resource_path: &str) -> Result<Value> {
        let r = self.request("GET", resource_path, None, &[])?;
        let rid = parse_json(&r.body)?.get("_rid").map(|v| v.to_string()).unwrap_or_default();
        let offers = self.query_all("offers", "SELECT * FROM root r WHERE r.offerResourceId = @rid", Some(&obj([("@rid", Value::from(rid.clone()))])), &[])?;
        offers.into_iter().next().ok_or_else(|| crate::util::cloud_error(crate::util::kinds::COSMOS, format!("Status code 404, \"No offer for resource {}\" (ErrorCode: NotFound)", resource_path)))
    }

    /// The operations of this module; false when `op` is not one of them.
    pub fn more_ops(&self, ex: &mut Exchange, op: &str) -> Result<bool> {
        let path = match op {
            "replaceDatabaseThroughput" | "readDatabaseThroughput" => format!("dbs/{}", self.db(ex)?),
            "replaceContainerThroughput" | "readContainerThroughput" => format!("dbs/{}/colls/{}", self.db(ex)?, self.coll(ex)?),
            _ => return Ok(false),
        };
        let offer = self.offer_of(&path)?;
        if op.starts_with("read") {
            ex.set_body(offer);
            return Ok(true);
        }
        let v = ex.header(headers::THROUGHPUT_PROPERTIES).cloned().or_else(|| self.throughput.clone().map(Value::from)).or_else(|| (!ex.body().is_null() && !ex.body().to_string().is_empty()).then(|| ex.body().clone()));
        let v = v.ok_or_else(|| CamelError::illegal_argument(format!("{} requires the CamelAzureCosmosDbThroughputProperties header", op)))?;
        let mut content = ObjectMap::new();
        match throughput_value(&v)? {
            (_, Some(max)) => {
                content.insert("offerAutopilotSettings".into(), obj([("maxThroughput", Value::Int(max))]));
            }
            (Some(t), None) => {
                content.insert("offerThroughput".into(), Value::Int(t));
            }
            _ => return Err(CamelError::illegal_argument("No throughput value given")),
        }
        let mut new_offer = offer.as_object().cloned().unwrap_or_default();
        new_offer.insert("content".into(), Value::Object(content));
        let id = new_offer.get("id").map(|i| i.to_string()).unwrap_or_default();
        let r = self.request("PUT", &format!("offers/{}", id.to_ascii_lowercase()), Some(Value::Object(new_offer)), &[])?;
        Self::set_response(ex, &r);
        ex.set_body(parse_json(&r.body)?);
        Ok(true)
    }

    /// Create the lease database / container when configured.
    pub fn start_leases(&self) -> Result<()> {
        if let Some(l) = &self.lease {
            if l.create_database {
                self.create_database(&l.database, true)?;
            }
            if l.create_container {
                self.create_container(&l.database, &l.container, Some("/id".to_string()), true)?;
            }
        }
        Ok(())
    }

    fn lease_id(&self, range: &str) -> String {
        format!("{}_{}..{}", self.database.clone().unwrap_or_default(), self.container.clone().unwrap_or_default(), range)
    }

    fn lease_path(&self, l: &LeaseConfig) -> String {
        format!("dbs/{}/colls/{}/docs", l.database, l.container)
    }

    fn write_lease(&self, l: &LeaseConfig, doc: &Value, etag: Option<&str>) -> Result<Option<(String, Value)>> {
        let id = doc.get("id").map(|i| i.to_string()).unwrap_or_default();
        let pk = ("x-ms-documentdb-partitionkey", Value::Array(vec![Value::from(id.clone())]).to_json());
        let resp = match etag {
            Some(e) => self.request("PUT", &format!("{}/{}", self.lease_path(l), id), Some(doc.clone()), &[pk, ("If-Match", e.to_string())]),
            None => self.request("POST", &self.lease_path(l), Some(doc.clone()), &[pk]),
        };
        match resp {
            Ok(r) => {
                let body = parse_json(&r.body).unwrap_or_else(|_| doc.clone());
                let etag = r.header("etag").map(|e| e.to_string()).or_else(|| body.get("_etag").map(|e| e.to_string())).unwrap_or_default();
                Ok(Some((etag, body)))
            }
            Err(e) if matches!(super::error_code(&e).as_deref(), Some("Conflict" | "PreconditionFailed" | "NotFound")) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn lease_doc(&self, range: &str, owner: &str, continuation: Option<&str>) -> Value {
        let mut m = ObjectMap::new();
        m.insert("id".into(), Value::from(self.lease_id(range)));
        m.insert("version".into(), Value::Int(1));
        m.insert("LeaseToken".into(), Value::from(range));
        m.insert("ContinuationToken".into(), continuation.map(Value::from).unwrap_or(Value::Null));
        m.insert("Owner".into(), Value::from(owner));
        m.insert("timestamp".into(), Value::from(now_millis().to_string()));
        Value::Object(m)
    }

    /// One balancing round: returns the ranges this host owns afterwards.
    fn balance(&self, l: &LeaseConfig, ranges: &[String]) -> Result<Vec<String>> {
        let now = now_millis();
        let pk_h = |id: &str| ("x-ms-documentdb-partitionkey", Value::Array(vec![Value::from(id)]).to_json());
        // read all leases (create the missing ones unowned)
        let mut all: Vec<(String, Value, String)> = Vec::new();
        for r in ranges {
            let id = self.lease_id(r);
            match self.request("GET", &format!("{}/{}", self.lease_path(l), id), None, &[pk_h(&id)]) {
                Ok(resp) => {
                    let doc = parse_json(&resp.body)?;
                    let etag = resp.header("etag").map(|e| e.to_string()).or_else(|| doc.get("_etag").map(|e| e.to_string())).unwrap_or_default();
                    all.push((r.clone(), doc, etag));
                }
                Err(e) if super::error_code(&e).as_deref() == Some("NotFound") => {
                    if let Some((etag, doc)) = self.write_lease(l, &self.lease_doc(r, "", None), None)? {
                        all.push((r.clone(), doc, etag));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        let owner = |d: &Value| d.get("Owner").map(|o| o.to_string()).unwrap_or_default();
        let fresh = |d: &Value| d.get("timestamp").and_then(|t| t.to_string().parse::<i64>().ok()).is_some_and(|t| now - t < l.expiration_millis);
        let mut hosts: std::collections::BTreeSet<String> = all.iter().filter(|(_, d, _)| !owner(d).is_empty() && fresh(d)).map(|(_, d, _)| owner(d)).collect();
        hosts.insert(l.host.clone());
        let target = ranges.len().div_ceil(hosts.len().max(1));
        let mut mine = Vec::new();
        let mut leases = self.leases.lock().unwrap();
        leases.retain(|r, _| ranges.contains(r));
        // renew our own leases
        for (r, d, etag) in &all {
            if owner(d) == l.host {
                let cont = d.get("ContinuationToken").filter(|c| !c.is_null()).map(|c| c.to_string());
                let doc = self.lease_doc(r, &l.host, cont.as_deref());
                match self.write_lease(l, &doc, Some(etag))? {
                    Some((etag, doc)) => {
                        leases.insert(r.clone(), Lease { etag, continuation: cont, doc });
                        mine.push(r.clone());
                    }
                    None => {
                        leases.remove(r);
                    }
                }
            } else {
                leases.remove(r);
            }
        }
        // take unowned / expired leases up to the fair share
        for (r, d, etag) in &all {
            if mine.len() >= target {
                break;
            }
            if owner(d) == l.host || (!owner(d).is_empty() && fresh(d)) {
                continue;
            }
            let cont = d.get("ContinuationToken").filter(|c| !c.is_null()).map(|c| c.to_string());
            let doc = self.lease_doc(r, &l.host, cont.as_deref());
            if let Some((etag, doc)) = self.write_lease(l, &doc, Some(etag))? {
                camel_core::camel_debug!("azure-cosmosdb", "Host {} acquired the lease of range {}", l.host, r);
                leases.insert(r.clone(), Lease { etag, continuation: cont, doc });
                mine.push(r.clone());
            }
        }
        // steal one lease from the most loaded host when it holds more than the fair share
        if mine.len() < target {
            let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
            for (_, d, _) in &all {
                if !owner(d).is_empty() && owner(d) != l.host && fresh(d) {
                    *counts.entry(owner(d)).or_default() += 1;
                }
            }
            if let Some((victim, n)) = counts.into_iter().max_by_key(|(_, n)| *n) {
                if n > target {
                    if let Some((r, d, etag)) = all.iter().find(|(_, d, _)| owner(d) == victim) {
                        let cont = d.get("ContinuationToken").filter(|c| !c.is_null()).map(|c| c.to_string());
                        let doc = self.lease_doc(r, &l.host, cont.as_deref());
                        if let Some((etag, doc)) = self.write_lease(l, &doc, Some(etag))? {
                            camel_core::camel_debug!("azure-cosmosdb", "Host {} stole the lease of range {} from {}", l.host, r, victim);
                            leases.insert(r.clone(), Lease { etag, continuation: cont, doc });
                            mine.push(r.clone());
                        }
                    }
                }
            }
        }
        // give back leases above the fair share so other hosts can take them
        while mine.len() > target {
            let r = mine.pop().expect("non-empty");
            if let Some(lease) = leases.remove(&r) {
                let doc = self.lease_doc(&r, "", lease.continuation.as_deref());
                self.write_lease(l, &doc, Some(&lease.etag))?;
            }
        }
        Ok(mine)
    }

    /// The change feed processor poll.
    pub fn poll_with_leases(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let l = self.lease.clone().expect("lease mode");
        let db = self.database.clone().ok_or_else(|| CamelError::illegal_argument("The change feed consumer requires a database name"))?;
        let coll = self.container.clone().ok_or_else(|| CamelError::illegal_argument("The change feed consumer requires a container name"))?;
        let ranges: Vec<String> = self.list_feed(&format!("dbs/{}/colls/{}/pkranges", db, coll), "PartitionKeyRanges", &[])?.iter().map(|r| r.get("id").map(|i| i.to_string()).unwrap_or_default()).collect();
        let mine = self.balance(&l, &ranges)?;
        let mut delivered = 0;
        for range in mine {
            let Some(lease) = self.leases.lock().unwrap().get(&range).cloned() else { continue };
            let mut h: Vec<(&str, String)> = vec![("A-IM", "Incremental feed".into()), ("x-ms-documentdb-partitionkeyrangeid", range.clone())];
            match &lease.continuation {
                Some(c) => h.push(("If-None-Match", c.clone())),
                None if !self.start_from_beginning => h.push(("If-None-Match", "*".into())),
                None => {}
            }
            let resp = self.request("GET", &format!("dbs/{}/colls/{}/docs", db, coll), None, &h)?;
            let new_cont = resp.header("etag").map(|e| e.to_string());
            let docs = if resp.status == 304 { Vec::new() } else { parse_json(&resp.body)?.get("Documents").and_then(|d| d.as_array()).cloned().unwrap_or_default() };
            let mut ok = true;
            if !docs.is_empty() {
                let mut ex = Exchange::new(ctx.clone());
                ex.set_header(ch::DATABASE_NAME, db.clone());
                ex.set_header(ch::CONTAINER_NAME, coll.clone());
                ex.set_header("CamelAzureCosmosDbLeaseToken", range.clone());
                ex.set_body(Value::Array(docs));
                set_batch(&mut ex, 0, 1);
                ok = process(logger, processor, &mut ex);
                delivered += 1;
            }
            // checkpoint (also the initial "now" position), unless processing failed
            if ok && new_cont.is_some() && new_cont != lease.continuation {
                let doc = self.lease_doc(&range, &l.host, new_cont.as_deref());
                match self.write_lease(&l, &doc, Some(&lease.etag))? {
                    Some((etag, doc)) => {
                        self.leases.lock().unwrap().insert(range.clone(), Lease { etag, continuation: new_cont, doc });
                    }
                    None => {
                        camel_core::camel_warn!(logger, "Lease of range {} was lost before the checkpoint", range);
                        self.leases.lock().unwrap().remove(&range);
                    }
                }
            }
        }
        Ok(delivered)
    }

    /// Release the leases of this host (consumer stop).
    pub fn release_leases(&self) {
        let Some(l) = self.lease.clone() else { return };
        let leases: Vec<(String, Lease)> = self.leases.lock().unwrap().drain().collect();
        for (r, lease) in leases {
            let doc = self.lease_doc(&r, "", lease.continuation.as_deref());
            if let Err(e) = self.write_lease(&l, &doc, Some(&lease.etag)) {
                camel_core::camel_warn!("azure-cosmosdb", "Cannot release the lease of range {}: {}", r, e);
            }
        }
    }
}
