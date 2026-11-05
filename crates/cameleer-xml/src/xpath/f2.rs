//! XPath 2.0 functions for the XPath 1.0 data model (used by XSLT 2.0 stylesheets compiled
//! with [`super::XPath::compile_v2`]). Sequences of atomic values are text-node sequences
//! (see [`super::Docs::atomic_sequence`]).
//!
//! Functions (core / `fn:` namespace): `matches`, `replace`, `tokenize` (flags `i s m x q`),
//! `string-join`, `distinct-values`, `exists`, `empty`, `reverse`, `subsequence`,
//! `index-of`, `remove`, `insert-before`, `min`, `max`, `avg`, `abs`,
//! `round-half-to-even`, `compare`, `codepoints-to-string`, `string-to-codepoints`,
//! `normalize-unicode`, `encode-for-uri`, `escape-html-uri`, `iri-to-uri`, `data`,
//! `node-name`, `exactly-one`, `zero-or-one`, `one-or-more`, `deep-equal`, `trace`,
//! `error`, `current-date`, `current-dateTime`, `current-time`, `implicit-timezone`,
//! `year-from-date(Time)`, `month-from-date(Time)`, `day-from-date(Time)`,
//! `hours-/minutes-/seconds-from-dateTime` / `-from-time`, `days-from-duration`...,
//! `format-date`, `format-dateTime`, `format-time` (`[Y0001]`, `[M01]`, `[MNn]`,
//! `[D1]`, `[FNn]`, `[H01]`, `[h1]`, `[m01]`, `[s01]`, `[f001]`, `[P]`, `[Z]`, `[z]`),
//! `base-uri`, `document-uri`, `static-base-uri` (empty), and the `xs:` constructors
//! (`xs:string`, `xs:integer` and subtypes, `xs:decimal`, `xs:double`, `xs:float`,
//! `xs:boolean`, `xs:date`, `xs:dateTime`, `xs:time`, `xs:anyURI`, `xs:untypedAtomic`...).

use cameleer_core::regex::Regex;

use super::{Ctx, XPathError, XValue};
use crate::dom::NodeType;

/// The XPath functions namespace.
pub const FN_NS: &str = "http://www.w3.org/2005/xpath-functions";
/// The XML Schema namespace (constructor functions).
pub const XS_NS: &str = "http://www.w3.org/2001/XMLSchema";

type R<T> = Result<T, XPathError>;

fn err<T>(m: impl Into<String>) -> R<T> {
    Err(XPathError::eval(m.into()))
}

/// The string items of a value (nodes → string values, atomic → one item; empty node-set → none).
pub fn strings(v: &XValue, ctx: &Ctx) -> Vec<String> {
    match v {
        XValue::Nodes(ns) => ns.iter().map(|n| ctx.docs.string_value(*n)).collect(),
        other => vec![other.to_string_value(ctx.docs)],
    }
}

fn count(v: &XValue) -> usize {
    match v {
        XValue::Nodes(ns) => ns.len(),
        _ => 1,
    }
}

fn compile_regex(pattern: &str, flags: &str) -> R<Regex> {
    let mut prefix = String::new();
    let mut pat = pattern.to_string();
    for c in flags.chars() {
        match c {
            'i' | 's' | 'm' | 'x' => prefix.push(c),
            'q' => pat = Regex::quote(pattern),
            _ => return err(format!("Invalid regular expression flags: {}", flags)),
        }
    }
    let full = if prefix.is_empty() { pat } else { format!("(?{}){}", prefix, pat) };
    Regex::new(&full).map_err(|e| XPathError::eval(format!("Invalid regular expression '{}': {}", pattern, e.message)))
}

/// Compile an XPath regular expression with flags (shared with `xsl:analyze-string`).
pub fn regex(pattern: &str, flags: &str) -> Result<Regex, XPathError> {
    compile_regex(pattern, flags)
}

fn num(v: &XValue, ctx: &Ctx) -> f64 {
    v.to_number(ctx.docs)
}

fn fmt_num(x: f64) -> XValue {
    XValue::Num(x)
}

fn round_half_even(x: f64, prec: i32) -> f64 {
    if x.is_nan() || x.is_infinite() {
        return x;
    }
    let m = 10f64.powi(prec);
    let y = x * m;
    let r = y.round();
    let r = if (y - y.trunc()).abs() == 0.5 { 2.0 * (y / 2.0).round() } else { r };
    r / m
}

fn encode_for_uri(s: &str, keep: &dyn Fn(char) -> bool) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if keep(c) {
            out.push(c);
        } else {
            let mut b = [0u8; 4];
            for x in c.encode_utf8(&mut b).bytes() {
                out.push_str(&format!("%{:02X}", x));
            }
        }
    }
    out
}

// ------------------------------------------------------------------ dates

/// A parsed date / time value.
#[derive(Clone, Copy, Debug, Default)]
pub struct DateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub nanos: u32,
    /// Time zone offset in minutes.
    pub tz: Option<i32>,
    pub has_date: bool,
    pub has_time: bool,
}

fn split_tz(s: &str) -> (&str, Option<i32>) {
    if let Some(r) = s.strip_suffix('Z') {
        return (r, Some(0));
    }
    if s.len() > 6 {
        let (a, b) = s.split_at(s.len() - 6);
        let bb = b.as_bytes();
        if (bb[0] == b'+' || bb[0] == b'-') && bb[3] == b':' {
            if let (Ok(h), Ok(m)) = (b[1..3].parse::<i32>(), b[4..6].parse::<i32>()) {
                let t = h * 60 + m;
                return (a, Some(if bb[0] == b'-' { -t } else { t }));
            }
        }
    }
    (s, None)
}

fn parse_time_part(t: &str, dt: &mut DateTime) -> Option<()> {
    let mut p = t.splitn(3, ':');
    dt.hour = p.next()?.parse().ok()?;
    dt.minute = p.next()?.parse().ok()?;
    let s = p.next()?;
    let (w, f) = s.split_once('.').unwrap_or((s, ""));
    dt.second = w.parse().ok()?;
    if !f.is_empty() {
        let digits: String = f.chars().chain("000000000".chars()).take(9).collect();
        dt.nanos = digits.parse().ok()?;
    }
    dt.has_time = true;
    (dt.hour < 25 && dt.minute < 60 && dt.second < 61).then_some(())
}

fn parse_date_part(d: &str, dt: &mut DateTime) -> Option<()> {
    let (neg, d) = match d.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, d),
    };
    let mut p = d.splitn(3, '-');
    let y: i64 = p.next()?.parse().ok()?;
    dt.year = if neg { -y } else { y };
    dt.month = p.next()?.parse().ok()?;
    dt.day = p.next()?.parse().ok()?;
    dt.has_date = true;
    ((1..=12).contains(&dt.month) && (1..=31).contains(&dt.day)).then_some(())
}

/// Parse an `xs:date`, `xs:dateTime` or `xs:time` lexical value.
pub fn parse_date_time(s: &str) -> Option<DateTime> {
    let s = s.trim();
    let mut dt = DateTime::default();
    let (body, tz) = split_tz(s);
    dt.tz = tz;
    if let Some((d, t)) = body.split_once('T') {
        parse_date_part(d, &mut dt)?;
        parse_time_part(t, &mut dt)?;
    } else if body.contains(':') {
        parse_time_part(body, &mut dt)?;
    } else {
        parse_date_part(body, &mut dt)?;
    }
    Some(dt)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn now() -> DateTime {
    let ms = cameleer_core::util_time::now_millis();
    let (y, mo, d, h, mi, s) = cameleer_core::util_time::civil_from_epoch(ms.div_euclid(1000));
    DateTime { year: y, month: mo, day: d, hour: h, minute: mi, second: s, nanos: (ms.rem_euclid(1000) as u32) * 1_000_000, tz: Some(0), has_date: true, has_time: true }
}

fn tz_text(tz: Option<i32>) -> String {
    match tz {
        None => String::new(),
        Some(0) => "Z".into(),
        Some(t) => format!("{}{:02}:{:02}", if t < 0 { '-' } else { '+' }, t.abs() / 60, t.abs() % 60),
    }
}

fn date_text(dt: &DateTime) -> String {
    format!("{}{:04}-{:02}-{:02}", if dt.year < 0 { "-" } else { "" }, dt.year.abs(), dt.month, dt.day)
}

fn time_text(dt: &DateTime) -> String {
    let mut t = format!("{:02}:{:02}:{:02}", dt.hour, dt.minute, dt.second);
    if dt.nanos > 0 {
        let f = format!("{:09}", dt.nanos);
        t.push('.');
        t.push_str(f.trim_end_matches('0'));
    }
    t
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

fn name_case(name: &str, modifier: &str, width: Option<(usize, usize)>) -> String {
    let mut s = name.to_string();
    if let Some((_, max)) = width {
        if max < s.chars().count() {
            s = s.chars().take(max.max(1)).collect();
        }
    } else if modifier.ends_with("n") && modifier.len() == 1 || modifier == "N" {
        // full name
    }
    match modifier.chars().next() {
        Some('N') if modifier.starts_with("Nn") => s,
        Some('N') => s.to_uppercase(),
        Some('n') => s.to_lowercase(),
        _ => s,
    }
}

fn to_alpha(mut n: u64, upper: bool) -> String {
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(b'a' + (n % 26) as u8);
        n /= 26;
    }
    s.reverse();
    let t = String::from_utf8(s).unwrap_or_default();
    if upper {
        t.to_uppercase()
    } else {
        t
    }
}

fn to_roman(n: u64, upper: bool) -> String {
    if n == 0 || n >= 4000 {
        return n.to_string();
    }
    let mut n = n;
    let mut s = String::new();
    for (v, r) in [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")] {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    if upper {
        s.to_uppercase()
    } else {
        s
    }
}

fn fmt_component(value: i64, pres: &str, width: Option<(usize, usize)>) -> String {
    match pres {
        "a" | "A" => to_alpha(value.max(0) as u64, pres == "A"),
        "i" | "I" => to_roman(value.max(0) as u64, pres == "I"),
        "w" => value.to_string(),
        _ => {
            let digits = pres.chars().filter(|c| c.is_ascii_digit()).count().max(1);
            let min = width.map(|w| w.0).unwrap_or(0).max(digits);
            let mut s = value.abs().to_string();
            while s.len() < min {
                s.insert(0, '0');
            }
            if value < 0 {
                s.insert(0, '-');
            }
            s
        }
    }
}

/// `format-date` / `format-dateTime` / `format-time` with an XSLT 2.0 picture string.
pub fn format_date_time(dt: &DateTime, picture: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = picture.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '[' if chars.peek() == Some(&'[') => {
                chars.next();
                out.push('[');
            }
            ']' if chars.peek() == Some(&']') => {
                chars.next();
                out.push(']');
            }
            '[' => {
                let mut spec = String::new();
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                    spec.push(d);
                }
                let spec: String = spec.chars().filter(|c| !c.is_whitespace()).collect();
                let mut it = spec.chars();
                let comp = it.next().ok_or("Empty variable marker in picture string")?;
                let rest: String = it.collect();
                let (pres, width) = match rest.split_once(',') {
                    Some((p, w)) => {
                        let (a, b) = w.split_once('-').unwrap_or((w, ""));
                        let min = if a == "*" { 0 } else { a.parse().unwrap_or(0) };
                        let max = if b.is_empty() || b == "*" { usize::MAX } else { b.parse().unwrap_or(usize::MAX) };
                        (p.to_string(), Some((min, max)))
                    }
                    None => (rest, None),
                };
                let need_date = matches!(comp, 'Y' | 'M' | 'D' | 'd' | 'F' | 'W' | 'w');
                let need_time = matches!(comp, 'H' | 'h' | 'm' | 's' | 'f' | 'P');
                if (need_date && !dt.has_date) || (need_time && !dt.has_time) {
                    return Err(format!("XTDE1350: Component [{}] is not available in this value", comp));
                }
                let default_pres = |d: &str| if pres.is_empty() { d.to_string() } else { pres.clone() };
                let s = match comp {
                    'Y' => {
                        let p = default_pres("1");
                        let mut y = fmt_component(dt.year, &p, width);
                        if p.len() == 2 && p.chars().all(|c| c.is_ascii_digit()) {
                            y = format!("{:02}", dt.year.rem_euclid(100));
                        }
                        y
                    }
                    'M' => {
                        let p = default_pres("1");
                        if p.starts_with('N') || p.starts_with('n') {
                            name_case(MONTHS[(dt.month as usize).saturating_sub(1) % 12], &p, width)
                        } else {
                            fmt_component(dt.month as i64, &p, width)
                        }
                    }
                    'D' => fmt_component(dt.day as i64, &default_pres("1"), width),
                    'd' => {
                        let doy = days_from_civil(dt.year, dt.month, dt.day) - days_from_civil(dt.year, 1, 1) + 1;
                        fmt_component(doy, &default_pres("1"), width)
                    }
                    'F' => {
                        let p = default_pres("n");
                        let wd = (days_from_civil(dt.year, dt.month, dt.day) + 3).rem_euclid(7) as usize; // 0 = Monday
                        if p.starts_with('N') || p.starts_with('n') {
                            name_case(DAYS[wd], &p, width)
                        } else {
                            fmt_component(wd as i64 + 1, &p, width)
                        }
                    }
                    'H' => fmt_component(dt.hour as i64, &default_pres("1"), width),
                    'h' => {
                        let h = dt.hour % 12;
                        fmt_component(if h == 0 { 12 } else { h as i64 }, &default_pres("1"), width)
                    }
                    'P' => {
                        let s = if dt.hour < 12 { "am" } else { "pm" };
                        match pres.as_str() {
                            "N" => s.to_uppercase(),
                            "Nn" => format!("{}{}", s[..1].to_uppercase(), &s[1..]),
                            _ => s.to_string(),
                        }
                    }
                    'm' => fmt_component(dt.minute as i64, &default_pres("01"), width),
                    's' => fmt_component(dt.second as i64, &default_pres("01"), width),
                    'f' => {
                        let p = default_pres("1");
                        let digits = p.chars().filter(|c| c.is_ascii_digit()).count().max(1);
                        let f = format!("{:09}", dt.nanos);
                        let n = width.map(|w| w.0.max(digits)).unwrap_or(digits).min(9);
                        f[..n].to_string()
                    }
                    'Z' => match dt.tz {
                        None => String::new(),
                        Some(0) if pres == "Z" => "Z".into(),
                        Some(t) => format!("{}{:02}:{:02}", if t < 0 { '-' } else { '+' }, t.abs() / 60, t.abs() % 60),
                    },
                    'z' => match dt.tz {
                        None => String::new(),
                        Some(t) => format!("GMT{}{:02}:{:02}", if t < 0 { '-' } else { '+' }, t.abs() / 60, t.abs() % 60),
                    },
                    'E' => if dt.year < 0 { "BC".into() } else { "AD".into() },
                    'C' => "ISO".into(),
                    other => return Err(format!("XTDE1340: Unknown component [{}] in picture string", other)),
                };
                out.push_str(&s);
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// Parse an `xs:duration` into (negative, years, months, days, hours, minutes, seconds).
pub fn parse_duration(s: &str) -> Option<(bool, i64, i64, i64, i64, i64, f64)> {
    let (neg, s) = match s.trim().strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.trim()),
    };
    let s = s.strip_prefix('P')?;
    let (date, time) = s.split_once('T').unwrap_or((s, ""));
    let mut v = (neg, 0, 0, 0, 0, 0, 0.0);
    let mut cur = String::new();
    for c in date.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else {
            let n: i64 = cur.parse().ok()?;
            match c {
                'Y' => v.1 = n,
                'M' => v.2 = n,
                'D' => v.3 = n,
                _ => return None,
            }
            cur.clear();
        }
    }
    for c in time.chars() {
        if c.is_ascii_digit() || c == '.' {
            cur.push(c);
        } else {
            match c {
                'H' => v.4 = cur.parse().ok()?,
                'M' => v.5 = cur.parse().ok()?,
                'S' => v.6 = cur.parse().ok()?,
                _ => return None,
            }
            cur.clear();
        }
    }
    cur.is_empty().then_some(v)
}

// ------------------------------------------------------------------ dispatch

/// Names of the XPath 2.0 functions implemented here (for `function-available`).
pub const FUNCTION_NAMES: &[&str] = &["abs", "adjust-date-to-timezone", "adjust-dateTime-to-timezone", "avg", "base-uri", "codepoints-to-string", "compare", "current-date", "current-dateTime", "current-time", "data", "day-from-date", "day-from-dateTime", "days-from-duration", "deep-equal", "default-collation", "distinct-values", "document-uri", "empty", "encode-for-uri", "error", "escape-html-uri", "exactly-one", "exists", "format-date", "format-dateTime", "format-time", "hours-from-dateTime", "hours-from-duration", "hours-from-time", "implicit-timezone", "index-of", "insert-before", "iri-to-uri", "lower-case", "matches", "max", "min", "minutes-from-dateTime", "minutes-from-duration", "minutes-from-time", "month-from-date", "month-from-dateTime", "months-from-duration", "node-name", "normalize-unicode", "one-or-more", "remove", "replace", "reverse", "round-half-to-even", "seconds-from-dateTime", "seconds-from-duration", "seconds-from-time", "static-base-uri", "string-join", "string-pad", "string-to-codepoints", "subsequence", "timezone-from-date", "timezone-from-dateTime", "timezone-from-time", "tokenize", "trace", "unordered", "upper-case", "year-from-date", "year-from-dateTime", "years-from-duration", "zero-or-one"];

/// Call an XPath 2.0 function; `None` when unknown.
pub fn call(uri: &str, local: &str, a: &[XValue], ctx: &Ctx) -> Option<R<XValue>> {
    if uri == XS_NS {
        return Some(constructor(local, a, ctx));
    }
    if !uri.is_empty() && uri != FN_NS {
        return None;
    }
    let s = |i: usize| a.get(i).map(|v| v.to_string_value(ctx.docs)).unwrap_or_default();
    let n = a.len();
    let seq = |strings: Vec<String>| -> XValue {
        if strings.len() == 1 {
            XValue::Str(strings.into_iter().next().unwrap())
        } else {
            ctx.docs.atomic_sequence(&strings)
        }
    };
    let dt_arg = |i: usize| -> R<Option<DateTime>> {
        match a.get(i) {
            Some(XValue::Nodes(ns)) if ns.is_empty() => Ok(None),
            Some(v) => parse_date_time(&v.to_string_value(ctx.docs)).map(Some).ok_or_else(|| XPathError::eval(format!("Invalid date/time value '{}'", v.to_string_value(ctx.docs)))),
            None => Ok(None),
        }
    };
    let r: R<XValue> = match (local, n) {
        ("matches", 2 | 3) => compile_regex(&s(1), &s(2)).map(|re| XValue::Bool(re.is_match(&s(0)))),
        ("replace", 3 | 4) => (|| {
            let flags = s(3);
            let re = compile_regex(&s(1), &flags)?;
            if re.is_full_match("") {
                return err(format!("The pattern '{}' matches a zero-length string", s(1)));
            }
            let rep = if flags.contains('q') { s(2).replace('\\', "\\\\").replace('$', "\\$") } else { s(2) };
            Ok(XValue::Str(re.replace_all(&s(0), &rep)))
        })(),
        ("tokenize", 1..=3) => (|| {
            let text = s(0);
            if n == 1 {
                return Ok(ctx.docs.atomic_sequence(&text.split_whitespace().map(String::from).collect::<Vec<_>>()));
            }
            if text.is_empty() {
                return Ok(XValue::Nodes(Vec::new()));
            }
            let re = compile_regex(&s(1), &s(2))?;
            if re.is_full_match("") {
                return err(format!("The pattern '{}' matches a zero-length string", s(1)));
            }
            let mut out = Vec::new();
            let mut last = 0;
            for (st, en) in re.find_iter(&text) {
                out.push(text[last..st].to_string());
                last = en;
            }
            out.push(text[last..].to_string());
            Ok(ctx.docs.atomic_sequence(&out))
        })(),
        ("string-join", 1 | 2) => Ok(XValue::Str(strings(&a[0], ctx).join(&s(1)))),
        ("distinct-values", 1 | 2) => {
            let mut out: Vec<String> = Vec::new();
            for x in strings(&a[0], ctx) {
                let dup = out.iter().any(|y| *y == x || matches!((x.trim().parse::<f64>(), y.trim().parse::<f64>()), (Ok(p), Ok(q)) if p == q));
                if !dup {
                    out.push(x);
                }
            }
            Ok(ctx.docs.atomic_sequence(&out))
        }
        ("exists", 1) => Ok(XValue::Bool(count(&a[0]) > 0)),
        ("empty", 1) => Ok(XValue::Bool(count(&a[0]) == 0)),
        ("reverse", 1) => {
            let mut v = strings(&a[0], ctx);
            v.reverse();
            Ok(ctx.docs.atomic_sequence(&v))
        }
        ("subsequence", 2 | 3) => {
            let start = num(&a[1], ctx).round();
            let len = if n == 3 { num(&a[2], ctx).round() } else { f64::INFINITY };
            let keep = |i: usize| {
                let p = (i + 1) as f64;
                p >= start && p < start + len
            };
            Ok(match &a[0] {
                XValue::Nodes(ns) => XValue::Nodes(ns.iter().enumerate().filter(|(i, _)| keep(*i)).map(|(_, x)| *x).collect()),
                other => {
                    if keep(0) {
                        other.clone()
                    } else {
                        XValue::Nodes(Vec::new())
                    }
                }
            })
        }
        ("index-of", 2 | 3) => {
            let target = s(1);
            let tn = target.trim().parse::<f64>().ok();
            let idx: Vec<String> = strings(&a[0], ctx)
                .iter()
                .enumerate()
                .filter(|(_, x)| **x == target || matches!((x.trim().parse::<f64>().ok(), tn), (Some(p), Some(q)) if p == q))
                .map(|(i, _)| (i + 1).to_string())
                .collect();
            Ok(ctx.docs.atomic_sequence(&idx))
        }
        ("remove", 2) => {
            let p = num(&a[1], ctx).round() as i64;
            Ok(match &a[0] {
                XValue::Nodes(ns) => XValue::Nodes(ns.iter().enumerate().filter(|(i, _)| (*i as i64 + 1) != p).map(|(_, x)| *x).collect()),
                other => {
                    if p == 1 {
                        XValue::Nodes(Vec::new())
                    } else {
                        other.clone()
                    }
                }
            })
        }
        ("insert-before", 3) => {
            let mut v = strings(&a[0], ctx);
            let p = (num(&a[1], ctx).round().max(1.0) as usize - 1).min(v.len());
            for (k, x) in strings(&a[2], ctx).into_iter().enumerate() {
                v.insert(p + k, x);
            }
            Ok(ctx.docs.atomic_sequence(&v))
        }
        ("min" | "max", 1 | 2) => {
            let v = strings(&a[0], ctx);
            if v.is_empty() {
                Ok(XValue::Nodes(Vec::new()))
            } else if v.iter().all(|x| x.trim().parse::<f64>().is_ok()) {
                let it = v.iter().map(|x| x.trim().parse::<f64>().unwrap());
                Ok(fmt_num(if local == "min" { it.fold(f64::INFINITY, f64::min) } else { it.fold(f64::NEG_INFINITY, f64::max) }))
            } else {
                let mut v = v;
                v.sort();
                Ok(XValue::Str(if local == "min" { v.remove(0) } else { v.pop().unwrap() }))
            }
        }
        ("avg", 1) => {
            let v = strings(&a[0], ctx);
            if v.is_empty() {
                Ok(XValue::Nodes(Vec::new()))
            } else {
                Ok(fmt_num(v.iter().map(|x| super::string_to_number(x)).sum::<f64>() / v.len() as f64))
            }
        }
        ("abs", 1) => Ok(fmt_num(num(&a[0], ctx).abs())),
        ("round-half-to-even", 1 | 2) => Ok(fmt_num(round_half_even(num(&a[0], ctx), if n == 2 { num(&a[1], ctx) as i32 } else { 0 }))),
        ("compare", 2 | 3) => Ok(XValue::Num(match s(0).cmp(&s(1)) {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Equal => 0.0,
            std::cmp::Ordering::Greater => 1.0,
        })),
        ("codepoints-to-string", 1) => Ok(XValue::Str(strings(&a[0], ctx).iter().filter_map(|x| x.trim().parse::<u32>().ok().and_then(char::from_u32)).collect())),
        ("string-to-codepoints", 1) => Ok(ctx.docs.atomic_sequence(&s(0).chars().map(|c| (c as u32).to_string()).collect::<Vec<_>>())),
        ("normalize-unicode", 1 | 2) => Ok(XValue::Str(s(0))),
        ("encode-for-uri", 1) => Ok(XValue::Str(encode_for_uri(&s(0), &|c| c.is_ascii_alphanumeric() || "-_.~".contains(c)))),
        ("iri-to-uri", 1) => Ok(XValue::Str(encode_for_uri(&s(0), &|c| c.is_ascii() && c > ' ' && !"<>\"{}|\\^`".contains(c)))),
        ("escape-html-uri", 1) => Ok(XValue::Str(encode_for_uri(&s(0), &|c| (' '..='~').contains(&c)))),
        ("data", 1) => Ok(seq(strings(&a[0], ctx))),
        ("node-name", 0 | 1) => {
            let node = match a.first() {
                Some(XValue::Nodes(ns)) => ns.first().copied(),
                Some(_) => return Some(err("node-name() requires a node")),
                None => Some(ctx.node),
            };
            Ok(match node {
                Some(x) if matches!(ctx.docs.node_type(x), Some(NodeType::Element | NodeType::Attribute | NodeType::ProcessingInstruction)) => {
                    let (p, l, _) = ctx.docs.node_name(x);
                    XValue::Str(if p.is_empty() { l } else { format!("{}:{}", p, l) })
                }
                _ => XValue::Nodes(Vec::new()),
            })
        }
        ("exactly-one" | "zero-or-one" | "one-or-more" | "unordered", 1) => {
            let c = count(&a[0]);
            match local {
                "exactly-one" if c != 1 => err("FORG0005: exactly-one() called with a sequence containing zero or more than one item"),
                "zero-or-one" if c > 1 => err("FORG0003: zero-or-one() called with a sequence containing more than one item"),
                "one-or-more" if c == 0 => err("FORG0004: one-or-more() called with an empty sequence"),
                _ => Ok(a[0].clone()),
            }
        }
        ("deep-equal", 2 | 3) => {
            let (x, y) = (strings(&a[0], ctx), strings(&a[1], ctx));
            let ser = |v: &XValue| match v {
                XValue::Nodes(ns) => ns.iter().map(|n| ctx.docs.node_to_string(*n)).collect::<Vec<_>>(),
                other => vec![other.to_string_value(ctx.docs)],
            };
            Ok(XValue::Bool(x.len() == y.len() && ser(&a[0]) == ser(&a[1])))
        }
        ("trace", 1 | 2) => {
            cameleer_core::camel_debug!("xpath", "trace {}: {}", s(1), s(0));
            Ok(a[0].clone())
        }
        ("error", 0..=3) => err(if n >= 2 { s(1) } else if n == 1 { s(0) } else { "FOER0000: error() called".to_string() }),
        ("current-dateTime", 0) => {
            let d = now();
            Ok(XValue::Str(format!("{}T{}{}", date_text(&d), time_text(&d), tz_text(d.tz))))
        }
        ("current-date", 0) => {
            let d = now();
            Ok(XValue::Str(format!("{}{}", date_text(&d), tz_text(d.tz))))
        }
        ("current-time", 0) => {
            let d = now();
            Ok(XValue::Str(format!("{}{}", time_text(&d), tz_text(d.tz))))
        }
        ("implicit-timezone", 0) => Ok(XValue::Str("PT0S".into())),
        ("year-from-date" | "year-from-dateTime", 1) => dt_arg(0).map(|d| d.map(|d| XValue::Num(d.year as f64)).unwrap_or(XValue::Nodes(Vec::new()))),
        ("month-from-date" | "month-from-dateTime", 1) => dt_arg(0).map(|d| d.map(|d| XValue::Num(d.month as f64)).unwrap_or(XValue::Nodes(Vec::new()))),
        ("day-from-date" | "day-from-dateTime", 1) => dt_arg(0).map(|d| d.map(|d| XValue::Num(d.day as f64)).unwrap_or(XValue::Nodes(Vec::new()))),
        ("hours-from-dateTime" | "hours-from-time", 1) => dt_arg(0).map(|d| d.map(|d| XValue::Num(d.hour as f64)).unwrap_or(XValue::Nodes(Vec::new()))),
        ("minutes-from-dateTime" | "minutes-from-time", 1) => dt_arg(0).map(|d| d.map(|d| XValue::Num(d.minute as f64)).unwrap_or(XValue::Nodes(Vec::new()))),
        ("seconds-from-dateTime" | "seconds-from-time", 1) => dt_arg(0).map(|d| d.map(|d| XValue::Num(d.second as f64 + d.nanos as f64 / 1e9)).unwrap_or(XValue::Nodes(Vec::new()))),
        ("timezone-from-date" | "timezone-from-dateTime" | "timezone-from-time", 1) => dt_arg(0).map(|d| match d.and_then(|d| d.tz) {
            Some(t) => XValue::Str(format!("{}PT{}H{}", if t < 0 { "-" } else { "" }, t.abs() / 60, if t.abs() % 60 > 0 { format!("{}M", t.abs() % 60) } else { String::new() })),
            None => XValue::Nodes(Vec::new()),
        }),
        ("years-from-duration" | "months-from-duration" | "days-from-duration" | "hours-from-duration" | "minutes-from-duration" | "seconds-from-duration", 1) => match parse_duration(&s(0)) {
            Some((neg, y, mo, d, h, mi, sec)) => {
                let sign = if neg { -1.0 } else { 1.0 };
                let v = match local {
                    "years-from-duration" => (y + mo / 12) as f64,
                    "months-from-duration" => (mo % 12) as f64,
                    "days-from-duration" => d as f64,
                    "hours-from-duration" => h as f64,
                    "minutes-from-duration" => mi as f64,
                    _ => sec,
                };
                Ok(XValue::Num(sign * v))
            }
            None => err(format!("Invalid duration '{}'", s(0))),
        },
        ("format-date" | "format-dateTime" | "format-time", 2..=5) => match dt_arg(0) {
            Ok(Some(d)) => format_date_time(&d, &s(1)).map(XValue::Str).map_err(XPathError::eval),
            Ok(None) => Ok(XValue::Nodes(Vec::new())),
            Err(e) => Err(e),
        },
        ("adjust-dateTime-to-timezone" | "adjust-date-to-timezone", 1 | 2) => Ok(a[0].clone()),
        ("base-uri" | "document-uri" | "static-base-uri" | "default-collation", _) => Ok(XValue::Str(if local == "default-collation" { "http://www.w3.org/2005/xpath-functions/collation/codepoint".into() } else { String::new() })),
        ("\u{0}idiv", 2) => {
            let (x, y) = (num(&a[0], ctx), num(&a[1], ctx));
            if y == 0.0 {
                err("FOAR0001: Integer division by zero")
            } else {
                Ok(XValue::Num((x / y).trunc()))
            }
        }
        ("string-pad", 2) => Ok(XValue::Str(s(0).repeat(num(&a[1], ctx).max(0.0) as usize))),
        ("upper-case" | "lower-case", 1) => Ok(XValue::Str(if local == "upper-case" { s(0).to_uppercase() } else { s(0).to_lowercase() })),
        _ => return None,
    };
    Some(r)
}

/// `xs:*` constructor functions.
fn constructor(local: &str, a: &[XValue], ctx: &Ctx) -> R<XValue> {
    let Some(v) = a.first() else { return err(format!("xs:{}() requires one argument", local)) };
    if let XValue::Nodes(ns) = v {
        if ns.is_empty() {
            return Ok(XValue::Nodes(Vec::new()));
        }
    }
    let s = v.to_string_value(ctx.docs);
    let t = s.trim();
    let bad = || err(format!("FORG0001: Cannot convert '{}' to xs:{}", t, local));
    match local {
        "string" | "normalizedString" | "token" | "anyURI" | "untypedAtomic" | "NCName" | "Name" | "language" | "ID" | "IDREF" | "QName" => Ok(XValue::Str(s)),
        "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger" | "positiveInteger" | "negativeInteger" | "nonPositiveInteger" | "unsignedInt" | "unsignedLong" | "unsignedShort" | "unsignedByte" => {
            let x = match v {
                XValue::Num(x) => x.trunc(),
                XValue::Bool(b) => *b as i64 as f64,
                _ => match t.trim_start_matches('+').parse::<i64>() {
                    Ok(i) => i as f64,
                    Err(_) => return bad(),
                },
            };
            if x.is_nan() || x.is_infinite() {
                return bad();
            }
            Ok(XValue::Num(x))
        }
        "decimal" | "double" | "float" => match v {
            XValue::Num(x) => Ok(XValue::Num(*x)),
            XValue::Bool(b) => Ok(XValue::Num(*b as i64 as f64)),
            _ => match t {
                "INF" => Ok(XValue::Num(f64::INFINITY)),
                "-INF" => Ok(XValue::Num(f64::NEG_INFINITY)),
                "NaN" if local != "decimal" => Ok(XValue::Num(f64::NAN)),
                _ => t.parse::<f64>().map(XValue::Num).or_else(|_| bad()),
            },
        },
        "boolean" => match v {
            XValue::Num(x) => Ok(XValue::Bool(*x != 0.0 && !x.is_nan())),
            XValue::Bool(b) => Ok(XValue::Bool(*b)),
            _ => match t {
                "true" | "1" => Ok(XValue::Bool(true)),
                "false" | "0" => Ok(XValue::Bool(false)),
                _ => bad(),
            },
        },
        "date" | "dateTime" | "time" => match parse_date_time(t) {
            Some(d) if (local == "date") == (d.has_date && !d.has_time) || local == "dateTime" && d.has_date && d.has_time || local == "time" && d.has_time && !d.has_date => Ok(XValue::Str(t.to_string())),
            Some(d) if local == "date" && d.has_date => Ok(XValue::Str(format!("{}{}", date_text(&d), tz_text(d.tz)))),
            _ => bad(),
        },
        "duration" | "dayTimeDuration" | "yearMonthDuration" => {
            if parse_duration(t).is_some() {
                Ok(XValue::Str(t.to_string()))
            } else {
                bad()
            }
        }
        _ => Ok(XValue::Str(s)),
    }
}
