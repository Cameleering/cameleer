//! Message, headers and the Exchange.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::value::{FromValue, Value};

/// Case-insensitive header map (Camel headers are case-insensitive) that preserves
/// the original key spelling and insertion-independent (sorted) iteration order.
///
/// Stored as a vector in insertion order: a lookup is a scan comparing lengths, then the
/// bytes ignoring ASCII case; a new header is appended. Iteration sorts by the lowercased
/// key (the order of the map this replaced). Messages carry few headers, and this needs one
/// allocation per key.
#[derive(Clone, Default)]
pub struct Headers {
    entries: Vec<(HeaderKey, Value)>,
}

/// A header name: owned, or shared by a processor that sets the same header on every
/// exchange (copies of the headers then share it instead of copying it).
#[derive(Clone)]
enum HeaderKey {
    Owned(String),
    Shared(std::sync::Arc<str>),
}

impl HeaderKey {
    #[inline]
    fn as_str(&self) -> &str {
        match self {
            HeaderKey::Owned(s) => s,
            HeaderKey::Shared(s) => s,
        }
    }
}

/// Compare two keys as their ASCII-lowercased forms (the iteration order).
#[inline]
fn cmp_ignore_ascii_case(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    for (x, y) in a.iter().zip(b) {
        match x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase()) {
            std::cmp::Ordering::Equal => {}
            o => return o,
        }
    }
    a.len().cmp(&b.len())
}

impl PartialEq for Headers {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.entries.iter().all(|(k, v)| other.entries.iter().any(|(ok, ov)| ok.as_str() == k.as_str() && ov == v))
    }
}

impl std::fmt::Debug for Headers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.sorted()).finish()
    }
}

impl Headers {
    pub fn new() -> Self {
        Self::default()
    }
    #[inline]
    fn position(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|(k, _)| {
            let k = k.as_str();
            k.len() == key.len() && k.eq_ignore_ascii_case(key)
        })
    }
    /// The entries in iteration order (sorted ignoring ASCII case).
    fn sorted(&self) -> Vec<(&str, &Value)> {
        let mut v: Vec<(&str, &Value)> = self.entries.iter().map(|(k, v)| (k.as_str(), v)).collect();
        v.sort_by(|a, b| cmp_ignore_ascii_case(a.0, b.0));
        v
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.position(key).map(|i| &self.entries[i].1)
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.position(key).map(move |i| &mut self.entries[i].1)
    }
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        let key = key.into();
        let at = self.position(&key);
        self.put(at, HeaderKey::Owned(key), value.into());
    }
    /// [`Headers::set`] with a name the caller keeps and shares (no copy of the name).
    pub fn set_shared(&mut self, key: &std::sync::Arc<str>, value: impl Into<Value>) {
        let at = self.position(key);
        self.put(at, HeaderKey::Shared(key.clone()), value.into());
    }
    #[inline]
    fn put(&mut self, at: Option<usize>, key: HeaderKey, value: Value) {
        match at {
            // the latest spelling of the key wins, as with the old map
            Some(i) => self.entries[i] = (key, value),
            None => self.entries.push((key, value)),
        }
    }
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.position(key).map(|i| self.entries.swap_remove(i).1)
    }
    pub fn contains(&self, key: &str) -> bool {
        self.position(key).is_some()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn clear(&mut self) {
        self.entries.clear()
    }
    /// Iterate `(original_key, value)` pairs (sorted ignoring ASCII case).
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.sorted().into_iter()
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.sorted().into_iter().map(|(k, _)| k)
    }
    /// Remove headers whose key matches a Camel-style pattern: exact (case-insensitive),
    /// wildcard (`Camel*`) or regex. Returns number removed.
    pub fn remove_matching(&mut self, pattern: &str, excludes: &[&str]) -> usize {
        let before = self.entries.len();
        self.entries.retain(|(orig, _)| {
            let orig = orig.as_str();
            !(crate::util::matches_pattern(orig, pattern) && !excludes.iter().any(|e| crate::util::matches_pattern(orig, e)))
        });
        before - self.entries.len()
    }
    /// Convert to a plain object Value (original key casing).
    pub fn to_value(&self) -> Value {
        Value::Object(self.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }
}

impl<K: Into<String>, V: Into<Value>> FromIterator<(K, V)> for Headers {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut h = Headers::new();
        for (k, v) in iter {
            h.set(k, v);
        }
        h
    }
}

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn id_prefix() -> &'static str {
    use std::sync::OnceLock;
    static PREFIX: OnceLock<String> = OnceLock::new();
    PREFIX.get_or_init(|| {
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        format!("ID-cameleer-{:x}-{}", t, std::process::id())
    })
}

/// Ids handed out per thread at once, so threads do not contend on the shared counter.
const ID_BLOCK: u64 = 256;

thread_local! {
    /// The next id number and the end of this thread's block.
    static ID_RANGE: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

fn next_id_number() -> u64 {
    ID_RANGE
        .try_with(|r| {
            let (mut next, mut end) = r.get();
            if next == end {
                next = ID_COUNTER.fetch_add(ID_BLOCK, Ordering::Relaxed);
                end = next + ID_BLOCK;
            }
            r.set((next + 1, end));
            next
        })
        .unwrap_or_else(|_| ID_COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Generate a new unique id (used for exchange and message ids).
pub fn generate_id() -> String {
    let n = next_id_number();
    // built by hand: `format!` costs more than the allocation on this hot path
    let prefix = id_prefix();
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    let mut v = n;
    loop {
        i -= 1;
        digits[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let mut id = String::with_capacity(prefix.len() + 1 + digits.len() - i);
    id.push_str(prefix);
    id.push('-');
    id.push_str(std::str::from_utf8(&digits[i..]).expect("ascii digits"));
    id
}

/// Exchange properties: a map from name to value with the API and the (sorted) iteration
/// order of a `BTreeMap<String, Value>`.
///
/// Stored as a vector in insertion order: exchanges carry a few properties and the EIPs set,
/// read and remove some for every exchange, so a lookup is a scan comparing lengths and then
/// (usually interned, identical) pointers, an insert appends and a remove swaps. Iteration
/// sorts by name, so the order seen is the map's order.
#[derive(Clone, Default)]
pub struct Properties {
    /// Well-known names are stored as `'static` strings (see `intern_name`).
    entries: Vec<(std::borrow::Cow<'static, str>, Value)>,
}

impl PartialEq for Properties {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.entries.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

impl std::fmt::Debug for Properties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

/// The `'static` copy of a well-known Camel property name, so storing it allocates nothing.
fn intern_name(key: &str) -> Option<&'static str> {
    Some(match key {
        props::CORRELATION_ID => props::CORRELATION_ID,
        props::SPLIT_INDEX => props::SPLIT_INDEX,
        props::SPLIT_SIZE => props::SPLIT_SIZE,
        props::SPLIT_COMPLETE => props::SPLIT_COMPLETE,
        props::MULTICAST_INDEX => props::MULTICAST_INDEX,
        props::MULTICAST_COMPLETE => props::MULTICAST_COMPLETE,
        props::AGGREGATED_SIZE => props::AGGREGATED_SIZE,
        props::AGGREGATED_COMPLETED_BY => props::AGGREGATED_COMPLETED_BY,
        props::AGGREGATED_CORRELATION_KEY => props::AGGREGATED_CORRELATION_KEY,
        props::LOOP_INDEX => props::LOOP_INDEX,
        props::LOOP_SIZE => props::LOOP_SIZE,
        props::FILTER_MATCHED => props::FILTER_MATCHED,
        props::ROUTE_STOP => props::ROUTE_STOP,
        props::ERRORHANDLER_HANDLED => props::ERRORHANDLER_HANDLED,
        props::EXCEPTION_CAUGHT => props::EXCEPTION_CAUGHT,
        props::FAILURE_ENDPOINT => props::FAILURE_ENDPOINT,
        props::FAILURE_ROUTE_ID => props::FAILURE_ROUTE_ID,
        props::FATAL_FALLBACK_ERROR_HANDLER => props::FATAL_FALLBACK_ERROR_HANDLER,
        props::REDELIVERED => props::REDELIVERED,
        props::REDELIVERY_COUNTER => props::REDELIVERY_COUNTER,
        props::REDELIVERY_MAX_COUNTER => props::REDELIVERY_MAX_COUNTER,
        props::REDELIVERY_EXHAUSTED => props::REDELIVERY_EXHAUSTED,
        props::TO_ENDPOINT => props::TO_ENDPOINT,
        props::SLIP_ENDPOINT => props::SLIP_ENDPOINT,
        props::RECIPIENT_LIST_ENDPOINT => props::RECIPIENT_LIST_ENDPOINT,
        props::TIMER_NAME => props::TIMER_NAME,
        props::TIMER_FIRED_TIME => props::TIMER_FIRED_TIME,
        props::TIMER_COUNTER => props::TIMER_COUNTER,
        props::TIMER_PERIOD => props::TIMER_PERIOD,
        props::CREATED_TIMESTAMP => props::CREATED_TIMESTAMP,
        props::BATCH_INDEX => props::BATCH_INDEX,
        props::BATCH_SIZE => props::BATCH_SIZE,
        props::BATCH_COMPLETE => props::BATCH_COMPLETE,
        props::DUPLICATE_MESSAGE => props::DUPLICATE_MESSAGE,
        props::FILE_NAME => props::FILE_NAME,
        props::FILE_NAME_ONLY => props::FILE_NAME_ONLY,
        props::FILE_PATH => props::FILE_PATH,
        props::FILE_LENGTH => props::FILE_LENGTH,
        props::FILE_LAST_MODIFIED => props::FILE_LAST_MODIFIED,
        props::HTTP_RESPONSE_CODE => props::HTTP_RESPONSE_CODE,
        props::HTTP_METHOD => props::HTTP_METHOD,
        props::HTTP_URI => props::HTTP_URI,
        props::HTTP_PATH => props::HTTP_PATH,
        props::HTTP_QUERY => props::HTTP_QUERY,
        props::CONTENT_TYPE => props::CONTENT_TYPE,
        props::CHARSET_NAME => props::CHARSET_NAME,
        props::STEP_ID => props::STEP_ID,
        props::ON_COMPLETION => props::ON_COMPLETION,
        props::INTERCEPTED_ENDPOINT => props::INTERCEPTED_ENDPOINT,
        props::CIRCUIT_BREAKER_STATE => props::CIRCUIT_BREAKER_STATE,
        props::RESPONSE_SUCCESSFUL_EXECUTION => props::RESPONSE_SUCCESSFUL_EXECUTION,
        props::RESPONSE_FROM_FALLBACK => props::RESPONSE_FROM_FALLBACK,
        props::RESPONSE_SHORT_CIRCUITED => props::RESPONSE_SHORT_CIRCUITED,
        props::RESPONSE_TIMED_OUT => props::RESPONSE_TIMED_OUT,
        props::RESPONSE_REJECTED => props::RESPONSE_REJECTED,
        crate::history::MESSAGE_HISTORY => crate::history::MESSAGE_HISTORY,
        crate::processor::errors::TRY_ROUTE_BLOCK => crate::processor::errors::TRY_ROUTE_BLOCK,
        crate::processor::errors::ROLLBACK_ONLY_LAST => crate::processor::errors::ROLLBACK_ONLY_LAST,
        crate::processor::errors::ORIGINAL_MESSAGE => crate::processor::errors::ORIGINAL_MESSAGE,
        crate::processor::errors::ON_COMPLETION_REGISTERED => crate::processor::errors::ON_COMPLETION_REGISTERED,
        crate::processor::routing::strategies::GROUPED_EXCHANGE => crate::processor::routing::strategies::GROUPED_EXCHANGE,
        crate::processor::routing::strategies::AGGREGATION_COMPLETE_CURRENT_GROUP => crate::processor::routing::strategies::AGGREGATION_COMPLETE_CURRENT_GROUP,
        crate::processor::routing::strategies::AGGREGATION_COMPLETE_ALL_GROUPS => crate::processor::routing::strategies::AGGREGATION_COMPLETE_ALL_GROUPS,
        crate::processor::routing::strategies::AGGREGATION_COMPLETE_ALL_GROUPS_INCLUSIVE => crate::processor::routing::strategies::AGGREGATION_COMPLETE_ALL_GROUPS_INCLUSIVE,
        _ => return None,
    })
}

/// A vacant or occupied slot of [`Properties::entry`].
pub struct PropertyEntry<'a> {
    map: &'a mut Properties,
    index: Option<usize>,
    key: String,
}

impl<'a> PropertyEntry<'a> {
    pub fn or_insert_with(self, f: impl FnOnce() -> Value) -> &'a mut Value {
        let i = match self.index {
            Some(i) => i,
            None => {
                self.map.entries.push((name_of(self.key), f()));
                self.map.entries.len() - 1
            }
        };
        &mut self.map.entries[i].1
    }
    pub fn or_insert(self, v: Value) -> &'a mut Value {
        self.or_insert_with(|| v)
    }
    pub fn or_default(self) -> &'a mut Value {
        self.or_insert_with(Value::default)
    }
}

impl Properties {
    pub fn new() -> Self {
        Self::default()
    }
    /// Index of `key`: compares lengths, then the (usually interned, so identical) pointers
    /// before the bytes (Camel's names share long prefixes).
    #[inline]
    fn position(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|(k, _)| k.len() == key.len() && (std::ptr::eq(k.as_ptr(), key.as_ptr()) || **k == *key))
    }
    /// The entries sorted by name (the iteration order).
    fn sorted(&self) -> Vec<(&str, &Value)> {
        let mut v: Vec<(&str, &Value)> = self.entries.iter().map(|(k, v)| (&**k, v)).collect();
        v.sort_unstable_by(|a, b| a.0.cmp(b.0));
        v
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.position(key).map(|i| &self.entries[i].1)
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.position(key).map(move |i| &mut self.entries[i].1)
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.position(key).is_some()
    }
    /// Insert or replace a value; returns the previous one.
    pub fn insert(&mut self, key: String, value: Value) -> Option<Value> {
        self.set(key, value)
    }
    /// Like [`Properties::insert`], converting the key only when it is new.
    pub fn set(&mut self, key: impl Into<String> + AsRef<str>, value: Value) -> Option<Value> {
        if let Some(i) = self.position(key.as_ref()) {
            return Some(std::mem::replace(&mut self.entries[i].1, value));
        }
        let name = match intern_name(key.as_ref()) {
            Some(s) => std::borrow::Cow::Borrowed(s),
            None => std::borrow::Cow::Owned(key.into()),
        };
        self.entries.push((name, value));
        None
    }
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.position(key).map(|i| self.entries.swap_remove(i).1)
    }
    pub fn entry(&mut self, key: String) -> PropertyEntry<'_> {
        let index = self.position(&key);
        PropertyEntry { map: self, index, key }
    }
    pub fn clear(&mut self) {
        self.entries.clear()
    }
    /// A copy with capacity for `extra` more properties.
    pub fn clone_with_room(&self, extra: usize) -> Properties {
        let mut entries = Vec::with_capacity(self.entries.len() + extra);
        entries.extend(self.entries.iter().cloned());
        Properties { entries }
    }
    pub fn retain(&mut self, mut f: impl FnMut(&str, &mut Value) -> bool) {
        self.entries.retain_mut(|(k, v)| f(k, v))
    }
    /// `(name, value)` pairs in name order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&str, &Value)> + ExactSizeIterator {
        self.sorted().into_iter()
    }
    pub fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = (&str, &mut Value)> + ExactSizeIterator {
        let mut v: Vec<(&str, &mut Value)> = self.entries.iter_mut().map(|(k, v)| (&**k, v)).collect();
        v.sort_unstable_by(|a, b| a.0.cmp(b.0));
        v.into_iter()
    }
    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &str> + ExactSizeIterator {
        self.sorted().into_iter().map(|(k, _)| k)
    }
    pub fn values(&self) -> impl DoubleEndedIterator<Item = &Value> + ExactSizeIterator {
        self.sorted().into_iter().map(|(_, v)| v)
    }
}

impl<K: Into<String> + AsRef<str>> Extend<(K, Value)> for Properties {
    fn extend<T: IntoIterator<Item = (K, Value)>>(&mut self, iter: T) {
        for (k, v) in iter {
            self.set(k, v);
        }
    }
}

impl<K: Into<String> + AsRef<str>> FromIterator<(K, Value)> for Properties {
    fn from_iter<T: IntoIterator<Item = (K, Value)>>(iter: T) -> Self {
        let mut p = Properties::new();
        p.extend(iter);
        p
    }
}

fn name_of(key: String) -> std::borrow::Cow<'static, str> {
    match intern_name(&key) {
        Some(s) => std::borrow::Cow::Borrowed(s),
        None => std::borrow::Cow::Owned(key),
    }
}

impl From<BTreeMap<String, Value>> for Properties {
    fn from(m: BTreeMap<String, Value>) -> Self {
        // the keys are unique
        Properties { entries: m.into_iter().map(|(k, v)| (name_of(k), v)).collect() }
    }
}

impl From<Properties> for BTreeMap<String, Value> {
    fn from(p: Properties) -> Self {
        p.into_iter().collect()
    }
}

impl IntoIterator for Properties {
    type Item = (String, Value);
    type IntoIter = std::iter::Map<std::vec::IntoIter<(std::borrow::Cow<'static, str>, Value)>, fn((std::borrow::Cow<'static, str>, Value)) -> (String, Value)>;
    fn into_iter(mut self) -> Self::IntoIter {
        self.entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        self.entries.into_iter().map(|(k, v)| (k.into_owned(), v))
    }
}

impl<'a> IntoIterator for &'a Properties {
    type Item = (&'a str, &'a Value);
    type IntoIter = std::vec::IntoIter<(&'a str, &'a Value)>;
    fn into_iter(self) -> Self::IntoIter {
        self.sorted().into_iter()
    }
}

/// An exchange or message id, generated on first use (Camel 4 also creates ids lazily).
///
/// Dereferences to the id `String`. Cloning generates the id first, so a copy of an
/// exchange keeps the same id.
pub struct LazyId {
    /// Null until generated (or set); then a leaked `Box<String>`, owned by this id.
    /// A pointer keeps the id (and so every exchange, which has two) small.
    ptr: std::sync::atomic::AtomicPtr<String>,
}

impl Default for LazyId {
    fn default() -> Self {
        LazyId::new()
    }
}

impl Drop for LazyId {
    fn drop(&mut self) {
        let p = *self.ptr.get_mut();
        if !p.is_null() {
            // SAFETY: a non-null pointer came from `Box::into_raw` and is owned by this id.
            drop(unsafe { Box::from_raw(p) });
        }
    }
}

impl LazyId {
    /// An id that is generated with [`generate_id`] when first read.
    pub const fn new() -> Self {
        LazyId { ptr: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()) }
    }
    fn ready(s: String) -> Self {
        LazyId { ptr: std::sync::atomic::AtomicPtr::new(Box::into_raw(Box::new(s))) }
    }
    pub fn as_str(&self) -> &str {
        self
    }
    /// Whether the id has been generated (or set) yet.
    pub fn is_generated(&self) -> bool {
        !self.ptr.load(Ordering::Acquire).is_null()
    }
    #[cold]
    fn init(&self) -> &String {
        let fresh = Box::into_raw(Box::new(generate_id()));
        match self.ptr.compare_exchange(std::ptr::null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire) {
            // SAFETY: published; never changed or freed while `self` lives.
            Ok(_) => unsafe { &*fresh },
            Err(existing) => {
                // another thread generated it first
                // SAFETY: `fresh` was never shared; `existing` is published as above.
                unsafe {
                    drop(Box::from_raw(fresh));
                    &*existing
                }
            }
        }
    }
}

impl std::ops::Deref for LazyId {
    type Target = String;
    #[inline]
    fn deref(&self) -> &String {
        let p = self.ptr.load(Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: a published string is never changed or freed while `self` lives.
            return unsafe { &*p };
        }
        self.init()
    }
}

impl Clone for LazyId {
    fn clone(&self) -> Self {
        LazyId::from(String::clone(self))
    }
}

impl From<String> for LazyId {
    fn from(s: String) -> Self {
        LazyId::ready(s)
    }
}

impl From<&str> for LazyId {
    fn from(s: &str) -> Self {
        LazyId::from(s.to_string())
    }
}

impl From<&LazyId> for String {
    fn from(id: &LazyId) -> Self {
        String::clone(id)
    }
}

impl From<LazyId> for String {
    fn from(id: LazyId) -> Self {
        let mut id = id;
        let p = std::mem::replace(id.ptr.get_mut(), std::ptr::null_mut());
        if p.is_null() {
            generate_id()
        } else {
            // SAFETY: owned by `id`, which no longer points to it.
            *unsafe { Box::from_raw(p) }
        }
    }
}

impl From<&LazyId> for Value {
    fn from(id: &LazyId) -> Self {
        Value::String(String::clone(id))
    }
}

impl std::fmt::Display for LazyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self)
    }
}

impl std::fmt::Debug for LazyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl PartialEq for LazyId {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for LazyId {}

impl std::hash::Hash for LazyId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state)
    }
}

impl PartialEq<str> for LazyId {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for LazyId {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for LazyId {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<LazyId> for String {
    fn eq(&self, other: &LazyId) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<LazyId> for &str {
    fn eq(&self, other: &LazyId) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<LazyId> for str {
    fn eq(&self, other: &LazyId) -> bool {
        self == other.as_str()
    }
}

/// A message: headers + body.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Message {
    pub message_id: LazyId,
    pub headers: Headers,
    pub body: Value,
}

impl Message {
    pub fn new(body: impl Into<Value>) -> Self {
        Message { message_id: LazyId::new(), headers: Headers::new(), body: body.into() }
    }
    pub fn body(&self) -> &Value {
        &self.body
    }
    pub fn set_body(&mut self, body: impl Into<Value>) {
        self.body = body.into();
    }
    pub fn body_as<T: FromValue>(&self) -> Result<T> {
        T::from_value(&self.body)
    }
    pub fn header(&self, key: &str) -> Option<&Value> {
        self.headers.get(key)
    }
    pub fn header_as<T: FromValue>(&self, key: &str) -> Result<Option<T>> {
        match self.headers.get(key) {
            None => Ok(None),
            Some(v) => T::from_value(v).map(Some),
        }
    }
    pub fn set_header(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.headers.set(key, value);
    }
    pub fn remove_header(&mut self, key: &str) -> Option<Value> {
        self.headers.remove(key)
    }
}

/// Message exchange pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExchangePattern {
    #[default]
    InOnly,
    InOut,
}

impl ExchangePattern {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "InOnly" | "inOnly" | "in-only" => Ok(ExchangePattern::InOnly),
            "InOut" | "inOut" | "in-out" => Ok(ExchangePattern::InOut),
            other => Err(CamelError::illegal_argument(format!("Unknown exchange pattern: {}", other))),
        }
    }
}

/// Well-known exchange property / header names (subset of Camel's `Exchange` constants).
pub mod props {
    pub const CORRELATION_ID: &str = "CamelCorrelationId";
    pub const SPLIT_INDEX: &str = "CamelSplitIndex";
    pub const SPLIT_SIZE: &str = "CamelSplitSize";
    pub const SPLIT_COMPLETE: &str = "CamelSplitComplete";
    pub const MULTICAST_INDEX: &str = "CamelMulticastIndex";
    pub const MULTICAST_COMPLETE: &str = "CamelMulticastComplete";
    pub const AGGREGATED_SIZE: &str = "CamelAggregatedSize";
    pub const AGGREGATED_COMPLETED_BY: &str = "CamelAggregatedCompletedBy";
    pub const AGGREGATED_CORRELATION_KEY: &str = "CamelAggregatedCorrelationKey";
    pub const LOOP_INDEX: &str = "CamelLoopIndex";
    pub const LOOP_SIZE: &str = "CamelLoopSize";
    pub const FILTER_MATCHED: &str = "CamelFilterMatched";
    pub const ROUTE_STOP: &str = "CamelRouteStop";
    pub const ERRORHANDLER_HANDLED: &str = "CamelErrorHandlerHandled";
    pub const EXCEPTION_CAUGHT: &str = "CamelExceptionCaught";
    pub const FAILURE_ENDPOINT: &str = "CamelFailureEndpoint";
    pub const FAILURE_ROUTE_ID: &str = "CamelFailureRouteId";
    pub const FATAL_FALLBACK_ERROR_HANDLER: &str = "CamelFatalFallbackErrorHandler";
    pub const REDELIVERED: &str = "CamelRedelivered";
    pub const REDELIVERY_COUNTER: &str = "CamelRedeliveryCounter";
    pub const REDELIVERY_MAX_COUNTER: &str = "CamelRedeliveryMaxCounter";
    pub const REDELIVERY_EXHAUSTED: &str = "CamelRedeliveryExhausted";
    pub const TO_ENDPOINT: &str = "CamelToEndpoint";
    pub const SLIP_ENDPOINT: &str = "CamelSlipEndpoint";
    pub const RECIPIENT_LIST_ENDPOINT: &str = "CamelRecipientListEndpoint";
    pub const TIMER_NAME: &str = "CamelTimerName";
    pub const TIMER_FIRED_TIME: &str = "CamelTimerFiredTime";
    pub const TIMER_COUNTER: &str = "CamelTimerCounter";
    pub const TIMER_PERIOD: &str = "CamelTimerPeriod";
    pub const CREATED_TIMESTAMP: &str = "CamelCreatedTimestamp";
    pub const BATCH_INDEX: &str = "CamelBatchIndex";
    pub const BATCH_SIZE: &str = "CamelBatchSize";
    pub const BATCH_COMPLETE: &str = "CamelBatchComplete";
    pub const DUPLICATE_MESSAGE: &str = "CamelDuplicateMessage";
    pub const FILE_NAME: &str = "CamelFileName";
    pub const FILE_NAME_ONLY: &str = "CamelFileNameOnly";
    pub const FILE_PATH: &str = "CamelFilePath";
    pub const FILE_LENGTH: &str = "CamelFileLength";
    pub const FILE_LAST_MODIFIED: &str = "CamelFileLastModified";
    pub const HTTP_RESPONSE_CODE: &str = "CamelHttpResponseCode";
    pub const HTTP_METHOD: &str = "CamelHttpMethod";
    pub const HTTP_URI: &str = "CamelHttpUri";
    pub const HTTP_PATH: &str = "CamelHttpPath";
    pub const HTTP_QUERY: &str = "CamelHttpQuery";
    pub const CONTENT_TYPE: &str = "Content-Type";
    pub const CHARSET_NAME: &str = "CamelCharsetName";
    pub const STEP_ID: &str = "CamelStepId";
    pub const ON_COMPLETION: &str = "CamelOnCompletion";
    pub const INTERCEPTED_ENDPOINT: &str = "CamelInterceptedEndpoint";
    pub const CIRCUIT_BREAKER_STATE: &str = "CamelCircuitBreakerState";
    pub const RESPONSE_SUCCESSFUL_EXECUTION: &str = "CamelCircuitBreakerSuccessfulExecution";
    pub const RESPONSE_FROM_FALLBACK: &str = "CamelCircuitBreakerResponseFromFallback";
    pub const RESPONSE_SHORT_CIRCUITED: &str = "CamelCircuitBreakerResponseShortCircuited";
    pub const RESPONSE_TIMED_OUT: &str = "CamelCircuitBreakerResponseTimedOut";
    pub const RESPONSE_REJECTED: &str = "CamelCircuitBreakerResponseRejected";
}

/// Callback executed when an exchange completes (Camel `Synchronization`).
pub type OnCompletionCallback = Box<dyn FnOnce(&Exchange) + Send>;

/// The Exchange: the container for a message flowing through a route.
pub struct Exchange {
    pub exchange_id: LazyId,
    pub pattern: ExchangePattern,
    pub message: Message,
    pub properties: Properties,
    pub exception: Option<CamelError>,
    pub context: Option<CamelContext>,
    /// The uri and id of the route the exchange was created in (shared, not copied).
    pub from_endpoint: Option<std::sync::Arc<str>>,
    pub from_route_id: Option<std::sync::Arc<str>>,
    pub rollback_only: bool,
    pub created: SystemTime,
    /// Synchronizations run when the unit of work completes. Not copied on clone.
    pub on_completions: Vec<OnCompletionCallback>,
    /// Set by an EIP that discards this exchange after its next step reads it (a split
    /// part whose only step is an aggregate): that step may move the message out instead
    /// of copying it. Not copied on clone.
    pub(crate) consumable: bool,
    /// Cooperative cancellation, shared by every copy of this exchange: once cancelled,
    /// [`Exchange::should_continue`] is false and pipelines stop before their next step
    /// (set by callers that give up waiting, such as an `ai-tool` call that timed out).
    pub(crate) cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// With a cancellation flag: past this instant the exchange counts as cancelled (set by
    /// [`Exchange::set_deadline`]), so a timeout needs no watchdog thread.
    pub(crate) deadline: Option<std::time::Instant>,
}

impl std::fmt::Debug for Exchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Exchange")
            .field("exchange_id", &self.exchange_id)
            .field("pattern", &self.pattern)
            .field("message", &self.message)
            .field("properties", &self.properties)
            .field("exception", &self.exception)
            .field("from_endpoint", &self.from_endpoint)
            .field("from_route_id", &self.from_route_id)
            .finish()
    }
}

impl Clone for Exchange {
    /// Copies everything except the on-completion synchronizations (same exchange id).
    fn clone(&self) -> Self {
        Exchange {
            exchange_id: self.exchange_id.clone(),
            pattern: self.pattern,
            message: self.message.clone(),
            properties: self.properties.clone(),
            exception: self.exception.clone(),
            context: self.context.clone(),
            from_endpoint: self.from_endpoint.clone(),
            from_route_id: self.from_route_id.clone(),
            rollback_only: self.rollback_only,
            created: self.created,
            on_completions: Vec::new(),
            consumable: false,
            cancel: self.cancel.clone(),
            deadline: self.deadline,
        }
    }
}

impl Default for Exchange {
    fn default() -> Self {
        Exchange::new(None)
    }
}

impl Exchange {
    pub fn new(context: Option<CamelContext>) -> Self {
        Exchange {
            exchange_id: LazyId::new(),
            pattern: ExchangePattern::InOnly,
            message: Message::new(Value::Null),
            properties: Properties::new(),
            exception: None,
            context,
            from_endpoint: None,
            from_route_id: None,
            rollback_only: false,
            created: crate::clock::now_system_time(),
            on_completions: Vec::new(),
            consumable: false,
            cancel: None,
            deadline: None,
        }
    }

    /// Create an exchange with the given body.
    pub fn with_body(context: Option<CamelContext>, body: impl Into<Value>) -> Self {
        let mut ex = Exchange::new(context);
        ex.message.body = body.into();
        ex
    }

    /// A copy with a new exchange id and the correlation id property set to this exchange's id.
    pub fn correlated_copy(&self) -> Exchange {
        let mut c = Exchange {
            exchange_id: LazyId::new(),
            pattern: self.pattern,
            message: Message { message_id: LazyId::new(), headers: self.message.headers.clone(), body: self.message.body.clone() },
            // room for the correlation id and the properties the EIPs add to copies
            properties: self.properties.clone_with_room(4),
            exception: self.exception.clone(),
            context: self.context.clone(),
            from_endpoint: self.from_endpoint.clone(),
            from_route_id: self.from_route_id.clone(),
            rollback_only: self.rollback_only,
            created: self.created,
            on_completions: Vec::new(),
            consumable: false,
            cancel: self.cancel.clone(),
            deadline: self.deadline,
        };
        c.properties.set(props::CORRELATION_ID, Value::from(&self.exchange_id));
        c
    }

    /// [`Exchange::correlated_copy`] that moves the message and properties out of this
    /// exchange instead of copying them (for an exchange that is discarded afterwards).
    pub(crate) fn take_correlated(&mut self) -> Exchange {
        let mut c = Exchange {
            exchange_id: LazyId::new(),
            pattern: self.pattern,
            message: Message {
                message_id: LazyId::new(),
                headers: std::mem::take(&mut self.message.headers),
                body: std::mem::take(&mut self.message.body),
            },
            properties: std::mem::take(&mut self.properties),
            exception: self.exception.clone(),
            context: self.context.clone(),
            from_endpoint: self.from_endpoint.clone(),
            from_route_id: self.from_route_id.clone(),
            rollback_only: self.rollback_only,
            created: self.created,
            on_completions: Vec::new(),
            consumable: false,
            cancel: self.cancel.clone(),
            deadline: self.deadline,
        };
        c.properties.set(props::CORRELATION_ID, Value::from(&self.exchange_id));
        c
    }

    // --- body / header convenience ---
    pub fn body(&self) -> &Value {
        &self.message.body
    }
    pub fn set_body(&mut self, body: impl Into<Value>) {
        self.message.body = body.into();
    }
    pub fn body_as<T: FromValue>(&self) -> Result<T> {
        self.message.body_as()
    }
    pub fn header(&self, key: &str) -> Option<&Value> {
        self.message.headers.get(key)
    }
    pub fn set_header(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.message.headers.set(key, value)
    }
    pub fn property(&self, key: &str) -> Option<&Value> {
        self.properties.get(key)
    }
    pub fn set_property(&mut self, key: impl Into<String> + AsRef<str>, value: impl Into<Value>) {
        self.properties.set(key, value.into());
    }
    pub fn remove_property(&mut self, key: &str) -> Option<Value> {
        self.properties.remove(key)
    }

    // --- failure state ---
    pub fn is_failed(&self) -> bool {
        self.exception.is_some()
    }
    pub fn set_exception(&mut self, e: CamelError) {
        self.exception = Some(e);
    }
    pub fn take_exception(&mut self) -> Option<CamelError> {
        self.exception.take()
    }
    /// True if the route has been told to stop routing this exchange (`stop()` EIP).
    pub fn is_route_stop(&self) -> bool {
        self.properties.get(props::ROUTE_STOP).map(|v| v.is_truthy()).unwrap_or(false)
    }
    pub fn set_route_stop(&mut self, stop: bool) {
        self.set_property(props::ROUTE_STOP, stop);
    }
    /// Whether routing should continue to the next processor in a pipeline.
    pub fn should_continue(&self) -> bool {
        !self.is_failed() && !self.is_route_stop() && !self.rollback_only && !self.is_cancelled()
    }
    /// Attach a cancellation flag: setting it to true stops routing this exchange and its
    /// copies before their next pipeline step. Steps already running are not interrupted.
    pub fn set_cancel_token(&mut self, token: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.cancel = Some(token);
    }
    /// The cancellation flag, if one is attached.
    pub fn cancel_token(&self) -> Option<&std::sync::Arc<std::sync::atomic::AtomicBool>> {
        self.cancel.as_ref()
    }
    /// Cancel the exchange (and its copies) once `deadline` has passed, checked before every
    /// step like the cancellation flag; attaches a flag if there is none.
    pub fn set_deadline(&mut self, deadline: std::time::Instant) {
        if self.cancel.is_none() {
            self.cancel = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        }
        self.deadline = Some(deadline);
    }
    /// True once the attached cancellation flag was set, or the deadline has passed.
    pub fn is_cancelled(&self) -> bool {
        if let (Some(d), Some(flag)) = (self.deadline, &self.cancel) {
            if !flag.load(std::sync::atomic::Ordering::Relaxed) && std::time::Instant::now() >= d {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        match &self.cancel {
            Some(c) => c.load(std::sync::atomic::Ordering::Relaxed),
            None => false,
        }
    }
    pub fn is_out_capable(&self) -> bool {
        self.pattern == ExchangePattern::InOut
    }

    /// Register a callback to run when this exchange's unit of work completes.
    pub fn add_on_completion(&mut self, f: impl FnOnce(&Exchange) + Send + 'static) {
        self.on_completions.push(Box::new(f));
    }

    /// Run and clear registered on-completion callbacks.
    pub fn run_on_completions(&mut self) {
        let cbs = std::mem::take(&mut self.on_completions);
        for cb in cbs {
            cb(self);
        }
    }

    /// The context, or an error if the exchange was created outside a context.
    pub fn context(&self) -> Result<&CamelContext> {
        self.context.as_ref().ok_or_else(|| CamelError::illegal_state("Exchange has no CamelContext"))
    }

    /// Created timestamp in epoch millis.
    pub fn created_millis(&self) -> i64 {
        self.created.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_lookups_are_case_insensitive_for_short_and_long_keys() {
        let mut h = Headers::new();
        h.set("Content-Type", "text/plain");
        let long = format!("X-{}", "Abc".repeat(30));
        h.set(long.as_str(), 1);
        assert_eq!(h.get("content-type"), Some(&Value::from("text/plain")));
        assert_eq!(h.get("CONTENT-TYPE"), Some(&Value::from("text/plain")));
        assert!(h.contains("Content-type"));
        assert_eq!(h.get(&long.to_ascii_uppercase()), Some(&Value::from(1)));
        assert_eq!(h.get(&long.to_ascii_lowercase()), Some(&Value::from(1)));
        *h.get_mut("CONTENT-type").unwrap() = Value::from("application/json");
        assert_eq!(h.keys().collect::<Vec<_>>(), vec!["Content-Type", long.as_str()]);
        assert_eq!(h.remove("content-TYPE"), Some(Value::from("application/json")));
        assert_eq!(h.remove(&long.to_ascii_uppercase()), Some(Value::from(1)));
        assert!(h.is_empty());
        // non-ASCII keys only fold ASCII letters, like before
        h.set("Ünïcode-Key", 2);
        assert_eq!(h.get("Ünïcode-KEY"), Some(&Value::from(2)));
        assert_eq!(h.get("ünïcode-key"), None);
    }

    #[test]
    fn generated_ids_are_unique_and_prefixed() {
        let a = generate_id();
        let b = generate_id();
        assert_ne!(a, b);
        let prefix = id_prefix();
        for id in [&a, &b] {
            let n = id.strip_prefix(prefix).and_then(|r| r.strip_prefix('-')).unwrap();
            assert!(n.parse::<u64>().is_ok(), "{id}");
        }
        let na: u64 = a.rsplit('-').next().unwrap().parse().unwrap();
        let nb: u64 = b.rsplit('-').next().unwrap().parse().unwrap();
        assert!(nb > na);
    }
}
