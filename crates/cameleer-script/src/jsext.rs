//! JavaScript built-ins beyond the core object model: `BigInt`, `Proxy` / `Reflect`,
//! `ArrayBuffer` / typed arrays / `DataView`, `TextEncoder` / `TextDecoder`, `Intl`
//! (`NumberFormat`, `DateTimeFormat`, `PluralRules`, `Collator` for en-US, en-GB, de-DE,
//! fr-FR), `globalThis`, `structuredClone`, tagged template strings and ES modules.

use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{BinOp, Dialect, UnOp};
use crate::bigint::BigInt;
use crate::interp::{Ctrl, Interp, R};
use crate::value::*;

/// Namespaces / constructors added by this module.
pub const NS: &[&str] = &[
    "Proxy", "BigInt", "%BigInt", "ArrayBuffer", "DataView", "Int8Array", "Uint8Array", "Uint8ClampedArray", "Int16Array", "Uint16Array", "Int32Array", "Uint32Array",
    "Float32Array", "Float64Array", "BigInt64Array", "BigUint64Array", "TextEncoder", "TextDecoder", "Intl", "Intl.NumberFormat", "Intl.DateTimeFormat",
    "Intl.PluralRules", "Intl.Collator", "Intl.ListFormat", "Intl.RelativeTimeFormat", "globalThis", "structuredClone", "%tpl", "%import", "%dynimport",
];

fn ns_name(n: &str) -> Option<&'static str> {
    NS.iter().find(|x| **x == n).copied()
}

/// Install the JavaScript globals of this module.
pub fn install(it: &mut Interp) {
    for n in NS {
        if !n.starts_with("Intl.") {
            it.globals.define(n, V::host(Host::Ns(n)));
        }
    }
    let exports = V::new_obj();
    it.globals.define("%exports", exports);
    it.globals.define("%asyncIter", V::Method(Rc::new((V::Null, "%async_iter".to_string()))));
}

// ====================================================================== typed arrays

/// Element type of a typed array (or a `DataView`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TK {
    I8,
    U8,
    U8C,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
    BI64,
    BU64,
    View,
}

impl TK {
    fn of(name: &str) -> Option<TK> {
        Some(match name {
            "Int8Array" => TK::I8,
            "Uint8Array" => TK::U8,
            "Uint8ClampedArray" => TK::U8C,
            "Int16Array" => TK::I16,
            "Uint16Array" => TK::U16,
            "Int32Array" => TK::I32,
            "Uint32Array" => TK::U32,
            "Float32Array" => TK::F32,
            "Float64Array" => TK::F64,
            "BigInt64Array" => TK::BI64,
            "BigUint64Array" => TK::BU64,
            _ => return None,
        })
    }
    fn name(self) -> &'static str {
        match self {
            TK::I8 => "Int8Array",
            TK::U8 => "Uint8Array",
            TK::U8C => "Uint8ClampedArray",
            TK::I16 => "Int16Array",
            TK::U16 => "Uint16Array",
            TK::I32 => "Int32Array",
            TK::U32 => "Uint32Array",
            TK::F32 => "Float32Array",
            TK::F64 => "Float64Array",
            TK::BI64 => "BigInt64Array",
            TK::BU64 => "BigUint64Array",
            TK::View => "DataView",
        }
    }
    pub fn size(self) -> usize {
        match self {
            TK::I8 | TK::U8 | TK::U8C | TK::View => 1,
            TK::I16 | TK::U16 => 2,
            TK::I32 | TK::U32 | TK::F32 => 4,
            TK::F64 | TK::BI64 | TK::BU64 => 8,
        }
    }
}

/// A typed array / DataView: a window on a shared `ArrayBuffer`.
#[derive(Clone, Debug, PartialEq)]
pub struct Typed {
    pub kind: TK,
    pub buf: Internal<Vec<u8>>,
    pub off: usize,
    /// Length in elements (bytes for a DataView).
    pub len: usize,
}

fn read_elem(kind: TK, b: &[u8], little: bool) -> V {
    macro_rules! rd {
        ($t:ty, $n:expr) => {{
            let mut a = [0u8; $n];
            a.copy_from_slice(&b[..$n]);
            if little {
                <$t>::from_le_bytes(a)
            } else {
                <$t>::from_be_bytes(a)
            }
        }};
    }
    match kind {
        TK::I8 => V::Int(b[0] as i8 as i64),
        TK::U8 | TK::U8C | TK::View => V::Int(b[0] as i64),
        TK::I16 => V::Int(rd!(i16, 2) as i64),
        TK::U16 => V::Int(rd!(u16, 2) as i64),
        TK::I32 => V::Int(rd!(i32, 4) as i64),
        TK::U32 => V::Int(rd!(u32, 4) as i64),
        TK::F32 => crate::interp::norm_num(rd!(f32, 4) as f64),
        TK::F64 => crate::interp::norm_num(rd!(f64, 8)),
        TK::BI64 => make_bigint(BigInt::from_i64(rd!(i64, 8))),
        TK::BU64 => make_bigint(BigInt::from_u128(rd!(u64, 8) as u128, false)),
    }
}

fn to_int_bits(f: f64) -> i64 {
    if !f.is_finite() {
        return 0;
    }
    let t = f.trunc();
    (t.rem_euclid(18446744073709551616.0)) as u64 as i64
}

fn write_elem(kind: TK, b: &mut [u8], v: &V, num: f64, little: bool) {
    macro_rules! wr {
        ($x:expr) => {{
            let bytes = if little { $x.to_le_bytes() } else { $x.to_be_bytes() };
            b[..bytes.len()].copy_from_slice(&bytes);
        }};
    }
    match kind {
        TK::I8 | TK::U8 | TK::View => b[0] = to_int_bits(num) as u8,
        TK::U8C => b[0] = if num.is_nan() { 0 } else { num.clamp(0.0, 255.0).round_ties_even() as u8 },
        TK::I16 | TK::U16 => wr!(to_int_bits(num) as u16),
        TK::I32 | TK::U32 => wr!(to_int_bits(num) as u32),
        TK::F32 => wr!(num as f32),
        TK::F64 => wr!(num),
        TK::BI64 | TK::BU64 => {
            let x = bigint_of(v).map(|b| b.low_u64()).unwrap_or(num as i64 as u64);
            wr!(x)
        }
    }
}

fn typed_of(v: &V) -> Option<Typed> {
    match v {
        V::Obj(o) => match &o.borrow().kind {
            ObjKind::Typed(t) => Some(t.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn buffer_of(v: &V) -> Option<Internal<Vec<u8>>> {
    match v {
        V::Obj(o) => match &o.borrow().kind {
            ObjKind::Buffer(b) => Some(b.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn make_buffer(b: Internal<Vec<u8>>) -> V {
    V::obj_kind(PropMap::default(), ObjKind::Buffer(b))
}

fn make_typed(t: Typed) -> V {
    V::obj_kind(PropMap::default(), ObjKind::Typed(t))
}

impl Typed {
    fn get(&self, i: usize) -> V {
        let b = self.buf.0.borrow();
        let s = self.off + i * self.kind.size();
        if s + self.kind.size() > b.len() {
            return V::Undef;
        }
        read_elem(self.kind, &b[s..], true)
    }
    fn set(&self, it: &Interp, i: usize, v: &V) {
        let num = if bigint_of(v).is_some() { 0.0 } else { num_of(it, v) };
        let mut b = self.buf.0.borrow_mut();
        let s = self.off + i * self.kind.size();
        if s + self.kind.size() <= b.len() {
            write_elem(self.kind, &mut b[s..], v, num, true);
        }
    }
    fn items(&self) -> Vec<V> {
        (0..self.len).map(|i| self.get(i)).collect()
    }
    fn bytes(&self) -> Vec<u8> {
        let b = self.buf.0.borrow();
        b[self.off..(self.off + self.len * self.kind.size()).min(b.len())].to_vec()
    }
}

fn num_of(it: &Interp, v: &V) -> f64 {
    match it.to_number(v) {
        V::Int(i) => i as f64,
        V::Num(f) => f,
        _ => f64::NAN,
    }
}

/// A new typed array of `kind` holding `items`.
fn typed_from(it: &Interp, kind: TK, items: &[V]) -> V {
    let t = Typed { kind, buf: Internal::new(vec![0u8; items.len() * kind.size()]), off: 0, len: items.len() };
    for (i, x) in items.iter().enumerate() {
        t.set(it, i, x);
    }
    make_typed(t)
}

/// `Uint8Array` over a copy of `bytes`.
pub fn uint8_array(bytes: Vec<u8>) -> V {
    let len = bytes.len();
    make_typed(Typed { kind: TK::U8, buf: Internal::new(bytes), off: 0, len })
}

fn construct_typed(it: &mut Interp, kind: TK, args: &[V], pos: u32) -> R<V> {
    let a0 = args.first().cloned().unwrap_or(V::Undef);
    if let Some(b) = buffer_of(&a0) {
        let total = b.0.borrow().len();
        let off = args.get(1).map(|v| num_of(it, v) as usize).unwrap_or(0);
        if off % kind.size() != 0 {
            return it.throw("RangeError", format!("start offset of {} should be a multiple of {}", kind.name(), kind.size()), Some(pos));
        }
        let len = match args.get(2) {
            Some(v) if !v.is_nullish() => num_of(it, v) as usize,
            _ => {
                if (total - off.min(total)) % kind.size() != 0 {
                    return it.throw("RangeError", format!("byte length of {} should be a multiple of {}", kind.name(), kind.size()), Some(pos));
                }
                (total - off.min(total)) / kind.size()
            }
        };
        if off + len * kind.size() > total {
            return it.throw("RangeError", format!("Invalid typed array length: {}", len), Some(pos));
        }
        return Ok(make_typed(Typed { kind, buf: b, off, len }));
    }
    if a0.is_number() || a0.is_nullish() {
        let n = if a0.is_nullish() { 0.0 } else { num_of(it, &a0) };
        if n < 0.0 || n.fract() != 0.0 {
            return it.throw("RangeError", format!("Invalid typed array length: {}", it.display(&a0)), Some(pos));
        }
        let n = n as usize;
        return Ok(make_typed(Typed { kind, buf: Internal::new(vec![0u8; n * kind.size()]), off: 0, len: n }));
    }
    let items = match &a0 {
        V::Bytes(b) => b.iter().map(|x| V::Int(*x as i64)).collect(),
        _ => it.iterate(&a0, Some(pos))?,
    };
    if matches!(kind, TK::BI64 | TK::BU64) && items.iter().any(|x| bigint_of(x).is_none()) {
        return it.type_error("Cannot convert a Number to a BigInt".into(), Some(pos));
    }
    Ok(typed_from(it, kind, &items))
}

fn construct_dataview(it: &mut Interp, args: &[V], pos: u32) -> R<V> {
    let a0 = args.first().cloned().unwrap_or(V::Undef);
    let Some(b) = buffer_of(&a0) else {
        return it.type_error("First argument to DataView constructor must be an ArrayBuffer".into(), Some(pos));
    };
    let total = b.0.borrow().len();
    let off = args.get(1).map(|v| num_of(it, v) as usize).unwrap_or(0);
    let len = match args.get(2) {
        Some(v) if !v.is_nullish() => num_of(it, v) as usize,
        _ => total.saturating_sub(off),
    };
    if off + len > total {
        return it.throw("RangeError", format!("Invalid DataView length {}", len), Some(pos));
    }
    Ok(make_typed(Typed { kind: TK::View, buf: b, off, len }))
}

fn dataview_method(it: &mut Interp, t: &Typed, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
    let (get, ty) = if let Some(ty) = name.strip_prefix("get") {
        (true, ty)
    } else if let Some(ty) = name.strip_prefix("set") {
        (false, ty)
    } else {
        return Ok(None);
    };
    let kind = match ty {
        "Int8" => TK::I8,
        "Uint8" => TK::U8,
        "Int16" => TK::I16,
        "Uint16" => TK::U16,
        "Int32" => TK::I32,
        "Uint32" => TK::U32,
        "Float32" => TK::F32,
        "Float64" => TK::F64,
        "BigInt64" => TK::BI64,
        "BigUint64" => TK::BU64,
        _ => return Ok(None),
    };
    let idx = args.first().map(|v| num_of(it, v)).unwrap_or(0.0);
    if idx < 0.0 || idx as usize + kind.size() > t.len {
        return it.throw("RangeError", "Offset is outside the bounds of the DataView".into(), Some(pos));
    }
    let s = t.off + idx as usize;
    if get {
        let little = args.get(1).is_some_and(|v| it.truthy(v));
        let b = t.buf.0.borrow();
        return Ok(Some(read_elem(kind, &b[s..], little)));
    }
    let v = args.get(1).cloned().unwrap_or(V::Undef);
    let little = args.get(2).is_some_and(|v| it.truthy(v));
    let num = if bigint_of(&v).is_some() { 0.0 } else { num_of(it, &v) };
    write_elem(kind, &mut t.buf.0.borrow_mut()[s..], &v, num, little);
    Ok(Some(V::Undef))
}

fn typed_method(it: &mut Interp, t: &Typed, recv: &V, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
    if t.kind == TK::View {
        return dataview_method(it, t, name, args, pos);
    }
    let a0 = args.first().cloned().unwrap_or(V::Undef);
    let rel = |it: &Interp, v: Option<&V>, def: usize| -> usize {
        match v {
            Some(x) if !x.is_nullish() => {
                let f = num_of(it, x);
                let f = if f.is_nan() { 0.0 } else { f.trunc() };
                if f < 0.0 {
                    (t.len as f64 + f).max(0.0) as usize
                } else {
                    (f as usize).min(t.len)
                }
            }
            _ => def,
        }
    };
    Ok(Some(match name {
        "set" => {
            let items = match typed_of(&a0) {
                Some(src) => src.items(),
                None => it.iterate(&a0, Some(pos))?,
            };
            let off = args.get(1).map(|v| num_of(it, v) as usize).unwrap_or(0);
            if off + items.len() > t.len {
                return it.throw("RangeError", "offset is out of bounds".into(), Some(pos));
            }
            for (i, x) in items.iter().enumerate() {
                t.set(it, off + i, x);
            }
            V::Undef
        }
        "subarray" => {
            let s = rel(it, args.first(), 0);
            let e = rel(it, args.get(1), t.len).max(s);
            make_typed(Typed { kind: t.kind, buf: t.buf.clone(), off: t.off + s * t.kind.size(), len: e - s })
        }
        "slice" => {
            let s = rel(it, args.first(), 0);
            let e = rel(it, args.get(1), t.len).max(s);
            let items: Vec<V> = (s..e).map(|i| t.get(i)).collect();
            typed_from(it, t.kind, &items)
        }
        "fill" => {
            let s = rel(it, args.get(1), 0);
            let e = rel(it, args.get(2), t.len);
            for i in s..e {
                t.set(it, i, &a0);
            }
            recv.clone()
        }
        "map" => {
            let mut out = vec![];
            for (i, x) in t.items().into_iter().enumerate() {
                out.push(it.call(&a0, V::Undef, vec![x, V::Int(i as i64), recv.clone()], pos)?);
            }
            typed_from(it, t.kind, &out)
        }
        "filter" => {
            let mut out = vec![];
            for (i, x) in t.items().into_iter().enumerate() {
                let keep = it.call(&a0, V::Undef, vec![x.clone(), V::Int(i as i64), recv.clone()], pos)?;
                if it.truthy(&keep) {
                    out.push(x);
                }
            }
            typed_from(it, t.kind, &out)
        }
        "sort" => {
            let mut items = t.items();
            if a0.is_callable() {
                items = crate::builtins::sort_vals(it, items, &mut |it, a, b| {
                    let r = it.call(&a0, V::Undef, vec![a.clone(), b.clone()], pos)?;
                    let f = num_of(it, &r);
                    Ok(f.partial_cmp(&0.0).unwrap_or(std::cmp::Ordering::Equal))
                })?;
            } else {
                items.sort_by(|a, b| match (bigint_of(a), bigint_of(b)) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    _ => a.as_f64().unwrap_or(0.0).partial_cmp(&b.as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal),
                });
            }
            for (i, x) in items.iter().enumerate() {
                t.set(it, i, x);
            }
            recv.clone()
        }
        "reverse" => {
            let items = t.items();
            for (i, x) in items.iter().rev().enumerate() {
                t.set(it, i, x);
            }
            recv.clone()
        }
        "toString" => V::string(t.items().iter().map(|x| it.display(x)).collect::<Vec<_>>().join(",")),
        "toReversed" => {
            let mut items = t.items();
            items.reverse();
            typed_from(it, t.kind, &items)
        }
        "toSorted" => {
            let c = typed_from(it, t.kind, &t.items());
            let ct = typed_of(&c).unwrap();
            typed_method(it, &ct, &c, "sort", args, pos)?;
            c
        }
        "with" => {
            let c = typed_from(it, t.kind, &t.items());
            let ct = typed_of(&c).unwrap();
            let i = num_of(it, &a0) as i64;
            let i = if i < 0 { i + t.len as i64 } else { i };
            if i < 0 || i as usize >= t.len {
                return it.throw("RangeError", "Invalid typed array index".into(), Some(pos));
            }
            ct.set(it, i as usize, &args.get(1).cloned().unwrap_or(V::Undef));
            c
        }
        "copyWithin" => {
            let target = rel(it, args.first(), 0);
            let s = rel(it, args.get(1), 0);
            let e = rel(it, args.get(2), t.len);
            let items: Vec<V> = (s..e).map(|i| t.get(i)).collect();
            for (k, x) in items.iter().enumerate() {
                if target + k < t.len {
                    t.set(it, target + k, x);
                }
            }
            recv.clone()
        }
        _ => {
            // read-only array methods on a snapshot of the elements
            let arr = V::arr(t.items());
            return match crate::builtins::list_method(it, &arr, name, args.to_vec(), pos) {
                Ok(v) => Ok(Some(v)),
                Err(e) => Err(e),
            };
        }
    }))
}

fn typed_property(t: &Typed, name: &str) -> Option<V> {
    Some(match name {
        "length" if t.kind != TK::View => V::Int(t.len as i64),
        "byteLength" => V::Int((t.len * t.kind.size()) as i64),
        "byteOffset" => V::Int(t.off as i64),
        "buffer" => make_buffer(t.buf.clone()),
        "BYTES_PER_ELEMENT" if t.kind != TK::View => V::Int(t.kind.size() as i64),
        _ => {
            if t.kind != TK::View {
                if let Ok(i) = name.parse::<usize>() {
                    return Some(if i < t.len { t.get(i) } else { V::Undef });
                }
            }
            return None;
        }
    })
}

// ====================================================================== BigInt

/// A BigInt value.
pub fn make_bigint(b: BigInt) -> V {
    V::obj_kind(PropMap::default(), ObjKind::BigInt(Rc::new(b)))
}

/// The BigInt held by a value.
pub fn bigint_of(v: &V) -> Option<Rc<BigInt>> {
    match v {
        V::Obj(o) => match &o.borrow().kind {
            ObjKind::BigInt(b) => Some(b.clone()),
            _ => None,
        },
        _ => None,
    }
}

pub fn is_bigint(v: &V) -> bool {
    bigint_of(v).is_some()
}

fn to_bigint(it: &mut Interp, v: &V, pos: u32) -> R<BigInt> {
    if let Some(b) = bigint_of(v) {
        return Ok((*b).clone());
    }
    match v {
        V::Int(i) => Ok(BigInt::from_i64(*i)),
        V::Num(f) => match BigInt::from_f64(*f) {
            Some(b) => Ok(b),
            None => it.throw("RangeError", format!("The number {} cannot be converted to a BigInt because it is not an integer", it.display(v)), Some(pos)),
        },
        V::Bool(b) => Ok(BigInt::from_i64(*b as i64)),
        V::Str(s) => match BigInt::parse(s) {
            Some(b) => Ok(b),
            None => it.throw("SyntaxError", format!("Cannot convert {} to a BigInt", s), Some(pos)),
        },
        _ => it.type_error(format!("Cannot convert {} to a BigInt", it.display(v)), Some(pos)),
    }
}

/// Binary operators with a BigInt operand.
pub fn bigint_binary(it: &mut Interp, op: BinOp, a: &V, b: &V, pos: u32) -> R<Option<V>> {
    use BinOp::*;
    let (x, y) = (bigint_of(a), bigint_of(b));
    match op {
        Eq | Ne | StrictEq | StrictNe => {
            let eq = match (&x, &y) {
                (Some(p), Some(q)) => p == q,
                (Some(p), None) | (None, Some(p)) if !matches!(op, StrictEq | StrictNe) => {
                    let other = if x.is_some() { b } else { a };
                    match other {
                        V::Int(i) => **p == BigInt::from_i64(*i),
                        V::Num(f) => BigInt::from_f64(*f).is_some_and(|q| **p == q),
                        V::Str(s) => BigInt::parse(s).is_some_and(|q| **p == q),
                        V::Bool(v) => **p == BigInt::from_i64(*v as i64),
                        _ => false,
                    }
                }
                _ => false,
            };
            return Ok(Some(V::Bool(if matches!(op, Eq | StrictEq) { eq } else { !eq })));
        }
        Lt | Le | Gt | Ge => {
            let ord = match (&x, &y) {
                (Some(p), Some(q)) => Some(p.cmp(q)),
                (Some(p), None) => {
                    let f = num_of(it, b);
                    p.to_f64().partial_cmp(&f)
                }
                (None, Some(q)) => {
                    let f = num_of(it, a);
                    f.partial_cmp(&q.to_f64())
                }
                _ => None,
            };
            let r = match ord {
                None => false,
                Some(o) => match op {
                    Lt => o.is_lt(),
                    Le => o.is_le(),
                    Gt => o.is_gt(),
                    _ => o.is_ge(),
                },
            };
            return Ok(Some(V::Bool(r)));
        }
        Add if matches!(a, V::Str(_)) || matches!(b, V::Str(_)) => {
            return Ok(Some(V::string(format!("{}{}", display_js(it, a), display_js(it, b)))));
        }
        In | NotIn | InstanceOf | NotInstanceOf => return Ok(None),
        _ => {}
    }
    let (Some(x), Some(y)) = (x, y) else {
        return it.type_error("Cannot mix BigInt and other types, use explicit conversions".into(), Some(pos));
    };
    let r = match op {
        Add => x.add(&y),
        Sub => x.sub(&y),
        Mul => x.mul(&y),
        Div | Mod => match x.divrem(&y) {
            Some((q, r)) => {
                if op == Div {
                    q
                } else {
                    r
                }
            }
            None => return it.throw("RangeError", "Division by zero".into(), Some(pos)),
        },
        Pow => {
            if y.neg {
                return it.throw("RangeError", "Exponent must be non-negative".into(), Some(pos));
            }
            x.pow(y.to_i64().unwrap_or(0) as u64)
        }
        BitAnd => x.bitop(&y, '&'),
        BitOr => x.bitop(&y, '|'),
        BitXor => x.bitop(&y, '^'),
        Shl => x.shl(y.to_i64().unwrap_or(0)),
        Shr => x.shr(y.to_i64().unwrap_or(0)),
        UShr => return it.type_error("BigInts have no unsigned right shift, use >> instead".into(), Some(pos)),
        _ => return Ok(None),
    };
    Ok(Some(make_bigint(r)))
}

/// Unary operators on a BigInt.
pub fn bigint_unary(it: &mut Interp, op: UnOp, b: &BigInt, pos: u32) -> R<Option<V>> {
    Ok(Some(match op {
        UnOp::Neg => make_bigint(b.neg()),
        UnOp::BitNot => make_bigint(b.not()),
        UnOp::Plus => return it.type_error("Cannot convert a BigInt value to a number".into(), Some(pos)),
        _ => return Ok(None),
    }))
}

fn display_js(it: &Interp, v: &V) -> String {
    match bigint_of(v) {
        Some(b) => b.to_string(),
        None => it.display(v),
    }
}

fn bigint_method(it: &mut Interp, b: &BigInt, name: &str, args: &[V]) -> R<Option<V>> {
    Ok(Some(match name {
        "toString" => {
            let radix = args.first().filter(|v| !v.is_nullish()).map(|v| num_of(it, v) as u32).unwrap_or(10);
            V::string(b.to_string_radix(radix.clamp(2, 36)))
        }
        "toLocaleString" => {
            let loc = locale_of(it, args.first());
            V::string(group_digits(&b.abs().to_string(), &loc, true, b.neg))
        }
        "valueOf" => make_bigint(b.clone()),
        _ => return Ok(None),
    }))
}

// ====================================================================== Proxy

fn proxy_of(v: &V) -> Option<(V, V)> {
    match v {
        V::Obj(o) => match &o.borrow().kind {
            ObjKind::Proxy(p) => Some(p.0.borrow().clone()),
            _ => None,
        },
        _ => None,
    }
}

pub fn is_proxy(v: &V) -> bool {
    proxy_of(v).is_some()
}

fn trap(it: &mut Interp, handler: &V, name: &str) -> R<Option<V>> {
    if handler.is_nullish() {
        return Ok(None);
    }
    let f = it.get_member(handler, name, 0)?;
    Ok(if f.is_callable() { Some(f) } else { None })
}

fn key_value(k: &str) -> V {
    V::str(k)
}

/// Proxy `get`.
pub fn proxy_get(it: &mut Interp, p: &V, key: &V, pos: u32) -> R<V> {
    let (target, handler) = proxy_of(p).unwrap();
    if let Some(f) = trap(it, &handler, "get")? {
        return it.call(&f, handler, vec![target, key.clone(), p.clone()], pos);
    }
    it.get_index(&target, key, pos)
}

/// Proxy `set`.
pub fn proxy_set(it: &mut Interp, p: &V, key: &V, v: V, pos: u32) -> R<()> {
    let (target, handler) = proxy_of(p).unwrap();
    if let Some(f) = trap(it, &handler, "set")? {
        let ok = it.call(&f, handler, vec![target, key.clone(), v, p.clone()], pos)?;
        if !it.truthy(&ok) {
            let k = it.key_string(key);
            return it.type_error(format!("'set' on proxy: trap returned falsish for property '{}'", k), Some(pos));
        }
        return Ok(());
    }
    it.set_index(&target, key, v, pos)
}

/// Proxy `has` (`key in proxy`).
pub fn proxy_has(it: &mut Interp, p: &V, key: &V, pos: u32) -> R<bool> {
    let (target, handler) = proxy_of(p).unwrap();
    if let Some(f) = trap(it, &handler, "has")? {
        let r = it.call(&f, handler, vec![target, key.clone()], pos)?;
        return Ok(it.truthy(&r));
    }
    it.contains(&target, key, pos)
}

/// Proxy `deleteProperty`.
pub fn proxy_delete(it: &mut Interp, p: &V, key: &str, pos: u32) -> R<bool> {
    let (target, handler) = proxy_of(p).unwrap();
    if let Some(f) = trap(it, &handler, "deleteProperty")? {
        let r = it.call(&f, handler, vec![target, key_value(key)], pos)?;
        return Ok(it.truthy(&r));
    }
    if let V::Obj(o) = &target {
        if let Some(_inner) = proxy_of(&target) {
            return proxy_delete(it, &target, key, pos);
        }
        o.borrow_mut().props.remove(key);
    }
    Ok(true)
}

/// Proxy `ownKeys`.
pub fn proxy_keys(it: &mut Interp, p: &V, pos: u32) -> R<Vec<V>> {
    let (target, handler) = proxy_of(p).unwrap();
    if let Some(f) = trap(it, &handler, "ownKeys")? {
        let r = it.call(&f, handler, vec![target], pos)?;
        return it.iterate(&r, Some(pos));
    }
    own_keys(it, &target, pos)
}

fn own_keys(it: &mut Interp, v: &V, pos: u32) -> R<Vec<V>> {
    if is_proxy(v) {
        return proxy_keys(it, v, pos);
    }
    if let Some(t) = typed_of(v) {
        if t.kind != TK::View {
            return Ok((0..t.len).map(|i| V::string(i.to_string())).collect());
        }
        return Ok(vec![]);
    }
    Ok(match v {
        V::Obj(o) => o.borrow().props.keys().map(|k| V::Str(k.clone())).collect(),
        V::Arr(a) => (0..a.borrow().len()).map(|i| V::string(i.to_string())).collect(),
        _ => vec![],
    })
}

/// Proxy `apply`.
pub fn proxy_call(it: &mut Interp, p: &V, this: V, args: Vec<V>, pos: u32) -> R<V> {
    let (target, handler) = proxy_of(p).unwrap();
    if let Some(f) = trap(it, &handler, "apply")? {
        return it.call(&f, handler, vec![target, this, V::arr(args)], pos);
    }
    it.call(&target, this, args, pos)
}

/// Proxy `construct`.
pub fn proxy_construct(it: &mut Interp, p: &V, args: Vec<V>, pos: u32) -> R<V> {
    let (target, handler) = proxy_of(p).unwrap();
    if let Some(f) = trap(it, &handler, "construct")? {
        let r = it.call(&f, handler, vec![target, V::arr(args), p.clone()], pos)?;
        if !matches!(r, V::Obj(_) | V::Arr(_) | V::Func(_)) {
            return it.type_error("proxy [[Construct]] must return an object".into(), Some(pos));
        }
        return Ok(r);
    }
    it.construct_pub(&target, args, pos)
}

/// Whether a proxy wraps a callable target.
pub fn proxy_callable(v: &V) -> bool {
    proxy_of(v).is_some_and(|(t, _)| t.is_callable() || is_proxy(&t) && proxy_callable(&t))
}

// ====================================================================== hooks used by the interpreter

/// Whether `o` is one of the special object kinds handled here.
pub fn is_ext(o: &ObjRef) -> bool {
    matches!(o.borrow().kind, ObjKind::BigInt(_) | ObjKind::Proxy(_) | ObjKind::Buffer(_) | ObjKind::Typed(_))
}

/// Property read on a special object.
pub fn get_member(it: &mut Interp, o: &V, name: &str, pos: u32) -> R<Option<V>> {
    if is_proxy(o) {
        return proxy_get(it, o, &V::str(name), pos).map(Some);
    }
    if let Some(t) = typed_of(o) {
        if let Some(v) = typed_property(&t, name) {
            return Ok(Some(v));
        }
        return Ok(Some(V::Method(Rc::new((o.clone(), name.to_string())))));
    }
    if let Some(b) = buffer_of(o) {
        return Ok(Some(match name {
            "byteLength" => V::Int(b.0.borrow().len() as i64),
            _ => V::Method(Rc::new((o.clone(), name.to_string()))),
        }));
    }
    if is_bigint(o) {
        return Ok(Some(V::Method(Rc::new((o.clone(), name.to_string())))));
    }
    Ok(None)
}

/// Index read on a special object.
pub fn get_index(it: &mut Interp, o: &V, k: &V, pos: u32) -> R<Option<V>> {
    if is_proxy(o) {
        return proxy_get(it, o, k, pos).map(Some);
    }
    if let Some(t) = typed_of(o) {
        if t.kind != TK::View {
            if let Some(f) = k.as_f64() {
                return Ok(Some(if f >= 0.0 && f.fract() == 0.0 && (f as usize) < t.len { t.get(f as usize) } else { V::Undef }));
            }
        }
        let key = it.key_string(k);
        return get_member(it, o, &key, pos);
    }
    Ok(None)
}

/// Property / index write on a special object (true when handled).
pub fn set_index(it: &mut Interp, o: &V, k: &V, v: V, pos: u32) -> R<bool> {
    if is_proxy(o) {
        proxy_set(it, o, k, v, pos)?;
        return Ok(true);
    }
    if let Some(t) = typed_of(o) {
        let idx = match k {
            V::Int(i) => Some(*i as f64),
            V::Num(f) => Some(*f),
            V::Str(s) => s.parse::<f64>().ok(),
            _ => None,
        };
        if let Some(f) = idx {
            if t.kind != TK::View && f >= 0.0 && f.fract() == 0.0 && (f as usize) < t.len {
                t.set(it, f as usize, &v);
            }
            return Ok(true);
        }
    }
    Ok(false)
}

/// Method call on a special object.
pub fn call_method(it: &mut Interp, recv: &V, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
    if let Some(t) = typed_of(recv) {
        return typed_method(it, &t, recv, name, args, pos);
    }
    if let Some(b) = buffer_of(recv) {
        return Ok(match name {
            "slice" => {
                let data = b.0.borrow().clone();
                let len = data.len() as f64;
                let r = |v: Option<&V>, def: f64| -> usize {
                    let f = v.filter(|x| !x.is_nullish()).and_then(|x| x.as_f64()).unwrap_or(def);
                    (if f < 0.0 { (len + f).max(0.0) } else { f.min(len) }) as usize
                };
                let s = r(args.first(), 0.0);
                let e = r(args.get(1), len).max(s);
                Some(make_buffer(Internal::new(data[s..e].to_vec())))
            }
            "toString" => Some(V::str("[object ArrayBuffer]")),
            _ => None,
        });
    }
    if let Some(b) = bigint_of(recv) {
        return bigint_method(it, &b, name, args);
    }
    Ok(None)
}

/// Elements produced by iterating a special object.
pub fn iterate(it: &mut Interp, v: &V, pos: Option<u32>) -> R<Option<Vec<V>>> {
    if let Some(t) = typed_of(v) {
        return Ok(Some(t.items()));
    }
    if is_proxy(v) {
        let (target, _) = proxy_of(v).unwrap();
        return it.iterate(&target, pos).map(Some);
    }
    Ok(None)
}

/// Keys enumerated by `for...in` / `Object.keys`.
pub fn keys(it: &mut Interp, v: &V) -> R<Option<Vec<V>>> {
    if is_proxy(v) || typed_of(v).is_some() {
        return own_keys(it, v, 0).map(Some);
    }
    Ok(None)
}

/// `typeof` of a special object.
pub fn typeof_of(v: &V) -> Option<&'static str> {
    if is_bigint(v) {
        return Some("bigint");
    }
    if proxy_callable(v) {
        return Some("function");
    }
    None
}

/// String conversion of a special object.
pub fn display(it: &Interp, o: &Obj) -> Option<String> {
    match &o.kind {
        ObjKind::BigInt(b) => Some(b.to_string()),
        ObjKind::Typed(t) if t.kind == TK::View => Some("[object DataView]".into()),
        ObjKind::Typed(t) => Some(t.items().iter().map(|x| display_js(it, x)).collect::<Vec<_>>().join(",")),
        ObjKind::Buffer(_) => Some("[object ArrayBuffer]".into()),
        ObjKind::Proxy(p) => {
            let t = p.0.borrow().0.clone();
            Some(it.display(&t))
        }
        _ => None,
    }
}

/// Conversion of a special object to a Camel value.
pub fn to_value(o: &Obj, conv: &dyn Fn(&V) -> cameleer_core::value::Value) -> Option<cameleer_core::value::Value> {
    use cameleer_core::value::Value;
    Some(match &o.kind {
        ObjKind::BigInt(b) => match b.to_i64() {
            Some(i) => Value::Int(i),
            None => Value::String(b.to_string()),
        },
        ObjKind::Buffer(b) => Value::Bytes(b.0.borrow().clone()),
        ObjKind::Typed(t) if matches!(t.kind, TK::U8 | TK::U8C | TK::View) => Value::Bytes(t.bytes()),
        ObjKind::Typed(t) => Value::Array(t.items().iter().map(conv).collect()),
        ObjKind::Proxy(p) => {
            let t = p.0.borrow().0.clone();
            conv(&t)
        }
        _ => return None,
    })
}

// ====================================================================== namespaces

/// `X(...)` for the namespaces of this module.
pub fn call_ns(it: &mut Interp, n: &str, args: &[V], pos: u32) -> R<Option<V>> {
    let a0 = args.first().cloned().unwrap_or(V::Undef);
    Ok(Some(match n {
        "BigInt" | "%BigInt" => make_bigint(to_bigint(it, &a0, pos)?),
        "structuredClone" => {
            let mut seen = HashMap::new();
            clone_deep(it, &a0, &mut seen, pos)?
        }
        "%tpl" => {
            // the strings array carries a `raw` property (kept in a side table)
            let raw = args.get(1).cloned().unwrap_or(V::Undef);
            set_arr_prop(it, &a0, "raw", raw);
            a0
        }
        "%import" => {
            let path = it.display(&a0);
            return import_module(it, &path, pos).map(Some);
        }
        "%dynimport" => {
            let path = it.display(&a0);
            let p = it.new_promise();
            match import_module(it, &path, pos) {
                Ok(ns) => it.resolve_promise(&p, ns)?,
                Err(crate::interp::Ctrl::Throw(e)) => it.reject_promise(&p, e),
                Err(e) => return Err(e),
            }
            p
        }
        "Proxy" => return it.type_error("Constructor Proxy requires 'new'".into(), Some(pos)),
        _ if TK::of(n).is_some() || n == "ArrayBuffer" || n == "DataView" => return it.type_error(format!("Constructor {} requires 'new'", n), Some(pos)),
        "Intl.NumberFormat" | "Intl.DateTimeFormat" | "Intl.PluralRules" | "Intl.Collator" | "Intl.ListFormat" | "Intl.RelativeTimeFormat" => return construct_ns(it, n, args, pos),
        _ => return Ok(None),
    }))
}

/// Attach an extra property to an array (`raw`, `index`, `groups`, `input`).
pub fn set_arr_prop(it: &mut Interp, a: &V, name: &str, v: V) {
    if let V::Arr(r) = a {
        let key = Rc::as_ptr(r) as *const u8 as usize;
        let e = it.arr_extra.entry(key).or_insert_with(|| (a.clone(), PropMap::default()));
        e.1.set(name, v);
    }
}

/// An extra property of an array.
pub fn arr_prop(it: &Interp, a: &ArrRef, name: &str) -> Option<V> {
    if it.arr_extra.is_empty() {
        return None;
    }
    let key = Rc::as_ptr(a) as *const u8 as usize;
    it.arr_extra.get(&key).and_then(|(_, p)| p.get(name).cloned())
}

/// A JavaScript regex match array (`[match, ...groups]` with `index`, `input` and `groups`).
pub fn js_match(it: &mut Interp, re: &cameleer_core::regex::Regex, c: &cameleer_core::regex::Captures, s: &str) -> V {
    let arr = V::arr(c.to_vec().into_iter().map(|g| g.map(V::string).unwrap_or(V::Undef)).collect());
    let st = c.span(0).map(|x| x.0).unwrap_or(0);
    set_arr_prop(it, &arr, "index", V::Int(s[..st].chars().count() as i64));
    set_arr_prop(it, &arr, "input", V::str(s));
    set_arr_prop(it, &arr, "groups", named_groups(re, c));
    arr
}

/// The `groups` object of a match (undefined without named groups).
pub fn named_groups(re: &cameleer_core::regex::Regex, c: &cameleer_core::regex::Captures) -> V {
    let names = re.group_names();
    if names.is_empty() {
        return V::Undef;
    }
    let mut ordered: Vec<(&String, &usize)> = names.iter().collect();
    ordered.sort_by_key(|(_, i)| **i);
    let mut p = PropMap::default();
    for (n, i) in ordered {
        p.set(n, c.get(*i).map(V::str).unwrap_or(V::Undef));
    }
    V::obj(p)
}

/// `new X(...)` for the namespaces of this module.
pub fn construct_ns(it: &mut Interp, n: &str, args: &[V], pos: u32) -> R<Option<V>> {
    let a0 = args.first().cloned().unwrap_or(V::Undef);
    if let Some(k) = TK::of(n) {
        return construct_typed(it, k, args, pos).map(Some);
    }
    Ok(Some(match n {
        "Proxy" => {
            let handler = args.get(1).cloned().unwrap_or(V::Undef);
            if !matches!(a0, V::Obj(_) | V::Arr(_) | V::Func(_) | V::Host(_) | V::Method(_)) || !matches!(handler, V::Obj(_)) {
                return it.type_error("Cannot create proxy with a non-object as target or handler".into(), Some(pos));
            }
            V::obj_kind(PropMap::default(), ObjKind::Proxy(Internal::new((a0, handler))))
        }
        "ArrayBuffer" => {
            let n = num_of(it, &a0);
            let n = if n.is_nan() { 0.0 } else { n };
            if n < 0.0 {
                return it.throw("RangeError", "Invalid array buffer length".into(), Some(pos));
            }
            make_buffer(Internal::new(vec![0u8; n as usize]))
        }
        "DataView" => construct_dataview(it, args, pos)?,
        "TextEncoder" => {
            let mut p = PropMap::default();
            p.set("encoding", V::str("utf-8"));
            p.set("encode", V::Method(Rc::new((V::Null, "%te_encode".into()))));
            V::obj(p)
        }
        "TextDecoder" => {
            let mut p = PropMap::default();
            let enc = if a0.is_nullish() { "utf-8".to_string() } else { it.display(&a0).to_lowercase() };
            p.set("encoding", V::string(enc));
            let me = V::obj(p);
            if let V::Obj(o) = &me {
                o.borrow_mut().props.set("decode", V::Method(Rc::new((me.clone(), "%td_decode".into()))));
            }
            me
        }
        "BigInt" | "%BigInt" => return it.type_error("BigInt is not a constructor".into(), Some(pos)),
        "Intl.NumberFormat" => intl_object(it, "NumberFormat", args),
        "Intl.DateTimeFormat" => intl_object(it, "DateTimeFormat", args),
        "Intl.PluralRules" => intl_object(it, "PluralRules", args),
        "Intl.Collator" => intl_object(it, "Collator", args),
        "Intl.ListFormat" => intl_object(it, "ListFormat", args),
        "Intl.RelativeTimeFormat" => intl_object(it, "RelativeTimeFormat", args),
        _ => return Ok(None),
    }))
}

/// Static members (`BigInt.asIntN`, `Intl.NumberFormat`, `Uint8Array.BYTES_PER_ELEMENT`).
pub fn ns_member(it: &mut Interp, ns: &str, name: &str) -> R<Option<V>> {
    if let Some(k) = TK::of(ns) {
        if name == "BYTES_PER_ELEMENT" {
            return Ok(Some(V::Int(k.size() as i64)));
        }
    }
    if ns == "Intl" {
        if let Some(n) = ns_name(&format!("Intl.{}", name)) {
            return Ok(Some(V::host(Host::Ns(n))));
        }
    }
    if ns == "globalThis" {
        if name == "globalThis" {
            return Ok(Some(V::host(Host::Ns("globalThis"))));
        }
        return Ok(Some(match it.globals.lookup(name) {
            Some(v) => v,
            None => match crate::builtins::type_ns(name) {
                Some(v) if it.d == Dialect::Js => v,
                _ => V::Undef,
            },
        }));
    }
    Ok(None)
}

/// Static functions (`BigInt.asIntN(..)`, `Uint8Array.from(..)`, `ArrayBuffer.isView(..)`,
/// `Reflect.*`, `Object.*` over proxies / typed arrays).
pub fn call_ns_fn(it: &mut Interp, ns: &str, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
    let a0 = args.first().cloned().unwrap_or(V::Undef);
    let a1 = args.get(1).cloned().unwrap_or(V::Undef);
    if let Some(k) = TK::of(ns) {
        return Ok(Some(match name {
            "from" => {
                let mut items = match typed_of(&a0) {
                    Some(t) => t.items(),
                    None => it.iterate(&a0, Some(pos))?,
                };
                if a1.is_callable() {
                    let mut out = vec![];
                    for (i, x) in items.into_iter().enumerate() {
                        out.push(it.call(&a1, V::Undef, vec![x, V::Int(i as i64)], pos)?);
                    }
                    items = out;
                }
                typed_from(it, k, &items)
            }
            "of" => typed_from(it, k, args),
            _ => return Ok(None),
        }));
    }
    if ns == "globalThis" {
        let f = ns_member(it, ns, name)?.unwrap_or(V::Undef);
        return it.call(&f, V::Undef, args.to_vec(), pos).map(Some);
    }
    match (ns, name) {
        ("String", "raw") => {
            let raw = match &a0 {
                V::Arr(a) => arr_prop(it, a, "raw").unwrap_or(a0.clone()),
                other => it.get_member(other, "raw", pos)?,
            };
            let parts = it.iterate(&raw, Some(pos))?;
            let mut out = String::new();
            for (i, p) in parts.iter().enumerate() {
                out.push_str(&it.display(p));
                if i + 1 < parts.len() {
                    if let Some(v) = args.get(i + 1) {
                        out.push_str(&display_js(it, v));
                    }
                }
            }
            return Ok(Some(V::string(out)));
        }
        ("BigInt", "asIntN" | "asUintN") => {
            let bits = num_of(it, &a0) as usize;
            let b = to_bigint(it, &a1, pos)?;
            return Ok(Some(make_bigint(if name == "asIntN" { b.as_int_n(bits) } else { b.as_uint_n(bits) })));
        }
        ("ArrayBuffer", "isView") => return Ok(Some(V::Bool(typed_of(&a0).is_some()))),
        ("Intl", "getCanonicalLocales") => {
            let items = if a0.is_nullish() { vec![] } else if let V::Str(_) = a0 { vec![a0.clone()] } else { it.iterate(&a0, Some(pos))? };
            return Ok(Some(V::arr(items.iter().map(|x| V::string(canonical_locale(&it.display(x)))).collect())));
        }
        ("Intl.NumberFormat" | "Intl.DateTimeFormat" | "Intl.PluralRules" | "Intl.Collator", "supportedLocalesOf") => {
            let items = if let V::Str(_) = a0 { vec![a0.clone()] } else { it.iterate(&a0, Some(pos))? };
            let out = items.iter().map(|x| canonical_locale(&it.display(x))).filter(|l| LOCALES.contains(&l.as_str()) || LOCALES.iter().any(|s| s.starts_with(l.as_str()))).map(V::string).collect();
            return Ok(Some(V::arr(out)));
        }
        ("Object" | "Reflect", "keys" | "getOwnPropertyNames" | "ownKeys") if is_proxy(&a0) || typed_of(&a0).is_some() => {
            return own_keys(it, &a0, pos).map(|k| Some(V::arr(k)));
        }
        ("Object", "values" | "entries") if is_proxy(&a0) || typed_of(&a0).is_some() => {
            let keys = own_keys(it, &a0, pos)?;
            let mut out = vec![];
            for k in keys {
                let v = it.get_index(&a0, &k, pos)?;
                out.push(if name == "values" { v } else { V::arr(vec![k, v]) });
            }
            return Ok(Some(V::arr(out)));
        }
        ("Reflect", "has") => {
            let r = if is_proxy(&a0) { proxy_has(it, &a0, &a1, pos)? } else { it.contains(&a0, &a1, pos)? };
            return Ok(Some(V::Bool(r)));
        }
        ("Reflect", "get") if is_proxy(&a0) => return proxy_get(it, &a0, &a1, pos).map(Some),
        ("Reflect", "set") if is_proxy(&a0) => {
            proxy_set(it, &a0, &a1, args.get(2).cloned().unwrap_or(V::Undef), pos)?;
            return Ok(Some(V::Bool(true)));
        }
        ("Reflect", "deleteProperty") => {
            let k = it.key_string(&a1);
            let r = if is_proxy(&a0) {
                proxy_delete(it, &a0, &k, pos)?
            } else {
                match &a0 {
                    V::Obj(o) => {
                        o.borrow_mut().props.remove(&k);
                        true
                    }
                    _ => false,
                }
            };
            return Ok(Some(V::Bool(r)));
        }
        ("Reflect", "defineProperty") => {
            let defp = it.get_member(&V::host(Host::Ns("Object")), "defineProperty", pos)?;
            let r = it.call(&defp, V::Undef, args.to_vec(), pos);
            return Ok(Some(V::Bool(r.is_ok())));
        }
        ("Reflect", "getPrototypeOf") => {
            let f = it.get_member(&V::host(Host::Ns("Object")), "getPrototypeOf", pos)?;
            return it.call(&f, V::Undef, vec![a0], pos).map(Some);
        }
        ("Reflect", "setPrototypeOf") => {
            let f = it.get_member(&V::host(Host::Ns("Object")), "setPrototypeOf", pos)?;
            let r = it.call(&f, V::Undef, args.to_vec(), pos);
            return Ok(Some(V::Bool(r.is_ok())));
        }
        ("Reflect", "getOwnPropertyDescriptor") => {
            let f = it.get_member(&V::host(Host::Ns("Object")), "getOwnPropertyDescriptor", pos)?;
            return it.call(&f, V::Undef, args.to_vec(), pos).map(Some);
        }
        ("Reflect", "isExtensible") => return Ok(Some(V::Bool(true))),
        ("Reflect", "preventExtensions") => return Ok(Some(V::Bool(true))),
        ("Reflect", "apply") if is_proxy(&a0) => {
            let a = it.iterate(&args.get(2).cloned().unwrap_or(V::Undef), Some(pos))?;
            return proxy_call(it, &a0, a1, a, pos).map(Some);
        }
        ("Reflect", "construct") if is_proxy(&a0) => {
            let a = it.iterate(&a1, Some(pos))?;
            return proxy_construct(it, &a0, a, pos).map(Some);
        }
        ("Proxy", "revocable") => {
            let p = construct_ns(it, "Proxy", args, pos)?.unwrap_or(V::Undef);
            let mut o = PropMap::default();
            o.set("proxy", p.clone());
            o.set("revoke", V::Method(Rc::new((p, "%proxy_revoke".into()))));
            return Ok(Some(V::obj(o)));
        }
        _ => {}
    }
    Ok(None)
}

/// Internal (`%`-named) methods of this module.
pub fn internal_method(it: &mut Interp, recv: &V, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
    let a0 = args.first().cloned().unwrap_or(V::Undef);
    Ok(Some(match name {
        "%te_encode" => uint8_array(it.display(&a0).into_bytes()),
        "%td_decode" => {
            let bytes = match (&a0, typed_of(&a0), buffer_of(&a0)) {
                (V::Bytes(b), _, _) => (**b).clone(),
                (_, Some(t), _) => t.bytes(),
                (_, _, Some(b)) => b.0.borrow().clone(),
                (V::Undef, _, _) => vec![],
                _ => it.iterate(&a0, Some(pos))?.iter().map(|x| num_of(it, x) as u8).collect(),
            };
            let enc = match recv {
                V::Obj(o) => o.borrow().props.get("encoding").map(|e| it.display(e)).unwrap_or_default(),
                _ => String::new(),
            };
            V::string(match enc.as_str() {
                "latin1" | "iso-8859-1" | "windows-1252" | "ascii" => bytes.iter().map(|b| *b as char).collect(),
                "utf-16le" => {
                    let u: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
                    String::from_utf16_lossy(&u)
                }
                _ => String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&bytes)).into_owned(),
            })
        }
        "%proxy_revoke" => {
            if let V::Obj(o) = recv {
                if let ObjKind::Proxy(p) = &o.borrow().kind {
                    *p.0.borrow_mut() = (V::Null, V::Null);
                }
            }
            V::Undef
        }
        n if n.starts_with("%intl:") => return intl_method(it, recv, &n[6..], args, pos).map(Some),
        _ => return Ok(None),
    }))
}

// ====================================================================== structuredClone

fn clone_deep(it: &mut Interp, v: &V, seen: &mut HashMap<usize, V>, pos: u32) -> R<V> {
    let key = match v {
        V::Arr(a) => Rc::as_ptr(a) as *const u8 as usize,
        V::Obj(o) => Rc::as_ptr(o) as *const u8 as usize,
        V::Func(_) | V::Method(_) => {
            return it.throw("DataCloneError", format!("{} could not be cloned.", it.display_short(v)), Some(pos));
        }
        _ => return Ok(v.clone()),
    };
    if let Some(c) = seen.get(&key) {
        return Ok(c.clone());
    }
    match v {
        V::Arr(a) => {
            let out = V::arr(vec![]);
            seen.insert(key, out.clone());
            let items = a.borrow().clone();
            let mut cloned = Vec::with_capacity(items.len());
            for x in &items {
                cloned.push(clone_deep(it, x, seen, pos)?);
            }
            if let V::Arr(o) = &out {
                *o.borrow_mut() = cloned;
            }
            Ok(out)
        }
        V::Obj(o) => {
            let kind = o.borrow().kind.clone();
            match kind {
                ObjKind::Date(ms) => Ok(V::obj_kind(PropMap::default(), ObjKind::Date(ms))),
                ObjKind::BigInt(_) => Ok(v.clone()),
                ObjKind::Buffer(b) => Ok(make_buffer(Internal::new(b.0.borrow().clone()))),
                ObjKind::Typed(t) => {
                    let bytes = t.bytes();
                    let len = t.len;
                    Ok(make_typed(Typed { kind: t.kind, buf: Internal::new(bytes), off: 0, len }))
                }
                ObjKind::Coll(c) => {
                    let (name, pairs) = {
                        let b = c.0.borrow();
                        let name = crate::collections::coll_name(&b);
                        let pairs = crate::collections::iterate_coll(&b).unwrap_or_default();
                        (name, pairs)
                    };
                    if name.starts_with("Weak") {
                        return it.throw("DataCloneError", format!("#<{}> could not be cloned.", name), Some(pos));
                    }
                    let mut items = vec![];
                    for p in &pairs {
                        items.push(clone_deep(it, p, seen, pos)?);
                    }
                    let r = it.construct_coll(name, &[V::arr(items)], pos)?;
                    seen.insert(key, r.clone());
                    Ok(r)
                }
                ObjKind::Symbol(..) | ObjKind::Promise(_) | ObjKind::Generator(_) | ObjKind::Proxy(_) => {
                    it.throw("DataCloneError", format!("{} could not be cloned.", it.display_short(v)), Some(pos))
                }
                ObjKind::Error => {
                    let props = o.borrow().props.clone();
                    Ok(V::obj_kind(props, ObjKind::Error))
                }
                _ => {
                    let out = V::new_obj();
                    seen.insert(key, out.clone());
                    let entries: Vec<(Rc<str>, V)> = o.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                    for (k, x) in entries {
                        if let V::Obj(acc) = &x {
                            if acc.borrow().kind == ObjKind::Accessor {
                                let val = it.get_member(v, &k, pos)?;
                                let c = clone_deep(it, &val, seen, pos)?;
                                if let V::Obj(oo) = &out {
                                    oo.borrow_mut().props.set(&k, c);
                                }
                                continue;
                            }
                        }
                        if matches!(x, V::Func(_) | V::Method(_)) {
                            return it.throw("DataCloneError", format!("{} could not be cloned.", it.display_short(&x)), Some(pos));
                        }
                        let c = clone_deep(it, &x, seen, pos)?;
                        if let V::Obj(oo) = &out {
                            oo.borrow_mut().props.set(&k, c);
                        }
                    }
                    Ok(out)
                }
            }
        }
        _ => Ok(v.clone()),
    }
}

// ====================================================================== ES modules

/// Load an ES module (cached per evaluation): runs it in its own scope and returns its
/// namespace object (the `export`ed bindings, `default` for the default export).
pub fn import_module(it: &mut Interp, path: &str, pos: u32) -> R<V> {
    let Some(file) = resolve_module(it, path) else {
        return it.throw("Error", format!("Cannot find module '{}'", path), Some(pos));
    };
    let key = format!("js:{}", file.display());
    if let Some(v) = it.imports.get(&key) {
        return Ok(v.clone());
    }
    let src = match std::fs::read_to_string(&file) {
        Ok(s) => s,
        Err(e) => return it.throw("Error", format!("Cannot read module '{}': {}", path, e), Some(pos)),
    };
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
    let exports = V::new_obj();
    if ext == "json" {
        let v = match crate::json::parse(&src) {
            Ok(v) => v,
            Err(m) => return it.throw("SyntaxError", format!("{}: {}", path, m), Some(pos)),
        };
        if let V::Obj(o) = &exports {
            o.borrow_mut().props.set("default", v);
        }
        it.imports.insert(key, exports.clone());
        return Ok(exports);
    }
    let prog = match crate::parser::parse_program(&src, Dialect::Js) {
        Ok(p) => p,
        Err(e) => {
            let (l, c) = crate::error::line_col(&src, e.pos);
            return it.throw("SyntaxError", format!("{}:{}:{}: {}", path, l, c, e.msg), Some(pos));
        }
    };
    // cycles see the partially initialized namespace
    it.imports.insert(key, exports.clone());
    let prog = Rc::new(prog);
    it.import_asts.push(prog.clone());
    let saved = std::mem::replace(&mut it.src, std::sync::Arc::from(src.as_str()));
    let env = crate::interp::new_scope(Some(it.globals.clone()), true);
    env.define("%exports", exports.clone());
    it.module_dirs.push(file.parent().map(|p| p.to_path_buf()).unwrap_or_default());
    it.hoist_pub(&prog.body, &env);
    let mut r = Ok(());
    for s in &prog.body {
        match it.exec(s, &env) {
            Ok(_) => {}
            Err(Ctrl::Throw(e)) => {
                r = Err(Ctrl::Throw(e));
                break;
            }
            Err(_) => {}
        }
    }
    it.module_dirs.pop();
    it.src = saved;
    r?;
    // a top-level-await module body: settle it
    Ok(exports)
}

fn resolve_module(it: &Interp, path: &str) -> Option<std::path::PathBuf> {
    let p = path.strip_prefix("file:").unwrap_or(path);
    let candidates = |base: std::path::PathBuf| -> Option<std::path::PathBuf> {
        if base.is_file() {
            return Some(base);
        }
        for ext in ["js", "mjs", "json"] {
            let f = base.with_extension(ext);
            if f.is_file() {
                return Some(f);
            }
        }
        let idx = base.join("index.js");
        idx.is_file().then_some(idx)
    };
    if (p.starts_with("./") || p.starts_with("../")) && !path.starts_with("classpath:") {
        let dir = it.module_dirs.last().cloned().or_else(|| it.script_dir.clone());
        if let Some(d) = dir {
            if let Some(f) = candidates(d.join(p)) {
                return Some(f);
            }
        }
    }
    if let Some(f) = crate::jobj::resolve_import(path) {
        return Some(f);
    }
    for ext in ["js", "mjs", "json"] {
        if let Some(f) = crate::jobj::resolve_import(&format!("{}.{}", path, ext)) {
            return Some(f);
        }
    }
    None
}

// ====================================================================== Intl

const LOCALES: &[&str] = &["en-US", "en-GB", "de-DE", "fr-FR", "en", "de", "fr"];

fn canonical_locale(s: &str) -> String {
    let s = s.replace('_', "-");
    let mut parts = s.split('-');
    let lang = parts.next().unwrap_or("en").to_lowercase();
    match parts.next() {
        Some(r) => format!("{}-{}", lang, r.to_uppercase()),
        None => lang,
    }
}

/// A supported locale (`en-US` by default; `de`, `fr`, `en-GB` regions).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Loc {
    EnUs,
    EnGb,
    De,
    Fr,
}

impl Loc {
    fn tag(self) -> &'static str {
        match self {
            Loc::EnUs => "en-US",
            Loc::EnGb => "en-GB",
            Loc::De => "de-DE",
            Loc::Fr => "fr-FR",
        }
    }
    fn group(self) -> &'static str {
        match self {
            Loc::EnUs | Loc::EnGb => ",",
            Loc::De => ".",
            Loc::Fr => "\u{202F}",
        }
    }
    fn decimal(self) -> &'static str {
        match self {
            Loc::EnUs | Loc::EnGb => ".",
            Loc::De | Loc::Fr => ",",
        }
    }
}

fn parse_loc(s: &str) -> Loc {
    let c = canonical_locale(s);
    if c.starts_with("de") {
        Loc::De
    } else if c.starts_with("fr") {
        Loc::Fr
    } else if c == "en-GB" || c == "en-IE" || c == "en-AU" || c == "en-NZ" {
        Loc::EnGb
    } else {
        Loc::EnUs
    }
}

fn locale_of(it: &mut Interp, v: Option<&V>) -> Loc {
    match v {
        Some(V::Str(s)) => parse_loc(s),
        Some(V::Arr(a)) => a.borrow().first().map(|x| parse_loc(&it.display(x))).unwrap_or(Loc::EnUs),
        _ => Loc::EnUs,
    }
}

fn group_digits(int: &str, loc: &Loc, grouping: bool, neg: bool) -> String {
    let mut out = String::new();
    let n = int.len();
    // German / French do not group four-digit numbers in ICU? They do (1.234); keep simple grouping
    for (i, c) in int.chars().enumerate() {
        if grouping && i > 0 && (n - i) % 3 == 0 {
            out.push_str(loc.group());
        }
        out.push(c);
    }
    if neg {
        format!("-{}", out)
    } else {
        out
    }
}

fn opt(it: &mut Interp, opts: &V, name: &str) -> Option<V> {
    match opts {
        V::Obj(o) => o.borrow().props.get(name).cloned().filter(|v| !v.is_nullish()).or_else(|| {
            let _ = &it;
            None
        }),
        _ => None,
    }
}

fn opt_s(it: &mut Interp, opts: &V, name: &str) -> Option<String> {
    opt(it, opts, name).map(|v| it.display(&v))
}

fn opt_n(it: &mut Interp, opts: &V, name: &str) -> Option<usize> {
    opt(it, opts, name).map(|v| num_of(it, &v).max(0.0) as usize)
}

fn intl_object(it: &mut Interp, kind: &str, args: &[V]) -> V {
    let loc = locale_of(it, args.first());
    let opts = args.get(1).cloned().unwrap_or(V::Undef);
    let mut p = PropMap::default();
    p.set("%kind", V::str(kind));
    p.set("%locale", V::str(loc.tag()));
    p.set("%opts", if matches!(opts, V::Obj(_)) { opts } else { V::new_obj() });
    let me = V::obj(p);
    let methods: &[&str] = match kind {
        "NumberFormat" => &["format", "formatToParts", "resolvedOptions"],
        "DateTimeFormat" => &["format", "formatToParts", "resolvedOptions", "formatRange"],
        "PluralRules" => &["select", "resolvedOptions"],
        "Collator" => &["compare", "resolvedOptions"],
        "ListFormat" => &["format", "resolvedOptions"],
        _ => &["format", "resolvedOptions"],
    };
    if let V::Obj(o) = &me {
        for m in methods {
            o.borrow_mut().props.set(m, V::Method(Rc::new((me.clone(), format!("%intl:{}", m)))));
        }
    }
    me
}

fn intl_method(it: &mut Interp, recv: &V, m: &str, args: &[V], pos: u32) -> R<V> {
    let (kind, loc, opts) = match recv {
        V::Obj(o) => {
            let b = o.borrow();
            (
                b.props.get("%kind").map(|v| it.display(v)).unwrap_or_default(),
                parse_loc(&b.props.get("%locale").map(|v| it.display(v)).unwrap_or_default()),
                b.props.get("%opts").cloned().unwrap_or(V::Undef),
            )
        }
        _ => return Ok(V::Undef),
    };
    let a0 = args.first().cloned().unwrap_or(V::Undef);
    match (kind.as_str(), m) {
        (_, "resolvedOptions") => {
            let mut p = match &opts {
                V::Obj(o) => o.borrow().props.clone(),
                _ => PropMap::default(),
            };
            p.set("locale", V::str(loc.tag()));
            if kind == "DateTimeFormat" && !p.contains("timeZone") {
                p.set("timeZone", V::str("UTC"));
            }
            if kind == "NumberFormat" {
                if !p.contains("style") {
                    p.set("style", V::str("decimal"));
                }
                p.set("numberingSystem", V::str("latn"));
            }
            Ok(V::obj(p))
        }
        ("NumberFormat", "format") => {
            let s = format_number(it, &a0, loc, &opts, pos)?;
            Ok(V::string(s))
        }
        ("NumberFormat", "formatToParts") => {
            let s = format_number(it, &a0, loc, &opts, pos)?;
            Ok(number_parts(&s, loc))
        }
        ("DateTimeFormat", "format") => {
            let ms = date_ms_of(it, &a0);
            Ok(V::string(format_date(it, ms, loc, &opts, DefaultParts::Date)?))
        }
        ("DateTimeFormat", "formatRange") => {
            let ms0 = date_ms_of(it, &a0);
            let a = format_date(it, ms0, loc, &opts, DefaultParts::Date)?;
            let ms1 = date_ms_of(it, &args.get(1).cloned().unwrap_or(V::Undef));
            let b = format_date(it, ms1, loc, &opts, DefaultParts::Date)?;
            Ok(V::string(if a == b { a } else { format!("{} – {}", a, b) }))
        }
        ("DateTimeFormat", "formatToParts") => {
            let ms0 = date_ms_of(it, &a0);
            let s = format_date(it, ms0, loc, &opts, DefaultParts::Date)?;
            let mut out = vec![];
            let mut cur = String::new();
            let mut digit = false;
            for c in s.chars() {
                let d = c.is_alphanumeric();
                if !cur.is_empty() && d != digit {
                    out.push(part(if digit { "literal-value" } else { "literal" }, &cur));
                    cur.clear();
                }
                digit = d;
                cur.push(c);
            }
            if !cur.is_empty() {
                out.push(part(if digit { "literal-value" } else { "literal" }, &cur));
            }
            // label the values: day / month / year order by locale
            Ok(V::arr(out))
        }
        ("PluralRules", "select") => {
            let n = num_of(it, &a0);
            let ordinal = opt_s(it, &opts, "type").as_deref() == Some("ordinal");
            Ok(V::str(plural(n, loc, ordinal)))
        }
        ("Collator", "compare") => {
            let a = it.display(&a0);
            let b = it.display(&args.get(1).cloned().unwrap_or(V::Undef));
            let numeric = opt(it, &opts, "numeric").is_some_and(|v| it.truthy(&v));
            let base = matches!(opt_s(it, &opts, "sensitivity").as_deref(), Some("base") | Some("accent"));
            Ok(V::Int(collate(&a, &b, numeric, base) as i64))
        }
        ("ListFormat", "format") => {
            let items: Vec<String> = it.iterate(&a0, Some(pos))?.iter().map(|x| it.display(x)).collect();
            let disj = opt_s(it, &opts, "type").as_deref() == Some("disjunction");
            let word = match (loc, disj) {
                (Loc::EnUs | Loc::EnGb, false) => "and",
                (Loc::EnUs | Loc::EnGb, true) => "or",
                (Loc::De, false) => "und",
                (Loc::De, true) => "oder",
                (Loc::Fr, false) => "et",
                (Loc::Fr, true) => "ou",
            };
            Ok(V::string(match items.len() {
                0 => String::new(),
                1 => items[0].clone(),
                2 => format!("{} {} {}", items[0], word, items[1]),
                n => {
                    let head = items[..n - 1].join(", ");
                    let comma = if loc == Loc::EnUs { "," } else { "" };
                    format!("{}{} {} {}", head, comma, word, items[n - 1])
                }
            }))
        }
        ("RelativeTimeFormat", "format") => {
            let n = num_of(it, &a0);
            let unit = it.display(&args.get(1).cloned().unwrap_or(V::Undef));
            let unit = unit.trim_end_matches('s').to_string();
            let numeric_auto = opt_s(it, &opts, "numeric").as_deref() == Some("auto");
            Ok(V::string(relative_time(n, &unit, loc, numeric_auto)))
        }
        _ => it.type_error(format!("Intl.{}.prototype.{} is not supported", kind, m), Some(pos)),
    }
}

fn part(t: &str, v: &str) -> V {
    let mut p = PropMap::default();
    p.set("type", V::str(if t == "literal-value" { "integer" } else { t }));
    p.set("value", V::str(v));
    V::obj(p)
}

fn number_parts(s: &str, loc: Loc) -> V {
    let mut out = vec![];
    let mut cur = String::new();
    let mut in_frac = false;
    let flush = |cur: &mut String, out: &mut Vec<V>, frac: bool| {
        if !cur.is_empty() {
            out.push(part(if frac { "fraction" } else { "integer" }, cur));
            cur.clear();
        }
    };
    for c in s.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
            continue;
        }
        flush(&mut cur, &mut out, in_frac);
        let cs = c.to_string();
        let t = if cs == loc.decimal() {
            in_frac = true;
            "decimal"
        } else if cs == loc.group() {
            "group"
        } else if c == '-' {
            "minusSign"
        } else if c == '%' {
            "percentSign"
        } else if c == '\u{a0}' || c == ' ' || c == '\u{202F}' {
            "literal"
        } else {
            "currency"
        };
        out.push(part(t, &cs));
    }
    flush(&mut cur, &mut out, in_frac);
    V::arr(out)
}

fn currency_info(code: &str) -> (&'static str, usize) {
    match code {
        "USD" => ("$", 2),
        "EUR" => ("€", 2),
        "GBP" => ("£", 2),
        "JPY" => ("¥", 0),
        "CHF" => ("CHF", 2),
        "CAD" => ("CA$", 2),
        "AUD" => ("A$", 2),
        "CNY" => ("CN¥", 2),
        "INR" => ("₹", 2),
        _ => ("", 2),
    }
}

/// Round `|f|` half away from zero to `digits` fractional digits on its shortest decimal
/// representation (as ICU does: 1.005 -> 1.01); returns (integer, fraction) digit strings.
fn fixed_digits(f: f64, digits: usize) -> (String, String) {
    let s = format!("{}", f.abs());
    let (int, frac) = match s.split_once('.') {
        Some((i, fr)) => (i.to_string(), fr.to_string()),
        None => (s, String::new()),
    };
    if frac.len() <= digits {
        return (int, format!("{:0<width$}", frac, width = digits));
    }
    let round_up = frac.as_bytes()[digits] >= b'5';
    let mut all: Vec<u8> = format!("{}{}", int, &frac[..digits]).into_bytes();
    if round_up {
        let mut i = all.len();
        loop {
            if i == 0 {
                all.insert(0, b'1');
                break;
            }
            i -= 1;
            if all[i] == b'9' {
                all[i] = b'0';
            } else {
                all[i] += 1;
                break;
            }
        }
    }
    let s = String::from_utf8(all).unwrap_or_default();
    let split = s.len() - digits;
    (s[..split].to_string(), s[split..].to_string())
}

fn format_number(it: &mut Interp, v: &V, loc: Loc, opts: &V, _pos: u32) -> R<String> {
    if let Some(b) = bigint_of(v) {
        let grouping = opt(it, opts, "useGrouping").map(|x| it.truthy(&x)).unwrap_or(true);
        return Ok(group_digits(&b.abs().to_string(), &loc, grouping, b.neg));
    }
    let f = num_of(it, v);
    let style = opt_s(it, opts, "style").unwrap_or_else(|| "decimal".into());
    if f.is_nan() {
        return Ok("NaN".into());
    }
    let currency = opt_s(it, opts, "currency").map(|c| c.to_uppercase());
    let (sym, cur_digits) = currency.as_deref().map(currency_info).unwrap_or(("", 2));
    let (mut min_fd, mut max_fd) = match style.as_str() {
        "currency" => (cur_digits, cur_digits),
        "percent" => (0, 0),
        _ => (0, 3),
    };
    if let Some(n) = opt_n(it, opts, "minimumFractionDigits") {
        min_fd = n;
        max_fd = max_fd.max(n);
    }
    if let Some(n) = opt_n(it, opts, "maximumFractionDigits") {
        max_fd = n;
        min_fd = min_fd.min(n);
    }
    let grouping = opt(it, opts, "useGrouping").map(|x| !matches!(x, V::Bool(false)) && it.display(&x) != "false").unwrap_or(true);
    let min_int = opt_n(it, opts, "minimumIntegerDigits").unwrap_or(1);
    let notation = opt_s(it, opts, "notation").unwrap_or_default();
    let mut x = if style == "percent" { f * 100.0 } else { f };
    let mut suffix = String::new();
    if notation == "compact" {
        let abs = x.abs();
        let (div, s_en, s_de, s_fr) = if abs >= 1e12 {
            (1e12, "T", "\u{a0}Bio.", "\u{a0}Bn")
        } else if abs >= 1e9 {
            (1e9, "B", "\u{a0}Mrd.", "\u{a0}Md")
        } else if abs >= 1e6 {
            (1e6, "M", "\u{a0}Mio.", "\u{a0}M")
        } else if abs >= 1e3 {
            (1e3, "K", "", "\u{a0}k")
        } else {
            (1.0, "", "", "")
        };
        if div > 1.0 && !(loc == Loc::De && div == 1e3) {
            x /= div;
            suffix = match loc {
                Loc::EnUs | Loc::EnGb => s_en.into(),
                Loc::De => s_de.into(),
                Loc::Fr => s_fr.into(),
            };
            if opt_n(it, opts, "maximumFractionDigits").is_none() {
                max_fd = if x.abs() < 100.0 { 1 } else { 0 };
                min_fd = 0;
            }
        } else if div > 1.0 {
            // de: thousands are not abbreviated
        }
    }
    let max_sig = opt_n(it, opts, "maximumSignificantDigits");
    let (int, mut frac) = if let Some(sig) = max_sig {
        let mag = if x == 0.0 { 0 } else { x.abs().log10().floor() as i32 + 1 };
        let fd = (sig as i32 - mag).max(0) as usize;
        let (i, fr) = fixed_digits(x, fd);
        (i, fr)
    } else {
        fixed_digits(x, max_fd)
    };
    while frac.len() > min_fd && frac.ends_with('0') && max_sig.is_none() {
        frac.pop();
    }
    if max_sig.is_some() {
        while frac.ends_with('0') {
            frac.pop();
        }
    }
    let int = if int.len() < min_int { format!("{}{}", "0".repeat(min_int - int.len()), int) } else { int };
    let neg = x < 0.0 && (int.chars().any(|c| c != '0') || frac.chars().any(|c| c != '0'));
    let mut num = group_digits(&int, &loc, grouping && !(loc != Loc::EnUs && loc != Loc::EnGb && int.len() == 4 && false), false);
    if !frac.is_empty() {
        num.push_str(loc.decimal());
        num.push_str(&frac);
    }
    num.push_str(&suffix);
    let sign = if neg {
        "-"
    } else if opt_s(it, opts, "signDisplay").as_deref() == Some("always") || (opt_s(it, opts, "signDisplay").as_deref() == Some("exceptZero") && x != 0.0) {
        "+"
    } else {
        ""
    };
    Ok(match style.as_str() {
        "currency" => {
            let display = opt_s(it, opts, "currencyDisplay").unwrap_or_else(|| "symbol".into());
            let code = currency.clone().unwrap_or_default();
            let sym = if display == "code" || sym.is_empty() {
                code.clone()
            } else if display == "name" {
                return Ok(format!("{}{} {}", sign, num, currency_name(&code, x)));
            } else if loc == Loc::EnUs && code == "CAD" {
                "CA$".into()
            } else if (loc == Loc::De || loc == Loc::Fr) && code == "USD" {
                "$".into()
            } else {
                sym.to_string()
            };
            match loc {
                Loc::EnUs | Loc::EnGb => {
                    if display == "code" || sym.len() > 2 && sym.chars().all(|c| c.is_ascii_uppercase()) {
                        format!("{}{}\u{a0}{}", sign, sym, num)
                    } else {
                        format!("{}{}{}", sign, sym, num)
                    }
                }
                Loc::De | Loc::Fr => format!("{}{}\u{a0}{}", sign, num, sym),
            }
        }
        "percent" => match loc {
            Loc::EnUs | Loc::EnGb => format!("{}{}%", sign, num),
            Loc::De => format!("{}{}\u{a0}%", sign, num),
            Loc::Fr => format!("{}{}\u{202F}%", sign, num),
        },
        "unit" => {
            let unit = opt_s(it, opts, "unit").unwrap_or_default();
            let long = opt_s(it, opts, "unitDisplay").as_deref() == Some("long");
            let u = unit_symbol(&unit, long, f);
            if u.starts_with(' ') || long {
                format!("{}{} {}", sign, num, u.trim())
            } else if matches!(unit.as_str(), "percent") {
                format!("{}{}{}", sign, num, u)
            } else {
                format!("{}{} {}", sign, num, u)
            }
        }
        _ => format!("{}{}", sign, num),
    })
}

fn currency_name(code: &str, x: f64) -> String {
    let one = x == 1.0;
    match code {
        "USD" => if one { "US dollar" } else { "US dollars" }.into(),
        "EUR" => if one { "euro" } else { "euros" }.into(),
        "GBP" => if one { "British pound" } else { "British pounds" }.into(),
        other => other.into(),
    }
}

fn unit_symbol(unit: &str, long: bool, n: f64) -> String {
    let one = n == 1.0;
    let (short, l1, ln) = match unit {
        "kilometer" => ("km", "kilometer", "kilometers"),
        "meter" => ("m", "meter", "meters"),
        "kilogram" => ("kg", "kilogram", "kilograms"),
        "gram" => ("g", "gram", "grams"),
        "liter" => ("L", "liter", "liters"),
        "second" => ("sec", "second", "seconds"),
        "minute" => ("min", "minute", "minutes"),
        "hour" => ("hr", "hour", "hours"),
        "day" => ("day", "day", "days"),
        "byte" => ("byte", "byte", "bytes"),
        "kilobyte" => ("kB", "kilobyte", "kilobytes"),
        "megabyte" => ("MB", "megabyte", "megabytes"),
        "gigabyte" => ("GB", "gigabyte", "gigabytes"),
        "celsius" => ("°C", "degree Celsius", "degrees Celsius"),
        "percent" => ("%", "percent", "percent"),
        "kilometer-per-hour" => ("km/h", "kilometer per hour", "kilometers per hour"),
        other => (other, other, other),
    };
    if long {
        if one { l1 } else { ln }.to_string()
    } else {
        short.to_string()
    }
}

fn plural(n: f64, loc: Loc, ordinal: bool) -> &'static str {
    if ordinal {
        if loc == Loc::EnUs || loc == Loc::EnGb {
            let i = n.abs() as i64;
            return match (i % 10, i % 100) {
                (1, x) if x != 11 => "one",
                (2, x) if x != 12 => "two",
                (3, x) if x != 13 => "few",
                _ => "other",
            };
        }
        if loc == Loc::Fr && n == 1.0 {
            return "one";
        }
        return "other";
    }
    match loc {
        Loc::Fr => {
            if (0.0..2.0).contains(&n.abs()) {
                "one"
            } else {
                "other"
            }
        }
        _ => {
            if n == 1.0 {
                "one"
            } else {
                "other"
            }
        }
    }
}

fn collate(a: &str, b: &str, numeric: bool, base: bool) -> i32 {
    let fold = |s: &str| -> String {
        s.chars()
            .map(|c| match c {
                'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
                'À' | 'Á' | 'Â' | 'Ä' | 'Ã' | 'Å' => 'A',
                'è' | 'é' | 'ê' | 'ë' => 'e',
                'È' | 'É' | 'Ê' | 'Ë' => 'E',
                'ì' | 'í' | 'î' | 'ï' => 'i',
                'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
                'Ö' | 'Ó' | 'Ò' | 'Ô' => 'O',
                'ù' | 'ú' | 'û' | 'ü' => 'u',
                'Ü' | 'Ú' | 'Ù' | 'Û' => 'U',
                'ç' => 'c',
                'Ç' => 'C',
                'ß' => 's',
                'ñ' => 'n',
                other => other,
            })
            .collect()
    };
    let (fa, fb) = (fold(a), fold(b));
    let key = |s: &str| -> Vec<(u8, String)> {
        // split into digit / non-digit runs for numeric collation
        let mut out = vec![];
        let mut cur = String::new();
        let mut dig = false;
        for c in s.chars() {
            let d = numeric && c.is_ascii_digit();
            if !cur.is_empty() && d != dig {
                out.push((dig as u8, std::mem::take(&mut cur)));
            }
            dig = d;
            cur.push(c);
        }
        if !cur.is_empty() {
            out.push((dig as u8, cur));
        }
        out
    };
    let (ka, kb) = (key(&fa), key(&fb));
    for (x, y) in ka.iter().zip(kb.iter()) {
        let o = if x.0 == 1 && y.0 == 1 {
            let (p, q) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
            p.len().cmp(&q.len()).then(p.cmp(q))
        } else {
            x.1.to_lowercase().cmp(&y.1.to_lowercase())
        };
        if o != std::cmp::Ordering::Equal {
            return o as i32;
        }
    }
    let o = ka.len().cmp(&kb.len());
    if o != std::cmp::Ordering::Equal {
        return o as i32;
    }
    if base {
        return 0;
    }
    // accents then case: lower case first
    let o = a.chars().map(|c| !c.is_ascii()).cmp(b.chars().map(|c| !c.is_ascii()));
    if o != std::cmp::Ordering::Equal {
        return o as i32;
    }
    let o = a.chars().map(|c| c.is_uppercase()).cmp(b.chars().map(|c| c.is_uppercase()));
    o as i32
}

fn relative_time(n: f64, unit: &str, loc: Loc, auto: bool) -> String {
    let abs = n.abs();
    let num = if abs.fract() == 0.0 { format!("{}", abs as i64) } else { crate::interp::js_num_str(abs) };
    if auto && abs <= 1.0 && abs.fract() == 0.0 {
        let special = match (loc, unit, n as i64) {
            (Loc::EnUs | Loc::EnGb, "day", -1) => Some("yesterday"),
            (Loc::EnUs | Loc::EnGb, "day", 0) => Some("today"),
            (Loc::EnUs | Loc::EnGb, "day", 1) => Some("tomorrow"),
            (Loc::De, "day", -1) => Some("gestern"),
            (Loc::De, "day", 0) => Some("heute"),
            (Loc::De, "day", 1) => Some("morgen"),
            (Loc::Fr, "day", -1) => Some("hier"),
            (Loc::Fr, "day", 0) => Some("aujourd’hui"),
            (Loc::Fr, "day", 1) => Some("demain"),
            (Loc::EnUs | Loc::EnGb, u, -1) => return format!("last {}", u),
            (Loc::EnUs | Loc::EnGb, u, 1) => return format!("next {}", u),
            _ => None,
        };
        if let Some(s) = special {
            return s.into();
        }
    }
    let one = abs == 1.0;
    match loc {
        Loc::EnUs | Loc::EnGb => {
            let u = if one { unit.to_string() } else { format!("{}s", unit) };
            if n < 0.0 || (n == 0.0 && n.is_sign_negative()) {
                format!("{} {} ago", num, u)
            } else {
                format!("in {} {}", num, u)
            }
        }
        Loc::De => {
            let (s, p, dat) = match unit {
                "second" => ("Sekunde", "Sekunden", "Sekunden"),
                "minute" => ("Minute", "Minuten", "Minuten"),
                "hour" => ("Stunde", "Stunden", "Stunden"),
                "day" => ("Tag", "Tagen", "Tagen"),
                "week" => ("Woche", "Wochen", "Wochen"),
                "month" => ("Monat", "Monaten", "Monaten"),
                "year" => ("Jahr", "Jahren", "Jahren"),
                other => (other, other, other),
            };
            let _ = p;
            if n < 0.0 {
                format!("vor {} {}", num, if one { if unit == "day" || unit == "month" || unit == "year" { format!("{}{}", s, if unit == "day" { "" } else { "" }) } else { s.to_string() } } else { dat.to_string() })
            } else {
                let fut = match unit {
                    "day" => if one { "Tag" } else { "Tagen" },
                    "month" => if one { "Monat" } else { "Monaten" },
                    "year" => if one { "Jahr" } else { "Jahren" },
                    _ => if one { s } else { dat },
                };
                format!("in {} {}", num, fut)
            }
        }
        Loc::Fr => {
            let (s, p) = match unit {
                "second" => ("seconde", "secondes"),
                "minute" => ("minute", "minutes"),
                "hour" => ("heure", "heures"),
                "day" => ("jour", "jours"),
                "week" => ("semaine", "semaines"),
                "month" => ("mois", "mois"),
                "year" => ("an", "ans"),
                other => (other, other),
            };
            let u = if abs < 2.0 { s } else { p };
            if n < 0.0 {
                format!("il y a {} {}", num, u)
            } else {
                format!("dans {} {}", num, u)
            }
        }
    }
}

fn date_ms_of(it: &mut Interp, v: &V) -> f64 {
    match v {
        V::Undef => cameleer_core::util_time::now_millis() as f64,
        V::Obj(o) => match o.borrow().kind {
            ObjKind::Date(ms) => ms,
            _ => f64::NAN,
        },
        other => num_of(it, other),
    }
}

/// Which parts a date format shows when no component options are given.
#[derive(Clone, Copy, PartialEq)]
pub enum DefaultParts {
    Date,
    Time,
    Both,
}

const MONTHS_EN: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const MONTHS_DE: [&str; 12] = ["Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November", "Dezember"];
const MONTHS_FR: [&str; 12] = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"];
const MONTHS_FR_SHORT: [&str; 12] = ["janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.", "déc."];
const MONTHS_DE_SHORT: [&str; 12] = ["Jan.", "Feb.", "März", "Apr.", "Mai", "Juni", "Juli", "Aug.", "Sept.", "Okt.", "Nov.", "Dez."];
const DAYS_EN: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const DAYS_DE: [&str; 7] = ["Sonntag", "Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag"];
const DAYS_FR: [&str; 7] = ["dimanche", "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi"];

fn month_name(loc: Loc, m: usize, short: bool, narrow: bool) -> String {
    let full = match loc {
        Loc::EnUs | Loc::EnGb => MONTHS_EN[m],
        Loc::De => MONTHS_DE[m],
        Loc::Fr => MONTHS_FR[m],
    };
    if narrow {
        return full.chars().next().unwrap().to_uppercase().collect();
    }
    if !short {
        return full.into();
    }
    match loc {
        Loc::EnUs | Loc::EnGb => {
            if m == 8 && loc == Loc::EnGb {
                "Sept".into()
            } else {
                full[..3].into()
            }
        }
        Loc::De => MONTHS_DE_SHORT[m].into(),
        Loc::Fr => MONTHS_FR_SHORT[m].into(),
    }
}

fn day_name(loc: Loc, d: usize, short: bool, narrow: bool) -> String {
    let full = match loc {
        Loc::EnUs | Loc::EnGb => DAYS_EN[d],
        Loc::De => DAYS_DE[d],
        Loc::Fr => DAYS_FR[d],
    };
    if narrow {
        return full.chars().next().unwrap().to_uppercase().collect();
    }
    if !short {
        return full.into();
    }
    match loc {
        Loc::EnUs | Loc::EnGb => full[..3].into(),
        Loc::De => format!("{}.", &full[..2]),
        Loc::Fr => format!("{}.", &full[..3]),
    }
}

/// Format a date (epoch milliseconds) like `Intl.DateTimeFormat` for the supported locales.
fn format_date(it: &mut Interp, ms: f64, loc: Loc, opts: &V, defaults: DefaultParts) -> R<String> {
    if ms.is_nan() {
        return it.throw("RangeError", "Invalid time value".into(), None);
    }
    let tz = opt_s(it, opts, "timeZone").unwrap_or_else(|| "UTC".into());
    let secs = (ms / 1000.0).floor() as i64;
    let offset = if tz.eq_ignore_ascii_case("UTC") || tz.eq_ignore_ascii_case("GMT") {
        0
    } else {
        match cameleer_core::tz::TimeZone::parse(&tz) {
            Ok(z) => z.offset_at(secs) as i64,
            Err(_) => return it.throw("RangeError", format!("Invalid time zone specified: {}", tz), None),
        }
    };
    let local = secs + offset;
    let (y, mo, d, h, mi, s) = cameleer_core::util_time::civil_from_epoch(local);
    let (mo, d) = (mo as usize, d as usize);
    let wd = ((local.div_euclid(86400) + 4).rem_euclid(7)) as usize;
    let get = |it: &mut Interp, k: &str| opt_s(it, opts, k);
    let date_style = get(it, "dateStyle");
    let time_style = get(it, "timeStyle");
    let mut year = get(it, "year");
    let mut month = get(it, "month");
    let mut day = get(it, "day");
    let weekday = get(it, "weekday");
    let mut hour = get(it, "hour");
    let mut minute = get(it, "minute");
    let mut second = get(it, "second");
    let era_name = get(it, "timeZoneName");
    let hour12_opt = opt(it, opts, "hour12").map(|v| it.truthy(&v));
    let hour_cycle = get(it, "hourCycle");
    let any_component = [&year, &month, &day, &weekday, &hour, &minute, &second].iter().any(|x| x.is_some());
    let mut out = String::new();
    let h12 = hour12_opt.unwrap_or(match hour_cycle.as_deref() {
        Some("h12") | Some("h11") => true,
        Some(_) => false,
        None => loc == Loc::EnUs,
    });
    let time_part = |hour: &Option<String>, minute: &Option<String>, second: &Option<String>| -> String {
        let mut t = String::new();
        let hh = if h12 {
            let x = h % 12;
            if x == 0 {
                12
            } else {
                x
            }
        } else {
            h
        };
        if let Some(hs) = hour {
            if hs == "2-digit" || (!h12 && minute.is_some()) {
                t.push_str(&format!("{:02}", hh));
            } else {
                t.push_str(&hh.to_string());
            }
        }
        if minute.is_some() {
            if !t.is_empty() {
                t.push(':');
            }
            t.push_str(&format!("{:02}", mi));
        }
        if second.is_some() {
            if !t.is_empty() {
                t.push(':');
            }
            t.push_str(&format!("{:02}", s));
        }
        if h12 && hour.is_some() {
            t.push('\u{202F}');
            t.push_str(if h < 12 { "AM" } else { "PM" });
        }
        t
    };
    if date_style.is_some() || time_style.is_some() {
        let mut date = String::new();
        if let Some(ds) = &date_style {
            date = match (loc, ds.as_str()) {
                (Loc::EnUs, "full") => format!("{}, {} {}, {}", day_name(loc, wd, false, false), month_name(loc, mo - 1, false, false), d, y),
                (Loc::EnUs, "long") => format!("{} {}, {}", month_name(loc, mo - 1, false, false), d, y),
                (Loc::EnUs, "medium") => format!("{} {}, {}", month_name(loc, mo - 1, true, false), d, y),
                (Loc::EnUs, _) => format!("{}/{}/{:02}", mo, d, y.rem_euclid(100)),
                (Loc::EnGb, "full") => format!("{} {} {} {}", day_name(loc, wd, false, false), d, month_name(loc, mo - 1, false, false), y),
                (Loc::EnGb, "long") => format!("{} {} {}", d, month_name(loc, mo - 1, false, false), y),
                (Loc::EnGb, "medium") => format!("{} {} {}", d, month_name(loc, mo - 1, true, false), y),
                (Loc::EnGb, _) => format!("{:02}/{:02}/{}", d, mo, y),
                (Loc::De, "full") => format!("{}, {}. {} {}", day_name(loc, wd, false, false), d, month_name(loc, mo - 1, false, false), y),
                (Loc::De, "long") => format!("{}. {} {}", d, month_name(loc, mo - 1, false, false), y),
                (Loc::De, "medium") => format!("{:02}.{:02}.{}", d, mo, y),
                (Loc::De, _) => format!("{:02}.{:02}.{:02}", d, mo, y.rem_euclid(100)),
                (Loc::Fr, "full") => format!("{} {} {} {}", day_name(loc, wd, false, false), d, month_name(loc, mo - 1, false, false), y),
                (Loc::Fr, "long") => format!("{} {} {}", d, month_name(loc, mo - 1, false, false), y),
                (Loc::Fr, "medium") => format!("{} {} {}", d, month_name(loc, mo - 1, true, false), y),
                (Loc::Fr, _) => format!("{:02}/{:02}/{}", d, mo, y),
            };
        }
        let mut time = String::new();
        if let Some(ts) = &time_style {
            let sec = if ts == "short" { None } else { Some("2-digit".to_string()) };
            time = time_part(&Some("numeric".into()), &Some("2-digit".into()), &sec);
            if matches!(ts.as_str(), "full" | "long") {
                time.push_str(if ts == "full" && loc == Loc::EnUs { " Coordinated Universal Time" } else { " UTC" });
            }
        }
        out = match (date.is_empty(), time.is_empty()) {
            (false, false) => {
                let sep = match (loc, date_style.as_deref()) {
                    (Loc::EnUs, Some("full") | Some("long")) => " at ",
                    (Loc::EnGb, Some("full") | Some("long")) => " at ",
                    (Loc::De, Some("full") | Some("long")) => " um ",
                    (Loc::Fr, Some("full") | Some("long")) => " à ",
                    (Loc::Fr, _) => " ",
                    _ => ", ",
                };
                format!("{}{}{}", date, sep, time)
            }
            (false, true) => date,
            _ => time,
        };
        return Ok(out);
    }
    if !any_component {
        match defaults {
            DefaultParts::Date => {
                year = Some("numeric".into());
                month = Some("numeric".into());
                day = Some("numeric".into());
            }
            DefaultParts::Time => {
                hour = Some("numeric".into());
                minute = Some("2-digit".into());
                second = Some("2-digit".into());
            }
            DefaultParts::Both => {
                year = Some("numeric".into());
                month = Some("numeric".into());
                day = Some("numeric".into());
                hour = Some("numeric".into());
                minute = Some("2-digit".into());
                second = Some("2-digit".into());
            }
        }
    }
    // date part
    let num = |v: usize, style: &Option<String>| -> String {
        if style.as_deref() == Some("2-digit") {
            format!("{:02}", v)
        } else {
            v.to_string()
        }
    };
    let yr = |style: &Option<String>| -> String {
        if style.as_deref() == Some("2-digit") {
            format!("{:02}", y.rem_euclid(100))
        } else {
            y.to_string()
        }
    };
    let mut date = String::new();
    let textual_month = matches!(month.as_deref(), Some("long") | Some("short") | Some("narrow"));
    if year.is_some() || month.is_some() || day.is_some() {
        if textual_month {
            let mname = month_name(loc, mo - 1, month.as_deref() == Some("short"), month.as_deref() == Some("narrow"));
            let dd = day.as_ref().map(|_| num(d, &day));
            let yy = year.as_ref().map(|_| yr(&year));
            date = match loc {
                Loc::EnUs => match (dd, yy) {
                    (Some(dd), Some(yy)) => format!("{} {}, {}", mname, dd, yy),
                    (Some(dd), None) => format!("{} {}", mname, dd),
                    (None, Some(yy)) => format!("{} {}", mname, yy),
                    (None, None) => mname,
                },
                Loc::EnGb | Loc::Fr => [dd, Some(mname), yy].into_iter().flatten().collect::<Vec<_>>().join(" "),
                Loc::De => match (dd, yy) {
                    (Some(dd), Some(yy)) => format!("{}. {} {}", dd, mname, yy),
                    (Some(dd), None) => format!("{}. {}", dd, mname),
                    (None, Some(yy)) => format!("{} {}", mname, yy),
                    (None, None) => mname,
                },
            };
        } else {
            let mm = month.as_ref().map(|_| num(mo, &month));
            let dd = day.as_ref().map(|_| num(d, &day));
            let yy = year.as_ref().map(|_| yr(&year));
            date = match loc {
                Loc::EnUs => [mm, dd, yy].into_iter().flatten().collect::<Vec<_>>().join("/"),
                Loc::EnGb | Loc::Fr => {
                    let mm = month.as_ref().map(|_| format!("{:02}", mo));
                    let dd = day.as_ref().map(|_| format!("{:02}", d));
                    [dd, mm, yy].into_iter().flatten().collect::<Vec<_>>().join("/")
                }
                Loc::De => {
                    let parts: Vec<String> = [dd, mm, yy].into_iter().flatten().collect();
                    if year.is_none() && parts.len() == 2 {
                        format!("{}.", parts.join("."))
                    } else {
                        parts.join(".")
                    }
                }
            };
        }
    }
    if let Some(w) = &weekday {
        let wn = day_name(loc, wd, w == "short", w == "narrow");
        date = if date.is_empty() {
            wn
        } else {
            match loc {
                Loc::EnUs | Loc::De => format!("{}, {}", wn, date),
                Loc::EnGb | Loc::Fr => format!("{} {}", wn, date),
            }
        };
    }
    let time = if hour.is_some() || minute.is_some() || second.is_some() { time_part(&hour, &minute, &second) } else { String::new() };
    out.push_str(&date);
    if !time.is_empty() {
        if !out.is_empty() {
            out.push_str(if loc == Loc::Fr { " " } else { ", " });
        }
        out.push_str(&time);
    }
    if let Some(tzn) = era_name {
        let name = if tz.eq_ignore_ascii_case("UTC") {
            "UTC".to_string()
        } else if tzn == "short" {
            let h = offset / 3600;
            let m = (offset.abs() % 3600) / 60;
            if m == 0 {
                format!("GMT{:+}", h)
            } else {
                format!("GMT{:+}:{:02}", h, m)
            }
        } else {
            tz.clone()
        };
        out.push(' ');
        out.push_str(&name);
    }
    Ok(out)
}

/// `Number.prototype.toLocaleString(locale, options)`.
pub fn number_to_locale(it: &mut Interp, v: &V, args: &[V], pos: u32) -> R<V> {
    let loc = locale_of(it, args.first());
    let opts = args.get(1).cloned().unwrap_or(V::Undef);
    format_number(it, v, loc, &opts, pos).map(V::string)
}

/// `Date.prototype.toLocale(Date|Time)String(locale, options)`.
pub fn date_to_locale(it: &mut Interp, ms: f64, which: &str, args: &[V]) -> R<V> {
    let loc = locale_of(it, args.first());
    let opts = args.get(1).cloned().unwrap_or(V::Undef);
    let defaults = match which {
        "toLocaleDateString" => DefaultParts::Date,
        "toLocaleTimeString" => DefaultParts::Time,
        _ => DefaultParts::Both,
    };
    format_date(it, ms, loc, &opts, defaults).map(V::string)
}
