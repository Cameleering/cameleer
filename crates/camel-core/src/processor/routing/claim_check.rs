//! Claim Check EIP: store message data (body + headers) in a repository and get it
//! back later in the route.
//!
//! As in Apache Camel the repository is scoped to the exchange: it is kept in the
//! `CamelClaimCheckRepository` exchange property (as a value), so concurrent exchanges
//! never see each other's data and sub-exchanges (split/multicast) get a copy.
//!
//! Operations: `Set` (store under a key, replacing), `Get` (merge back, keep),
//! `GetAndRemove`, `Push` (stack), `Pop` (merge back the top of the stack).
//! Default merge: the stored body replaces the body and stored headers are merged
//! over the current headers. A `filter` controls what is merged back:
//! comma separated `body`, `headers`, `header:<pattern>`; prefix `-` excludes,
//! `+` (or nothing) includes. With only exclusions everything else is merged.

use std::sync::Arc;

use crate::error::{CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::Exchange;
use crate::model::{ProcessorDefinition, RouteContext};
use crate::spi::Processor;
use crate::value::{ObjectMap, Value};

/// Exchange property holding the claim check repository.
pub const CLAIM_CHECK_REPOSITORY: &str = "CamelClaimCheckRepository";

/// Claim check operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimCheckOperation {
    Set,
    Get,
    GetAndRemove,
    Push,
    Pop,
}

impl ClaimCheckOperation {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "Set" | "set" => ClaimCheckOperation::Set,
            "Get" | "get" => ClaimCheckOperation::Get,
            "GetAndRemove" | "getAndRemove" => ClaimCheckOperation::GetAndRemove,
            "Push" | "push" => ClaimCheckOperation::Push,
            "Pop" | "pop" => ClaimCheckOperation::Pop,
            other => return Err(CamelError::illegal_argument(format!("Unknown claim check operation: {}", other))),
        })
    }
}

/// Claim check definition.
pub struct ClaimCheckDefinition {
    pub id: Option<String>,
    pub operation: ClaimCheckOperation,
    pub key: Option<String>,
    pub filter: Option<String>,
}

#[derive(Clone, Debug)]
struct MergeFilter {
    body: bool,
    all_headers: bool,
    header_includes: Vec<String>,
    header_excludes: Vec<String>,
}

impl MergeFilter {
    fn parse(filter: Option<&str>) -> Self {
        let f = match filter.map(str::trim).filter(|f| !f.is_empty()) {
            None => return MergeFilter { body: true, all_headers: true, header_includes: vec![], header_excludes: vec![] },
            Some(f) => f,
        };
        let mut inc_body = false;
        let mut exc_body = false;
        let mut inc_all = false;
        let mut exc_all = false;
        let mut includes = Vec::new();
        let mut excludes = Vec::new();
        let mut any_include = false;
        for part in f.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (exclude, p) = match part.strip_prefix('-') {
                Some(r) => (true, r),
                None => (false, part.strip_prefix('+').unwrap_or(part)),
            };
            if !exclude {
                any_include = true;
            }
            match p {
                "body" => {
                    if exclude {
                        exc_body = true
                    } else {
                        inc_body = true
                    }
                }
                "headers" => {
                    if exclude {
                        exc_all = true
                    } else {
                        inc_all = true
                    }
                }
                other => {
                    let pat = other.strip_prefix("header:").unwrap_or(other).to_string();
                    if exclude {
                        excludes.push(pat)
                    } else {
                        includes.push(pat)
                    }
                }
            }
        }
        if !any_include {
            // only exclusions: everything but the excluded parts
            inc_body = true;
            inc_all = true;
        }
        MergeFilter { body: inc_body && !exc_body, all_headers: inc_all && !exc_all, header_includes: includes, header_excludes: excludes }
    }

    fn header_selected(&self, name: &str) -> bool {
        if self.header_excludes.iter().any(|p| crate::util::matches_pattern(name, p)) {
            return false;
        }
        self.all_headers || self.header_includes.iter().any(|p| crate::util::matches_pattern(name, p))
    }

    fn merge(&self, ex: &mut Exchange, data: &Value) {
        if self.body {
            ex.message.body = data.get("body").cloned().unwrap_or(Value::Null);
        }
        if let Some(Value::Object(h)) = data.get("headers") {
            for (k, v) in h {
                if self.header_selected(k) {
                    ex.message.headers.set(k.clone(), v.clone());
                }
            }
        }
    }
}

fn snapshot(ex: &Exchange) -> Value {
    Value::object([("body", ex.message.body.clone()), ("headers", ex.message.headers.to_value())])
}

fn repo(ex: &mut Exchange) -> &mut ObjectMap {
    let entry = ex
        .properties
        .entry(CLAIM_CHECK_REPOSITORY.to_string())
        .or_insert_with(|| Value::object([("keys", Value::Object(Default::default())), ("stack", Value::Array(vec![]))]));
    if !matches!(entry, Value::Object(_)) {
        *entry = Value::object([("keys", Value::Object(Default::default())), ("stack", Value::Array(vec![]))]);
    }
    entry.as_object_mut().unwrap()
}

fn keys_of(ex: &mut Exchange) -> &mut ObjectMap {
    let r = repo(ex);
    if !matches!(r.get("keys"), Some(Value::Object(_))) {
        r.insert("keys".into(), Value::Object(Default::default()));
    }
    r.get_mut("keys").unwrap().as_object_mut().unwrap()
}

fn stack_of(ex: &mut Exchange) -> &mut Vec<Value> {
    let r = repo(ex);
    if !matches!(r.get("stack"), Some(Value::Array(_))) {
        r.insert("stack".into(), Value::Array(vec![]));
    }
    match r.get_mut("stack").unwrap() {
        Value::Array(a) => a,
        _ => unreachable!(),
    }
}

impl ProcessorDefinition for ClaimCheckDefinition {
    fn name(&self) -> &str {
        "claimCheck"
    }
    fn label(&self) -> String {
        format!("claimCheck[{:?}{}]", self.operation, self.key.as_ref().map(|k| format!(", {}", k)).unwrap_or_default())
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let op = self.operation;
        let key = match &self.key {
            Some(k) => Some(rc.context.resolve_placeholders(k)?),
            None => None,
        };
        if matches!(op, ClaimCheckOperation::Set | ClaimCheckOperation::Get | ClaimCheckOperation::GetAndRemove)
            && key.as_deref().map(str::is_empty).unwrap_or(true)
        {
            return Err(CamelError::illegal_argument(format!("Key must be specified for the claim check operation {:?}", op)));
        }
        let filter = MergeFilter::parse(self.filter.as_deref());
        Ok(Arc::new(move |ex: &mut Exchange| {
            match op {
                ClaimCheckOperation::Set => {
                    let data = snapshot(ex);
                    keys_of(ex).insert(key.clone().unwrap(), data);
                }
                ClaimCheckOperation::Get => {
                    if let Some(data) = keys_of(ex).get(key.as_deref().unwrap()).cloned() {
                        filter.merge(ex, &data);
                    }
                }
                ClaimCheckOperation::GetAndRemove => {
                    if let Some(data) = keys_of(ex).remove(key.as_deref().unwrap()) {
                        filter.merge(ex, &data);
                    }
                }
                ClaimCheckOperation::Push => {
                    let data = snapshot(ex);
                    stack_of(ex).push(data);
                }
                ClaimCheckOperation::Pop => {
                    if let Some(data) = stack_of(ex).pop() {
                        filter.merge(ex, &data);
                    }
                }
            }
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}
