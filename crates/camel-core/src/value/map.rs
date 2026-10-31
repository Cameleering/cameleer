//! [`ObjectMap`]: the insertion-ordered string-keyed map behind [`Value::Object`].
//!
//! Apache Camel's JSON support (Jackson), CSV `useOrderedMaps`, SQL row maps, YAML and most
//! other places produce a `java.util.LinkedHashMap`: iteration follows the order in which keys
//! were first inserted. `ObjectMap` reproduces that. Its API mirrors the subset of
//! `BTreeMap<String, Value>` used across the workspace, so most code only changes the type name.
//!
//! - [`ObjectMap::insert`] on an existing key replaces the value and keeps the key's position
//!   (like `LinkedHashMap.put`).
//! - [`ObjectMap::remove`] shifts the following entries down, preserving the order of the rest.
//! - Equality (`==`) is order-insensitive, like Java's `Map.equals`: two maps are equal when they
//!   hold the same keys mapped to equal values, whatever their order. Use [`ObjectMap::eq_ordered`]
//!   when order matters.
//!
//! Small maps are searched linearly; once a map grows past a few entries a `HashMap` index from
//! key to position is maintained as well.
//!
//! [`Value::Object`]: crate::value::Value::Object

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::iter::FusedIterator;
use std::ops::Index;

use super::Value;

/// Above this many entries, lookups go through a hash index instead of a linear scan.
const INDEX_THRESHOLD: usize = 12;

/// An insertion-ordered map from `String` to [`Value`] (Camel's `LinkedHashMap`).
#[derive(Clone, Default)]
pub struct ObjectMap {
    entries: Vec<(String, Value)>,
    index: Option<HashMap<String, usize>>,
}

impl ObjectMap {
    /// An empty map.
    pub fn new() -> Self {
        ObjectMap { entries: Vec::new(), index: None }
    }

    /// An empty map with room for `n` entries.
    pub fn with_capacity(n: usize) -> Self {
        ObjectMap { entries: Vec::with_capacity(n), index: None }
    }

    fn position(&self, key: &str) -> Option<usize> {
        match &self.index {
            Some(ix) => ix.get(key).copied(),
            None => self.entries.iter().position(|(k, _)| k == key),
        }
    }

    fn rebuild_index(&mut self) {
        if self.entries.len() > INDEX_THRESHOLD {
            self.index = Some(self.entries.iter().enumerate().map(|(i, (k, _))| (k.clone(), i)).collect());
        } else {
            self.index = None;
        }
    }

    /// Insert `key` → `value`. An existing key keeps its position and its old value is returned;
    /// a new key is appended at the end.
    pub fn insert(&mut self, key: String, value: Value) -> Option<Value> {
        if let Some(i) = self.position(&key) {
            return Some(std::mem::replace(&mut self.entries[i].1, value));
        }
        let i = self.entries.len();
        if let Some(ix) = &mut self.index {
            ix.insert(key.clone(), i);
        }
        self.entries.push((key, value));
        if self.index.is_none() && self.entries.len() > INDEX_THRESHOLD {
            self.rebuild_index();
        }
        None
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.position(key).map(|i| &self.entries[i].1)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.position(key).map(move |i| &mut self.entries[i].1)
    }

    pub fn get_key_value(&self, key: &str) -> Option<(&String, &Value)> {
        self.position(key).map(|i| {
            let (k, v) = &self.entries[i];
            (k, v)
        })
    }

    /// Entry at position `i` in insertion order.
    pub fn get_index(&self, i: usize) -> Option<(&String, &Value)> {
        self.entries.get(i).map(|(k, v)| (k, v))
    }

    /// Position of `key` in insertion order.
    pub fn get_index_of(&self, key: &str) -> Option<usize> {
        self.position(key)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.position(key).is_some()
    }

    /// Remove `key`, shifting later entries down (order of the rest is kept).
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.remove_entry(key).map(|(_, v)| v)
    }

    /// Remove `key` and return the stored key and value (order of the rest is kept).
    pub fn remove_entry(&mut self, key: &str) -> Option<(String, Value)> {
        let i = self.position(key)?;
        let e = self.entries.remove(i);
        if self.index.is_some() {
            self.rebuild_index();
        }
        Some(e)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.index = None;
    }

    /// The first entry in insertion order.
    pub fn first(&self) -> Option<(&String, &Value)> {
        self.get_index(0)
    }

    /// The last entry in insertion order.
    pub fn last(&self) -> Option<(&String, &Value)> {
        self.entries.last().map(|(k, v)| (k, v))
    }

    /// Remove and return the last entry.
    pub fn pop(&mut self) -> Option<(String, Value)> {
        let e = self.entries.pop()?;
        if let Some(ix) = &mut self.index {
            ix.remove(&e.0);
        }
        Some(e)
    }

    pub fn iter(&self) -> Iter<'_> {
        Iter(self.entries.iter())
    }

    pub fn iter_mut(&mut self) -> IterMut<'_> {
        IterMut(self.entries.iter_mut())
    }

    pub fn keys(&self) -> Keys<'_> {
        Keys(self.entries.iter())
    }

    pub fn values(&self) -> Values<'_> {
        Values(self.entries.iter())
    }

    pub fn values_mut(&mut self) -> ValuesMut<'_> {
        ValuesMut(self.entries.iter_mut())
    }

    pub fn into_keys(self) -> impl DoubleEndedIterator<Item = String> + ExactSizeIterator {
        self.entries.into_iter().map(|(k, _)| k)
    }

    pub fn into_values(self) -> impl DoubleEndedIterator<Item = Value> + ExactSizeIterator {
        self.entries.into_iter().map(|(_, v)| v)
    }

    /// Keep only the entries for which `f` returns true (order is kept).
    pub fn retain(&mut self, mut f: impl FnMut(&String, &mut Value) -> bool) {
        let before = self.entries.len();
        self.entries.retain_mut(|(k, v)| f(k, v));
        if self.entries.len() != before {
            self.rebuild_index();
        }
    }

    /// Move all entries of `other` into `self` (existing keys keep their position).
    pub fn append(&mut self, other: &mut ObjectMap) {
        let taken = std::mem::take(other);
        self.extend(taken);
    }

    /// Sort the entries by key (for the few outputs that are sorted on purpose).
    pub fn sort_keys(&mut self) {
        self.entries.sort_by(|a, b| a.0.cmp(&b.0));
        self.rebuild_index();
    }

    /// Like `==` but also requires the same key order (recursively for nested objects).
    pub fn eq_ordered(&self, other: &ObjectMap) -> bool {
        self.len() == other.len()
            && self.entries.iter().zip(other.entries.iter()).all(|((ka, va), (kb, vb))| {
                ka == kb
                    && match (va, vb) {
                        (Value::Object(a), Value::Object(b)) => a.eq_ordered(b),
                        _ => va == vb,
                    }
            })
    }

    /// Entry API (like `BTreeMap::entry`).
    pub fn entry(&mut self, key: String) -> Entry<'_> {
        match self.position(&key) {
            Some(index) => Entry::Occupied(OccupiedEntry { map: self, index }),
            None => Entry::Vacant(VacantEntry { map: self, key }),
        }
    }
}

impl PartialEq for ObjectMap {
    /// Order-insensitive, like `java.util.Map.equals`.
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.entries.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

impl fmt::Debug for ObjectMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl Index<&str> for ObjectMap {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        self.get(key).unwrap_or_else(|| panic!("key not found in ObjectMap: {key}"))
    }
}

impl Index<&String> for ObjectMap {
    type Output = Value;
    fn index(&self, key: &String) -> &Value {
        &self[key.as_str()]
    }
}

impl FromIterator<(String, Value)> for ObjectMap {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iter: I) -> Self {
        let mut m = ObjectMap::new();
        m.extend(iter);
        m
    }
}

impl Extend<(String, Value)> for ObjectMap {
    fn extend<I: IntoIterator<Item = (String, Value)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

impl<const N: usize> From<[(String, Value); N]> for ObjectMap {
    fn from(items: [(String, Value); N]) -> Self {
        items.into_iter().collect()
    }
}

impl From<BTreeMap<String, Value>> for ObjectMap {
    /// Entries come in key order (the `BTreeMap`'s iteration order).
    fn from(m: BTreeMap<String, Value>) -> Self {
        m.into_iter().collect()
    }
}

impl From<ObjectMap> for BTreeMap<String, Value> {
    /// Loses the insertion order (entries are re-sorted by key).
    fn from(m: ObjectMap) -> Self {
        m.entries.into_iter().collect()
    }
}

impl From<Vec<(String, Value)>> for ObjectMap {
    fn from(v: Vec<(String, Value)>) -> Self {
        v.into_iter().collect()
    }
}

impl From<ObjectMap> for Vec<(String, Value)> {
    fn from(m: ObjectMap) -> Self {
        m.entries
    }
}

// ---------------------------------------------------------------------------
// Iterators
// ---------------------------------------------------------------------------

macro_rules! iter_impl {
    ($name:ident, $inner:ty, $item:ty, |$e:ident| $map:expr) => {
        impl<'a> Iterator for $name<'a> {
            type Item = $item;
            fn next(&mut self) -> Option<Self::Item> {
                self.0.next().map(|$e| $map)
            }
            fn size_hint(&self) -> (usize, Option<usize>) {
                self.0.size_hint()
            }
        }
        impl<'a> DoubleEndedIterator for $name<'a> {
            fn next_back(&mut self) -> Option<Self::Item> {
                self.0.next_back().map(|$e| $map)
            }
        }
        impl<'a> ExactSizeIterator for $name<'a> {}
        impl<'a> FusedIterator for $name<'a> {}
    };
}

/// Iterator over `(&String, &Value)` in insertion order.
#[derive(Clone)]
pub struct Iter<'a>(std::slice::Iter<'a, (String, Value)>);
iter_impl!(Iter, std::slice::Iter<'a, (String, Value)>, (&'a String, &'a Value), |e| (&e.0, &e.1));

/// Iterator over `(&String, &mut Value)` in insertion order.
pub struct IterMut<'a>(std::slice::IterMut<'a, (String, Value)>);
iter_impl!(IterMut, std::slice::IterMut<'a, (String, Value)>, (&'a String, &'a mut Value), |e| (&e.0, &mut e.1));

/// Iterator over the keys in insertion order.
#[derive(Clone)]
pub struct Keys<'a>(std::slice::Iter<'a, (String, Value)>);
iter_impl!(Keys, std::slice::Iter<'a, (String, Value)>, &'a String, |e| &e.0);

/// Iterator over the values in insertion order.
#[derive(Clone)]
pub struct Values<'a>(std::slice::Iter<'a, (String, Value)>);
iter_impl!(Values, std::slice::Iter<'a, (String, Value)>, &'a Value, |e| &e.1);

/// Iterator over mutable values in insertion order.
pub struct ValuesMut<'a>(std::slice::IterMut<'a, (String, Value)>);
iter_impl!(ValuesMut, std::slice::IterMut<'a, (String, Value)>, &'a mut Value, |e| &mut e.1);

/// Owning iterator over `(String, Value)` in insertion order.
pub struct IntoIter(std::vec::IntoIter<(String, Value)>);

impl Iterator for IntoIter {
    type Item = (String, Value);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
impl DoubleEndedIterator for IntoIter {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back()
    }
}
impl ExactSizeIterator for IntoIter {}
impl FusedIterator for IntoIter {}

impl IntoIterator for ObjectMap {
    type Item = (String, Value);
    type IntoIter = IntoIter;
    fn into_iter(self) -> IntoIter {
        IntoIter(self.entries.into_iter())
    }
}

impl<'a> IntoIterator for &'a ObjectMap {
    type Item = (&'a String, &'a Value);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut ObjectMap {
    type Item = (&'a String, &'a mut Value);
    type IntoIter = IterMut<'a>;
    fn into_iter(self) -> IterMut<'a> {
        self.iter_mut()
    }
}

// ---------------------------------------------------------------------------
// Entry API
// ---------------------------------------------------------------------------

/// A view into a single entry of an [`ObjectMap`].
pub enum Entry<'a> {
    Occupied(OccupiedEntry<'a>),
    Vacant(VacantEntry<'a>),
}

pub struct OccupiedEntry<'a> {
    map: &'a mut ObjectMap,
    index: usize,
}

pub struct VacantEntry<'a> {
    map: &'a mut ObjectMap,
    key: String,
}

impl<'a> Entry<'a> {
    pub fn key(&self) -> &str {
        match self {
            Entry::Occupied(o) => o.key(),
            Entry::Vacant(v) => &v.key,
        }
    }

    pub fn or_insert(self, default: Value) -> &'a mut Value {
        match self {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(default),
        }
    }

    pub fn or_insert_with<F: FnOnce() -> Value>(self, f: F) -> &'a mut Value {
        match self {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(f()),
        }
    }

    pub fn or_default(self) -> &'a mut Value {
        self.or_insert_with(Value::default)
    }

    pub fn and_modify<F: FnOnce(&mut Value)>(mut self, f: F) -> Self {
        if let Entry::Occupied(o) = &mut self {
            f(o.get_mut());
        }
        self
    }
}

impl<'a> OccupiedEntry<'a> {
    pub fn key(&self) -> &str {
        &self.map.entries[self.index].0
    }
    pub fn get(&self) -> &Value {
        &self.map.entries[self.index].1
    }
    pub fn get_mut(&mut self) -> &mut Value {
        &mut self.map.entries[self.index].1
    }
    pub fn into_mut(self) -> &'a mut Value {
        &mut self.map.entries[self.index].1
    }
    pub fn insert(&mut self, value: Value) -> Value {
        std::mem::replace(self.get_mut(), value)
    }
    /// Remove the entry (order of the rest is kept).
    pub fn remove(self) -> Value {
        let e = self.map.entries.remove(self.index);
        if self.map.index.is_some() {
            self.map.rebuild_index();
        }
        e.1
    }
}

impl<'a> VacantEntry<'a> {
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn insert(self, value: Value) -> &'a mut Value {
        let VacantEntry { map, key } = self;
        map.insert(key, value);
        let i = map.entries.len() - 1;
        &mut map.entries[i].1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(m: &ObjectMap) -> Vec<&str> {
        m.keys().map(String::as_str).collect()
    }

    #[test]
    fn insertion_order_and_overwrite() {
        let mut m = ObjectMap::new();
        m.insert("b".into(), Value::from(1));
        m.insert("a".into(), Value::from(2));
        m.insert("c".into(), Value::from(3));
        assert_eq!(keys(&m), ["b", "a", "c"]);
        assert_eq!(m.insert("b".into(), Value::from(9)), Some(Value::from(1)));
        assert_eq!(keys(&m), ["b", "a", "c"]);
        assert_eq!(m["b"], Value::from(9));
        assert_eq!(m.remove("a"), Some(Value::from(2)));
        assert_eq!(keys(&m), ["b", "c"]);
        assert!(!m.contains_key("a"));
    }

    #[test]
    fn large_maps_use_index_consistently() {
        let mut m = ObjectMap::new();
        for i in (0..40).rev() {
            m.insert(format!("k{i}"), Value::from(i));
        }
        assert_eq!(m.len(), 40);
        assert_eq!(m.get("k7"), Some(&Value::from(7)));
        assert_eq!(m.first().unwrap().0, "k39");
        m.remove("k39");
        assert_eq!(m.first().unwrap().0, "k38");
        assert_eq!(m.get("k0"), Some(&Value::from(0)));
        m.retain(|_, v| v.as_i64().unwrap() % 2 == 0);
        assert_eq!(m.len(), 20);
        assert_eq!(m.get("k10"), Some(&Value::from(10)));
        assert_eq!(m.get("k11"), None);
        m.insert("k10".into(), Value::from(100));
        assert_eq!(m.len(), 20);
        assert_eq!(m.get_index_of("k38"), Some(0));
    }

    #[test]
    fn equality_is_order_insensitive() {
        let a: ObjectMap = [("x".to_string(), Value::from(1)), ("y".to_string(), Value::from(2))].into();
        let b: ObjectMap = [("y".to_string(), Value::from(2)), ("x".to_string(), Value::from(1))].into();
        assert_eq!(a, b);
        assert!(!a.eq_ordered(&b));
        assert!(a.eq_ordered(&a.clone()));
    }

    #[test]
    fn entry_api() {
        let mut m = ObjectMap::new();
        m.entry("n".into()).or_insert(Value::from(0));
        if let Value::Int(i) = m.entry("n".into()).or_insert(Value::Null) {
            *i += 5;
        }
        m.entry("z".into()).or_default();
        assert_eq!(m["n"], Value::from(5));
        assert_eq!(keys(&m), ["n", "z"]);
        let mut sorted = m.clone();
        sorted.insert("a".into(), Value::Null);
        sorted.sort_keys();
        assert_eq!(keys(&sorted), ["a", "n", "z"]);
        let bt: BTreeMap<String, Value> = sorted.into();
        assert_eq!(ObjectMap::from(bt).len(), 3);
    }
}
