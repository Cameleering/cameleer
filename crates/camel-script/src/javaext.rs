//! Java runtime extensions: `char` helpers (the parser types char literals and promotes chars
//! to int in arithmetic), `List.remove(Object)`, sorted `TreeMap` / `TreeSet` (kept ordered
//! on every change, natural ordering or a `Comparator`, navigation methods) and `java.time`
//! basics (`LocalDate`, `LocalDateTime`, `LocalTime`, `Duration`, `Period`, `ChronoUnit`,
//! `DayOfWeek` / `Month`, `DateTimeFormatter.ofPattern`).

use std::cmp::Ordering;
use std::rc::Rc;

use camel_core::util_time::{civil_from_epoch, epoch_from_civil};

use crate::interp::{Interp, R};
use crate::value::*;

/// Java class namespaces added by this module.
pub const NS: &[&str] = &["LocalDate", "LocalDateTime", "LocalTime", "Duration", "Period", "ChronoUnit", "DateTimeFormatter", "DayOfWeek", "Month", "ZoneOffset", "Instant"];

/// Install the Java globals of this module.
pub fn install(it: &mut Interp) {
    for n in NS {
        it.globals.define(n, V::host(Host::Ns(n)));
    }
    for f in ["%char", "%chr", "%ord", "%jcast"] {
        it.globals.define(f, V::Method(Rc::new((V::Null, f.to_string()))));
    }
}

// ====================================================================== java.time values

/// A `java.time` value.
#[derive(Clone, Debug, PartialEq)]
pub enum JTime {
    /// Epoch day.
    Date(i64),
    /// Local epoch seconds and nanoseconds.
    DateTime(i64, u32),
    /// Second of day and nanoseconds.
    Time(i64, u32),
    /// Seconds and nanoseconds.
    Duration(i64, u32),
    Period(i64, i64, i64),
    /// `DayOfWeek` (1 = Monday) / `Month` (1 = January).
    Dow(u32),
    Month(u32),
    /// `ChronoUnit` name.
    Unit(String),
    /// `DateTimeFormatter` pattern (`%iso...` for the ISO formatters).
    Formatter(String),
}

fn jt(v: JTime) -> V {
    V::obj_kind(PropMap::default(), ObjKind::JTime(v))
}

/// The java.time value of a runtime value.
pub fn jtime_of(v: &V) -> Option<JTime> {
    match v {
        V::Obj(o) => match &o.borrow().kind {
            ObjKind::JTime(t) => Some(t.clone()),
            _ => None,
        },
        _ => None,
    }
}

const DOW: [&str; 7] = ["MONDAY", "TUESDAY", "WEDNESDAY", "THURSDAY", "FRIDAY", "SATURDAY", "SUNDAY"];
const MONTHS: [&str; 12] = ["JANUARY", "FEBRUARY", "MARCH", "APRIL", "MAY", "JUNE", "JULY", "AUGUST", "SEPTEMBER", "OCTOBER", "NOVEMBER", "DECEMBER"];

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn month_len(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
    }
}

fn ymd(day: i64) -> (i64, u32, u32) {
    let (y, m, d, _, _, _) = civil_from_epoch(day * 86400);
    (y, m, d)
}

fn epoch_day(y: i64, m: u32, d: u32) -> i64 {
    epoch_from_civil(y, m, d, 0, 0, 0).div_euclid(86400)
}

fn plus_months(day: i64, months: i64) -> i64 {
    let (y, m, d) = ymd(day);
    let total = y * 12 + (m as i64 - 1) + months;
    let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    epoch_day(ny, nm, d.min(month_len(ny, nm)))
}

fn time_str(sod: i64, nanos: u32) -> String {
    let (h, m, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let mut out = format!("{:02}:{:02}", h, m);
    if s != 0 || nanos != 0 {
        out.push_str(&format!(":{:02}", s));
    }
    if nanos != 0 {
        let frac = format!("{:09}", nanos);
        let frac = if nanos % 1_000_000 == 0 { &frac[..3] } else if nanos % 1000 == 0 { &frac[..6] } else { &frac[..] };
        out.push('.');
        out.push_str(frac);
    }
    out
}

fn date_str(day: i64) -> String {
    let (y, m, d) = ymd(day);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn duration_str(secs: i64, nanos: u32) -> String {
    if secs == 0 && nanos == 0 {
        return "PT0S".into();
    }
    let neg = secs < 0;
    let t = secs.unsigned_abs() as i64;
    let (h, m, s) = (t / 3600, (t % 3600) / 60, t % 60);
    let sg = if neg { "-" } else { "" };
    let mut out = String::from("PT");
    if h != 0 {
        out.push_str(&format!("{}{}H", sg, h));
    }
    if m != 0 {
        out.push_str(&format!("{}{}M", sg, m));
    }
    if s != 0 || nanos != 0 {
        if nanos != 0 {
            out.push_str(&format!("{}{}.{}S", sg, s, format!("{:09}", nanos).trim_end_matches('0')));
        } else {
            out.push_str(&format!("{}{}S", sg, s));
        }
    }
    out
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

/// String form of a java.time value (`toString()`).
pub fn display(t: &JTime) -> String {
    match t {
        JTime::Date(d) => date_str(*d),
        JTime::DateTime(s, n) => format!("{}T{}", date_str(s.div_euclid(86400)), time_str(s.rem_euclid(86400), *n)),
        JTime::Time(s, n) => time_str(*s, *n),
        JTime::Duration(s, n) => duration_str(*s, *n),
        JTime::Period(y, m, d) => period_str(*y, *m, *d),
        JTime::Dow(d) => DOW[(*d as usize + 6) % 7].into(),
        JTime::Month(m) => MONTHS[(*m as usize + 11) % 12].into(),
        JTime::Unit(u) => u.clone(),
        JTime::Formatter(p) => format!("DateTimeFormatter[{}]", p),
    }
}

/// Ordering of two java.time values of the same kind.
pub fn compare(a: &JTime, b: &JTime) -> Option<Ordering> {
    match (a, b) {
        (JTime::Date(x), JTime::Date(y)) => Some(x.cmp(y)),
        (JTime::DateTime(x, n), JTime::DateTime(y, m)) | (JTime::Time(x, n), JTime::Time(y, m)) | (JTime::Duration(x, n), JTime::Duration(y, m)) => Some((x, n).cmp(&(y, m))),
        (JTime::Dow(x), JTime::Dow(y)) | (JTime::Month(x), JTime::Month(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

/// Local epoch seconds of a date / date-time value.
fn local_secs(t: &JTime) -> Option<(i64, u32)> {
    match t {
        JTime::Date(d) => Some((d * 86400, 0)),
        JTime::DateTime(s, n) => Some((*s, *n)),
        JTime::Time(s, n) => Some((*s, *n)),
        _ => None,
    }
}

fn format_pattern(t: &JTime, pattern: &str) -> String {
    match pattern {
        "%iso_date" | "%iso_local_date" => match t {
            JTime::DateTime(s, _) => date_str(s.div_euclid(86400)),
            other => display(other),
        },
        "%basic_iso_date" => match t {
            JTime::Date(d) | JTime::DateTime(d, _) => {
                let day = if matches!(t, JTime::DateTime(..)) { d.div_euclid(86400) } else { *d };
                date_str(day).replace('-', "")
            }
            other => display(other),
        },
        p if p.starts_with('%') => display(t),
        p => {
            let (s, n) = local_secs(t).unwrap_or((0, 0));
            camel_core::util_time::format_millis_utc(s * 1000 + (n / 1_000_000) as i64, p)
        }
    }
}

fn i(v: &V) -> i64 {
    v.as_f64().unwrap_or(0.0) as i64
}

fn unit_secs(u: &str) -> Option<i64> {
    Some(match u {
        "NANOS" | "MICROS" | "MILLIS" => 0,
        "SECONDS" => 1,
        "MINUTES" => 60,
        "HOURS" => 3600,
        "HALF_DAYS" => 43200,
        "DAYS" => 86400,
        "WEEKS" => 7 * 86400,
        _ => return None,
    })
}

/// `amount` units added to a date / date-time.
fn plus(it: &Interp, t: &JTime, amount: i64, unit: &str, pos: u32) -> R<JTime> {
    let months = match unit {
        "MONTHS" => Some(amount),
        "YEARS" => Some(amount * 12),
        "DECADES" => Some(amount * 120),
        "CENTURIES" => Some(amount * 1200),
        _ => None,
    };
    Ok(match t {
        JTime::Date(d) => match months {
            Some(mo) => JTime::Date(plus_months(*d, mo)),
            None => match unit {
                "DAYS" => JTime::Date(d + amount),
                "WEEKS" => JTime::Date(d + 7 * amount),
                _ => return it.throw("UnsupportedTemporalTypeException", format!("Unsupported unit: {}", unit), Some(pos)),
            },
        },
        JTime::DateTime(s, n) => match months {
            Some(mo) => {
                let day = s.div_euclid(86400);
                JTime::DateTime(plus_months(day, mo) * 86400 + s.rem_euclid(86400), *n)
            }
            None => {
                let k = unit_secs(unit).unwrap_or(0);
                match unit {
                    "MILLIS" => JTime::DateTime(s + (amount / 1000), n + ((amount % 1000) * 1_000_000) as u32),
                    _ => JTime::DateTime(s + k * amount, *n),
                }
            }
        },
        JTime::Time(s, n) => JTime::Time((s + unit_secs(unit).unwrap_or(0) * amount).rem_euclid(86400), *n),
        JTime::Duration(s, n) => JTime::Duration(s + unit_secs(unit).unwrap_or(0) * amount, *n),
        other => other.clone(),
    })
}

/// Whole `unit`s from `a` to `b` (`ChronoUnit.X.between(a, b)` / `a.until(b, unit)`).
fn between(it: &Interp, a: &JTime, b: &JTime, unit: &str, pos: u32) -> R<i64> {
    let (Some((sa, na)), Some((sb, nb))) = (local_secs(a), local_secs(b)) else {
        return it.throw("DateTimeException", "unsupported temporal values for between()".into(), Some(pos));
    };
    let months = |x: i64, y: i64| -> i64 {
        let (ya, ma, da) = ymd(x.div_euclid(86400));
        let (yb, mb, db) = ymd(y.div_euclid(86400));
        let mut m = (yb * 12 + mb as i64) - (ya * 12 + ma as i64);
        let ta = (da, x.rem_euclid(86400));
        let tb = (db, y.rem_euclid(86400));
        if m > 0 && tb < ta {
            m -= 1;
        } else if m < 0 && tb > ta {
            m += 1;
        }
        m
    };
    Ok(match unit {
        "MONTHS" => months(sa, sb),
        "YEARS" => months(sa, sb) / 12,
        "DECADES" => months(sa, sb) / 120,
        "MILLIS" => (sb - sa) * 1000 + (nb as i64 - na as i64) / 1_000_000,
        u => match unit_secs(u) {
            Some(k) if k > 0 => {
                let diff = sb - sa - if nb < na { 1 } else { 0 };
                diff / k
            }
            _ => return it.throw("UnsupportedTemporalTypeException", format!("Unsupported unit: {}", unit), Some(pos)),
        },
    })
}

fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let y = s.get(..4)?.parse().ok()?;
    if s.as_bytes().get(4) != Some(&b'-') || s.as_bytes().get(7) != Some(&b'-') || s.len() != 10 {
        return None;
    }
    let m: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&m) || d == 0 || d > month_len(y, m) {
        return None;
    }
    Some(epoch_day(y, m, d))
}

fn parse_time(s: &str) -> Option<(i64, u32)> {
    let mut parts = s.trim().split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let (sec, nanos) = match parts.next() {
        Some(x) => match x.split_once('.') {
            Some((a, f)) => (a.parse::<i64>().ok()?, format!("{:0<9}", f).get(..9)?.parse::<u32>().ok()?),
            None => (x.parse::<i64>().ok()?, 0),
        },
        None => (0, 0),
    };
    if h > 23 || m > 59 || sec > 59 {
        return None;
    }
    Some((h * 3600 + m * 60 + sec, nanos))
}

fn parse_duration(s: &str) -> Option<i64> {
    let t = s.trim().to_uppercase();
    let rest = t.strip_prefix("PT").or_else(|| t.strip_prefix('P'))?;
    let mut total = 0i64;
    let mut num = String::new();
    let mut in_time = t.starts_with("PT");
    for c in rest.chars() {
        match c {
            '0'..='9' | '-' | '.' => num.push(c),
            'T' => in_time = true,
            'D' => {
                total += num.parse::<f64>().ok()? as i64 * 86400;
                num.clear();
            }
            'H' if in_time => {
                total += num.parse::<f64>().ok()? as i64 * 3600;
                num.clear();
            }
            'M' if in_time => {
                total += num.parse::<f64>().ok()? as i64 * 60;
                num.clear();
            }
            'S' if in_time => {
                total += num.parse::<f64>().ok()? as i64;
                num.clear();
            }
            _ => return None,
        }
    }
    Some(total)
}

fn now_local() -> (i64, u32) {
    let ms = camel_core::util_time::now_millis();
    let secs = ms.div_euclid(1000);
    let off = camel_core::tz::TimeZone::local().offset_at(secs) as i64;
    (secs + off, (ms.rem_euclid(1000) * 1_000_000) as u32)
}

impl Interp {
    fn jt_arg(&self, v: &V, pos: u32) -> R<JTime> {
        match jtime_of(v) {
            Some(t) => Ok(t),
            None => self.throw("DateTimeException", format!("not a java.time value: {}", self.display(v)), Some(pos)),
        }
    }

    fn parse_with(&self, text: &str, fmt: Option<&V>, kind: &str, pos: u32) -> R<JTime> {
        let pattern = fmt.and_then(jtime_of).and_then(|t| if let JTime::Formatter(p) = t { Some(p) } else { None });
        if let Some(p) = pattern.clone().filter(|p| !p.starts_with('%')) {
            if let Some(ms) = crate::dstime::parse_local_millis(text, &p) {
                let secs = ms.div_euclid(1000);
                return Ok(match kind {
                    "LocalDate" => JTime::Date(secs.div_euclid(86400)),
                    "LocalTime" => JTime::Time(secs.rem_euclid(86400), 0),
                    _ => JTime::DateTime(secs, (ms.rem_euclid(1000) * 1_000_000) as u32),
                });
            }
            return self.throw("DateTimeParseException", format!("Text '{}' could not be parsed with pattern {}", text, p), Some(pos));
        }
        let fail = || self.throw("DateTimeParseException", format!("Text '{}' could not be parsed", text), Some(pos));
        let text = text.trim();
        match kind {
            "LocalDate" => {
                let t = if pattern.as_deref() == Some("%basic_iso_date") && text.len() == 8 { format!("{}-{}-{}", &text[..4], &text[4..6], &text[6..]) } else { text.to_string() };
                parse_date(&t).map(JTime::Date).map_or_else(fail, Ok)
            }
            "LocalTime" => parse_time(text).map(|(s, n)| JTime::Time(s, n)).map_or_else(fail, Ok),
            "Duration" => parse_duration(text).map(|s| JTime::Duration(s, 0)).map_or_else(fail, Ok),
            _ => match text.split_once('T') {
                Some((d, t)) => match (parse_date(d), parse_time(t)) {
                    (Some(day), Some((s, n))) => Ok(JTime::DateTime(day * 86400 + s, n)),
                    _ => fail(),
                },
                None => fail(),
            },
        }
    }

    /// Static members of the java.time classes (`ChronoUnit.DAYS`, `DayOfWeek.MONDAY`...).
    pub fn jtime_member(&mut self, ns: &str, name: &str) -> Option<V> {
        Some(match (ns, name) {
            ("ChronoUnit", u) => jt(JTime::Unit(u.to_string())),
            ("DayOfWeek", d) => jt(JTime::Dow(DOW.iter().position(|x| *x == d)? as u32 + 1)),
            ("Month", m) => jt(JTime::Month(MONTHS.iter().position(|x| *x == m)? as u32 + 1)),
            ("DateTimeFormatter", "ISO_LOCAL_DATE" | "ISO_DATE") => jt(JTime::Formatter("%iso_date".into())),
            ("DateTimeFormatter", "ISO_LOCAL_DATE_TIME" | "ISO_DATE_TIME") => jt(JTime::Formatter("%iso_date_time".into())),
            ("DateTimeFormatter", "ISO_LOCAL_TIME" | "ISO_TIME") => jt(JTime::Formatter("%iso_time".into())),
            ("DateTimeFormatter", "BASIC_ISO_DATE") => jt(JTime::Formatter("%basic_iso_date".into())),
            ("ZoneOffset", "UTC") => V::str("Z"),
            ("LocalDate", "MIN") => jt(JTime::Date(epoch_day(-999_999_999, 1, 1))),
            ("LocalDate", "MAX") => jt(JTime::Date(epoch_day(999_999_999, 12, 31))),
            ("LocalDate", "EPOCH") => jt(JTime::Date(0)),
            ("LocalTime", "MIDNIGHT" | "MIN") => jt(JTime::Time(0, 0)),
            ("LocalTime", "NOON") => jt(JTime::Time(43200, 0)),
            ("Duration", "ZERO") => jt(JTime::Duration(0, 0)),
            ("Period", "ZERO") => jt(JTime::Period(0, 0, 0)),
            _ => return None,
        })
    }

    /// Static methods of the java.time classes.
    pub fn jtime_static(&mut self, ns: &str, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
        let a = |k: usize| args.get(k).cloned().unwrap_or(V::Null);
        let n = |k: usize| i(&args.get(k).cloned().unwrap_or(V::Int(0)));
        Ok(Some(match (ns, name) {
            ("LocalDate", "of") => {
                let m = match jtime_of(&a(1)) {
                    Some(JTime::Month(m)) => m,
                    _ => n(1) as u32,
                };
                let (y, d) = (n(0), n(2) as u32);
                if !(1..=12).contains(&m) || d == 0 || d > month_len(y, m) {
                    return self.throw("DateTimeException", format!("Invalid date: {}-{}-{}", y, m, d), Some(pos)).map(Some);
                }
                jt(JTime::Date(epoch_day(y, m, d)))
            }
            ("LocalDate", "ofEpochDay") => jt(JTime::Date(n(0))),
            ("LocalDate", "ofYearDay") => jt(JTime::Date(epoch_day(n(0), 1, 1) + n(1) - 1)),
            ("LocalDate", "now") => jt(JTime::Date(now_local().0.div_euclid(86400))),
            ("LocalDateTime", "of") => {
                if let (Some(JTime::Date(d)), Some(JTime::Time(s, nn))) = (jtime_of(&a(0)), jtime_of(&a(1))) {
                    jt(JTime::DateTime(d * 86400 + s, nn))
                } else {
                    let day = epoch_day(n(0), n(1) as u32, n(2) as u32);
                    jt(JTime::DateTime(day * 86400 + n(3) * 3600 + n(4) * 60 + n(5), n(6) as u32))
                }
            }
            ("LocalDateTime", "now") => {
                let (s, nn) = now_local();
                jt(JTime::DateTime(s, nn))
            }
            ("LocalDateTime", "ofEpochSecond") => jt(JTime::DateTime(n(0), n(1) as u32)),
            ("LocalTime", "of") => jt(JTime::Time(n(0) * 3600 + n(1) * 60 + n(2), n(3) as u32)),
            ("LocalTime", "ofSecondOfDay") => jt(JTime::Time(n(0), 0)),
            ("LocalTime", "now") => {
                let (s, nn) = now_local();
                jt(JTime::Time(s.rem_euclid(86400), nn))
            }
            ("LocalDate" | "LocalDateTime" | "LocalTime" | "Duration", "parse") => {
                let text = self.display(&a(0));
                jt(self.parse_with(&text, args.get(1), ns, pos)?)
            }
            ("Duration", "ofDays") => jt(JTime::Duration(n(0) * 86400, 0)),
            ("Duration", "ofHours") => jt(JTime::Duration(n(0) * 3600, 0)),
            ("Duration", "ofMinutes") => jt(JTime::Duration(n(0) * 60, 0)),
            ("Duration", "ofSeconds") => jt(JTime::Duration(n(0), n(1) as u32)),
            ("Duration", "ofMillis") => jt(JTime::Duration(n(0).div_euclid(1000), (n(0).rem_euclid(1000) * 1_000_000) as u32)),
            ("Duration", "between") => {
                let (x, y) = (self.jt_arg(&a(0), pos)?, self.jt_arg(&a(1), pos)?);
                let (Some((sa, na)), Some((sb, nb))) = (local_secs(&x), local_secs(&y)) else { return Ok(Some(V::Null)) };
                let mut s = sb - sa;
                let mut nn = nb as i64 - na as i64;
                if nn < 0 {
                    s -= 1;
                    nn += 1_000_000_000;
                }
                jt(JTime::Duration(s, nn as u32))
            }
            ("Period", "of") => jt(JTime::Period(n(0), n(1), n(2))),
            ("Period", "ofDays") => jt(JTime::Period(0, 0, n(0))),
            ("Period", "ofMonths") => jt(JTime::Period(0, n(0), 0)),
            ("Period", "ofYears") => jt(JTime::Period(n(0), 0, 0)),
            ("Period", "ofWeeks") => jt(JTime::Period(0, 0, n(0) * 7)),
            ("Period", "between") => {
                let (Some(JTime::Date(x)), Some(JTime::Date(y))) = (jtime_of(&a(0)), jtime_of(&a(1))) else {
                    return self.throw("DateTimeException", "Period.between expects two LocalDates".into(), Some(pos)).map(Some);
                };
                let (y1, m1, d1) = ymd(x);
                let (y2, m2, d2) = ymd(y);
                let mut total = (y2 * 12 + m2 as i64) - (y1 * 12 + m1 as i64);
                let mut days = d2 as i64 - d1 as i64;
                if total > 0 && days < 0 {
                    total -= 1;
                    days = y - plus_months(x, total);
                } else if total < 0 && days > 0 {
                    total += 1;
                    days -= month_len(y2, m2) as i64;
                }
                jt(JTime::Period(total / 12, total % 12, days))
            }
            ("DateTimeFormatter", "ofPattern") => jt(JTime::Formatter(self.display(&a(0)))),
            ("DayOfWeek", "of") => jt(JTime::Dow(n(0) as u32)),
            ("Month", "of") => jt(JTime::Month(n(0) as u32)),
            ("DayOfWeek" | "Month", "valueOf") => match self.jtime_member(ns, &self.display(&a(0))) {
                Some(v) => v,
                None => return self.throw("IllegalArgumentException", format!("No enum constant {}.{}", ns, self.display(&a(0))), Some(pos)).map(Some),
            },
            ("DayOfWeek", "values") => V::arr((1..=7).map(|d| jt(JTime::Dow(d))).collect()),
            ("Month", "values") => V::arr((1..=12).map(|m| jt(JTime::Month(m))).collect()),
            _ => return Ok(None),
        }))
    }

    /// Methods of java.time values.
    pub fn jtime_method(&mut self, t: &JTime, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
        let a = |k: usize| args.get(k).cloned().unwrap_or(V::Null);
        let n0 = i(&a(0));
        let unit_plus = |me: &mut Interp, sign: i64, unit: &str| -> R<V> { Ok(jt(plus(me, t, sign * n0, unit, pos)?)) };
        if let Some(rest) = name.strip_prefix("plus").or_else(|| name.strip_prefix("minus")) {
            let sign = if name.starts_with("plus") { 1 } else { -1 };
            let unit = match rest {
                "Days" => Some("DAYS"),
                "Weeks" => Some("WEEKS"),
                "Months" => Some("MONTHS"),
                "Years" => Some("YEARS"),
                "Hours" => Some("HOURS"),
                "Minutes" => Some("MINUTES"),
                "Seconds" => Some("SECONDS"),
                "Millis" => Some("MILLIS"),
                _ => None,
            };
            if let Some(u) = unit {
                return unit_plus(self, sign, u).map(Some);
            }
            if rest.is_empty() {
                // plus(amount, unit) / plus(Duration|Period)
                if let Some(JTime::Unit(u)) = jtime_of(&a(1)) {
                    return Ok(Some(jt(plus(self, t, sign * n0, &u, pos)?)));
                }
                match jtime_of(&a(0)) {
                    Some(JTime::Duration(s, _)) => return Ok(Some(jt(plus(self, t, sign * s, "SECONDS", pos)?))),
                    Some(JTime::Period(y, m, d)) => {
                        let r = plus(self, t, sign * (y * 12 + m), "MONTHS", pos)?;
                        return Ok(Some(jt(plus(self, &r, sign * d, "DAYS", pos)?)));
                    }
                    _ => {}
                }
            }
        }
        let date_part = |t: &JTime| -> Option<(i64, u32, u32)> {
            match t {
                JTime::Date(d) => Some(ymd(*d)),
                JTime::DateTime(s, _) => Some(ymd(s.div_euclid(86400))),
                _ => None,
            }
        };
        let sod = |t: &JTime| -> Option<(i64, u32)> {
            match t {
                JTime::DateTime(s, n) => Some((s.rem_euclid(86400), *n)),
                JTime::Time(s, n) => Some((*s, *n)),
                _ => None,
            }
        };
        let day_of = |t: &JTime| -> Option<i64> {
            match t {
                JTime::Date(d) => Some(*d),
                JTime::DateTime(s, _) => Some(s.div_euclid(86400)),
                _ => None,
            }
        };
        let with_date = |t: &JTime, day: i64| -> JTime {
            match t {
                JTime::DateTime(s, n) => JTime::DateTime(day * 86400 + s.rem_euclid(86400), *n),
                _ => JTime::Date(day),
            }
        };
        let with_time = |t: &JTime, secs: i64, nanos: u32| -> JTime {
            match t {
                JTime::DateTime(s, _) => JTime::DateTime(s.div_euclid(86400) * 86400 + secs, nanos),
                _ => JTime::Time(secs, nanos),
            }
        };
        Ok(Some(match name {
            "toString" => V::string(display(t)),
            "format" => {
                let pattern = match jtime_of(&a(0)) {
                    Some(JTime::Formatter(p)) => p,
                    _ => self.display(&a(0)),
                };
                if let JTime::Formatter(p) = t {
                    // formatter.format(temporal)
                    let target = self.jt_arg(&a(0), pos)?;
                    return Ok(Some(V::string(format_pattern(&target, p))));
                }
                V::string(format_pattern(t, &pattern))
            }
            "parse" if matches!(t, JTime::Formatter(_)) => jt(self.parse_with(&self.display(&a(0)), None, "LocalDateTime", pos)?),
            "getYear" => V::Int(date_part(t).map(|p| p.0).unwrap_or(0)),
            "getMonthValue" => V::Int(date_part(t).map(|p| p.1 as i64).unwrap_or(0)),
            "getMonth" => jt(JTime::Month(date_part(t).map(|p| p.1).unwrap_or(1))),
            "getDayOfMonth" => V::Int(date_part(t).map(|p| p.2 as i64).unwrap_or(0)),
            "getDayOfYear" => {
                let (y, _, _) = date_part(t).unwrap_or((1970, 1, 1));
                V::Int(day_of(t).unwrap_or(0) - epoch_day(y, 1, 1) + 1)
            }
            "getDayOfWeek" => jt(JTime::Dow(((day_of(t).unwrap_or(0) + 3).rem_euclid(7) + 1) as u32)),
            "lengthOfMonth" => {
                let (y, m, _) = date_part(t).unwrap_or((1970, 1, 1));
                V::Int(month_len(y, m) as i64)
            }
            "lengthOfYear" => V::Int(if is_leap(date_part(t).map(|p| p.0).unwrap_or(1970)) { 366 } else { 365 }),
            "isLeapYear" => V::Bool(is_leap(date_part(t).map(|p| p.0).unwrap_or(1970))),
            "getHour" => V::Int(sod(t).map(|s| s.0 / 3600).unwrap_or(0)),
            "getMinute" => V::Int(sod(t).map(|s| (s.0 % 3600) / 60).unwrap_or(0)),
            "getSecond" => V::Int(sod(t).map(|s| s.0 % 60).unwrap_or(0)),
            "getNano" => V::Int(sod(t).map(|s| s.1 as i64).unwrap_or(0)),
            "toSecondOfDay" => V::Int(sod(t).map(|s| s.0).unwrap_or(0)),
            "toEpochDay" => V::Int(day_of(t).unwrap_or(0)),
            "toEpochSecond" => V::Int(local_secs(t).map(|s| s.0).unwrap_or(0)),
            "withDayOfMonth" | "withMonth" | "withYear" | "withDayOfYear" => {
                let (y, m, d) = date_part(t).unwrap_or((1970, 1, 1));
                let day = match name {
                    "withDayOfMonth" => epoch_day(y, m, (n0 as u32).min(month_len(y, m))),
                    "withMonth" => epoch_day(y, n0 as u32, d.min(month_len(y, n0 as u32))),
                    "withYear" => epoch_day(n0, m, d.min(month_len(n0, m))),
                    _ => epoch_day(y, 1, 1) + n0 - 1,
                };
                jt(with_date(t, day))
            }
            "withHour" | "withMinute" | "withSecond" | "withNano" => {
                let (s, nn) = sod(t).unwrap_or((0, 0));
                let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
                let (secs, nanos) = match name {
                    "withHour" => (n0 * 3600 + m * 60 + sec, nn),
                    "withMinute" => (h * 3600 + n0 * 60 + sec, nn),
                    "withSecond" => (h * 3600 + m * 60 + n0, nn),
                    _ => (s, n0 as u32),
                };
                jt(with_time(t, secs, nanos))
            }
            "atStartOfDay" => jt(JTime::DateTime(day_of(t).unwrap_or(0) * 86400, 0)),
            "atTime" => match (t, jtime_of(&a(0))) {
                (JTime::Date(d), Some(JTime::Time(s, nn))) => jt(JTime::DateTime(d * 86400 + s, nn)),
                (JTime::Date(d), _) => jt(JTime::DateTime(d * 86400 + n0 * 3600 + i(&a(1)) * 60 + i(&a(2)), 0)),
                _ => return Ok(None),
            },
            "atDate" => match (t, jtime_of(&a(0))) {
                (JTime::Time(s, nn), Some(JTime::Date(d))) => jt(JTime::DateTime(d * 86400 + s, *nn)),
                _ => return Ok(None),
            },
            "toLocalDate" => jt(JTime::Date(day_of(t).unwrap_or(0))),
            "toLocalTime" => {
                let (s, nn) = sod(t).unwrap_or((0, 0));
                jt(JTime::Time(s, nn))
            }
            "isBefore" | "isAfter" | "isEqual" | "compareTo" | "equals" => {
                let o = jtime_of(&a(0));
                let ord = o.as_ref().and_then(|o| compare(t, o));
                match name {
                    "isBefore" => V::Bool(ord == Some(Ordering::Less)),
                    "isAfter" => V::Bool(ord == Some(Ordering::Greater)),
                    "compareTo" => V::Int(ord.map(|o| o as i64).unwrap_or(0)),
                    _ => V::Bool(o.as_ref() == Some(t)),
                }
            }
            "hashCode" => V::Int(display(t).bytes().fold(0i64, |h, b| h.wrapping_mul(31).wrapping_add(b as i64)) as i32 as i64),
            "until" => {
                let other = self.jt_arg(&a(0), pos)?;
                match jtime_of(&a(1)) {
                    Some(JTime::Unit(u)) => V::Int(between(self, t, &other, &u, pos)?),
                    _ => return self.jtime_static("Period", "between", &[jt(t.clone()), a(0)], pos),
                }
            }
            "datesUntil" => {
                let (Some(x), Some(JTime::Date(y))) = (day_of(t), jtime_of(&a(0))) else { return Ok(None) };
                V::arr((x..y).map(|d| jt(JTime::Date(d))).collect())
            }
            // Duration / Period
            "toDays" => V::Int(match t {
                JTime::Duration(s, _) => s / 86400,
                _ => 0,
            }),
            "toHours" => V::Int(match t {
                JTime::Duration(s, _) => s / 3600,
                _ => 0,
            }),
            "toMinutes" => V::Int(match t {
                JTime::Duration(s, _) => s / 60,
                _ => 0,
            }),
            "getSeconds" | "toSeconds" => V::Int(match t {
                JTime::Duration(s, _) => *s,
                _ => 0,
            }),
            "toMillis" => V::Int(match t {
                JTime::Duration(s, n) => s * 1000 + *n as i64 / 1_000_000,
                _ => 0,
            }),
            "toMinutesPart" => V::Int(match t {
                JTime::Duration(s, _) => (s % 3600) / 60,
                _ => 0,
            }),
            "toSecondsPart" => V::Int(match t {
                JTime::Duration(s, _) => s % 60,
                _ => 0,
            }),
            "isZero" => V::Bool(matches!(t, JTime::Duration(0, 0) | JTime::Period(0, 0, 0))),
            "isNegative" => V::Bool(matches!(t, JTime::Duration(s, _) if *s < 0) || matches!(t, JTime::Period(y, m, d) if *y < 0 || *m < 0 || *d < 0)),
            "getYears" => V::Int(if let JTime::Period(y, _, _) = t { *y } else { 0 }),
            "getMonths" => V::Int(if let JTime::Period(_, m, _) = t { *m } else { 0 }),
            "getDays" => V::Int(if let JTime::Period(_, _, d) = t { *d } else { 0 }),
            "toTotalMonths" => V::Int(if let JTime::Period(y, m, _) = t { y * 12 + m } else { 0 }),
            // DayOfWeek / Month / ChronoUnit
            "getValue" | "ordinal" => match t {
                JTime::Dow(d) | JTime::Month(d) => V::Int(*d as i64 - if name == "ordinal" { 1 } else { 0 }),
                _ => return Ok(None),
            },
            "name" => V::string(display(t)),
            "length" if matches!(t, JTime::Month(_)) => match t {
                JTime::Month(m) => V::Int(month_len(if self.truthy(&a(0)) { 2024 } else { 2023 }, *m) as i64),
                _ => V::Null,
            },
            "between" => match t {
                JTime::Unit(u) => {
                    let (x, y) = (self.jt_arg(&a(0), pos)?, self.jt_arg(&a(1), pos)?);
                    V::Int(between(self, &x, &y, u, pos)?)
                }
                _ => return Ok(None),
            },
            _ => return Ok(None),
        }))
    }
}

// ====================================================================== sorted collections

impl Interp {
    /// Register a `TreeMap` / `TreeSet` (with an optional comparator).
    pub fn register_sorted(&mut self, v: &V, comparator: Option<V>) {
        if let V::Obj(o) = v {
            self.jsorted.insert(Rc::as_ptr(o) as *const u8 as usize, (Rc::downgrade(o), comparator));
            let o = o.clone();
            let _ = self.resort(&o, 0);
        }
    }

    /// The comparator of a registered sorted collection (`Some(None)` for natural ordering).
    pub fn sorted_info(&self, o: &ObjRef) -> Option<Option<V>> {
        if self.jsorted.is_empty() {
            return None;
        }
        let key = Rc::as_ptr(o) as *const u8 as usize;
        self.jsorted.get(&key).filter(|(w, _)| w.upgrade().is_some_and(|x| Rc::ptr_eq(&x, o))).map(|(_, c)| c.clone())
    }

    fn sort_cmp(&mut self, cmp: &Option<V>, a: &V, b: &V, pos: u32) -> R<Ordering> {
        match cmp {
            Some(f) => {
                let r = self.call(f, V::Undef, vec![a.clone(), b.clone()], pos)?;
                Ok(r.as_f64().unwrap_or(0.0).partial_cmp(&0.0).unwrap_or(Ordering::Equal))
            }
            None => Ok(crate::builtins::natural(self, a, b)),
        }
    }

    /// Restore the order of a sorted collection after a change.
    pub fn resort(&mut self, o: &ObjRef, pos: u32) -> R<()> {
        let Some(cmp) = self.sorted_info(o) else { return Ok(()) };
        let is_set = o.borrow().kind == ObjKind::Set;
        let pairs: Vec<(Rc<str>, V)> = o.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let keyed: Vec<(V, Rc<str>, V)> = pairs.into_iter().map(|(k, v)| (if is_set { v.clone() } else { map_key(&k) }, k, v)).collect();
        let mut items: Vec<V> = keyed.iter().map(|(sk, _, _)| sk.clone()).collect();
        // sort indices by the comparison of their sort keys
        let mut idx: Vec<usize> = (0..items.len()).collect();
        let snapshot = items.clone();
        let mut err = None;
        let mut sorted_idx = Vec::with_capacity(idx.len());
        // insertion sort keeps it simple and stable for script sized collections
        for i0 in idx.drain(..) {
            let mut at = sorted_idx.len();
            for (p, &j) in sorted_idx.iter().enumerate() {
                match self.sort_cmp(&cmp, &snapshot[i0], &snapshot[j], pos) {
                    Ok(Ordering::Less) => {
                        at = p;
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        err = Some(e);
                        break;
                    }
                }
            }
            sorted_idx.insert(at, i0);
        }
        if let Some(e) = err {
            return Err(e);
        }
        items.clear();
        let mut b = o.borrow_mut();
        b.props.clear();
        for j in sorted_idx {
            let (_, k, v) = &keyed[j];
            b.props.set(k, v.clone());
        }
        Ok(())
    }

    /// Navigation methods of `TreeMap` / `TreeSet`.
    pub fn sorted_method(&mut self, o: &ObjRef, recv: &V, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
        let Some(cmp) = self.sorted_info(o) else { return Ok(None) };
        let is_set = o.borrow().kind == ObjKind::Set;
        let entries: Vec<(V, V)> = o.borrow().props.iter().map(|(k, v)| if is_set { (v.clone(), v.clone()) } else { (map_key(k), v.clone()) }).collect();
        let a0 = args.first().cloned().unwrap_or(V::Null);
        let entry = |k: &V, v: &V, it: &Interp| crate::builtins::entry(&it.key_string(k), v.clone());
        let pick = |it: &mut Interp, pred: &dyn Fn(Ordering) -> bool, last: bool| -> R<Option<(V, V)>> {
            let mut found = None;
            for (k, v) in &entries {
                let o = it.sort_cmp(&cmp, k, &a0, pos)?;
                if pred(o) {
                    found = Some((k.clone(), v.clone()));
                    if !last {
                        break;
                    }
                }
            }
            Ok(found)
        };
        let key_or_null = |r: Option<(V, V)>| r.map(|(k, _)| k).unwrap_or(V::Null);
        let sub = |it: &mut Interp, keep: &dyn Fn(&mut Interp, &V) -> R<bool>| -> R<V> {
            let mut kept = vec![];
            for (k, v) in &entries {
                if keep(it, k)? {
                    kept.push((k.clone(), v.clone()));
                }
            }
            let out = if is_set {
                crate::builtins::make_set(it, kept.into_iter().map(|(k, _)| k).collect())
            } else {
                let mut p = PropMap::default();
                for (k, v) in kept {
                    p.set(&it.key_string(&k), v);
                }
                V::obj(p)
            };
            it.register_sorted(&out, cmp.clone());
            Ok(out)
        };
        Ok(Some(match (is_set, name) {
            (false, "firstKey") | (true, "first") => match entries.first() {
                Some((k, _)) => k.clone(),
                None => return self.throw("NoSuchElementException", String::new(), Some(pos)).map(Some),
            },
            (false, "lastKey") | (true, "last") => match entries.last() {
                Some((k, _)) => k.clone(),
                None => return self.throw("NoSuchElementException", String::new(), Some(pos)).map(Some),
            },
            (false, "firstEntry") => entries.first().map(|(k, v)| entry(k, v, self)).unwrap_or(V::Null),
            (false, "lastEntry") => entries.last().map(|(k, v)| entry(k, v, self)).unwrap_or(V::Null),
            (_, "floorKey" | "floor") => key_or_null(pick(self, &|o| o != Ordering::Greater, true)?),
            (_, "lowerKey" | "lower") => key_or_null(pick(self, &|o| o == Ordering::Less, true)?),
            (_, "ceilingKey" | "ceiling") => key_or_null(pick(self, &|o| o != Ordering::Less, false)?),
            (_, "higherKey" | "higher") => key_or_null(pick(self, &|o| o == Ordering::Greater, false)?),
            (false, "floorEntry") => pick(self, &|o| o != Ordering::Greater, true)?.map(|(k, v)| entry(&k, &v, self)).unwrap_or(V::Null),
            (false, "ceilingEntry") => pick(self, &|o| o != Ordering::Less, false)?.map(|(k, v)| entry(&k, &v, self)).unwrap_or(V::Null),
            (false, "headMap") | (true, "headSet") => {
                let incl = args.get(1).is_some_and(|v| self.truthy(v));
                let c = cmp.clone();
                sub(self, &|it, k| Ok(matches!(it.sort_cmp(&c, k, &a0, pos)?, Ordering::Less) || (incl && it.sort_cmp(&c, k, &a0, pos)? == Ordering::Equal)))?
            }
            (false, "tailMap") | (true, "tailSet") => {
                let incl = args.get(1).map(|v| self.truthy(v)).unwrap_or(true);
                let c = cmp.clone();
                sub(self, &|it, k| Ok(matches!(it.sort_cmp(&c, k, &a0, pos)?, Ordering::Greater) || (incl && it.sort_cmp(&c, k, &a0, pos)? == Ordering::Equal)))?
            }
            (false, "subMap") | (true, "subSet") => {
                let (hi, hi_incl, lo_incl) = if args.len() >= 4 { (args[2].clone(), self.truthy(&args[3]), self.truthy(&args[1])) } else { (args.get(1).cloned().unwrap_or(V::Null), false, true) };
                let c = cmp.clone();
                sub(self, &|it, k| {
                    let lo = it.sort_cmp(&c, k, &a0, pos)?;
                    let h = it.sort_cmp(&c, k, &hi, pos)?;
                    Ok((lo == Ordering::Greater || (lo_incl && lo == Ordering::Equal)) && (h == Ordering::Less || (hi_incl && h == Ordering::Equal)))
                })?
            }
            (false, "pollFirstEntry" | "pollLastEntry") | (true, "pollFirst" | "pollLast") => {
                let first = name.contains("First");
                let e = if first { entries.first() } else { entries.last() };
                match e {
                    Some((k, v)) => {
                        let ks = if is_set { o.borrow().props.keys().nth(if first { 0 } else { entries.len() - 1 }).cloned() } else { Some(Rc::from(self.key_string(k).as_str())) };
                        if let Some(ks) = ks {
                            o.borrow_mut().props.remove(&ks);
                        }
                        if is_set {
                            k.clone()
                        } else {
                            entry(k, v, self)
                        }
                    }
                    None => V::Null,
                }
            }
            (false, "descendingMap") => {
                let mut p = PropMap::default();
                for (k, v) in entries.iter().rev() {
                    p.set(&self.key_string(k), v.clone());
                }
                V::obj(p)
            }
            (false, "descendingKeySet") | (true, "descendingSet") => V::arr(entries.iter().rev().map(|(k, _)| k.clone()).collect()),
            (false, "navigableKeySet" | "keySet") => V::arr(entries.iter().map(|(k, _)| k.clone()).collect()),
            (_, "comparator") => cmp.clone().unwrap_or(V::Null),
            _ => {
                let _ = recv;
                return Ok(None);
            }
        }))
    }
}

/// A map key as a value (numeric keys of `TreeMap<Integer, ..>` come back as numbers).
fn map_key(k: &str) -> V {
    if let Ok(i) = k.parse::<i64>() {
        if i.to_string() == k {
            return V::Int(i);
        }
    }
    V::str(k)
}

// ====================================================================== char / remove helpers

impl Interp {
    /// Java runtime helpers (`%char`, `%chr`, `%ord`, `%jcast`, `%remove_object`).
    pub fn java_internal(&mut self, recv: &V, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
        let a0 = args.first().cloned().unwrap_or(V::Null);
        Ok(Some(match name {
            "%char" | "%jcast" => a0,
            "%chr" => match &a0 {
                V::Str(s) if s.chars().count() == 1 => a0.clone(),
                other => {
                    let code = other.as_f64().unwrap_or(0.0) as i64 as u32 & 0xFFFF;
                    V::string(char::from_u32(code).map(|c| c.to_string()).unwrap_or_else(|| "\u{FFFD}".into()))
                }
            },
            "%ord" => match &a0 {
                V::Str(s) => V::Int(s.chars().next().map(|c| c as i64).unwrap_or(0)),
                V::Null => return self.throw("NullPointerException", "Cannot unbox null value".into(), Some(pos)).map(Some),
                other => other.clone(),
            },
            "%remove_object" => match recv {
                V::Arr(a) => {
                    let idx = a.borrow().iter().position(|x| crate::interp::deep_eq(x, &a0));
                    match idx {
                        Some(i) => {
                            a.borrow_mut().remove(i);
                            V::Bool(true)
                        }
                        None => V::Bool(false),
                    }
                }
                other => self.call_method(other, "remove", args.to_vec(), pos)?,
            },
            _ => return Ok(None),
        }))
    }
}
