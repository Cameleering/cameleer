//! jq evaluator: continuation-passing generators with path tracking.
//!
//! Every expression is evaluated against a [`Pv`] (a value plus, in path mode, the path
//! that produced it) and pushes each output into a callback. Control flow that must cut a
//! generator short (`label`/`break`, `limit`, `first`, errors) unwinds through [`Ctl`].

use std::cell::Cell;
use std::cmp::Ordering;
use std::rc::Rc;

use super::parse::{AssignOp, Ast, BinOp, FuncDef, ObjKey, Pattern, StrPart};
use crate::message::Exchange;
use crate::value::{ObjectMap, Value};

/// A path into a value (keys, indices and slice objects).
pub(crate) type Path = Vec<Value>;

/// A value and, in path mode, its path.
#[derive(Clone, Debug)]
pub(crate) struct Pv {
    pub v: Value,
    pub p: Option<Path>,
}

impl Pv {
    pub fn val(v: Value) -> Pv {
        Pv { v, p: None }
    }
}

/// Non-local control flow.
#[derive(Debug)]
pub(crate) enum Ctl {
    /// A jq error carrying its value.
    Err(Value),
    /// `break` to the label with this id.
    Break(u64),
    /// An error raised downstream of a `try`/`//`/`?//` body, passed through untouched.
    Through(u64, Box<Ctl>),
}

pub(crate) type R = std::result::Result<(), Ctl>;
pub(crate) type Res<T> = std::result::Result<T, Ctl>;
pub(crate) type Cb<'c> = &'c mut dyn FnMut(Pv) -> R;

thread_local! {
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

pub(crate) fn next_id() -> u64 {
    NEXT_ID.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    })
}

pub(crate) fn err<T>(m: impl Into<String>) -> Res<T> {
    Err(Ctl::Err(Value::String(m.into())))
}

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

pub(crate) enum Bind<'a> {
    Var(&'a str, Value),
    Func(&'a FuncDef),
    Closure(&'a str, &'a Ast, Env<'a>),
    Label(&'a str, u64),
}

pub(crate) struct Node<'a> {
    b: Bind<'a>,
    next: Env<'a>,
}

#[derive(Clone, Default)]
pub(crate) struct Env<'a>(Option<Rc<Node<'a>>>);

impl<'a> Env<'a> {
    pub fn push(&self, b: Bind<'a>) -> Env<'a> {
        Env(Some(Rc::new(Node { b, next: self.clone() })))
    }

    fn var(&self, name: &str) -> Option<Value> {
        let mut cur = &self.0;
        while let Some(n) = cur {
            if let Bind::Var(v, val) = &n.b {
                if *v == name {
                    return Some(val.clone());
                }
            }
            cur = &n.next.0;
        }
        None
    }

    fn label(&self, name: &str) -> Option<u64> {
        let mut cur = &self.0;
        while let Some(n) = cur {
            if let Bind::Label(l, id) = &n.b {
                if *l == name {
                    return Some(*id);
                }
            }
            cur = &n.next.0;
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Value helpers
// ---------------------------------------------------------------------------

/// A jq number: integral values within the safe range become `Int`.
pub(crate) fn num(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15 {
        Value::Int(f as i64)
    } else {
        Value::Float(f)
    }
}

pub(crate) fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
}

/// jq type name.
pub(crate) fn tname(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Int(_) | Value::Float(_) => "number",
        Value::String(_) | Value::Bytes(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// jq's JSON rendering.
pub(crate) fn to_json(v: &Value) -> String {
    match v {
        Value::Float(f) if !f.is_finite() => {
            if f.is_nan() {
                "null".into()
            } else if *f > 0.0 {
                "1.7976931348623157e+308".into()
            } else {
                "-1.7976931348623157e+308".into()
            }
        }
        Value::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e17 {
                format!("{}", *f as i128)
            } else {
                let s = format!("{}", f);
                if s.len() > 17 && !s.contains('e') && f.abs() >= 1e17 {
                    format!("{:e}", f).replace("e", "e+")
                } else {
                    s
                }
            }
        }
        Value::Array(a) => {
            let mut s = String::from("[");
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&to_json(x));
            }
            s.push(']');
            s
        }
        Value::Object(o) => {
            let mut s = String::from("{");
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                crate::value::json_escape(k, &mut s);
                s.push(':');
                s.push_str(&to_json(x));
            }
            s.push('}');
            s
        }
        Value::String(st) => {
            let mut s = String::new();
            crate::value::json_escape(st, &mut s);
            s.replace('\u{7f}', "\\u007f")
        }
        other => other.to_json(),
    }
}

/// Value dump truncated like jq's error messages.
pub(crate) fn dump_trunc(v: &Value) -> String {
    let s = to_json(v);
    if s.chars().count() > 11 {
        format!("{}...", s.chars().take(10).collect::<String>())
    } else {
        s
    }
}

pub(crate) fn tostring(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        other => to_json(other),
    }
}

pub(crate) fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn rank(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(false) => 1,
        Value::Bool(true) => 2,
        Value::Int(_) | Value::Float(_) => 3,
        Value::String(_) | Value::Bytes(_) => 4,
        Value::Array(_) => 5,
        Value::Object(_) => 6,
    }
}

/// jq's total ordering.
pub(crate) fn cmp(a: &Value, b: &Value) -> Ordering {
    let (ra, rb) = (rank(a), rank(b));
    if ra != rb {
        return ra.cmp(&rb);
    }
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Int(_) | Value::Float(_), _) => as_f64(a).unwrap().partial_cmp(&as_f64(b).unwrap()).unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::String(_) | Value::Bytes(_), _) => tostring(a).cmp(&tostring(b)),
        (Value::Array(x), Value::Array(y)) => {
            for (p, q) in x.iter().zip(y.iter()) {
                let c = cmp(p, q);
                if c != Ordering::Equal {
                    return c;
                }
            }
            x.len().cmp(&y.len())
        }
        (Value::Object(x), Value::Object(y)) => {
            let mut kx: Vec<&String> = x.keys().collect();
            let mut ky: Vec<&String> = y.keys().collect();
            kx.sort();
            ky.sort();
            match kx.cmp(&ky) {
                Ordering::Equal => {
                    for k in kx {
                        let c = cmp(&x[k.as_str()], &y[k.as_str()]);
                        if c != Ordering::Equal {
                            return c;
                        }
                    }
                    Ordering::Equal
                }
                o => o,
            }
        }
        _ => Ordering::Equal,
    }
}

pub(crate) fn sorted_keys(o: &ObjectMap) -> Vec<String> {
    let mut k: Vec<String> = o.keys().cloned().collect();
    k.sort();
    k
}

fn index_num(len: usize, f: f64) -> Option<usize> {
    let i = f.floor() as i64;
    let i = if i < 0 { len as i64 + i } else { i };
    if i < 0 || i >= len as i64 {
        None
    } else {
        Some(i as usize)
    }
}

/// Resolve slice bounds (jq 1.7.1: start floors, end ceils) over a length.
pub(crate) fn slice_bounds(len: usize, from: &Value, to: &Value) -> Res<(usize, usize)> {
    let conv = |v: &Value, dflt: i64, is_end: bool| -> Res<i64> {
        match v {
            Value::Null => Ok(dflt),
            v => {
                let f = as_f64(v).ok_or_else(|| Ctl::Err(Value::from("Start and end indices of an array slice must be numbers")))?;
                let i = if is_end { f.ceil() } else { f.floor() };
                let i = if i.is_infinite() { if i > 0.0 { len as i64 } else { -(len as i64) - 1 } } else { i as i64 };
                Ok(if i < 0 { (len as i64 + i).max(0) } else { i.min(len as i64) })
            }
        }
    };
    let s = conv(from, 0, false)?;
    let e = conv(to, len as i64, true)?;
    let s = s.clamp(0, len as i64) as usize;
    let e = (e.clamp(0, len as i64) as usize).max(s);
    Ok((s, e))
}

fn slice_key(k: &Value) -> Option<(Value, Value)> {
    if let Value::Object(o) = k {
        if o.len() <= 2 && o.keys().all(|k| k == "start" || k == "end") && o.contains_key("start") && o.contains_key("end") {
            return Some((o.get("start").cloned().unwrap_or(Value::Null), o.get("end").cloned().unwrap_or(Value::Null)));
        }
    }
    None
}

pub(crate) fn slice(v: &Value, from: &Value, to: &Value) -> Res<Value> {
    match v {
        Value::Null => Ok(Value::Null),
        Value::Array(a) => {
            let (s, e) = slice_bounds(a.len(), from, to)?;
            Ok(Value::Array(a[s..e].to_vec()))
        }
        Value::String(st) => {
            let chars: Vec<char> = st.chars().collect();
            let (s, e) = slice_bounds(chars.len(), from, to)?;
            Ok(Value::String(chars[s..e].iter().collect()))
        }
        other => err(format!("Cannot index {} with object", tname(other))),
    }
}

/// `v[k]`.
pub(crate) fn index(v: &Value, k: &Value) -> Res<Value> {
    match (v, k) {
        (Value::Object(o), Value::String(s)) => Ok(o.get(s).cloned().unwrap_or(Value::Null)),
        (Value::Array(a), Value::Int(_) | Value::Float(_)) => Ok(index_num(a.len(), as_f64(k).unwrap()).map(|i| a[i].clone()).unwrap_or(Value::Null)),
        (Value::Null, Value::String(_) | Value::Int(_) | Value::Float(_) | Value::Null) => Ok(Value::Null),
        (Value::Array(a), Value::Array(needle)) => Ok(indices_of(a, needle)),
        (_, Value::Object(_)) if slice_key(k).is_some() => {
            let (s, e) = slice_key(k).unwrap();
            slice(v, &s, &e)
        }
        (Value::Null, Value::Object(_)) => Ok(Value::Null),
        (_, Value::String(s)) => err(format!("Cannot index {} with \"{}\"", tname(v), s)),
        _ => err(format!("Cannot index {} with {}", tname(v), tname(k))),
    }
}

pub(crate) fn indices_of(a: &[Value], needle: &[Value]) -> Value {
    if needle.is_empty() {
        return Value::Null;
    }
    let mut out = Vec::new();
    if a.len() >= needle.len() {
        for i in 0..=(a.len() - needle.len()) {
            if a[i..i + needle.len()].iter().zip(needle).all(|(x, y)| cmp(x, y) == Ordering::Equal) {
                out.push(Value::Int(i as i64));
            }
        }
    }
    Value::Array(out)
}

/// `getpath(p)`.
pub(crate) fn getpath(v: &Value, path: &[Value]) -> Res<Value> {
    let mut cur = v.clone();
    for k in path {
        if cur.is_null() {
            return Ok(Value::Null);
        }
        cur = index(&cur, k)?;
    }
    Ok(cur)
}

/// `setpath(p; x)`.
pub(crate) fn setpath(v: Value, path: &[Value], x: Value) -> Res<Value> {
    let Some((k, rest)) = path.split_first() else { return Ok(x) };
    if let Some((s, e)) = slice_key(k) {
        return match v {
            Value::Null | Value::Array(_) => {
                let mut a = match v {
                    Value::Array(a) => a,
                    _ => Vec::new(),
                };
                let (si, ei) = slice_bounds(a.len(), &s, &e)?;
                let cur = Value::Array(a[si..ei].to_vec());
                let newv = setpath(cur, rest, x)?;
                match newv {
                    Value::Array(items) => {
                        a.splice(si..ei, items);
                        Ok(Value::Array(a))
                    }
                    _ => err("A slice of an array can only be assigned another array"),
                }
            }
            other => err(format!("Cannot update field at object index of {}", tname(&other))),
        };
    }
    match (v, k) {
        (Value::Object(mut o), Value::String(s)) => {
            let child = o.get_mut(s).map(std::mem::take).unwrap_or_default();
            let nv = setpath(child, rest, x)?;
            o.insert(s.clone(), nv);
            Ok(Value::Object(o))
        }
        (Value::Null, Value::String(s)) => {
            let mut o = ObjectMap::new();
            o.insert(s.clone(), setpath(Value::Null, rest, x)?);
            Ok(Value::Object(o))
        }
        (v @ (Value::Array(_) | Value::Null), Value::Int(_) | Value::Float(_)) => {
            let mut a = match v {
                Value::Array(a) => a,
                _ => Vec::new(),
            };
            let f = as_f64(k).unwrap().floor() as i64;
            let i = if f < 0 {
                let j = a.len() as i64 + f;
                if j < 0 {
                    return err("Out of bounds negative array index");
                }
                j as usize
            } else {
                f as usize
            };
            if i > 100_000_000 {
                return err("Array index too large");
            }
            if i >= a.len() {
                a.resize(i + 1, Value::Null);
            }
            let child = std::mem::take(&mut a[i]);
            a[i] = setpath(child, rest, x)?;
            Ok(Value::Array(a))
        }
        (Value::Object(_), _) => err(format!("Cannot index object with {}", tname(k))),
        (Value::Array(_), Value::String(s)) => err(format!("Cannot index array with \"{}\"", s)),
        (other, Value::String(s)) => err(format!("Cannot index {} with \"{}\"", tname(&other), s)),
        (other, _) => err(format!("Cannot index {} with {}", tname(&other), tname(k))),
    }
}

/// `setpath` (object keys keep their position).
pub(crate) fn setpath_ordered(v: Value, path: &[Value], x: Value) -> Res<Value> {
    setpath(v, path, x)
}

fn delpath(v: Value, path: &[Value]) -> Res<Value> {
    let Some((k, rest)) = path.split_first() else { return Ok(Value::Null) };
    if v.is_null() {
        return Ok(Value::Null);
    }
    if rest.is_empty() {
        if let Some((s, e)) = slice_key(k) {
            return match v {
                Value::Array(mut a) => {
                    let (si, ei) = slice_bounds(a.len(), &s, &e)?;
                    a.drain(si..ei);
                    Ok(Value::Array(a))
                }
                other => err(format!("Cannot delete slice of {}", tname(&other))),
            };
        }
        return match (v, k) {
            (Value::Object(mut o), Value::String(s)) => {
                o.remove(s);
                Ok(Value::Object(o))
            }
            (Value::Array(mut a), Value::Int(_) | Value::Float(_)) => {
                if let Some(i) = index_num(a.len(), as_f64(k).unwrap()) {
                    a.remove(i);
                }
                Ok(Value::Array(a))
            }
            (other, Value::String(s)) => err(format!("Cannot delete field at object index of {} (\"{}\")", tname(&other), s)),
            (other, _) => err(format!("Cannot delete field at index of {}", tname(&other))),
        };
    }
    let child = getpath(&v, std::slice::from_ref(k))?;
    if child.is_null() {
        return Ok(v);
    }
    let nc = delpath(child, rest)?;
    setpath_ordered(v, std::slice::from_ref(k), nc)
}

/// `delpaths(ps)`: delete longest/last paths first.
pub(crate) fn delpaths(v: Value, paths: &[Value]) -> Res<Value> {
    let mut ps: Vec<&Vec<Value>> = Vec::new();
    for p in paths {
        match p {
            Value::Array(a) => ps.push(a),
            _ => return err("Path must be specified as an array"),
        }
    }
    let mut sorted: Vec<Value> = ps.iter().map(|p| Value::Array((*p).clone())).collect();
    sorted.sort_by(cmp);
    sorted.dedup_by(|a, b| cmp(a, b) == Ordering::Equal);
    let mut cur = v;
    for p in sorted.iter().rev() {
        if let Value::Array(p) = p {
            cur = delpath(cur, p)?;
        }
    }
    Ok(cur)
}

fn arith_err(a: &Value, b: &Value, verb: &str) -> Ctl {
    Ctl::Err(Value::String(format!("{} ({}) and {} ({}) cannot be {}", tname(a), dump_trunc(a), tname(b), dump_trunc(b), verb)))
}

fn deep_merge(a: &ObjectMap, b: &ObjectMap) -> ObjectMap {
    let mut m = a.clone();
    for (k, v) in b {
        let merged = match (m.get(k), v) {
            (Some(Value::Object(x)), Value::Object(y)) => Value::Object(deep_merge(x, y)),
            _ => v.clone(),
        };
        m.insert(k.clone(), merged);
    }
    m
}

/// Binary arithmetic and comparison operators.
pub(crate) fn binop(op: BinOp, a: &Value, b: &Value) -> Res<Value> {
    use BinOp::*;
    match op {
        Eq => return Ok(Value::Bool(cmp(a, b) == Ordering::Equal)),
        Ne => return Ok(Value::Bool(cmp(a, b) != Ordering::Equal)),
        Lt => return Ok(Value::Bool(cmp(a, b) == Ordering::Less)),
        Le => return Ok(Value::Bool(cmp(a, b) != Ordering::Greater)),
        Gt => return Ok(Value::Bool(cmp(a, b) == Ordering::Greater)),
        Ge => return Ok(Value::Bool(cmp(a, b) != Ordering::Less)),
        _ => {}
    }
    match (op, a, b) {
        (Add, Value::Null, x) | (Add, x, Value::Null) => Ok(x.clone()),
        (Add, Value::Int(x), Value::Int(y)) => Ok(x.checked_add(*y).map(Value::Int).unwrap_or_else(|| num(*x as f64 + *y as f64))),
        (Sub, Value::Int(x), Value::Int(y)) => Ok(x.checked_sub(*y).map(Value::Int).unwrap_or_else(|| num(*x as f64 - *y as f64))),
        (Mul, Value::Int(x), Value::Int(y)) => Ok(x.checked_mul(*y).map(Value::Int).unwrap_or_else(|| num(*x as f64 * *y as f64))),
        (Mod, Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            let (x, y) = (as_f64(a).unwrap(), as_f64(b).unwrap());
            if x.is_nan() || y.is_nan() {
                return Ok(Value::Float(f64::NAN));
            }
            let yi = trunc_i64(y);
            if yi == 0 {
                return Err(Ctl::Err(Value::String(format!("{} ({}) and {} ({}) cannot be divided because the divisor is zero", tname(a), dump_trunc(a), tname(b), dump_trunc(b)))));
            }
            let xi = trunc_i64(x);
            Ok(Value::Int(if yi == -1 { 0 } else { xi % yi }))
        }
        (Div, Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            let (x, y) = (as_f64(a).unwrap(), as_f64(b).unwrap());
            if y == 0.0 {
                return Err(Ctl::Err(Value::String(format!("{} ({}) and {} ({}) cannot be divided because the divisor is zero", tname(a), dump_trunc(a), tname(b), dump_trunc(b)))));
            }
            Ok(num(x / y))
        }
        (Add | Sub | Mul, Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            let (x, y) = (as_f64(a).unwrap(), as_f64(b).unwrap());
            Ok(num(match op {
                Add => x + y,
                Sub => x - y,
                _ => x * y,
            }))
        }
        (Add, Value::String(x), Value::String(y)) => Ok(Value::String(format!("{}{}", x, y))),
        (Add, Value::Array(x), Value::Array(y)) => Ok(Value::Array(x.iter().chain(y.iter()).cloned().collect())),
        (Add, Value::Object(x), Value::Object(y)) => {
            let mut m = x.clone();
            for (k, v) in y {
                m.insert(k.clone(), v.clone());
            }
            Ok(Value::Object(m))
        }
        (Sub, Value::Array(x), Value::Array(y)) => Ok(Value::Array(x.iter().filter(|i| !y.iter().any(|j| cmp(i, j) == Ordering::Equal)).cloned().collect())),
        (Mul, Value::String(s), n @ (Value::Int(_) | Value::Float(_))) | (Mul, n @ (Value::Int(_) | Value::Float(_)), Value::String(s)) => {
            let f = as_f64(n).unwrap();
            if f <= 0.0 {
                return Ok(Value::Null);
            }
            let times = if f < 1.0 { 1 } else { f as usize };
            Ok(Value::String(s.repeat(times)))
        }
        (Mul, Value::Object(x), Value::Object(y)) => Ok(Value::Object(deep_merge(x, y))),
        (Div, Value::String(x), Value::String(y)) => Ok(split_str(x, y)),
        (Add, _, _) => Err(arith_err(a, b, "added")),
        (Sub, _, _) => Err(arith_err(a, b, "subtracted")),
        (Mul, _, _) => Err(arith_err(a, b, "multiplied")),
        _ => Err(arith_err(a, b, "divided")),
    }
}

fn trunc_i64(f: f64) -> i64 {
    if f >= i64::MAX as f64 {
        i64::MAX
    } else if f <= i64::MIN as f64 {
        i64::MIN
    } else {
        f as i64
    }
}

pub(crate) fn split_str(s: &str, sep: &str) -> Value {
    if s.is_empty() {
        return Value::Array(vec![]);
    }
    if sep.is_empty() {
        return Value::Array(s.chars().map(|c| Value::String(c.to_string())).collect());
    }
    Value::Array(s.split(sep).map(|p| Value::String(p.to_string())).collect())
}

// ---------------------------------------------------------------------------
// Interpreter
// ---------------------------------------------------------------------------

/// Evaluation context: the optional exchange (for Camel's `header`/`property` functions).
pub(crate) struct Interp<'x> {
    pub ex: Option<&'x Exchange>,
}

impl<'x> Interp<'x> {
    /// All outputs of `ast` as values (finite generators only).
    pub(crate) fn values<'a>(&self, ast: &'a Ast, env: &Env<'a>, v: &Value) -> Res<Vec<Value>> {
        let mut out = Vec::new();
        self.eval(ast, env, Pv::val(v.clone()), &mut |x| {
            out.push(x.v);
            Ok(())
        })?;
        Ok(out)
    }

    /// All paths produced by `ast` on `v`.
    pub(crate) fn paths<'a>(&self, ast: &'a Ast, env: &Env<'a>, v: &Value) -> Res<Vec<Path>> {
        let mut out = Vec::new();
        self.eval(ast, env, Pv { v: v.clone(), p: Some(Vec::new()) }, &mut |x| match x.p {
            Some(p) => {
                out.push(p);
                Ok(())
            }
            None => err(format!("Invalid path expression with result {}", dump_trunc(&x.v))),
        })?;
        Ok(out)
    }

    /// Evaluate argument expressions as values and call `f` for each combination.
    pub(crate) fn with_args<'a>(&self, args: &'a [Ast], env: &Env<'a>, v: &Value, f: &mut dyn FnMut(&[Value]) -> R) -> R {
        fn go<'a>(me: &Interp<'_>, args: &'a [Ast], env: &Env<'a>, v: &Value, acc: &mut Vec<Value>, f: &mut dyn FnMut(&[Value]) -> R) -> R {
            if acc.len() == args.len() {
                return f(acc);
            }
            let i = acc.len();
            me.eval(&args[i], env, Pv::val(v.clone()), &mut |x| {
                acc.push(x.v);
                let r = go(me, args, env, v, acc, f);
                acc.pop();
                r
            })
        }
        let mut acc = Vec::with_capacity(args.len());
        go(self, args, env, v, &mut acc, f)
    }

    /// Run `body`, catching its errors (not those raised by `cb`). Returns `Ok(None)` when
    /// it finished, `Ok(Some(err))` when it raised `err`.
    pub(crate) fn catching(&self, cb: Cb<'_>, body: &mut dyn FnMut(Cb<'_>) -> R) -> Res<Option<Value>> {
        let id = next_id();
        let r = body(&mut |x| cb(x).map_err(|c| Ctl::Through(id, Box::new(c))));
        match r {
            Ok(()) => Ok(None),
            Err(Ctl::Through(i, c)) if i == id => Err(*c),
            Err(Ctl::Err(v)) => Ok(Some(v)),
            Err(other) => Err(other),
        }
    }

    pub(crate) fn eval<'a>(&self, ast: &'a Ast, env: &Env<'a>, inp: Pv, cb: Cb<'_>) -> R {
        match ast {
            Ast::Identity => cb(inp),
            Ast::RecurseAll => self.recurse_default(inp, cb),
            Ast::Lit(v) => cb(Pv::val(v.clone())),
            Ast::Loc(line) => cb(Pv::val(Value::object([("file", Value::from("<stdin>")), ("line", Value::Int(*line as i64))]))),
            Ast::Var(name) => {
                let v = match env.var(name) {
                    Some(v) => v,
                    None if name == "ENV" => super::builtins::env_object(),
                    None if name == "__prog_args" => Value::Array(vec![]),
                    None => return err(format!("${} is not defined", name)),
                };
                cb(Pv::val(v))
            }
            Ast::Str(fmt, parts) => self.string(fmt.as_deref(), parts, 0, String::new(), env, &inp.v, cb),
            Ast::Format(f) => {
                let s = super::builtins::format(f, &inp.v)?;
                cb(Pv::val(Value::String(s)))
            }
            Ast::Index(t, k) => {
                let keys = self.values(k, env, &inp.v)?;
                for key in keys {
                    self.eval(t, env, inp.clone(), &mut |tp: Pv| {
                        let v = index(&tp.v, &key)?;
                        let p = tp.p.map(|mut p| {
                            p.push(key.clone());
                            p
                        });
                        cb(Pv { v, p })
                    })?;
                }
                Ok(())
            }
            Ast::Slice(t, from, to) => {
                let froms = match from {
                    Some(f) => self.values(f, env, &inp.v)?,
                    None => vec![Value::Null],
                };
                let tos = match to {
                    Some(t) => self.values(t, env, &inp.v)?,
                    None => vec![Value::Null],
                };
                for e in &tos {
                    for s in &froms {
                        self.eval(t, env, inp.clone(), &mut |tp: Pv| {
                            let v = slice(&tp.v, s, e)?;
                            let p = tp.p.map(|mut p| {
                                p.push(Value::object([("start", s.clone()), ("end", e.clone())]));
                                p
                            });
                            cb(Pv { v, p })
                        })?;
                    }
                }
                Ok(())
            }
            Ast::Iterate(t) => self.eval(t, env, inp, &mut |tp: Pv| iterate(tp, cb)),
            Ast::Try(body, catch) => {
                let caught = self.catching(cb, &mut |c| self.eval(body, env, inp.clone(), c))?;
                match (caught, catch) {
                    (Some(e), Some(handler)) => {
                        // `error(null)` inside try/catch: catch receives null.
                        self.eval(handler, env, Pv::val(e), cb)
                    }
                    _ => Ok(()),
                }
            }
            Ast::Array(None) => cb(Pv::val(Value::Array(vec![]))),
            Ast::Array(Some(e)) => {
                let items = self.values(e, env, &inp.v)?;
                cb(Pv::val(Value::Array(items)))
            }
            Ast::Object(entries) => self.object(entries, 0, ObjectMap::new(), env, &inp.v, cb),
            Ast::Neg(e) => self.eval(e, env, Pv::val(inp.v.clone()), &mut |x| match &x.v {
                Value::Int(i) => cb(Pv::val(i.checked_neg().map(Value::Int).unwrap_or_else(|| num(-(*i as f64))))),
                Value::Float(f) => cb(Pv::val(num(-f))),
                other => err(format!("{} ({}) cannot be negated", tname(other), dump_trunc(other))),
            }),
            Ast::Pipe(a, b) => self.eval(a, env, inp, &mut |x| self.eval(b, env, x, cb)),
            Ast::Comma(a, b) => {
                self.eval(a, env, inp.clone(), cb)?;
                self.eval(b, env, inp, cb)
            }
            Ast::Bin(op, a, b) => {
                let v = Pv::val(inp.v.clone());
                self.eval(b, env, v.clone(), &mut |rb| self.eval(a, env, v.clone(), &mut |ra| cb(Pv::val(binop(*op, &ra.v, &rb.v)?))))
            }
            Ast::And(a, b) => {
                let v = Pv::val(inp.v.clone());
                self.eval(a, env, v.clone(), &mut |ra| {
                    if !truthy(&ra.v) {
                        cb(Pv::val(Value::Bool(false)))
                    } else {
                        self.eval(b, env, v.clone(), &mut |rb| cb(Pv::val(Value::Bool(truthy(&rb.v)))))
                    }
                })
            }
            Ast::Or(a, b) => {
                let v = Pv::val(inp.v.clone());
                self.eval(a, env, v.clone(), &mut |ra| {
                    if truthy(&ra.v) {
                        cb(Pv::val(Value::Bool(true)))
                    } else {
                        self.eval(b, env, v.clone(), &mut |rb| cb(Pv::val(Value::Bool(truthy(&rb.v)))))
                    }
                })
            }
            Ast::Alt(a, b) => {
                let mut any = false;
                let _ = self.catching(cb, &mut |c| {
                    self.eval(a, env, inp.clone(), &mut |x| {
                        if truthy(&x.v) {
                            any = true;
                            c(x)
                        } else {
                            Ok(())
                        }
                    })
                })?;
                if any {
                    Ok(())
                } else {
                    self.eval(b, env, inp, cb)
                }
            }
            Ast::Assign(op, lhs, rhs) => self.assign(*op, lhs, rhs, env, inp, cb),
            Ast::If(branches, otherwise) => self.if_branch(branches, 0, otherwise.as_deref(), env, inp, cb),
            Ast::Reduce(src, pat, init, upd) => self.eval(init, env, inp.clone(), &mut |init_v| {
                let mut state = Some(init_v);
                self.eval(src, env, Pv::val(inp.v.clone()), &mut |x| {
                    for env2 in self.destructure(pat, &x.v, env, &inp.v)? {
                        let cur = state.take().unwrap_or(Pv::val(Value::Null));
                        let mut last = None;
                        self.eval(upd, &env2, cur, &mut |o| {
                            last = Some(o);
                            Ok(())
                        })?;
                        state = Some(last.unwrap_or(Pv::val(Value::Null)));
                    }
                    Ok(())
                })?;
                cb(state.take().unwrap_or(Pv::val(Value::Null)))
            }),
            Ast::Foreach(src, pat, init, upd, ext) => self.eval(init, env, inp.clone(), &mut |init_v| {
                let mut state = Some(init_v);
                self.eval(src, env, Pv::val(inp.v.clone()), &mut |x| {
                    for env2 in self.destructure(pat, &x.v, env, &inp.v)? {
                        let cur = state.take().unwrap_or(Pv::val(Value::Null));
                        let mut outs = Vec::new();
                        self.eval(upd, &env2, cur.clone(), &mut |o| {
                            outs.push(o);
                            Ok(())
                        })?;
                        for o in outs {
                            state = Some(o.clone());
                            match ext {
                                Some(e) => self.eval(e, &env2, o, cb)?,
                                None => cb(o)?,
                            }
                        }
                        if state.is_none() {
                            state = Some(cur);
                        }
                    }
                    Ok(())
                })
            }),
            Ast::Bind(src, pats, body) => self.eval(src, env, Pv::val(inp.v.clone()), &mut |x| {
                if pats.len() == 1 {
                    for env2 in self.destructure(&pats[0], &x.v, env, &inp.v)? {
                        self.eval(body, &env2, inp.clone(), cb)?;
                    }
                    return Ok(());
                }
                // `?//` alternatives: all variables are bound (null when absent); an error in
                // the body with one pattern moves on to the next.
                let mut all_vars = Vec::new();
                for p in pats.iter() {
                    p.vars(&mut all_vars);
                }
                let mut base = env.clone();
                for v in &all_vars {
                    base = base.push(Bind::Var(leak_name(pats, v), Value::Null));
                }
                let n = pats.len();
                for (i, pat) in pats.iter().enumerate() {
                    let last = i + 1 == n;
                    let caught = self.catching(cb, &mut |c| {
                        for env2 in self.destructure(pat, &x.v, &base, &inp.v)? {
                            self.eval(body, &env2, inp.clone(), c)?;
                        }
                        Ok(())
                    })?;
                    match caught {
                        None => return Ok(()),
                        Some(e) if last => return Err(Ctl::Err(e)),
                        Some(_) => continue,
                    }
                }
                Ok(())
            }),
            Ast::Label(name, body) => {
                let id = next_id();
                let env2 = env.push(Bind::Label(name, id));
                match self.eval(body, &env2, inp, cb) {
                    Err(Ctl::Break(i)) if i == id => Ok(()),
                    r => r,
                }
            }
            Ast::Break(name) => match env.label(name) {
                Some(id) => Err(Ctl::Break(id)),
                None => err(format!("$*label-{} is not defined", name)),
            },
            Ast::Call(name, args) => self.call(name, args, env, inp, cb),
            Ast::Defs(defs, body) => {
                let mut env2 = env.clone();
                for d in defs {
                    env2 = env2.push(Bind::Func(d));
                }
                self.eval(body, &env2, inp, cb)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn string<'a>(&self, fmt: Option<&str>, parts: &'a [StrPart], i: usize, acc: String, env: &Env<'a>, v: &Value, cb: Cb<'_>) -> R {
        if i == parts.len() {
            return cb(Pv::val(Value::String(acc)));
        }
        match &parts[i] {
            StrPart::Lit(s) => self.string(fmt, parts, i + 1, acc + s, env, v, cb),
            StrPart::Interp(e) => self.eval(e, env, Pv::val(v.clone()), &mut |x| {
                let s = match fmt {
                    Some(f) => super::builtins::format(f, &x.v)?,
                    None => tostring(&x.v),
                };
                self.string(fmt, parts, i + 1, format!("{}{}", acc, s), env, v, cb)
            }),
        }
    }

    fn object_key<'a>(&self, key: &'a ObjKey, env: &Env<'a>, v: &Value, f: &mut dyn FnMut(String, Option<Value>) -> R) -> R {
        match key {
            ObjKey::Lit(s) => f(s.clone(), None),
            ObjKey::Var(name) => {
                let val = env.var(name).or_else(|| if name == "ENV" { Some(super::builtins::env_object()) } else { None }).ok_or_else(|| Ctl::Err(Value::String(format!("${} is not defined", name))))?;
                f(name.clone(), Some(val))
            }
            ObjKey::Loc(line) => f("__loc__".into(), Some(Value::object([("file", Value::from("<stdin>")), ("line", Value::Int(*line as i64))]))),
            ObjKey::Str(fmt, parts) => self.string(fmt.as_deref(), parts, 0, String::new(), env, v, &mut |x| f(tostring(&x.v), None)),
            ObjKey::Expr(e) => self.eval(e, env, Pv::val(v.clone()), &mut |x| match x.v {
                Value::String(s) => f(s, None),
                other => err(format!("Object keys must be strings (got {})", tname(&other))),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn object<'a>(&self, entries: &'a [(ObjKey, Option<Ast>)], i: usize, acc: ObjectMap, env: &Env<'a>, v: &Value, cb: Cb<'_>) -> R {
        if i == entries.len() {
            return cb(Pv::val(Value::Object(acc)));
        }
        let (key, val) = &entries[i];
        self.object_key(key, env, v, &mut |k, fixed| {
            if let Some(fixed) = fixed {
                if val.is_none() {
                    let mut m = acc.clone();
                    m.insert(k, fixed);
                    return self.object(entries, i + 1, m, env, v, cb);
                }
            }
            match val {
                Some(e) => self.eval(e, env, Pv::val(v.clone()), &mut |x| {
                    let mut m = acc.clone();
                    m.insert(k.clone(), x.v);
                    self.object(entries, i + 1, m, env, v, cb)
                }),
                None => {
                    let x = index(v, &Value::String(k.clone()))?;
                    let mut m = acc.clone();
                    m.insert(k, x);
                    self.object(entries, i + 1, m, env, v, cb)
                }
            }
        })
    }

    fn if_branch<'a>(&self, branches: &'a [(Ast, Ast)], i: usize, otherwise: Option<&'a Ast>, env: &Env<'a>, inp: Pv, cb: Cb<'_>) -> R {
        if i == branches.len() {
            return match otherwise {
                Some(e) => self.eval(e, env, inp, cb),
                None => cb(inp),
            };
        }
        let (c, b) = &branches[i];
        self.eval(c, env, Pv::val(inp.v.clone()), &mut |cv| {
            if truthy(&cv.v) {
                self.eval(b, env, inp.clone(), cb)
            } else {
                self.if_branch(branches, i + 1, otherwise, env, inp.clone(), cb)
            }
        })
    }

    fn recurse_default(&self, inp: Pv, cb: Cb<'_>) -> R {
        cb(inp.clone())?;
        match &inp.v {
            Value::Array(_) | Value::Object(_) => iterate(inp, &mut |x| self.recurse_default(x, cb)),
            _ => Ok(()),
        }
    }

    /// Bind a destructuring pattern; one environment per combination.
    pub(crate) fn destructure<'a>(&self, pat: &'a Pattern, v: &Value, env: &Env<'a>, dot: &Value) -> Res<Vec<Env<'a>>> {
        match pat {
            Pattern::Var(name) => Ok(vec![env.push(Bind::Var(name, v.clone()))]),
            Pattern::Array(items) => {
                if !matches!(v, Value::Array(_) | Value::Null) {
                    return err(format!("Cannot index {} with number", tname(v)));
                }
                let mut envs = vec![env.clone()];
                for (i, p) in items.iter().enumerate() {
                    let item = index(v, &Value::Int(i as i64))?;
                    let mut next = Vec::new();
                    for e in &envs {
                        next.extend(self.destructure(p, &item, e, dot)?);
                    }
                    envs = next;
                }
                Ok(envs)
            }
            Pattern::Object(entries) => {
                let mut envs = vec![env.clone()];
                for (k, p) in entries {
                    let mut next = Vec::new();
                    for e in &envs {
                        let mut keys: Vec<(String, Option<&'a str>)> = Vec::new();
                        match k {
                            ObjKey::Var(name) => keys.push((name.clone(), Some(name.as_str()))),
                            ObjKey::Lit(s) => keys.push((s.clone(), None)),
                            other => {
                                self.object_key(other, e, dot, &mut |s, _| {
                                    keys.push((s, None));
                                    Ok(())
                                })?;
                            }
                        }
                        for (key, bind_as) in keys {
                            if !matches!(v, Value::Object(_) | Value::Null) {
                                return err(format!("Cannot index {} with \"{}\"", tname(v), key));
                            }
                            let item = index(v, &Value::String(key))?;
                            let mut e2 = e.clone();
                            if let Some(name) = bind_as {
                                e2 = e2.push(Bind::Var(name, item.clone()));
                            }
                            match p {
                                Some(p) => next.extend(self.destructure(p, &item, &e2, dot)?),
                                None => next.push(e2),
                            }
                        }
                    }
                    envs = next;
                }
                Ok(envs)
            }
        }
    }

    fn assign<'a>(&self, op: AssignOp, lhs: &'a Ast, rhs: &'a Ast, env: &Env<'a>, inp: Pv, cb: Cb<'_>) -> R {
        let root = inp.v.clone();
        match op {
            AssignOp::Update => {
                let paths = self.paths(lhs, env, &root)?;
                let mut state = root;
                let mut deletions = Vec::new();
                for p in paths {
                    let cur = getpath(&state, &p)?;
                    let id = next_id();
                    let mut first = None;
                    let r = self.eval(rhs, env, Pv::val(cur), &mut |x| {
                        first = Some(x.v);
                        Err(Ctl::Break(id))
                    });
                    match r {
                        Ok(()) => {}
                        Err(Ctl::Break(i)) if i == id => {}
                        Err(e) => return Err(e),
                    }
                    match first {
                        Some(nv) => state = setpath_ordered(state, &p, nv)?,
                        None => deletions.push(Value::Array(p)),
                    }
                }
                if !deletions.is_empty() {
                    state = delpaths(state, &deletions)?;
                }
                cb(Pv::val(state))
            }
            AssignOp::Set => self.eval(rhs, env, Pv::val(root.clone()), &mut |val| {
                let paths = self.paths(lhs, env, &root)?;
                let mut state = root.clone();
                for p in paths {
                    state = setpath_ordered(state, &p, val.v.clone())?;
                }
                cb(Pv::val(state))
            }),
            AssignOp::Arith(bop) => self.eval(rhs, env, Pv::val(root.clone()), &mut |val| {
                let paths = self.paths(lhs, env, &root)?;
                let mut state = root.clone();
                for p in paths {
                    let cur = getpath(&state, &p)?;
                    let nv = binop(bop, &cur, &val.v)?;
                    state = setpath_ordered(state, &p, nv)?;
                }
                cb(Pv::val(state))
            }),
            AssignOp::Alt => self.eval(rhs, env, Pv::val(root.clone()), &mut |val| {
                let paths = self.paths(lhs, env, &root)?;
                let mut state = root.clone();
                for p in paths {
                    let cur = getpath(&state, &p)?;
                    if !truthy(&cur) {
                        state = setpath_ordered(state, &p, val.v.clone())?;
                    }
                }
                cb(Pv::val(state))
            }),
        }
    }

    pub(crate) fn call<'a>(&self, name: &'a str, args: &'a [Ast], env: &Env<'a>, inp: Pv, cb: Cb<'_>) -> R {
        let mut cur = env.0.clone();
        while let Some(node) = cur {
            match &node.b {
                Bind::Func(d) if d.name == name && d.params.len() == args.len() => {
                    let def_env = Env(Some(node.clone()));
                    return self.call_def(d, def_env, args, env, inp, cb);
                }
                Bind::Closure(n, body, cenv) if *n == name && args.is_empty() => {
                    return self.eval(body, cenv, inp, cb);
                }
                _ => {}
            }
            cur = node.next.0.clone();
        }
        if let Some((d, def_env)) = super::prelude_lookup(name, args.len()) {
            return self.call_def(d, def_env, args, env, inp, cb);
        }
        super::builtins::native(self, name, args, env, inp, cb)
    }

    fn call_def<'a>(&self, d: &'a FuncDef, def_env: Env<'a>, args: &'a [Ast], caller: &Env<'a>, inp: Pv, cb: Cb<'_>) -> R {
        let mut env2 = def_env;
        let mut value_params: Vec<(&'a str, &'a Ast)> = Vec::new();
        for (p, a) in d.params.iter().zip(args) {
            match p.strip_prefix('$') {
                None => env2 = env2.push(Bind::Closure(p.as_str(), a, caller.clone())),
                Some(v) => {
                    env2 = env2.push(Bind::Closure(v, a, caller.clone()));
                    value_params.push((v, a));
                }
            }
        }
        if value_params.is_empty() {
            return self.eval(&d.body, &env2, inp, cb);
        }
        self.bind_values(d, &value_params, 0, env2, caller, inp, cb)
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_values<'a>(&self, d: &'a FuncDef, vp: &[(&'a str, &'a Ast)], i: usize, env2: Env<'a>, caller: &Env<'a>, inp: Pv, cb: Cb<'_>) -> R {
        if i == vp.len() {
            return self.eval(&d.body, &env2, inp, cb);
        }
        let (name, a) = vp[i];
        self.eval(a, caller, Pv::val(inp.v.clone()), &mut |x| self.bind_values(d, vp, i + 1, env2.push(Bind::Var(name, x.v)), caller, inp.clone(), cb))
    }
}

/// Pattern variable names live in the AST; find the `&'a str` for `name`.
fn leak_name<'a>(pats: &'a [Pattern], name: &str) -> &'a str {
    fn find<'a>(p: &'a Pattern, name: &str) -> Option<&'a str> {
        match p {
            Pattern::Var(v) if v == name => Some(v.as_str()),
            Pattern::Var(_) => None,
            Pattern::Array(items) => items.iter().find_map(|i| find(i, name)),
            Pattern::Object(entries) => entries.iter().find_map(|(k, p)| {
                if let ObjKey::Var(v) = k {
                    if v == name {
                        return Some(v.as_str());
                    }
                }
                p.as_ref().and_then(|p| find(p, name))
            }),
        }
    }
    pats.iter().find_map(|p| find(p, name)).unwrap_or("")
}

/// `.[]` with paths.
pub(crate) fn iterate(tp: Pv, cb: Cb<'_>) -> R {
    match tp.v {
        Value::Array(a) => {
            for (i, x) in a.into_iter().enumerate() {
                let p = tp.p.as_ref().map(|p| {
                    let mut p = p.clone();
                    p.push(Value::Int(i as i64));
                    p
                });
                cb(Pv { v: x, p })?;
            }
            Ok(())
        }
        Value::Object(o) => {
            for (k, x) in o {
                let p = tp.p.as_ref().map(|p| {
                    let mut p = p.clone();
                    p.push(Value::String(k.clone()));
                    p
                });
                cb(Pv { v: x, p })?;
            }
            Ok(())
        }
        Value::Null => err("Cannot iterate over null"),
        other => err(format!("Cannot iterate over {} ({})", tname(&other), dump_trunc(&other))),
    }
}
