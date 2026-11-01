//! Built-in globals, namespaces (`Math`, `JSON`, `Object`, Python builtins, Java classes...) and
//! the methods of strings, lists, maps, numbers, dates and regular expressions.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use camel_core::language::support::{random_u64, uuid_v4};
use camel_core::util_time::{civil_from_epoch, epoch_from_civil, now_millis};

use crate::ast::{BinOp, Dialect};
use crate::interp::{deep_eq, error_is_a, java_num_str, js_num_str, norm_num, Interp, R};
use crate::json::{self, JsonOpts};
use crate::value::*;

// ====================================================================== names

const CLASS_NS: &[&str] = &[
    "Math", "JSON", "Object", "Array", "String", "Number", "Boolean", "Date", "Error", "TypeError", "RangeError", "SyntaxError", "ReferenceError", "RegExp", "Integer",
    "Long", "Double", "Float", "Short", "Byte", "BigDecimal", "BigInteger", "Character", "List", "ArrayList", "LinkedList", "Map", "HashMap", "LinkedHashMap",
    "TreeMap", "Set", "HashSet", "LinkedHashSet", "TreeSet", "System", "UUID", "JsonSlurper", "JsonOutput", "Collections", "Arrays", "Pattern", "StringBuilder",
    "Closure", "Iterable", "Collection", "CharSequence", "GString", "Function", "Promise", "Symbol", "WeakMap", "WeakSet", "Java", "Reflect", "Optional", "Objects",
    "Collectors", "Stream", "IntStream", "Character", "SystemOut", "Thread", "AggregateError",
];

const FN_NS: &[&str] = &[
    "parseInt", "parseFloat", "isNaN", "isFinite", "encodeURIComponent", "decodeURIComponent", "encodeURI", "decodeURI", "console", "print", "println", "sprintf",
    "__pattern", "__isEmpty", "__bean", "__kwargs", "len", "str", "int", "float", "bool", "list", "dict", "tuple", "set", "range", "sum", "min", "max", "sorted",
    "reversed", "enumerate", "zip", "map", "filter", "any", "all", "abs", "round", "isinstance", "repr", "ord", "chr", "hex", "type", "divmod", "pow", "hasattr",
    "getattr", "json", "math", "re", "std", "cml", "ds", "setTimeout", "clearTimeout", "setInterval", "clearInterval", "queueMicrotask", "next", "iter", "issubclass",
    "setattr", "callable", "id", "vars",
];

/// A class-like namespace by (simple) name.
pub fn type_ns(name: &str) -> Option<V> {
    CLASS_NS.iter().find(|n| **n == name).map(|n| V::host(Host::Ns(n)))
}

fn fn_ns(name: &str) -> Option<&'static str> {
    FN_NS.iter().find(|n| **n == name).copied()
}

fn def_ns(it: &Interp, names: &[&'static str]) {
    for n in names {
        it.globals.define(n, V::host(Host::Ns(n)));
    }
}

/// Install the dialect's globals and the exchange bindings.
pub fn install_globals(it: &mut Interp) {
    match it.d {
        Dialect::Js => {
            def_ns(
                it,
                &[
                    "Math", "JSON", "Object", "Array", "String", "Number", "Boolean", "Date", "Error", "TypeError", "RangeError", "SyntaxError", "ReferenceError", "RegExp",
                    "parseInt", "parseFloat", "isNaN", "isFinite", "encodeURIComponent", "decodeURIComponent", "encodeURI", "decodeURI", "console", "print",
                    "Promise", "Map", "Set", "WeakMap", "WeakSet", "Symbol", "Java", "Reflect", "setTimeout", "clearTimeout", "setInterval", "clearInterval",
                    "queueMicrotask", "AggregateError",
                ],
            );
            it.globals.define("NaN", V::Num(f64::NAN));
            it.globals.define("Infinity", V::Num(f64::INFINITY));
            crate::jsext::install(it);
        }
        Dialect::Groovy => {
            def_ns(
                it,
                &[
                    "Math", "Integer", "Long", "Double", "Float", "Boolean", "String", "BigDecimal", "BigInteger", "List", "Map", "Set", "Date", "Object", "Number", "System", "UUID",
                    "JsonSlurper", "JsonOutput", "Collections", "Arrays", "Pattern", "println", "print", "sprintf",
                ],
            );
            crate::groovyext::install(it);
        }
        Dialect::Python => {
            def_ns(
                it,
                &[
                    "len", "str", "int", "float", "bool", "list", "dict", "tuple", "set", "range", "sum", "min", "max", "sorted", "reversed", "enumerate", "zip", "map", "filter",
                    "any", "all", "abs", "round", "isinstance", "print", "repr", "ord", "chr", "hex", "type", "divmod", "pow", "hasattr", "getattr", "json", "math", "re",
                    "next", "iter", "issubclass", "setattr", "callable", "id", "vars",
                ],
            );
            crate::pyext::install(it);
            for e in ["Exception", "ValueError", "TypeError", "KeyError", "IndexError", "ZeroDivisionError", "RuntimeError", "AttributeError", "NameError", "AssertionError", "StopIteration", "NotImplementedError"] {
                it.globals.define(e, V::host(Host::Type(e.to_string())));
            }
        }
        Dialect::Jsonnet => def_ns(it, &["std", "cml", "ds"]),
        Dialect::Java => crate::java::install(it),
        Dialect::Ognl | Dialect::Mvel | Dialect::Spel => {}
    }
    crate::host::install_bindings(it);
}

/// Fallback global functions callable by name (`__isEmpty`, `__pattern`, ...).
pub fn script_function(_it: &Interp, name: &str) -> Option<V> {
    if name.starts_with("__") {
        return fn_ns(name).map(|n| V::host(Host::Ns(n)));
    }
    None
}

// ====================================================================== type names / display

/// Python type name.
pub fn py_type_name(v: &V) -> &'static str {
    match v {
        V::Undef | V::Null => "NoneType",
        V::Bool(_) => "bool",
        V::Int(_) => "int",
        V::Num(_) => "float",
        V::Str(_) => "str",
        V::Bytes(_) => "bytes",
        V::Arr(_) => "list",
        V::Obj(o) => match o.borrow().kind {
            ObjKind::Set => "set",
            ObjKind::Error => "Exception",
            _ => "dict",
        },
        V::Func(_) | V::Method(_) => "function",
        V::Host(_) => "object",
        V::Regex(_) => "re.Pattern",
    }
}

/// Java class name of a value (Groovy, OGNL, MVEL, SpEL).
pub fn java_type(v: &V) -> String {
    match v {
        V::Undef | V::Null => "null".into(),
        V::Bool(_) => "java.lang.Boolean".into(),
        V::Int(i) if i32::try_from(*i).is_ok() => "java.lang.Integer".into(),
        V::Int(_) => "java.lang.Long".into(),
        V::Num(_) => "java.math.BigDecimal".into(),
        V::Str(_) => "java.lang.String".into(),
        V::Bytes(_) => "[B".into(),
        V::Arr(_) => "java.util.ArrayList".into(),
        V::Obj(o) => match &o.borrow().kind {
            ObjKind::Set => "java.util.LinkedHashSet".into(),
            ObjKind::Date(_) => "java.util.Date".into(),
            ObjKind::Error => o.borrow().props.get("name").and_then(|n| n.as_str().map(|s| s.to_string())).unwrap_or_else(|| "java.lang.Exception".into()),
            _ => "java.util.LinkedHashMap".into(),
        },
        V::Func(_) | V::Method(_) => "groovy.lang.Closure".into(),
        V::Host(h) => match &**h {
            Host::Exchange => "org.apache.camel.Exchange".into(),
            Host::Message => "org.apache.camel.Message".into(),
            Host::Context => "org.apache.camel.CamelContext".into(),
            _ => "java.lang.Object".into(),
        },
        V::Regex(_) => "java.util.regex.Pattern".into(),
    }
}

/// JS `typeof`.
pub fn js_typeof(v: &V) -> &'static str {
    if let V::Obj(o) = v {
        if matches!(o.borrow().kind, ObjKind::Symbol(..)) {
            return "symbol";
        }
        if let Some(t) = crate::jsext::typeof_of(v) {
            return t;
        }
    }
    if matches!(v, V::Host(h) if matches!(**h, Host::Ns("globalThis") | Host::Ns("Intl"))) {
        return "object";
    }
    match v {
        V::Undef => "undefined",
        V::Null => "object",
        V::Bool(_) => "boolean",
        V::Int(_) | V::Num(_) => "number",
        V::Str(_) => "string",
        V::Func(_) | V::Method(_) => "function",
        V::Host(h) if matches!(**h, Host::Ns(n) if n != "Math" && n != "JSON" && n != "console") || matches!(**h, Host::NsFn(..)) => "function",
        _ => "object",
    }
}

/// Jsonnet `std.type`.
pub fn jsonnet_type(v: &V) -> &'static str {
    match v {
        V::Undef | V::Null => "null",
        V::Bool(_) => "boolean",
        V::Int(_) | V::Num(_) => "number",
        V::Str(_) => "string",
        V::Arr(_) => "array",
        V::Obj(_) => "object",
        _ => "function",
    }
}

/// Jsonnet compact manifestation (`std.toString`).
pub fn jsonnet_manifest(v: &V) -> String {
    json::stringify(v, &JsonOpts { indent: None, item_sep: " ", key_sep: " ", sort_keys: true, js: false, newline: "\n" }).unwrap_or_else(|| "null".into())
}

/// Jsonnet string conversion (strings unchanged, everything else manifested).
pub fn jsonnet_to_string(_it: &Interp, v: &V) -> String {
    match v {
        V::Str(s) => s.to_string(),
        other => jsonnet_manifest(other),
    }
}

/// Python `repr`.
pub fn py_repr(it: &Interp, v: &V) -> String {
    match v {
        V::Str(s) => {
            let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
            let mut out = String::new();
            out.push(q);
            for c in s.chars() {
                match c {
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\r' => out.push_str("\\r"),
                    '\\' => out.push_str("\\\\"),
                    c if c == q => {
                        out.push('\\');
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
            out.push(q);
            out
        }
        other => display(it, other, 0),
    }
}

fn display_nested(it: &Interp, v: &V, depth: usize) -> String {
    match (it.d, v) {
        (Dialect::Python, V::Str(_)) => py_repr(it, v),
        (Dialect::Groovy, V::Str(s)) if depth > 0 && false => s.to_string(),
        _ => display(it, v, depth + 1),
    }
}

/// String conversion according to the dialect.
pub fn display(it: &Interp, v: &V, depth: usize) -> String {
    let d = it.d;
    if depth > 40 {
        return "...".into();
    }
    match v {
        V::Undef => match d {
            Dialect::Js => "undefined".into(),
            Dialect::Python => "None".into(),
            _ => "null".into(),
        },
        V::Null => if d == Dialect::Python { "None" } else { "null" }.into(),
        V::Bool(b) => match (d, b) {
            (Dialect::Python, true) => "True".into(),
            (Dialect::Python, false) => "False".into(),
            (_, b) => b.to_string(),
        },
        V::Int(i) => i.to_string(),
        V::Num(f) => match d {
            Dialect::Js | Dialect::Jsonnet => js_num_str(*f),
            Dialect::Python => java_num_str(*f, true),
            _ => java_num_str(*f, false),
        },
        V::Str(s) => s.to_string(),
        V::Bytes(b) => {
            if d == Dialect::Python {
                format!("b'{}'", String::from_utf8_lossy(b))
            } else {
                String::from_utf8_lossy(b).into_owned()
            }
        }
        V::Arr(a) => {
            if d == Dialect::Python {
                if let Some(s) = it.namedtuple_display(a) {
                    return s;
                }
            }
            let items = a.borrow().clone();
            match d {
                Dialect::Js => items.iter().map(|x| if x.is_nullish() { String::new() } else { display(it, x, depth + 1) }).collect::<Vec<_>>().join(","),
                Dialect::Jsonnet => jsonnet_manifest(v),
                _ => format!("[{}]", items.iter().map(|x| display_nested(it, x, depth)).collect::<Vec<_>>().join(", ")),
            }
        }
        V::Obj(o) => {
            let ob = o.borrow();
            if let Some(s) = crate::jsext::display(it, &ob) {
                return s;
            }
            if let ObjKind::JTime(t) = &ob.kind {
                return crate::javaext::display(t);
            }
            match &ob.kind {
                ObjKind::Error => {
                    let name = ob.props.get("name").map(|n| display(it, n, depth + 1)).unwrap_or_else(|| "Error".into());
                    let msg = ob.props.get("message").map(|n| display(it, n, depth + 1)).unwrap_or_default();
                    if d == Dialect::Python {
                        return msg;
                    }
                    if msg.is_empty() {
                        name
                    } else {
                        format!("{}: {}", name, msg)
                    }
                }
                ObjKind::Date(ms) => iso_date(*ms),
                ObjKind::Symbol(_, desc) => format!("Symbol({})", desc),
                ObjKind::Coll(c) => {
                    let name = crate::collections::coll_name(&c.0.borrow());
                    if d == Dialect::Js {
                        format!("[object {}]", name)
                    } else {
                        let items = crate::collections::iterate_coll(&c.0.borrow()).unwrap_or_default();
                        format!("[{}]", items.iter().map(|x| display_nested(it, x, depth)).collect::<Vec<_>>().join(", "))
                    }
                }
                ObjKind::Promise(_) => "[object Promise]".into(),
                ObjKind::Generator(_) => match d {
                    Dialect::Python => "<generator object>".into(),
                    _ => "[object Generator]".into(),
                },
                ObjKind::Accessor => "[Getter/Setter]".into(),
                ObjKind::Optional => match ob.props.get("value") {
                    Some(x) => format!("Optional[{}]", display(it, x, depth + 1)),
                    None => "Optional.empty".into(),
                },
                ObjKind::Set => {
                    let items: Vec<V> = ob.props.values().cloned().collect();
                    drop(ob);
                    if d == Dialect::Python {
                        if items.is_empty() {
                            return "set()".into();
                        }
                        return format!("{{{}}}", items.iter().map(|x| display_nested(it, x, depth)).collect::<Vec<_>>().join(", "));
                    }
                    format!("[{}]", items.iter().map(|x| display_nested(it, x, depth)).collect::<Vec<_>>().join(", "))
                }
                _ => {
                    if let Some(sb) = ob.props.get("%sb") {
                        return display(it, sb, depth + 1);
                    }
                    let items: Vec<(Rc<str>, V)> = ob.props.iter().map(|(k, x)| (k.clone(), x.clone())).collect();
                    drop(ob);
                    match d {
                        Dialect::Js => "[object Object]".into(),
                        Dialect::Jsonnet => jsonnet_manifest(v),
                        Dialect::Python => {
                            let inner = format!("{{{}}}", items.iter().map(|(k, x)| format!("{}: {}", py_repr(it, &V::Str(k.clone())), display_nested(it, x, depth))).collect::<Vec<_>>().join(", "));
                            it.py_dict_display(o, inner.clone()).unwrap_or(inner)
                        }
                        Dialect::Groovy => {
                            if items.is_empty() {
                                return "[:]".into();
                            }
                            format!("[{}]", items.iter().map(|(k, x)| format!("{}:{}", k, display_nested(it, x, depth))).collect::<Vec<_>>().join(", "))
                        }
                        _ => format!("{{{}}}", items.iter().map(|(k, x)| format!("{}={}", k, display_nested(it, x, depth))).collect::<Vec<_>>().join(", ")),
                    }
                }
            }
        }
        V::Func(c) => {
            let name = c.def.name.clone().or_else(|| c.props.borrow().get("name").map(|n| display(it, n, depth + 1))).unwrap_or_default();
            match d {
                Dialect::Python => format!("<function {}>", if name.is_empty() { "<lambda>" } else { &name }),
                Dialect::Groovy => "Script1$_run_closure1".into(),
                _ => format!("function {}() {{ [code] }}", name),
            }
        }
        V::Method(m) => format!("function {}() {{ [native code] }}", m.1),
        V::Host(h) => match &**h {
            Host::Headers | Host::Properties | Host::Variables => match crate::host::snapshot(it, h) {
                Some(s) => display(it, &s, depth + 1),
                None => String::new(),
            },
            Host::Message => format!("Message[{}]", it.host.message_id),
            Host::Exchange => format!("Exchange[{}]", it.host.exchange_id),
            Host::Context => it.host.context.as_ref().map(|c| c.name().to_string()).unwrap_or_else(|| "camel-1".into()),
            Host::Log => "log".into(),
            Host::Root => "RootObject".into(),
            Host::Bean(b) => format!("bean:{}", b),
            Host::Meta(t) => format!("groovy.lang.ExpandoMetaClass[{}]", t),
            Host::Ns(n) => match d {
                Dialect::Js => format!("function {}() {{ [native code] }}", n),
                Dialect::Python => format!("<class '{}'>", n),
                _ => format!("class java.lang.{}", n),
            },
            Host::NsFn(_, n) => format!("function {}() {{ [native code] }}", n),
            Host::Type(n) => {
                if d == Dialect::Python {
                    format!("<class '{}'>", n)
                } else {
                    format!("class {}", n)
                }
            }
        },
        V::Regex(r) => {
            if d == Dialect::Js {
                format!("/{}/{}", r.source, r.flags)
            } else {
                r.source.clone()
            }
        }
    }
}

// ====================================================================== dates

/// ISO-8601 (UTC) rendering of epoch millis.
pub fn iso_date(ms: f64) -> String {
    if !ms.is_finite() {
        return "Invalid Date".into();
    }
    let ms = ms as i64;
    let (y, mo, d, h, mi, s) = civil_from_epoch(ms.div_euclid(1000));
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, h, mi, s, ms.rem_euclid(1000))
}

/// Parse an ISO date (`2024-01-31`, `2024-01-31T10:00:00Z`, with offset or millis).
pub fn parse_date(s: &str) -> Option<f64> {
    let s = s.trim();
    let num = |a: usize, b: usize| -> Option<u32> { s.get(a..b)?.parse::<u32>().ok() };
    let y: i64 = s.get(0..4)?.parse().ok()?;
    if s.as_bytes().get(4) != Some(&b'-') {
        return None;
    }
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let (mut h, mut mi, mut sec, mut ms) = (0, 0, 0, 0i64);
    let mut offset_min: i64 = 0;
    if s.len() > 10 {
        let rest = &s[11..];
        let rb = rest.as_bytes();
        h = rest.get(0..2)?.parse().ok()?;
        mi = rest.get(3..5)?.parse().ok()?;
        let mut i = 5;
        if rb.get(5) == Some(&b':') {
            sec = rest.get(6..8)?.parse().ok()?;
            i = 8;
            if rb.get(8) == Some(&b'.') {
                let mut j = 9;
                while j < rb.len() && rb[j].is_ascii_digit() {
                    j += 1;
                }
                let frac = &rest[9..j];
                let f3: String = format!("{:0<3}", &frac[..frac.len().min(3)]);
                ms = f3.parse().ok()?;
                i = j;
            }
        }
        let tz = &rest[i..];
        if !tz.is_empty() && tz != "Z" {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let t = &tz[1..];
            let (th, tm) = t.split_once(':').unwrap_or((&t[..t.len().min(2)], t.get(2..).unwrap_or("0")));
            offset_min = sign * (th.parse::<i64>().ok()? * 60 + tm.parse::<i64>().unwrap_or(0));
        }
    }
    let secs = epoch_from_civil(y, mo, d, h, mi, sec) - offset_min * 60;
    Some((secs * 1000 + ms) as f64)
}

fn make_date(ms: f64) -> V {
    V::Obj(Rc::new(RefCell::new(Obj { props: PropMap::default(), proto: None, kind: ObjKind::Date(ms) })))
}

fn date_parts(ms: f64) -> (i64, u32, u32, u32, u32, u32, i64, i64) {
    let m = ms as i64;
    let (y, mo, d, h, mi, s) = civil_from_epoch(m.div_euclid(1000));
    let days = m.div_euclid(86_400_000);
    (y, mo, d, h, mi, s, m.rem_euclid(1000), (days + 4).rem_euclid(7))
}

fn date_method(it: &mut Interp, ms: f64, recv: &V, name: &str, args: &[V]) -> R<Option<V>> {
    let (y, mo, d, h, mi, s, mil, wd) = date_parts(ms);
    let set = |f: f64| {
        if let V::Obj(o) = recv {
            o.borrow_mut().kind = ObjKind::Date(f);
        }
        V::Int(f as i64)
    };
    Ok(Some(match name {
        "getTime" | "valueOf" => norm_num(ms),
        "toISOString" | "toJSON" => V::string(iso_date(ms)),
        "getFullYear" | "getUTCFullYear" | "getYear" => V::Int(y),
        "getMonth" | "getUTCMonth" => V::Int(mo as i64 - 1),
        "getDate" | "getUTCDate" => V::Int(d as i64),
        "getDay" | "getUTCDay" => V::Int(wd),
        "getHours" | "getUTCHours" => V::Int(h as i64),
        "getMinutes" | "getUTCMinutes" => V::Int(mi as i64),
        "getSeconds" | "getUTCSeconds" => V::Int(s as i64),
        "getMilliseconds" | "getUTCMilliseconds" => V::Int(mil),
        "getTimezoneOffset" => V::Int(0),
        "toLocaleString" | "toLocaleDateString" | "toLocaleTimeString" if it.d == Dialect::Js => crate::jsext::date_to_locale(it, ms, name, args)?,
        "toString" | "toUTCString" | "toDateString" | "toLocaleString" => V::string(iso_date(ms)),
        "setTime" => set(args.first().and_then(|v| v.as_f64()).unwrap_or(f64::NAN)),
        "format" => {
            let p = args.first().map(|v| it.display(v)).unwrap_or_else(|| "yyyy-MM-dd'T'HH:mm:ss".into());
            V::string(camel_core::util_time::format_millis_utc(ms as i64, &p))
        }
        "before" => V::Bool(args.first().and_then(date_ms).is_some_and(|o| ms < o)),
        "after" => V::Bool(args.first().and_then(date_ms).is_some_and(|o| ms > o)),
        "compareTo" => V::Int(args.first().and_then(date_ms).map(|o| ms.partial_cmp(&o).map(|c| c as i64).unwrap_or(0)).unwrap_or(0)),
        "plus" => make_date(ms + args.first().and_then(|v| v.as_f64()).unwrap_or(0.0) * 86_400_000.0),
        "minus" => make_date(ms - args.first().and_then(|v| v.as_f64()).unwrap_or(0.0) * 86_400_000.0),
        _ => return Ok(None),
    }))
}

fn date_ms(v: &V) -> Option<f64> {
    match v {
        V::Obj(o) => match o.borrow().kind {
            ObjKind::Date(ms) => Some(ms),
            _ => None,
        },
        other => other.as_f64(),
    }
}

// ====================================================================== helpers

fn arg(args: &[V], i: usize) -> V {
    args.get(i).cloned().unwrap_or(V::Undef)
}

fn sarg(it: &Interp, args: &[V], i: usize) -> String {
    match args.get(i) {
        Some(V::Undef) | None => String::new(),
        Some(v) => it.display(v),
    }
}

fn farg(it: &Interp, args: &[V], i: usize) -> f64 {
    match args.get(i) {
        Some(v) => match it.to_number(v) {
            V::Int(i) => i as f64,
            V::Num(f) => f,
            _ => f64::NAN,
        },
        None => f64::NAN,
    }
}

fn iarg(it: &Interp, args: &[V], i: usize, def: i64) -> i64 {
    match args.get(i) {
        Some(V::Undef) | None => def,
        Some(_) => {
            let f = farg(it, args, i);
            if f.is_nan() {
                def
            } else {
                f as i64
            }
        }
    }
}

/// Number of declared parameters of a callback (implicit `it` counts as one).
pub fn arity(f: &V) -> usize {
    match f {
        V::Func(c) => {
            if c.def.params.is_empty() {
                if c.def.implicit_it {
                    1
                } else if c.def.rest.is_some() {
                    3
                } else {
                    0
                }
            } else {
                c.def.params.len()
            }
        }
        _ => 1,
    }
}

fn kwargs(args: &mut Vec<V>) -> Option<ObjRef> {
    if let Some(V::Obj(o)) = args.last() {
        if o.borrow().kind == ObjKind::Kwargs {
            let o = o.clone();
            args.pop();
            return Some(o);
        }
    }
    None
}

fn kw(k: &Option<ObjRef>, name: &str) -> Option<V> {
    k.as_ref().and_then(|o| o.borrow().props.get(name).cloned())
}

/// Element callback arguments: JS `(x, i, arr)`, others `(x)`.
fn elem_args(it: &Interp, x: &V, i: usize, arr: &V) -> Vec<V> {
    if it.d == Dialect::Js {
        vec![x.clone(), V::Int(i as i64), arr.clone()]
    } else {
        vec![x.clone()]
    }
}

/// A map entry object `{key, value}`.
pub fn entry(k: &str, v: V) -> V {
    let mut p = PropMap::default();
    p.set("key", V::str(k));
    p.set("value", v);
    V::obj(p)
}

/// Entries of an object.
pub fn entries_of(o: &V) -> Vec<V> {
    match o {
        V::Obj(ob) => ob.borrow().props.iter().map(|(k, v)| entry(k, v.clone())).collect(),
        _ => vec![],
    }
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn char_slice(s: &str, a: usize, b: usize) -> String {
    if b <= a {
        return String::new();
    }
    s.chars().skip(a).take(b - a).collect()
}

fn byte_to_char(s: &str, b: usize) -> usize {
    s[..b].chars().count()
}

fn char_to_byte(s: &str, c: usize) -> usize {
    s.char_indices().nth(c).map(|(i, _)| i).unwrap_or(s.len())
}

/// Relative index (negative from the end), clamped to `[0, len]`.
fn rel(i: f64, len: usize) -> usize {
    if i.is_nan() {
        return 0;
    }
    if i < 0.0 {
        let r = len as f64 + i;
        if r < 0.0 {
            0
        } else {
            r as usize
        }
    } else {
        (i as usize).min(len)
    }
}

/// A set value.
pub fn make_set(it: &mut Interp, items: Vec<V>) -> V {
    let mut pm = PropMap::default();
    for x in items {
        let k = set_key(it, &x);
        if !pm.contains(&k) {
            pm.set(&k, x);
        }
    }
    V::Obj(Rc::new(RefCell::new(Obj { props: pm, proto: None, kind: ObjKind::Set })))
}

fn set_key(it: &Interp, v: &V) -> String {
    match v {
        V::Str(s) => format!("s:{}", s),
        V::Num(f) if f.fract() == 0.0 => format!("n:{}", *f as i64),
        other if other.is_number() => format!("n:{}", it.display(other)),
        other => format!("{}:{}", py_type_name(other), json::stringify(other, &JsonOpts::compact()).unwrap_or_default()),
    }
}

fn set_items(v: &V) -> Vec<V> {
    match v {
        V::Obj(o) => o.borrow().props.values().cloned().collect(),
        V::Arr(a) => a.borrow().clone(),
        _ => vec![],
    }
}

/// Python set operators `|`, `&`, `^`.
pub fn set_op(it: &mut Interp, op: BinOp, a: &V, b: &V) -> R<V> {
    let x = set_items(a);
    let y = set_items(b);
    let in_y = |it: &Interp, v: &V| y.iter().any(|w| it.equals(v, w));
    let in_x = |it: &Interp, v: &V| x.iter().any(|w| it.equals(v, w));
    let out: Vec<V> = match op {
        BinOp::BitOr => x.iter().chain(y.iter()).cloned().collect(),
        BinOp::BitAnd => x.iter().filter(|v| in_y(it, v)).cloned().collect(),
        _ => x.iter().filter(|v| !in_y(it, v)).chain(y.iter().filter(|v| !in_x(it, v))).cloned().collect(),
    };
    Ok(make_set(it, out))
}

/// Merge sort with a fallible comparator.
pub fn sort_vals(it: &mut Interp, v: Vec<V>, cmp: &mut dyn FnMut(&mut Interp, &V, &V) -> R<Ordering>) -> R<Vec<V>> {
    if v.len() <= 1 {
        return Ok(v);
    }
    let mid = v.len() / 2;
    let mut left = v;
    let right = left.split_off(mid);
    let left = sort_vals(it, left, cmp)?;
    let right = sort_vals(it, right, cmp)?;
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        if cmp(it, &right[j], &left[i])? == Ordering::Less {
            out.push(right[j].clone());
            j += 1;
        } else {
            out.push(left[i].clone());
            i += 1;
        }
    }
    out.extend_from_slice(&left[i..]);
    out.extend_from_slice(&right[j..]);
    Ok(out)
}

/// Natural ordering (numbers, strings, ...; incomparable values by type then text).
pub fn natural(it: &Interp, a: &V, b: &V) -> Ordering {
    if let Some(o) = it.compare(a, b) {
        return o;
    }
    match (a.is_nullish(), b.is_nullish()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => it.display(a).cmp(&it.display(b)),
    }
}

fn cmp_result(it: &Interp, v: &V) -> Ordering {
    let f = match it.to_number(v) {
        V::Int(i) => i as f64,
        V::Num(f) => f,
        _ => 0.0,
    };
    if let V::Bool(b) = v {
        return if *b { Ordering::Greater } else { Ordering::Less };
    }
    if f < 0.0 {
        Ordering::Less
    } else if f > 0.0 {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

/// Sort with an optional Groovy/JS comparator or key closure.
fn sort_with(it: &mut Interp, items: Vec<V>, f: Option<&V>, js_default: bool, pos: u32) -> R<Vec<V>> {
    match f {
        Some(f) if f.is_callable() => {
            let f = f.clone();
            if arity(&f) == 1 && it.d != Dialect::Js && it.d != Dialect::Java {
                // key extractor
                let mut keyed = Vec::new();
                for x in items {
                    let k = it.call(&f, V::Undef, vec![x.clone()], pos)?;
                    keyed.push(V::arr(vec![k, x]));
                }
                let sorted = sort_vals(it, keyed, &mut |it, a, b| {
                    let (ka, kb) = (first(a), first(b));
                    Ok(natural(it, &ka, &kb))
                })?;
                Ok(sorted.iter().map(|p| if let V::Arr(a) = p { a.borrow()[1].clone() } else { V::Null }).collect())
            } else {
                sort_vals(it, items, &mut |it, a, b| {
                    let r = it.call(&f, V::Undef, vec![a.clone(), b.clone()], pos)?;
                    Ok(cmp_result(it, &r))
                })
            }
        }
        _ => {
            if js_default {
                sort_vals(it, items, &mut |it, a, b| {
                    Ok(match (matches!(a, V::Undef), matches!(b, V::Undef)) {
                        (true, true) => Ordering::Equal,
                        (true, false) => Ordering::Greater,
                        (false, true) => Ordering::Less,
                        _ => it.display(a).cmp(&it.display(b)),
                    })
                })
            } else {
                sort_vals(it, items, &mut |it, a, b| it.py_order(a, b, pos))
            }
        }
    }
}

fn first(v: &V) -> V {
    match v {
        V::Arr(a) => a.borrow().first().cloned().unwrap_or(V::Null),
        _ => V::Null,
    }
}

// ====================================================================== regex

/// Create a regex value from a JS-style pattern and flags.
pub fn make_regex(it: &Interp, p: &str, flags: &str, pos: u32) -> R<V> {
    Ok(V::Regex(compile_regex(it, p, flags, pos)?))
}

fn compile_regex(it: &Interp, p: &str, flags: &str, pos: u32) -> R<Rc<RegexVal>> {
    let mut pat = String::new();
    if flags.contains('m') {
        pat.push_str("(?m)");
    }
    if flags.contains('s') {
        pat.push_str("(?s)");
    }
    pat.push_str(p);
    match camel_core::regex::Regex::with_flags(&pat, flags.contains('i')) {
        Ok(re) => Ok(Rc::new(RegexVal { source: p.to_string(), flags: flags.to_string(), re })),
        Err(e) => {
            let name = match it.d {
                Dialect::Python => "re.error",
                Dialect::Js => "SyntaxError",
                _ => "PatternSyntaxException",
            };
            it.throw(name, format!("Invalid regular expression: /{}/: {}", p, e.message), if pos > 0 { Some(pos) } else { None })
        }
    }
}

/// A regex from a regex value or a pattern string.
pub fn to_regex(it: &mut Interp, v: &V, pos: u32) -> R<Rc<RegexVal>> {
    match v {
        V::Regex(r) => Ok(r.clone()),
        other => {
            let s = it.display(other);
            compile_regex(it, &s, "", pos)
        }
    }
}

/// Groovy `=~`: the list of matches (strings, or `[match, group1, ...]` when the pattern has groups).
pub fn groovy_find(it: &mut Interp, a: &V, b: &V, pos: u32) -> R<V> {
    let s = it.display(a);
    let re = to_regex(it, b, pos)?;
    Ok(V::arr(match_list(&re, &s)))
}

fn match_list(re: &RegexVal, s: &str) -> Vec<V> {
    let groups = re.re.group_count();
    re.re
        .captures_iter(s)
        .iter()
        .map(|c| {
            if groups == 0 {
                V::string(c.get(0).unwrap_or("").to_string())
            } else {
                V::arr(c.to_vec().into_iter().map(|g| g.map(V::string).unwrap_or(V::Null)).collect())
            }
        })
        .collect()
}

fn js_replacement(r: &str) -> String {
    // `$&` whole match, `$<name>` named group, `$$` a dollar sign, `\` is literal
    let mut out = String::new();
    let cs: Vec<char> = r.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '\\' {
            out.push_str("\\\\");
        } else if c == '$' && i + 1 < cs.len() {
            match cs[i + 1] {
                '&' => {
                    out.push_str("$0");
                    i += 1;
                }
                '$' => {
                    out.push_str("\\$");
                    i += 1;
                }
                '<' => {
                    if let Some(end) = cs[i + 2..].iter().position(|x| *x == '>') {
                        let name: String = cs[i + 2..i + 2 + end].iter().collect();
                        out.push_str(&format!("${{{}}}", name));
                        i += end + 2;
                    } else {
                        out.push('$');
                    }
                }
                _ => out.push('$'),
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

fn regex_replace_fn(it: &mut Interp, re: &RegexVal, s: &str, f: &V, global: bool, pos: u32) -> R<String> {
    let mut out = String::new();
    let mut last = 0;
    for c in re.re.captures_iter(s) {
        let (st, en) = c.span(0).unwrap_or((0, 0));
        out.push_str(&s[last..st]);
        let mut args: Vec<V> = c.to_vec().into_iter().map(|g| g.map(V::string).unwrap_or(V::Undef)).collect();
        if it.d == Dialect::Js {
            args.push(V::Int(byte_to_char(s, st) as i64));
            args.push(V::str(s));
            let groups = crate::jsext::named_groups(&re.re, &c);
            if !matches!(groups, V::Undef) {
                args.push(groups);
            }
        } else if it.d == Dialect::Python {
            args = vec![match_obj(&c, s)];
        }
        let r = it.call(f, V::Undef, args, pos)?;
        out.push_str(&it.display(&r));
        last = en;
        if !global {
            break;
        }
    }
    out.push_str(&s[last..]);
    Ok(out)
}

fn match_obj(c: &camel_core::regex::Captures, s: &str) -> V {
    let mut p = PropMap::default();
    p.set("%match", V::arr(c.to_vec().into_iter().map(|g| g.map(V::string).unwrap_or(V::Null)).collect()));
    let (st, en) = c.span(0).unwrap_or((0, 0));
    p.set("%start", V::Int(byte_to_char(s, st) as i64));
    p.set("%end", V::Int(byte_to_char(s, en) as i64));
    V::obj(p)
}

fn regex_method(it: &mut Interp, r: &Rc<RegexVal>, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
    let s = sarg(it, args, 0);
    Ok(Some(match name {
        "test" => V::Bool(r.re.is_match(&s)),
        "exec" if it.d == Dialect::Js && r.flags.contains('g') => {
            // global regexes advance `lastIndex` (kept per regex object)
            let key = format!("%lastIndex:{:p}", Rc::as_ptr(r));
            let start = it.globals.lookup(&key).and_then(|v| v.as_f64()).unwrap_or(0.0) as usize;
            let bstart = char_to_byte(&s, start);
            let found = if bstart <= s.len() { r.re.captures_iter(&s[bstart..]).into_iter().next() } else { None };
            match found {
                Some(_) => {
                    let all = r.re.captures_iter(&s);
                    let c = all.into_iter().find(|c| c.span(0).is_some_and(|(st, _)| st >= bstart)).unwrap();
                    let (_, en) = c.span(0).unwrap_or((0, 0));
                    let en_c = byte_to_char(&s, en) as i64;
                    let next = if c.span(0).is_some_and(|(a, b)| a == b) { en_c + 1 } else { en_c };
                    it.globals.define(&key, V::Int(next));
                    crate::jsext::js_match(it, &r.re, &c, &s)
                }
                None => {
                    it.globals.define(&key, V::Int(0));
                    V::Null
                }
            }
        }
        "exec" => match r.re.captures_match(&s) {
            Some(c) if it.d == Dialect::Js => crate::jsext::js_match(it, &r.re, &c, &s),
            Some(c) => V::arr(c.to_vec().into_iter().map(|g| g.map(V::string).unwrap_or(V::Undef)).collect()),
            None => V::Null,
        },
        "matcher" => V::arr(match_list(r, &s)),
        "matches" | "fullmatch" if it.d != Dialect::Python => V::Bool(r.re.is_full_match(&s)),
        "split" if it.d != Dialect::Python => V::arr(r.re.split(&s).into_iter().map(V::string).collect()),
        "pattern" | "toString" | "source" => V::string(if name == "toString" && it.d == Dialect::Js { format!("/{}/{}", r.source, r.flags) } else { r.source.clone() }),
        "match" | "search" | "fullmatch" | "findall" | "sub" | "split" | "finditer" => {
            let mut a = vec![V::Regex(r.clone())];
            a.extend(args.iter().cloned());
            return call_ns_fn(it, "re", name, a, pos).map(Some);
        }
        _ => return Ok(None),
    }))
}

// ====================================================================== ranges & slices

/// Materialize a range (`1..5`, `1..<5`, `'a'..'e'`, reversed ranges).
pub fn make_range(it: &mut Interp, f: &V, t: &V, exclusive: bool) -> R<V> {
    if let (V::Str(a), V::Str(b)) = (f, t) {
        if let (Some(x), Some(y)) = (a.chars().next(), b.chars().next()) {
            let (x, y) = (x as u32, y as u32);
            let v: Vec<V> = if x <= y {
                (x..if exclusive { y } else { y + 1 }).filter_map(char::from_u32).map(|c| V::string(c.to_string())).collect()
            } else {
                ((if exclusive { y + 1 } else { y })..=x).rev().filter_map(char::from_u32).map(|c| V::string(c.to_string())).collect()
            };
            return Ok(V::arr(v));
        }
    }
    let a = farg(it, &[f.clone()], 0);
    let b = farg(it, &[t.clone()], 0);
    if a.is_nan() || b.is_nan() {
        return it.type_error(format!("invalid range bounds {}..{}", it.display(f), it.display(t)), None);
    }
    let (a, b) = (a as i64, b as i64);
    if (a - b).unsigned_abs() > 10_000_000 {
        return it.throw("Error", "range too large".into(), None);
    }
    let v: Vec<V> = if a <= b {
        (a..if exclusive { b } else { b + 1 }).map(V::Int).collect()
    } else {
        ((if exclusive { b + 1 } else { b })..=a).rev().map(V::Int).collect()
    };
    Ok(V::arr(v))
}

/// Groovy `list[a..b]` / `str[a..b]` (negative indices from the end, reversed when a > b).
pub fn range_slice(it: &mut Interp, o: &V, f: &V, t: &V, exclusive: bool, pos: u32) -> R<V> {
    let len = match o {
        V::Str(s) => char_len(s),
        V::Arr(a) => a.borrow().len(),
        V::Null | V::Undef => return Ok(V::Null),
        other => return it.type_error(format!("cannot slice {}", it.display(other)), Some(pos)),
    } as i64;
    let norm = |i: i64| if i < 0 { i + len } else { i };
    let mut a = norm(iarg(it, &[f.clone()], 0, 0));
    let mut b = norm(iarg(it, &[t.clone()], 0, 0));
    let rev = a > b;
    if exclusive {
        if rev {
            b += 1;
        } else {
            b -= 1;
        }
    }
    if rev {
        std::mem::swap(&mut a, &mut b);
    }
    if len == 0 || b < a {
        return Ok(match o {
            V::Str(_) => V::str(""),
            _ => V::arr(vec![]),
        });
    }
    let a = a.clamp(0, len - 1) as usize;
    let b = b.clamp(0, len - 1) as usize;
    Ok(match o {
        V::Str(s) => {
            let mut sub = char_slice(s, a, b + 1);
            if rev {
                sub = sub.chars().rev().collect();
            }
            V::string(sub)
        }
        V::Arr(arr) => {
            let mut items: Vec<V> = arr.borrow()[a..=b].to_vec();
            if rev {
                items.reverse();
            }
            V::arr(items)
        }
        _ => V::Null,
    })
}

/// Python / Jsonnet slicing `a[start:end:step]`.
pub fn py_slice(it: &mut Interp, o: &V, s: Option<V>, e: Option<V>, st: Option<V>, pos: u32) -> R<V> {
    let step = match &st {
        Some(v) if !v.is_nullish() => iarg(it, std::slice::from_ref(v), 0, 1),
        _ => 1,
    };
    if step == 0 {
        return it.throw("ValueError", "slice step cannot be zero".into(), Some(pos));
    }
    let (chars, items): (Option<Vec<char>>, Option<Vec<V>>) = match o {
        V::Str(x) => (Some(x.chars().collect()), None),
        V::Arr(a) => (None, Some(a.borrow().clone())),
        other => return it.type_error(format!("'{}' object is not subscriptable", py_type_name(other)), Some(pos)),
    };
    let len = chars.as_ref().map(|c| c.len()).or(items.as_ref().map(|i| i.len())).unwrap_or(0) as i64;
    let idx = |v: &Option<V>, def: i64, it: &Interp| -> i64 {
        match v {
            Some(x) if !x.is_nullish() => {
                let i = iarg(it, std::slice::from_ref(x), 0, def);
                let i = if i < 0 { i + len } else { i };
                if step > 0 {
                    i.clamp(0, len)
                } else {
                    i.clamp(-1, len - 1)
                }
            }
            _ => def,
        }
    };
    let (start, end) = if step > 0 { (idx(&s, 0, it), idx(&e, len, it)) } else { (idx(&s, len - 1, it), idx(&e, -1, it)) };
    let mut sel = Vec::new();
    let mut i = start;
    while (step > 0 && i < end) || (step < 0 && i > end) {
        sel.push(i as usize);
        i += step;
    }
    Ok(match (chars, items) {
        (Some(c), _) => V::string(sel.iter().map(|i| c[*i]).collect()),
        (_, Some(v)) => V::arr(sel.iter().map(|i| v[*i].clone()).collect()),
        _ => V::Null,
    })
}

// ====================================================================== types

/// Whether a value is an instance of a (Java / Python) type name.
pub fn is_type(v: &V, name: &str) -> bool {
    match name {
        "String" | "CharSequence" | "GString" | "str" => matches!(v, V::Str(_)),
        "Integer" | "Long" | "Short" | "Byte" | "BigInteger" | "int" | "long" | "short" | "byte" => matches!(v, V::Int(_)),
        "Double" | "Float" | "BigDecimal" | "double" | "float" => matches!(v, V::Num(_)),
        "Number" => v.is_number(),
        "Boolean" | "boolean" | "bool" => matches!(v, V::Bool(_)),
        "List" | "ArrayList" | "LinkedList" | "Object[]" | "list" | "tuple" | "Array" => matches!(v, V::Arr(_)),
        "Collection" | "Iterable" => matches!(v, V::Arr(_)) || matches!(v, V::Obj(o) if o.borrow().kind == ObjKind::Set),
        "Map" | "HashMap" | "LinkedHashMap" | "TreeMap" | "dict" => matches!(v, V::Obj(o) if matches!(o.borrow().kind, ObjKind::Plain | ObjKind::Kwargs)),
        "Set" | "HashSet" | "LinkedHashSet" | "TreeSet" | "set" => matches!(v, V::Obj(o) if o.borrow().kind == ObjKind::Set),
        "Closure" | "Function" => matches!(v, V::Func(_) | V::Method(_)),
        "Date" => matches!(v, V::Obj(o) if matches!(o.borrow().kind, ObjKind::Date(_))),
        "Pattern" | "RegExp" => matches!(v, V::Regex(_)),
        "Object" | "object" => !v.is_nullish(),
        "byte[]" | "bytes" => matches!(v, V::Bytes(_)),
        n => match v {
            V::Obj(o) if o.borrow().kind == ObjKind::Error => {
                let en = o.borrow().props.get("name").and_then(|x| x.as_str().map(|s| s.to_string())).unwrap_or_default();
                error_is_a(&en, n)
            }
            _ => false,
        },
    }
}

/// `instanceof`.
pub fn instance_of(it: &mut Interp, a: &V, b: &V) -> bool {
    match b {
        V::Host(h) => match &**h {
            Host::Ns(_) | Host::Type(_) if it.d == Dialect::Js => {
                let n: &str = match &**h {
                    Host::Ns(n) => n,
                    Host::Type(t) => t,
                    _ => "",
                };
                let kind_name = match a {
                    V::Obj(o) => match &o.borrow().kind {
                        ObjKind::Coll(c) => Some(crate::collections::coll_name(&c.0.borrow())),
                        ObjKind::Promise(_) => Some("Promise"),
                        ObjKind::Symbol(..) => Some("Symbol"),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(k) = kind_name {
                    return k == n || n == "Object";
                }
                match n {
                    "Array" => matches!(a, V::Arr(_)),
                    "Object" => matches!(a, V::Obj(_) | V::Arr(_) | V::Func(_) | V::Regex(_)),
                    "Function" => matches!(a, V::Func(_) | V::Method(_)),
                    "Date" => is_type(a, "Date"),
                    "RegExp" => matches!(a, V::Regex(_)),
                    "String" | "Number" | "Boolean" => false,
                    other => is_type(a, other),
                }
            }
            Host::Ns(n) => is_type(a, n),
            Host::Type(n) => is_type(a, n),
            _ => false,
        },
        V::Func(c) => {
            let proto = match it.func_prototype(c) {
                Some(V::Obj(p)) => p,
                _ => return false,
            };
            let mut cur = match a {
                V::Obj(o) => o.borrow().proto.clone(),
                _ => None,
            };
            while let Some(p) = cur {
                if Rc::ptr_eq(&p, &proto) {
                    return true;
                }
                cur = p.borrow().proto.clone();
            }
            false
        }
        _ => false,
    }
}

fn parse_int_str(s: &str, radix: u32) -> Option<i64> {
    let t = s.trim();
    let (neg, t) = if let Some(r) = t.strip_prefix('-') { (true, r) } else { (false, t.strip_prefix('+').unwrap_or(t)) };
    let v = i64::from_str_radix(t, radix).ok()?;
    Some(if neg { -v } else { v })
}

/// Groovy `as Type` / Camel type conversion (`getBody(Integer)`).
pub fn as_type(it: &mut Interp, v: V, t: &str, pos: u32) -> R<V> {
    let t = t.rsplit('.').next().unwrap_or(t);
    if v.is_nullish() {
        return Ok(if t == "boolean" || t == "Boolean" { V::Bool(false) } else { V::Null });
    }
    let nfe = |it: &Interp, s: &str| -> R<V> { it.throw("NumberFormatException", format!("For input string: \"{}\"", s), Some(pos)) };
    Ok(match t {
        "String" | "GString" | "CharSequence" | "str" => match &v {
            V::Bytes(b) => V::string(String::from_utf8_lossy(b).into_owned()),
            _ => V::string(it.display(&v)),
        },
        "Integer" | "int" | "Long" | "long" | "Short" | "short" | "BigInteger" | "Byte" | "byte" => match &v {
            V::Int(_) => v,
            V::Num(f) => V::Int(f.trunc() as i64),
            V::Bool(b) => V::Int(*b as i64),
            V::Str(s) => match parse_int_str(s, 10) {
                Some(i) => V::Int(i),
                None => return nfe(it, s),
            },
            V::Bytes(_) => {
                let s = it.display(&v);
                match parse_int_str(&s, 10) {
                    Some(i) => V::Int(i),
                    None => return nfe(it, &s),
                }
            }
            other => return it.type_error(format!("Cannot cast object '{}' to class 'java.lang.Integer'", it.display(other)), Some(pos)),
        },
        "Double" | "double" | "Float" | "float" | "BigDecimal" | "Number" => match &v {
            V::Int(i) if t != "Number" => V::Num(*i as f64),
            V::Int(_) | V::Num(_) => v,
            V::Str(_) | V::Bytes(_) => {
                let s = it.display(&v);
                match s.trim().parse::<f64>() {
                    Ok(f) => V::Num(f),
                    Err(_) => return nfe(it, &s),
                }
            }
            V::Bool(b) => V::Num(*b as i64 as f64),
            other => return it.type_error(format!("Cannot cast object '{}' to class 'java.lang.Double'", it.display(other)), Some(pos)),
        },
        "Boolean" | "boolean" | "bool" => match &v {
            V::Str(s) if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") => V::Bool(s.eq_ignore_ascii_case("true")),
            other => V::Bool(it.truthy(other)),
        },
        "List" | "ArrayList" | "LinkedList" | "Collection" | "Iterable" | "Object[]" | "String[]" | "list" | "tuple" => match &v {
            V::Arr(a) => V::arr(a.borrow().clone()),
            V::Str(s) if s.trim_start().starts_with('[') => json::parse(s).unwrap_or_else(|_| V::arr(vec![v.clone()])),
            _ => V::arr(it.iterate(&v, Some(pos))?),
        },
        "Set" | "HashSet" | "LinkedHashSet" | "TreeSet" | "set" => {
            let mut items = it.iterate(&v, Some(pos))?;
            if t == "TreeSet" {
                items = sort_with(it, items, None, false, pos)?;
            }
            make_set(it, items)
        }
        "Map" | "HashMap" | "LinkedHashMap" | "TreeMap" | "dict" | "JsonObject" => match &v {
            V::Obj(_) => v,
            V::Str(_) | V::Bytes(_) => {
                let s = it.display(&v);
                match json::parse(&s) {
                    Ok(o @ V::Obj(_)) => o,
                    _ => return it.throw("NoTypeConversionAvailableException", format!("No type converter available to convert from type: java.lang.String to the required type: java.util.Map with value {}", s), Some(pos)),
                }
            }
            other => return it.type_error(format!("Cannot cast object '{}' to class 'java.util.Map'", it.display(other)), Some(pos)),
        },
        "byte[]" | "bytes" => match &v {
            V::Bytes(_) => v,
            other => V::Bytes(Rc::new(it.display(other).into_bytes())),
        },
        "Date" => match &v {
            V::Str(s) => make_date(parse_date(s).unwrap_or(f64::NAN)),
            V::Int(i) => make_date(*i as f64),
            _ => v,
        },
        _ => v,
    })
}

// ====================================================================== namespaces

/// Members of namespaces: constants or functions.
pub fn ns_member(it: &mut Interp, ns: &'static str, name: &str, _pos: u32) -> R<V> {
    if it.d.groovy_like() && crate::javaext::NS.contains(&ns) {
        if let Some(v) = it.jtime_member(ns, name) {
            return Ok(v);
        }
    }
    if it.d == Dialect::Python {
        if let Some(v) = it.py_ns_member(ns, name) {
            return Ok(v);
        }
    }
    if it.d == Dialect::Js {
        if let Some(v) = crate::jsext::ns_member(it, ns, name)? {
            return Ok(v);
        }
    }
    let c = match (ns, name) {
        (_, "class") => return Ok(V::host(Host::Ns(ns))),
        ("Math" | "math", "PI" | "pi") => V::Num(std::f64::consts::PI),
        ("Math" | "math", "E" | "e") => V::Num(std::f64::consts::E),
        ("math", "inf") => V::Num(f64::INFINITY),
        ("math", "nan") => V::Num(f64::NAN),
        ("Number", "MAX_SAFE_INTEGER") => V::Int(9_007_199_254_740_991),
        ("Number", "MIN_SAFE_INTEGER") => V::Int(-9_007_199_254_740_991),
        ("Number", "EPSILON") => V::Num(f64::EPSILON),
        ("Number" | "Double", "MAX_VALUE") => V::Num(f64::MAX),
        ("Number", "MIN_VALUE") => V::Num(5e-324),
        ("Double", "MIN_VALUE") => V::Num(f64::MIN_POSITIVE),
        ("Number" | "Double", "POSITIVE_INFINITY") => V::Num(f64::INFINITY),
        ("Number" | "Double", "NEGATIVE_INFINITY") => V::Num(f64::NEG_INFINITY),
        ("Number" | "Double", "NaN") => V::Num(f64::NAN),
        ("Integer", "MAX_VALUE") => V::Int(i32::MAX as i64),
        ("Integer", "MIN_VALUE") => V::Int(i32::MIN as i64),
        ("Long", "MAX_VALUE") => V::Int(i64::MAX),
        ("Long", "MIN_VALUE") => V::Int(i64::MIN),
        ("Boolean", "TRUE") => V::Bool(true),
        ("Boolean", "FALSE") => V::Bool(false),
        ("BigDecimal", "ZERO") => V::Num(0.0),
        ("BigDecimal", "ONE") => V::Num(1.0),
        ("BigDecimal", "TEN") => V::Num(10.0),
        ("Collections", "EMPTY_LIST") => V::arr(vec![]),
        ("Collections", "EMPTY_MAP") => V::new_obj(),
        (_, "name" | "simpleName" | "__name__") if it.d != Dialect::Js || name == "name" => V::str(ns),
        ("std" | "cml" | "ds", _) => return crate::jsonnet::std_member(it, ns, name),
        ("Symbol", "iterator" | "asyncIterator" | "hasInstance" | "toPrimitive" | "toStringTag" | "species" | "isConcatSpreadable" | "match" | "unscopables") => {
            return Ok(it.symbol_for(&format!("@@{}", name)));
        }
        (_, "metaClass") if it.d == Dialect::Groovy => return Ok(V::host(Host::Meta(ns.to_string()))),
        ("System", "out" | "err") => return Ok(V::host(Host::Ns("SystemOut"))),
        _ => V::host(Host::NsFn(ns, name.to_string())),
    };
    Ok(c)
}

fn math1(it: &Interp, args: &[V], f: fn(f64) -> f64) -> V {
    it.mk_num(f(farg(it, args, 0)))
}

fn json_parse(it: &mut Interp, s: &str, pos: u32) -> R<V> {
    match json::parse(s) {
        Ok(v) => Ok(v),
        Err(m) => {
            let name = match it.d {
                Dialect::Js => "SyntaxError",
                Dialect::Python => "JSONDecodeError",
                Dialect::Groovy => "JsonException",
                _ => "JsonParseException",
            };
            it.throw(name, m, Some(pos))
        }
    }
}

fn uri_encode(s: &str, keep: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || "-_.!~*'()".contains(c) || keep.contains(c) {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn uri_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn js_parse_int(s: &str, radix: i64) -> V {
    let t = s.trim_start();
    let (neg, mut t) = if let Some(r) = t.strip_prefix('-') { (true, r) } else { (false, t.strip_prefix('+').unwrap_or(t)) };
    let mut radix = if radix == 0 { 10 } else { radix as u32 };
    if (radix == 16 || radix == 10) && (t.starts_with("0x") || t.starts_with("0X")) {
        t = &t[2..];
        radix = 16;
    }
    if !(2..=36).contains(&radix) {
        return V::Num(f64::NAN);
    }
    let digits: String = t.chars().take_while(|c| c.is_digit(radix)).collect();
    if digits.is_empty() {
        return V::Num(f64::NAN);
    }
    match i64::from_str_radix(&digits, radix) {
        Ok(v) => V::Int(if neg { -v } else { v }),
        Err(_) => V::Num(f64::NAN),
    }
}

fn js_parse_float(s: &str) -> V {
    let t = s.trim_start();
    let mut end = 0;
    let b = t.as_bytes();
    let mut seen_dot = false;
    let mut seen_e = false;
    while end < b.len() {
        let c = b[end];
        if c.is_ascii_digit() || (end == 0 && (c == b'-' || c == b'+')) {
        } else if c == b'.' && !seen_dot && !seen_e {
            seen_dot = true;
        } else if (c == b'e' || c == b'E') && !seen_e && end > 0 {
            seen_e = true;
            if end + 1 < b.len() && (b[end + 1] == b'-' || b[end + 1] == b'+') {
                end += 1;
            }
        } else {
            break;
        }
        end += 1;
    }
    if t.starts_with("Infinity") {
        return V::Num(f64::INFINITY);
    }
    let mut cand = &t[..end];
    while !cand.is_empty() {
        if let Ok(f) = cand.parse::<f64>() {
            return norm_num(f);
        }
        cand = &cand[..cand.len() - 1];
    }
    V::Num(f64::NAN)
}

fn py_int(it: &mut Interp, v: &V, base: Option<i64>, pos: u32) -> R<V> {
    Ok(match v {
        V::Int(_) => v.clone(),
        V::Num(f) => V::Int(f.trunc() as i64),
        V::Bool(b) => V::Int(*b as i64),
        V::Str(s) => {
            let t: String = s.trim().chars().filter(|c| *c != '_').collect();
            match parse_int_str(&t, base.unwrap_or(10) as u32) {
                Some(i) => V::Int(i),
                None => return it.throw("ValueError", format!("invalid literal for int() with base {}: {}", base.unwrap_or(10), py_repr(it, v)), Some(pos)),
            }
        }
        other => return it.throw("TypeError", format!("int() argument must be a string or a number, not '{}'", py_type_name(other)), Some(pos)),
    })
}

fn py_float(it: &mut Interp, v: &V, pos: u32) -> R<V> {
    Ok(match v {
        V::Int(i) => V::Num(*i as f64),
        V::Num(_) => v.clone(),
        V::Bool(b) => V::Num(*b as i64 as f64),
        V::Str(s) => match s.trim().to_ascii_lowercase().as_str() {
            "inf" | "infinity" | "+inf" => V::Num(f64::INFINITY),
            "-inf" | "-infinity" => V::Num(f64::NEG_INFINITY),
            "nan" => V::Num(f64::NAN),
            t => match t.parse::<f64>() {
                Ok(f) => V::Num(f),
                Err(_) => return it.throw("ValueError", format!("could not convert string to float: {}", py_repr(it, v)), Some(pos)),
            },
        },
        other => return it.throw("TypeError", format!("float() argument must be a string or a number, not '{}'", py_type_name(other)), Some(pos)),
    })
}

fn len_of(it: &mut Interp, v: &V, pos: u32) -> R<i64> {
    if let Some(r) = it.py_special(v, "__len__", vec![], pos)? {
        return Ok(r.as_f64().unwrap_or(0.0) as i64);
    }
    if let V::Obj(o) = v {
        if let ObjKind::Coll(c) = &o.borrow().kind {
            if let crate::collections::Coll::Map(m) | crate::collections::Coll::Set(m) = &*c.0.borrow() {
                return Ok(m.len() as i64);
            }
        }
    }
    Ok(match v {
        V::Str(s) => char_len(s) as i64,
        V::Bytes(b) => b.len() as i64,
        V::Arr(a) => a.borrow().len() as i64,
        V::Obj(o) => o.borrow().props.len() as i64,
        V::Host(h) => match crate::host::snapshot(it, h) {
            Some(V::Obj(o)) => o.borrow().props.len() as i64,
            _ => 0,
        },
        other => return it.throw("TypeError", format!("object of type '{}' has no len()", py_type_name(other)), Some(pos)),
    })
}

fn min_max(it: &mut Interp, mut args: Vec<V>, want_max: bool, pos: u32) -> R<V> {
    let kwa = kwargs(&mut args);
    let items = if args.len() == 1 { it.iterate(&args[0], Some(pos))? } else { args };
    if items.is_empty() {
        if let Some(d) = kw(&kwa, "default") {
            return Ok(d);
        }
        return it.throw("ValueError", format!("{}() arg is an empty sequence", if want_max { "max" } else { "min" }), Some(pos));
    }
    let key = kw(&kwa, "key");
    let mut best = items[0].clone();
    let mut best_k = match &key {
        Some(k) => it.call(k, V::Undef, vec![best.clone()], pos)?,
        None => best.clone(),
    };
    for x in items.into_iter().skip(1) {
        let k = match &key {
            Some(f) => it.call(f, V::Undef, vec![x.clone()], pos)?,
            None => x.clone(),
        };
        let o = natural(it, &k, &best_k);
        if (want_max && o == Ordering::Greater) || (!want_max && o == Ordering::Less) {
            best = x;
            best_k = k;
        }
    }
    Ok(best)
}

fn sum_values(it: &mut Interp, items: Vec<V>, start: V, pos: u32) -> R<V> {
    let mut acc = start;
    for x in items {
        acc = if acc.is_nullish() { x } else { it.binary(BinOp::Add, acc, x, pos)? };
    }
    Ok(acc)
}

fn log_line(it: &Interp, args: &[V]) {
    let msg = args.iter().map(|a| it.display(a)).collect::<Vec<_>>().join(" ");
    camel_core::log::log(camel_core::log::Level::Info, "camel.script", msg);
}

/// Call a namespace / global function value (`String(x)`, `len(x)`, `parseInt(s)`).
pub fn call_ns(it: &mut Interp, n: &'static str, mut args: Vec<V>, pos: u32) -> R<V> {
    let a0 = arg(&args, 0);
    if it.d == Dialect::Js {
        if let Some(v) = crate::jsext::call_ns(it, n, &args, pos)? {
            return Ok(v);
        }
        if let Some(b) = crate::jsext::bigint_of(&a0) {
            match n {
                "Number" => return Ok(norm_num(b.to_f64())),
                "String" => return Ok(V::string(b.to_string())),
                "parseInt" | "parseFloat" => return Ok(norm_num(b.to_f64())),
                _ => {}
            }
        }
    }
    Ok(match n {
        "String" => {
            if args.is_empty() {
                V::str("")
            } else {
                V::string(it.display(&a0))
            }
        }
        "Number" => {
            if args.is_empty() {
                V::Int(0)
            } else {
                it.to_number(&a0)
            }
        }
        "Boolean" => V::Bool(it.truthy(&a0)),
        "Array" => {
            if args.len() == 1 && a0.is_number() {
                V::arr(vec![V::Undef; farg(it, &args, 0) as usize])
            } else {
                V::arr(args)
            }
        }
        "Object" => {
            if matches!(a0, V::Obj(_)) {
                a0
            } else {
                V::new_obj()
            }
        }
        "Date" => V::string(iso_date(now_millis() as f64)),
        "Error" | "TypeError" | "RangeError" | "SyntaxError" | "ReferenceError" => {
            let msg = sarg(it, &args, 0);
            it.make_error(n, msg, None)
        }
        "RegExp" => {
            if let V::Regex(_) = a0 {
                return Ok(a0);
            }
            let p = sarg(it, &args, 0);
            let f = sarg(it, &args, 1);
            make_regex(it, &p, &f, pos)?
        }
        "parseInt" => {
            let s = sarg(it, &args, 0);
            js_parse_int(&s, iarg(it, &args, 1, 0))
        }
        "parseFloat" => js_parse_float(&sarg(it, &args, 0)),
        "isNaN" => V::Bool(farg(it, &args, 0).is_nan()),
        "isFinite" => V::Bool(farg(it, &args, 0).is_finite()),
        "encodeURIComponent" => V::string(uri_encode(&sarg(it, &args, 0), "")),
        "encodeURI" => V::string(uri_encode(&sarg(it, &args, 0), ";,/?:@&=+$#")),
        "decodeURIComponent" | "decodeURI" => V::string(uri_decode(&sarg(it, &args, 0))),
        "print" | "println" => {
            kwargs(&mut args);
            let mut strs = Vec::with_capacity(args.len());
            for a in &args {
                strs.push(V::string(it.to_str(a)?));
            }
            log_line(it, &strs);
            it.undef()
        }
        "Symbol" => {
            let desc = if a0.is_nullish() { String::new() } else { it.display(&a0) };
            it.new_symbol(&desc)
        }
        "Promise" | "Map" | "Set" | "WeakMap" | "WeakSet" if it.d == Dialect::Js => return it.type_error(format!("Constructor {} requires 'new'", n), Some(pos)),
        "setTimeout" | "setInterval" => {
            let delay = if args.len() > 1 { farg(it, &args, 1) } else { 0.0 };
            let rest: Vec<V> = args.iter().skip(2).cloned().collect();
            V::Int(it.add_timer(a0, delay, rest, n == "setInterval"))
        }
        "clearTimeout" | "clearInterval" => {
            it.clear_timer(farg(it, &args, 0) as i64);
            V::Undef
        }
        "queueMicrotask" => {
            it.queue_microtask(a0, vec![]);
            V::Undef
        }
        "next" => {
            let it_v = a0.clone();
            match &it_v {
                V::Obj(o) if matches!(o.borrow().kind, ObjKind::Generator(_) | ObjKind::Coll(_)) => match it.iter_next(&it_v)? {
                    Some(v) => v,
                    None => match args.get(1) {
                        Some(d) => d.clone(),
                        None => return it.throw("StopIteration", String::new(), Some(pos)),
                    },
                },
                _ => match it.call_method(&it_v, "__next__", vec![], pos) {
                    Err(crate::interp::Ctrl::Throw(e)) if error_name(&e) == "StopIteration" && args.len() > 1 => args[1].clone(),
                    other => other?,
                },
            }
        }
        "iter" => match it.lazy_iterator(&a0)? {
            Some(i) => i,
            None => {
                let items = it.iterate(&a0, Some(pos))?;
                crate::collections::list_iterator(items)
            }
        },
        "callable" => V::Bool(a0.is_callable() || matches!(&a0, V::Func(_))),
        "id" => V::Int(match &a0 {
            V::Obj(o) => Rc::as_ptr(o) as usize as i64,
            V::Arr(a) => Rc::as_ptr(a) as usize as i64,
            V::Func(f) => Rc::as_ptr(f) as usize as i64,
            _ => 0,
        }),
        "vars" => match &a0 {
            V::Obj(o) => V::obj(o.borrow().props.clone()),
            _ => V::new_obj(),
        },
        "setattr" => {
            let k = sarg(it, &args, 1);
            it.set_member(&a0, &k, arg(&args, 2), pos)?;
            V::Null
        }
        "issubclass" => {
            let b = arg(&args, 1);
            match (&a0, &b) {
                (V::Func(x), V::Func(y)) => {
                    let mut cur = Some(V::Func(x.clone()));
                    let mut r = false;
                    while let Some(V::Func(c)) = cur {
                        if Rc::ptr_eq(&c, y) {
                            r = true;
                            break;
                        }
                        cur = c.parent.clone();
                    }
                    V::Bool(r)
                }
                _ => V::Bool(false),
            }
        }
        "sprintf" => {
            let f = sarg(it, &args, 0);
            let rest: Vec<V> = args.iter().skip(1).cloned().collect();
            match crate::format::printf(it, &f, &rest, None) {
                Ok(s) => V::string(s),
                Err(m) => return it.throw("IllegalFormatException", m, Some(pos)),
            }
        }
        "__pattern" => V::Regex(to_regex(it, &a0, pos)?),
        "__isEmpty" => V::Bool(match &a0 {
            V::Undef | V::Null => true,
            V::Str(s) => s.trim().is_empty(),
            V::Arr(a) => a.borrow().is_empty(),
            V::Obj(o) => o.borrow().props.is_empty(),
            _ => false,
        }),
        "__kwargs" => {
            if let V::Obj(o) = &a0 {
                o.borrow_mut().kind = ObjKind::Kwargs;
            }
            a0
        }
        "__bean" => {
            let name = sarg(it, &args, 0);
            return crate::host::bean_ref(it, &name, pos);
        }
        // ---------------- Python builtins
        "len" => V::Int(len_of(it, &a0, pos)?),
        "str" => {
            if args.is_empty() {
                V::str("")
            } else if crate::classes::class_of(&a0).is_some() {
                V::string(it.to_str(&a0)?)
            } else if let V::Bytes(b) = &a0 {
                V::string(String::from_utf8_lossy(b).into_owned())
            } else {
                V::string(it.display(&a0))
            }
        }
        "int" => {
            let kwa = kwargs(&mut args);
            if args.is_empty() {
                V::Int(0)
            } else {
                let base = if args.len() > 1 { Some(iarg(it, &args, 1, 10)) } else { kw(&kwa, "base").map(|b| b.as_f64().unwrap_or(10.0) as i64) };
                py_int(it, &a0, base, pos)?
            }
        }
        "float" => {
            if args.is_empty() {
                V::Num(0.0)
            } else {
                py_float(it, &a0, pos)?
            }
        }
        "bool" => V::Bool(it.truthy(&a0)),
        "list" | "tuple" => {
            if args.is_empty() {
                V::arr(vec![])
            } else {
                V::arr(it.iterate(&a0, Some(pos))?)
            }
        }
        "set" => {
            let items = if args.is_empty() { vec![] } else { it.iterate(&a0, Some(pos))? };
            make_set(it, items)
        }
        "dict" => {
            let kwa = kwargs(&mut args);
            let mut pm = PropMap::default();
            match args.first() {
                Some(V::Obj(o)) if matches!(o.borrow().kind, ObjKind::Plain | ObjKind::Kwargs) && o.borrow().proto.is_none() => pm = o.borrow().props.clone(),
                Some(other) => {
                    let other = other.clone();
                    for pair in it.iterate(&other, Some(pos))? {
                        let kv = it.iterate(&pair, Some(pos))?;
                        if kv.len() == 2 {
                            let k = it.key_string(&kv[0]);
                            pm.set(&k, kv[1].clone());
                        }
                    }
                }
                None => {}
            }
            if let Some(k) = kwa {
                for (key, v) in k.borrow().props.iter() {
                    pm.set(key, v.clone());
                }
            }
            V::obj(pm)
        }
        "range" => {
            let (a, b, s) = match args.len() {
                0 => return it.throw("TypeError", "range expected at least 1 argument, got 0".into(), Some(pos)),
                1 => (0, iarg(it, &args, 0, 0), 1),
                2 => (iarg(it, &args, 0, 0), iarg(it, &args, 1, 0), 1),
                _ => (iarg(it, &args, 0, 0), iarg(it, &args, 1, 0), iarg(it, &args, 2, 1)),
            };
            if s == 0 {
                return it.throw("ValueError", "range() arg 3 must not be zero".into(), Some(pos));
            }
            let count = if s > 0 { (b - a + s - 1).max(0) / s } else { (a - b - s - 1).max(0) / -s };
            if count > 10_000_000 {
                return it.throw("MemoryError", "range too large".into(), Some(pos));
            }
            V::arr((0..count).map(|i| V::Int(a + i * s)).collect())
        }
        "sum" => {
            let items = it.iterate(&a0, Some(pos))?;
            let start = args.get(1).cloned().unwrap_or(V::Int(0));
            sum_values(it, items, start, pos)?
        }
        "min" => min_max(it, args, false, pos)?,
        "max" => min_max(it, args, true, pos)?,
        "sorted" => {
            let kwa = kwargs(&mut args);
            let items = it.iterate(&arg(&args, 0), Some(pos))?;
            let key = kw(&kwa, "key").filter(|k| !k.is_nullish());
            let mut out = match key {
                Some(k) => {
                    let mut keyed = Vec::new();
                    for x in items {
                        let kv = it.call(&k, V::Undef, vec![x.clone()], pos)?;
                        keyed.push(V::arr(vec![kv, x]));
                    }
                    let sorted = sort_vals(it, keyed, &mut |it, a, b| it.py_order(&first(a), &first(b), pos))?;
                    sorted.iter().map(|p| if let V::Arr(a) = p { a.borrow()[1].clone() } else { V::Null }).collect()
                }
                None => sort_with(it, items, None, false, pos)?,
            };
            if kw(&kwa, "reverse").is_some_and(|r| it.truthy(&r)) {
                out.reverse();
            }
            V::arr(out)
        }
        "reversed" => {
            let mut items = it.iterate(&a0, Some(pos))?;
            items.reverse();
            V::arr(items)
        }
        "enumerate" => {
            let kwa = kwargs(&mut args);
            let start = kw(&kwa, "start").and_then(|v| v.as_f64()).map(|f| f as i64).unwrap_or_else(|| iarg(it, &args, 1, 0));
            let items = it.iterate(&a0, Some(pos))?;
            V::arr(items.into_iter().enumerate().map(|(i, x)| V::arr(vec![V::Int(start + i as i64), x])).collect())
        }
        "zip" => {
            let mut lists = Vec::new();
            for a in &args {
                lists.push(it.iterate(a, Some(pos))?);
            }
            let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
            V::arr((0..n).map(|i| V::arr(lists.iter().map(|l| l[i].clone()).collect())).collect())
        }
        "map" => {
            let f = a0;
            let items = it.iterate(&arg(&args, 1), Some(pos))?;
            let mut out = Vec::new();
            for x in items {
                out.push(it.call(&f, V::Undef, vec![x], pos)?);
            }
            V::arr(out)
        }
        "filter" => {
            let f = a0;
            let items = it.iterate(&arg(&args, 1), Some(pos))?;
            let mut out = Vec::new();
            for x in items {
                let keep = if f.is_nullish() { it.truthy(&x) } else { let r = it.call(&f, V::Undef, vec![x.clone()], pos)?; it.truthy(&r) };
                if keep {
                    out.push(x);
                }
            }
            V::arr(out)
        }
        "any" => {
            let items = it.iterate(&a0, Some(pos))?;
            V::Bool(items.iter().any(|x| it.truthy(x)))
        }
        "all" => {
            let items = it.iterate(&a0, Some(pos))?;
            V::Bool(items.iter().all(|x| it.truthy(x)))
        }
        "abs" => match a0 {
            V::Int(i) => V::Int(i.abs()),
            _ => V::Num(farg(it, &args, 0).abs()),
        },
        "round" => {
            let f = farg(it, &args, 0);
            match args.get(1) {
                Some(n) if !n.is_nullish() => {
                    let n = iarg(it, &args, 1, 0);
                    let m = 10f64.powi(n as i32);
                    let r = (f * m).round_ties_even() / m;
                    if matches!(a0, V::Int(_)) {
                        V::Int(r as i64)
                    } else {
                        V::Num(r)
                    }
                }
                _ => V::Int(f.round_ties_even() as i64),
            }
        }
        "isinstance" => {
            let types = match &args.get(1) {
                Some(V::Arr(a)) => a.borrow().clone(),
                Some(t) => vec![(*t).clone()],
                None => vec![],
            };
            let mut r = false;
            for t in &types {
                r = r || match t {
                    V::Host(h) => match &**h {
                        Host::Ns(n) => is_type(&a0, n),
                        Host::Type(n) => is_type(&a0, n),
                        _ => false,
                    },
                    V::Func(_) => instance_of(it, &a0, t),
                    _ => false,
                };
            }
            V::Bool(r)
        }
        "repr" => match it.py_special(&a0, "__repr__", vec![], pos)? {
            Some(r) => r,
            None => V::string(py_repr(it, &a0)),
        },
        "ord" => V::Int(sarg(it, &args, 0).chars().next().map(|c| c as i64).unwrap_or(0)),
        "chr" => V::string(char::from_u32(iarg(it, &args, 0, 0) as u32).map(|c| c.to_string()).unwrap_or_default()),
        "hex" => {
            let i = iarg(it, &args, 0, 0);
            V::string(if i < 0 { format!("-0x{:x}", -i) } else { format!("0x{:x}", i) })
        }
        "type" if args.len() == 3 => it.py_type3(&a0, &arg(&args, 1), &arg(&args, 2), pos)?,
        "type" => match crate::classes::class_of(&a0) {
            Some(c) => V::Func(c),
            None if matches!(&a0, V::Obj(o) if o.borrow().kind == ObjKind::Error) => V::host(Host::Type(error_name(&a0))),
            None => V::host(Host::Type(py_type_name(&a0).to_string())),
        },
        "divmod" => {
            let q = it.binary(BinOp::FloorDiv, a0.clone(), arg(&args, 1), pos)?;
            let r = it.binary(BinOp::Mod, a0, arg(&args, 1), pos)?;
            V::arr(vec![q, r])
        }
        "pow" => it.binary(BinOp::Pow, a0, arg(&args, 1), pos)?,
        "hasattr" => {
            let k = sarg(it, &args, 1);
            V::Bool(match &a0 {
                V::Obj(o) => o.borrow().lookup(&k).is_some(),
                _ => false,
            })
        }
        "getattr" => {
            let k = sarg(it, &args, 1);
            match it.get_member(&a0, &k, pos) {
                Ok(v) if !v.is_nullish() => v,
                Ok(_) | Err(_) if args.len() > 2 => args[2].clone(),
                Ok(v) => v,
                Err(e) => return Err(e),
            }
        }
        "Integer" | "Long" | "Double" | "BigDecimal" | "Float" => as_type(it, a0, n, pos)?,
        _ => return it.type_error(format!("{} is not a function", n), Some(pos)),
    })
}

/// `new X(...)` for built-in classes.
pub fn construct_ns(it: &mut Interp, n: &'static str, args: Vec<V>, pos: u32) -> R<V> {
    let a0 = arg(&args, 0);
    if matches!(n, "TreeMap" | "TreeSet") && it.d.groovy_like() {
        // sorted collections: kept ordered on every change (natural order or a Comparator)
        let cmp = if a0.is_callable() { Some(a0.clone()) } else { None };
        let v = if n == "TreeMap" {
            let mut p = PropMap::default();
            if let V::Obj(o) = &a0 {
                if cmp.is_none() {
                    p = o.borrow().props.clone();
                }
            }
            V::obj(p)
        } else {
            let items = if a0.is_nullish() || a0.is_number() || cmp.is_some() { vec![] } else { it.iterate(&a0, Some(pos))? };
            make_set(it, items)
        };
        it.register_sorted(&v, cmp);
        return Ok(v);
    }
    if it.d == Dialect::Js {
        if let Some(v) = crate::jsext::construct_ns(it, n, &args, pos)? {
            return Ok(v);
        }
        match n {
            "Map" | "Set" | "WeakMap" | "WeakSet" => return it.construct_coll(n, &args, pos),
            "Promise" => return it.construct_promise(&args, pos),
            "Symbol" => return it.type_error("Symbol is not a constructor".into(), Some(pos)),
            _ => {}
        }
    }
    if let Some(v) = crate::java::construct(it, n, &args, pos)? {
        return Ok(v);
    }
    Ok(match n {
        "Date" => {
            if args.is_empty() {
                make_date(now_millis() as f64)
            } else if args.len() == 1 {
                match &a0 {
                    V::Str(s) => make_date(parse_date(s).unwrap_or(f64::NAN)),
                    V::Obj(_) => make_date(date_ms(&a0).unwrap_or(f64::NAN)),
                    _ => make_date(farg(it, &args, 0)),
                }
            } else {
                let y = iarg(it, &args, 0, 1970);
                let mo = iarg(it, &args, 1, 0);
                let d = iarg(it, &args, 2, 1);
                let h = iarg(it, &args, 3, 0);
                let mi = iarg(it, &args, 4, 0);
                let s = iarg(it, &args, 5, 0);
                let ms = iarg(it, &args, 6, 0);
                let (y, mo) = (y + mo.div_euclid(12), mo.rem_euclid(12));
                let secs = epoch_from_civil(y, mo as u32 + 1, 1, 0, 0, 0) + (d - 1) * 86_400 + h * 3600 + mi * 60 + s;
                make_date((secs * 1000 + ms) as f64)
            }
        }
        "Error" | "TypeError" | "RangeError" | "SyntaxError" | "ReferenceError" => {
            let msg = sarg(it, &args, 0);
            it.make_error(n, msg, None)
        }
        "Array" | "Object" | "RegExp" | "String" | "Number" | "Boolean" => return call_ns(it, n, args, pos),
        "ArrayList" | "LinkedList" | "List" => {
            if a0.is_nullish() || a0.is_number() {
                V::arr(vec![])
            } else {
                V::arr(it.iterate(&a0, Some(pos))?)
            }
        }
        "HashMap" | "LinkedHashMap" | "TreeMap" | "Map" => match &a0 {
            V::Obj(o) => {
                let mut pm = o.borrow().props.clone();
                if n == "TreeMap" {
                    pm.sort_keys();
                }
                V::obj(pm)
            }
            _ => V::new_obj(),
        },
        "HashSet" | "LinkedHashSet" | "TreeSet" | "Set" => {
            let items = if a0.is_nullish() || a0.is_number() { vec![] } else { it.iterate(&a0, Some(pos))? };
            make_set(it, items)
        }
        "JsonSlurper" => V::host(Host::Ns("JsonSlurper")),
        "StringBuilder" => {
            let mut p = PropMap::default();
            p.set("%sb", V::string(if a0.is_number() || a0.is_nullish() { String::new() } else { it.display(&a0) }));
            V::obj(p)
        }
        "Integer" | "Long" | "Double" | "BigDecimal" | "Float" | "Short" | "BigInteger" => as_type(it, a0, n, pos)?,
        _ => return construct_type(it, n, args, pos),
    })
}

/// `new X(...)` / `X(...)` for classes without built-in support: exception types create error objects.
pub fn construct_type(it: &mut Interp, n: &str, args: Vec<V>, pos: u32) -> R<V> {
    if n.ends_with("Exception") || n.ends_with("Error") || n == "Throwable" || n == "StopIteration" {
        let msg = if args.is_empty() { String::new() } else { args.iter().map(|a| it.display(a)).collect::<Vec<_>>().join(", ") };
        return Ok(it.make_error(n, msg, None));
    }
    if n == "Object" {
        return Ok(V::new_obj());
    }
    if n == "Expando" && it.d == Dialect::Groovy {
        // a dynamic bean: properties and closures (called as methods) can be added freely
        let props = match args.first() {
            Some(V::Obj(o)) => o.borrow().props.clone(),
            _ => PropMap::default(),
        };
        let mut marker = Obj::new();
        marker.props.set("%expando", V::Bool(true));
        return Ok(V::Obj(new_obj_ref(Obj { props, proto: Some(new_obj_ref(marker)), kind: ObjKind::Plain })));
    }
    let msg = match it.d {
        Dialect::Groovy => format!("unable to resolve class {}", n),
        _ => format!("{} is not a constructor", n),
    };
    it.throw(if it.d == Dialect::Groovy { "MultipleCompilationErrorsException" } else { "TypeError" }, msg, Some(pos))
}

fn json_dumps(it: &Interp, v: &V, kwa: &Option<ObjRef>) -> String {
    let indent = kw(kwa, "indent").filter(|i| !i.is_nullish()).map(|i| match i {
        V::Str(s) => s.to_string(),
        other => " ".repeat(other.as_f64().unwrap_or(0.0) as usize),
    });
    let sort = kw(kwa, "sort_keys").is_some_and(|s| it.truthy(&s));
    let (item, key) = match kw(kwa, "separators") {
        Some(V::Arr(a)) => {
            let a = a.borrow();
            let i = a.first().map(|x| it.display(x)).unwrap_or_default();
            let k = a.get(1).map(|x| it.display(x)).unwrap_or_default();
            (if i.ends_with(' ') { " " } else { "" }, if k.ends_with(' ') { " " } else { "" })
        }
        _ => (" ", " "),
    };
    json::stringify(v, &JsonOpts { indent, item_sep: item, key_sep: key, sort_keys: sort, js: false, newline: "\n" }).unwrap_or_else(|| "null".into())
}

/// Call a namespace member function (`Math.max`, `JSON.parse`, `json.loads`, `std.length`...).
pub fn call_ns_fn(it: &mut Interp, ns: &str, name: &str, mut args: Vec<V>, pos: u32) -> R<V> {
    if it.d.groovy_like() && crate::javaext::NS.contains(&ns) {
        if let Some(v) = it.jtime_static(ns, name, &args, pos)? {
            return Ok(v);
        }
    }
    if it.d == Dialect::Python {
        if let Some(v) = it.py_call_ns_fn(ns, name, args.clone(), pos)? {
            return Ok(v);
        }
    }
    if it.d == Dialect::Js {
        if let Some(v) = crate::jsext::call_ns_fn(it, ns, name, &args, pos)? {
            return Ok(v);
        }
    }
    let a0 = arg(&args, 0);
    let java = it.d != Dialect::Js;
    let r = match (ns, name) {
        ("std" | "cml" | "ds", _) => return crate::jsonnet::std_call(it, ns, name, args, pos),
        ("jOOR", _) => return crate::java::call_fn(it, name, args, pos),
        ("ds.datetime", _) => return crate::dstime::call(it, name, args, pos),
        (n, _) if n.starts_with("ds.") => return crate::dslib::call(it, n, name, args, pos),
        ("Comparator" | "Function" | "Collectors" | "Optional" | "Objects" | "Character" | "Stream" | "IntStream" | "LongStream" | "DoubleStream" | "SystemOut" | "List" | "Set" | "Map" | "JavaString" | "JavaSystem", _)
            if matches!(ns, "Comparator" | "Function" | "Collectors" | "Optional" | "Objects" | "Character" | "Stream" | "IntStream" | "LongStream" | "DoubleStream" | "SystemOut" | "JavaString" | "JavaSystem") || matches!((ns, name), ("List" | "Set", "of" | "copyOf") | ("Map", "of" | "entry" | "ofEntries" | "copyOf")) =>
        {
            if let Some(v) = crate::java::java_static(it, ns, name, args, pos)? {
                return Ok(v);
            }
            return it.type_error(format!("{}.{} is not a function", ns, name), Some(pos));
        }
        // ---------------- Math
        ("Math" | "math", "abs" | "fabs") => match a0 {
            V::Int(i) if name == "abs" => V::Int(i.abs()),
            _ => math1(it, &args, f64::abs),
        },
        ("Math" | "math", "floor") => {
            let f = farg(it, &args, 0).floor();
            if ns == "math" {
                V::Int(f as i64)
            } else {
                it.mk_num(f)
            }
        }
        ("Math" | "math", "ceil") => {
            let f = farg(it, &args, 0).ceil();
            if ns == "math" {
                V::Int(f as i64)
            } else {
                it.mk_num(f)
            }
        }
        ("Math", "round") => {
            let f = farg(it, &args, 0);
            let r = (f + 0.5).floor();
            if java {
                V::Int(r as i64)
            } else {
                it.mk_num(r)
            }
        }
        ("Math" | "math", "trunc") => {
            let f = farg(it, &args, 0).trunc();
            if ns == "math" {
                V::Int(f as i64)
            } else {
                it.mk_num(f)
            }
        }
        ("Math", "sign" | "signum") => math1(it, &args, |f| if f > 0.0 { 1.0 } else if f < 0.0 { -1.0 } else { f }),
        ("Math" | "math", "sqrt") => math1(it, &args, f64::sqrt),
        ("Math", "cbrt") => math1(it, &args, f64::cbrt),
        ("Math" | "math", "exp") => math1(it, &args, f64::exp),
        ("Math" | "math", "log") => {
            if ns == "math" && args.len() > 1 {
                let b = farg(it, &args, 1);
                V::Num(farg(it, &args, 0).ln() / b.ln())
            } else {
                math1(it, &args, f64::ln)
            }
        }
        ("Math" | "math", "log10") => math1(it, &args, f64::log10),
        ("Math" | "math", "log2") => math1(it, &args, f64::log2),
        ("Math" | "math", "log1p") => math1(it, &args, f64::ln_1p),
        ("Math" | "math", "sin") => math1(it, &args, f64::sin),
        ("Math" | "math", "cos") => math1(it, &args, f64::cos),
        ("Math" | "math", "tan") => math1(it, &args, f64::tan),
        ("Math" | "math", "asin") => math1(it, &args, f64::asin),
        ("Math" | "math", "acos") => math1(it, &args, f64::acos),
        ("Math" | "math", "atan") => math1(it, &args, f64::atan),
        ("Math" | "math", "atan2") => it.mk_num(farg(it, &args, 0).atan2(farg(it, &args, 1))),
        ("Math" | "math", "hypot") => it.mk_num(args.iter().enumerate().map(|(i, _)| farg(it, &args, i).powi(2)).sum::<f64>().sqrt()),
        ("Math" | "math", "pow") => {
            if java || ns == "math" {
                V::Num(farg(it, &args, 0).powf(farg(it, &args, 1)))
            } else {
                it.mk_num(farg(it, &args, 0).powf(farg(it, &args, 1)))
            }
        }
        ("Math", "random") => it.mk_num((random_u64() >> 11) as f64 / (1u64 << 53) as f64),
        ("Math", "toRadians") => V::Num(farg(it, &args, 0).to_radians()),
        ("Math", "toDegrees") => V::Num(farg(it, &args, 0).to_degrees()),
        ("math", "isnan") => V::Bool(farg(it, &args, 0).is_nan()),
        ("math", "isinf") => V::Bool(farg(it, &args, 0).is_infinite()),
        ("math", "factorial") => {
            let n = iarg(it, &args, 0, 0);
            V::Int((1..=n.max(1)).product())
        }
        ("math", "gcd") => {
            let (mut a, mut b) = (iarg(it, &args, 0, 0).abs(), iarg(it, &args, 1, 0).abs());
            while b != 0 {
                (a, b) = (b, a % b);
            }
            V::Int(a)
        }
        ("Math" | "Integer" | "Long" | "Double", "max" | "min") => {
            let want_max = name == "max";
            if args.is_empty() {
                return Ok(it.mk_num(if want_max { f64::NEG_INFINITY } else { f64::INFINITY }));
            }
            let nums: Vec<V> = args.iter().map(|a| if it.d == Dialect::Js { it.to_number(a) } else { a.clone() }).collect();
            if nums.iter().any(|n| matches!(n, V::Num(f) if f.is_nan())) {
                return Ok(V::Num(f64::NAN));
            }
            let mut best = nums[0].clone();
            for n in nums.into_iter().skip(1) {
                let o = natural(it, &n, &best);
                if (want_max && o == Ordering::Greater) || (!want_max && o == Ordering::Less) {
                    best = n;
                }
            }
            best
        }
        ("Math", "floorDiv") => V::Int(iarg(it, &args, 0, 0).div_euclid(iarg(it, &args, 1, 1))),
        ("Math", "floorMod") => V::Int(iarg(it, &args, 0, 0).rem_euclid(iarg(it, &args, 1, 1))),
        // ---------------- JSON
        ("JSON", "parse") => json_parse(it, &sarg(it, &args, 0), pos)?,
        ("JSON", "stringify") => {
            let a0 = it.materialize(&a0)?;
            let indent = match args.get(2) {
                Some(V::Int(n)) if *n > 0 => Some(" ".repeat((*n).min(10) as usize)),
                Some(V::Str(s)) if !s.is_empty() => Some(s.to_string()),
                _ => None,
            };
            match json::stringify(&a0, &JsonOpts { indent, item_sep: "", key_sep: "", sort_keys: false, js: true, newline: "\n" }) {
                Some(s) => V::string(s),
                None => V::Undef,
            }
        }
        ("json", "loads" | "load") => json_parse(it, &sarg(it, &args, 0), pos)?,
        ("json", "dumps" | "dump") => {
            let kwa = kwargs(&mut args);
            V::string(json_dumps(it, &arg(&args, 0), &kwa))
        }
        ("JsonSlurper", "parseText" | "parse") => json_parse(it, &sarg(it, &args, 0), pos)?,
        ("JsonOutput", "toJson") => V::string(json::stringify(&a0, &JsonOpts { indent: None, item_sep: "", key_sep: "", sort_keys: false, js: false, newline: "\n" }).unwrap_or_else(|| "null".into())),
        ("JsonOutput", "prettyPrint") => {
            let v = json_parse(it, &sarg(it, &args, 0), pos)?;
            V::string(json::stringify(&v, &JsonOpts { indent: Some("    ".into()), item_sep: "", key_sep: "", sort_keys: false, js: false, newline: "\n" }).unwrap_or_default())
        }
        // ---------------- Object / Array / Number / String / Date
        ("Object", "keys" | "getOwnPropertyNames") => match &a0 {
            V::Obj(o) => V::arr(o.borrow().props.keys().map(|k| V::Str(k.clone())).collect()),
            V::Arr(a) => V::arr((0..a.borrow().len()).map(|i| V::string(i.to_string())).collect()),
            V::Str(s) => V::arr((0..char_len(s)).map(|i| V::string(i.to_string())).collect()),
            V::Host(h) => match crate::host::snapshot(it, h) {
                Some(V::Obj(o)) => V::arr(o.borrow().props.keys().map(|k| V::Str(k.clone())).collect()),
                _ => V::arr(vec![]),
            },
            V::Null | V::Undef => return it.type_error("Cannot convert undefined or null to object".into(), Some(pos)),
            _ => V::arr(vec![]),
        },
        ("Object", "values") => match &a0 {
            V::Obj(o) => V::arr(o.borrow().props.values().cloned().collect()),
            V::Arr(a) => V::arr(a.borrow().clone()),
            V::Host(h) => match crate::host::snapshot(it, h) {
                Some(V::Obj(o)) => V::arr(o.borrow().props.values().cloned().collect()),
                _ => V::arr(vec![]),
            },
            _ => V::arr(vec![]),
        },
        ("Object", "entries") => {
            let src = match &a0 {
                V::Host(h) => crate::host::snapshot(it, h).unwrap_or(V::Null),
                other => other.clone(),
            };
            match &src {
                V::Obj(o) => V::arr(o.borrow().props.iter().map(|(k, v)| V::arr(vec![V::Str(k.clone()), v.clone()])).collect()),
                V::Arr(a) => V::arr(a.borrow().iter().enumerate().map(|(i, v)| V::arr(vec![V::string(i.to_string()), v.clone()])).collect()),
                _ => V::arr(vec![]),
            }
        }
        ("Object", "assign") => {
            if let V::Obj(target) = &a0 {
                for src in args.iter().skip(1) {
                    let src = match src {
                        V::Host(h) => crate::host::snapshot(it, h).unwrap_or(V::Null),
                        other => other.clone(),
                    };
                    if let V::Obj(s) = &src {
                        let items: Vec<(Rc<str>, V)> = s.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        for (k, v) in items {
                            target.borrow_mut().props.set(&k, v);
                        }
                    }
                }
            }
            a0
        }
        ("Object", "fromEntries") => {
            let mut pm = PropMap::default();
            for e in it.iterate(&a0, Some(pos))? {
                let kv = it.iterate(&e, Some(pos))?;
                if let Some(k) = kv.first() {
                    let ks = it.key_string(k);
                    pm.set(&ks, kv.get(1).cloned().unwrap_or(V::Undef));
                }
            }
            V::obj(pm)
        }
        ("Object", "freeze" | "seal" | "preventExtensions") => a0,
        ("Object", "isFrozen") => V::Bool(false),
        ("Object", "create") => {
            let o = Obj { props: PropMap::default(), proto: if let V::Obj(p) = &a0 { Some(p.clone()) } else { None }, kind: ObjKind::Plain };
            V::Obj(Rc::new(RefCell::new(o)))
        }
        ("Object", "getPrototypeOf") => match &a0 {
            V::Obj(o) => o.borrow().proto.clone().map(V::Obj).unwrap_or(V::Null),
            _ => V::Null,
        },
        ("Object", "setPrototypeOf") => {
            if let (V::Obj(o), V::Obj(p)) = (&a0, &arg(&args, 1)) {
                o.borrow_mut().proto = Some(p.clone());
            }
            a0
        }
        ("Object", "is") => V::Bool(crate::interp::strict_eq(&a0, &arg(&args, 1))),
        ("Array", "isArray") => V::Bool(matches!(a0, V::Arr(_))),
        ("Array" | "Arrays" | "List" | "Set", "from" | "of" | "asList") => {
            let items = if name == "from" { it.iterate(&a0, Some(pos))? } else { args.clone() };
            let items = if let (Some(f), "from") = (args.get(1), name) {
                let f = f.clone();
                let mut out = Vec::new();
                for (i, x) in items.into_iter().enumerate() {
                    out.push(it.call(&f, V::Undef, vec![x, V::Int(i as i64)], pos)?);
                }
                out
            } else {
                items
            };
            if ns == "Set" {
                make_set(it, items)
            } else {
                V::arr(items)
            }
        }
        ("Map", "of") => {
            let mut pm = PropMap::default();
            for pair in args.chunks(2) {
                let k = it.key_string(&pair[0]);
                pm.set(&k, pair.get(1).cloned().unwrap_or(V::Null));
            }
            V::obj(pm)
        }
        ("Number", "isInteger") => V::Bool(a0.is_number() && a0.as_f64().is_some_and(|f| f.fract() == 0.0 && f.is_finite())),
        ("Number", "isSafeInteger") => V::Bool(a0.is_number() && a0.as_f64().is_some_and(|f| f.fract() == 0.0 && f.abs() <= 9_007_199_254_740_991.0)),
        ("Number", "isFinite") => V::Bool(a0.is_number() && a0.as_f64().is_some_and(f64::is_finite)),
        ("Number", "isNaN") => V::Bool(matches!(a0, V::Num(f) if f.is_nan())),
        ("Number", "parseFloat") => js_parse_float(&sarg(it, &args, 0)),
        ("Number", "parseInt") => {
            let s = sarg(it, &args, 0);
            js_parse_int(&s, iarg(it, &args, 1, 0))
        }
        ("String", "fromCharCode" | "fromCodePoint") => V::string(args.iter().enumerate().filter_map(|(i, _)| char::from_u32(farg(it, &args, i) as u32)).collect()),
        ("String", "valueOf") => V::string(it.display(&a0)),
        ("String", "format") => {
            let f = sarg(it, &args, 0);
            let rest: Vec<V> = args.iter().skip(1).cloned().collect();
            match crate::format::printf(it, &f, &rest, None) {
                Ok(s) => V::string(s),
                Err(m) => return it.throw("IllegalFormatException", m, Some(pos)),
            }
        }
        ("String", "join") => {
            let sep = sarg(it, &args, 0);
            let iterable = args.len() == 2 && (matches!(args[1], V::Arr(_)) || matches!(&args[1], V::Obj(o) if o.borrow().kind == ObjKind::Set));
            let items = if iterable { it.iterate(&args[1], Some(pos))? } else { args.iter().skip(1).cloned().collect() };
            V::string(items.iter().map(|x| it.display(x)).collect::<Vec<_>>().join(&sep))
        }
        ("Date", "now") => V::Int(now_millis()),
        ("Date", "parse") => norm_num(parse_date(&sarg(it, &args, 0)).unwrap_or(f64::NAN)),
        ("Date", "UTC") => {
            let d = construct_ns(it, "Date", args, pos)?;
            norm_num(date_ms(&d).unwrap_or(f64::NAN))
        }
        ("console", _) => {
            let mut strs = Vec::with_capacity(args.len());
            for a in &args {
                strs.push(if matches!(a, V::Obj(_)) { V::string(it.to_str(a)?) } else { a.clone() });
            }
            log_line(it, &strs);
            V::Undef
        }
        ("Promise", f) => return it.promise_static(f, args, pos),
        ("Symbol", "for") => {
            let k = sarg(it, &args, 0);
            it.symbol_for(&k)
        }
        ("Symbol", "keyFor") => {
            let mut found = V::Undef;
            for (k, v) in it.symbols.iter() {
                if v.same(&a0) && !k.starts_with("@@") {
                    found = V::str(k);
                }
            }
            found
        }
        ("Java", "type") => return crate::java::java_type_for_js(it, &sarg(it, &args, 0), pos),
        ("Object", "defineProperty") => {
            let k = it.key_string(&arg(&args, 1));
            define_property(it, &a0, &k, &arg(&args, 2), pos)?;
            a0
        }
        ("Object", "defineProperties") => {
            if let V::Obj(ps) = arg(&args, 1) {
                let items: Vec<(Rc<str>, V)> = ps.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                for (k, desc) in items {
                    define_property(it, &a0, &k, &desc, pos)?;
                }
            }
            a0
        }
        ("Object", "getOwnPropertyDescriptor") => {
            let k = it.key_string(&arg(&args, 1));
            match &a0 {
                V::Obj(o) => {
                    let v = o.borrow().props.get(&k).cloned();
                    match v {
                        Some(V::Obj(acc)) if acc.borrow().kind == ObjKind::Accessor => {
                            let mut p = PropMap::default();
                            p.set("get", acc.borrow().props.get("get").cloned().unwrap_or(V::Undef));
                            p.set("set", acc.borrow().props.get("set").cloned().unwrap_or(V::Undef));
                            p.set("enumerable", V::Bool(true));
                            p.set("configurable", V::Bool(true));
                            V::obj(p)
                        }
                        Some(v) => {
                            let mut p = PropMap::default();
                            p.set("value", v);
                            p.set("writable", V::Bool(true));
                            p.set("enumerable", V::Bool(true));
                            p.set("configurable", V::Bool(true));
                            V::obj(p)
                        }
                        None => V::Undef,
                    }
                }
                _ => V::Undef,
            }
        }
        ("Object", "getOwnPropertySymbols") => match &a0 {
            V::Obj(o) => {
                let keys: Vec<Rc<str>> = o.borrow().props.symbols().into_iter().map(|(k, _)| k).collect();
                let mut out = Vec::new();
                for k in keys {
                    if let Some(s) = it.symbols.values().find(|s| it.key_string(s) == *k) {
                        out.push(s.clone());
                    }
                }
                V::arr(out)
            }
            _ => V::arr(vec![]),
        }
        ("Reflect", "ownKeys") => match &a0 {
            V::Obj(o) => V::arr(o.borrow().props.keys().map(|k| V::Str(k.clone())).collect()),
            _ => V::arr(vec![]),
        },
        ("Reflect", "has") => {
            let k = it.key_string(&arg(&args, 1));
            V::Bool(matches!(&a0, V::Obj(o) if o.borrow().lookup(&k).is_some()))
        }
        ("Reflect", "get") => {
            let k = arg(&args, 1);
            it.get_index(&a0, &k, pos)?
        }
        ("Reflect", "set") => {
            let k = arg(&args, 1);
            it.set_index(&a0, &k, arg(&args, 2), pos)?;
            V::Bool(true)
        }
        ("Reflect", "apply") => {
            let a = it.clone_iter(&arg(&args, 2))?;
            it.call(&a0, arg(&args, 1), a, pos)?
        }
        ("Reflect", "construct") => {
            let a = it.clone_iter(&arg(&args, 1))?;
            it.construct_pub(&a0, a, pos)?
        }
        // ---------------- Java classes
        ("Integer" | "Long" | "Short" | "Byte" | "BigInteger", "parseInt" | "parseLong" | "valueOf" | "parseShort" | "decode") => {
            if a0.is_number() {
                V::Int(farg(it, &args, 0) as i64)
            } else {
                let s = sarg(it, &args, 0);
                match parse_int_str(&s, iarg(it, &args, 1, 10) as u32) {
                    Some(i) => V::Int(i),
                    None => return it.throw("NumberFormatException", format!("For input string: \"{}\"", s), Some(pos)),
                }
            }
        }
        ("Integer" | "Long", "toString") => {
            let i = iarg(it, &args, 0, 0);
            let r = iarg(it, &args, 1, 10);
            V::string(match r {
                16 => format!("{:x}", i),
                2 => format!("{:b}", i),
                8 => format!("{:o}", i),
                _ => i.to_string(),
            })
        }
        ("Integer" | "Long", "toHexString") => V::string(format!("{:x}", iarg(it, &args, 0, 0))),
        ("Integer" | "Long", "toBinaryString") => V::string(format!("{:b}", iarg(it, &args, 0, 0))),
        ("Integer" | "Long" | "Double", "sum") => it.binary(BinOp::Add, a0, arg(&args, 1), pos)?,
        ("Integer" | "Long" | "Double", "compare") => V::Int(natural(it, &a0, &arg(&args, 1)) as i64),
        ("Double" | "Float" | "BigDecimal", "parseDouble" | "parseFloat" | "valueOf") => {
            let s = sarg(it, &args, 0);
            match s.trim().parse::<f64>() {
                Ok(f) => V::Num(f),
                Err(_) => return it.throw("NumberFormatException", format!("For input string: \"{}\"", s), Some(pos)),
            }
        }
        ("Double", "isNaN") => V::Bool(farg(it, &args, 0).is_nan()),
        ("Boolean", "parseBoolean" | "valueOf") => V::Bool(sarg(it, &args, 0).eq_ignore_ascii_case("true") || matches!(a0, V::Bool(true))),
        ("Boolean", "toString") => V::string(it.display(&a0)),
        ("System", "currentTimeMillis") => V::Int(now_millis()),
        ("System", "nanoTime") => V::Int(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)),
        ("System", "getenv") => std::env::var(sarg(it, &args, 0)).map(V::string).unwrap_or(V::Null),
        ("System", "getProperty") => {
            let k = sarg(it, &args, 0);
            match k.as_str() {
                "line.separator" => V::str("\n"),
                "file.separator" => V::str("/"),
                "user.dir" => std::env::current_dir().map(|d| V::string(d.display().to_string())).unwrap_or(V::Null),
                _ => args.get(1).cloned().unwrap_or(V::Null),
            }
        }
        ("System", "lineSeparator") => V::str("\n"),
        ("UUID", "randomUUID") => V::string(uuid_v4()),
        ("UUID", "fromString") => V::string(sarg(it, &args, 0)),
        ("Collections", "sort") => {
            if let V::Arr(a) = &a0 {
                let items = a.borrow().clone();
                let sorted = sort_with(it, items, args.get(1), false, pos)?;
                *a.borrow_mut() = sorted;
            }
            V::Undef
        }
        ("Collections", "reverse") => {
            if let V::Arr(a) = &a0 {
                a.borrow_mut().reverse();
            }
            V::Undef
        }
        ("Collections", "max" | "min") => min_max(it, vec![a0], name == "max", pos)?,
        ("Collections", "emptyList") => V::arr(vec![]),
        ("Collections", "emptyMap") => V::new_obj(),
        ("Collections", "singletonList") => V::arr(vec![a0]),
        ("Collections", "unmodifiableList" | "unmodifiableMap" | "unmodifiableSet" | "synchronizedList" | "synchronizedMap") => a0,
        ("Arrays", "toString") => V::string(display(it, &a0, 0)),
        ("Arrays", "sort") => return call_ns_fn(it, "Collections", "sort", args, pos),
        ("Arrays", "stream") => match a0 {
            V::Arr(a) => V::arr(a.borrow().clone()),
            other => V::arr(it.iterate(&other, Some(pos))?),
        },
        ("Pattern", "compile") => {
            let p = sarg(it, &args, 0);
            make_regex(it, &p, "", pos)?
        }
        ("Pattern", "matches") => {
            let re = to_regex(it, &a0, pos)?;
            V::Bool(re.re.is_full_match(&sarg(it, &args, 1)))
        }
        ("Pattern", "quote") => V::string(camel_core::regex::Regex::quote(&sarg(it, &args, 0))),
        // ---------------- Python re
        ("re", "compile") => {
            let p = sarg(it, &args, 0);
            make_regex(it, &p, "", pos)?
        }
        ("re", "escape") => V::string(camel_core::regex::Regex::quote(&sarg(it, &args, 0))),
        ("re", _) => {
            let kwa = kwargs(&mut args);
            let re = to_regex(it, &a0, pos)?;
            match name {
                "match" | "search" | "fullmatch" => {
                    let s = sarg(it, &args, 1);
                    let c = match name {
                        "match" => re.re.captures_match(&s).filter(|c| c.span(0).is_some_and(|(st, _)| st == 0)).or_else(|| {
                            // anchored match at the start
                            let anchored = camel_core::regex::Regex::new(&format!("^(?:{})", re.re.as_str())).ok()?;
                            anchored.captures_match(&s)
                        }),
                        "fullmatch" => re.re.full_captures(&s),
                        _ => re.re.captures_match(&s),
                    };
                    match c {
                        Some(c) => match_obj(&c, &s),
                        None => V::Null,
                    }
                }
                "findall" => {
                    let s = sarg(it, &args, 1);
                    let groups = re.re.group_count();
                    V::arr(
                        re.re
                            .captures_iter(&s)
                            .iter()
                            .map(|c| {
                                let v = c.to_vec();
                                match groups {
                                    0 => V::string(v[0].clone().unwrap_or_default()),
                                    1 => V::string(v[1].clone().unwrap_or_default()),
                                    _ => V::arr(v.into_iter().skip(1).map(|g| V::string(g.unwrap_or_default())).collect()),
                                }
                            })
                            .collect(),
                    )
                }
                "finditer" => {
                    let s = sarg(it, &args, 1);
                    V::arr(re.re.captures_iter(&s).iter().map(|c| match_obj(c, &s)).collect())
                }
                "sub" => {
                    let s = sarg(it, &args, 2);
                    let count = kw(&kwa, "count").map(|c| farg(it, &[c], 0) as i64).unwrap_or_else(|| iarg(it, &args, 3, 0));
                    let repl = arg(&args, 1);
                    if repl.is_callable() {
                        V::string(regex_replace_fn(it, &re, &s, &repl, count != 1, pos)?)
                    } else {
                        let r = it.display(&repl);
                        let r = py_repl(&r);
                        V::string(if count == 1 { re.re.replace_first(&s, &r) } else { re.re.replace_all(&s, &r) })
                    }
                }
                "split" => {
                    let s = sarg(it, &args, 1);
                    V::arr(re.re.split_limit(&s, -1).into_iter().map(V::string).collect())
                }
                _ => return it.throw("AttributeError", format!("module 're' has no attribute '{}'", name), Some(pos)),
            }
        }
        _ => {
            let msg = match it.d {
                Dialect::Python => format!("module '{}' has no attribute '{}'", ns, name),
                Dialect::Groovy => format!("No signature of method: static {}.{}() is applicable", ns, name),
                Dialect::Spel => format!("EL1004E: Method call: Method {}() cannot be found on type {}", name, ns),
                _ => format!("{}.{} is not a function", ns, name),
            };
            let ename = match it.d {
                Dialect::Python => "AttributeError",
                Dialect::Groovy => "MissingMethodException",
                Dialect::Spel => "SpelEvaluationException",
                _ => "TypeError",
            };
            return it.throw(ename, msg, Some(pos));
        }
    };
    Ok(r)
}

fn py_repl(r: &str) -> String {
    // Python \1 / \g<1> -> $1
    let mut out = String::new();
    let mut chars = r.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(d) = chars.peek().copied() {
                if d.is_ascii_digit() {
                    chars.next();
                    out.push('$');
                    out.push(d);
                    continue;
                }
                if d == 'g' {
                    chars.next();
                    if chars.peek() == Some(&'<') {
                        chars.next();
                        let name: String = chars.by_ref().take_while(|c| *c != '>').collect();
                        out.push_str(&format!("${{{}}}", name));
                        continue;
                    }
                }
            }
        }
        if c == '$' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ====================================================================== value properties

/// Property of a primitive / array / regex value.
pub fn value_property(it: &mut Interp, o: &V, name: &str, pos: u32) -> R<V> {
    let java = it.d.is_java() || it.d == Dialect::Groovy;
    if it.d == Dialect::Js && matches!(o, V::Str(_) | V::Arr(_)) && name.starts_with(SYM_PREFIX) && name == it.well_known_key("iterator") {
        return Ok(V::Method(Rc::new((o.clone(), "%iter".to_string()))));
    }
    if let (V::Arr(a), Dialect::Js) = (o, it.d) {
        if let Some(r) = crate::jsext::arr_prop(it, a, name) {
            return Ok(r);
        }
    }
    if let (V::Arr(a), Dialect::Python) = (o, it.d) {
        if let Some(r) = it.namedtuple_member(a, name, pos)? {
            return Ok(r);
        }
    }
    match o {
        V::Str(s) => {
            if let Ok(i) = name.parse::<i64>() {
                return it.get_index(o, &V::Int(i), pos);
            }
            match name {
                "length" => return Ok(V::Int(char_len(s) as i64)),
                "empty" if java => return Ok(V::Bool(s.is_empty())),
                "bytes" if java => return Ok(V::Bytes(Rc::new(s.as_bytes().to_vec()))),
                "class" => return Ok(V::host(Host::Type("java.lang.String".into()))),
                _ => {}
            }
        }
        V::Bytes(b) => {
            if name == "length" || name == "size" {
                return Ok(V::Int(b.len() as i64));
            }
        }
        V::Arr(a) => {
            if let Ok(i) = name.parse::<i64>() {
                return it.get_index(o, &V::Int(i), pos);
            }
            match name {
                "length" => return Ok(V::Int(a.borrow().len() as i64)),
                "size" if it.d.is_java() => return Ok(V::Int(a.borrow().len() as i64)),
                "empty" if java => return Ok(V::Bool(a.borrow().is_empty())),
                "class" => return Ok(V::host(Host::Type("java.util.ArrayList".into()))),
                _ => {}
            }
            if it.d == Dialect::Groovy || it.d == Dialect::Mvel {
                // GPath: property of every element
                let items = a.borrow().clone();
                let mut out = Vec::new();
                for x in items {
                    if x.is_nullish() {
                        continue;
                    }
                    out.push(it.get_member(&x, name, pos)?);
                }
                return Ok(V::arr(out));
            }
        }
        V::Regex(r) => match name {
            "source" => return Ok(V::string(r.source.clone())),
            "flags" => return Ok(V::string(r.flags.clone())),
            "global" => return Ok(V::Bool(r.flags.contains('g'))),
            "ignoreCase" => return Ok(V::Bool(r.flags.contains('i'))),
            "multiline" => return Ok(V::Bool(r.flags.contains('m'))),
            "lastIndex" => return Ok(V::Int(0)),
            "pattern" => return Ok(V::string(r.source.clone())),
            _ => {}
        },
        _ => {
            if name == "class" {
                return Ok(V::host(Host::Type(java_type(o))));
            }
        }
    }
    match it.d {
        Dialect::Js => Ok(V::Undef),
        Dialect::Python => it.throw("AttributeError", format!("'{}' object has no attribute '{}'", py_type_name(o), name), Some(pos)),
        Dialect::Groovy => it.throw("MissingPropertyException", format!("No such property: {} for class: {}", name, java_type(o)), Some(pos)),
        Dialect::Spel => it.throw("SpelEvaluationException", format!("EL1008E: Property or field '{}' cannot be found on object of type '{}'", name, java_type(o)), Some(pos)),
        Dialect::Jsonnet => it.throw("RUNTIME ERROR", format!("unexpected type {}, expected object", jsonnet_type(o)), Some(pos)),
        _ => it.throw("PropertyAccessException", format!("could not access property '{}' of {}", name, java_type(o)), Some(pos)),
    }
}

/// Missing property of an object.
pub fn object_property(it: &mut Interp, o: &V, name: &str, _pos: u32) -> R<V> {
    let ob = match o {
        V::Obj(ob) => ob.clone(),
        _ => return Ok(it.undef()),
    };
    let kind = ob.borrow().kind.clone();
    match &kind {
        ObjKind::Coll(c) => {
            if let Some(v) = it.coll_property(c, name) {
                return Ok(v);
            }
            if name.starts_with(SYM_PREFIX) && name == it.well_known_key("iterator") {
                return Ok(V::Method(Rc::new((o.clone(), "%self".to_string()))));
            }
        }
        ObjKind::Generator(_) => {
            if name.starts_with(SYM_PREFIX) && (name == it.well_known_key("iterator") || name == it.well_known_key("asyncIterator")) {
                return Ok(V::Method(Rc::new((o.clone(), "%self".to_string()))));
            }
            if it.d == Dialect::Js && matches!(name, "next" | "return" | "throw") {
                return Ok(V::Method(Rc::new((o.clone(), name.to_string()))));
            }
        }
        ObjKind::Promise(_) if it.d == Dialect::Js && matches!(name, "then" | "catch" | "finally") => {
            return Ok(V::Method(Rc::new((o.clone(), name.to_string()))));
        }
        ObjKind::Symbol(_, desc) => {
            if name == "description" {
                return Ok(V::str(desc));
            }
        }
        ObjKind::Optional if it.d == Dialect::Java => {}
        _ => {}
    }
    if it.d == Dialect::Js && name.starts_with(SYM_PREFIX) && name == it.well_known_key("iterator") {
        return Ok(V::Undef);
    }
    if let ObjKind::Date(ms) = kind {
        if name == "time" {
            return Ok(V::Int(ms as i64));
        }
    }
    if kind == ObjKind::Error && (name == "localizedMessage" || name == "cause") {
        return Ok(if name == "cause" { V::Null } else { ob.borrow().props.get("message").cloned().unwrap_or(V::Null) });
    }
    if ob.borrow().props.contains("%sb") && name == "length" {
        let s = it.display(o);
        return Ok(V::Int(char_len(&s) as i64));
    }
    if it.d.is_java() || it.d == Dialect::Groovy {
        let len = ob.borrow().props.len();
        match name {
            "size" if it.d.is_java() => return Ok(V::Int(len as i64)),
            "empty" if it.d.is_java() => return Ok(V::Bool(len == 0)),
            "keySet" if it.d.is_java() => return Ok(V::arr(ob.borrow().props.keys().map(|k| V::Str(k.clone())).collect())),
            "values" if it.d.is_java() => return Ok(V::arr(ob.borrow().props.values().cloned().collect())),
            "class" => return Ok(V::host(Host::Type(java_type(o)))),
            _ => {}
        }
    }
    if it.d == Dialect::Js && name == "constructor" {
        return Ok(V::host(Host::Ns("Object")));
    }
    if it.d == Dialect::Jsonnet {
        return it.throw("RUNTIME ERROR", format!("field does not exist: {}", name), Some(_pos));
    }
    Ok(it.undef())
}

// ====================================================================== methods

fn not_found(it: &mut Interp, recv: &V, name: &str, args: &[V], pos: u32) -> R<V> {
    match it.d {
        Dialect::Js => it.throw("TypeError", format!("{}.{} is not a function", it.display_short(recv), name), Some(pos)),
        Dialect::Python => it.throw("AttributeError", format!("'{}' object has no attribute '{}'", py_type_name(recv), name), Some(pos)),
        Dialect::Groovy => {
            let types = args.iter().map(java_type).collect::<Vec<_>>().join(", ");
            it.throw("MissingMethodException", format!("No signature of method: {}.{}() is applicable for argument types: ({})", java_type(recv), name, types), Some(pos))
        }
        Dialect::Spel => it.throw("SpelEvaluationException", format!("EL1004E: Method call: Method {}() cannot be found on type {}", name, java_type(recv)), Some(pos)),
        Dialect::Ognl => it.throw("MethodFailedException", format!("Method \"{}\" failed for object {}", name, it.display(recv)), Some(pos)),
        Dialect::Mvel => it.throw("PropertyAccessException", format!("unable to resolve method: {}.{}()", java_type(recv), name), Some(pos)),
        Dialect::Jsonnet => it.throw("RUNTIME ERROR", format!("no method {}", name), Some(pos)),
        Dialect::Java => {
            let types = args.iter().map(java_type).collect::<Vec<_>>().join(", ");
            it.throw("CompilationException", format!("cannot find symbol: method {}({}) in {}", name, types, java_type(recv)), Some(pos))
        }
    }
}

/// The `name` of an error value.
pub fn error_name(e: &V) -> String {
    match e {
        V::Obj(o) => match o.borrow().props.get("name") {
            Some(V::Str(s)) => s.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// Built-in method call on a value.
pub fn call_method(it: &mut Interp, recv: &V, name: &str, args: Vec<V>, pos: u32) -> R<V> {
    if name.starts_with('%') {
        if let Some(v) = it.promise_internal(recv, name, &args)? {
            return Ok(v);
        }
        if let Some(m) = name.strip_prefix("%mref:") {
            return crate::java::method_ref_call(it, recv, m, args, pos);
        }
        if let Some(m) = name.strip_prefix("%super:") {
            return it.super_builtin(recv, m, &args, pos);
        }
        if name == "%self" {
            return Ok(recv.clone());
        }
        if name == "%iter" {
            let items = it.iterate(recv, Some(pos))?;
            return Ok(crate::collections::list_iterator(items));
        }
        if let Some(v) = crate::jsext::internal_method(it, recv, name, &args, pos)? {
            return Ok(v);
        }
        if it.d == Dialect::Java {
            if let Some(v) = it.java_internal(recv, name, &args, pos)? {
                return Ok(v);
            }
        }
        if name == "%groovy_use" {
            return it.groovy_use(&args, pos);
        }
        if name.starts_with("%tpl") {
            if let Some(v) = it.mvel_template_fn(name, &args, pos)? {
                return Ok(v);
            }
        }
        if it.d == Dialect::Python {
            if let Some(v) = it.py_internal(recv, name, &args, pos)? {
                return Ok(v);
            }
            if let (V::Arr(a), Some(m)) = (recv, name.strip_prefix("%nt")) {
                if let Some(v) = it.namedtuple_method(a, m, &args, pos)? {
                    return Ok(v);
                }
            }
        }
    }
    if let V::Obj(o) = recv {
        if crate::jsext::is_ext(o) {
            if let Some(v) = crate::jsext::call_method(it, recv, name, &args, pos)? {
                return Ok(v);
            }
        }
        let kind = o.borrow().kind.clone();
        let r = match &kind {
            ObjKind::Coll(c) => it.coll_method(c, recv, name, &args, pos)?,
            ObjKind::Promise(_) => it.promise_method(recv, name, &args)?,
            ObjKind::Generator(g) => it.generator_method(g, recv, name, &args, pos)?,
            ObjKind::Symbol(_, desc) => match name {
                "toString" => Some(V::string(format!("Symbol({})", desc))),
                "valueOf" => Some(recv.clone()),
                _ => None,
            },
            ObjKind::Optional => crate::java::optional_method(it, o, recv, name, &args, pos)?,
            ObjKind::JTime(t) => it.jtime_method(t, name, &args, pos)?,
            ObjKind::Jsonnet(_) => None,
            _ => None,
        };
        if let Some(v) = r {
            return Ok(v);
        }
        if !it.jsorted.is_empty() && it.sorted_info(o).is_some() {
            // TreeMap / TreeSet: navigation methods, other methods keep the order
            let o = o.clone();
            if let Some(v) = it.sorted_method(&o, recv, name, &args, pos)? {
                return Ok(v);
            }
            let key = Rc::as_ptr(&o) as *const u8 as usize;
            let entry = it.jsorted.remove(&key);
            let r = call_method(it, recv, name, args, pos);
            if let Some(e) = entry {
                it.jsorted.insert(key, e);
            }
            it.resort(&o, pos)?;
            return r;
        }
    }
    if it.d == Dialect::Java {
        if let Some(v) = crate::java::java_method(it, recv, name, &args, pos)? {
            return Ok(v);
        }
    }
    if name == "%bound" {
        if let V::Obj(o) = recv {
            let (f, this, pre) = {
                let b = o.borrow();
                (b.props.get("fn").cloned().unwrap_or(V::Undef), b.props.get("this").cloned().unwrap_or(V::Undef), b.props.get("args").cloned().unwrap_or(V::Undef))
            };
            let mut a = match pre {
                V::Arr(p) => p.borrow().clone(),
                _ => vec![],
            };
            a.extend(args);
            return it.call(&f, this, a, pos);
        }
    }
    let r = match recv {
        V::Str(s) => string_method(it, s.clone(), name, &args, pos)?,
        V::Bytes(b) => match name {
            "decode" => Some(V::string(String::from_utf8_lossy(b).into_owned())),
            "size" | "length" => Some(V::Int(b.len() as i64)),
            _ => {
                let s: Rc<str> = Rc::from(String::from_utf8_lossy(b).as_ref());
                string_method(it, s, name, &args, pos)?
            }
        },
        V::Arr(a) if it.d == Dialect::Python && name.starts_with('_') && it.namedtuple_method(a, name, &args, pos)?.is_some() => it.namedtuple_method(a, name, &args, pos)?,
        V::Arr(a) => array_method(it, a.clone(), recv, name, args.clone(), pos)?,
        V::Obj(o) => {
            let kind = o.borrow().kind.clone();
            match kind {
                ObjKind::Date(ms) => date_method(it, ms, recv, name, &args)?,
                ObjKind::Set => set_method(it, o.clone(), recv, name, args.clone(), pos)?,
                ObjKind::Error => match name {
                    "getMessage" | "getLocalizedMessage" => Some(o.borrow().props.get("message").cloned().unwrap_or(V::Null)),
                    "getCause" => Some(V::Null),
                    "printStackTrace" => Some(V::Null),
                    _ => None,
                },
                ObjKind::Plain if it.d == Dialect::Python && !it.py_dicts.is_empty() => match it.py_dict_method(o, recv, name, &args, pos)? {
                    Some(v) => Some(v),
                    None => object_method(it, o.clone(), recv, name, args.clone(), pos)?,
                },
                _ => object_method(it, o.clone(), recv, name, args.clone(), pos)?,
            }
        }
        V::Int(_) | V::Num(_) => number_method(it, recv, name, &args, pos)?,
        V::Bool(b) => match name {
            "booleanValue" => Some(V::Bool(*b)),
            _ => None,
        },
        V::Regex(r) => regex_method(it, r, name, &args, pos)?,
        V::Func(_) => match name {
            "memoize" | "trampoline" | "dehydrate" | "rehydrate" => Some(recv.clone()),
            "curry" => {
                let mut p = PropMap::default();
                p.set("fn", recv.clone());
                p.set("this", V::Undef);
                p.set("args", V::arr(args.clone()));
                Some(V::Method(Rc::new((V::obj(p), "%bound".to_string()))))
            }
            _ => None,
        },
        _ => None,
    };
    if let Some(v) = r {
        return Ok(v);
    }
    // generic methods
    Ok(match name {
        "toString" | "__str__" => V::string(it.display(recv)),
        "equals" => V::Bool(deep_eq(recv, &arg(&args, 0))),
        "hashCode" => V::Int(it.display(recv).bytes().fold(0i64, |h, b| h.wrapping_mul(31).wrapping_add(b as i64)) as i32 as i64),
        "getClass" => V::host(Host::Type(java_type(recv))),
        "is" => V::Bool(recv.same(&arg(&args, 0))),
        "asBoolean" => V::Bool(it.truthy(recv)),
        "valueOf" => recv.clone(),
        "inspect" => V::string(py_repr(it, recv)),
        "asType" => {
            let t = match args.first() {
                Some(V::Host(h)) => match &**h {
                    Host::Ns(n) => n.to_string(),
                    Host::Type(n) => n.clone(),
                    _ => String::new(),
                },
                Some(other) => it.display(other),
                None => String::new(),
            };
            as_type(it, recv.clone(), &t, pos)?
        }
        "with" | "tap" | "also" if it.d == Dialect::Groovy => {
            let f = arg(&args, 0);
            let r = it.call(&f, recv.clone(), vec![recv.clone()], pos)?;
            if name == "with" {
                r
            } else {
                recv.clone()
            }
        }
        "getProperty" | "getAt" => {
            let k = arg(&args, 0);
            it.get_index(recv, &k, pos)?
        }
        "isCase" => {
            let x = arg(&args, 0);
            let v = it.contains(recv, &x, pos).unwrap_or(false) || it.equals(recv, &x);
            V::Bool(v)
        }
        _ => return not_found(it, recv, name, &args, pos),
    })
}

fn string_method(it: &mut Interp, s: Rc<str>, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
    let d = it.d;
    let a = |i: usize| arg(args, i);
    let sa = |it: &Interp, i: usize| sarg(it, args, i);
    let len = char_len(&s);
    let js = d == Dialect::Js;
    Ok(Some(match name {
        "length" | "size" | "__len__" => V::Int(len as i64),
        "isEmpty" => V::Bool(s.is_empty()),
        "isBlank" => V::Bool(s.trim().is_empty()),
        "toUpperCase" | "upper" | "toLocaleUpperCase" => V::string(s.to_uppercase()),
        "toLowerCase" | "lower" | "toLocaleLowerCase" => V::string(s.to_lowercase()),
        "trim" => V::string(s.trim().to_string()),
        "strip" => {
            if let Some(V::Str(chars)) = args.first() {
                let cs: Vec<char> = chars.chars().collect();
                V::string(s.trim_matches(|c| cs.contains(&c)).to_string())
            } else {
                V::string(s.trim().to_string())
            }
        }
        "trimStart" | "trimLeft" | "lstrip" | "stripLeading" => {
            if let Some(V::Str(chars)) = args.first() {
                let cs: Vec<char> = chars.chars().collect();
                V::string(s.trim_start_matches(|c| cs.contains(&c)).to_string())
            } else {
                V::string(s.trim_start().to_string())
            }
        }
        "trimEnd" | "trimRight" | "rstrip" | "stripTrailing" => {
            if let Some(V::Str(chars)) = args.first() {
                let cs: Vec<char> = chars.chars().collect();
                V::string(s.trim_end_matches(|c| cs.contains(&c)).to_string())
            } else {
                V::string(s.trim_end().to_string())
            }
        }
        "split" => {
            let parts: Vec<String> = match (d, args.first()) {
                (_, Some(V::Regex(r))) => r.re.split_limit(&s, -1),
                (Dialect::Js, None | Some(V::Undef)) => vec![s.to_string()],
                (Dialect::Js, Some(sep)) => {
                    let sep = it.display(sep);
                    if sep.is_empty() {
                        s.chars().map(|c| c.to_string()).collect()
                    } else {
                        s.split(sep.as_str()).map(|x| x.to_string()).collect()
                    }
                }
                (Dialect::Python | Dialect::Jsonnet, None | Some(V::Null)) => {
                    let max = iarg(it, args, 1, -1);
                    if max < 0 {
                        s.split_whitespace().map(|x| x.to_string()).collect()
                    } else {
                        let mut out = Vec::new();
                        let mut rest = s.trim_start();
                        while !rest.is_empty() && (out.len() as i64) < max {
                            match rest.find(char::is_whitespace) {
                                Some(i) => {
                                    out.push(rest[..i].to_string());
                                    rest = rest[i..].trim_start();
                                }
                                None => {
                                    out.push(rest.to_string());
                                    rest = "";
                                }
                            }
                        }
                        if !rest.is_empty() {
                            out.push(rest.to_string());
                        }
                        out
                    }
                }
                (Dialect::Python | Dialect::Jsonnet, Some(sep)) => {
                    let sep = it.display(sep);
                    if sep.is_empty() {
                        return it.throw("ValueError", "empty separator".into(), Some(pos)).map(Some);
                    }
                    let max = iarg(it, args, 1, -1);
                    if max < 0 {
                        s.split(sep.as_str()).map(|x| x.to_string()).collect()
                    } else {
                        s.splitn(max as usize + 1, sep.as_str()).map(|x| x.to_string()).collect()
                    }
                }
                (_, None) => s.split_whitespace().map(|x| x.to_string()).collect(),
                (_, Some(sep)) => {
                    // Java String.split(regex[, limit])
                    let p = it.display(sep);
                    let re = to_regex(it, &V::string(p), pos)?;
                    re.re.split_limit(&s, iarg(it, args, 1, 0) as i32)
                }
            };
            let mut parts = parts;
            if js {
                if let Some(lim) = args.get(1).filter(|v| !v.is_nullish()) {
                    parts.truncate(farg(it, std::slice::from_ref(lim), 0) as usize);
                }
            }
            V::arr(parts.into_iter().map(V::string).collect())
        }
        "tokenize" => {
            let delims = if args.is_empty() { " \t\n\r\x0c".to_string() } else { sa(it, 0) };
            V::arr(s.split(|c| delims.contains(c)).filter(|x| !x.is_empty()).map(V::str).collect())
        }
        "splitlines" | "readLines" | "lines" => V::arr(s.lines().map(V::str).collect()),
        "indexOf" | "find" if !(name == "find" && d == Dialect::Groovy) => {
            let sub = sa(it, 0);
            let from = rel(farg(it, args, 1).max(0.0), len);
            let from = if args.len() > 1 { from } else { 0 };
            let start_b = char_to_byte(&s, from);
            V::Int(s[start_b..].find(sub.as_str()).map(|b| byte_to_char(&s, start_b + b) as i64).unwrap_or(-1))
        }
        "index" if d == Dialect::Python => {
            let sub = sa(it, 0);
            match s.find(sub.as_str()) {
                Some(b) => V::Int(byte_to_char(&s, b) as i64),
                None => return it.throw("ValueError", "substring not found".into(), Some(pos)).map(Some),
            }
        }
        "lastIndexOf" | "rfind" => {
            let sub = sa(it, 0);
            V::Int(s.rfind(sub.as_str()).map(|b| byte_to_char(&s, b) as i64).unwrap_or(-1))
        }
        "find" => {
            // Groovy: first regex match
            let re = to_regex(it, &a(0), pos)?;
            match re.re.find(&s) {
                Some((x, y)) => {
                    let m = V::string(s[x..y].to_string());
                    if let Some(f) = args.get(1).filter(|f| f.is_callable()) {
                        let f = f.clone();
                        return it.call(&f, V::Undef, vec![m], pos).map(Some);
                    }
                    m
                }
                None => V::Null,
            }
        }
        "findAll" if d == Dialect::Groovy => {
            let re = to_regex(it, &a(0), pos)?;
            V::arr(re.re.find_all(&s).into_iter().map(V::string).collect())
        }
        "count" => {
            let sub = sa(it, 0);
            if sub.is_empty() {
                V::Int(len as i64 + 1)
            } else {
                V::Int(s.matches(sub.as_str()).count() as i64)
            }
        }
        "contains" | "includes" | "__contains__" => {
            let sub = sa(it, 0);
            V::Bool(s.contains(sub.as_str()))
        }
        "startsWith" | "startswith" => {
            let p = match a(0) {
                V::Arr(items) => {
                    let items = items.borrow().clone();
                    return Ok(Some(V::Bool(items.iter().any(|x| s.starts_with(it.display(x).as_str())))));
                }
                other => it.display(&other),
            };
            let from = if args.len() > 1 { char_to_byte(&s, iarg(it, args, 1, 0).max(0) as usize) } else { 0 };
            V::Bool(s[from..].starts_with(p.as_str()))
        }
        "endsWith" | "endswith" => {
            let p = match a(0) {
                V::Arr(items) => {
                    let items = items.borrow().clone();
                    return Ok(Some(V::Bool(items.iter().any(|x| s.ends_with(it.display(x).as_str())))));
                }
                other => it.display(&other),
            };
            V::Bool(s.ends_with(p.as_str()))
        }
        "substring" => {
            let mut x = farg(it, args, 0);
            let mut y = if args.len() > 1 && !a(1).is_nullish() { farg(it, args, 1) } else { len as f64 };
            if js {
                x = x.max(0.0).min(len as f64);
                y = y.max(0.0).min(len as f64);
                if x > y {
                    std::mem::swap(&mut x, &mut y);
                }
            } else if x < 0.0 || y > len as f64 || x > y {
                return it.throw("StringIndexOutOfBoundsException", format!("begin {}, end {}, length {}", x as i64, y as i64, len), Some(pos)).map(Some);
            }
            V::string(char_slice(&s, x as usize, y as usize))
        }
        "substr" => {
            let x = rel(farg(it, args, 0), len);
            let n = if args.len() > 1 { farg(it, args, 1).max(0.0) as usize } else { len };
            V::string(char_slice(&s, x, (x + n).min(len)))
        }
        "slice" => {
            let x = rel(if args.is_empty() { 0.0 } else { farg(it, args, 0) }, len);
            let y = if args.len() > 1 && !a(1).is_nullish() { rel(farg(it, args, 1), len) } else { len };
            V::string(char_slice(&s, x, y))
        }
        "charAt" | "getAt" => {
            if let (V::Arr(_), "getAt") = (&a(0), name) {
                let idx = a(0);
                return it.get_index(&V::Str(s.clone()), &idx, pos).map(Some);
            }
            let i = iarg(it, args, 0, 0);
            let c = if i < 0 { None } else { s.chars().nth(i as usize) };
            match (c, d) {
                (Some(c), _) => V::string(c.to_string()),
                (None, Dialect::Js) => V::str(""),
                (None, _) => return it.throw("StringIndexOutOfBoundsException", format!("index {}, length {}", i, len), Some(pos)).map(Some),
            }
        }
        "at" => {
            let i = iarg(it, args, 0, 0);
            let i = if i < 0 { i + len as i64 } else { i };
            s.chars().nth(i.max(0) as usize).filter(|_| i >= 0).map(|c| V::string(c.to_string())).unwrap_or(V::Undef)
        }
        "charCodeAt" | "codePointAt" => {
            let i = iarg(it, args, 0, 0);
            s.chars().nth(i.max(0) as usize).map(|c| V::Int(c as i64)).unwrap_or(V::Num(f64::NAN))
        }
        "replace" => {
            match (d, &a(0)) {
                (_, V::Regex(r)) => {
                    let r = r.clone();
                    let rep = a(1);
                    let global = r.flags.contains('g');
                    if rep.is_callable() {
                        V::string(regex_replace_fn(it, &r, &s, &rep, global, pos)?)
                    } else {
                        let rp = js_replacement(&it.display(&rep));
                        V::string(if global { r.re.replace_all(&s, &rp) } else { r.re.replace_first(&s, &rp) })
                    }
                }
                (Dialect::Js, pat) => {
                    let p = it.display(pat);
                    let rep = a(1);
                    match s.find(p.as_str()) {
                        Some(b) => {
                            let r = if rep.is_callable() {
                                let v = it.call(&rep, V::Undef, vec![V::string(p.clone()), V::Int(byte_to_char(&s, b) as i64), V::Str(s.clone())], pos)?;
                                it.display(&v)
                            } else {
                                it.display(&rep).replace("$&", &p)
                            };
                            V::string(format!("{}{}{}", &s[..b], r, &s[b + p.len()..]))
                        }
                        None => V::Str(s.clone()),
                    }
                }
                (Dialect::Python, pat) => {
                    let p = it.display(pat);
                    let r = sa(it, 1);
                    let count = iarg(it, args, 2, -1);
                    if count < 0 {
                        V::string(s.replace(p.as_str(), &r))
                    } else {
                        V::string(s.replacen(p.as_str(), &r, count as usize))
                    }
                }
                (_, pat) => {
                    let p = it.display(pat);
                    let r = sa(it, 1);
                    V::string(s.replace(p.as_str(), &r))
                }
            }
        }
        "replaceAll" => {
            let rep = a(1);
            match (&a(0), d) {
                (V::Regex(r), _) => {
                    let r = r.clone();
                    if rep.is_callable() {
                        V::string(regex_replace_fn(it, &r, &s, &rep, true, pos)?)
                    } else {
                        V::string(r.re.replace_all(&s, &js_replacement(&it.display(&rep))))
                    }
                }
                (pat, Dialect::Js) => {
                    let p = it.display(pat);
                    V::string(s.replace(p.as_str(), &it.display(&rep)))
                }
                (pat, _) => {
                    let re = to_regex(it, pat, pos)?;
                    if rep.is_callable() {
                        V::string(regex_replace_fn(it, &re, &s, &rep, true, pos)?)
                    } else {
                        V::string(re.re.replace_all(&s, &it.display(&rep)))
                    }
                }
            }
        }
        "replaceFirst" => {
            let re = to_regex(it, &a(0), pos)?;
            V::string(re.re.replace_first(&s, &sa(it, 1)))
        }
        "match" if js => {
            let re = to_regex(it, &a(0), pos)?;
            if re.flags.contains('g') {
                let all = re.re.find_all(&s);
                if all.is_empty() {
                    V::Null
                } else {
                    V::arr(all.into_iter().map(V::string).collect())
                }
            } else {
                match re.re.captures_match(&s) {
                    Some(c) => crate::jsext::js_match(it, &re.re, &c, &s),
                    None => V::Null,
                }
            }
        }
        "matchAll" => {
            let re = to_regex(it, &a(0), pos)?;
            let caps = re.re.captures_iter(&s);
            let mut out = vec![];
            for c in &caps {
                out.push(if js { crate::jsext::js_match(it, &re.re, c, &s) } else { V::arr(c.to_vec().into_iter().map(|g| g.map(V::string).unwrap_or(V::Undef)).collect()) });
            }
            if js {
                crate::collections::list_iterator(out)
            } else {
                V::arr(out)
            }
        }
        "search" => {
            let re = to_regex(it, &a(0), pos)?;
            V::Int(re.re.find(&s).map(|(x, _)| byte_to_char(&s, x) as i64).unwrap_or(-1))
        }
        "matches" => {
            let re = to_regex(it, &a(0), pos)?;
            V::Bool(re.re.is_full_match(&s))
        }
        "concat" | "plus" => {
            let mut out = s.to_string();
            for x in args {
                out.push_str(&it.display(x));
            }
            V::string(out)
        }
        "repeat" | "multiply" => V::string(s.repeat(iarg(it, args, 0, 0).max(0) as usize)),
        "padStart" | "padLeft" | "rjust" => {
            let w = iarg(it, args, 0, 0).max(0) as usize;
            let fill = if args.len() > 1 { sa(it, 1) } else { " ".into() };
            if len >= w || fill.is_empty() {
                V::Str(s.clone())
            } else {
                let need = w - len;
                let pad: String = fill.chars().cycle().take(need).collect();
                V::string(format!("{}{}", pad, s))
            }
        }
        "padEnd" | "padRight" | "ljust" => {
            let w = iarg(it, args, 0, 0).max(0) as usize;
            let fill = if args.len() > 1 { sa(it, 1) } else { " ".into() };
            if len >= w || fill.is_empty() {
                V::Str(s.clone())
            } else {
                let need = w - len;
                let pad: String = fill.chars().cycle().take(need).collect();
                V::string(format!("{}{}", s, pad))
            }
        }
        "center" => {
            let w = iarg(it, args, 0, 0).max(0) as usize;
            let fill = if args.len() > 1 { sa(it, 1) } else { " ".into() };
            if len >= w {
                V::Str(s.clone())
            } else {
                let total = w - len;
                let left = if d == Dialect::Python { total / 2 + (total & w & 1) } else { total / 2 };
                let right = total - left;
                let f: String = fill.chars().take(1).collect();
                V::string(format!("{}{}{}", f.repeat(left), s, f.repeat(right)))
            }
        }
        "zfill" => {
            let w = iarg(it, args, 0, 0).max(0) as usize;
            if len >= w {
                V::Str(s.clone())
            } else {
                let (sign, rest) = if s.starts_with('-') || s.starts_with('+') { s.split_at(1) } else { ("", &*s) };
                V::string(format!("{}{}{}", sign, "0".repeat(w - len), rest))
            }
        }
        "equals" => V::Bool(matches!(a(0), V::Str(ref o) if **o == *s)),
        "equalsIgnoreCase" => V::Bool(sa(it, 0).to_lowercase() == s.to_lowercase()),
        "compareTo" | "localeCompare" => V::Int(match s.as_ref().cmp(sa(it, 0).as_str()) {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }),
        "compareToIgnoreCase" => V::Int(s.to_lowercase().cmp(&sa(it, 0).to_lowercase()) as i64),
        "reverse" => V::string(s.chars().rev().collect()),
        "capitalize" => {
            let mut c = s.chars();
            V::string(match c.next() {
                Some(f) => {
                    let rest: String = if d == Dialect::Python { c.as_str().to_lowercase() } else { c.as_str().to_string() };
                    f.to_uppercase().collect::<String>() + &rest
                }
                None => String::new(),
            })
        }
        "uncapitalize" => {
            let mut c = s.chars();
            V::string(match c.next() {
                Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
                None => String::new(),
            })
        }
        "title" => {
            let mut out = String::new();
            let mut prev_alpha = false;
            for c in s.chars() {
                if c.is_alphabetic() {
                    if prev_alpha {
                        out.extend(c.to_lowercase());
                    } else {
                        out.extend(c.to_uppercase());
                    }
                    prev_alpha = true;
                } else {
                    out.push(c);
                    prev_alpha = false;
                }
            }
            V::string(out)
        }
        "swapcase" => V::string(s.chars().map(|c| if c.is_uppercase() { c.to_lowercase().collect::<String>() } else { c.to_uppercase().collect() }).collect()),
        "toInteger" | "toLong" | "asInteger" | "toBigInteger" | "intValue" => match parse_int_str(&s, 10) {
            Some(i) => V::Int(i),
            None => return it.throw("NumberFormatException", format!("For input string: \"{}\"", s), Some(pos)).map(Some),
        },
        "toDouble" | "toFloat" | "toBigDecimal" | "doubleValue" => match s.trim().parse::<f64>() {
            Ok(f) => V::Num(f),
            Err(_) => return it.throw("NumberFormatException", format!("For input string: \"{}\"", s), Some(pos)).map(Some),
        },
        "toBoolean" => V::Bool(matches!(s.trim().to_lowercase().as_str(), "true" | "y" | "1")),
        "isNumber" | "isBigDecimal" | "isDouble" | "isFloat" => V::Bool(s.trim().parse::<f64>().is_ok()),
        "isInteger" | "isLong" | "isBigInteger" => V::Bool(parse_int_str(&s, 10).is_some()),
        "isdigit" | "isnumeric" | "isdecimal" => V::Bool(!s.is_empty() && s.chars().all(|c| c.is_ascii_digit())),
        "isalpha" => V::Bool(!s.is_empty() && s.chars().all(char::is_alphabetic)),
        "isalnum" => V::Bool(!s.is_empty() && s.chars().all(char::is_alphanumeric)),
        "isspace" => V::Bool(!s.is_empty() && s.chars().all(char::is_whitespace)),
        "isupper" => V::Bool(s.chars().any(char::is_alphabetic) && !s.chars().any(char::is_lowercase)),
        "islower" => V::Bool(s.chars().any(char::is_alphabetic) && !s.chars().any(char::is_uppercase)),
        "format" if d == Dialect::Python => {
            let mut args = args.to_vec();
            let kwa = kwargs(&mut args);
            match crate::format::py_format(it, &s, &args, kwa.as_ref()) {
                Ok(r) => V::string(r),
                Err(m) => return it.throw("ValueError", m, Some(pos)).map(Some),
            }
        }
        "format" => match crate::format::printf(it, &s, args, None) {
            Ok(r) => V::string(r),
            Err(m) => return it.throw("IllegalFormatException", m, Some(pos)).map(Some),
        },
        "join" => {
            let items = it.iterate(&a(0), Some(pos))?;
            V::string(items.iter().map(|x| it.display(x)).collect::<Vec<_>>().join(&s))
        }
        "toCharArray" | "toList" | "chars" => V::arr(s.chars().map(|c| V::string(c.to_string())).collect()),
        "encode" | "getBytes" => V::Bytes(Rc::new(s.as_bytes().to_vec())),
        "take" => V::string(s.chars().take(iarg(it, args, 0, 0).max(0) as usize).collect()),
        "drop" => V::string(s.chars().skip(iarg(it, args, 0, 0).max(0) as usize).collect()),
        "each" | "eachWithIndex" | "collect" if d == Dialect::Groovy => {
            let chars: Vec<V> = s.chars().map(|c| V::string(c.to_string())).collect();
            return array_method(it, Rc::new(RefCell::new(chars.clone())), &V::arr(chars), name, args.to_vec(), pos);
        }
        "stripIndent" => {
            let lines: Vec<&str> = s.lines().collect();
            let indent = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
            V::string(lines.iter().map(|l| if l.len() >= indent { &l[indent..] } else { l.trim_start() }).collect::<Vec<_>>().join("\n"))
        }
        "stripMargin" => {
            let m = if args.is_empty() { '|' } else { sa(it, 0).chars().next().unwrap_or('|') };
            V::string(s.lines().map(|l| { let t = l.trim_start(); t.strip_prefix(m).unwrap_or(l) }).collect::<Vec<_>>().join("\n"))
        }
        "normalize" | "intern" | "toString" | "valueOf" | "trimIndent" => V::Str(s.clone()),
        _ => return Ok(None),
    }))
}

fn number_method(it: &mut Interp, recv: &V, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
    let f = recv.as_f64().unwrap_or(f64::NAN);
    let d = it.d;
    if name == "toLocaleString" && d == Dialect::Js {
        return crate::jsext::number_to_locale(it, recv, args, pos).map(Some);
    }
    Ok(Some(match name {
        "toFixed" => {
            let n = iarg(it, args, 0, 0).clamp(0, 100) as usize;
            V::string(format!("{:.*}", n, f))
        }
        "toPrecision" => {
            let p = iarg(it, args, 0, 6).max(1) as usize;
            let e = if f == 0.0 { 0 } else { f.abs().log10().floor() as i64 };
            V::string(format!("{:.*}", (p as i64 - 1 - e).max(0) as usize, f))
        }
        "toString" if !args.is_empty() && d == Dialect::Js => {
            let radix = iarg(it, args, 0, 10) as u32;
            let mut n = f as i64;
            if radix == 10 || !(2..=36).contains(&radix) {
                V::string(js_num_str(f))
            } else {
                let neg = n < 0;
                n = n.abs();
                let mut digits = Vec::new();
                loop {
                    digits.push(std::char::from_digit((n % radix as i64) as u32, radix).unwrap());
                    n /= radix as i64;
                    if n == 0 {
                        break;
                    }
                }
                if neg {
                    digits.push('-');
                }
                V::string(digits.iter().rev().collect())
            }
        }
        "toLocaleString" => {
            let s = if recv.is_number() && f.fract() == 0.0 { format!("{}", f as i64) } else { format!("{:.3}", f).trim_end_matches('0').trim_end_matches('.').to_string() };
            let (ip, fp) = s.split_once('.').map(|(a, b)| (a.to_string(), format!(".{}", b))).unwrap_or((s.clone(), String::new()));
            let neg = ip.starts_with('-');
            let digits = ip.trim_start_matches('-');
            let mut out = String::new();
            for (i, c) in digits.chars().enumerate() {
                if i > 0 && (digits.len() - i) % 3 == 0 {
                    out.push(',');
                }
                out.push(c);
            }
            V::string(format!("{}{}{}", if neg { "-" } else { "" }, out, fp))
        }
        "intValue" | "longValue" | "shortValue" | "toInteger" | "toLong" | "asInteger" | "toBigInteger" | "byteValue" => V::Int(f.trunc() as i64),
        "doubleValue" | "floatValue" | "toDouble" | "toFloat" | "toBigDecimal" => V::Num(f),
        "times" => {
            let cb = arg(args, 0);
            for i in 0..(f as i64).max(0) {
                it.call(&cb, V::Undef, vec![V::Int(i)], pos)?;
            }
            V::Null
        }
        "upto" | "downto" => {
            let to = iarg(it, args, 0, 0);
            let cb = arg(args, 1);
            let from = f as i64;
            if name == "upto" {
                for i in from..=to {
                    it.call(&cb, V::Undef, vec![V::Int(i)], pos)?;
                }
            } else {
                let mut i = from;
                while i >= to {
                    it.call(&cb, V::Undef, vec![V::Int(i)], pos)?;
                    i -= 1;
                }
            }
            V::Null
        }
        "step" => {
            let to = farg(it, args, 0);
            let st = farg(it, args, 1);
            let cb = arg(args, 2);
            let mut i = f;
            while (st > 0.0 && i < to) || (st < 0.0 && i > to) {
                it.call(&cb, V::Undef, vec![it.mk_num(i)], pos)?;
                i += st;
            }
            V::Null
        }
        "abs" => match recv {
            V::Int(i) => V::Int(i.abs()),
            _ => V::Num(f.abs()),
        },
        "round" => {
            if args.is_empty() {
                V::Int((f + 0.5).floor() as i64)
            } else {
                let n = iarg(it, args, 0, 0);
                let m = 10f64.powi(n as i32);
                V::Num((f * m).round() / m)
            }
        }
        "floor" => V::Int(f.floor() as i64),
        "ceil" => V::Int(f.ceil() as i64),
        "intdiv" => {
            let y = iarg(it, args, 0, 1);
            if y == 0 {
                return it.throw("ArithmeticException", "Division by zero".into(), Some(pos)).map(Some);
            }
            V::Int((f as i64) / y)
        }
        "mod" => it.binary(BinOp::Mod, recv.clone(), arg(args, 0), pos)?,
        "power" => it.binary(BinOp::Pow, recv.clone(), arg(args, 0), pos)?,
        "plus" => it.binary(BinOp::Add, recv.clone(), arg(args, 0), pos)?,
        "minus" => it.binary(BinOp::Sub, recv.clone(), arg(args, 0), pos)?,
        "multiply" => it.binary(BinOp::Mul, recv.clone(), arg(args, 0), pos)?,
        "div" => it.binary(BinOp::Div, recv.clone(), arg(args, 0), pos)?,
        "compareTo" => V::Int(natural(it, recv, &arg(args, 0)) as i64),
        "is_integer" => V::Bool(f.fract() == 0.0),
        "isNaN" => V::Bool(f.is_nan()),
        "isInfinite" => V::Bool(f.is_infinite()),
        "equals" => V::Bool(crate::interp::deep_eq(recv, &arg(args, 0)) && std::mem::discriminant(recv) == std::mem::discriminant(&arg(args, 0))),
        "toBoolean" => V::Bool(f != 0.0),
        _ => return Ok(None),
    }))
}

fn set_method(it: &mut Interp, o: ObjRef, recv: &V, name: &str, args: Vec<V>, pos: u32) -> R<Option<V>> {
    let x = arg(&args, 0);
    match name {
        "add" => {
            let k = set_key(it, &x);
            let added = !o.borrow().props.contains(&k);
            o.borrow_mut().props.set(&k, x);
            return Ok(Some(if it.d == Dialect::Python { V::Null } else { V::Bool(added) }));
        }
        "remove" | "discard" | "delete" => {
            let k = set_key(it, &x);
            let had = o.borrow_mut().props.remove(&k).is_some();
            if !had && name == "remove" && it.d == Dialect::Python {
                return it.throw("KeyError", py_repr(it, &x), Some(pos)).map(Some);
            }
            return Ok(Some(if it.d == Dialect::Python { V::Null } else { V::Bool(had) }));
        }
        "contains" | "has" | "__contains__" => {
            let k = set_key(it, &x);
            return Ok(Some(V::Bool(o.borrow().props.contains(&k))));
        }
        "addAll" if it.d != Dialect::Python => {
            let mut changed = false;
            for v in it.clone_iter(&x)? {
                let k = set_key(it, &v);
                changed |= !o.borrow().props.contains(&k);
                o.borrow_mut().props.set(&k, v);
            }
            return Ok(Some(V::Bool(changed)));
        }
        "removeAll" | "retainAll" if it.d != Dialect::Python => {
            let other: Vec<String> = it.clone_iter(&x)?.iter().map(|v| set_key(it, v)).collect();
            let keys: Vec<Rc<str>> = o.borrow().props.keys().cloned().collect();
            let mut changed = false;
            for k in keys {
                let present = other.iter().any(|x| **x == *k);
                if (name == "removeAll") == present {
                    o.borrow_mut().props.remove(&k);
                    changed = true;
                }
            }
            return Ok(Some(V::Bool(changed)));
        }
        "containsAll" if it.d != Dialect::Python => {
            let all = it.clone_iter(&x)?.iter().all(|v| o.borrow().props.contains(&set_key(it, v)));
            return Ok(Some(V::Bool(all)));
        }
        "size" | "__len__" => return Ok(Some(V::Int(o.borrow().props.len() as i64))),
        "isEmpty" => return Ok(Some(V::Bool(o.borrow().props.is_empty()))),
        "clear" => {
            o.borrow_mut().props.clear();
            return Ok(Some(V::Null));
        }
        "union" | "intersection" | "difference" | "symmetric_difference" => {
            let xs = it.clone_iter(&x)?;
            let other = make_set(it, xs);
            let op = match name {
                "union" => BinOp::BitOr,
                "intersection" => BinOp::BitAnd,
                "difference" => {
                    let ys = set_items(&other);
                    let out: Vec<V> = set_items(recv).into_iter().filter(|v| !ys.iter().any(|w| it.equals(v, w))).collect();
                    return Ok(Some(make_set(it, out)));
                }
                _ => BinOp::BitXor,
            };
            return set_op(it, op, recv, &other).map(Some);
        }
        "update" => {
            for v in it.clone_iter(&x)? {
                let k = set_key(it, &v);
                o.borrow_mut().props.set(&k, v);
            }
            return Ok(Some(V::Null));
        }
        _ => {}
    }
    // everything else: as a list
    let items = set_items(recv);
    let list = V::arr(items.clone());
    let r = array_method(it, Rc::new(RefCell::new(items)), &list, name, args, pos)?;
    Ok(r.map(|v| match (&v, name) {
        (V::Arr(a), "findAll" | "filter" | "collect" | "plus" | "minus") if name != "collect" => {
            let items = a.borrow().clone();
            make_set(it, items)
        }
        _ => v,
    }))
}

impl Interp {
    /// Iterate into a vector (helper for builtins).
    pub fn clone_iter(&mut self, v: &V) -> R<Vec<V>> {
        self.iterate(v, None)
    }
}

fn object_method(it: &mut Interp, o: ObjRef, recv: &V, name: &str, mut args: Vec<V>, pos: u32) -> R<Option<V>> {
    let d = it.d;
    // StringBuilder
    if o.borrow().props.contains("%sb") {
        let cur = o.borrow().props.get("%sb").map(|v| it.display(v)).unwrap_or_default();
        match name {
            "append" | "leftShift" => {
                let add = sarg(it, &args, 0);
                o.borrow_mut().props.set("%sb", V::string(cur + &add));
                return Ok(Some(recv.clone()));
            }
            "insert" => {
                let i = iarg(it, &args, 0, 0).max(0) as usize;
                let b = char_to_byte(&cur, i);
                let add = sarg(it, &args, 1);
                o.borrow_mut().props.set("%sb", V::string(format!("{}{}{}", &cur[..b], add, &cur[b..])));
                return Ok(Some(recv.clone()));
            }
            "reverse" => {
                o.borrow_mut().props.set("%sb", V::string(cur.chars().rev().collect()));
                return Ok(Some(recv.clone()));
            }
            "setLength" => {
                let n = iarg(it, &args, 0, 0).max(0) as usize;
                o.borrow_mut().props.set("%sb", V::string(cur.chars().take(n).collect()));
                return Ok(Some(V::Null));
            }
            "toString" => return Ok(Some(V::string(cur))),
            _ => return string_method(it, Rc::from(cur.as_str()), name, &args, pos),
        }
    }
    // Python re match objects
    let m = o.borrow().props.get("%match").cloned();
    if let Some(V::Arr(groups)) = m {
        let g = groups.borrow().clone();
        let start = o.borrow().props.get("%start").cloned().unwrap_or(V::Int(0));
        let end = o.borrow().props.get("%end").cloned().unwrap_or(V::Int(0));
        return Ok(Some(match name {
            "group" => {
                if args.len() > 1 {
                    V::arr(args.iter().map(|a| g.get(a.as_f64().unwrap_or(0.0) as usize).cloned().unwrap_or(V::Null)).collect())
                } else {
                    let i = iarg(it, &args, 0, 0) as usize;
                    match g.get(i) {
                        Some(v) => v.clone(),
                        None => return it.throw("IndexError", "no such group".into(), Some(pos)).map(Some),
                    }
                }
            }
            "groups" => V::arr(g.into_iter().skip(1).collect()),
            "start" => start,
            "end" => end,
            "span" => V::arr(vec![start, end]),
            _ => return Ok(None),
        }));
    }
    let a0 = arg(&args, 0);
    let key = |it: &Interp, v: &V| it.key_string(v);
    let get = |k: &str| o.borrow().props.get(k).cloned();
    let entries = || -> Vec<(Rc<str>, V)> { o.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect() };
    // callback with (k, v) when the closure has 2 params, else an entry
    let kv_args = |f: &V, k: &Rc<str>, v: &V| -> Vec<V> {
        if arity(f) >= 2 || d == Dialect::Js {
            vec![V::Str(k.clone()), v.clone()]
        } else {
            vec![entry(k, v.clone())]
        }
    };
    Ok(Some(match name {
        "hasOwnProperty" | "containsKey" | "has_key" | "__contains__" | "has" => V::Bool(o.borrow().props.contains(&key(it, &a0))),
        "containsValue" => {
            let vals: Vec<V> = o.borrow().props.values().cloned().collect();
            V::Bool(vals.iter().any(|v| it.equals(v, &a0)))
        }
        "get" | "getOrDefault" | "getAt" => {
            let k = key(it, &a0);
            match get(&k) {
                Some(v) => v,
                None => args.get(1).cloned().unwrap_or_else(|| it.undef()),
            }
        }
        "put" | "putAt" | "set" => {
            let k = key(it, &a0);
            let old = get(&k).unwrap_or(V::Null);
            o.borrow_mut().props.set(&k, arg(&args, 1));
            if name == "set" && d == Dialect::Js {
                recv.clone()
            } else {
                old
            }
        }
        "putIfAbsent" | "setdefault" => {
            let k = key(it, &a0);
            match get(&k) {
                Some(v) => v,
                None => {
                    let v = args.get(1).cloned().unwrap_or(V::Null);
                    o.borrow_mut().props.set(&k, v.clone());
                    if name == "setdefault" {
                        v
                    } else {
                        V::Null
                    }
                }
            }
        }
        "putAll" | "update" => {
            let kwa = kwargs(&mut args);
            let src = arg(&args, 0);
            let src = match &src {
                V::Host(h) => crate::host::snapshot(it, h).unwrap_or(V::Null),
                other => other.clone(),
            };
            if let V::Obj(s) = &src {
                let items: Vec<(Rc<str>, V)> = s.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                for (k, v) in items {
                    o.borrow_mut().props.set(&k, v);
                }
            } else if let V::Arr(pairs) = &src {
                for p in pairs.borrow().clone() {
                    let kv = it.iterate(&p, Some(pos))?;
                    if kv.len() == 2 {
                        let k = key(it, &kv[0]);
                        o.borrow_mut().props.set(&k, kv[1].clone());
                    }
                }
            }
            if let Some(k) = kwa {
                for (kk, v) in k.borrow().props.iter() {
                    o.borrow_mut().props.set(kk, v.clone());
                }
            }
            V::Null
        }
        "remove" | "pop" | "delete" => {
            let k = key(it, &a0);
            let r = o.borrow_mut().props.remove(&k);
            match r {
                Some(v) => {
                    if name == "delete" && d == Dialect::Js {
                        V::Bool(true)
                    } else {
                        v
                    }
                }
                None if args.len() > 1 => args[1].clone(),
                None if name == "pop" && d == Dialect::Python => return it.throw("KeyError", py_repr(it, &a0), Some(pos)).map(Some),
                None => V::Null,
            }
        }
        "keySet" | "keys" => {
            let keys: Vec<V> = o.borrow().props.keys().map(|k| V::Str(k.clone())).collect();
            V::arr(keys)
        }
        "values" => V::arr(o.borrow().props.values().cloned().collect()),
        "entrySet" => V::arr(entries().iter().map(|(k, v)| entry(k, v.clone())).collect()),
        "items" | "entries" => V::arr(entries().iter().map(|(k, v)| V::arr(vec![V::Str(k.clone()), v.clone()])).collect()),
        "size" | "__len__" => V::Int(o.borrow().props.len() as i64),
        "isEmpty" => V::Bool(o.borrow().props.is_empty()),
        "clear" => {
            o.borrow_mut().props.clear();
            V::Null
        }
        "copy" | "clone" => V::obj(o.borrow().props.clone()),
        "each" | "forEach" => {
            for (k, v) in entries() {
                let a = if d == Dialect::Js || (name == "forEach" && arity(&a0) >= 2) { vec![v.clone(), V::Str(k.clone())] } else { kv_args(&a0, &k, &v) };
                let a = if name == "forEach" && d != Dialect::Js { vec![V::Str(k.clone()), v.clone()] } else { a };
                it.call(&a0, V::Undef, a, pos)?;
            }
            if name == "each" {
                recv.clone()
            } else {
                V::Undef
            }
        }
        "eachWithIndex" => {
            for (i, (k, v)) in entries().into_iter().enumerate() {
                let a = if arity(&a0) >= 3 { vec![V::Str(k.clone()), v.clone(), V::Int(i as i64)] } else { vec![entry(&k, v.clone()), V::Int(i as i64)] };
                it.call(&a0, V::Undef, a, pos)?;
            }
            recv.clone()
        }
        "collect" => {
            let mut out = Vec::new();
            for (k, v) in entries() {
                let a = kv_args(&a0, &k, &v);
                out.push(it.call(&a0, V::Undef, a, pos)?);
            }
            V::arr(out)
        }
        "collectEntries" => {
            let mut pm = PropMap::default();
            for (k, v) in entries() {
                let a = kv_args(&a0, &k, &v);
                let r = it.call(&a0, V::Undef, a, pos)?;
                add_entry(it, &mut pm, &r, pos)?;
            }
            V::obj(pm)
        }
        "findAll" | "filter" => {
            let mut pm = PropMap::default();
            for (k, v) in entries() {
                let a = kv_args(&a0, &k, &v);
                let r = it.call(&a0, V::Undef, a, pos)?;
                if it.truthy(&r) {
                    pm.set(&k, v);
                }
            }
            V::obj(pm)
        }
        "find" => {
            let mut found = V::Null;
            for (k, v) in entries() {
                let a = kv_args(&a0, &k, &v);
                let r = it.call(&a0, V::Undef, a, pos)?;
                if it.truthy(&r) {
                    found = entry(&k, v);
                    break;
                }
            }
            found
        }
        "findResult" => {
            let mut found = V::Null;
            for (k, v) in entries() {
                let a = kv_args(&a0, &k, &v);
                let r = it.call(&a0, V::Undef, a, pos)?;
                if !r.is_nullish() {
                    found = r;
                    break;
                }
            }
            found
        }
        "any" | "every" | "count" => {
            let mut n = 0;
            let total = o.borrow().props.len();
            for (k, v) in entries() {
                let a = kv_args(&a0, &k, &v);
                let r = it.call(&a0, V::Undef, a, pos)?;
                if it.truthy(&r) {
                    n += 1;
                    if name == "any" {
                        break;
                    }
                } else if name == "every" {
                    return Ok(Some(V::Bool(false)));
                }
            }
            match name {
                "any" => V::Bool(n > 0),
                "every" => V::Bool(n == total),
                _ => V::Int(n as i64),
            }
        }
        "inject" => {
            let (mut acc, f) = if args.len() > 1 { (a0.clone(), arg(&args, 1)) } else { (V::Null, a0.clone()) };
            for (k, v) in entries() {
                let a = if arity(&f) >= 3 { vec![acc, V::Str(k.clone()), v.clone()] } else { vec![acc, entry(&k, v.clone())] };
                acc = it.call(&f, V::Undef, a, pos)?;
            }
            acc
        }
        "groupBy" => {
            let mut pm = PropMap::default();
            for (k, v) in entries() {
                let a = kv_args(&a0, &k, &v);
                let g = it.call(&a0, V::Undef, a, pos)?;
                let gk = key(it, &g);
                let cur = pm.get(&gk).cloned().unwrap_or_else(V::new_obj);
                if let V::Obj(c) = &cur {
                    c.borrow_mut().props.set(&k, v);
                }
                pm.set(&gk, cur);
            }
            V::obj(pm)
        }
        "sort" => {
            let items: Vec<V> = entries().iter().map(|(k, v)| entry(k, v.clone())).collect();
            let sorted = if a0.is_callable() {
                sort_with(it, items, Some(&a0), false, pos)?
            } else {
                sort_vals(it, items, &mut |it, a, b| {
                    let ka = it.get_member(a, "key", 0)?;
                    let kb = it.get_member(b, "key", 0)?;
                    Ok(natural(it, &ka, &kb))
                })?
            };
            let mut pm = PropMap::default();
            for e in sorted {
                let k = it.get_member(&e, "key", 0)?;
                let v = it.get_member(&e, "value", 0)?;
                let ks = key(it, &k);
                pm.set(&ks, v);
            }
            V::obj(pm)
        }
        "max" | "min" => {
            let items: Vec<V> = entries().iter().map(|(k, v)| entry(k, v.clone())).collect();
            if items.is_empty() {
                V::Null
            } else {
                let sorted = sort_with(it, items, Some(&a0), false, pos)?;
                if name == "max" {
                    sorted.last().cloned().unwrap_or(V::Null)
                } else {
                    sorted.first().cloned().unwrap_or(V::Null)
                }
            }
        }
        "subMap" => {
            let keys = it.iterate(&a0, Some(pos))?;
            let mut pm = PropMap::default();
            for k in keys {
                let ks = key(it, &k);
                if let Some(v) = get(&ks) {
                    pm.set(&ks, v);
                }
            }
            V::obj(pm)
        }
        "plus" => match &a0 {
            V::Obj(y) => crate::interp::merge_objects(&o, y),
            _ => return Ok(None),
        },
        "minus" => {
            let mut pm = o.borrow().props.clone();
            match &a0 {
                V::Obj(y) => {
                    for k in y.borrow().props.keys() {
                        pm.remove(k);
                    }
                }
                other => {
                    for k in it.iterate(other, Some(pos))? {
                        let ks = key(it, &k);
                        pm.remove(&ks);
                    }
                }
            }
            V::obj(pm)
        }
        "getKey" | "getValue" if o.borrow().props.contains("key") => get(if name == "getKey" { "key" } else { "value" }).unwrap_or(V::Null),
        "setValue" if o.borrow().props.contains("key") => {
            o.borrow_mut().props.set("value", a0);
            V::Null
        }
        _ => {
            // JavaBean style accessors on maps (Groovy / Java languages)
            if d != Dialect::Js && d != Dialect::Python {
                if let Some(p) = name.strip_prefix("get").or_else(|| name.strip_prefix("is")).filter(|r| r.chars().next().is_some_and(|c| c.is_ascii_uppercase())) {
                    let k = lc_first(p);
                    if let Some(v) = get(&k) {
                        return Ok(Some(v));
                    }
                }
                if let Some(p) = name.strip_prefix("set").filter(|r| r.chars().next().is_some_and(|c| c.is_ascii_uppercase())) {
                    o.borrow_mut().props.set(&lc_first(p), a0);
                    return Ok(Some(V::Null));
                }
            }
            return Ok(None);
        }
    }))
}

fn lc_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn add_entry(it: &mut Interp, pm: &mut PropMap, r: &V, pos: u32) -> R<()> {
    match r {
        V::Obj(o) => {
            let ob = o.borrow();
            if ob.props.len() == 2 && ob.props.contains("key") && ob.props.contains("value") {
                let k = it.key_string(ob.props.get("key").unwrap());
                pm.set(&k, ob.props.get("value").unwrap().clone());
            } else {
                for (k, v) in ob.props.iter() {
                    pm.set(k, v.clone());
                }
            }
        }
        V::Arr(_) => {
            let kv = it.iterate(r, Some(pos))?;
            if let Some(k) = kv.first() {
                let ks = it.key_string(k);
                pm.set(&ks, kv.get(1).cloned().unwrap_or(V::Null));
            }
        }
        _ => {}
    }
    Ok(())
}

fn flatten_into(v: &V, depth: i64, out: &mut Vec<V>) {
    match v {
        V::Arr(a) if depth != 0 => {
            for x in a.borrow().iter() {
                flatten_into(x, depth - 1, out);
            }
        }
        other => out.push(other.clone()),
    }
}

/// A generic list method on an array value (bypassing the Java method layer).
pub fn list_method(it: &mut Interp, recv: &V, name: &str, args: Vec<V>, pos: u32) -> R<V> {
    if let V::Arr(a) = recv {
        if let Some(v) = array_method(it, a.clone(), recv, name, args, pos)? {
            return Ok(v);
        }
    }
    it.type_error(format!("{} is not a function", name), Some(pos))
}

fn array_method(it: &mut Interp, a: ArrRef, recv: &V, name: &str, mut args: Vec<V>, pos: u32) -> R<Option<V>> {
    let d = it.d;
    let js = d == Dialect::Js;
    let a0 = arg(&args, 0);
    let items = || a.borrow().clone();
    let len = a.borrow().len();
    let cb = |it: &mut Interp, f: &V, x: &V, i: usize| -> R<V> {
        let args = elem_args(it, x, i, recv);
        it.call(f, V::Undef, args, pos)
    };
    let truth = |it: &mut Interp, f: &V, x: &V, i: usize| -> R<bool> {
        if f.is_nullish() {
            return Ok(it.truthy(x));
        }
        if !f.is_callable() {
            // Groovy grep / find with a value: isCase
            return Ok(match f {
                V::Regex(r) => r.re.is_full_match(&it.display(x)),
                V::Host(h) => match &**h {
                    Host::Ns(n) => is_type(x, n),
                    Host::Type(n) => is_type(x, n),
                    _ => false,
                },
                V::Arr(_) => it.contains(f, x, pos)?,
                other => it.equals(other, x),
            });
        }
        let r = cb(it, f, x, i)?;
        Ok(it.truthy(&r))
    };
    Ok(Some(match name {
        "push" | "add" | "append" | "leftShift" | "addLast" | "offer" => {
            if name == "add" && args.len() == 2 && d != Dialect::Js {
                let i = (iarg(it, &args, 0, 0).max(0) as usize).min(len);
                a.borrow_mut().insert(i, args[1].clone());
                return Ok(Some(V::Null));
            }
            a.borrow_mut().extend(args.iter().cloned());
            match (d, name) {
                (Dialect::Js, _) => V::Int(a.borrow().len() as i64),
                (_, "leftShift") => recv.clone(),
                (Dialect::Python, _) => V::Null,
                _ => V::Bool(true),
            }
        }
        "pop" | "removeLast" => {
            if d == Dialect::Python && !args.is_empty() {
                let i = iarg(it, &args, 0, -1);
                let i = if i < 0 { i + len as i64 } else { i };
                if i < 0 || i as usize >= len {
                    return it.throw("IndexError", "pop index out of range".into(), Some(pos)).map(Some);
                }
                return Ok(Some(a.borrow_mut().remove(i as usize)));
            }
            match a.borrow_mut().pop() {
                Some(v) => v,
                None if d == Dialect::Python => return it.throw("IndexError", "pop from empty list".into(), Some(pos)).map(Some),
                None if d == Dialect::Groovy => return it.throw("NoSuchElementException", "Cannot pop() an empty List".into(), Some(pos)).map(Some),
                None => it.undef(),
            }
        }
        "shift" | "removeFirst" | "poll" => {
            if len == 0 {
                it.undef()
            } else {
                a.borrow_mut().remove(0)
            }
        }
        "unshift" | "addFirst" => {
            let mut v = args.clone();
            v.extend(items());
            *a.borrow_mut() = v;
            V::Int(a.borrow().len() as i64)
        }
        "insert" => {
            let i = iarg(it, &args, 0, 0);
            let i = if i < 0 { (i + len as i64).max(0) as usize } else { (i as usize).min(len) };
            a.borrow_mut().insert(i, arg(&args, 1));
            V::Null
        }
        "extend" | "addAll" => {
            let more = it.iterate(&a0, Some(pos))?;
            a.borrow_mut().extend(more);
            if d == Dialect::Python {
                V::Null
            } else {
                V::Bool(true)
            }
        }
        "slice" => {
            let x = rel(if args.is_empty() { 0.0 } else { farg(it, &args, 0) }, len);
            let y = if args.len() > 1 && !arg(&args, 1).is_nullish() { rel(farg(it, &args, 1), len) } else { len };
            V::arr(if x < y { a.borrow()[x..y].to_vec() } else { vec![] })
        }
        "subList" => {
            let x = (iarg(it, &args, 0, 0).max(0) as usize).min(len);
            let y = (iarg(it, &args, 1, len as i64).max(0) as usize).min(len);
            V::arr(if x < y { a.borrow()[x..y].to_vec() } else { vec![] })
        }
        "splice" => {
            let start = rel(farg(it, &args, 0), len);
            let del = if args.len() > 1 { (farg(it, &args, 1).max(0.0) as usize).min(len - start) } else { len - start };
            let ins: Vec<V> = args.iter().skip(2).cloned().collect();
            let removed: Vec<V> = a.borrow_mut().splice(start..start + del, ins).collect();
            V::arr(removed)
        }
        "concat" => {
            let mut v = items();
            for x in &args {
                match x {
                    V::Arr(o) => v.extend(o.borrow().iter().cloned()),
                    other => v.push(other.clone()),
                }
            }
            V::arr(v)
        }
        "join" => {
            let sep = match args.first() {
                Some(V::Undef) | None => {
                    if js {
                        ",".to_string()
                    } else {
                        String::new()
                    }
                }
                Some(s) => it.display(s),
            };
            V::string(items().iter().map(|x| if js && x.is_nullish() { String::new() } else { it.display(x) }).collect::<Vec<_>>().join(&sep))
        }
        "indexOf" | "lastIndexOf" | "index" | "findIndexValue" => {
            let v = items();
            let pos_found = if name == "lastIndexOf" { v.iter().rposition(|x| it.equals(x, &a0)) } else { v.iter().position(|x| if js { crate::interp::strict_eq(x, &a0) } else { it.equals(x, &a0) }) };
            match pos_found {
                Some(i) => V::Int(i as i64),
                None if name == "index" => return it.throw("ValueError", format!("{} is not in list", py_repr(it, &a0)), Some(pos)).map(Some),
                None => V::Int(-1),
            }
        }
        "includes" | "contains" | "__contains__" => {
            let v = items();
            V::Bool(v.iter().any(|x| it.equals(x, &a0)))
        }
        "containsAll" => {
            let v = items();
            let need = it.iterate(&a0, Some(pos))?;
            V::Bool(need.iter().all(|n| v.iter().any(|x| it.equals(x, n))))
        }
        "find" | "findLast" => {
            let v = items();
            let mut found = it.undef();
            let order: Vec<usize> = if name == "findLast" { (0..v.len()).rev().collect() } else { (0..v.len()).collect() };
            for i in order {
                if truth(it, &a0, &v[i], i)? {
                    found = v[i].clone();
                    break;
                }
            }
            found
        }
        "findIndex" | "findIndexOf" | "findLastIndex" | "findLastIndexOf" => {
            let v = items();
            let mut found = -1;
            let order: Vec<usize> = if name.contains("Last") { (0..v.len()).rev().collect() } else { (0..v.len()).collect() };
            for i in order {
                if truth(it, &a0, &v[i], i)? {
                    found = i as i64;
                    break;
                }
            }
            V::Int(found)
        }
        "findResult" => {
            let v = items();
            let mut found = V::Null;
            for (i, x) in v.iter().enumerate() {
                let r = cb(it, &a0, x, i)?;
                if !r.is_nullish() {
                    found = r;
                    break;
                }
            }
            found
        }
        "filter" | "findAll" | "grep" => {
            let v = items();
            let mut out = Vec::new();
            for (i, x) in v.iter().enumerate() {
                if truth(it, &a0, x, i)? {
                    out.push(x.clone());
                }
            }
            V::arr(out)
        }
        "map" | "collect" => {
            if a0.is_nullish() && d != Dialect::Js {
                return Ok(Some(V::arr(items())));
            }
            let v = items();
            let mut out = Vec::with_capacity(v.len());
            for (i, x) in v.iter().enumerate() {
                out.push(cb(it, &a0, x, i)?);
            }
            V::arr(out)
        }
        "flatMap" | "collectMany" => {
            let v = items();
            let mut out = Vec::new();
            for (i, x) in v.iter().enumerate() {
                let r = cb(it, &a0, x, i)?;
                flatten_into(&r, 1, &mut out);
            }
            V::arr(out)
        }
        "forEach" | "each" => {
            let v = items();
            for (i, x) in v.iter().enumerate() {
                cb(it, &a0, x, i)?;
            }
            if name == "each" {
                recv.clone()
            } else {
                V::Undef
            }
        }
        "reverseEach" => {
            let v = items();
            for (i, x) in v.iter().enumerate().rev() {
                cb(it, &a0, x, i)?;
            }
            recv.clone()
        }
        "eachWithIndex" | "forEachIndexed" => {
            let v = items();
            for (i, x) in v.iter().enumerate() {
                it.call(&a0, V::Undef, vec![x.clone(), V::Int(i as i64)], pos)?;
            }
            recv.clone()
        }
        "withIndex" | "indexed" => V::arr(items().into_iter().enumerate().map(|(i, x)| V::arr(vec![x, V::Int(i as i64)])).collect()),
        "reduce" | "reduceRight" => {
            let mut v = items();
            if name == "reduceRight" {
                v.reverse();
            }
            let (mut acc, start) = if args.len() > 1 {
                (arg(&args, 1), 0)
            } else if v.is_empty() {
                return it.type_error("Reduce of empty array with no initial value".into(), Some(pos)).map(Some);
            } else {
                (v[0].clone(), 1)
            };
            for (i, x) in v.iter().enumerate().skip(start) {
                let call_args = if js { vec![acc, x.clone(), V::Int(i as i64), recv.clone()] } else { vec![acc, x.clone()] };
                acc = it.call(&a0, V::Undef, call_args, pos)?;
            }
            acc
        }
        "inject" => {
            let v = items();
            let (mut acc, f, start) = if args.len() > 1 { (a0.clone(), arg(&args, 1), 0) } else { (v.first().cloned().unwrap_or(V::Null), a0.clone(), 1) };
            for x in v.iter().skip(start) {
                acc = it.call(&f, V::Undef, vec![acc, x.clone()], pos)?;
            }
            acc
        }
        "some" | "any" => {
            let v = items();
            let mut r = false;
            for (i, x) in v.iter().enumerate() {
                if truth(it, &a0, x, i)? {
                    r = true;
                    break;
                }
            }
            V::Bool(r)
        }
        "every" => {
            let v = items();
            let mut r = true;
            for (i, x) in v.iter().enumerate() {
                if !truth(it, &a0, x, i)? {
                    r = false;
                    break;
                }
            }
            V::Bool(r)
        }
        "count" => {
            let v = items();
            let mut n = 0;
            for (i, x) in v.iter().enumerate() {
                let hit = if a0.is_callable() { truth(it, &a0, x, i)? } else { it.equals(x, &a0) };
                if hit {
                    n += 1;
                }
            }
            V::Int(n)
        }
        "sort" | "toSorted" | "sorted" => {
            let kwa = kwargs(&mut args);
            let a0 = arg(&args, 0);
            let mutate = name == "sort" && !matches!(a0, V::Bool(false));
            let f = if matches!(a0, V::Bool(_)) { arg(&args, 1) } else { a0 };
            let mut sorted = if let Some(k) = kw(&kwa, "key").filter(|k| !k.is_nullish()) {
                let mut keyed = Vec::new();
                for x in items() {
                    let kv = it.call(&k, V::Undef, vec![x.clone()], pos)?;
                    keyed.push(V::arr(vec![kv, x]));
                }
                let s = sort_vals(it, keyed, &mut |it, a, b| it.py_order(&first(a), &first(b), pos))?;
                s.iter().map(|p| if let V::Arr(a) = p { a.borrow()[1].clone() } else { V::Null }).collect()
            } else {
                sort_with(it, items(), if f.is_nullish() { None } else { Some(&f) }, js, pos)?
            };
            if kw(&kwa, "reverse").is_some_and(|r| it.truthy(&r)) {
                sorted.reverse();
            }
            if mutate {
                *a.borrow_mut() = sorted;
                if d == Dialect::Python {
                    V::Null
                } else {
                    recv.clone()
                }
            } else {
                V::arr(sorted)
            }
        }
        "reverse" | "reversed" | "toReversed" => {
            if js && name == "reverse" || d == Dialect::Python && name == "reverse" {
                a.borrow_mut().reverse();
                if d == Dialect::Python {
                    V::Null
                } else {
                    recv.clone()
                }
            } else {
                let mut v = items();
                v.reverse();
                V::arr(v)
            }
        }
        "flat" | "flatten" => {
            let depth = if name == "flatten" { -1 } else { iarg(it, &args, 0, 1) };
            let mut out = Vec::new();
            for x in items() {
                flatten_into(&x, depth, &mut out);
            }
            V::arr(out)
        }
        "fill" => {
            let x = rel(if args.len() > 1 { farg(it, &args, 1) } else { 0.0 }, len);
            let y = if args.len() > 2 { rel(farg(it, &args, 2), len) } else { len };
            for i in x..y {
                a.borrow_mut()[i] = a0.clone();
            }
            recv.clone()
        }
        "at" => {
            let i = iarg(it, &args, 0, 0);
            let i = if i < 0 { i + len as i64 } else { i };
            if i < 0 || i as usize >= len {
                it.undef()
            } else {
                a.borrow()[i as usize].clone()
            }
        }
        "keys" => V::arr((0..len).map(|i| V::Int(i as i64)).collect()),
        "entries" => V::arr(items().into_iter().enumerate().map(|(i, x)| V::arr(vec![V::Int(i as i64), x])).collect()),
        "values" | "toList" | "asList" | "toArray" | "copy" | "clone" | "asImmutable" | "asUnmodifiable" | "stream" => V::arr(items()),
        "toSet" | "asSet" => make_set(it, items()),
        "size" | "length" | "__len__" => V::Int(len as i64),
        "isEmpty" => V::Bool(len == 0),
        "first" | "head" | "last" | "getFirst" | "getLast" | "peek" => {
            if len == 0 {
                if d == Dialect::Groovy {
                    return it.throw("NoSuchElementException", format!("Cannot access {}() element from an empty List", name), Some(pos)).map(Some);
                }
                V::Null
            } else if args.is_empty() || name != "first" && name != "last" {
                let i = if name == "last" || name == "getLast" || name == "peek" { len - 1 } else { 0 };
                a.borrow()[i].clone()
            } else {
                let n = (iarg(it, &args, 0, 1).max(0) as usize).min(len);
                if name == "first" {
                    V::arr(a.borrow()[..n].to_vec())
                } else {
                    V::arr(a.borrow()[len - n..].to_vec())
                }
            }
        }
        "tail" => V::arr(items().into_iter().skip(1).collect()),
        "init" => V::arr(items().into_iter().take(len.saturating_sub(1)).collect()),
        "take" => V::arr(items().into_iter().take(iarg(it, &args, 0, 0).max(0) as usize).collect()),
        "drop" => V::arr(items().into_iter().skip(iarg(it, &args, 0, 0).max(0) as usize).collect()),
        "takeWhile" | "dropWhile" => {
            let v = items();
            let mut n = 0;
            for (i, x) in v.iter().enumerate() {
                if truth(it, &a0, x, i)? {
                    n += 1;
                } else {
                    break;
                }
            }
            V::arr(if name == "takeWhile" { v[..n].to_vec() } else { v[n..].to_vec() })
        }
        "get" | "getAt" | "elementAt" => {
            if let V::Arr(_) = a0 {
                return it.get_index(recv, &a0, pos).map(Some);
            }
            let i = iarg(it, &args, 0, 0);
            if (i < 0 || i as usize >= len) && d != Dialect::Groovy {
                return it.throw("IndexOutOfBoundsException", format!("Index {} out of bounds for length {}", i, len), Some(pos)).map(Some);
            }
            it.get_index(recv, &V::Int(i), pos)?
        }
        "set" | "putAt" => {
            let i = arg(&args, 0);
            let old = it.get_index(recv, &i, pos).unwrap_or(V::Null);
            it.set_index(recv, &i, arg(&args, 1), pos)?;
            old
        }
        "remove" | "removeAt" => {
            if d == Dialect::Python {
                let v = items();
                match v.iter().position(|x| it.equals(x, &a0)) {
                    Some(i) => {
                        a.borrow_mut().remove(i);
                        V::Null
                    }
                    None => return it.throw("ValueError", "list.remove(x): x not in list".into(), Some(pos)).map(Some),
                }
            } else if let V::Int(i) = a0 {
                let i = if i < 0 { i + len as i64 } else { i };
                if i < 0 || i as usize >= len {
                    return it.throw("IndexOutOfBoundsException", format!("Index {} out of bounds for length {}", i, len), Some(pos)).map(Some);
                }
                a.borrow_mut().remove(i as usize)
            } else {
                let v = items();
                match v.iter().position(|x| it.equals(x, &a0)) {
                    Some(i) => {
                        a.borrow_mut().remove(i);
                        V::Bool(true)
                    }
                    None => V::Bool(false),
                }
            }
        }
        "removeElement" => {
            let v = items();
            match v.iter().position(|x| it.equals(x, &a0)) {
                Some(i) => {
                    a.borrow_mut().remove(i);
                    V::Bool(true)
                }
                None => V::Bool(false),
            }
        }
        "removeAll" | "retainAll" | "removeIf" => {
            let v = items();
            let mut keep = Vec::new();
            let mut changed = false;
            for (i, x) in v.iter().enumerate() {
                let hit = if a0.is_callable() { truth(it, &a0, x, i)? } else { it.contains(&a0, x, pos)? };
                let k = if name == "retainAll" { hit } else { !hit };
                if k {
                    keep.push(x.clone());
                } else {
                    changed = true;
                }
            }
            *a.borrow_mut() = keep;
            V::Bool(changed)
        }
        "clear" => {
            a.borrow_mut().clear();
            V::Null
        }
        "plus" => {
            let mut v = items();
            match &a0 {
                V::Arr(o) => v.extend(o.borrow().iter().cloned()),
                other => v.push(other.clone()),
            }
            V::arr(v)
        }
        "minus" => it.binary(BinOp::Sub, recv.clone(), a0, pos)?,
        "intersect" => {
            let other = it.iterate(&a0, Some(pos))?;
            let v: Vec<V> = items().into_iter().filter(|x| other.iter().any(|o| it.equals(x, o))).collect();
            V::arr(v)
        }
        "disjoint" => {
            let other = it.iterate(&a0, Some(pos))?;
            let v = items();
            V::Bool(!v.iter().any(|x| other.iter().any(|o| it.equals(x, o))))
        }
        "sum" => {
            let v = items();
            let (init, f) = if a0.is_callable() { (V::Null, Some(a0.clone())) } else if args.is_empty() { (V::Null, None) } else { (a0.clone(), arg(&args, 1).is_callable().then(|| arg(&args, 1))) };
            let mut vals = Vec::new();
            for (i, x) in v.iter().enumerate() {
                vals.push(match &f {
                    Some(f) => cb(it, f, x, i)?,
                    None => x.clone(),
                });
            }
            let empty = vals.is_empty();
            let r = sum_values(it, vals, init, pos)?;
            if empty && r.is_nullish() && d != Dialect::Groovy {
                V::Int(0)
            } else {
                r
            }
        }
        "average" => {
            let v = items();
            if v.is_empty() {
                V::Null
            } else {
                let n = v.len();
                let s = sum_values(it, v, V::Null, pos)?;
                V::Num(s.as_f64().unwrap_or(f64::NAN) / n as f64)
            }
        }
        "max" | "min" => {
            let v = items();
            if v.is_empty() {
                V::Null
            } else {
                let sorted = sort_with(it, v, if a0.is_callable() { Some(&a0) } else { None }, false, pos)?;
                if name == "max" {
                    sorted.last().cloned().unwrap_or(V::Null)
                } else {
                    sorted.first().cloned().unwrap_or(V::Null)
                }
            }
        }
        "unique" | "toUnique" | "distinct" => {
            let v = items();
            let mut out: Vec<V> = Vec::new();
            let mut keys: Vec<V> = Vec::new();
            for x in v {
                let k = if a0.is_callable() && arity(&a0) == 1 { it.call(&a0, V::Undef, vec![x.clone()], pos)? } else { x.clone() };
                if !keys.iter().any(|y| it.equals(y, &k)) {
                    keys.push(k);
                    out.push(x);
                }
            }
            if name == "unique" {
                *a.borrow_mut() = out;
                recv.clone()
            } else {
                V::arr(out)
            }
        }
        "groupBy" | "countBy" => {
            let mut pm = PropMap::default();
            for (i, x) in items().iter().enumerate() {
                let g = cb(it, &a0, x, i)?;
                let gk = it.key_string(&g);
                if name == "countBy" {
                    let n = pm.get(&gk).and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
                    pm.set(&gk, V::Int(n + 1));
                } else {
                    let cur = pm.get(&gk).cloned().unwrap_or_else(|| V::arr(vec![]));
                    if let V::Arr(c) = &cur {
                        c.borrow_mut().push(x.clone());
                    }
                    pm.set(&gk, cur);
                }
            }
            V::obj(pm)
        }
        "collectEntries" => {
            let mut pm = PropMap::default();
            for (i, x) in items().iter().enumerate() {
                let r = if a0.is_callable() { cb(it, &a0, x, i)? } else { x.clone() };
                add_entry(it, &mut pm, &r, pos)?;
            }
            V::obj(pm)
        }
        "split" if d == Dialect::Groovy => {
            let (mut yes, mut no) = (Vec::new(), Vec::new());
            for (i, x) in items().iter().enumerate() {
                if truth(it, &a0, x, i)? {
                    yes.push(x.clone());
                } else {
                    no.push(x.clone());
                }
            }
            V::arr(vec![V::arr(yes), V::arr(no)])
        }
        "collate" => {
            let n = iarg(it, &args, 0, 1).max(1) as usize;
            V::arr(items().chunks(n).map(|c| V::arr(c.to_vec())).collect())
        }
        "swap" => {
            let i = iarg(it, &args, 0, 0) as usize;
            let j = iarg(it, &args, 1, 0) as usize;
            if i < len && j < len {
                a.borrow_mut().swap(i, j);
            }
            recv.clone()
        }
        _ => return Ok(None),
    }))
}

/// `Object.defineProperty(o, k, descriptor)`.
fn define_property(it: &mut Interp, o: &V, k: &str, desc: &V, pos: u32) -> R<()> {
    let V::Obj(ob) = o else {
        return it.type_error("Object.defineProperty called on non-object".into(), Some(pos));
    };
    let get = it.get_member(desc, "get", pos).ok().filter(|v| v.is_callable());
    let set = it.get_member(desc, "set", pos).ok().filter(|v| v.is_callable());
    if get.is_some() || set.is_some() {
        let existing = ob.borrow().props.get(k).cloned();
        let acc = it.make_accessor(get, set, existing);
        ob.borrow_mut().props.set(k, acc);
    } else {
        let v = it.get_member(desc, "value", pos).unwrap_or(V::Undef);
        ob.borrow_mut().props.set(k, v);
    }
    Ok(())
}
