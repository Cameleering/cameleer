//! Java (Camel `joor` / `java` language): bindings, helper functions and the parts of the JDK
//! a snippet commonly uses (`Optional`, streams and `Collectors`, functional interfaces,
//! method references, `Objects`, `Character`, `System.out`...). Java source is parsed by the
//! C-like parser ([`crate::parser`]) in the [`Dialect::Java`] dialect and evaluated by the shared
//! interpreter with Java semantics (integer division, Java `toString`, exceptions).
//!
//! Also: `Java.type(..)` for JavaScript (a few safe JDK classes).

use std::rc::Rc;

use crate::interp::{Ctrl, Interp, R};
use crate::value::*;

const JOOR_FNS: &[&str] = &[
    "bodyAs",
    "optionalBodyAs",
    "header",
    "headerAs",
    "optionalHeaderAs",
    "exchangeProperty",
    "exchangePropertyAs",
    "optionalExchangePropertyAs",
    "variable",
    "variableAs",
    "optionalVariableAs",
    "__newarray",
];

const JAVA_NS: &[&str] = &[
    "Math", "Integer", "Long", "Double", "Float", "Short", "Byte", "Boolean", "String", "Character", "List", "Map", "Set", "Arrays", "Collections", "Objects", "Optional",
    "System", "UUID", "StringBuilder", "ArrayList", "LinkedList", "HashMap", "LinkedHashMap", "TreeMap", "HashSet", "LinkedHashSet", "TreeSet", "Collectors", "Stream",
    "IntStream", "BigDecimal", "BigInteger", "Number", "Object", "Pattern", "Thread",
];

/// Install the Java bindings: `optionalBody`, the jOOR helper functions and the JDK classes.
pub fn install(it: &mut Interp) {
    for n in JAVA_NS {
        if let Some(v) = crate::builtins::type_ns(n) {
            it.globals.define(n, v);
        }
    }
    it.globals.define("Comparator", V::host(Host::Ns("Comparator")));
    for f in JOOR_FNS {
        it.globals.define(f, V::host(Host::NsFn("jOOR", f.to_string())));
    }
    let body = it.host.body();
    it.globals.define("optionalBody", optional(Some(body)));
}

/// `java.util.Optional` value.
pub fn optional(v: Option<V>) -> V {
    let mut p = PropMap::default();
    if let Some(v) = v.filter(|v| !v.is_nullish()) {
        p.set("value", v);
    }
    V::obj_kind(p, ObjKind::Optional)
}

fn type_name(v: &V) -> String {
    match v {
        V::Str(s) => s.to_string(),
        V::Host(h) => match &**h {
            Host::Ns(n) => n.to_string(),
            Host::Type(n) => n.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn convert(it: &mut Interp, v: V, ty: &V, pos: u32) -> R<V> {
    let t = type_name(ty);
    if t.is_empty() || v.is_nullish() || t == "Object" {
        return Ok(v);
    }
    crate::builtins::as_type(it, v, &t, pos)
}

/// The jOOR helper functions (`bodyAs(String.class)`, `headerAs("x", Integer.class)`...).
pub fn call_fn(it: &mut Interp, name: &str, args: Vec<V>, pos: u32) -> R<V> {
    let a = |i: usize| args.get(i).cloned().unwrap_or(V::Null);
    let lookup = |it: &mut Interp, which: &str, key: &V| -> R<V> {
        let host = match which {
            "header" => V::host(Host::Headers),
            "property" => V::host(Host::Properties),
            _ => V::host(Host::Variables),
        };
        let k = it.display(key);
        it.call_method(&host, "get", vec![V::string(k)], pos)
    };
    Ok(match name {
        "bodyAs" | "optionalBodyAs" => {
            let b = it.host.body();
            let v = convert(it, b, &a(0), pos)?;
            if name == "optionalBodyAs" {
                optional(Some(v))
            } else {
                v
            }
        }
        "header" | "exchangeProperty" | "variable" => {
            let which = match name {
                "header" => "header",
                "exchangeProperty" => "property",
                _ => "variable",
            };
            lookup(it, which, &a(0))?
        }
        "headerAs" | "exchangePropertyAs" | "variableAs" | "optionalHeaderAs" | "optionalExchangePropertyAs" | "optionalVariableAs" => {
            let which = if name.contains("eader") {
                "header"
            } else if name.contains("roperty") {
                "property"
            } else {
                "variable"
            };
            let v = lookup(it, which, &a(0))?;
            // (name, type) or (name, default, type)
            let (default, ty) = if args.len() >= 3 { (Some(a(1)), a(2)) } else { (None, a(1)) };
            let v = if v.is_nullish() { default.unwrap_or(V::Null) } else { v };
            let v = convert(it, v, &ty, pos)?;
            if name.starts_with("optional") {
                optional(Some(v))
            } else {
                v
            }
        }
        "__newarray" => {
            let n = a(0).as_f64().unwrap_or(0.0).max(0.0) as usize;
            V::arr(vec![a(1); n])
        }
        _ => return it.type_error(format!("{} is not a function", name), Some(pos)),
    })
}

/// `Java.type(name)` in JavaScript: a few safe JDK classes.
pub fn java_type_for_js(it: &mut Interp, name: &str, pos: u32) -> R<V> {
    let simple = name.rsplit('.').next().unwrap_or(name);
    let allowed = matches!(
        name,
        "java.util.ArrayList"
            | "java.util.LinkedList"
            | "java.util.HashMap"
            | "java.util.LinkedHashMap"
            | "java.util.TreeMap"
            | "java.util.HashSet"
            | "java.util.LinkedHashSet"
            | "java.util.TreeSet"
            | "java.util.UUID"
            | "java.util.Arrays"
            | "java.util.Collections"
            | "java.util.Objects"
            | "java.util.Optional"
            | "java.lang.String"
            | "java.lang.Integer"
            | "java.lang.Long"
            | "java.lang.Double"
            | "java.lang.Boolean"
            | "java.lang.Character"
            | "java.lang.Math"
            | "java.lang.StringBuilder"
            | "java.lang.System"
            | "java.math.BigDecimal"
            | "java.math.BigInteger"
    );
    if !allowed {
        return it.throw("TypeError", format!("Access to host class {} is not allowed or does not exist.", name), Some(pos));
    }
    let ns = match simple {
        // keep the JavaScript String / Math globals distinct from the Java classes
        "String" => "JavaString",
        "System" => "JavaSystem",
        other => {
            let found: Option<&'static str> = match crate::builtins::type_ns(other) {
                Some(V::Host(h)) => match &*h {
                    Host::Ns(n) => Some(*n),
                    _ => None,
                },
                _ => None,
            };
            match found {
                Some(n) => n,
                None => return Ok(V::host(Host::Type(simple.to_string()))),
            }
        }
    };
    Ok(V::host(Host::Ns(ns)))
}

/// Java constructors not covered by the generic builtins.
pub fn construct(it: &mut Interp, n: &str, args: &[V], pos: u32) -> R<Option<V>> {
    let _ = pos;
    Ok(match n {
        "JavaString" => Some(V::string(args.first().map(|a| it.display(a)).unwrap_or_default())),
        "Thread" | "JavaSystem" => None,
        _ => None,
    })
}

fn fn_value(data: V, kind: &str) -> V {
    V::Method(Rc::new((data, format!("%mref:%{}", kind))))
}

/// Call a method reference (`Type::method`, `obj::method`, `Type::new`) or a composed
/// functional value.
pub fn method_ref_call(it: &mut Interp, recv: &V, m: &str, args: Vec<V>, pos: u32) -> R<V> {
    if let Some(kind) = m.strip_prefix('%') {
        return composed_call(it, recv, kind, args, pos);
    }
    match recv {
        V::Host(h) => {
            let ns = match &**h {
                Host::Ns(n) => Some(*n),
                _ => None,
            };
            let tname = type_name(recv);
            if m == "new" {
                return match ns {
                    Some(n) => crate::builtins::construct_ns(it, n, args, pos),
                    None => crate::builtins::construct_type(it, &tname, args, pos),
                };
            }
            // instance method of the first argument (`String::toUpperCase`, `Person::getName`)
            let instance_first = !args.is_empty() && (crate::builtins::is_type(&args[0], &tname) || ns.is_none() || matches!(tname.as_str(), "Object"));
            if instance_first && !is_static_name(&tname, m, args.len()) {
                let mut rest = args.clone();
                let recv0 = rest.remove(0);
                match it.call_method(&recv0, m, rest, pos) {
                    Ok(v) => return Ok(v),
                    Err(Ctrl::Throw(e)) if ns.is_some() && is_missing(&e) => {}
                    Err(e) => return Err(e),
                }
            }
            match ns {
                Some(n) => crate::builtins::call_ns_fn(it, n, m, args, pos),
                None => it.throw("CompilationException", format!("invalid method reference {}::{}", tname, m), Some(pos)),
            }
        }
        V::Func(c) if c.flavor == Flavor::Class => {
            if m == "new" {
                return it.construct_class(c, args, pos);
            }
            let mut rest = args;
            if rest.is_empty() {
                return it.call_method(recv, m, vec![], pos);
            }
            let recv0 = rest.remove(0);
            it.call_method(&recv0, m, rest, pos)
        }
        _ => it.call_method(recv, m, args, pos),
    }
}

fn is_missing(e: &V) -> bool {
    let n = crate::builtins::error_name(e);
    n == "CompilationException" || n == "MissingMethodException" || n == "TypeError"
}

fn is_static_name(t: &str, m: &str, nargs: usize) -> bool {
    match t {
        "String" => matches!(m, "valueOf" | "format" | "join" | "copyValueOf"),
        "Integer" | "Long" | "Double" | "Float" | "Short" | "Byte" => {
            matches!(m, "parseInt" | "parseLong" | "parseDouble" | "parseFloat" | "valueOf" | "sum" | "max" | "min" | "compare" | "signum" | "toBinaryString" | "toHexString" | "bitCount")
                || (m == "toString" && nargs == 1)
        }
        "Boolean" => matches!(m, "parseBoolean" | "valueOf" | "logicalAnd" | "logicalOr"),
        "Character" => true,
        "Objects" | "Math" | "Arrays" | "Collections" | "Optional" | "List" | "Map" | "Set" | "SystemOut" | "System" | "Stream" | "IntStream" | "Collectors" => true,
        _ => false,
    }
}

/// Composed functional values (`Comparator.comparing(f)`, `f.andThen(g)`, `p.negate()`...).
fn composed_call(it: &mut Interp, data: &V, kind: &str, args: Vec<V>, pos: u32) -> R<V> {
    let get = |k: &str| -> V {
        match data {
            V::Obj(o) => o.borrow().props.get(k).cloned().unwrap_or(V::Undef),
            _ => V::Undef,
        }
    };
    let a0 = args.first().cloned().unwrap_or(V::Null);
    let a1 = args.get(1).cloned().unwrap_or(V::Null);
    match kind {
        "comparing" => {
            let key = get("f");
            let ka = it.call(&key, V::Undef, vec![a0], pos)?;
            let kb = it.call(&key, V::Undef, vec![a1], pos)?;
            let c = get("cmp");
            if c.is_callable() {
                return it.call(&c, V::Undef, vec![ka, kb], pos);
            }
            Ok(V::Int(cmp_int(it, &ka, &kb)))
        }
        "natural" => Ok(V::Int(cmp_int(it, &a0, &a1))),
        "reverse" => Ok(V::Int(-cmp_int(it, &a0, &a1))),
        "reversed" => {
            let c = get("f");
            let r = it.call(&c, V::Undef, vec![a0, a1], pos)?;
            Ok(V::Int(-(r.as_f64().unwrap_or(0.0) as i64)))
        }
        "then" => {
            let c = get("f");
            let r = it.call(&c, V::Undef, vec![a0.clone(), a1.clone()], pos)?;
            if r.as_f64().unwrap_or(0.0) != 0.0 {
                return Ok(r);
            }
            let g = get("g");
            it.call(&g, V::Undef, vec![a0, a1], pos)
        }
        "andThen" => {
            let f = get("f");
            let g = get("g");
            let r = it.call(&f, V::Undef, args.clone(), pos)?;
            if get("consumer").same(&V::Bool(true)) {
                it.call(&g, V::Undef, args, pos)?;
                return Ok(V::Null);
            }
            it.call(&g, V::Undef, vec![r], pos)
        }
        "compose" => {
            let f = get("f");
            let g = get("g");
            let r = it.call(&g, V::Undef, args, pos)?;
            it.call(&f, V::Undef, vec![r], pos)
        }
        "negate" => {
            let f = get("f");
            let r = it.call(&f, V::Undef, args, pos)?;
            Ok(V::Bool(!it.truthy(&r)))
        }
        "and" | "or" => {
            let f = get("f");
            let g = get("g");
            let r = it.call(&f, V::Undef, args.clone(), pos)?;
            let t = it.truthy(&r);
            if kind == "and" && !t {
                return Ok(V::Bool(false));
            }
            if kind == "or" && t {
                return Ok(V::Bool(true));
            }
            let r2 = it.call(&g, V::Undef, args, pos)?;
            Ok(V::Bool(it.truthy(&r2)))
        }
        "identity" => Ok(a0),
        _ => it.type_error(format!("unknown functional value {}", kind), Some(pos)),
    }
}

fn cmp_int(it: &Interp, a: &V, b: &V) -> i64 {
    match it.compare(a, b) {
        Some(std::cmp::Ordering::Less) => -1,
        Some(std::cmp::Ordering::Greater) => 1,
        _ => 0,
    }
}

fn data(pairs: &[(&str, V)]) -> V {
    let mut p = PropMap::default();
    for (k, v) in pairs {
        p.set(k, v.clone());
    }
    V::obj(p)
}

/// `Comparator.*` and `Function.identity()`.
pub fn comparator_static(it: &mut Interp, name: &str, args: Vec<V>, pos: u32) -> R<V> {
    let a0 = args.first().cloned().unwrap_or(V::Null);
    Ok(match name {
        "comparing" | "comparingInt" | "comparingLong" | "comparingDouble" => fn_value(data(&[("f", a0), ("cmp", args.get(1).cloned().unwrap_or(V::Undef))]), "comparing"),
        "naturalOrder" => fn_value(V::Null, "natural"),
        "reverseOrder" => fn_value(V::Null, "reverse"),
        "identity" => fn_value(V::Null, "identity"),
        _ => return it.type_error(format!("Comparator.{} is not supported", name), Some(pos)),
    })
}

fn collector(kind: &str, args: &[V]) -> V {
    let mut p = PropMap::default();
    p.set("%collector", V::str(kind));
    p.set("args", V::arr(args.to_vec()));
    V::obj(p)
}

/// `Collectors.*`.
pub fn collectors_static(it: &mut Interp, name: &str, args: Vec<V>, pos: u32) -> R<V> {
    match name {
        "toList" | "toUnmodifiableList" | "toSet" | "toUnmodifiableSet" | "joining" | "groupingBy" | "toMap" | "counting" | "summingInt" | "summingLong" | "summingDouble"
        | "averagingInt" | "averagingLong" | "averagingDouble" | "partitioningBy" | "mapping" => Ok(collector(name, &args)),
        _ => it.type_error(format!("Collectors.{} is not supported", name), Some(pos)),
    }
}

fn collect(it: &mut Interp, items: Vec<V>, c: &V, pos: u32) -> R<V> {
    let (kind, args) = match c {
        V::Obj(o) => {
            let b = o.borrow();
            let k = b.props.get("%collector").map(|v| it.display(v)).unwrap_or_default();
            let a = match b.props.get("args") {
                Some(V::Arr(a)) => a.borrow().clone(),
                _ => vec![],
            };
            (k, a)
        }
        _ => return it.type_error("collect() requires a Collector".into(), Some(pos)),
    };
    let arg = |i: usize| args.get(i).cloned().unwrap_or(V::Null);
    Ok(match kind.as_str() {
        "toList" | "toUnmodifiableList" => V::arr(items),
        "toSet" | "toUnmodifiableSet" => crate::builtins::make_set(it, items),
        "joining" => {
            let sep = if args.is_empty() { String::new() } else { it.display(&arg(0)) };
            let parts: Vec<String> = items.iter().map(|x| it.display(x)).collect();
            let pre = if args.len() > 1 { it.display(&arg(1)) } else { String::new() };
            let suf = if args.len() > 2 { it.display(&arg(2)) } else { String::new() };
            V::string(format!("{}{}{}", pre, parts.join(&sep), suf))
        }
        "counting" => V::Int(items.len() as i64),
        "summingInt" | "summingLong" | "summingDouble" | "averagingInt" | "averagingLong" | "averagingDouble" => {
            let f = arg(0);
            let mut sum = 0.0;
            let mut all_int = true;
            for x in &items {
                let v = it.call(&f, V::Undef, vec![x.clone()], pos)?;
                if !matches!(v, V::Int(_)) {
                    all_int = false;
                }
                sum += v.as_f64().unwrap_or(0.0);
            }
            if kind.starts_with("averaging") {
                V::Num(if items.is_empty() { 0.0 } else { sum / items.len() as f64 })
            } else if all_int && kind != "summingDouble" {
                V::Int(sum as i64)
            } else {
                V::Num(sum)
            }
        }
        "mapping" => {
            let f = arg(0);
            let mut mapped = Vec::new();
            for x in items {
                mapped.push(it.call(&f, V::Undef, vec![x], pos)?);
            }
            collect(it, mapped, &arg(1), pos)?
        }
        "groupingBy" | "partitioningBy" => {
            let f = arg(0);
            let mut groups: Vec<(String, V, Vec<V>)> = Vec::new();
            if kind == "partitioningBy" {
                groups.push(("false".into(), V::Bool(false), vec![]));
                groups.push(("true".into(), V::Bool(true), vec![]));
            }
            for x in items {
                let k = it.call(&f, V::Undef, vec![x.clone()], pos)?;
                let ks = it.key_string(&k);
                match groups.iter_mut().find(|(s, _, _)| *s == ks) {
                    Some((_, _, l)) => l.push(x),
                    None => groups.push((ks, k, vec![x])),
                }
            }
            let mut pm = PropMap::default();
            for (ks, _, l) in groups {
                let v = if args.len() > 1 { collect(it, l, &arg(args.len() - 1), pos)? } else { V::arr(l) };
                pm.set(&ks, v);
            }
            if kind == "groupingBy" && args.len() == 3 {
                // groupingBy(f, TreeMap::new, downstream): sorted keys
                pm.sort_keys();
            }
            V::obj(pm)
        }
        "toMap" => {
            let (kf, vf) = (arg(0), arg(1));
            let merge = arg(2);
            let mut pm = PropMap::default();
            for x in items {
                let k = it.call(&kf, V::Undef, vec![x.clone()], pos)?;
                let v = it.call(&vf, V::Undef, vec![x], pos)?;
                let ks = it.key_string(&k);
                let v = match pm.get(&ks).cloned() {
                    Some(old) if merge.is_callable() => it.call(&merge, V::Undef, vec![old, v], pos)?,
                    Some(_) => return it.throw("IllegalStateException", format!("Duplicate key {}", ks), Some(pos)),
                    None => v,
                };
                pm.set(&ks, v);
            }
            V::obj(pm)
        }
        other => return it.type_error(format!("unsupported collector {}", other), Some(pos)),
    })
}

/// `Optional` methods.
pub fn optional_method(it: &mut Interp, o: &ObjRef, recv: &V, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
    let v = o.borrow().props.get("value").cloned();
    let a0 = args.first().cloned().unwrap_or(V::Null);
    Ok(Some(match name {
        "isPresent" => V::Bool(v.is_some()),
        "isEmpty" => V::Bool(v.is_none()),
        "get" | "orElseThrow" | "getAsInt" | "getAsDouble" | "getAsLong" => match v {
            Some(x) => x,
            None => {
                if name == "orElseThrow" && a0.is_callable() {
                    let e = it.call(&a0, V::Undef, vec![], pos)?;
                    return Err(Ctrl::Throw(e));
                }
                return it.throw("NoSuchElementException", "No value present".into(), Some(pos));
            }
        },
        "orElse" => v.unwrap_or(a0),
        "orElseGet" => match v {
            Some(x) => x,
            None => it.call(&a0, V::Undef, vec![], pos)?,
        },
        "map" => match v {
            Some(x) => {
                let r = it.call(&a0, V::Undef, vec![x], pos)?;
                optional(Some(r))
            }
            None => recv.clone(),
        },
        "flatMap" => match v {
            Some(x) => it.call(&a0, V::Undef, vec![x], pos)?,
            None => recv.clone(),
        },
        "filter" => match v {
            Some(x) => {
                let r = it.call(&a0, V::Undef, vec![x.clone()], pos)?;
                if it.truthy(&r) {
                    recv.clone()
                } else {
                    optional(None)
                }
            }
            None => recv.clone(),
        },
        "ifPresent" => {
            if let Some(x) = v {
                it.call(&a0, V::Undef, vec![x], pos)?;
            }
            V::Null
        }
        "ifPresentOrElse" => {
            match v {
                Some(x) => it.call(&a0, V::Undef, vec![x], pos)?,
                None => it.call(&args.get(1).cloned().unwrap_or(V::Null), V::Undef, vec![], pos)?,
            };
            V::Null
        }
        "or" => match v {
            Some(_) => recv.clone(),
            None => it.call(&a0, V::Undef, vec![], pos)?,
        },
        "stream" => V::arr(v.into_iter().collect()),
        "toString" => V::string(it.display(recv)),
        "equals" => V::Bool(crate::interp::deep_eq(recv, &a0)),
        _ => return Ok(None),
    }))
}

fn int_arg(v: &V) -> i64 {
    v.as_f64().unwrap_or(0.0) as i64
}

/// Java-specific methods of values (functional interfaces, streams, collections, strings).
pub fn java_method(it: &mut Interp, recv: &V, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
    let a0 = args.first().cloned().unwrap_or(V::Null);
    let a1 = args.get(1).cloned().unwrap_or(V::Null);
    match recv {
        V::Func(_) | V::Method(_) => {
            return Ok(Some(match name {
                "apply" | "accept" | "test" | "get" | "run" | "call" | "applyAsInt" | "applyAsLong" | "applyAsDouble" | "compare" | "getAsInt" | "getAsDouble" | "getAsLong"
                | "getAsBoolean" => {
                    let r = it.call(recv, V::Undef, args.to_vec(), pos)?;
                    if name == "accept" || name == "run" {
                        V::Null
                    } else {
                        r
                    }
                }
                "andThen" => fn_value(data(&[("f", recv.clone()), ("g", a0)]), "andThen"),
                "compose" => fn_value(data(&[("f", recv.clone()), ("g", a0)]), "compose"),
                "negate" => fn_value(data(&[("f", recv.clone())]), "negate"),
                "and" => fn_value(data(&[("f", recv.clone()), ("g", a0)]), "and"),
                "or" => fn_value(data(&[("f", recv.clone()), ("g", a0)]), "or"),
                "reversed" => fn_value(data(&[("f", recv.clone())]), "reversed"),
                "thenComparing" | "thenComparingInt" | "thenComparingDouble" | "thenComparingLong" => {
                    let g = if a0.is_callable() && crate::builtins::arity(&a0) == 1 { fn_value(data(&[("f", a0)]), "comparing") } else { a0 };
                    fn_value(data(&[("f", recv.clone()), ("g", g)]), "then")
                }
                _ => return Ok(None),
            }));
        }
        V::Arr(a) => {
            let items = a.borrow().clone();
            return Ok(Some(match name {
                "stream" | "parallelStream" | "boxed" | "mapToObj" | "asDoubleStream" | "asLongStream" | "sequential" | "parallel" | "toList" if args.is_empty() || name == "mapToObj" => {
                    if name == "mapToObj" {
                        let mut out = Vec::with_capacity(items.len());
                        for x in items {
                            out.push(it.call(&a0, V::Undef, vec![x], pos)?);
                        }
                        V::arr(out)
                    } else {
                        V::arr(items)
                    }
                }
                "map" | "mapToInt" | "mapToLong" | "mapToDouble" => {
                    let mut out = Vec::with_capacity(items.len());
                    for x in items {
                        out.push(it.call(&a0, V::Undef, vec![x], pos)?);
                    }
                    V::arr(out)
                }
                "filter" => {
                    let mut out = Vec::new();
                    for x in items {
                        let r = it.call(&a0, V::Undef, vec![x.clone()], pos)?;
                        if it.truthy(&r) {
                            out.push(x);
                        }
                    }
                    V::arr(out)
                }
                "peek" => {
                    for x in &items {
                        it.call(&a0, V::Undef, vec![x.clone()], pos)?;
                    }
                    V::arr(items)
                }
                "limit" => V::arr(items.into_iter().take(int_arg(&a0).max(0) as usize).collect()),
                "skip" => V::arr(items.into_iter().skip(int_arg(&a0).max(0) as usize).collect()),
                "count" if args.is_empty() => V::Int(items.len() as i64),
                "anyMatch" | "allMatch" | "noneMatch" => {
                    let mut any = false;
                    let mut all = true;
                    for x in items {
                        let r = it.call(&a0, V::Undef, vec![x], pos)?;
                        if it.truthy(&r) {
                            any = true;
                        } else {
                            all = false;
                        }
                    }
                    V::Bool(match name {
                        "anyMatch" => any,
                        "allMatch" => all,
                        _ => !any,
                    })
                }
                "findFirst" | "findAny" => optional(items.into_iter().next()),
                "reduce" => {
                    if args.len() >= 2 {
                        let mut acc = a0;
                        for x in items {
                            acc = it.call(&a1, V::Undef, vec![acc, x], pos)?;
                        }
                        acc
                    } else {
                        let mut iter = items.into_iter();
                        match iter.next() {
                            None => optional(None),
                            Some(first) => {
                                let mut acc = first;
                                for x in iter {
                                    acc = it.call(&a0, V::Undef, vec![acc, x], pos)?;
                                }
                                optional(Some(acc))
                            }
                        }
                    }
                }
                "max" | "min" if args.len() == 1 && a0.is_callable() => {
                    let mut best: Option<V> = None;
                    for x in items {
                        best = Some(match best {
                            None => x,
                            Some(b) => {
                                let r = it.call(&a0, V::Undef, vec![x.clone(), b.clone()], pos)?;
                                let c = r.as_f64().unwrap_or(0.0);
                                if (name == "max" && c > 0.0) || (name == "min" && c < 0.0) {
                                    x
                                } else {
                                    b
                                }
                            }
                        });
                    }
                    optional(best)
                }
                "max" | "min" if args.is_empty() => {
                    let mut best: Option<V> = None;
                    for x in items {
                        best = Some(match best {
                            None => x,
                            Some(b) => {
                                let o = cmp_int(it, &x, &b);
                                if (name == "max" && o > 0) || (name == "min" && o < 0) {
                                    x
                                } else {
                                    b
                                }
                            }
                        });
                    }
                    optional(best)
                }
                "average" if args.is_empty() => {
                    if items.is_empty() {
                        optional(None)
                    } else {
                        let s: f64 = items.iter().map(|x| x.as_f64().unwrap_or(0.0)).sum();
                        optional(Some(V::Num(s / items.len() as f64)))
                    }
                }
                "collect" if args.len() == 1 => collect(it, items, &a0, pos)?,
                "sorted" => {
                    let mut v = V::arr(items);
                    let a = if args.is_empty() { vec![] } else { vec![a0] };
                    v = crate::builtins::list_method(it, &v, "sort", a, pos)?;
                    v
                }
                "distinct" => crate::builtins::list_method(it, &V::arr(items), "unique", vec![], pos)?,
                "flatMap" => {
                    let mut out = Vec::new();
                    for x in items {
                        let r = it.call(&a0, V::Undef, vec![x], pos)?;
                        out.extend(it.iterate(&r, Some(pos))?);
                    }
                    V::arr(out)
                }
                "forEachOrdered" => {
                    for x in items {
                        it.call(&a0, V::Undef, vec![x], pos)?;
                    }
                    V::Null
                }
                "sort" => {
                    // List.sort(comparator) sorts in place (null: natural order)
                    let cmp = if a0.is_nullish() { vec![] } else { vec![a0] };
                    let sorted = crate::builtins::list_method(it, &V::arr(items), "sort", cmp, pos)?;
                    if let V::Arr(s) = sorted {
                        let v = s.borrow().clone();
                        *a.borrow_mut() = v;
                    }
                    V::Null
                }
                "remove" if args.len() == 1 && matches!(a0, V::Int(_)) => {
                    let i = int_arg(&a0);
                    let len = items.len() as i64;
                    if i < 0 || i >= len {
                        return it.throw("IndexOutOfBoundsException", format!("Index {} out of bounds for length {}", i, len), Some(pos)).map(Some);
                    }
                    a.borrow_mut().remove(i as usize)
                }
                "get" if args.len() == 1 => {
                    let i = int_arg(&a0);
                    let len = items.len() as i64;
                    if i < 0 || i >= len {
                        return it.throw("IndexOutOfBoundsException", format!("Index {} out of bounds for length {}", i, len), Some(pos)).map(Some);
                    }
                    items[i as usize].clone()
                }
                "iterator" => crate::collections::list_iterator(items),
                "hasNext" => return Ok(None),
                _ => return Ok(None),
            }));
        }
        V::Obj(o) => {
            let kind = o.borrow().kind.clone();
            if let ObjKind::Coll(c) = &kind {
                if let crate::collections::Coll::Iter(items, i) = &mut *c.0.borrow_mut() {
                    match name {
                        "hasNext" => return Ok(Some(V::Bool(*i < items.len()))),
                        "next" => {
                            let x = items.get(*i).cloned();
                            *i += 1;
                            return match x {
                                Some(v) => Ok(Some(v)),
                                None => it.throw("NoSuchElementException", String::new(), Some(pos)).map(Some),
                            };
                        }
                        _ => {}
                    }
                }
            }
            if kind == ObjKind::Plain && o.borrow().props.contains("key") && o.borrow().props.contains("value") && o.borrow().props.len() == 2 {
                match name {
                    "getKey" => return Ok(Some(o.borrow().props.get("key").cloned().unwrap_or(V::Null))),
                    "getValue" => return Ok(Some(o.borrow().props.get("value").cloned().unwrap_or(V::Null))),
                    _ => {}
                }
            }
            if kind == ObjKind::Plain {
                let r = match name {
                    "computeIfAbsent" => {
                        let k = it.key_string(&a0);
                        let cur = o.borrow().props.get(&k).cloned();
                        match cur {
                            Some(v) if !v.is_nullish() => v,
                            _ => {
                                let v = it.call(&a1, V::Undef, vec![a0.clone()], pos)?;
                                if !v.is_nullish() {
                                    o.borrow_mut().props.set(&k, v.clone());
                                }
                                v
                            }
                        }
                    }
                    "computeIfPresent" | "compute" => {
                        let k = it.key_string(&a0);
                        let cur = o.borrow().props.get(&k).cloned();
                        if name == "computeIfPresent" && cur.as_ref().is_none_or(|v| v.is_nullish()) {
                            return Ok(Some(V::Null));
                        }
                        let v = it.call(&a1, V::Undef, vec![a0.clone(), cur.unwrap_or(V::Null)], pos)?;
                        if v.is_nullish() {
                            o.borrow_mut().props.remove(&k);
                        } else {
                            o.borrow_mut().props.set(&k, v.clone());
                        }
                        v
                    }
                    "merge" => {
                        let k = it.key_string(&a0);
                        let cur = o.borrow().props.get(&k).cloned();
                        let f = args.get(2).cloned().unwrap_or(V::Null);
                        let v = match cur {
                            Some(old) if !old.is_nullish() => it.call(&f, V::Undef, vec![old, a1], pos)?,
                            _ => a1,
                        };
                        o.borrow_mut().props.set(&k, v.clone());
                        v
                    }
                    "forEach" if crate::builtins::arity(&a0) == 2 => {
                        let entries: Vec<(Rc<str>, V)> = o.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        for (k, v) in entries {
                            it.call(&a0, V::Undef, vec![V::Str(k), v], pos)?;
                        }
                        V::Null
                    }
                    "replaceAll" if a0.is_callable() => {
                        let entries: Vec<(Rc<str>, V)> = o.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        for (k, v) in entries {
                            let nv = it.call(&a0, V::Undef, vec![V::Str(k.clone()), v], pos)?;
                            o.borrow_mut().props.set(&k, nv);
                        }
                        V::Null
                    }
                    _ => return Ok(None),
                };
                return Ok(Some(r));
            }
        }
        V::Str(s) => {
            return Ok(Some(match name {
                "length" if args.is_empty() => V::Int(s.chars().count() as i64),
                "chars" => V::arr(s.chars().map(|c| V::Int(c as i64)).collect()),
                "toCharArray" => V::arr(s.chars().map(|c| V::string(c.to_string())).collect()),
                "formatted" => {
                    let r = crate::format::printf(it, s, args, None);
                    match r {
                        Ok(x) => V::string(x),
                        Err(m) => return it.throw("IllegalFormatException", m, Some(pos)).map(Some),
                    }
                }
                "isBlank" => V::Bool(s.trim().is_empty()),
                "strip" => V::string(s.trim().to_string()),
                "indent" => {
                    let n = int_arg(&a0).max(0) as usize;
                    V::string(s.lines().map(|l| format!("{}{}\n", " ".repeat(n), l)).collect())
                }
                "charAt" => {
                    let i = int_arg(&a0);
                    match s.chars().nth(i.max(0) as usize) {
                        Some(c) if i >= 0 => V::string(c.to_string()),
                        _ => return it.throw("StringIndexOutOfBoundsException", format!("index {}, length {}", i, s.chars().count()), Some(pos)).map(Some),
                    }
                }
                _ => return Ok(None),
            }));
        }
        _ => {}
    }
    let _ = a1;
    Ok(None)
}

/// Static functions of the Java-only namespaces (`Objects`, `Optional`, `Character`,
/// `Collectors`, `Comparator`, `Stream`, `IntStream`, `System.out`, `List.of`...).
pub fn java_static(it: &mut Interp, ns: &str, name: &str, args: Vec<V>, pos: u32) -> R<Option<V>> {
    let a0 = args.first().cloned().unwrap_or(V::Null);
    let a1 = args.get(1).cloned().unwrap_or(V::Null);
    Ok(Some(match (ns, name) {
        ("Comparator" | "Function", _) => comparator_static(it, name, args, pos)?,
        ("Collectors", _) => collectors_static(it, name, args, pos)?,
        ("Optional", "of") => {
            if a0.is_nullish() {
                return it.throw("NullPointerException", "Optional.of(null)".into(), Some(pos)).map(Some);
            }
            optional(Some(a0))
        }
        ("Optional", "ofNullable") => optional(Some(a0)),
        ("Optional", "empty") => optional(None),
        ("Objects", "equals") => V::Bool(crate::interp::deep_eq(&a0, &a1)),
        ("Objects", "isNull") => V::Bool(a0.is_nullish()),
        ("Objects", "nonNull") => V::Bool(!a0.is_nullish()),
        ("Objects", "toString") => {
            if a0.is_nullish() && args.len() > 1 {
                a1
            } else {
                V::string(it.to_str(&a0)?)
            }
        }
        ("Objects", "requireNonNull") => {
            if a0.is_nullish() {
                let msg = if args.len() > 1 { it.display(&a1) } else { String::new() };
                return it.throw("NullPointerException", msg, Some(pos)).map(Some);
            }
            a0
        }
        ("Objects", "requireNonNullElse") => {
            if a0.is_nullish() {
                a1
            } else {
                a0
            }
        }
        ("Objects", "hash") => V::Int(args.iter().fold(1i64, |h, v| h.wrapping_mul(31).wrapping_add(it.display(v).bytes().fold(0i64, |x, b| x.wrapping_mul(31).wrapping_add(b as i64)))) as i32 as i64),
        ("Objects", "hashCode") => V::Int(it.display(&a0).bytes().fold(0i64, |x, b| x.wrapping_mul(31).wrapping_add(b as i64)) as i32 as i64),
        ("Character", f) => {
            let s = it.display(&a0);
            let c = s.chars().next().unwrap_or('\0');
            match f {
                "isDigit" => V::Bool(c.is_ascii_digit()),
                "isLetter" | "isAlphabetic" => V::Bool(c.is_alphabetic()),
                "isLetterOrDigit" => V::Bool(c.is_alphanumeric()),
                "isUpperCase" => V::Bool(c.is_uppercase()),
                "isLowerCase" => V::Bool(c.is_lowercase()),
                "isWhitespace" | "isSpaceChar" => V::Bool(c.is_whitespace()),
                "toUpperCase" => V::string(c.to_uppercase().collect()),
                "toLowerCase" => V::string(c.to_lowercase().collect()),
                "getNumericValue" => V::Int(c.to_digit(36).map(|d| d as i64).unwrap_or(-1)),
                "toString" | "valueOf" => V::string(c.to_string()),
                "toChars" => V::arr(vec![V::string(char::from_u32(a0.as_f64().unwrap_or(0.0) as u32).unwrap_or('\0').to_string())]),
                _ => return Ok(None),
            }
        }
        ("Stream", "of") => V::arr(args),
        ("Stream", "empty") => V::arr(vec![]),
        ("Stream", "concat") => {
            let mut v = it.iterate(&a0, Some(pos))?;
            v.extend(it.iterate(&a1, Some(pos))?);
            V::arr(v)
        }
        ("Stream", "iterate") if args.len() == 3 => {
            let (mut x, test, next) = (a0, a1, args[2].clone());
            let mut out = Vec::new();
            loop {
                let t = it.call(&test, V::Undef, vec![x.clone()], pos)?;
                if !it.truthy(&t) || out.len() > 1_000_000 {
                    break;
                }
                out.push(x.clone());
                x = it.call(&next, V::Undef, vec![x], pos)?;
            }
            V::arr(out)
        }
        ("IntStream" | "LongStream", "range" | "rangeClosed") => {
            let (lo, hi) = (int_arg(&a0), int_arg(&a1) + if name == "rangeClosed" { 1 } else { 0 });
            if hi - lo > 10_000_000 {
                return it.throw("OutOfMemoryError", "range too large".into(), Some(pos)).map(Some);
            }
            V::arr((lo..hi.max(lo)).map(V::Int).collect())
        }
        ("IntStream" | "LongStream" | "DoubleStream", "of") => V::arr(args),
        ("SystemOut", "println" | "print" | "printf" | "format") => {
            let line = if name == "printf" || name == "format" {
                let f = it.display(&a0);
                crate::format::printf(it, &f, &args[1.min(args.len())..], None).unwrap_or(f)
            } else {
                it.to_str(&a0)?
            };
            camel_core::log::log(camel_core::log::Level::Info, "camel.script", line);
            V::Null
        }
        ("List" | "Set", "of" | "copyOf") => {
            let items = if name == "copyOf" { it.iterate(&a0, Some(pos))? } else { args };
            if ns == "Set" {
                crate::builtins::make_set(it, items)
            } else {
                V::arr(items)
            }
        }
        ("Map", "of") => {
            let mut pm = PropMap::default();
            for kv in args.chunks(2) {
                let k = it.key_string(&kv[0]);
                pm.set(&k, kv.get(1).cloned().unwrap_or(V::Null));
            }
            V::obj(pm)
        }
        ("Map", "entry") => crate::builtins::entry(&it.key_string(&a0), a1),
        ("Map", "ofEntries") => {
            let mut pm = PropMap::default();
            for e in args {
                let k = it.get_member(&e, "key", pos)?;
                let v = it.get_member(&e, "value", pos)?;
                pm.set(&it.key_string(&k), v);
            }
            V::obj(pm)
        }
        ("Map", "copyOf") => match &a0 {
            V::Obj(o) => V::obj(o.borrow().props.clone()),
            _ => V::new_obj(),
        },
        ("JavaString", f) => match f {
            "valueOf" | "copyValueOf" => V::string(it.to_str(&a0)?),
            "join" => {
                let sep = it.display(&a0);
                let parts: Vec<V> = if args.len() == 2 && matches!(a1, V::Arr(_) | V::Obj(_)) { it.iterate(&a1, Some(pos))? } else { args[1..].to_vec() };
                V::string(parts.iter().map(|p| it.display(p)).collect::<Vec<_>>().join(&sep))
            }
            "format" => {
                let f = it.display(&a0);
                V::string(crate::format::printf(it, &f, &args[1.min(args.len())..], None).unwrap_or(f))
            }
            _ => return Ok(None),
        },
        ("JavaSystem", "currentTimeMillis") => V::Int(camel_core::util_time::now_millis() as i64),
        ("JavaSystem", "nanoTime") => V::Int((camel_core::util_time::now_millis() as i64).wrapping_mul(1_000_000)),
        ("JavaSystem", "lineSeparator") => V::str("\n"),
        _ => return Ok(None),
    }))
}

