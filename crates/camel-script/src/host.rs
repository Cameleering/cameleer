//! Exchange bindings: `exchange`, `request`/`message`, `headers`, `properties`, `variables`,
//! `camelContext`, `log` and the OGNL/MVEL/SpEL root object.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use camel_core::context::CamelContext;
use camel_core::message::{Exchange, Headers};
use camel_core::value::Value;

use crate::ast::Dialect;
use crate::interp::{Interp, R};
use crate::value::*;

const VAR_PREFIX: &str = "CamelVariable.";

/// The exchange state visible to a script (a working copy written back after mutable evaluation).
pub struct HostState {
    pub exchange_id: String,
    pub pattern: String,
    pub message_id: String,
    pub context: Option<CamelContext>,
    pub headers: RefCell<Headers>,
    body_value: RefCell<Value>,
    body: RefCell<Option<V>>,
    pub properties: RefCell<BTreeMap<String, Value>>,
    pub exception: Option<(String, String)>,
    pub route_id: Option<String>,
}

impl HostState {
    /// Build from the parts of an exchange.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        exchange_id: String,
        pattern: String,
        message_id: String,
        context: Option<CamelContext>,
        headers: Headers,
        body: Value,
        properties: BTreeMap<String, Value>,
        exception: Option<(String, String)>,
        route_id: Option<String>,
    ) -> HostState {
        HostState {
            exchange_id,
            pattern,
            message_id,
            context,
            headers: RefCell::new(headers),
            body_value: RefCell::new(body),
            body: RefCell::new(None),
            properties: RefCell::new(properties),
            exception,
            route_id,
        }
    }

    /// The body as a runtime value (converted once, shared afterwards).
    pub fn body(&self) -> V {
        if let Some(b) = self.body.borrow().as_ref() {
            return b.clone();
        }
        let v = from_value(&self.body_value.borrow());
        *self.body.borrow_mut() = Some(v.clone());
        v
    }

    /// Replace the body.
    pub fn set_body(&self, v: V) {
        *self.body.borrow_mut() = Some(v);
    }

    /// The raw body (without conversion) when untouched.
    pub fn body_value(&self) -> Value {
        match self.body.borrow().as_ref() {
            Some(v) => to_value(v),
            None => self.body_value.borrow().clone(),
        }
    }

    /// Convert the body to a Camel value now (before the object graph is torn down).
    pub fn freeze_body(&self) {
        let v = self.body.borrow_mut().take();
        if let Some(v) = v {
            *self.body_value.borrow_mut() = to_value(&v);
        }
    }

    /// Take the (possibly modified) headers, body and properties.
    pub fn into_parts(self) -> (Headers, Value, BTreeMap<String, Value>) {
        let body = match self.body.into_inner() {
            Some(v) => to_value(&v),
            None => self.body_value.into_inner(),
        };
        (self.headers.into_inner(), body, self.properties.into_inner())
    }
}

/// Define the exchange bindings in the interpreter's globals.
pub fn install_bindings(it: &mut Interp) {
    let g = it.globals.clone();
    let d = it.d;
    if d == Dialect::Jsonnet {
        return;
    }
    let ex = V::host(Host::Exchange);
    let msg = V::host(Host::Message);
    g.define("exchange", ex);
    g.define("request", msg.clone());
    g.define("message", msg.clone());
    g.define("response", msg.clone());
    g.define("context", V::host(Host::Context));
    g.define("camelContext", V::host(Host::Context));
    g.define("headers", V::host(Host::Headers));
    g.define("properties", V::host(Host::Properties));
    g.define("exchangeProperties", V::host(Host::Properties));
    g.define("variables", V::host(Host::Variables));
    g.define("log", V::host(Host::Log));
    if matches!(d, Dialect::Ognl | Dialect::Spel | Dialect::Mvel) {
        let root = V::host(Host::Root);
        g.define("#root", root.clone());
        g.define("#this", root.clone());
        g.define("this", root);
        g.define("in", msg.clone());
        g.define("out", msg);
    }
    let body = it.host.body();
    g.define("body", body);
}

/// A member of the OGNL / MVEL / SpEL root object.
pub fn root_member(it: &mut Interp, name: &str) -> Option<V> {
    Some(match name {
        "exchange" => V::host(Host::Exchange),
        "context" | "camelContext" => V::host(Host::Context),
        "request" | "in" | "message" | "out" | "response" => V::host(Host::Message),
        "body" => it.host.body(),
        "headers" => V::host(Host::Headers),
        "properties" | "exchangeProperties" => V::host(Host::Properties),
        "variables" => V::host(Host::Variables),
        "exception" => exception_value(it),
        "exchangeId" => V::str(&it.host.exchange_id),
        _ => return None,
    })
}

fn exception_value(it: &Interp) -> V {
    match &it.host.exception {
        Some((kind, msg)) => it.make_error(kind, msg.clone(), None),
        None => V::Null,
    }
}

fn header(it: &Interp, name: &str) -> V {
    it.host.headers.borrow().get(name).map(from_value).unwrap_or(V::Null)
}

fn property(it: &Interp, name: &str) -> V {
    it.host.properties.borrow().get(name).map(from_value).unwrap_or(V::Null)
}

fn variable(it: &Interp, name: &str) -> V {
    it.host.properties.borrow().get(&format!("{}{}", VAR_PREFIX, name)).map(from_value).unwrap_or(V::Null)
}

fn arg_str(it: &Interp, args: &[V], i: usize) -> String {
    args.get(i).map(|v| it.display(v)).unwrap_or_default()
}

/// Convert to a Java-like type given a type value (`String.class`, `"Integer"`, `Integer`).
fn convert_arg(it: &mut Interp, v: V, ty: Option<&V>, pos: u32) -> R<V> {
    let t = match ty {
        None => return Ok(v),
        Some(V::Str(s)) => s.to_string(),
        Some(V::Host(h)) => match &**h {
            Host::Ns(n) => n.to_string(),
            Host::Type(n) => n.clone(),
            _ => return Ok(v),
        },
        Some(_) => return Ok(v),
    };
    crate::builtins::as_type(it, v, &t, pos)
}

/// A snapshot object of a map-like host (headers, properties, variables).
pub fn snapshot(it: &Interp, h: &Host) -> Option<V> {
    let mut pm = PropMap::default();
    match h {
        Host::Headers => {
            for (k, v) in it.host.headers.borrow().iter() {
                pm.set(k, from_value(v));
            }
        }
        Host::Properties => {
            for (k, v) in it.host.properties.borrow().iter() {
                if !k.starts_with(VAR_PREFIX) {
                    pm.set(k, from_value(v));
                }
            }
        }
        Host::Variables => {
            for (k, v) in it.host.properties.borrow().iter() {
                if let Some(n) = k.strip_prefix(VAR_PREFIX) {
                    pm.set(n, from_value(v));
                }
            }
        }
        Host::Message => {
            let mut m = PropMap::default();
            m.set("body", it.host.body());
            m.set("headers", snapshot(it, &Host::Headers).unwrap_or(V::Null));
            return Some(V::obj(m));
        }
        _ => return None,
    }
    Some(V::obj(pm))
}

/// Whether a map-like host has the key.
pub fn contains_key(it: &Interp, h: &Host, k: &str) -> bool {
    match h {
        Host::Headers => it.host.headers.borrow().contains(k),
        Host::Properties => it.host.properties.borrow().contains_key(k),
        Host::Variables => it.host.properties.borrow().contains_key(&format!("{}{}", VAR_PREFIX, k)),
        _ => false,
    }
}

/// Remove a key of a map-like host.
pub fn remove_key(it: &Interp, h: &Host, k: &str) -> Option<V> {
    match h {
        Host::Headers => it.host.headers.borrow_mut().remove(k).map(|v| from_value(&v)),
        Host::Properties => it.host.properties.borrow_mut().remove(k).map(|v| from_value(&v)),
        Host::Variables => it.host.properties.borrow_mut().remove(&format!("{}{}", VAR_PREFIX, k)).map(|v| from_value(&v)),
        _ => None,
    }
}

fn put_key(it: &Interp, h: &Host, k: &str, v: &V) -> bool {
    let val = to_value(v);
    match h {
        Host::Headers => it.host.headers.borrow_mut().set(k, val),
        Host::Properties => {
            it.host.properties.borrow_mut().insert(k.to_string(), val);
        }
        Host::Variables => {
            it.host.properties.borrow_mut().insert(format!("{}{}", VAR_PREFIX, k), val);
        }
        _ => return false,
    }
    true
}

/// Lower-case the first letter (bean property names).
pub fn lc_first_pub(s: &str) -> String {
    lc_first(s)
}

fn lc_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Property read on a host object.
pub fn get_member(it: &mut Interp, h: &Rc<Host>, recv: &V, name: &str, pos: u32) -> R<V> {
    let method = || V::Method(Rc::new((recv.clone(), name.to_string())));
    Ok(match &**h {
        Host::Exchange => match name {
            "in" | "message" | "out" | "request" | "response" => V::host(Host::Message),
            "properties" | "exchangeProperties" | "allProperties" => V::host(Host::Properties),
            "variables" => V::host(Host::Variables),
            "exchangeId" => V::str(&it.host.exchange_id),
            "context" | "camelContext" => V::host(Host::Context),
            "pattern" => V::str(&it.host.pattern),
            "exception" => exception_value(it),
            "failed" => V::Bool(it.host.exception.is_some()),
            "fromRouteId" => it.host.route_id.as_deref().map(V::str).unwrap_or(V::Null),
            _ => method(),
        },
        Host::Message => match name {
            "body" => it.host.body(),
            "headers" => V::host(Host::Headers),
            "messageId" => V::str(&it.host.message_id),
            "exchange" => V::host(Host::Exchange),
            _ => method(),
        },
        Host::Headers => {
            if it.host.headers.borrow().contains(name) || it.d != Dialect::Js || !is_map_method(name) {
                header(it, name)
            } else {
                method()
            }
        }
        Host::Properties => {
            if it.host.properties.borrow().contains_key(name) || !is_map_method(name) {
                property(it, name)
            } else {
                method()
            }
        }
        Host::Variables => {
            if contains_key(it, h, name) || !is_map_method(name) {
                variable(it, name)
            } else {
                method()
            }
        }
        Host::Context => match name {
            "name" => V::string(it.host.context.as_ref().map(|c| c.name().to_string()).unwrap_or_else(|| "camel-1".into())),
            "version" => V::str(env!("CARGO_PKG_VERSION")),
            _ => method(),
        },
        Host::Log => method(),
        Host::Root => match root_member(it, name) {
            Some(v) => v,
            None => method(),
        },
        Host::Ns(n) => return crate::builtins::ns_member(it, n, name, pos),
        Host::Bean(b) => {
            let bean = bean_value(it, b, pos)?;
            if let V::Host(bh) = &bean {
                if matches!(&**bh, Host::Bean(_)) {
                    return Ok(method());
                }
            }
            return it.get_member(&bean, name, pos);
        }
        Host::Meta(_) => method(),
        Host::NsFn(..) | Host::Type(_) => {
            if name == "name" || name == "simpleName" {
                return Ok(V::string(match &**h {
                    Host::Type(n) => n.clone(),
                    Host::NsFn(_, n) => n.clone(),
                    _ => String::new(),
                }));
            }
            if name == "class" {
                return Ok(recv.clone());
            }
            method()
        }
    })
}

fn is_map_method(name: &str) -> bool {
    matches!(
        name,
        "get" | "put" | "set" | "containsKey" | "has" | "remove" | "delete" | "keySet" | "keys" | "values" | "size" | "isEmpty" | "clear" | "putAll" | "entrySet" | "items" | "forEach" | "each" | "getOrDefault"
    )
}

/// Property write on a host object.
pub fn set_member(it: &mut Interp, h: &Rc<Host>, name: &str, v: V, pos: u32) -> R<()> {
    match &**h {
        Host::Message | Host::Root if name == "body" => {
            it.host.set_body(v);
            Ok(())
        }
        Host::Message if name == "headers" => {
            it.host.headers.borrow_mut().clear();
            if let V::Obj(o) = &v {
                for (k, x) in o.borrow().props.iter() {
                    it.host.headers.borrow_mut().set(k.to_string(), to_value(x));
                }
            }
            Ok(())
        }
        Host::Headers | Host::Properties | Host::Variables => {
            put_key(it, h, name, &v);
            Ok(())
        }
        Host::Exchange if name == "pattern" => Ok(()),
        _ => it.type_error(format!("cannot set property '{}' of {}", name, it.display(&V::Host(h.clone()))), Some(pos)),
    }
}

fn slf4j(it: &Interp, args: &[V]) -> String {
    let mut msg = arg_str(it, args, 0);
    for a in args.iter().skip(1) {
        if let Some(i) = msg.find("{}") {
            msg.replace_range(i..i + 2, &it.display(a));
        } else {
            msg.push(' ');
            msg.push_str(&it.display(a));
        }
    }
    msg
}

/// Method call on a host object.
pub fn call_method(it: &mut Interp, h: &Rc<Host>, recv: &V, name: &str, args: Vec<V>, pos: u32) -> R<V> {
    match &**h {
        Host::Bean(b) => return call_bean(it, b, name, args, pos),
        Host::Meta(_) => return it.type_error(format!("metaClass.{}() is not supported", name), Some(pos)),
        Host::Exchange => match name {
            "getIn" | "getMessage" | "getOut" | "getRequest" => return Ok(V::host(Host::Message)),
            "getProperty" | "getExchangeProperty" => {
                let v = property(it, &arg_str(it, &args, 0));
                return default_or_convert(it, v, args.get(1), pos);
            }
            "setProperty" | "setExchangeProperty" => {
                let k = arg_str(it, &args, 0);
                put_key(it, &Host::Properties, &k, args.get(1).unwrap_or(&V::Null));
                return Ok(V::Undef);
            }
            "removeProperty" => return Ok(remove_key(it, &Host::Properties, &arg_str(it, &args, 0)).unwrap_or(V::Null)),
            "getProperties" | "getAllProperties" => return Ok(V::host(Host::Properties)),
            "hasProperties" => return Ok(V::Bool(!it.host.properties.borrow().is_empty())),
            "getVariable" => {
                let v = variable(it, &arg_str(it, &args, 0));
                return default_or_convert(it, v, args.get(1), pos);
            }
            "setVariable" => {
                let k = arg_str(it, &args, 0);
                put_key(it, &Host::Variables, &k, args.get(1).unwrap_or(&V::Null));
                return Ok(V::Undef);
            }
            "removeVariable" => return Ok(remove_key(it, &Host::Variables, &arg_str(it, &args, 0)).unwrap_or(V::Null)),
            "getVariables" => return Ok(V::host(Host::Variables)),
            "getExchangeId" => return Ok(V::str(&it.host.exchange_id)),
            "getContext" | "getCamelContext" => return Ok(V::host(Host::Context)),
            "getPattern" => return Ok(V::str(&it.host.pattern)),
            "isFailed" => return Ok(V::Bool(it.host.exception.is_some())),
            "getException" => return Ok(exception_value(it)),
            "getFromRouteId" => return Ok(it.host.route_id.as_deref().map(V::str).unwrap_or(V::Null)),
            "toString" => return Ok(V::string(format!("Exchange[{}]", it.host.exchange_id))),
            _ => {}
        },
        Host::Message => match name {
            "getBody" | "getMandatoryBody" => {
                let b = it.host.body();
                return convert_arg(it, b, args.first(), pos);
            }
            "setBody" => {
                let v = args.first().cloned().unwrap_or(V::Null);
                let v = convert_arg(it, v, args.get(1), pos)?;
                it.host.set_body(v);
                return Ok(V::Undef);
            }
            "getHeader" => {
                let v = header(it, &arg_str(it, &args, 0));
                return default_or_convert(it, v, args.get(1), pos);
            }
            "setHeader" => {
                let k = arg_str(it, &args, 0);
                put_key(it, &Host::Headers, &k, args.get(1).unwrap_or(&V::Null));
                return Ok(V::Undef);
            }
            "removeHeader" => return Ok(remove_key(it, &Host::Headers, &arg_str(it, &args, 0)).unwrap_or(V::Null)),
            "removeHeaders" => {
                let pattern = arg_str(it, &args, 0);
                let n = it.host.headers.borrow_mut().remove_matching(&pattern, &[]);
                return Ok(V::Bool(n > 0));
            }
            "getHeaders" => return Ok(V::host(Host::Headers)),
            "setHeaders" => {
                return set_member(it, h, "headers", args.first().cloned().unwrap_or(V::Null), pos).map(|_| V::Undef);
            }
            "hasHeaders" => return Ok(V::Bool(!it.host.headers.borrow().is_empty())),
            "getMessageId" => return Ok(V::str(&it.host.message_id)),
            "getExchange" => return Ok(V::host(Host::Exchange)),
            "toString" => {
                let b = it.host.body();
                return Ok(V::string(format!("Message[{}]", it.display(&b))));
            }
            _ => {}
        },
        Host::Headers | Host::Properties | Host::Variables => match name {
            "get" | "getOrDefault" => {
                let k = arg_str(it, &args, 0);
                if !contains_key(it, h, &k) {
                    return Ok(args.get(1).cloned().unwrap_or(V::Null));
                }
                return get_member_plain(it, h, &k);
            }
            "put" | "set" | "setdefault" | "__setitem__" => {
                let k = arg_str(it, &args, 0);
                let old = if contains_key(it, h, &k) { get_member_plain(it, h, &k)? } else { V::Null };
                if name == "setdefault" && contains_key(it, h, &k) {
                    return Ok(old);
                }
                put_key(it, h, &k, args.get(1).unwrap_or(&V::Null));
                return Ok(if name == "setdefault" { args.get(1).cloned().unwrap_or(V::Null) } else { old });
            }
            "containsKey" | "has" | "__contains__" => return Ok(V::Bool(contains_key(it, h, &arg_str(it, &args, 0)))),
            "remove" | "delete" | "pop" => {
                let k = arg_str(it, &args, 0);
                return Ok(remove_key(it, h, &k).or_else(|| args.get(1).cloned()).unwrap_or(V::Null));
            }
            "clear" => {
                match &**h {
                    Host::Headers => it.host.headers.borrow_mut().clear(),
                    Host::Properties => it.host.properties.borrow_mut().retain(|k, _| k.starts_with(VAR_PREFIX)),
                    _ => it.host.properties.borrow_mut().retain(|k, _| !k.starts_with(VAR_PREFIX)),
                }
                return Ok(V::Undef);
            }
            "putAll" | "update" => {
                if let Some(V::Obj(o)) = args.first() {
                    let items: Vec<(String, V)> = o.borrow().props.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
                    for (k, v) in items {
                        put_key(it, h, &k, &v);
                    }
                }
                return Ok(V::Undef);
            }
            "size" => return Ok(V::Int(snapshot(it, h).and_then(|s| if let V::Obj(o) = s { Some(o.borrow().props.len() as i64) } else { None }).unwrap_or(0))),
            _ => {}
        },
        Host::Context => match name {
            "getName" => return Ok(V::string(it.host.context.as_ref().map(|c| c.name().to_string()).unwrap_or_else(|| "camel-1".into()))),
            "getVersion" => return Ok(V::str(env!("CARGO_PKG_VERSION"))),
            "resolvePropertyPlaceholders" => {
                let s = arg_str(it, &args, 0);
                return match &it.host.context {
                    Some(c) => match c.resolve_placeholders(&s) {
                        Ok(r) => Ok(V::string(r)),
                        Err(e) => it.throw(&e.kind, e.message, Some(pos)),
                    },
                    None => Ok(V::string(s)),
                };
            }
            "toString" => return Ok(V::string(it.host.context.as_ref().map(|c| c.name().to_string()).unwrap_or_default())),
            _ => {}
        },
        Host::Log => {
            let msg = slf4j(it, &args);
            let logger = "camel.script";
            use camel_core::log::{log, Level};
            let level = match name {
                "trace" => Level::Trace,
                "debug" => Level::Debug,
                "info" | "log" => Level::Info,
                "warn" | "warning" => Level::Warn,
                "error" => Level::Error,
                _ => return it.type_error(format!("log.{} is not a function", name), Some(pos)),
            };
            log(level, logger, msg);
            return Ok(V::Undef);
        }
        Host::Root => match name {
            "property" | "exchangeProperty" => {
                let v = property(it, &arg_str(it, &args, 0));
                return convert_arg(it, v, args.get(1), pos);
            }
            "header" => {
                let v = header(it, &arg_str(it, &args, 0));
                return convert_arg(it, v, args.get(1), pos);
            }
            "variable" => {
                let v = variable(it, &arg_str(it, &args, 0));
                return convert_arg(it, v, args.get(1), pos);
            }
            _ => {}
        },
        Host::Ns(n) => return crate::builtins::call_ns_fn(it, n, name, args, pos),
        Host::NsFn(..) | Host::Type(_) => {}
    }
    // JavaBean getter: getFoo() / isFoo() -> foo
    if args.is_empty() {
        let prop = name.strip_prefix("get").or_else(|| name.strip_prefix("is")).filter(|r| r.chars().next().is_some_and(|c| c.is_ascii_uppercase()));
        if let Some(p) = prop {
            let m = get_member(it, h, recv, &lc_first(p), pos)?;
            if !matches!(m, V::Method(_)) {
                return Ok(m);
            }
        }
    }
    // map-like hosts: delegate read-only methods to a snapshot
    if let Some(snap) = snapshot(it, h) {
        return it.call_method(&snap, name, args, pos);
    }
    let what = it.display(recv);
    match it.d {
        Dialect::Groovy => it.throw("MissingMethodException", format!("No signature of method: {}.{}() is applicable", what, name), Some(pos)),
        Dialect::Spel => it.throw("SpelEvaluationException", format!("EL1004E: Method call: Method {}() cannot be found on type {}", name, what), Some(pos)),
        Dialect::Python => it.throw("AttributeError", format!("'{}' object has no attribute '{}'", what, name), Some(pos)),
        _ => it.type_error(format!("{}.{} is not a function", what, name), Some(pos)),
    }
}

fn get_member_plain(it: &mut Interp, h: &Host, k: &str) -> R<V> {
    Ok(match h {
        Host::Headers => header(it, k),
        Host::Properties => property(it, k),
        _ => variable(it, k),
    })
}

fn default_or_convert(it: &mut Interp, v: V, second: Option<&V>, pos: u32) -> R<V> {
    match second {
        None => Ok(v),
        Some(t @ V::Host(_)) => convert_arg(it, v, Some(t), pos),
        Some(d) => Ok(if v.is_nullish() { d.clone() } else { v }),
    }
}

// ====================================================================== registry beans (SpEL `@name`)

fn registry_value(any: &std::sync::Arc<dyn std::any::Any + Send + Sync>) -> Option<Value> {
    if let Some(v) = any.downcast_ref::<Value>() {
        return Some(v.clone());
    }
    if let Some(v) = any.downcast_ref::<String>() {
        return Some(Value::String(v.clone()));
    }
    if let Some(v) = any.downcast_ref::<&'static str>() {
        return Some(Value::String(v.to_string()));
    }
    if let Some(v) = any.downcast_ref::<i64>() {
        return Some(Value::Int(*v));
    }
    if let Some(v) = any.downcast_ref::<i32>() {
        return Some(Value::Int(*v as i64));
    }
    if let Some(v) = any.downcast_ref::<f64>() {
        return Some(Value::Float(*v));
    }
    if let Some(v) = any.downcast_ref::<bool>() {
        return Some(Value::Bool(*v));
    }
    if let Some(v) = any.downcast_ref::<Vec<Value>>() {
        return Some(Value::Array(v.clone()));
    }
    if let Some(v) = any.downcast_ref::<BTreeMap<String, Value>>() {
        let mut m = camel_core::value::ObjectMap::new();
        for (k, x) in v {
            m.insert(k.clone(), x.clone());
        }
        return Some(Value::Object(m));
    }
    None
}

/// `@name`: a registry bean. Plain values (`Value`, strings, numbers, booleans, lists, maps) are
/// returned as values; invokable beans (bean methods, `Bean` objects, processors) as a bean
/// reference whose methods are invoked through the registry.
pub fn bean_ref(it: &mut Interp, name: &str, pos: u32) -> R<V> {
    if let Some(v) = it.beans.get(name) {
        return Ok(v.clone());
    }
    let Some(ctx) = it.host.context.clone() else {
        return it.throw("SpelEvaluationException", format!("EL1057E: No bean resolver registered in the context to resolve access to bean '{}'", name), Some(pos));
    };
    let reg = ctx.registry();
    let v = match reg.lookup_any(name) {
        Some(any) => match registry_value(&any) {
            Some(v) => from_value(&v),
            None => V::host(Host::Bean(name.to_string())),
        },
        None => {
            // bean methods bound as `name.method`
            let prefix = format!("{}.", name);
            if reg.names().iter().any(|n| n.starts_with(&prefix)) {
                V::host(Host::Bean(name.to_string()))
            } else {
                return it.throw("SpelEvaluationException", format!("EL1058E: A problem occurred when trying to resolve bean '{}': no bean named '{}' in the registry", name, name), Some(pos));
            }
        }
    };
    it.beans.insert(name.to_string(), v.clone());
    Ok(v)
}

fn bean_value(it: &mut Interp, name: &str, pos: u32) -> R<V> {
    bean_ref(it, name, pos)
}

/// An exchange built from the script's view (for invoking registry beans).
fn exchange_copy(it: &Interp, body: Value) -> Exchange {
    let mut ex = Exchange::with_body(it.host.context.clone(), body);
    ex.exchange_id = it.host.exchange_id.clone();
    ex.message.headers = it.host.headers.borrow().clone();
    ex.properties = it.host.properties.borrow().clone();
    ex
}

/// Invoke method `method` of registry bean `name` with `args` (the single argument, or the
/// argument list, becomes the message body; no argument keeps the current body).
fn call_bean(it: &mut Interp, name: &str, method: &str, args: Vec<V>, pos: u32) -> R<V> {
    let Some(ctx) = it.host.context.clone() else {
        return it.throw("SpelEvaluationException", format!("EL1057E: No bean resolver registered in the context to resolve access to bean '{}'", name), Some(pos));
    };
    let body = match args.len() {
        0 => it.host.body_value(),
        1 => to_value(&args[0]),
        _ => Value::Array(args.iter().map(to_value).collect()),
    };
    let reg = ctx.registry();
    for cand in [format!("{}.{}", name, method), format!("{}::{}", name, method)] {
        if let Some(b) = reg.lookup::<camel_core::language::support::BeanMethod>(&cand) {
            let ex = exchange_copy(it, body);
            return match b.invoke(&ex) {
                Ok(v) => Ok(from_value(&v)),
                Err(e) => it.throw(&e.kind, e.message, Some(pos)),
            };
        }
    }
    if let Some(bean) = camel_core::component::bean::lookup_bean(reg, name) {
        let mut ex = exchange_copy(it, body);
        return match bean.invoke(Some(method), &mut ex) {
            Ok(()) => Ok(from_value(&ex.message.body)),
            Err(e) => it.throw(&e.kind, e.message, Some(pos)),
        };
    }
    if let Some(b) = reg.lookup::<camel_core::language::support::BeanMethod>(name) {
        let ex = exchange_copy(it, body);
        return match b.invoke(&ex) {
            Ok(v) => Ok(from_value(&v)),
            Err(e) => it.throw(&e.kind, e.message, Some(pos)),
        };
    }
    it.throw("SpelEvaluationException", format!("EL1004E: Method call: Method {}() cannot be found on bean '{}'", method, name), Some(pos))
}
