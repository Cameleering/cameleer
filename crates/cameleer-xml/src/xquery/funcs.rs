//! The XQuery function library (a useful slice of XPath/XQuery Functions and Operators).

use std::collections::HashSet;

use cameleer_core::regex::Regex;

use crate::dom::NodeType;
use crate::xpath::{Docs, XNode};

use super::ast::{Expr, QN};
use super::eval::{arith, untyped_to_string, Focus, Interp, Seq, R};
use super::types::*;
use super::{FN_NS, MATH_NS};

use super::ast::Arith;

fn arity_err(local: &str, n: usize) -> XqError {
    XqError::static_err("XPST0017", format!("Unknown function {}() with {} argument(s)", local, n))
}

/// Translate XPath regex flags into an inline-flag prefix for the Java-style engine.
fn compile_regex(pattern: &str, flags: &str) -> R<Regex> {
    let mut prefix = String::new();
    let mut pat = pattern.to_string();
    for c in flags.chars() {
        match c {
            'i' | 's' | 'm' | 'x' => prefix.push(c),
            'q' => pat = Regex::quote(pattern),
            _ => return Err(XqError::new("FORX0001", format!("Invalid regular expression flags: {}", flags))),
        }
    }
    let full = if prefix.is_empty() { pat } else { format!("(?{}){}", prefix, pat) };
    Regex::new(&full).map_err(|e| XqError::new("FORX0002", format!("Invalid regular expression '{}': {}", pattern, e.message)))
}

fn xpath_round(f: f64) -> f64 {
    if f.is_nan() || f.is_infinite() {
        return f;
    }
    (f + 0.5).floor()
}

fn round_half_even(f: f64, prec: i64) -> f64 {
    let m = 10f64.powi(prec as i32);
    let x = f * m;
    let r = x.round();
    let r = if (x - x.trunc()).abs() == 0.5 { 2.0 * (x / 2.0).round() } else { r };
    r / m
}

fn map_numeric(a: Atomic, f: impl Fn(f64) -> f64) -> R<Atomic> {
    Ok(match a {
        Atomic::Int(i) => Atomic::Int(f(i as f64) as i64),
        Atomic::Dec(d) => Atomic::Dec(Decimal::from_f64(f(d.to_f64())).unwrap_or_default()),
        Atomic::Flt(d) => Atomic::Flt(f(d)),
        Atomic::Dbl(d) => Atomic::Dbl(f(d)),
        Atomic::Untyped(_) => Atomic::Dbl(f(cast(&a, AtomicType::Double)?.as_f64())),
        other => return Err(XqError::type_err(format!("Expected a numeric value, found {}", other.type_of().name()))),
    })
}

fn distinct_key(a: &Atomic) -> String {
    match a {
        x if x.is_numeric() => {
            let f = x.as_f64();
            if f.is_nan() {
                "n:NaN".into()
            } else {
                format!("n:{}", f)
            }
        }
        Atomic::Untyped(s) | Atomic::Str(s) | Atomic::AnyUri(s) => format!("s:{}", s),
        other => format!("{}:{}", other.type_of().name(), other),
    }
}

fn substring(s: &str, start: f64, len: Option<f64>) -> String {
    let chars: Vec<char> = s.chars().collect();
    let st = xpath_round(start);
    let end = match len {
        Some(l) => st + xpath_round(l),
        None => f64::INFINITY,
    };
    chars.iter().enumerate().filter(|(i, _)| {
        let p = (*i + 1) as f64;
        p >= st && p < end
    }).map(|(_, c)| *c).collect()
}

fn date_part(a: &Atomic, part: &str) -> R<Item> {
    let s = a.to_string();
    let (date, time) = match a {
        Atomic::DateTime(_) => s.split_once('T').map(|(d, t)| (d.to_string(), t.to_string())).unwrap_or_default(),
        Atomic::Date(_) => (s.clone(), String::new()),
        Atomic::Time(_) => (String::new(), s.clone()),
        Atomic::Untyped(_) | Atomic::Str(_) => {
            if s.contains('T') {
                s.split_once('T').map(|(d, t)| (d.to_string(), t.to_string())).unwrap_or_default()
            } else if s.contains(':') {
                (String::new(), s.clone())
            } else {
                (s.clone(), String::new())
            }
        }
        other => return Err(XqError::type_err(format!("Expected a date/time value, found {}", other.type_of().name()))),
    };
    let neg = date.starts_with('-');
    let dparts: Vec<&str> = date.trim_start_matches('-').splitn(3, '-').collect();
    let tbody: String = time.chars().take_while(|c| c.is_ascii_digit() || *c == ':' || *c == '.').collect();
    let tparts: Vec<&str> = tbody.split(':').collect();
    let int = |x: Option<&&str>| -> R<i64> { x.and_then(|v| v.get(..2.min(v.len())).map(|_| *v)).and_then(|v| v.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()).ok_or_else(|| XqError::new("FORG0001", format!("Invalid date/time value '{}'", s))) };
    Ok(match part {
        "year" => {
            let y = dparts.first().and_then(|v| v.parse::<i64>().ok()).ok_or_else(|| XqError::new("FORG0001", format!("Invalid date value '{}'", s)))?;
            Item::int(if neg { -y } else { y })
        }
        "month" => Item::int(int(dparts.get(1))?),
        "day" => Item::int(int(dparts.get(2))?),
        "hours" => Item::int(int(tparts.first())?),
        "minutes" => Item::int(int(tparts.get(1))?),
        "seconds" => {
            let v = tparts.get(2).ok_or_else(|| XqError::new("FORG0001", format!("Invalid time value '{}'", s)))?;
            if v.contains('.') {
                Item::Atomic(Atomic::Dec(Decimal::parse(v).unwrap_or_default()))
            } else {
                Item::int(v.parse().unwrap_or(0))
            }
        }
        _ => unreachable!(),
    })
}

/// Deep equality of two sequences (`fn:deep-equal`).
pub(crate) fn deep_equal(docs: &Docs, a: &[Item], b: &[Item]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| match (x, y) {
            (Item::Atomic(p), Item::Atomic(q)) => {
                if p.is_numeric() && q.is_numeric() && p.as_f64().is_nan() && q.as_f64().is_nan() {
                    return true;
                }
                matches!(compare_atomic(&untyped_to_string(p.clone()), &untyped_to_string(q.clone())), Ok(Some(std::cmp::Ordering::Equal)))
            }
            (Item::Node(p), Item::Node(q)) => nodes_deep_equal(docs, *p, *q),
            _ => false,
        })
}

fn nodes_deep_equal(docs: &Docs, a: XNode, b: XNode) -> bool {
    let (ta, tb) = (docs.node_type(a), docs.node_type(b));
    if ta != tb {
        return false;
    }
    let (da, db) = (docs.get(a.doc), docs.get(b.doc));
    match ta {
        Some(NodeType::Element) => {
            if !da.name(a.node_id()).same_expanded(db.name(b.node_id())) {
                return false;
            }
            let aa = da.attributes(a.node_id());
            let ab = db.attributes(b.node_id());
            if aa.len() != ab.len() {
                return false;
            }
            for &x in aa {
                let n = da.name(x);
                if db.attribute_ns(b.node_id(), &n.uri, &n.local) != Some(da.value(x)) {
                    return false;
                }
            }
            children_equal(docs, a, b)
        }
        Some(NodeType::Document) => children_equal(docs, a, b),
        Some(NodeType::Attribute) | Some(NodeType::ProcessingInstruction) => da.name(a.node_id()).same_expanded(db.name(b.node_id())) && da.value(a.node_id()) == db.value(b.node_id()),
        _ => docs.string_value(a) == docs.string_value(b),
    }
}

fn children_equal(docs: &Docs, a: XNode, b: XNode) -> bool {
    let kids = |n: XNode| -> Vec<XNode> {
        let d = docs.get(n.doc);
        d.children(n.node_id()).iter().copied().filter(|&c| matches!(d.node_type(c), NodeType::Element | NodeType::Text)).map(|c| XNode::new(n.doc, c)).collect()
    };
    let (ka, kb) = (kids(a), kids(b));
    ka.len() == kb.len() && ka.iter().zip(&kb).all(|(x, y)| nodes_deep_equal(docs, *x, *y))
}

fn encode_for_uri(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Call a built-in function.
pub(crate) fn call_builtin(it: &mut Interp, name: &QN, args: &[Expr], f: &Focus) -> R<Seq> {
    let local = name.local.as_str();
    let n = args.len();
    if name.uri == MATH_NS {
        let mut vals = Vec::new();
        for a in args {
            let v = it.eval(a, f)?;
            vals.push(it.atomize_opt(&v, "a math function argument")?.map(|a| a.as_f64()));
        }
        let x = vals.first().copied().flatten();
        let r = match (local, n) {
            ("pi", 0) => Some(std::f64::consts::PI),
            ("sqrt", 1) => x.map(f64::sqrt),
            ("exp", 1) => x.map(f64::exp),
            ("exp10", 1) => x.map(|v| 10f64.powf(v)),
            ("log", 1) => x.map(f64::ln),
            ("log10", 1) => x.map(f64::log10),
            ("sin", 1) => x.map(f64::sin),
            ("cos", 1) => x.map(f64::cos),
            ("tan", 1) => x.map(f64::tan),
            ("asin", 1) => x.map(f64::asin),
            ("acos", 1) => x.map(f64::acos),
            ("atan", 1) => x.map(f64::atan),
            ("pow", 2) => match (x, vals[1]) {
                (Some(a), Some(b)) => Some(a.powf(b)),
                _ => None,
            },
            ("atan2", 2) => match (x, vals[1]) {
                (Some(a), Some(b)) => Some(a.atan2(b)),
                _ => None,
            },
            _ => return Err(arity_err(local, n)),
        };
        return Ok(r.map(|v| vec![Item::dbl(v)]).unwrap_or_default());
    }
    if name.uri != FN_NS {
        return Err(XqError::static_err("XPST0017", format!("Unknown function {}() in namespace '{}'", local, name.uri)));
    }
    // focus-dependent functions without arguments
    let ctx_arg = n == 0 && matches!(local, "string" | "data" | "number" | "normalize-space" | "string-length" | "name" | "local-name" | "namespace-uri" | "root" | "node-name" | "generate-id" | "base-uri" | "document-uri");
    match (local, n) {
        ("position", 0) => {
            return if f.item.is_some() { Ok(vec![Item::int(f.pos as i64)]) } else { Err(XqError::new("XPDY0002", "position(): the context item is absent")) };
        }
        ("last", 0) => {
            return if f.item.is_some() { Ok(vec![Item::int(f.size as i64)]) } else { Err(XqError::new("XPDY0002", "last(): the context item is absent")) };
        }
        ("true", 0) => return Ok(vec![Item::bool(true)]),
        ("false", 0) => return Ok(vec![Item::bool(false)]),
        ("current-dateTime", 0) => return Ok(vec![Item::Atomic(Atomic::DateTime(it.now.clone()))]),
        ("current-date", 0) => return Ok(vec![Item::Atomic(cast(&Atomic::DateTime(it.now.clone()), AtomicType::Date)?)]),
        ("current-time", 0) => return Ok(vec![Item::Atomic(cast(&Atomic::DateTime(it.now.clone()), AtomicType::Time)?)]),
        _ => {}
    }
    let mut a: Vec<Seq> = Vec::with_capacity(n.max(1));
    if ctx_arg {
        match &f.item {
            Some(i) => a.push(vec![i.clone()]),
            None => return Err(XqError::new("XPDY0002", format!("{}(): the context item is absent", local))),
        }
    } else {
        for e in args {
            a.push(it.eval(e, f)?);
        }
    }
    let na = a.len();
    // helpers
    let opt_str = |it: &Interp, s: &Seq| -> R<String> { Ok(it.atomize_opt(s, &format!("an argument of {}()", local))?.map(|x| x.to_string()).unwrap_or_default()) };
    let opt_num = |it: &Interp, s: &Seq| -> R<Option<Atomic>> {
        Ok(match it.atomize_opt(s, &format!("an argument of {}()", local))? {
            None => None,
            Some(x @ Atomic::Untyped(_)) => Some(cast(&x, AtomicType::Double)?),
            Some(x) if x.is_numeric() => Some(x),
            Some(x) => return Err(XqError::type_err(format!("{}() expects a numeric argument, found {}", local, x.type_of().name()))),
        })
    };
    let num = |it: &Interp, s: &Seq| -> R<f64> { Ok(opt_num(it, s)?.map(|x| x.as_f64()).unwrap_or(f64::NAN)) };
    let one_node = |s: &Seq| -> R<Option<XNode>> {
        match s.as_slice() {
            [] => Ok(None),
            [Item::Node(n)] => Ok(Some(*n)),
            [Item::Atomic(_)] => Err(XqError::type_err(format!("{}() expects a node argument", local))),
            _ => Err(XqError::type_err(format!("{}() expects a single node", local))),
        }
    };
    let s1 = |s: String| Ok(vec![Item::str(s)]);
    let b1 = |b: bool| Ok(vec![Item::bool(b)]);
    match (local, na) {
        ("string", 1) => {
            let s = match a[0].as_slice() {
                [] => String::new(),
                [x] => it.string_value(x),
                _ => return Err(XqError::type_err("string() expects at most one item")),
            };
            s1(s)
        }
        ("data", 1) => Ok(it.atomize(&a[0]).into_iter().map(Item::Atomic).collect()),
        ("concat", k) if k >= 2 => {
            let mut s = String::new();
            for x in &a {
                s.push_str(&opt_str(it, x)?);
            }
            s1(s)
        }
        ("string-join", 1 | 2) => {
            let sep = if na == 2 { opt_str(it, &a[1])? } else { String::new() };
            s1(it.atomize(&a[0]).iter().map(|x| x.to_string()).collect::<Vec<_>>().join(&sep))
        }
        ("substring", 2 | 3) => {
            let s = opt_str(it, &a[0])?;
            let st = num(it, &a[1])?;
            let len = if na == 3 { Some(num(it, &a[2])?) } else { None };
            s1(substring(&s, st, len))
        }
        ("substring-before", 2) => {
            let (s, t) = (opt_str(it, &a[0])?, opt_str(it, &a[1])?);
            s1(s.find(&t).map(|i| s[..i].to_string()).unwrap_or_default())
        }
        ("substring-after", 2) => {
            let (s, t) = (opt_str(it, &a[0])?, opt_str(it, &a[1])?);
            s1(s.find(&t).map(|i| s[i + t.len()..].to_string()).unwrap_or_default())
        }
        ("contains", 2) => b1(opt_str(it, &a[0])?.contains(&opt_str(it, &a[1])?)),
        ("starts-with", 2) => b1(opt_str(it, &a[0])?.starts_with(&opt_str(it, &a[1])?)),
        ("ends-with", 2) => b1(opt_str(it, &a[0])?.ends_with(&opt_str(it, &a[1])?)),
        ("upper-case", 1) => s1(opt_str(it, &a[0])?.to_uppercase()),
        ("lower-case", 1) => s1(opt_str(it, &a[0])?.to_lowercase()),
        ("normalize-space", 1) => s1(opt_str(it, &a[0])?.split_whitespace().collect::<Vec<_>>().join(" ")),
        ("string-length", 1) => {
            let s = match a[0].as_slice() {
                [] => String::new(),
                [x] => it.string_value(x),
                _ => return Err(XqError::type_err("string-length() expects at most one item")),
            };
            Ok(vec![Item::int(s.chars().count() as i64)])
        }
        ("translate", 3) => {
            let s = opt_str(it, &a[0])?;
            let from: Vec<char> = opt_str(it, &a[1])?.chars().collect();
            let to: Vec<char> = opt_str(it, &a[2])?.chars().collect();
            s1(s.chars().filter_map(|c| match from.iter().position(|&x| x == c) {
                Some(i) => to.get(i).copied(),
                None => Some(c),
            }).collect())
        }
        ("compare", 2) => {
            let (Some(x), Some(y)) = (it.atomize_opt(&a[0], "compare()")?, it.atomize_opt(&a[1], "compare()")?) else { return Ok(Vec::new()) };
            Ok(vec![Item::int(match x.to_string().cmp(&y.to_string()) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })])
        }
        ("codepoints-to-string", 1) => {
            let mut s = String::new();
            for x in it.atomize(&a[0]) {
                let cp = super::eval::to_integer(&x)?;
                s.push(char::from_u32(cp as u32).ok_or_else(|| XqError::new("FOCH0001", format!("Invalid codepoint {}", cp)))?);
            }
            s1(s)
        }
        ("string-to-codepoints", 1) => Ok(opt_str(it, &a[0])?.chars().map(|c| Item::int(c as i64)).collect()),
        ("encode-for-uri", 1) => s1(encode_for_uri(&opt_str(it, &a[0])?)),
        ("matches", 2 | 3) => {
            let s = opt_str(it, &a[0])?;
            let flags = if na == 3 { opt_str(it, &a[2])? } else { String::new() };
            let re = compile_regex(&opt_str(it, &a[1])?, &flags)?;
            b1(re.is_match(&s))
        }
        ("replace", 3 | 4) => {
            let s = opt_str(it, &a[0])?;
            let flags = if na == 4 { opt_str(it, &a[3])? } else { String::new() };
            let pat = opt_str(it, &a[1])?;
            let re = compile_regex(&pat, &flags)?;
            if re.is_full_match("") {
                return Err(XqError::new("FORX0003", format!("The pattern '{}' matches a zero-length string", pat)));
            }
            let rep = opt_str(it, &a[2])?;
            let rep = if flags.contains('q') { rep.replace('\\', "\\\\").replace('$', "\\$") } else { rep };
            s1(re.replace_all(&s, &rep))
        }
        ("tokenize", 1..=3) => {
            let s = opt_str(it, &a[0])?;
            if na == 1 {
                return Ok(s.split_whitespace().map(Item::str).collect());
            }
            if s.is_empty() {
                return Ok(Vec::new());
            }
            let flags = if na == 3 { opt_str(it, &a[2])? } else { String::new() };
            let pat = opt_str(it, &a[1])?;
            let re = compile_regex(&pat, &flags)?;
            if re.is_full_match("") {
                return Err(XqError::new("FORX0003", format!("The pattern '{}' matches a zero-length string", pat)));
            }
            let mut out = Vec::new();
            let mut last = 0;
            for (st, en) in re.find_iter(&s) {
                out.push(Item::str(&s[last..st]));
                last = en;
            }
            out.push(Item::str(&s[last..]));
            Ok(out)
        }
        ("count", 1) => Ok(vec![Item::int(a[0].len() as i64)]),
        ("exists", 1) => b1(!a[0].is_empty()),
        ("empty", 1) => b1(a[0].is_empty()),
        ("boolean", 1) => b1(it.ebv(&a[0])?),
        ("not", 1) => b1(!it.ebv(&a[0])?),
        ("sum", 1 | 2) => {
            let vals = it.atomize(&a[0]);
            if vals.is_empty() {
                return Ok(if na == 2 { a[1].clone() } else { vec![Item::int(0)] });
            }
            let mut acc: Option<Atomic> = None;
            for v in vals {
                acc = Some(match acc {
                    None => match v {
                        Atomic::Untyped(_) => cast(&v, AtomicType::Double)?,
                        x if x.is_numeric() => x,
                        x => return Err(XqError::type_err(format!("sum(): cannot add a value of type {}", x.type_of().name()))),
                    },
                    Some(x) => arith(Arith::Add, &x, &v)?,
                });
            }
            Ok(acc.into_iter().map(Item::Atomic).collect())
        }
        ("avg", 1) => {
            let vals = it.atomize(&a[0]);
            if vals.is_empty() {
                return Ok(Vec::new());
            }
            let count = vals.len() as i64;
            let mut acc = Atomic::Int(0);
            for v in vals {
                acc = arith(Arith::Add, &acc, &v)?;
            }
            Ok(vec![Item::Atomic(arith(Arith::Div, &acc, &Atomic::Int(count))?)])
        }
        ("min" | "max", 1 | 2) => {
            let vals: Vec<Atomic> = it
                .atomize(&a[0])
                .into_iter()
                .map(|v| if let Atomic::Untyped(_) = v { cast(&v, AtomicType::Double) } else { Ok(v) })
                .collect::<R<_>>()?;
            let mut best: Option<Atomic> = None;
            for v in vals {
                if v.is_numeric() && v.as_f64().is_nan() {
                    return Ok(vec![Item::Atomic(v)]);
                }
                best = Some(match best {
                    None => v,
                    Some(b) => {
                        let o = compare_atomic(&v, &b)?.unwrap_or(std::cmp::Ordering::Equal);
                        let take = if local == "min" { o == std::cmp::Ordering::Less } else { o == std::cmp::Ordering::Greater };
                        // numeric promotion of the result type
                        let (w, l) = if take { (v, b) } else { (b, v) };
                        if w.is_numeric() && l.is_numeric() && matches!(l, Atomic::Dbl(_)) && !matches!(w, Atomic::Dbl(_)) {
                            Atomic::Dbl(w.as_f64())
                        } else {
                            w
                        }
                    }
                });
            }
            Ok(best.into_iter().map(Item::Atomic).collect())
        }
        ("distinct-values", 1 | 2) => {
            let mut seen = HashSet::new();
            let mut out = Vec::new();
            for v in it.atomize(&a[0]) {
                if seen.insert(distinct_key(&v)) {
                    out.push(Item::Atomic(v));
                }
            }
            Ok(out)
        }
        ("reverse", 1) => {
            let mut v = a[0].clone();
            v.reverse();
            Ok(v)
        }
        ("subsequence", 2 | 3) => {
            let st = xpath_round(num(it, &a[1])?);
            let end = if na == 3 { st + xpath_round(num(it, &a[2])?) } else { f64::INFINITY };
            Ok(a[0].iter().enumerate().filter(|(i, _)| {
                let p = (*i + 1) as f64;
                p >= st && p < end
            }).map(|(_, x)| x.clone()).collect())
        }
        ("head", 1) => Ok(a[0].first().cloned().into_iter().collect()),
        ("tail", 1) => Ok(a[0].iter().skip(1).cloned().collect()),
        ("insert-before", 3) => {
            let pos = super::eval::to_integer(&it.atomize_opt(&a[1], "insert-before() position")?.unwrap_or(Atomic::Int(1)))?;
            let mut v = a[0].clone();
            let idx = (pos.max(1) as usize - 1).min(v.len());
            for (k, x) in a[2].iter().enumerate() {
                v.insert(idx + k, x.clone());
            }
            Ok(v)
        }
        ("remove", 2) => {
            let pos = super::eval::to_integer(&it.atomize_opt(&a[1], "remove() position")?.unwrap_or(Atomic::Int(0)))?;
            Ok(a[0].iter().enumerate().filter(|(i, _)| (*i as i64 + 1) != pos).map(|(_, x)| x.clone()).collect())
        }
        ("index-of", 2 | 3) => {
            let Some(target) = it.atomize_opt(&a[1], "index-of() search value")? else { return Ok(Vec::new()) };
            let target = untyped_to_string(target);
            let mut out = Vec::new();
            for (i, v) in it.atomize(&a[0]).into_iter().enumerate() {
                if let Ok(Some(std::cmp::Ordering::Equal)) = compare_atomic(&untyped_to_string(v), &target) {
                    out.push(Item::int(i as i64 + 1));
                }
            }
            Ok(out)
        }
        ("unordered", 1) => Ok(a[0].clone()),
        ("zero-or-one", 1) => {
            if a[0].len() > 1 {
                return Err(XqError::new("FORG0003", "zero-or-one() called with a sequence containing more than one item"));
            }
            Ok(a[0].clone())
        }
        ("one-or-more", 1) => {
            if a[0].is_empty() {
                return Err(XqError::new("FORG0004", "one-or-more() called with an empty sequence"));
            }
            Ok(a[0].clone())
        }
        ("exactly-one", 1) => {
            if a[0].len() != 1 {
                return Err(XqError::new("FORG0005", "exactly-one() called with a sequence not containing exactly one item"));
            }
            Ok(a[0].clone())
        }
        ("deep-equal", 2 | 3) => b1(deep_equal(it.docs, &a[0], &a[1])),
        ("number", 1) => {
            let v = match it.atomize_opt(&a[0], "number()")? {
                None => f64::NAN,
                Some(x) => cast(&x, AtomicType::Double).map(|d| d.as_f64()).unwrap_or(f64::NAN),
            };
            Ok(vec![Item::dbl(v)])
        }
        ("abs", 1) => Ok(opt_num(it, &a[0])?.map(|x| map_numeric(x, f64::abs)).transpose()?.into_iter().map(Item::Atomic).collect()),
        ("floor", 1) => Ok(opt_num(it, &a[0])?.map(|x| map_numeric(x, f64::floor)).transpose()?.into_iter().map(Item::Atomic).collect()),
        ("ceiling", 1) => Ok(opt_num(it, &a[0])?.map(|x| map_numeric(x, f64::ceil)).transpose()?.into_iter().map(Item::Atomic).collect()),
        ("round", 1 | 2) => {
            let prec = if na == 2 { super::eval::to_integer(&it.atomize_opt(&a[1], "round() precision")?.unwrap_or(Atomic::Int(0)))? } else { 0 };
            let m = 10f64.powi(prec as i32);
            Ok(opt_num(it, &a[0])?.map(|x| map_numeric(x, |v| xpath_round(v * m) / m)).transpose()?.into_iter().map(Item::Atomic).collect())
        }
        ("round-half-to-even", 1 | 2) => {
            let prec = if na == 2 { super::eval::to_integer(&it.atomize_opt(&a[1], "precision")?.unwrap_or(Atomic::Int(0)))? } else { 0 };
            Ok(opt_num(it, &a[0])?.map(|x| map_numeric(x, |v| round_half_even(v, prec))).transpose()?.into_iter().map(Item::Atomic).collect())
        }
        ("name" | "local-name" | "namespace-uri" | "node-name", 1) => {
            let Some(nd) = one_node(&a[0])? else {
                return Ok(if local == "node-name" { Vec::new() } else { vec![Item::str("")] });
            };
            let (p, l, u) = it.docs.node_name(nd);
            Ok(match local {
                "name" => vec![Item::str(if p.is_empty() { l } else { format!("{}:{}", p, l) })],
                "local-name" => vec![Item::str(l)],
                "namespace-uri" => vec![Item::Atomic(Atomic::AnyUri(u))],
                _ => {
                    if l.is_empty() {
                        Vec::new()
                    } else {
                        vec![Item::Atomic(Atomic::QName(p, l, u))]
                    }
                }
            })
        }
        ("root", 1) => {
            let Some(nd) = one_node(&a[0])? else { return Ok(Vec::new()) };
            let d = it.docs.get(nd.doc);
            Ok(vec![Item::Node(XNode::new(nd.doc, d.tree_root(nd.node_id())))])
        }
        ("generate-id", 1) => {
            let Some(nd) = one_node(&a[0])? else { return s1(String::new()) };
            s1(format!("d{}n{}", nd.doc, nd.id))
        }
        ("base-uri" | "document-uri", 1) => {
            let Some(nd) = one_node(&a[0])? else { return Ok(Vec::new()) };
            Ok(it.docs.get(nd.doc).base_uri.clone().map(|u| vec![Item::Atomic(Atomic::AnyUri(u))]).unwrap_or_default())
        }
        ("doc" | "doc-available", 1) => {
            let uri = opt_str(it, &a[0])?;
            if uri.is_empty() {
                return Ok(Vec::new());
            }
            let r = load_doc(it, &uri);
            if local == "doc-available" {
                return b1(r.is_ok());
            }
            Ok(vec![Item::Node(r?)])
        }
        ("parse-xml", 1) => {
            let s = opt_str(it, &a[0])?;
            let d = crate::dom::Document::parse(&s).map_err(|e| XqError::new("FODC0006", format!("parse-xml(): {}", e)))?;
            Ok(vec![Item::Node(XNode::new(it.add_doc(d), 0))])
        }
        ("serialize", 1 | 2) => s1(super::serialize_items(it.docs, &a[0])),
        ("error", 0..=3) => {
            let code = match a.first().map(|s| it.atomize(s)) {
                Some(v) if !v.is_empty() => match &v[0] {
                    Atomic::QName(_, l, _) => l.clone(),
                    other => {
                        let s = other.to_string();
                        s.rsplit(':').next().unwrap_or(&s).to_string()
                    }
                },
                _ => "FOER0000".into(),
            };
            let desc = if na >= 2 { opt_str(it, &a[1])? } else { "Error signalled by fn:error()".into() };
            Err(XqError::new(&code, desc))
        }
        ("trace", 1 | 2) => {
            let label = if na == 2 { opt_str(it, &a[1])? } else { String::new() };
            cameleer_core::camel_debug!("xquery", "trace {}: {}", label, super::serialize_items(it.docs, &a[0]));
            Ok(a[0].clone())
        }
        ("year-from-dateTime" | "year-from-date", 1) => date_fn(it, &a[0], "year"),
        ("month-from-dateTime" | "month-from-date", 1) => date_fn(it, &a[0], "month"),
        ("day-from-dateTime" | "day-from-date", 1) => date_fn(it, &a[0], "day"),
        ("hours-from-dateTime" | "hours-from-time", 1) => date_fn(it, &a[0], "hours"),
        ("minutes-from-dateTime" | "minutes-from-time", 1) => date_fn(it, &a[0], "minutes"),
        ("seconds-from-dateTime" | "seconds-from-time", 1) => date_fn(it, &a[0], "seconds"),
        ("years-from-duration" | "months-from-duration" | "days-from-duration" | "hours-from-duration" | "minutes-from-duration" | "seconds-from-duration", 1) => {
            match it.atomize_opt(&a[0], "a duration argument")? {
                None => Ok(Vec::new()),
                Some(Atomic::Untyped(s)) => Ok(vec![Item::Atomic(super::datetime::duration_component(&Atomic::Duration(s), local.split('-').next().unwrap())?)]),
                Some(d) => Ok(vec![Item::Atomic(super::datetime::duration_component(&d, local.split('-').next().unwrap())?)]),
            }
        }
        ("adjust-dateTime-to-timezone" | "adjust-date-to-timezone" | "adjust-time-to-timezone", 1 | 2) => {
            let Some(v) = it.atomize_opt(&a[0], "a date/time argument")? else { return Ok(Vec::new()) };
            let v = match v {
                Atomic::Untyped(s) => cast(&Atomic::Untyped(s), if local.contains("dateTime") { AtomicType::DateTime } else if local.contains("date") { AtomicType::Date } else { AtomicType::Time })?,
                other => other,
            };
            let tz = if na == 2 {
                match it.atomize_opt(&a[1], "a timezone argument")? {
                    None => Some(None),
                    Some(d) => {
                        let us = super::datetime::Duration::of(&d).ok_or_else(|| XqError::type_err("The timezone must be an xs:dayTimeDuration"))?.micros;
                        if us % 60_000_000 != 0 || us.abs() > 14 * 3_600_000_000 {
                            return Err(XqError::new("FODT0003", format!("Invalid timezone {}", d)));
                        }
                        Some(Some((us / 60_000_000) as i32))
                    }
                }
            } else {
                None
            };
            Ok(vec![Item::Atomic(super::datetime::adjust(&v, tz)?)])
        }
        ("dateTime", 2) => {
            let (Some(d), Some(t)) = (it.atomize_opt(&a[0], "a date argument")?, it.atomize_opt(&a[1], "a time argument")?) else { return Ok(Vec::new()) };
            let d = cast(&d, AtomicType::Date)?;
            let t = cast(&t, AtomicType::Time)?;
            Ok(vec![Item::Atomic(super::datetime::date_time(&d, &t)?)])
        }
        ("timezone-from-dateTime" | "timezone-from-date" | "timezone-from-time", 1) => {
            let Some(v) = it.atomize_opt(&a[0], "a date/time argument")? else { return Ok(Vec::new()) };
            Ok(super::datetime::timezone_of(&v).map(Item::Atomic).into_iter().collect())
        }
        ("local-name-from-QName" | "namespace-uri-from-QName" | "prefix-from-QName", 1) => match it.atomize_opt(&a[0], "a QName argument")? {
            None => Ok(Vec::new()),
            Some(Atomic::QName(p, l, u)) => Ok(vec![Item::str(match local {
                "local-name-from-QName" => l,
                "namespace-uri-from-QName" => u,
                _ => p,
            })]),
            Some(other) => Err(XqError::type_err(format!("{}() expects an xs:QName, found {}", local, other.type_of().name()))),
        },
        ("implicit-timezone", 0) => Ok(vec![Item::Atomic(Atomic::Duration("PT0S".into()))]),
        ("lang", 1 | 2) => {
            let node = if na == 2 {
                one_node(&a[1])?
            } else {
                match &f.item {
                    Some(Item::Node(n)) => Some(*n),
                    _ => return Err(XqError::new("XPDY0002", "lang(): the context item is not a node")),
                }
            };
            let want = opt_str(it, &a[0])?.to_lowercase();
            let Some(nd) = node else { return b1(false) };
            let l = it.docs.get(nd.doc).lang(nd.node_id()).unwrap_or_default().to_lowercase();
            b1(l == want || l.starts_with(&format!("{}-", want)))
        }
        _ => Err(arity_err(local, na)),
    }
}

fn date_fn(it: &Interp, s: &Seq, part: &str) -> R<Seq> {
    match it.atomize_opt(s, "a date/time argument")? {
        None => Ok(Vec::new()),
        Some(a) => Ok(vec![date_part(&a, part)?]),
    }
}

/// `fn:doc`: local files only (relative to the query file's directory, then the working directory).
fn load_doc(it: &mut Interp, uri: &str) -> R<XNode> {
    let u = uri.trim();
    if u.contains("://") && !u.starts_with("file://") {
        return Err(XqError::new("FODC0002", format!("doc(): only local files are supported: {}", u)));
    }
    let path_s = u.strip_prefix("file://").or_else(|| u.strip_prefix("file:")).or_else(|| u.strip_prefix("classpath:")).unwrap_or(u);
    let mut path = std::path::PathBuf::from(path_s);
    if path.is_relative() {
        if let Some(b) = &it.base_dir {
            let cand = b.join(&path);
            if cand.exists() {
                path = cand;
            }
        }
    }
    let key = path.display().to_string();
    if let Some(n) = it.doc_cache.get(&key) {
        return Ok(*n);
    }
    let bytes = std::fs::read(&path).map_err(|e| XqError::new("FODC0002", format!("doc(): cannot read {}: {}", key, e)))?;
    let mut d = crate::dom::Document::parse_bytes(&bytes).map_err(|e| XqError::new("FODC0002", format!("doc(): {} is not well-formed: {}", key, e)))?;
    d.base_uri = Some(key.clone());
    let n = XNode::new(it.add_doc(d), 0);
    it.doc_cache.insert(key, n);
    Ok(n)
}
