//! Runtime values of the script interpreter (a dynamic object model over `camel_core::Value`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use camel_core::value::{ObjectMap, Value};

use crate::ast::FuncDef;
use crate::interp::Env;
pub use crate::arena::{new_arr_ref, new_closure, new_obj_ref};

/// A shared, mutable array.
pub type ArrRef = Rc<RefCell<Vec<V>>>;
/// A shared, mutable object.
pub type ObjRef = Rc<RefCell<Obj>>;

/// A runtime value.
#[derive(Clone)]
pub enum V {
    Undef,
    Null,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(Rc<str>),
    Bytes(Rc<Vec<u8>>),
    Arr(ArrRef),
    Obj(ObjRef),
    Func(Rc<Closure>),
    /// A method of a value looked up without calling it (`s.trim`), called later.
    Method(Rc<(V, String)>),
    /// Built-in host objects (exchange bindings, `Math`, `JSON`, `std`, ...).
    Host(Rc<Host>),
    Regex(Rc<RegexVal>),
}

/// A compiled regular expression value (JS `/x/g`, Groovy `~/x/`).
pub struct RegexVal {
    pub source: String,
    pub flags: String,
    pub re: camel_core::regex::Regex,
}

/// Shared interior state of a special object (compared by identity).
pub struct Internal<T>(pub Rc<RefCell<T>>);

impl<T> Internal<T> {
    pub fn new(v: T) -> Internal<T> {
        Internal(Rc::new(RefCell::new(v)))
    }
}

impl<T> Clone for Internal<T> {
    fn clone(&self) -> Self {
        Internal(self.0.clone())
    }
}

impl<T> PartialEq for Internal<T> {
    fn eq(&self, o: &Self) -> bool {
        Rc::ptr_eq(&self.0, &o.0)
    }
}

impl<T> std::fmt::Debug for Internal<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<internal>")
    }
}

/// Kind of an object.
#[derive(Clone, Debug, PartialEq)]
pub enum ObjKind {
    Plain,
    Error,
    Date(f64),
    /// A set (Groovy `as Set`, Python `set()`), stored as keys mapped to the element.
    Set,
    /// Python keyword arguments passed as the last call argument.
    Kwargs,
    /// An accessor property (`get` / `set` functions in the props), stored as a property value.
    Accessor,
    /// A JavaScript symbol (id, description).
    Symbol(u64, Rc<str>),
    /// JavaScript `Map` / `Set` / `WeakMap` / `WeakSet`.
    Coll(Internal<crate::collections::Coll>),
    /// A promise.
    Promise(Internal<crate::promise::PromiseState>),
    /// A generator object (JavaScript `function*`, Python generator functions, async functions).
    Generator(Internal<crate::gen::GenState>),
    /// `java.util.Optional` (`value` prop, absent when empty).
    Optional,
    /// A DataSonnet object with late-bound `self` / `super` and hidden fields.
    Jsonnet(Internal<crate::jobj::JMeta>),
    /// A JavaScript `BigInt`.
    BigInt(Rc<crate::bigint::BigInt>),
    /// A JavaScript `Proxy` (target, handler).
    Proxy(Internal<(V, V)>),
    /// A JavaScript `ArrayBuffer`.
    Buffer(Internal<Vec<u8>>),
    /// A JavaScript typed array or `DataView`.
    Typed(crate::jsext::Typed),
}

/// An object / map with insertion-ordered keys and an optional prototype.
pub struct Obj {
    pub props: PropMap,
    pub proto: Option<ObjRef>,
    pub kind: ObjKind,
}

impl Obj {
    pub fn new() -> Obj {
        Obj { props: PropMap::default(), proto: None, kind: ObjKind::Plain }
    }
    /// Own or inherited property.
    pub fn lookup(&self, key: &str) -> Option<V> {
        if let Some(v) = self.props.get(key) {
            return Some(v.clone());
        }
        let mut p = self.proto.clone();
        while let Some(o) = p {
            let b = o.borrow();
            if let Some(v) = b.props.get(key) {
                return Some(v.clone());
            }
            p = b.proto.clone();
        }
        None
    }
}

impl Default for Obj {
    fn default() -> Self {
        Obj::new()
    }
}

/// Prefix of property keys that are symbols (kept apart from the string keys).
pub const SYM_PREFIX: &str = "@@sym:";

fn is_sym(k: &str) -> bool {
    k.starts_with(SYM_PREFIX)
}

/// Insertion ordered property map. Symbol keys are stored apart and are not enumerated.
#[derive(Clone, Default)]
pub struct PropMap {
    keys: Vec<Rc<str>>,
    map: HashMap<Rc<str>, V>,
    syms: Option<Box<HashMap<Rc<str>, V>>>,
}

impl PropMap {
    pub fn get(&self, k: &str) -> Option<&V> {
        if is_sym(k) {
            return self.syms.as_ref().and_then(|m| m.get(k));
        }
        self.map.get(k)
    }
    pub fn contains(&self, k: &str) -> bool {
        if is_sym(k) {
            return self.syms.as_ref().is_some_and(|m| m.contains_key(k));
        }
        self.map.contains_key(k)
    }
    /// Symbol keyed properties.
    pub fn symbols(&self) -> Vec<(Rc<str>, V)> {
        self.syms.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()
    }
    pub fn set(&mut self, k: &str, v: V) {
        if is_sym(k) {
            self.syms.get_or_insert_with(Default::default).insert(Rc::from(k), v);
            return;
        }
        if let Some(slot) = self.map.get_mut(k) {
            *slot = v;
        } else {
            let key: Rc<str> = Rc::from(k);
            self.keys.push(key.clone());
            self.map.insert(key, v);
        }
    }
    pub fn remove(&mut self, k: &str) -> Option<V> {
        if is_sym(k) {
            return self.syms.as_mut().and_then(|m| m.remove(k));
        }
        let v = self.map.remove(k)?;
        self.keys.retain(|x| &**x != k);
        Some(v)
    }
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub fn clear(&mut self) {
        self.keys.clear();
        self.map.clear();
        self.syms = None;
    }
    pub fn keys(&self) -> impl Iterator<Item = &Rc<str>> {
        self.keys.iter()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&Rc<str>, &V)> {
        self.keys.iter().map(move |k| (k, &self.map[k]))
    }
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.keys.iter().map(move |k| &self.map[k])
    }
    /// Sort the keys (Jsonnet field order).
    pub fn sort_keys(&mut self) {
        self.keys.sort();
    }
}

/// How a closure is invoked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Plain,
    /// A class constructor (calling it constructs an instance).
    Class,
    /// Python instance method (`self` is passed as the first argument when bound).
    PyMethod,
    /// Python `@staticmethod`.
    PyStatic,
    /// Python `@classmethod` (the class is passed as the first argument).
    PyClassMethod,
}

/// A user defined function / closure.
pub struct Closure {
    pub def: Arc<FuncDef>,
    pub env: Env,
    pub props: RefCell<PropMap>,
    /// Class constructors: the parent class constructor (`super(...)`).
    pub parent: Option<V>,
    /// Class constructors: the home object for `super.method()`.
    pub home: RefCell<Option<ObjRef>>,
    pub flavor: Flavor,
}

/// Built-in host objects.
#[derive(Clone, Debug, PartialEq)]
pub enum Host {
    Exchange,
    Message,
    Headers,
    Properties,
    Variables,
    Context,
    Log,
    /// The root object of OGNL / MVEL / SpEL expressions.
    Root,
    /// A namespace, constructor or global function (`Math`, `JSON`, `String`, `parseInt`, `len`, `std`...).
    Ns(&'static str),
    /// A namespace member function (`Math.max`, `std.length`), called later.
    NsFn(&'static str, String),
    /// A (Java) class name without built-in support (exception types, ...).
    Type(String),
    /// A registry bean (SpEL `@name`).
    Bean(String),
    /// Groovy `Type.metaClass` (assigning closures adds methods to the type).
    Meta(String),
}

impl V {
    pub fn str(s: &str) -> V {
        V::Str(Rc::from(s))
    }
    pub fn string(s: String) -> V {
        V::Str(Rc::from(s))
    }
    pub fn arr(v: Vec<V>) -> V {
        V::Arr(new_arr_ref(v))
    }
    pub fn obj(p: PropMap) -> V {
        V::Obj(new_obj_ref(Obj { props: p, proto: None, kind: ObjKind::Plain }))
    }
    pub fn obj_kind(p: PropMap, kind: ObjKind) -> V {
        V::Obj(new_obj_ref(Obj { props: p, proto: None, kind }))
    }
    pub fn new_obj() -> V {
        V::obj(PropMap::default())
    }
    pub fn host(h: Host) -> V {
        V::Host(Rc::new(h))
    }
    pub fn is_nullish(&self) -> bool {
        matches!(self, V::Undef | V::Null)
    }
    pub fn is_number(&self) -> bool {
        matches!(self, V::Int(_) | V::Num(_))
    }
    pub fn is_callable(&self) -> bool {
        matches!(self, V::Func(_) | V::Method(_)) || matches!(self, V::Host(h) if matches!(**h, Host::Ns(_) | Host::NsFn(..) | Host::Type(_))) || (matches!(self, V::Obj(_)) && crate::jsext::proxy_callable(self))
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            V::Int(i) => Some(*i as f64),
            V::Num(f) => Some(*f),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            V::Str(s) => Some(s),
            _ => None,
        }
    }
    /// Same object identity (or primitive equality).
    pub fn same(&self, o: &V) -> bool {
        match (self, o) {
            (V::Arr(a), V::Arr(b)) => Rc::ptr_eq(a, b),
            (V::Obj(a), V::Obj(b)) => Rc::ptr_eq(a, b),
            (V::Func(a), V::Func(b)) => Rc::ptr_eq(a, b),
            (V::Host(a), V::Host(b)) => a == b,
            (V::Undef, V::Undef) | (V::Null, V::Null) => true,
            (V::Bool(a), V::Bool(b)) => a == b,
            (V::Str(a), V::Str(b)) => a == b,
            (a, b) if a.is_number() && b.is_number() => a.as_f64() == b.as_f64(),
            _ => false,
        }
    }
}

/// Convert a Camel value to a runtime value (deep copy).
pub fn from_value(v: &Value) -> V {
    match v {
        Value::Null => V::Null,
        Value::Bool(b) => V::Bool(*b),
        Value::Int(i) => V::Int(*i),
        Value::Float(f) => V::Num(*f),
        Value::String(s) => V::str(s),
        Value::Bytes(b) => V::Bytes(Rc::new(b.clone())),
        Value::Array(a) => V::arr(a.iter().map(from_value).collect()),
        Value::Object(o) => {
            let mut p = PropMap::default();
            for (k, x) in o {
                p.set(k, from_value(x));
            }
            V::obj(p)
        }
    }
}

/// Convert a runtime value to a Camel value (deep copy). Functions become null; dates become
/// ISO-8601 strings.
pub fn to_value(v: &V) -> Value {
    to_value_depth(v, 0)
}

fn to_value_depth(v: &V, depth: usize) -> Value {
    if depth > 200 {
        return Value::Null;
    }
    match v {
        V::Undef | V::Null => Value::Null,
        V::Bool(b) => Value::Bool(*b),
        V::Int(i) => Value::Int(*i),
        V::Num(f) => Value::Float(*f),
        V::Str(s) => Value::String(s.to_string()),
        V::Bytes(b) => Value::Bytes((**b).clone()),
        V::Arr(a) => Value::Array(a.borrow().iter().map(|x| to_value_depth(x, depth + 1)).collect()),
        V::Obj(o) => {
            let o = o.borrow();
            if let Some(v) = crate::jsext::to_value(&o, &|x| to_value_depth(x, depth + 1)) {
                return v;
            }
            match &o.kind {
                ObjKind::Date(ms) => Value::String(crate::builtins::iso_date(*ms)),
                ObjKind::Set => Value::Array(o.props.values().map(|x| to_value_depth(x, depth + 1)).collect()),
                ObjKind::Coll(c) => crate::collections::coll_to_value(&c.0.borrow(), &|x| to_value_depth(x, depth + 1), &|k| match k {
                    V::Str(s) => s.to_string(),
                    V::Int(i) => i.to_string(),
                    other => match to_value_depth(other, depth + 1) {
                        Value::String(s) => s,
                        v => v.to_string(),
                    },
                }),
                ObjKind::Symbol(..) | ObjKind::Accessor | ObjKind::Promise(_) | ObjKind::Generator(_) => Value::Null,
                ObjKind::Optional => o.props.get("value").map(|x| to_value_depth(x, depth + 1)).unwrap_or(Value::Null),
                _ => {
                    let mut m = ObjectMap::new();
                    for (k, x) in o.props.iter() {
                        if matches!(x, V::Func(_)) {
                            continue;
                        }
                        m.insert(k.to_string(), to_value_depth(x, depth + 1));
                    }
                    Value::Object(m)
                }
            }
        }
        V::Regex(r) => Value::String(r.source.clone()),
        V::Func(_) | V::Method(_) | V::Host(_) => Value::Null,
    }
}
