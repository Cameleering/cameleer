//! The DataSonnet `ds` library: the top-level `ds.*` functions and the `ds.arrays`,
//! `ds.binaries`, `ds.crypto` (hashes / HMAC / AES via camel-tls), `ds.jsonpath`, `ds.math`,
//! `ds.numbers`, `ds.objects`, `ds.period`, `ds.strings`, `ds.url` and `ds.xml` modules
//! (`ds.datetime` is in [`crate::dstime`]).
//!
//! Callbacks receive DataSonnet's arguments (`item, index` for arrays, `value, key, index`
//! for objects) as far as their declared parameters go.

use std::cmp::Ordering;
use std::rc::Rc;

use crate::interp::{Interp, R};
use crate::value::*;

/// The `ds` sub-module namespaces.
pub const MODULES: &[&str] = &["ds.arrays", "ds.binaries", "ds.crypto", "ds.jsonpath", "ds.math", "ds.numbers", "ds.objects", "ds.period", "ds.strings", "ds.url", "ds.xml"];

/// `ds.<module>` as a namespace value.
pub fn module(name: &str) -> Option<V> {
    let full = format!("ds.{}", name);
    MODULES.iter().find(|m| **m == full).map(|m| V::host(Host::Ns(m)))
}

fn arg(args: &[V], i: usize) -> V {
    args.get(i).cloned().unwrap_or(V::Null)
}

fn err<X>(it: &Interp, msg: String, pos: u32) -> R<X> {
    it.throw("RUNTIME ERROR", msg, Some(pos))
}

fn s(it: &Interp, v: &V) -> String {
    match v {
        V::Str(x) => x.to_string(),
        other => crate::builtins::jsonnet_to_string(it, other),
    }
}

fn f(v: &V) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

fn num(x: f64) -> V {
    crate::interp::norm_num(x)
}

fn arr(it: &mut Interp, v: &V, fname: &str, pos: u32) -> R<Vec<V>> {
    match v {
        V::Arr(a) => Ok(a.borrow().clone()),
        V::Null => Ok(vec![]),
        other => err(it, format!("ds.{} expected array, got {}", fname, crate::builtins::jsonnet_type(other)), pos),
    }
}

fn entries(it: &mut Interp, v: &V, fname: &str, pos: u32) -> R<Vec<(String, V)>> {
    match v {
        V::Obj(o) => {
            let keys: Vec<String> = it.keys_of_pub(v).iter().map(|k| it.key_string(k)).collect();
            let mut out = Vec::new();
            for k in keys {
                let x = it.get_member(v, &k, pos)?;
                out.push((k, x));
            }
            let _ = o;
            Ok(out)
        }
        V::Null => Ok(vec![]),
        other => err(it, format!("ds.{} expected object, got {}", fname, crate::builtins::jsonnet_type(other)), pos),
    }
}

fn obj(pairs: Vec<(String, V)>) -> V {
    let mut p = PropMap::default();
    for (k, v) in pairs {
        p.set(&k, v);
    }
    V::obj(p)
}

/// Call a callback with as many of the arguments as it declares.
fn cb(it: &mut Interp, func: &V, args: Vec<V>, pos: u32) -> R<V> {
    let n = crate::builtins::arity(func).max(1);
    let a: Vec<V> = args.into_iter().take(n).collect();
    it.call(func, V::Undef, a, pos)
}

fn truthy(v: &V) -> bool {
    matches!(v, V::Bool(true))
}

fn cmp(it: &Interp, a: &V, b: &V) -> Ordering {
    crate::builtins::natural(it, a, b)
}

fn regex(it: &mut Interp, p: &V, pos: u32) -> R<Rc<RegexVal>> {
    crate::builtins::to_regex(it, p, pos)
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(data: &[u8]) -> String {
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                out.push(B64[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn unbase64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32;
        buf = buf << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn words(s: &str) -> Vec<String> {
    // split camelCase, snake_case, kebab-case and spaces into lower-case words
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' || c.is_whitespace() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c.is_uppercase() && !cur.is_empty() && (chars.get(i.wrapping_sub(1)).is_some_and(|p| p.is_lowercase() || p.is_ascii_digit()) || chars.get(i + 1).is_some_and(|n| n.is_lowercase())) {
            out.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn capitalize_word(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn pluralize(w: &str) -> String {
    let lower = w.to_lowercase();
    let irregular = [("person", "people"), ("man", "men"), ("woman", "women"), ("child", "children"), ("mouse", "mice"), ("tooth", "teeth"), ("foot", "feet"), ("goose", "geese"), ("ox", "oxen")];
    if let Some((_, p)) = irregular.iter().find(|(s, _)| *s == lower) {
        return p.to_string();
    }
    if ["sheep", "fish", "series", "species", "money", "rice", "information", "equipment"].contains(&lower.as_str()) {
        return w.to_string();
    }
    if lower.ends_with('y') && !lower.ends_with("ay") && !lower.ends_with("ey") && !lower.ends_with("oy") && !lower.ends_with("uy") {
        return format!("{}ies", &w[..w.len() - 1]);
    }
    if lower.ends_with("fe") {
        return format!("{}ves", &w[..w.len() - 2]);
    }
    if lower.ends_with('f') && !lower.ends_with("ff") {
        return format!("{}ves", &w[..w.len() - 1]);
    }
    if lower.ends_with('s') || lower.ends_with('x') || lower.ends_with('z') || lower.ends_with("ch") || lower.ends_with("sh") {
        return format!("{}es", w);
    }
    format!("{}s", w)
}

fn singularize(w: &str) -> String {
    let lower = w.to_lowercase();
    let irregular = [("people", "person"), ("men", "man"), ("women", "woman"), ("children", "child"), ("mice", "mouse"), ("teeth", "tooth"), ("feet", "foot"), ("geese", "goose"), ("oxen", "ox")];
    if let Some((_, p)) = irregular.iter().find(|(s, _)| *s == lower) {
        return p.to_string();
    }
    if ["sheep", "fish", "series", "species", "money", "rice", "information", "equipment"].contains(&lower.as_str()) {
        return w.to_string();
    }
    if lower.ends_with("ies") && w.len() > 3 {
        return format!("{}y", &w[..w.len() - 3]);
    }
    if lower.ends_with("ves") {
        return format!("{}f", &w[..w.len() - 3]);
    }
    for suf in ["ches", "shes", "sses", "xes", "zes"] {
        if lower.ends_with(suf) {
            return w[..w.len() - 2].to_string();
        }
    }
    if lower.ends_with('s') && !lower.ends_with("ss") {
        return w[..w.len() - 1].to_string();
    }
    w.to_string()
}

fn ordinalize(n: i64) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{}{}", n, suffix)
}

fn hash_bytes(alg: &str, data: &[u8]) -> Option<Vec<u8>> {
    use camel_tls::crypto::hash::{Digest, HashAlg};
    let a = alg.to_ascii_uppercase().replace(['-', '_'], "");
    Some(match a.as_str() {
        "MD5" => camel_tls::crypto::md5::Md5::digest(data),
        "SHA1" | "SHA" => HashAlg::Sha1.hash(data),
        "SHA256" => HashAlg::Sha256.hash(data),
        "SHA384" => HashAlg::Sha384.hash(data),
        "SHA512" => HashAlg::Sha512.hash(data),
        _ => return None,
    })
}

fn hmac_bytes(alg: &str, key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    use camel_tls::crypto::hash::HashAlg;
    let a = alg.to_ascii_uppercase().replace(['-', '_'], "");
    let a = a.strip_prefix("HMAC").unwrap_or(&a).to_string();
    Some(match a.as_str() {
        "MD5" => camel_tls::crypto::hash::hmac::<camel_tls::crypto::md5::Md5>(key, data),
        "SHA1" => HashAlg::Sha1.hmac(key, data),
        "SHA256" => HashAlg::Sha256.hmac(key, data),
        "SHA384" => HashAlg::Sha384.hmac(key, data),
        "SHA512" => HashAlg::Sha512.hmac(key, data),
        _ => return None,
    })
}

/// AES key from a secret: used as is when 16/24/32 bytes long, otherwise its SHA-256 digest.
fn aes_key(secret: &str) -> Vec<u8> {
    let b = secret.as_bytes();
    if matches!(b.len(), 16 | 24 | 32) {
        b.to_vec()
    } else {
        camel_tls::crypto::hash::HashAlg::Sha256.hash(b)
    }
}

fn period_str(y: i64, m: i64, d: i64) -> String {
    if y == 0 && m == 0 && d == 0 {
        return "P0D".into();
    }
    let mut s = String::from("P");
    if y != 0 {
        s.push_str(&format!("{}Y", y));
    }
    if m != 0 {
        s.push_str(&format!("{}M", m));
    }
    if d != 0 {
        s.push_str(&format!("{}D", d));
    }
    s
}

fn duration_str(total_secs: f64) -> String {
    if total_secs == 0.0 {
        return "PT0S".into();
    }
    let neg = total_secs < 0.0;
    let t = total_secs.abs();
    let h = (t / 3600.0).floor() as i64;
    let m = ((t - h as f64 * 3600.0) / 60.0).floor() as i64;
    let sec = t - h as f64 * 3600.0 - m as f64 * 60.0;
    let sg = if neg { "-" } else { "" };
    let mut s = String::from("PT");
    if h != 0 {
        s.push_str(&format!("{}{}H", sg, h));
    }
    if m != 0 {
        s.push_str(&format!("{}{}M", sg, m));
    }
    if sec != 0.0 {
        s.push_str(&format!("{}{}S", sg, crate::interp::js_num_str(sec)));
    }
    s
}

fn date_parts(it: &Interp, v: &V) -> Option<(i64, u32, u32)> {
    let t = s(it, v);
    let d = t.get(..10)?;
    let y = d.get(..4)?.parse().ok()?;
    let m = d.get(5..7)?.parse().ok()?;
    let dd = d.get(8..10)?.parse().ok()?;
    Some((y, m, dd))
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// `ds.read` / `ds.write` media type conversions.
fn read_media(it: &mut Interp, data: &V, mime: &str, params: &V, pos: u32) -> R<V> {
    let mut mt = mime.to_string();
    if let V::Obj(o) = params {
        for (k, v) in o.borrow().props.iter() {
            mt.push_str(&format!("; {}={}", k, crate::builtins::jsonnet_to_string(it, v)));
        }
    }
    let body = camel_core::value::Value::String(s(it, data));
    match crate::media::read_payload(&body, &mt, &Default::default()) {
        Ok(v) => Ok(v),
        Err(e) => err(it, e.message, pos),
    }
}

fn write_media(it: &mut Interp, data: &V, mime: &str, params: &V, pos: u32) -> R<V> {
    let mut mt = mime.to_string();
    if let V::Obj(o) = params {
        for (k, v) in o.borrow().props.iter() {
            mt.push_str(&format!("; {}={}", k, crate::builtins::jsonnet_to_string(it, v)));
        }
    }
    let out = to_value(data);
    match crate::media::write_output(it, data, out, &mt, &Default::default()) {
        Ok(camel_core::value::Value::String(x)) => Ok(V::string(x)),
        Ok(other) => Ok(from_value(&other)),
        Err(e) => err(it, e.message, pos),
    }
}

/// `ds.<name>(args)` / `ds.<module>.<name>(args)`.
pub fn call(it: &mut Interp, ns: &str, name: &str, args: Vec<V>, pos: u32) -> R<V> {
    let a0 = arg(&args, 0);
    let a1 = arg(&args, 1);
    let a2 = arg(&args, 2);
    let a3 = arg(&args, 3);
    let module = ns.strip_prefix("ds.").unwrap_or("");
    Ok(match (module, name) {
        // ================================================================ ds.*
        ("", "append") => {
            let mut v = arr(it, &a0, name, pos)?;
            v.push(a1);
            V::arr(v)
        }
        ("", "prepend") => {
            let mut v = arr(it, &a0, name, pos)?;
            v.insert(0, a1);
            V::arr(v)
        }
        ("", "combine") => match (&a0, &a1) {
            (V::Arr(x), V::Arr(y)) => {
                let mut v = x.borrow().clone();
                v.extend(y.borrow().iter().cloned());
                V::arr(v)
            }
            (V::Obj(_), V::Obj(_)) => {
                let mut pairs = entries(it, &a0, name, pos)?;
                for (k, v) in entries(it, &a1, name, pos)? {
                    match pairs.iter_mut().find(|(x, _)| *x == k) {
                        Some(p) => p.1 = v,
                        None => pairs.push((k, v)),
                    }
                }
                obj(pairs)
            }
            (V::Arr(_), _) | (_, V::Arr(_)) | (V::Obj(_), _) | (_, V::Obj(_)) => return err(it, "ds.combine: cannot combine an array or object with a scalar".into(), pos),
            _ => V::string(format!("{}{}", s(it, &a0), s(it, &a1))),
        },
        ("", "contains") => match &a0 {
            V::Arr(x) => V::Bool(x.borrow().iter().any(|e| crate::interp::deep_eq(e, &a1))),
            V::Str(st) => match &a1 {
                V::Regex(_) => V::Bool(regex(it, &a1, pos)?.re.is_match(st)),
                other => V::Bool(st.contains(&s(it, other))),
            },
            V::Null => V::Bool(false),
            _ => return err(it, "ds.contains expects an array or a string".into(), pos),
        },
        ("", "distinctBy") => match &a0 {
            V::Obj(_) => {
                let mut seen: Vec<V> = vec![];
                let mut out = vec![];
                for (k, v) in entries(it, &a0, name, pos)? {
                    let key = cb(it, &a1, vec![v.clone(), V::string(k.clone())], pos)?;
                    if !seen.iter().any(|x| crate::interp::deep_eq(x, &key)) {
                        seen.push(key);
                        out.push((k, v));
                    }
                }
                obj(out)
            }
            _ => {
                let mut seen: Vec<V> = vec![];
                let mut out = vec![];
                for (i, x) in arr(it, &a0, name, pos)?.into_iter().enumerate() {
                    let key = cb(it, &a1, vec![x.clone(), V::Int(i as i64)], pos)?;
                    if !seen.iter().any(|y| crate::interp::deep_eq(y, &key)) {
                        seen.push(key);
                        out.push(x);
                    }
                }
                V::arr(out)
            }
        },
        ("", "endsWith") => V::Bool(s(it, &a0).to_lowercase().ends_with(&s(it, &a1).to_lowercase())),
        ("", "startsWith") => V::Bool(s(it, &a0).to_lowercase().starts_with(&s(it, &a1).to_lowercase())),
        ("", "entriesOf") => V::arr(
            entries(it, &a0, name, pos)?
                .into_iter()
                .map(|(k, v)| obj(vec![("key".into(), V::string(k)), ("value".into(), v)]))
                .collect(),
        ),
        ("", "filter") => {
            let mut out = vec![];
            for (i, x) in arr(it, &a0, name, pos)?.into_iter().enumerate() {
                if truthy(&cb(it, &a1, vec![x.clone(), V::Int(i as i64)], pos)?) {
                    out.push(x);
                }
            }
            V::arr(out)
        }
        ("", "filterObject") => {
            let mut out = vec![];
            for (i, (k, v)) in entries(it, &a0, name, pos)?.into_iter().enumerate() {
                if truthy(&cb(it, &a1, vec![v.clone(), V::string(k.clone()), V::Int(i as i64)], pos)?) {
                    out.push((k, v));
                }
            }
            obj(out)
        }
        ("", "find") => match &a0 {
            V::Str(st) => {
                let needle = s(it, &a1);
                let chars: Vec<char> = st.chars().collect();
                let nc: Vec<char> = needle.chars().collect();
                let mut out = vec![];
                if !nc.is_empty() {
                    for i in 0..chars.len() {
                        if chars[i..].starts_with(&nc) {
                            out.push(V::Int(i as i64));
                        }
                    }
                }
                V::arr(out)
            }
            _ => V::arr(arr(it, &a0, name, pos)?.iter().enumerate().filter(|(_, x)| crate::interp::deep_eq(x, &a1)).map(|(i, _)| V::Int(i as i64)).collect()),
        },
        ("", "flatMap") => {
            let mut out = vec![];
            for (i, x) in arr(it, &a0, name, pos)?.into_iter().enumerate() {
                match cb(it, &a1, vec![x, V::Int(i as i64)], pos)? {
                    V::Arr(a) => out.extend(a.borrow().iter().cloned()),
                    other => out.push(other),
                }
            }
            V::arr(out)
        }
        ("", "flatten") => {
            let mut out = vec![];
            for x in arr(it, &a0, name, pos)? {
                match x {
                    V::Arr(a) => out.extend(a.borrow().iter().cloned()),
                    V::Null => {}
                    other => out.push(other),
                }
            }
            V::arr(out)
        }
        ("", "foldLeft") => {
            let mut acc = a1.clone();
            for x in arr(it, &a0, name, pos)? {
                acc = it.call(&a2, V::Undef, vec![x, acc], pos)?;
            }
            acc
        }
        ("", "reduce") => {
            let items = arr(it, &a0, name, pos)?;
            let mut iter = items.into_iter();
            let mut acc = match (args.len() > 2, iter.next()) {
                (true, first) => {
                    let mut acc = a2.clone();
                    if let Some(f0) = first {
                        acc = it.call(&a1, V::Undef, vec![f0, acc], pos)?;
                    }
                    acc
                }
                (false, Some(f0)) => f0,
                (false, None) => return Ok(V::Null),
            };
            for x in iter {
                acc = it.call(&a1, V::Undef, vec![x, acc], pos)?;
            }
            acc
        }
        ("", "groupBy") => {
            let mut groups: Vec<(String, Vec<(String, V)>)> = vec![];
            let is_obj = matches!(a0, V::Obj(_));
            let items: Vec<(String, V)> = if is_obj { entries(it, &a0, name, pos)? } else { arr(it, &a0, name, pos)?.into_iter().enumerate().map(|(i, x)| (i.to_string(), x)).collect() };
            for (i, (k, x)) in items.into_iter().enumerate() {
                let key = if is_obj { cb(it, &a1, vec![x.clone(), V::string(k.clone())], pos)? } else { cb(it, &a1, vec![x.clone(), V::Int(i as i64)], pos)? };
                let ks = s(it, &key);
                match groups.iter_mut().find(|(g, _)| *g == ks) {
                    Some((_, l)) => l.push((k, x)),
                    None => groups.push((ks, vec![(k, x)])),
                }
            }
            obj(groups.into_iter().map(|(g, l)| (g, if is_obj { obj(l) } else { V::arr(l.into_iter().map(|(_, x)| x).collect()) })).collect())
        }
        ("", "isBlank") => V::Bool(a0.is_nullish() || s(it, &a0).trim().is_empty()),
        ("", "isDecimal") => V::Bool(a0.is_number() && f(&a0).fract() != 0.0),
        ("", "isInteger") => V::Bool(a0.is_number() && f(&a0).fract() == 0.0),
        ("", "isEven") => V::Bool(f(&a0) % 2.0 == 0.0),
        ("", "isOdd") => V::Bool(f(&a0).abs() % 2.0 == 1.0),
        ("", "isEmpty") => V::Bool(match &a0 {
            V::Null | V::Undef => true,
            V::Str(x) => x.is_empty(),
            V::Arr(a) => a.borrow().is_empty(),
            V::Obj(o) => o.borrow().props.is_empty(),
            _ => false,
        }),
        ("", "joinBy") => {
            let items = arr(it, &a0, name, pos)?;
            V::string(items.iter().map(|x| s(it, x)).collect::<Vec<_>>().join(&s(it, &a1)))
        }
        ("", "keysOf") => V::arr(entries(it, &a0, name, pos)?.into_iter().map(|(k, _)| V::string(k)).collect()),
        ("", "valuesOf") => V::arr(entries(it, &a0, name, pos)?.into_iter().map(|(_, v)| v).collect()),
        ("", "lower") => V::string(s(it, &a0).to_lowercase()),
        ("", "upper") => V::string(s(it, &a0).to_uppercase()),
        ("", "trim") => V::string(s(it, &a0).trim().to_string()),
        ("", "map") => {
            let mut out = vec![];
            for (i, x) in arr(it, &a0, name, pos)?.into_iter().enumerate() {
                out.push(cb(it, &a1, vec![x, V::Int(i as i64)], pos)?);
            }
            V::arr(out)
        }
        ("", "mapEntries") => {
            let mut out = vec![];
            for (i, (k, v)) in entries(it, &a0, name, pos)?.into_iter().enumerate() {
                out.push(cb(it, &a1, vec![v, V::string(k), V::Int(i as i64)], pos)?);
            }
            V::arr(out)
        }
        ("", "mapObject") => {
            let mut pairs: Vec<(String, V)> = vec![];
            for (i, (k, v)) in entries(it, &a0, name, pos)?.into_iter().enumerate() {
                let r = cb(it, &a1, vec![v, V::string(k), V::Int(i as i64)], pos)?;
                for (rk, rv) in entries(it, &r, name, pos)? {
                    match pairs.iter_mut().find(|(x, _)| *x == rk) {
                        Some(p) => p.1 = rv,
                        None => pairs.push((rk, rv)),
                    }
                }
            }
            obj(pairs)
        }
        ("", "match") => {
            let st = s(it, &a0);
            let re = regex(it, &a1, pos)?;
            match re.re.full_captures(&st) {
                Some(c) => V::arr(c.to_vec().into_iter().map(|g| g.map(V::string).unwrap_or(V::Null)).collect()),
                None => V::arr(vec![]),
            }
        }
        ("", "matches") => {
            let st = s(it, &a0);
            V::Bool(regex(it, &a1, pos)?.re.is_full_match(&st))
        }
        ("", "scan") => {
            let st = s(it, &a0);
            let re = regex(it, &a1, pos)?;
            V::arr(re.re.captures_iter(&st).iter().map(|c| V::arr(c.to_vec().into_iter().map(|g| g.map(V::string).unwrap_or(V::Null)).collect())).collect())
        }
        ("", "max") | ("", "min") => {
            let items = arr(it, &a0, name, pos)?;
            let mut best: Option<V> = None;
            for x in items {
                let better = match &best {
                    None => true,
                    Some(b) => {
                        let o = cmp(it, &x, b);
                        if name == "max" {
                            o == Ordering::Greater
                        } else {
                            o == Ordering::Less
                        }
                    }
                };
                if better {
                    best = Some(x);
                }
            }
            best.unwrap_or(V::Null)
        }
        ("", "maxBy") | ("", "minBy") => {
            let items = arr(it, &a0, name, pos)?;
            let mut best: Option<(V, V)> = None;
            for x in items {
                let k = cb(it, &a1, vec![x.clone()], pos)?;
                let better = match &best {
                    None => true,
                    Some((bk, _)) => {
                        let o = cmp(it, &k, bk);
                        if name == "maxBy" {
                            o == Ordering::Greater
                        } else {
                            o == Ordering::Less
                        }
                    }
                };
                if better {
                    best = Some((k, x));
                }
            }
            best.map(|(_, x)| x).unwrap_or(V::Null)
        }
        ("", "orderBy") => match &a0 {
            V::Obj(_) => {
                let mut keyed = vec![];
                for (k, v) in entries(it, &a0, name, pos)? {
                    let key = cb(it, &a1, vec![v.clone(), V::string(k.clone())], pos)?;
                    keyed.push((key, k, v));
                }
                keyed.sort_by(|a, b| cmp(it, &a.0, &b.0));
                obj(keyed.into_iter().map(|(_, k, v)| (k, v)).collect())
            }
            _ => {
                let mut keyed = vec![];
                for (i, x) in arr(it, &a0, name, pos)?.into_iter().enumerate() {
                    let key = cb(it, &a1, vec![x.clone(), V::Int(i as i64)], pos)?;
                    keyed.push((key, x));
                }
                keyed.sort_by(|a, b| cmp(it, &a.0, &b.0));
                V::arr(keyed.into_iter().map(|(_, x)| x).collect())
            }
        },
        ("", "parseDouble") => match s(it, &a0).trim().parse::<f64>() {
            Ok(x) => num(x),
            Err(_) => return err(it, format!("ds.parseDouble: invalid number '{}'", s(it, &a0)), pos),
        },
        ("", "parseInt") => match s(it, &a0).trim().parse::<f64>() {
            Ok(x) => V::Int(x as i64),
            Err(_) => return err(it, format!("ds.parseInt: invalid number '{}'", s(it, &a0)), pos),
        },
        ("", "parseHex") => V::Int(i64::from_str_radix(s(it, &a0).trim().trim_start_matches("0x"), 16).unwrap_or(0)),
        ("", "parseOctal") => V::Int(i64::from_str_radix(s(it, &a0).trim(), 8).unwrap_or(0)),
        ("", "range") => {
            let (a, b) = (f(&a0) as i64, f(&a1) as i64);
            V::arr((a..=b).map(V::Int).collect())
        }
        ("", "read") => read_media(it, &a0, &s(it, &a1), &a2, pos)?,
        ("", "write") => write_media(it, &a0, &s(it, &a1), &a2, pos)?,
        ("", "readUrl") => {
            let url = s(it, &a0);
            let path = url.strip_prefix("classpath://").or_else(|| url.strip_prefix("classpath:")).map(|p| p.trim_start_matches('/').to_string()).unwrap_or_else(|| url.strip_prefix("file://").or_else(|| url.strip_prefix("file:")).unwrap_or(&url).to_string());
            let file = crate::jobj::resolve_import(&path).unwrap_or_else(|| std::path::PathBuf::from(&path));
            match std::fs::read_to_string(&file) {
                Ok(t) => crate::json::parse(&t).unwrap_or_else(|_| V::string(t)),
                Err(e) => return err(it, format!("ds.readUrl: cannot read {}: {}", url, e), pos),
            }
        }
        ("", "remove") => match &a0 {
            V::Obj(_) => {
                let drop: Vec<String> = match &a1 {
                    V::Arr(a) => a.borrow().iter().map(|x| s(it, x)).collect(),
                    other => vec![s(it, other)],
                };
                obj(entries(it, &a0, name, pos)?.into_iter().filter(|(k, _)| !drop.contains(k)).collect())
            }
            _ => {
                let items = arr(it, &a0, name, pos)?;
                let drop: Vec<V> = match &a1 {
                    V::Arr(a) => a.borrow().clone(),
                    other => vec![other.clone()],
                };
                V::arr(items.into_iter().filter(|x| !drop.iter().any(|d| crate::interp::deep_eq(x, d))).collect())
            }
        },
        ("", "removeMatch") => match &a0 {
            V::Obj(_) => {
                let other = entries(it, &a1, name, pos)?;
                obj(entries(it, &a0, name, pos)?.into_iter().filter(|(k, v)| !other.iter().any(|(ok, ov)| ok == k && crate::interp::deep_eq(ov, v))).collect())
            }
            _ => {
                let other = arr(it, &a1, name, pos)?;
                V::arr(arr(it, &a0, name, pos)?.into_iter().filter(|x| !other.iter().any(|o| crate::interp::deep_eq(o, x))).collect())
            }
        },
        ("", "replace") => {
            let st = s(it, &a0);
            let re = regex(it, &a1, pos)?;
            V::string(re.re.replace_all(&st, &s(it, &a2)))
        }
        ("", "select") => {
            let path = s(it, &a1);
            let mut cur = a0.clone();
            for part in path.split('.').filter(|p| !p.is_empty()) {
                cur = match &cur {
                    V::Obj(_) => match it.get_member(&cur, part, pos) {
                        Ok(v) => v,
                        Err(_) => V::Null,
                    },
                    V::Arr(a) => part.parse::<usize>().ok().and_then(|i| a.borrow().get(i).cloned()).unwrap_or(V::Null),
                    _ => V::Null,
                };
            }
            cur
        }
        ("", "sizeOf") => V::Int(match &a0 {
            V::Str(x) => x.chars().count() as i64,
            V::Arr(a) => a.borrow().len() as i64,
            V::Obj(o) => o.borrow().props.len() as i64,
            V::Null => 0,
            V::Func(c) => c.def.params.len() as i64,
            _ => return err(it, "ds.sizeOf expects a string, array, object or function".into(), pos),
        }),
        ("", "splitBy") => {
            let st = s(it, &a0);
            match &a1 {
                V::Regex(_) => V::arr(regex(it, &a1, pos)?.re.split(&st).into_iter().map(V::string).collect()),
                other => {
                    let sep = s(it, other);
                    let re = camel_core::regex::Regex::new(&sep).ok();
                    match re {
                        Some(r) if sep.chars().count() > 1 && sep.chars().any(|c| "\\[]().*+?|^$".contains(c)) => V::arr(r.split(&st).into_iter().map(V::string).collect()),
                        _ => V::arr(st.split(sep.as_str()).map(V::str).collect()),
                    }
                }
            }
        }
        ("", "toString") => V::string(s(it, &a0)),
        ("", "typeOf") => V::str(crate::builtins::jsonnet_type(&a0)),
        ("", "unzip") => {
            let rows = arr(it, &a0, name, pos)?;
            let rows: Vec<Vec<V>> = rows.iter().map(|r| if let V::Arr(a) = r { a.borrow().clone() } else { vec![] }).collect();
            let n = rows.iter().map(|r| r.len()).min().unwrap_or(0);
            V::arr((0..n).map(|i| V::arr(rows.iter().map(|r| r[i].clone()).collect())).collect())
        }
        ("", "zip") => {
            let (x, y) = (arr(it, &a0, name, pos)?, arr(it, &a1, name, pos)?);
            V::arr(x.into_iter().zip(y).map(|(p, q)| V::arr(vec![p, q])).collect())
        }
        ("", "uuid") => V::string(camel_core::language::support::uuid_v4()),
        // ================================================================ ds.arrays
        ("arrays", "countBy") => {
            let mut n = 0;
            for x in arr(it, &a0, name, pos)? {
                if truthy(&cb(it, &a1, vec![x], pos)?) {
                    n += 1;
                }
            }
            V::Int(n)
        }
        ("arrays", "deepFlatten") => {
            fn flat(v: &V, out: &mut Vec<V>) {
                match v {
                    V::Arr(a) => a.borrow().iter().for_each(|x| flat(x, out)),
                    other => out.push(other.clone()),
                }
            }
            let mut out = vec![];
            flat(&a0, &mut out);
            V::arr(out)
        }
        ("arrays", "divideBy") => {
            let items = arr(it, &a0, name, pos)?;
            let n = (f(&a1) as usize).max(1);
            V::arr(items.chunks(n).map(|c| V::arr(c.to_vec())).collect())
        }
        ("arrays", "drop") => V::arr(arr(it, &a0, name, pos)?.into_iter().skip(f(&a1).max(0.0) as usize).collect()),
        ("arrays", "take") => V::arr(arr(it, &a0, name, pos)?.into_iter().take(f(&a1).max(0.0) as usize).collect()),
        ("arrays", "dropWhile") | ("arrays", "takeWhile") => {
            let items = arr(it, &a0, name, pos)?;
            let mut k = 0;
            while k < items.len() && truthy(&cb(it, &a1, vec![items[k].clone()], pos)?) {
                k += 1;
            }
            V::arr(if name == "dropWhile" { items[k..].to_vec() } else { items[..k].to_vec() })
        }
        ("arrays", "duplicates") => {
            let items = arr(it, &a0, name, pos)?;
            let mut out: Vec<V> = vec![];
            for (i, x) in items.iter().enumerate() {
                if items[..i].iter().any(|y| crate::interp::deep_eq(x, y)) && !out.iter().any(|y| crate::interp::deep_eq(x, y)) {
                    out.push(x.clone());
                }
            }
            V::arr(out)
        }
        ("arrays", "every") | ("arrays", "some") => {
            let items = arr(it, &a0, name, pos)?;
            let mut r = name == "every";
            for x in items {
                let t = truthy(&cb(it, &a1, vec![x], pos)?);
                if name == "every" && !t {
                    r = false;
                    break;
                }
                if name == "some" && t {
                    r = true;
                    break;
                }
            }
            V::Bool(r)
        }
        ("arrays", "firstWith") => {
            let mut found = V::Null;
            for (i, x) in arr(it, &a0, name, pos)?.into_iter().enumerate() {
                if truthy(&cb(it, &a1, vec![x.clone(), V::Int(i as i64)], pos)?) {
                    found = x;
                    break;
                }
            }
            found
        }
        ("arrays", "indexOf") => V::Int(arr(it, &a0, name, pos)?.iter().position(|x| crate::interp::deep_eq(x, &a1)).map(|i| i as i64).unwrap_or(-1)),
        ("arrays", "lastIndexOf") => V::Int(arr(it, &a0, name, pos)?.iter().rposition(|x| crate::interp::deep_eq(x, &a1)).map(|i| i as i64).unwrap_or(-1)),
        ("arrays", "indexWhere") => {
            let mut r = -1;
            for (i, x) in arr(it, &a0, name, pos)?.into_iter().enumerate() {
                if truthy(&cb(it, &a1, vec![x], pos)?) {
                    r = i as i64;
                    break;
                }
            }
            V::Int(r)
        }
        ("arrays", "join") | ("arrays", "leftJoin") | ("arrays", "outerJoin") => {
            let (left, right) = (arr(it, &a0, name, pos)?, arr(it, &a1, name, pos)?);
            let mut lk = vec![];
            for x in &left {
                lk.push(cb(it, &a2, vec![x.clone()], pos)?);
            }
            let mut rk = vec![];
            for y in &right {
                rk.push(cb(it, &a3, vec![y.clone()], pos)?);
            }
            let mut out = vec![];
            let mut right_used = vec![false; right.len()];
            for (i, x) in left.iter().enumerate() {
                let mut matched = false;
                for (j, y) in right.iter().enumerate() {
                    if crate::interp::deep_eq(&lk[i], &rk[j]) {
                        matched = true;
                        right_used[j] = true;
                        out.push(obj(vec![("l".into(), x.clone()), ("r".into(), y.clone())]));
                    }
                }
                if !matched && name != "join" {
                    out.push(obj(vec![("l".into(), x.clone())]));
                }
            }
            if name == "outerJoin" {
                for (j, y) in right.iter().enumerate() {
                    if !right_used[j] {
                        out.push(obj(vec![("r".into(), y.clone())]));
                    }
                }
            }
            V::arr(out)
        }
        ("arrays", "occurrences") => {
            let mut counts: Vec<(String, i64)> = vec![];
            for x in arr(it, &a0, name, pos)? {
                let k = cb(it, &a1, vec![x], pos)?;
                let ks = s(it, &k);
                match counts.iter_mut().find(|(c, _)| *c == ks) {
                    Some(c) => c.1 += 1,
                    None => counts.push((ks, 1)),
                }
            }
            obj(counts.into_iter().map(|(k, n)| (k, V::Int(n))).collect())
        }
        ("arrays", "partition") => {
            let (mut ok, mut ko) = (vec![], vec![]);
            for x in arr(it, &a0, name, pos)? {
                if truthy(&cb(it, &a1, vec![x.clone()], pos)?) {
                    ok.push(x);
                } else {
                    ko.push(x);
                }
            }
            obj(vec![("success".into(), V::arr(ok)), ("failure".into(), V::arr(ko))])
        }
        ("arrays", "slice") => {
            let items = arr(it, &a0, name, pos)?;
            let (a, b) = ((f(&a1).max(0.0) as usize).min(items.len()), (f(&a2).max(0.0) as usize).min(items.len()));
            V::arr(if a < b { items[a..b].to_vec() } else { vec![] })
        }
        ("arrays", "splitAt") => {
            let items = arr(it, &a0, name, pos)?;
            let k = (f(&a1).max(0.0) as usize).min(items.len());
            obj(vec![("l".into(), V::arr(items[..k].to_vec())), ("r".into(), V::arr(items[k..].to_vec()))])
        }
        ("arrays", "splitWhere") => {
            let items = arr(it, &a0, name, pos)?;
            let mut k = items.len();
            for (i, x) in items.iter().enumerate() {
                if truthy(&cb(it, &a1, vec![x.clone()], pos)?) {
                    k = i;
                    break;
                }
            }
            obj(vec![("l".into(), V::arr(items[..k].to_vec())), ("r".into(), V::arr(items[k..].to_vec()))])
        }
        ("arrays", "sumBy") => {
            let mut total = 0.0;
            for x in arr(it, &a0, name, pos)? {
                total += f(&cb(it, &a1, vec![x], pos)?);
            }
            num(total)
        }
        // ================================================================ ds.binaries
        ("binaries", "fromBase64") => match unbase64(&s(it, &a0)) {
            Some(b) => V::string(String::from_utf8_lossy(&b).into_owned()),
            None => return err(it, "ds.binaries.fromBase64: invalid base64".into(), pos),
        },
        ("binaries", "toBase64") => V::string(base64(s(it, &a0).as_bytes())),
        ("binaries", "fromHex") => match unhex(&s(it, &a0)) {
            Some(b) => V::string(String::from_utf8_lossy(&b).into_owned()),
            None => return err(it, "ds.binaries.fromHex: invalid hex".into(), pos),
        },
        ("binaries", "toHex") => V::string(hex(s(it, &a0).as_bytes()).to_uppercase()),
        ("binaries", "readLinesWith") => V::arr(s(it, &a0).lines().map(V::str).collect()),
        ("binaries", "writeLinesWith") => {
            let items = arr(it, &a0, name, pos)?;
            V::string(items.iter().map(|x| s(it, x) + "\n").collect::<String>())
        }
        // ================================================================ ds.crypto
        ("crypto", "hash") => match hash_bytes(&s(it, &a1), s(it, &a0).as_bytes()) {
            Some(d) => V::string(hex(&d)),
            None => return err(it, format!("ds.crypto.hash: unsupported algorithm {}", s(it, &a1)), pos),
        },
        ("crypto", "hmac") => match hmac_bytes(&s(it, &a2), s(it, &a1).as_bytes(), s(it, &a0).as_bytes()) {
            Some(d) => V::string(hex(&d)),
            None => return err(it, format!("ds.crypto.hmac: unsupported algorithm {}", s(it, &a2)), pos),
        },
        ("crypto", "encrypt") | ("crypto", "decrypt") => {
            use camel_tls::crypto::aes::{cbc_decrypt, cbc_encrypt, Aes};
            let transformation = if a2.is_nullish() { "AES/CBC/PKCS5Padding".to_string() } else { s(it, &a2) };
            let t = transformation.to_ascii_uppercase();
            if !t.starts_with("AES") {
                return err(it, format!("ds.crypto.{}: unsupported transformation {}", name, transformation), pos);
            }
            let aes = Aes::new(&aes_key(&s(it, &a1)));
            let ecb = t.contains("/ECB");
            if name == "encrypt" {
                let data = s(it, &a0);
                let out = if ecb {
                    cbc_encrypt_ecb(&aes, data.as_bytes())
                } else {
                    let iv = camel_tls::crypto::rng::random_bytes(16);
                    let mut o = iv.clone();
                    o.extend(cbc_encrypt(&aes, &iv, data.as_bytes()));
                    o
                };
                V::string(base64(&out))
            } else {
                let Some(raw) = unbase64(&s(it, &a0)) else { return err(it, "ds.crypto.decrypt: invalid base64".into(), pos) };
                let plain = if ecb {
                    cbc_decrypt_ecb(&aes, &raw)
                } else if raw.len() > 16 {
                    cbc_decrypt(&aes, &raw[..16], &raw[16..])
                } else {
                    None
                };
                match plain {
                    Some(p) => V::string(String::from_utf8_lossy(&p).into_owned()),
                    None => return err(it, "ds.crypto.decrypt: bad key or corrupted data".into(), pos),
                }
            }
        }
        // ================================================================ ds.jsonpath
        ("jsonpath", "select") => {
            let path = s(it, &a1);
            match camel_core::language::jsonpath::query(&to_value(&a0), &path) {
                Ok(camel_core::value::Value::Array(a)) => V::arr(a.iter().map(from_value).collect()),
                Ok(camel_core::value::Value::Null) => V::arr(vec![]),
                Ok(other) => V::arr(vec![from_value(&other)]),
                Err(e) => return err(it, format!("ds.jsonpath.select: {}", e.message), pos),
            }
        }
        // ================================================================ ds.math
        ("math", "abs") => num(f(&a0).abs()),
        ("math", "acos") => num(f(&a0).acos()),
        ("math", "asin") => num(f(&a0).asin()),
        ("math", "atan") => num(f(&a0).atan()),
        ("math", "cos") => num(f(&a0).cos()),
        ("math", "sin") => num(f(&a0).sin()),
        ("math", "tan") => num(f(&a0).tan()),
        ("math", "exp") => num(f(&a0).exp()),
        ("math", "log") => num(f(&a0).ln()),
        ("math", "sqrt") => num(f(&a0).sqrt()),
        ("math", "pow") => num(f(&a0).powf(f(&a1))),
        ("math", "ceil") => num(f(&a0).ceil()),
        ("math", "floor") => num(f(&a0).floor()),
        ("math", "mod") => num(f(&a0) % f(&a1)),
        ("math", "round") => {
            let prec = if a1.is_nullish() { 0 } else { f(&a1) as i32 };
            let m = 10f64.powi(prec);
            let x = f(&a0) * m;
            // half up (away from zero), as Java's HALF_UP
            num((x.abs() + 0.5 + 1e-9).floor().copysign(x) / m)
        }
        ("math", "sum") => num(arr(it, &a0, name, pos)?.iter().map(f).sum()),
        ("math", "avg") => {
            let items = arr(it, &a0, name, pos)?;
            if items.is_empty() {
                return err(it, "ds.math.avg of an empty array".into(), pos);
            }
            num(items.iter().map(f).sum::<f64>() / items.len() as f64)
        }
        ("math", "exponent") => {
            let x = f(&a0);
            V::Int(if x == 0.0 { 0 } else { x.abs().log2().floor() as i64 + 1 })
        }
        ("math", "mantissa") => {
            let x = f(&a0);
            if x == 0.0 {
                V::Int(0)
            } else {
                let e = x.abs().log2().floor() + 1.0;
                num(x / 2f64.powf(e))
            }
        }
        ("math", "random") => num((camel_core::language::support::random_u64() >> 11) as f64 / (1u64 << 53) as f64),
        ("math", "randomInt") => V::Int((camel_core::language::support::random_u64() % (f(&a0).max(1.0) as u64)) as i64),
        // ================================================================ ds.numbers
        ("numbers", "fromBinary") => V::Int(i64::from_str_radix(&s(it, &a0), 2).unwrap_or(0)),
        ("numbers", "fromHex") => V::Int(i64::from_str_radix(&s(it, &a0).to_lowercase(), 16).unwrap_or(0)),
        ("numbers", "fromRadixNumber") => V::Int(i64::from_str_radix(&s(it, &a0).to_lowercase(), f(&a1) as u32).unwrap_or(0)),
        ("numbers", "toBinary") => V::string(radix(f(&a0) as i64, 2)),
        ("numbers", "toHex") => V::string(radix(f(&a0) as i64, 16).to_uppercase()),
        ("numbers", "toRadixNumber") => V::string(radix(f(&a0) as i64, f(&a1) as u32)),
        // ================================================================ ds.objects
        ("objects", "divideBy") => {
            let pairs = entries(it, &a0, name, pos)?;
            let n = (f(&a1) as usize).max(1);
            V::arr(pairs.chunks(n).map(|c| obj(c.to_vec())).collect())
        }
        ("objects", "everyEntry") | ("objects", "someEntry") => {
            let every = name == "everyEntry";
            let mut r = every;
            for (k, v) in entries(it, &a0, name, pos)? {
                let t = truthy(&cb(it, &a1, vec![v, V::string(k)], pos)?);
                if every && !t {
                    r = false;
                    break;
                }
                if !every && t {
                    r = true;
                    break;
                }
            }
            V::Bool(r)
        }
        ("objects", "mergeWith") => {
            let mut pairs = entries(it, &a0, name, pos)?;
            for (k, v) in entries(it, &a1, name, pos)? {
                match pairs.iter_mut().find(|(x, _)| *x == k) {
                    Some(p) => p.1 = v,
                    None => pairs.push((k, v)),
                }
            }
            obj(pairs)
        }
        ("objects", "takeWhile") => {
            let mut out = vec![];
            for (k, v) in entries(it, &a0, name, pos)? {
                if !truthy(&cb(it, &a1, vec![v.clone(), V::string(k.clone())], pos)?) {
                    break;
                }
                out.push((k, v));
            }
            obj(out)
        }
        // ================================================================ ds.period
        ("period", "years") => V::string(period_str(f(&a0) as i64, 0, 0)),
        ("period", "months") => V::string(period_str(0, f(&a0) as i64, 0)),
        ("period", "days") => V::string(period_str(0, 0, f(&a0) as i64)),
        ("period", "hours") => V::string(duration_str(f(&a0) * 3600.0)),
        ("period", "minutes") => V::string(duration_str(f(&a0) * 60.0)),
        ("period", "seconds") => V::string(duration_str(f(&a0))),
        ("period", "period") => {
            let g = |it: &mut Interp, k: &str| -> R<i64> {
                Ok(match &a0 {
                    V::Obj(_) => {
                        let v = it.get_member(&a0, k, pos).unwrap_or(V::Null);
                        f(&v).max(f64::MIN) as i64
                    }
                    _ => 0,
                })
            };
            let (y, m, d) = (g(it, "years")?, g(it, "months")?, g(it, "days")?);
            V::string(period_str(y, m, d))
        }
        ("period", "duration") => {
            let g = |it: &mut Interp, k: &str| -> f64 {
                match &a0 {
                    V::Obj(_) => {
                        let v = it.get_member(&a0, k, pos).unwrap_or(V::Null);
                        if v.is_nullish() {
                            0.0
                        } else {
                            f(&v)
                        }
                    }
                    _ => 0.0,
                }
            };
            let total = g(it, "days") * 86400.0 + g(it, "hours") * 3600.0 + g(it, "minutes") * 60.0 + g(it, "seconds");
            V::string(duration_str(total))
        }
        ("period", "between") => {
            let (Some((y1, m1, d1)), Some((y2, m2, d2))) = (date_parts(it, &a0), date_parts(it, &a1)) else {
                return err(it, "ds.period.between expects two dates".into(), pos);
            };
            let mut y = y2 - y1;
            let mut m = m2 as i64 - m1 as i64;
            let mut d = d2 as i64 - d1 as i64;
            if d < 0 {
                m -= 1;
                let (py, pm) = if m2 == 1 { (y2 - 1, 12) } else { (y2, m2 - 1) };
                d += days_in_month(py, pm) as i64;
            }
            if m < 0 {
                y -= 1;
                m += 12;
            }
            V::string(period_str(y, m, d))
        }
        // ================================================================ ds.strings
        ("strings", "appendIfMissing") => {
            let (st, suf) = (s(it, &a0), s(it, &a1));
            V::string(if st.ends_with(&suf) { st } else { st + &suf })
        }
        ("strings", "prependIfMissing") => {
            let (st, pre) = (s(it, &a0), s(it, &a1));
            V::string(if st.starts_with(&pre) { st } else { pre + &st })
        }
        ("strings", "camelize") => {
            let w = words(&s(it, &a0));
            V::string(w.iter().enumerate().map(|(i, x)| if i == 0 { x.clone() } else { capitalize_word(x) }).collect())
        }
        ("strings", "capitalize") => V::string(words(&s(it, &a0)).iter().map(|w| capitalize_word(w)).collect::<Vec<_>>().join(" ")),
        ("strings", "dasherize") => V::string(words(&s(it, &a0)).join("-")),
        ("strings", "underscore") => V::string(words(&s(it, &a0)).join("_")),
        ("strings", "charCode") => V::Int(s(it, &a0).chars().next().map(|c| c as i64).unwrap_or(0)),
        ("strings", "charCodeAt") => V::Int(s(it, &a0).chars().nth(f(&a1) as usize).map(|c| c as i64).unwrap_or(0)),
        ("strings", "fromCharCode") => V::string(char::from_u32(f(&a0) as u32).map(|c| c.to_string()).unwrap_or_default()),
        ("strings", "isAlpha") => V::Bool({
            let st = s(it, &a0);
            !st.is_empty() && st.chars().all(|c| c.is_alphabetic())
        }),
        ("strings", "isAlphanumeric") => V::Bool({
            let st = s(it, &a0);
            !st.is_empty() && st.chars().all(|c| c.is_alphanumeric())
        }),
        ("strings", "isNumeric") => V::Bool({
            let st = s(it, &a0);
            !st.is_empty() && st.chars().all(|c| c.is_ascii_digit())
        }),
        ("strings", "isLowerCase") => V::Bool({
            let st = s(it, &a0);
            st.chars().any(|c| c.is_alphabetic()) && !st.chars().any(|c| c.is_uppercase())
        }),
        ("strings", "isUpperCase") => V::Bool({
            let st = s(it, &a0);
            st.chars().any(|c| c.is_alphabetic()) && !st.chars().any(|c| c.is_lowercase())
        }),
        ("strings", "isWhitespace") => V::Bool(s(it, &a0).chars().all(|c| c.is_whitespace())),
        ("strings", "leftPad") | ("strings", "rightPad") => {
            let st = s(it, &a0);
            let n = f(&a1) as usize;
            let ch = if a2.is_nullish() { " ".to_string() } else { s(it, &a2) };
            let len = st.chars().count();
            if len >= n || ch.is_empty() {
                V::string(st)
            } else {
                let pad: String = ch.chars().cycle().take(n - len).collect();
                V::string(if name == "leftPad" { pad + &st } else { st + &pad })
            }
        }
        ("strings", "ordinalize") => V::string(ordinalize(f(&a0) as i64)),
        ("strings", "pluralize") => V::string(pluralize(&s(it, &a0))),
        ("strings", "singularize") => V::string(singularize(&s(it, &a0))),
        ("strings", "repeat") => V::string(s(it, &a0).repeat(f(&a1).max(0.0) as usize)),
        ("strings", "substringAfter") => {
            let (st, sep) = (s(it, &a0), s(it, &a1));
            V::string(st.find(&sep).map(|i| st[i + sep.len()..].to_string()).unwrap_or_default())
        }
        ("strings", "substringAfterLast") => {
            let (st, sep) = (s(it, &a0), s(it, &a1));
            V::string(st.rfind(&sep).map(|i| st[i + sep.len()..].to_string()).unwrap_or_default())
        }
        ("strings", "substringBefore") => {
            let (st, sep) = (s(it, &a0), s(it, &a1));
            V::string(st.find(&sep).map(|i| st[..i].to_string()).unwrap_or_default())
        }
        ("strings", "substringBeforeLast") => {
            let (st, sep) = (s(it, &a0), s(it, &a1));
            V::string(st.rfind(&sep).map(|i| st[..i].to_string()).unwrap_or_default())
        }
        ("strings", "unwrap") => {
            let (st, w) = (s(it, &a0), s(it, &a1));
            V::string(if st.len() >= 2 * w.len() && st.starts_with(&w) && st.ends_with(&w) && !w.is_empty() { st[w.len()..st.len() - w.len()].to_string() } else { st })
        }
        ("strings", "withMaxSize") => V::string(s(it, &a0).chars().take(f(&a1).max(0.0) as usize).collect()),
        ("strings", "wrapIfMissing") => {
            let (mut st, w) = (s(it, &a0), s(it, &a1));
            if !st.starts_with(&w) {
                st = w.clone() + &st;
            }
            if !st.ends_with(&w) {
                st.push_str(&w);
            }
            V::string(st)
        }
        ("strings", "wrapWith") => {
            let (st, w) = (s(it, &a0), s(it, &a1));
            V::string(format!("{}{}{}", w, st, w))
        }
        // ================================================================ ds.url
        ("url", "encode") => V::string(url_encode(&s(it, &a0))),
        ("url", "decode") => V::string(url_decode(&s(it, &a0))),
        // ================================================================ ds.xml
        ("xml", "flattenContents") => {
            fn flat(it: &Interp, v: &V, out: &mut String) {
                match v {
                    V::Obj(o) => {
                        for (k, x) in o.borrow().props.iter() {
                            if k.starts_with('@') {
                                continue;
                            }
                            flat(it, x, out);
                        }
                    }
                    V::Arr(a) => a.borrow().iter().for_each(|x| flat(it, x, out)),
                    V::Null => {}
                    other => out.push_str(&crate::builtins::jsonnet_to_string(it, other)),
                }
            }
            let mut out = String::new();
            flat(it, &a0, &mut out);
            V::string(out)
        }
        _ => return err(it, format!("field does not exist: {}.{}", ns, name), pos),
    })
}

fn radix(n: i64, r: u32) -> String {
    if n == 0 {
        return "0".into();
    }
    let neg = n < 0;
    let mut u = n.unsigned_abs();
    let mut d = vec![];
    while u > 0 {
        d.push(std::char::from_digit((u % r as u64) as u32, r).unwrap());
        u /= r as u64;
    }
    if neg {
        d.push('-');
    }
    d.iter().rev().collect()
}

fn cbc_encrypt_ecb(aes: &camel_tls::crypto::aes::Aes, data: &[u8]) -> Vec<u8> {
    let pad = 16 - data.len() % 16;
    let mut buf = data.to_vec();
    buf.extend(std::iter::repeat_n(pad as u8, pad));
    for chunk in buf.chunks_mut(16) {
        let b: &mut [u8; 16] = chunk.try_into().unwrap();
        aes.encrypt_block(b);
    }
    buf
}

fn cbc_decrypt_ecb(aes: &camel_tls::crypto::aes::Aes, data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() || data.len() % 16 != 0 {
        return None;
    }
    let mut buf = data.to_vec();
    for chunk in buf.chunks_mut(16) {
        let b: &mut [u8; 16] = chunk.try_into().unwrap();
        aes.decrypt_block(b);
    }
    let pad = *buf.last()? as usize;
    if pad == 0 || pad > 16 || buf.len() < pad {
        return None;
    }
    buf.truncate(buf.len() - pad);
    Some(buf)
}
