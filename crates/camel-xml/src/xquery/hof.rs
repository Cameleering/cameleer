//! XQuery 3.1 maps, arrays and higher-order functions: constructors, lookups, dynamic
//! calls, inline functions, named function references, partial application, and the
//! `fn:` (`for-each`, `filter`, `fold-left`, `fold-right`, `for-each-pair`, `sort`,
//! `apply`, `function-arity`, `function-name`, `function-lookup`), `map:` and `array:`
//! function libraries.

use std::cmp::Ordering;
use std::rc::Rc;

use super::ast::{Expr, LookupKey, QN};
use super::eval::{Focus, Interp, Seq, R};
use super::types::*;
use super::{ARRAY_NS, FN_NS, MAP_NS};

const ARG_NS: &str = "urn:x-rust-camel:xquery-arg";

fn member(a: &[Vec<Item>], i: i64) -> R<Seq> {
    if i < 1 || i as usize > a.len() {
        return Err(XqError::new("FOAY0001", format!("Array index {} out of bounds (1..{})", i, a.len())));
    }
    Ok(a[i as usize - 1].clone())
}

impl Interp<'_> {
    fn one_item(&self, s: Seq, what: &str) -> R<Item> {
        match s.len() {
            1 => Ok(s.into_iter().next().unwrap()),
            n => Err(XqError::type_err(format!("{} must be a single item, found {} items", what, n))),
        }
    }

    fn func_arg(&self, s: Seq, what: &str) -> R<Item> {
        let it = self.one_item(s, what)?;
        match it {
            Item::Func(_) | Item::Map(_) | Item::Array(_) => Ok(it),
            _ => Err(XqError::type_err(format!("{} must be a function item", what))),
        }
    }

    fn map_arg(&self, s: Seq, what: &str) -> R<Rc<XqMap>> {
        match self.one_item(s, what)? {
            Item::Map(m) => Ok(m),
            _ => Err(XqError::type_err(format!("{} must be a map", what))),
        }
    }

    fn array_arg(&self, s: Seq, what: &str) -> R<Rc<Vec<Vec<Item>>>> {
        match self.one_item(s, what)? {
            Item::Array(a) => Ok(a),
            _ => Err(XqError::type_err(format!("{} must be an array", what))),
        }
    }

    fn key_arg(&self, s: &Seq, what: &str) -> R<Atomic> {
        self.atomize_opt(s, what)?.ok_or_else(|| XqError::type_err(format!("{} must not be empty", what)))
    }

    fn int_arg(&self, s: &Seq, what: &str) -> R<i64> {
        let a = self.key_arg(s, what)?;
        super::eval::to_integer(&a)
    }

    /// Evaluate the XQuery 3.1 expressions.
    pub(crate) fn eval31(&mut self, e: &Expr, f: &Focus) -> R<Seq> {
        match e {
            Expr::MapCtor(entries) => {
                let mut m = XqMap::default();
                for (k, v) in entries {
                    let kv = self.eval(k, f)?;
                    let key = self.key_arg(&kv, "a map key")?;
                    let key = super::eval::untyped_to_string(key);
                    if m.get(&key).is_some() {
                        return Err(XqError::new("XQDY0137", format!("Duplicate key '{}' in map constructor", key)));
                    }
                    let val = self.eval(v, f)?;
                    m.entries.push((key, val));
                }
                Ok(vec![Item::Map(Rc::new(m))])
            }
            Expr::ArrayCtor { square, members } => {
                let mut out = Vec::new();
                if *square {
                    for m in members {
                        out.push(self.eval(m, f)?);
                    }
                } else {
                    for m in members {
                        for it in self.eval(m, f)? {
                            out.push(vec![it]);
                        }
                    }
                }
                Ok(vec![Item::Array(Rc::new(out))])
            }
            Expr::InlineFunc { params, ret, body } => {
                let closure = self.vars.clone();
                Ok(vec![Item::Func(Rc::new(FuncItem::Inline { params: params.clone(), ret: ret.clone(), body: body.clone(), closure }))])
            }
            Expr::FuncRef(name, arity) => {
                let known = self.module.functions.contains_key(&(name.clone(), *arity))
                    || [FN_NS, MAP_NS, ARRAY_NS, super::MATH_NS, super::XS_NS].contains(&name.uri.as_str());
                if !known {
                    return Err(XqError::static_err("XPST0017", format!("Unknown function {}#{}", name.local, arity)));
                }
                Ok(vec![Item::Func(Rc::new(FuncItem::Named { name: name.clone(), arity: *arity }))])
            }
            Expr::DynCall(fe, args) => {
                let fv = self.eval(fe, f)?;
                let func = self.func_arg(fv, "The target of a dynamic function call")?;
                let mut vals = Vec::with_capacity(args.len());
                let mut partial = false;
                for a in args {
                    match a {
                        Some(x) => vals.push(Some(self.eval(x, f)?)),
                        None => {
                            partial = true;
                            vals.push(None);
                        }
                    }
                }
                if partial {
                    return Ok(vec![Item::Func(Rc::new(FuncItem::Partial { base: func, args: vals }))]);
                }
                self.call_item(&func, vals.into_iter().map(|v| v.unwrap()).collect())
            }
            Expr::PartialCall { name, args } => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(match a {
                        Some(x) => Some(self.eval(x, f)?),
                        None => None,
                    });
                }
                let base = Item::Func(Rc::new(FuncItem::Named { name: name.clone(), arity: args.len() }));
                Ok(vec![Item::Func(Rc::new(FuncItem::Partial { base, args: vals }))])
            }
            Expr::Lookup(input, key) => {
                let items = match input {
                    Some(i) => self.eval(i, f)?,
                    None => vec![f.item.clone().ok_or_else(|| XqError::new("XPDY0002", "The context item for '?' is absent"))?],
                };
                let keys: Option<Vec<Atomic>> = match key {
                    LookupKey::Name(n) => Some(vec![Atomic::Str(n.clone())]),
                    LookupKey::Int(i) => Some(vec![Atomic::Int(*i)]),
                    LookupKey::Wildcard => None,
                    LookupKey::Expr(e) => {
                        let v = self.eval(e, f)?;
                        Some(self.atomize(&v))
                    }
                };
                let mut out = Vec::new();
                for it in items {
                    match it {
                        Item::Map(m) => match &keys {
                            None => {
                                for (_, v) in &m.entries {
                                    out.extend(v.iter().cloned());
                                }
                            }
                            Some(ks) => {
                                for k in ks {
                                    if let Some(v) = m.get(&super::eval::untyped_to_string(k.clone())) {
                                        out.extend(v.iter().cloned());
                                    }
                                }
                            }
                        },
                        Item::Array(a) => match &keys {
                            None => {
                                for m in a.iter() {
                                    out.extend(m.iter().cloned());
                                }
                            }
                            Some(ks) => {
                                for k in ks {
                                    let i = match k {
                                        Atomic::Int(i) => *i,
                                        other => return Err(XqError::type_err(format!("An array lookup key must be an integer, found {}", other))),
                                    };
                                    out.extend(member(&a, i)?);
                                }
                            }
                        },
                        _ => return Err(XqError::type_err("The '?' lookup operator applies to maps and arrays")),
                    }
                }
                Ok(out)
            }
            _ => unreachable!(),
        }
    }

    /// Call a function item (function, map or array) with argument values.
    pub(crate) fn call_item(&mut self, func: &Item, args: Vec<Seq>) -> R<Seq> {
        match func {
            Item::Map(m) => {
                if args.len() != 1 {
                    return Err(XqError::type_err("A map takes exactly one argument"));
                }
                let k = self.key_arg(&args[0], "a map key")?;
                Ok(m.get(&super::eval::untyped_to_string(k)).cloned().unwrap_or_default())
            }
            Item::Array(a) => {
                if args.len() != 1 {
                    return Err(XqError::type_err("An array takes exactly one argument"));
                }
                let i = self.int_arg(&args[0], "an array index")?;
                member(a, i)
            }
            Item::Func(fi) => match fi.as_ref() {
                FuncItem::Named { name, arity } => {
                    if args.len() != *arity {
                        return Err(XqError::type_err(format!("Function {}#{} called with {} arguments", name.local, arity, args.len())));
                    }
                    if let Some(func) = self.module.functions.get(&(name.clone(), *arity)) {
                        return self.call_user(func, args);
                    }
                    // built-ins evaluate expressions: pass the values through variables
                    let base = self.vars.len();
                    let mut exprs = Vec::with_capacity(args.len());
                    for (i, v) in args.into_iter().enumerate() {
                        let q = QN::new(ARG_NS, format!("a{}_{}", base, i));
                        self.vars.push((q.clone(), Rc::new(v)));
                        exprs.push(Expr::Var(q));
                    }
                    let r = self.call(name, &exprs, &Focus::none());
                    self.vars.truncate(base);
                    r
                }
                FuncItem::Inline { params, ret, body, closure } => {
                    if args.len() != params.len() {
                        return Err(XqError::type_err(format!("An inline function with {} parameters called with {} arguments", params.len(), args.len())));
                    }
                    if self.depth >= super::eval::MAX_DEPTH {
                        return Err(XqError::new("XPDY0130", "Too many nested function calls"));
                    }
                    let mut frame = closure.clone();
                    for ((q, t), v) in params.iter().zip(args) {
                        let v = match t {
                            Some(t) => self.convert_to(v, t, &format!("parameter ${}", q.local))?,
                            None => v,
                        };
                        frame.push((q.clone(), Rc::new(v)));
                    }
                    self.depth += 1;
                    let saved = std::mem::replace(&mut self.vars, frame);
                    let r = self.eval(body, &Focus::none());
                    self.vars = saved;
                    self.depth -= 1;
                    let r = r?;
                    match ret {
                        Some(t) => self.convert_to(r, t, "the result of an inline function"),
                        None => Ok(r),
                    }
                }
                FuncItem::Partial { base, args: fixed } => {
                    let mut it = args.into_iter();
                    let mut full = Vec::with_capacity(fixed.len());
                    for a in fixed {
                        full.push(match a {
                            Some(v) => v.clone(),
                            None => it.next().ok_or_else(|| XqError::type_err("Too few arguments for a partially applied function"))?,
                        });
                    }
                    if it.next().is_some() {
                        return Err(XqError::type_err("Too many arguments for a partially applied function"));
                    }
                    self.call_item(base, full)
                }
            },
            _ => Err(XqError::type_err("Not a function item")),
        }
    }

    /// Higher-order and map / array functions; `None` when `name` is not one of them.
    pub(crate) fn call_hof(&mut self, name: &QN, args: &[Expr], f: &Focus) -> Option<R<Seq>> {
        let l = name.local.as_str();
        let n = args.len();
        let handled = match name.uri.as_str() {
            FN_NS => matches!(
                (l, n),
                ("for-each" | "filter" | "apply" | "for-each-pair" | "fold-left" | "fold-right", _) | ("sort", 1..=3) | ("function-arity" | "function-name", 1) | ("function-lookup", 2)
            ),
            MAP_NS | ARRAY_NS => true,
            _ => false,
        };
        if !handled {
            return None;
        }
        let mut a: Vec<Seq> = Vec::with_capacity(n);
        for e in args {
            match self.eval(e, f) {
                Ok(v) => a.push(v),
                Err(e) => return Some(Err(e)),
            }
        }
        Some(match name.uri.as_str() {
            FN_NS => self.fn_hof(l, a),
            MAP_NS => self.map_fn(l, a),
            _ => self.array_fn(l, a),
        })
    }

    /// Stable sort order of items by their (atomized) sort keys.
    fn sort_order(&self, keys: &[Seq]) -> R<Vec<usize>> {
        let ks: Vec<Vec<Atomic>> = keys.iter().map(|k| self.atomize(k).into_iter().map(super::eval::untyped_to_string).collect()).collect();
        let mut idx: Vec<usize> = (0..ks.len()).collect();
        let mut err = None;
        idx.sort_by(|&x, &y| {
            for (p, q) in ks[x].iter().zip(&ks[y]) {
                let o = match compare_atomic(p, q) {
                    Ok(Some(o)) => o,
                    Ok(None) => (!p.as_f64().is_nan()).cmp(&!q.as_f64().is_nan()),
                    Err(e) => {
                        err.get_or_insert(e);
                        Ordering::Equal
                    }
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
            ks[x].len().cmp(&ks[y].len())
        });
        match err {
            Some(e) => Err(e),
            None => Ok(idx),
        }
    }

    fn fn_hof(&mut self, l: &str, mut a: Vec<Seq>) -> R<Seq> {
        let n = a.len();
        match (l, n) {
            ("for-each", 2) => {
                let func = self.func_arg(a.pop().unwrap(), "for-each() $action")?;
                let mut out = Vec::new();
                for it in a.pop().unwrap() {
                    out.extend(self.call_item(&func, vec![vec![it]])?);
                }
                Ok(out)
            }
            ("filter", 2) => {
                let func = self.func_arg(a.pop().unwrap(), "filter() $predicate")?;
                let mut out = Vec::new();
                for it in a.pop().unwrap() {
                    let r = self.call_item(&func, vec![vec![it.clone()]])?;
                    match r.as_slice() {
                        [Item::Atomic(Atomic::Bool(true))] => out.push(it),
                        [Item::Atomic(Atomic::Bool(false))] => {}
                        _ => return Err(XqError::type_err("filter(): the predicate must return a single xs:boolean")),
                    }
                }
                Ok(out)
            }
            ("fold-left", 3) | ("fold-right", 3) => {
                let func = self.func_arg(a.pop().unwrap(), "the fold function")?;
                let mut acc = a.pop().unwrap();
                let seq = a.pop().unwrap();
                if l == "fold-left" {
                    for it in seq {
                        acc = self.call_item(&func, vec![acc, vec![it]])?;
                    }
                } else {
                    for it in seq.into_iter().rev() {
                        acc = self.call_item(&func, vec![vec![it], acc])?;
                    }
                }
                Ok(acc)
            }
            ("for-each-pair", 3) => {
                let func = self.func_arg(a.pop().unwrap(), "for-each-pair() $action")?;
                let ys = a.pop().unwrap();
                let xs = a.pop().unwrap();
                let mut out = Vec::new();
                for (x, y) in xs.into_iter().zip(ys) {
                    out.extend(self.call_item(&func, vec![vec![x], vec![y]])?);
                }
                Ok(out)
            }
            ("sort", 1..=3) => {
                let key = if n == 3 { Some(self.func_arg(a.pop().unwrap(), "sort() $key")?) } else { None };
                let seq = a.into_iter().next().unwrap();
                let mut keys = Vec::with_capacity(seq.len());
                for it in &seq {
                    keys.push(match &key {
                        Some(kf) => self.call_item(kf, vec![vec![it.clone()]])?,
                        None => vec![it.clone()],
                    });
                }
                let order = self.sort_order(&keys)?;
                Ok(order.into_iter().map(|i| seq[i].clone()).collect())
            }
            ("apply", 2) => {
                let arr = self.array_arg(a.pop().unwrap(), "apply() $arguments")?;
                let func = self.func_arg(a.pop().unwrap(), "apply() $function")?;
                self.call_item(&func, arr.as_ref().clone())
            }
            ("function-arity", 1) => {
                let func = self.func_arg(a.pop().unwrap(), "function-arity() $func")?;
                Ok(vec![Item::int(match &func {
                    Item::Func(fi) => fi.arity() as i64,
                    _ => 1,
                })])
            }
            ("function-name", 1) => {
                let func = self.func_arg(a.pop().unwrap(), "function-name() $func")?;
                Ok(match &func {
                    Item::Func(fi) => match fi.as_ref() {
                        FuncItem::Named { name, .. } => vec![Item::Atomic(Atomic::QName(String::new(), name.local.clone(), name.uri.clone()))],
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                })
            }
            ("function-lookup", 2) => {
                let arity = self.int_arg(&a[1], "function-lookup() $arity")? as usize;
                let q = self.key_arg(&a[0], "function-lookup() $name")?;
                let Atomic::QName(_, local, uri) = q else { return Err(XqError::type_err("function-lookup() expects an xs:QName")) };
                let name = QN::new(uri, local);
                let known = self.module.functions.contains_key(&(name.clone(), arity)) || [FN_NS, MAP_NS, ARRAY_NS, super::MATH_NS].contains(&name.uri.as_str());
                Ok(if known { vec![Item::Func(Rc::new(FuncItem::Named { name, arity }))] } else { Vec::new() })
            }
            _ => Err(XqError::static_err("XPST0017", format!("Function {}() with {} arguments is not available", l, n))),
        }
    }

    fn map_fn(&mut self, l: &str, mut a: Vec<Seq>) -> R<Seq> {
        let n = a.len();
        let one = |m: XqMap| Ok(vec![Item::Map(Rc::new(m))]);
        match (l, n) {
            ("size", 1) => Ok(vec![Item::int(self.map_arg(a.pop().unwrap(), "map:size() $map")?.entries.len() as i64)]),
            ("keys", 1) => Ok(self.map_arg(a.pop().unwrap(), "map:keys() $map")?.entries.iter().map(|(k, _)| Item::Atomic(k.clone())).collect()),
            ("contains", 2) => {
                let k = self.key_arg(&a[1], "map:contains() $key")?;
                let m = self.map_arg(a.swap_remove(0), "map:contains() $map")?;
                Ok(vec![Item::bool(m.get(&super::eval::untyped_to_string(k)).is_some())])
            }
            ("get", 2) => {
                let k = self.key_arg(&a[1], "map:get() $key")?;
                let m = self.map_arg(a.swap_remove(0), "map:get() $map")?;
                Ok(m.get(&super::eval::untyped_to_string(k)).cloned().unwrap_or_default())
            }
            ("put", 3) => {
                let v = a.pop().unwrap();
                let k = self.key_arg(&a[1], "map:put() $key")?;
                let mut m = self.map_arg(a.swap_remove(0), "map:put() $map")?.as_ref().clone();
                m.put(super::eval::untyped_to_string(k), v);
                one(m)
            }
            ("remove", 2) => {
                let ks = self.atomize(&a[1]);
                let mut m = self.map_arg(a.swap_remove(0), "map:remove() $map")?.as_ref().clone();
                for k in ks {
                    m.remove(&super::eval::untyped_to_string(k));
                }
                one(m)
            }
            ("entry", 2) => {
                let v = a.pop().unwrap();
                let k = self.key_arg(&a[0], "map:entry() $key")?;
                one(XqMap { entries: vec![(super::eval::untyped_to_string(k), v)] })
            }
            ("merge", 1 | 2) => {
                let use_first = if n == 2 {
                    match self.map_arg(a.pop().unwrap(), "map:merge() $options")?.get(&Atomic::Str("duplicates".into())) {
                        Some(v) => !matches!(self.atomize(v).first().map(|x| x.to_string()).as_deref(), Some("use-last")),
                        None => true,
                    }
                } else {
                    true
                };
                let mut m = XqMap::default();
                for it in a.pop().unwrap() {
                    let Item::Map(src) = it else { return Err(XqError::type_err("map:merge() expects maps")) };
                    for (k, v) in &src.entries {
                        if use_first && m.get(k).is_some() {
                            continue;
                        }
                        m.put(k.clone(), v.clone());
                    }
                }
                one(m)
            }
            ("for-each", 2) => {
                let func = self.func_arg(a.pop().unwrap(), "map:for-each() $action")?;
                let m = self.map_arg(a.pop().unwrap(), "map:for-each() $map")?;
                let mut out = Vec::new();
                for (k, v) in &m.entries {
                    out.extend(self.call_item(&func, vec![vec![Item::Atomic(k.clone())], v.clone()])?);
                }
                Ok(out)
            }
            ("find", 2) => {
                let k = super::eval::untyped_to_string(self.key_arg(&a[1], "map:find() $key")?);
                let mut found = Vec::new();
                let mut stack: Vec<Item> = a.swap_remove(0);
                while let Some(it) = stack.pop() {
                    match it {
                        Item::Map(m) => {
                            if let Some(v) = m.get(&k) {
                                found.push(v.clone());
                            }
                            for (_, v) in m.entries.iter().rev() {
                                stack.extend(v.iter().rev().cloned());
                            }
                        }
                        Item::Array(arr) => {
                            for m in arr.iter().rev() {
                                stack.extend(m.iter().rev().cloned());
                            }
                        }
                        _ => {}
                    }
                }
                Ok(vec![Item::Array(Rc::new(found))])
            }
            _ => Err(XqError::static_err("XPST0017", format!("Function map:{}() with {} arguments is not available", l, n))),
        }
    }

    fn array_fn(&mut self, l: &str, mut a: Vec<Seq>) -> R<Seq> {
        let n = a.len();
        let one = |v: Vec<Vec<Item>>| Ok(vec![Item::Array(Rc::new(v))]);
        match (l, n) {
            ("size", 1) => Ok(vec![Item::int(self.array_arg(a.pop().unwrap(), "array:size() $array")?.len() as i64)]),
            ("get", 2) => {
                let i = self.int_arg(&a[1], "array:get() $position")?;
                let arr = self.array_arg(a.swap_remove(0), "array:get() $array")?;
                member(&arr, i)
            }
            ("put", 3) => {
                let v = a.pop().unwrap();
                let i = self.int_arg(&a[1], "array:put() $position")?;
                let mut arr = self.array_arg(a.swap_remove(0), "array:put() $array")?.as_ref().clone();
                member(&arr, i)?;
                arr[i as usize - 1] = v;
                one(arr)
            }
            ("append", 2) => {
                let v = a.pop().unwrap();
                let mut arr = self.array_arg(a.pop().unwrap(), "array:append() $array")?.as_ref().clone();
                arr.push(v);
                one(arr)
            }
            ("head", 1) => {
                let arr = self.array_arg(a.pop().unwrap(), "array:head() $array")?;
                member(&arr, 1)
            }
            ("tail", 1) => {
                let arr = self.array_arg(a.pop().unwrap(), "array:tail() $array")?;
                if arr.is_empty() {
                    return Err(XqError::new("FOAY0001", "array:tail() of an empty array"));
                }
                one(arr[1..].to_vec())
            }
            ("subarray", 2 | 3) => {
                let len = if n == 3 { Some(self.int_arg(&a[2], "array:subarray() $length")?) } else { None };
                let st = self.int_arg(&a[1], "array:subarray() $start")?;
                let arr = self.array_arg(a.swap_remove(0), "array:subarray() $array")?;
                let end = len.map(|l| st + l).unwrap_or(arr.len() as i64 + 1);
                if st < 1 || end < st || end > arr.len() as i64 + 1 {
                    return Err(XqError::new("FOAY0001", "array:subarray() bounds out of range"));
                }
                one(arr[st as usize - 1..end as usize - 1].to_vec())
            }
            ("remove", 2) => {
                let mut idx: Vec<i64> = Vec::new();
                for x in self.atomize(&a[1]) {
                    idx.push(super::eval::to_integer(&x)?);
                }
                let arr = self.array_arg(a.swap_remove(0), "array:remove() $array")?;
                for &i in &idx {
                    member(&arr, i)?;
                }
                one(arr.iter().enumerate().filter(|(k, _)| !idx.contains(&(*k as i64 + 1))).map(|(_, m)| m.clone()).collect())
            }
            ("insert-before", 3) => {
                let v = a.pop().unwrap();
                let i = self.int_arg(&a[1], "array:insert-before() $position")?;
                let mut arr = self.array_arg(a.swap_remove(0), "array:insert-before() $array")?.as_ref().clone();
                if i < 1 || i as usize > arr.len() + 1 {
                    return Err(XqError::new("FOAY0001", "array:insert-before() position out of range"));
                }
                arr.insert(i as usize - 1, v);
                one(arr)
            }
            ("reverse", 1) => {
                let mut arr = self.array_arg(a.pop().unwrap(), "array:reverse() $array")?.as_ref().clone();
                arr.reverse();
                one(arr)
            }
            ("join", 1) => {
                let mut out = Vec::new();
                for it in a.pop().unwrap() {
                    let Item::Array(x) = it else { return Err(XqError::type_err("array:join() expects arrays")) };
                    out.extend(x.iter().cloned());
                }
                one(out)
            }
            ("flatten", 1) => {
                fn flat(items: &[Item], out: &mut Vec<Item>) {
                    for it in items {
                        match it {
                            Item::Array(a) => {
                                for m in a.iter() {
                                    flat(m, out);
                                }
                            }
                            other => out.push(other.clone()),
                        }
                    }
                }
                let mut out = Vec::new();
                flat(&a[0], &mut out);
                Ok(out)
            }
            ("for-each", 2) => {
                let func = self.func_arg(a.pop().unwrap(), "array:for-each() $action")?;
                let arr = self.array_arg(a.pop().unwrap(), "array:for-each() $array")?;
                let mut out = Vec::new();
                for m in arr.iter() {
                    out.push(self.call_item(&func, vec![m.clone()])?);
                }
                one(out)
            }
            ("filter", 2) => {
                let func = self.func_arg(a.pop().unwrap(), "array:filter() $function")?;
                let arr = self.array_arg(a.pop().unwrap(), "array:filter() $array")?;
                let mut out = Vec::new();
                for m in arr.iter() {
                    let r = self.call_item(&func, vec![m.clone()])?;
                    if self.ebv(&r)? {
                        out.push(m.clone());
                    }
                }
                one(out)
            }
            ("fold-left" | "fold-right", 3) => {
                let func = self.func_arg(a.pop().unwrap(), "the fold function")?;
                let mut acc = a.pop().unwrap();
                let arr = self.array_arg(a.pop().unwrap(), "the array")?;
                if l == "fold-left" {
                    for m in arr.iter() {
                        acc = self.call_item(&func, vec![acc, m.clone()])?;
                    }
                } else {
                    for m in arr.iter().rev() {
                        acc = self.call_item(&func, vec![m.clone(), acc])?;
                    }
                }
                Ok(acc)
            }
            ("sort", 1..=3) => {
                let key = if n == 3 { Some(self.func_arg(a.pop().unwrap(), "array:sort() $key")?) } else { None };
                let arr = self.array_arg(a.swap_remove(0), "array:sort() $array")?;
                let mut keys = Vec::with_capacity(arr.len());
                for m in arr.iter() {
                    keys.push(match &key {
                        Some(k) => self.call_item(k, vec![m.clone()])?,
                        None => m.clone(),
                    });
                }
                let order = self.sort_order(&keys)?;
                one(order.into_iter().map(|i| arr[i].clone()).collect())
            }
            _ => Err(XqError::static_err("XPST0017", format!("Function array:{}() with {} arguments is not available", l, n))),
        }
    }
}
