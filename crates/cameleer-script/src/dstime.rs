//! DataSonnet `ds.datetime` functions: ISO-8601 offset date-times (as strings, like DataSonnet),
//! time-zone aware through `cameleer_core::tz` (zoneinfo region ids such as `Europe/Paris`, fixed
//! offsets) and Java-style patterns through `cameleer_core::util_time`.

use cameleer_core::tz::TimeZone;
use cameleer_core::util_time::{civil_from_epoch, epoch_from_civil, format_in_zone, now_millis};

use crate::interp::{Interp, R};
use crate::value::*;

/// An instant with the UTC offset it was written in.
#[derive(Clone, Copy, Debug)]
struct Dt {
    millis: i64,
    offset: i32,
}

fn digits(s: &[u8], i: &mut usize, n: usize) -> Option<i64> {
    if *i + n > s.len() || !s[*i..*i + n].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let v = std::str::from_utf8(&s[*i..*i + n]).ok()?.parse().ok()?;
    *i += n;
    Some(v)
}

/// Parse `yyyy-MM-dd['T'HH:mm[:ss[.SSS]]][Z|±hh:mm]` (a missing offset means UTC).
fn parse_iso(text: &str) -> Option<Dt> {
    let s = text.trim().as_bytes();
    let mut i = 0;
    let y = digits(s, &mut i, 4)?;
    if s.get(i) != Some(&b'-') {
        return None;
    }
    i += 1;
    let mo = digits(s, &mut i, 2)?;
    if s.get(i) != Some(&b'-') {
        return None;
    }
    i += 1;
    let d = digits(s, &mut i, 2)?;
    let (mut h, mut mi, mut sec, mut ms) = (0, 0, 0, 0);
    if matches!(s.get(i), Some(b'T') | Some(b' ')) {
        i += 1;
        h = digits(s, &mut i, 2)?;
        if s.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        mi = digits(s, &mut i, 2)?;
        if s.get(i) == Some(&b':') {
            i += 1;
            sec = digits(s, &mut i, 2)?;
            if s.get(i) == Some(&b'.') {
                i += 1;
                let start = i;
                while i < s.len() && s[i].is_ascii_digit() {
                    i += 1;
                }
                let frac = std::str::from_utf8(&s[start..i]).ok()?;
                let f3: String = frac.chars().chain(std::iter::repeat('0')).take(3).collect();
                ms = f3.parse().ok()?;
            }
        }
    }
    let offset = match s.get(i) {
        None => 0,
        Some(b'Z') | Some(b'z') => {
            i += 1;
            0
        }
        Some(c @ (b'+' | b'-')) => {
            let sign = if *c == b'-' { -1 } else { 1 };
            i += 1;
            let oh = digits(s, &mut i, 2)?;
            if s.get(i) == Some(&b':') {
                i += 1;
            }
            let om = digits(s, &mut i, 2).unwrap_or(0);
            sign * (oh * 3600 + om * 60) as i32
        }
        _ => return None,
    };
    // `[Europe/Paris]` zone suffix is ignored (the offset is authoritative)
    if i < s.len() && s[i] != b'[' {
        return None;
    }
    let local = epoch_from_civil(y, mo as u32, d as u32, h as u32, mi as u32, sec as u32);
    Some(Dt { millis: (local - offset as i64) * 1000 + ms, offset })
}

fn offset_zone(offset: i32) -> TimeZone {
    if offset == 0 {
        return TimeZone::utc();
    }
    let a = offset.abs();
    TimeZone::fixed(&format!("GMT{}{:02}:{:02}", if offset < 0 { '-' } else { '+' }, a / 3600, (a % 3600) / 60), offset)
}

/// ISO-8601 offset date-time text (`2024-01-01T10:00:00Z`, `...+01:00`, millis when present).
fn iso(dt: Dt) -> String {
    let pat = if dt.millis.rem_euclid(1000) != 0 { "yyyy-MM-dd'T'HH:mm:ss.SSS" } else { "yyyy-MM-dd'T'HH:mm:ss" };
    let mut s = format_in_zone(dt.millis, pat, &offset_zone(dt.offset));
    if dt.offset == 0 {
        s.push('Z');
    } else {
        let a = dt.offset.abs();
        s.push_str(&format!("{}{:02}:{:02}", if dt.offset < 0 { '-' } else { '+' }, a / 3600, (a % 3600) / 60));
    }
    s
}

/// Parse text with a Java-style pattern (`yyyy MM dd HH mm ss SSS`, `Z`/`X` offsets, quoted
/// literals); missing fields default to the epoch start, a missing offset to UTC.
/// Parse with a Java pattern: local epoch milliseconds (the wall clock time of the text).
pub(crate) fn parse_local_millis(text: &str, pattern: &str) -> Option<i64> {
    parse_pattern(text, pattern).map(|dt| dt.millis + dt.offset as i64 * 1000)
}

fn parse_pattern(text: &str, pattern: &str) -> Option<Dt> {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    let (mut y, mut mo, mut d, mut h, mut mi, mut s, mut ms) = (1970i64, 1i64, 1i64, 0i64, 0i64, 0i64, 0i64);
    let mut pm: Option<bool> = None;
    let mut offset = 0i32;
    let (mut i, mut j) = (0usize, 0usize);
    let num = |t: &[char], j: &mut usize, max: usize| -> Option<i64> {
        let start = *j;
        while *j < t.len() && *j - start < max && t[*j].is_ascii_digit() {
            *j += 1;
        }
        if *j == start {
            return None;
        }
        t[start..*j].iter().collect::<String>().parse().ok()
    };
    while i < p.len() {
        let c = p[i];
        if c == '\'' {
            i += 1;
            while i < p.len() && p[i] != '\'' {
                if t.get(j) != Some(&p[i]) {
                    return None;
                }
                i += 1;
                j += 1;
            }
            i += 1;
            continue;
        }
        if !c.is_ascii_alphabetic() {
            if t.get(j) != Some(&c) {
                return None;
            }
            i += 1;
            j += 1;
            continue;
        }
        let mut n = 0;
        while i < p.len() && p[i] == c {
            n += 1;
            i += 1;
        }
        let fixed = i < p.len() && p[i].is_ascii_alphabetic();
        let max = if fixed { n } else { 9 };
        match c {
            'y' | 'u' => y = num(&t, &mut j, if n == 2 { 2 } else { max.max(4) })?,
            'M' => {
                if n >= 3 {
                    let rest: String = t[j..].iter().collect();
                    let names = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
                    let k = names.iter().position(|m| rest.to_ascii_lowercase().starts_with(m))?;
                    mo = k as i64 + 1;
                    j += 3;
                    while j < t.len() && t[j].is_ascii_alphabetic() {
                        j += 1;
                    }
                } else {
                    mo = num(&t, &mut j, max.max(2))?
                }
            }
            'd' => d = num(&t, &mut j, max.max(2))?,
            'H' | 'k' => h = num(&t, &mut j, max.max(2))?,
            'h' | 'K' => h = num(&t, &mut j, max.max(2))?,
            'm' => mi = num(&t, &mut j, max.max(2))?,
            's' => s = num(&t, &mut j, max.max(2))?,
            'S' => {
                let start = j;
                let v = num(&t, &mut j, n)?;
                let len = j - start;
                ms = v * 1000 / 10i64.pow(len as u32);
            }
            'a' => {
                let rest: String = t[j..].iter().take(2).collect::<String>().to_ascii_uppercase();
                pm = Some(rest == "PM");
                j += 2;
            }
            'E' => {
                while j < t.len() && t[j].is_ascii_alphabetic() {
                    j += 1;
                }
            }
            'Z' | 'X' | 'x' => {
                if t.get(j) == Some(&'Z') {
                    j += 1;
                } else {
                    let sign = match t.get(j) {
                        Some('+') => 1,
                        Some('-') => -1,
                        _ => return None,
                    };
                    j += 1;
                    let oh = num(&t, &mut j, 2)?;
                    if t.get(j) == Some(&':') {
                        j += 1;
                    }
                    let om = num(&t, &mut j, 2).unwrap_or(0);
                    offset = sign * (oh * 3600 + om * 60) as i32;
                }
            }
            _ => return None,
        }
    }
    if j != t.len() {
        return None;
    }
    if let Some(pm) = pm {
        if pm && h < 12 {
            h += 12;
        } else if !pm && h == 12 {
            h = 0;
        }
    }
    let local = epoch_from_civil(y, mo as u32, d as u32, h as u32, mi as u32, s as u32);
    Some(Dt { millis: (local - offset as i64) * 1000 + ms, offset })
}

/// Add an ISO-8601 period / duration (`P1Y2M3D`, `PT4H5M6S`, `-P1D`) in the local time of `dt`.
fn plus_period(dt: Dt, period: &str, sign: i64) -> Option<Dt> {
    let mut p = period.trim();
    let mut sign = sign;
    if let Some(r) = p.strip_prefix('-') {
        sign = -sign;
        p = r;
    }
    let p = p.strip_prefix('P').or_else(|| p.strip_prefix('p'))?;
    let (date, time) = match p.find(['T', 't']) {
        Some(i) => (&p[..i], &p[i + 1..]),
        None => (p, ""),
    };
    let (mut years, mut months, mut days, mut secs) = (0i64, 0i64, 0i64, 0f64);
    let mut cur = String::new();
    for c in date.chars() {
        match c {
            '0'..='9' | '-' => cur.push(c),
            'Y' | 'y' => years = std::mem::take(&mut cur).parse().ok()?,
            'M' | 'm' => months = std::mem::take(&mut cur).parse().ok()?,
            'W' | 'w' => days += 7 * std::mem::take(&mut cur).parse::<i64>().ok()?,
            'D' | 'd' => days += std::mem::take(&mut cur).parse::<i64>().ok()?,
            _ => return None,
        }
    }
    for c in time.chars() {
        match c {
            '0'..='9' | '-' | '.' => cur.push(c),
            'H' | 'h' => secs += std::mem::take(&mut cur).parse::<f64>().ok()? * 3600.0,
            'M' | 'm' => secs += std::mem::take(&mut cur).parse::<f64>().ok()? * 60.0,
            'S' | 's' => secs += std::mem::take(&mut cur).parse::<f64>().ok()?,
            _ => return None,
        }
    }
    let local_ms = dt.millis + dt.offset as i64 * 1000;
    let ms = local_ms.rem_euclid(1000);
    let (y, mo, d, h, mi, s) = civil_from_epoch(local_ms.div_euclid(1000));
    let total_m = y * 12 + (mo as i64 - 1) + sign * (years * 12 + months);
    let ny = total_m.div_euclid(12);
    let nm = (total_m.rem_euclid(12) + 1) as u32;
    let nd = d.min(days_in_month(ny, nm));
    let base = epoch_from_civil(ny, nm, nd, h, mi, s) + sign * days * 86_400;
    let local = base * 1000 + ms + (sign as f64 * secs * 1000.0).round() as i64;
    Some(Dt { millis: local - dt.offset as i64 * 1000, offset: dt.offset })
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn local_parts(dt: Dt) -> (i64, u32, u32, u32, u32, u32) {
    civil_from_epoch((dt.millis + dt.offset as i64 * 1000).div_euclid(1000))
}

/// `ds.datetime.<name>(args)`.
pub fn call(it: &mut Interp, name: &str, args: Vec<V>, pos: u32) -> R<V> {
    let s = |i: usize| -> String {
        match args.get(i) {
            Some(V::Str(x)) => x.to_string(),
            Some(v) => it.display(v),
            None => String::new(),
        }
    };
    let dt = |it: &Interp, i: usize| -> R<Dt> {
        let text = match args.get(i) {
            Some(V::Str(x)) => x.to_string(),
            Some(v) => it.display(v),
            None => String::new(),
        };
        match parse_iso(&text) {
            Some(d) => Ok(d),
            None => it.throw("RUNTIME ERROR", format!("ds.datetime: cannot parse '{}' as an ISO-8601 date-time", text), Some(pos)),
        }
    };
    Ok(match name {
        "now" => V::string(iso(Dt { millis: now_millis(), offset: 0 })),
        "today" => {
            let d = Dt { millis: now_millis(), offset: 0 };
            let (y, m, dd, ..) = local_parts(d);
            V::string(iso(Dt { millis: epoch_from_civil(y, m, dd, 0, 0, 0) * 1000, offset: 0 }))
        }
        "format" => {
            let d = dt(it, 0)?;
            V::string(format_in_zone(d.millis, &s(1), &offset_zone(d.offset)))
        }
        "parse" => {
            let text = s(0);
            let fmt = s(1);
            let d = match fmt.as_str() {
                "unix" | "timestamp" | "epoch" => {
                    let secs: f64 = match args.first() {
                        Some(v) if v.is_number() => v.as_f64().unwrap_or(0.0),
                        _ => text.trim().parse().unwrap_or(f64::NAN),
                    };
                    if secs.is_nan() {
                        return it.throw("RUNTIME ERROR", format!("ds.datetime.parse: invalid timestamp '{}'", text), Some(pos));
                    }
                    Some(Dt { millis: (secs * 1000.0) as i64, offset: 0 })
                }
                "" => parse_iso(&text),
                f => parse_pattern(&text, f),
            };
            match d {
                Some(d) => V::string(iso(d)),
                None => return it.throw("RUNTIME ERROR", format!("ds.datetime.parse: '{}' does not match the format '{}'", text, fmt), Some(pos)),
            }
        }
        "changeTimeZone" => {
            let d = dt(it, 0)?;
            let zone = match TimeZone::parse(&s(1)) {
                Ok(z) => z,
                Err(e) => return it.throw("RUNTIME ERROR", format!("ds.datetime.changeTimeZone: {}", e.message), Some(pos)),
            };
            let offset = zone.offset_at(d.millis.div_euclid(1000));
            V::string(iso(Dt { millis: d.millis, offset }))
        }
        "toLocalDate" => V::string(format_in_zone(dt(it, 0)?.millis, "yyyy-MM-dd", &offset_zone(dt(it, 0)?.offset))),
        "toLocalTime" => V::string(format_in_zone(dt(it, 0)?.millis, "HH:mm:ss", &offset_zone(dt(it, 0)?.offset))),
        "toLocalDateTime" => V::string(format_in_zone(dt(it, 0)?.millis, "yyyy-MM-dd'T'HH:mm:ss", &offset_zone(dt(it, 0)?.offset))),
        "plus" | "minus" => {
            let d = dt(it, 0)?;
            match plus_period(d, &s(1), if name == "plus" { 1 } else { -1 }) {
                Some(r) => V::string(iso(r)),
                None => return it.throw("RUNTIME ERROR", format!("ds.datetime.{}: invalid period '{}'", name, s(1)), Some(pos)),
            }
        }
        "compare" => {
            let (a, b) = (dt(it, 0)?, dt(it, 1)?);
            V::Int(match a.millis.cmp(&b.millis) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })
        }
        "daysBetween" => {
            let (a, b) = (dt(it, 0)?, dt(it, 1)?);
            V::Int((b.millis - a.millis) / 86_400_000)
        }
        "isLeapYear" => V::Bool(is_leap(local_parts(dt(it, 0)?).0)),
        "atBeginningOfDay" | "atBeginningOfMonth" | "atBeginningOfYear" | "atBeginningOfHour" => {
            let d = dt(it, 0)?;
            let (y, m, dd, h, ..) = local_parts(d);
            let local = match name {
                "atBeginningOfDay" => epoch_from_civil(y, m, dd, 0, 0, 0),
                "atBeginningOfMonth" => epoch_from_civil(y, m, 1, 0, 0, 0),
                "atBeginningOfYear" => epoch_from_civil(y, 1, 1, 0, 0, 0),
                _ => epoch_from_civil(y, m, dd, h, 0, 0),
            };
            V::string(iso(Dt { millis: (local - d.offset as i64) * 1000, offset: d.offset }))
        }
        "toEpochSeconds" | "toEpochMilli" => {
            let d = dt(it, 0)?;
            V::Int(if name == "toEpochMilli" { d.millis } else { d.millis.div_euclid(1000) })
        }
        _ => return it.throw("RUNTIME ERROR", format!("field does not exist: ds.datetime.{}", name), Some(pos)),
    })
}
