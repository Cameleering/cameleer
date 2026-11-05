//! JavaScript keyed collections (`Map`, `Set`, `WeakMap`, `WeakSet`), list iterators and
//! symbols.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crate::interp::{Interp, R};
use crate::value::*;

/// A key held weakly (`WeakMap` / `WeakSet`).
pub enum WeakKey {
    Obj(Weak<RefCell<Obj>>),
    Arr(Weak<RefCell<Vec<V>>>),
    Func(Weak<Closure>),
}

impl WeakKey {
    fn upgrade(&self) -> Option<V> {
        match self {
            WeakKey::Obj(w) => w.upgrade().map(V::Obj),
            WeakKey::Arr(w) => w.upgrade().map(V::Arr),
            WeakKey::Func(w) => w.upgrade().map(V::Func),
        }
    }
}

/// A collection key.
pub enum Key {
    Strong(V),
    Weak(WeakKey),
}

impl Key {
    fn get(&self) -> Option<V> {
        match self {
            Key::Strong(v) => Some(v.clone()),
            Key::Weak(w) => w.upgrade(),
        }
    }
}

/// Insertion ordered entries with SameValueZero key lookup.
#[derive(Default)]
pub struct MapData {
    entries: Vec<Option<(Key, V)>>,
    index: HashMap<String, usize>,
    weak: bool,
}

/// The state of a collection object.
pub enum Coll {
    Map(MapData),
    Set(MapData),
    WeakMap(MapData),
    WeakSet(MapData),
    /// A snapshot iterator over values (`map.entries()`, `yield*` over an array...).
    Iter(Vec<V>, usize),
}

/// SameValueZero key of a value.
pub fn key_of(v: &V) -> String {
    match v {
        V::Undef => "u".into(),
        V::Null => "n".into(),
        V::Bool(b) => format!("b:{}", b),
        V::Int(i) => format!("d:{}", i),
        V::Num(f) => {
            if f.is_nan() {
                "d:NaN".into()
            } else if f.fract() == 0.0 && f.abs() < 9.0e15 {
                format!("d:{}", *f as i64)
            } else {
                format!("d:{}", f)
            }
        }
        V::Str(s) => format!("s:{}", s),
        V::Obj(o) => format!("p:{:p}", Rc::as_ptr(o)),
        V::Arr(a) => format!("p:{:p}", Rc::as_ptr(a)),
        V::Func(f) => format!("p:{:p}", Rc::as_ptr(f)),
        V::Bytes(b) => format!("p:{:p}", Rc::as_ptr(b)),
        V::Regex(r) => format!("p:{:p}", Rc::as_ptr(r)),
        V::Method(m) => format!("p:{:p}", Rc::as_ptr(m)),
        V::Host(h) => format!("h:{:?}", h),
    }
}

impl MapData {
    fn new(weak: bool) -> MapData {
        MapData { weak, ..Default::default() }
    }
    fn slot(&mut self, k: &V) -> Option<usize> {
        let key = key_of(k);
        let i = *self.index.get(&key)?;
        // a dead weak key: drop the entry
        if self.entries[i].as_ref().is_some_and(|(k, _)| k.get().is_none()) {
            self.entries[i] = None;
            self.index.remove(&key);
            return None;
        }
        Some(i)
    }
    pub fn get(&mut self, k: &V) -> Option<V> {
        let i = self.slot(k)?;
        self.entries[i].as_ref().map(|(_, v)| v.clone())
    }
    pub fn has(&mut self, k: &V) -> bool {
        self.slot(k).is_some()
    }
    pub fn set(&mut self, k: V, v: V) {
        if let Some(i) = self.slot(&k) {
            if let Some(e) = self.entries[i].as_mut() {
                e.1 = v;
            }
            return;
        }
        let key = key_of(&k);
        let stored = if self.weak {
            Key::Weak(match &k {
                V::Obj(o) => WeakKey::Obj(Rc::downgrade(o)),
                V::Arr(a) => WeakKey::Arr(Rc::downgrade(a)),
                V::Func(f) => WeakKey::Func(Rc::downgrade(f)),
                _ => return,
            })
        } else {
            Key::Strong(k)
        };
        self.index.insert(key, self.entries.len());
        self.entries.push(Some((stored, v)));
    }
    pub fn delete(&mut self, k: &V) -> bool {
        match self.slot(k) {
            Some(i) => {
                self.entries[i] = None;
                self.index.remove(&key_of(k));
                if self.entries.len() > 32 && self.index.len() * 2 < self.entries.len() {
                    self.compact();
                }
                true
            }
            None => false,
        }
    }
    fn compact(&mut self) {
        let old = std::mem::take(&mut self.entries);
        self.index.clear();
        for (k, v) in old.into_iter().flatten() {
            if let Some(kv) = k.get() {
                self.index.insert(key_of(&kv), self.entries.len());
                self.entries.push(Some((k, v)));
            }
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
    }
    pub fn len(&self) -> usize {
        self.entries.iter().flatten().filter(|(k, _)| k.get().is_some()).count()
    }
    /// Live entries.
    pub fn pairs(&self) -> Vec<(V, V)> {
        self.entries.iter().flatten().filter_map(|(k, v)| k.get().map(|k| (k, v.clone()))).collect()
    }
}

fn coll_value(c: Coll) -> V {
    V::Obj(new_obj_ref(Obj { props: PropMap::default(), proto: None, kind: ObjKind::Coll(Internal::new(c)) }))
}

/// A snapshot iterator over `items`.
pub fn list_iterator(items: Vec<V>) -> V {
    coll_value(Coll::Iter(items, 0))
}

/// The entries of a collection for iteration: `[k, v]` pairs for maps, values for sets.
pub fn iterate_coll(c: &Coll) -> Option<Vec<V>> {
    Some(match c {
        Coll::Map(m) => m.pairs().into_iter().map(|(k, v)| V::arr(vec![k, v])).collect(),
        Coll::Set(m) => m.pairs().into_iter().map(|(k, _)| k).collect(),
        Coll::Iter(items, i) => items[(*i).min(items.len())..].to_vec(),
        Coll::WeakMap(_) | Coll::WeakSet(_) => return None,
    })
}

/// Conversion of a collection to a Camel value: maps become objects (keys converted to
/// strings), sets become arrays.
pub fn coll_to_value(c: &Coll, conv: &dyn Fn(&V) -> cameleer_core::value::Value, key: &dyn Fn(&V) -> String) -> cameleer_core::value::Value {
    use cameleer_core::value::{ObjectMap, Value};
    match c {
        Coll::Map(m) => {
            let mut o = ObjectMap::new();
            for (k, v) in m.pairs() {
                o.insert(key(&k), conv(&v));
            }
            Value::Object(o)
        }
        Coll::Set(m) => Value::Array(m.pairs().iter().map(|(k, _)| conv(k)).collect()),
        Coll::Iter(items, i) => Value::Array(items[(*i).min(items.len())..].iter().map(conv).collect()),
        Coll::WeakMap(_) | Coll::WeakSet(_) => Value::Object(ObjectMap::new()),
    }
}

/// Type name of a collection (`Map`, `Set`...).
pub fn coll_name(c: &Coll) -> &'static str {
    match c {
        Coll::Map(_) => "Map",
        Coll::Set(_) => "Set",
        Coll::WeakMap(_) => "WeakMap",
        Coll::WeakSet(_) => "WeakSet",
        Coll::Iter(..) => "Array Iterator",
    }
}

impl Interp {
    /// `new Map(it)` / `new Set(it)` / `new WeakMap(it)` / `new WeakSet(it)`.
    pub fn construct_coll(&mut self, name: &str, args: &[V], pos: u32) -> R<V> {
        let init = args.first().cloned().unwrap_or(V::Undef);
        let items = if init.is_nullish() { vec![] } else { self.iterate(&init, Some(pos))? };
        let weak = name.starts_with("Weak");
        let mut m = MapData::new(weak);
        let is_map = name.ends_with("Map");
        for it in items {
            if is_map {
                let k = self.get_index(&it, &V::Int(0), pos)?;
                let v = self.get_index(&it, &V::Int(1), pos)?;
                if weak && !matches!(k, V::Obj(_) | V::Arr(_) | V::Func(_)) {
                    return self.type_error("Invalid value used as weak map key".into(), Some(pos));
                }
                m.set(k, v);
            } else {
                if weak && !matches!(it, V::Obj(_) | V::Arr(_) | V::Func(_)) {
                    return self.type_error("Invalid value used in weak set".into(), Some(pos));
                }
                m.set(it.clone(), it);
            }
        }
        Ok(coll_value(match name {
            "Map" => Coll::Map(m),
            "Set" => Coll::Set(m),
            "WeakMap" => Coll::WeakMap(m),
            _ => Coll::WeakSet(m),
        }))
    }

    /// Property of a collection (`size`).
    pub fn coll_property(&mut self, c: &Internal<Coll>, name: &str) -> Option<V> {
        let b = c.0.borrow();
        match (&*b, name) {
            (Coll::Map(m) | Coll::Set(m), "size") => Some(V::Int(m.len() as i64)),
            _ => None,
        }
    }

    /// Methods of collections and list iterators.
    pub fn coll_method(&mut self, c: &Internal<Coll>, recv: &V, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
        let a0 = args.first().cloned().unwrap_or(V::Undef);
        let a1 = args.get(1).cloned().unwrap_or(V::Undef);
        let kind = match &*c.0.borrow() {
            Coll::Map(_) => 0,
            Coll::Set(_) => 1,
            Coll::WeakMap(_) => 2,
            Coll::WeakSet(_) => 3,
            Coll::Iter(..) => 4,
        };
        if kind == 4 {
            return Ok(match name {
                "next" => {
                    let x = match &mut *c.0.borrow_mut() {
                        Coll::Iter(items, i) => {
                            let x = items.get(*i).cloned();
                            *i += 1;
                            x
                        }
                        _ => None,
                    };
                    Some(match x {
                        Some(v) => self.iter_result(v, false),
                        None => self.iter_result(V::Undef, true),
                    })
                }
                "%self" => Some(recv.clone()),
                _ => None,
            });
        }
        let weak_key_ok = |v: &V| matches!(v, V::Obj(_) | V::Arr(_) | V::Func(_));
        let r = {
            let mut b = c.0.borrow_mut();
            let m = match &mut *b {
                Coll::Map(m) | Coll::Set(m) | Coll::WeakMap(m) | Coll::WeakSet(m) => m,
                Coll::Iter(..) => unreachable!(),
            };
            match name {
                "get" if kind == 0 || kind == 2 => Some(m.get(&a0).unwrap_or(V::Undef)),
                "set" if kind == 0 || kind == 2 => {
                    if kind == 2 && !weak_key_ok(&a0) {
                        drop(b);
                        return self.type_error("Invalid value used as weak map key".into(), Some(pos));
                    }
                    m.set(a0.clone(), a1.clone());
                    Some(recv.clone())
                }
                "add" if kind == 1 || kind == 3 => {
                    if kind == 3 && !weak_key_ok(&a0) {
                        drop(b);
                        return self.type_error("Invalid value used in weak set".into(), Some(pos));
                    }
                    m.set(a0.clone(), a0.clone());
                    Some(recv.clone())
                }
                "has" => Some(V::Bool(m.has(&a0))),
                "delete" => Some(V::Bool(m.delete(&a0))),
                "clear" if kind < 2 => {
                    m.clear();
                    Some(V::Undef)
                }
                _ => None,
            }
        };
        if r.is_some() {
            return Ok(r);
        }
        if kind >= 2 {
            return Ok(None);
        }
        let pairs = match &*c.0.borrow() {
            Coll::Map(m) | Coll::Set(m) => m.pairs(),
            _ => vec![],
        };
        Ok(Some(match name {
            "forEach" => {
                for (k, v) in pairs {
                    let a = if kind == 0 { vec![v, k, recv.clone()] } else { vec![k.clone(), k, recv.clone()] };
                    self.call(&a0, a1.clone(), a, pos)?;
                }
                V::Undef
            }
            "keys" => list_iterator(pairs.into_iter().map(|(k, _)| k).collect()),
            "values" => list_iterator(pairs.into_iter().map(|(k, v)| if kind == 0 { v } else { k }).collect()),
            "entries" | "%self" => {
                if name == "%self" && kind == 1 {
                    list_iterator(pairs.into_iter().map(|(k, _)| k).collect())
                } else {
                    list_iterator(pairs.into_iter().map(|(k, v)| V::arr(vec![k.clone(), if kind == 0 { v } else { k }])).collect())
                }
            }
            "toString" => V::string(format!("[object {}]", if kind == 0 { "Map" } else { "Set" })),
            _ => return Ok(None),
        }))
    }

    // ------------------------------------------------------------------ symbols
    /// A new unique symbol.
    pub fn new_symbol(&mut self, desc: &str) -> V {
        self.next_symbol += 1;
        V::Obj(new_obj_ref(Obj { props: PropMap::default(), proto: None, kind: ObjKind::Symbol(self.next_symbol, Rc::from(desc)) }))
    }

    /// A well-known symbol (`Symbol.iterator`...), or a registered one (`Symbol.for(key)`).
    pub fn symbol_for(&mut self, key: &str) -> V {
        if let Some(s) = self.symbols.get(key) {
            return s.clone();
        }
        let desc = key.strip_prefix("@@").map(|w| format!("Symbol.{}", w)).unwrap_or_else(|| key.to_string());
        let s = self.new_symbol(&desc);
        self.symbols.insert(key.to_string(), s.clone());
        s
    }

    /// Property key of a well-known symbol.
    pub fn well_known_key(&mut self, name: &str) -> String {
        let s = self.symbol_for(&format!("@@{}", name));
        self.key_string(&s)
    }
}

/// Property key string of a symbol value.
pub fn symbol_key(id: u64, desc: &str) -> String {
    format!("{}{}:{}", SYM_PREFIX, id, desc)
}

