//! Date, time and duration arithmetic (`xs:dateTime - xs:dateTime`, `xs:date +
//! xs:yearMonthDuration`, `xs:dayTimeDuration * 2`...), duration component accessors
//! and timezone adjustment.
//!
//! Durations are `(months, microseconds)`; dates and times are microseconds since the
//! epoch plus an optional timezone offset in minutes. Values without a timezone are
//! treated as UTC (the implicit timezone).

use std::cmp::Ordering;

use camel_core::util_time::{civil_from_epoch, epoch_from_civil};

use super::types::{Atomic, Decimal, XqError, XqResult};

const US: i64 = 1_000_000;
const DAY_US: i64 = 86_400 * US;

/// A duration: total months and total microseconds (both carry the sign).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Duration {
    pub months: i64,
    pub micros: i64,
}

fn bad(s: &str, what: &str) -> XqError {
    XqError::new("FORG0001", format!("Invalid {} value '{}'", what, s))
}

/// Parse fractional seconds `12.345` into microseconds.
fn parse_seconds(s: &str) -> Option<i64> {
    let (i, f) = s.split_once('.').unwrap_or((s, ""));
    if i.is_empty() || !i.bytes().all(|b| b.is_ascii_digit()) || !f.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut frac = f.to_string();
    frac.truncate(6);
    while frac.len() < 6 {
        frac.push('0');
    }
    Some(i.parse::<i64>().ok()? * US + frac.parse::<i64>().ok()?)
}

impl Duration {
    pub fn parse(s: &str) -> XqResult<Duration> {
        let t = s.trim();
        let (neg, body) = match t.strip_prefix('-') {
            Some(b) => (true, b),
            None => (false, t),
        };
        let body = body.strip_prefix('P').ok_or_else(|| bad(s, "duration"))?;
        if body.is_empty() || body.ends_with('T') {
            return Err(bad(s, "duration"));
        }
        let (date, time) = body.split_once('T').unwrap_or((body, ""));
        let mut d = Duration::default();
        let mut num = String::new();
        for c in date.chars() {
            match c {
                '0'..='9' => num.push(c),
                'Y' | 'M' | 'D' => {
                    let v: i64 = num.parse().map_err(|_| bad(s, "duration"))?;
                    num.clear();
                    match c {
                        'Y' => d.months += v * 12,
                        'M' => d.months += v,
                        _ => d.micros += v * DAY_US,
                    }
                }
                _ => return Err(bad(s, "duration")),
            }
        }
        for c in time.chars() {
            match c {
                '0'..='9' | '.' => num.push(c),
                'H' | 'M' => {
                    let v: i64 = num.parse().map_err(|_| bad(s, "duration"))?;
                    num.clear();
                    d.micros += v * if c == 'H' { 3600 * US } else { 60 * US };
                }
                'S' => {
                    d.micros += parse_seconds(&num).ok_or_else(|| bad(s, "duration"))?;
                    num.clear();
                }
                _ => return Err(bad(s, "duration")),
            }
        }
        if !num.is_empty() {
            return Err(bad(s, "duration"));
        }
        if neg {
            d.months = -d.months;
            d.micros = -d.micros;
        }
        Ok(d)
    }

    pub fn of(a: &Atomic) -> Option<Duration> {
        match a {
            Atomic::Duration(s) => Duration::parse(s).ok(),
            _ => None,
        }
    }

    pub fn is_year_month(&self) -> bool {
        self.micros == 0 && self.months != 0
    }

    /// Canonical lexical form (`P1Y2M`, `P3DT4H5M6.5S`, `PT0S`).
    pub fn format(&self) -> String {
        if self.months == 0 && self.micros == 0 {
            return "PT0S".into();
        }
        let neg = self.months < 0 || self.micros < 0;
        let m = self.months.abs();
        let us = self.micros.abs();
        let mut s = String::new();
        if neg {
            s.push('-');
        }
        s.push('P');
        let (y, mo) = (m / 12, m % 12);
        if y > 0 {
            s.push_str(&format!("{}Y", y));
        }
        if mo > 0 {
            s.push_str(&format!("{}M", mo));
        }
        let days = us / DAY_US;
        let rest = us % DAY_US;
        if days > 0 {
            s.push_str(&format!("{}D", days));
        }
        if rest > 0 {
            s.push('T');
            let h = rest / (3600 * US);
            let mi = rest % (3600 * US) / (60 * US);
            let sec = rest % (60 * US);
            if h > 0 {
                s.push_str(&format!("{}H", h));
            }
            if mi > 0 {
                s.push_str(&format!("{}M", mi));
            }
            if sec > 0 {
                let whole = sec / US;
                let frac = sec % US;
                if frac == 0 {
                    s.push_str(&format!("{}S", whole));
                } else {
                    let f = format!("{:06}", frac);
                    s.push_str(&format!("{}.{}S", whole, f.trim_end_matches('0')));
                }
            }
        }
        s
    }

    pub fn to_atomic(self) -> Atomic {
        Atomic::Duration(self.format())
    }

    fn neg(self) -> Duration {
        Duration { months: -self.months, micros: -self.micros }
    }
}

/// A date / time / dateTime instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instant {
    /// Local wall-clock time as microseconds since 1970-01-01T00:00:00.
    pub local: i64,
    /// Timezone offset in minutes.
    pub tz: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    DateTime,
    Date,
    Time,
}

fn split_tz(s: &str) -> XqResult<(&str, Option<i32>)> {
    if let Some(b) = s.strip_suffix('Z') {
        return Ok((b, Some(0)));
    }
    if s.len() > 6 {
        let (a, b) = s.split_at(s.len() - 6);
        if (b.starts_with('+') || b.starts_with('-')) && &b[3..4] == ":" {
            let h: i32 = b[1..3].parse().map_err(|_| bad(s, "timezone"))?;
            let m: i32 = b[4..6].parse().map_err(|_| bad(s, "timezone"))?;
            let v = h * 60 + m;
            return Ok((a, Some(if b.starts_with('-') { -v } else { v })));
        }
    }
    Ok((s, None))
}

fn parse_time_of_day(t: &str, s: &str) -> XqResult<i64> {
    let p: Vec<&str> = t.split(':').collect();
    if p.len() != 3 {
        return Err(bad(s, "time"));
    }
    let h: i64 = p[0].parse().map_err(|_| bad(s, "time"))?;
    let m: i64 = p[1].parse().map_err(|_| bad(s, "time"))?;
    let sec = parse_seconds(p[2]).ok_or_else(|| bad(s, "time"))?;
    Ok(h * 3600 * US + m * 60 * US + sec)
}

fn parse_date_part(d: &str, s: &str) -> XqResult<i64> {
    let neg = d.starts_with('-');
    let parts: Vec<&str> = d.trim_start_matches('-').splitn(3, '-').collect();
    if parts.len() != 3 {
        return Err(bad(s, "date"));
    }
    let y: i64 = parts[0].parse().map_err(|_| bad(s, "date"))?;
    let m: u32 = parts[1].parse().map_err(|_| bad(s, "date"))?;
    let day: u32 = parts[2].parse().map_err(|_| bad(s, "date"))?;
    Ok(epoch_from_civil(if neg { -y } else { y }, m, day, 0, 0, 0) * US)
}

impl Instant {
    pub fn parse(s: &str, kind: Kind) -> XqResult<Instant> {
        let (body, tz) = split_tz(s.trim())?;
        let local = match kind {
            Kind::DateTime => {
                let (d, t) = body.split_once('T').ok_or_else(|| bad(s, "dateTime"))?;
                parse_date_part(d, s)? + parse_time_of_day(t, s)?
            }
            Kind::Date => parse_date_part(body, s)?,
            Kind::Time => parse_time_of_day(body, s)?,
        };
        Ok(Instant { local, tz })
    }

    pub fn of(a: &Atomic) -> Option<(Instant, Kind)> {
        let (s, k) = match a {
            Atomic::DateTime(s) => (s, Kind::DateTime),
            Atomic::Date(s) => (s, Kind::Date),
            Atomic::Time(s) => (s, Kind::Time),
            _ => return None,
        };
        Instant::parse(s, k).ok().map(|i| (i, k))
    }

    /// UTC microseconds (local time minus the offset).
    pub fn utc(&self) -> i64 {
        self.local - self.tz.unwrap_or(0) as i64 * 60 * US
    }

    pub fn format(&self, kind: Kind) -> String {
        let secs = self.local.div_euclid(US);
        let frac = self.local.rem_euclid(US);
        let (y, mo, d, h, mi, se) = civil_from_epoch(secs);
        let fs = if frac == 0 { String::new() } else { format!(".{}", format!("{:06}", frac).trim_end_matches('0')) };
        let date = if y < 0 { format!("-{:04}-{:02}-{:02}", -y, mo, d) } else { format!("{:04}-{:02}-{:02}", y, mo, d) };
        let time = format!("{:02}:{:02}:{:02}{}", h, mi, se, fs);
        let tz = match self.tz {
            None => String::new(),
            Some(0) => "Z".into(),
            Some(m) => format!("{}{:02}:{:02}", if m < 0 { '-' } else { '+' }, m.abs() / 60, m.abs() % 60),
        };
        match kind {
            Kind::DateTime => format!("{}T{}{}", date, time, tz),
            Kind::Date => format!("{}{}", date, tz),
            Kind::Time => format!("{}{}", time, tz),
        }
    }

    pub fn to_atomic(self, kind: Kind) -> Atomic {
        let s = self.format(kind);
        match kind {
            Kind::DateTime => Atomic::DateTime(s),
            Kind::Date => Atomic::Date(s),
            Kind::Time => Atomic::Time(s),
        }
    }

    /// Add months (clamping the day to the month length) then microseconds.
    fn plus(&self, d: Duration, kind: Kind) -> Instant {
        let mut local = self.local;
        if d.months != 0 && kind != Kind::Time {
            let secs = local.div_euclid(US);
            let frac = local.rem_euclid(US);
            let (y, mo, day, h, mi, se) = civil_from_epoch(secs);
            let total = y * 12 + (mo as i64 - 1) + d.months;
            let (ny, nm) = (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32);
            let dim = days_in_month(ny, nm);
            local = epoch_from_civil(ny, nm, day.min(dim), h, mi, se) * US + frac;
        }
        local += d.micros;
        match kind {
            Kind::Time => local = local.rem_euclid(DAY_US),
            Kind::Date => local = local.div_euclid(DAY_US) * DAY_US,
            Kind::DateTime => {}
        }
        Instant { local, tz: self.tz }
    }
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

/// Arithmetic involving dates, times or durations; `None` when neither operand is one.
pub fn arith(op: char, x: &Atomic, y: &Atomic) -> Option<XqResult<Atomic>> {
    let xi = Instant::of(x);
    let yi = Instant::of(y);
    let xd = Duration::of(x);
    let yd = Duration::of(y);
    if xi.is_none() && yi.is_none() && xd.is_none() && yd.is_none() {
        return None;
    }
    let type_err = || Some(Err(XqError::type_err(format!("Operator '{}' is not defined for {} and {}", op, x.type_of().name(), y.type_of().name()))));
    let num = |a: &Atomic| -> Option<f64> {
        if a.is_numeric() {
            Some(a.as_f64())
        } else if let Atomic::Untyped(s) = a {
            s.trim().parse().ok()
        } else {
            None
        }
    };
    Some(Ok(match (op, xi, yi, xd, yd) {
        ('-', Some((a, ka)), Some((b, kb)), _, _) if ka == kb => Duration { months: 0, micros: a.utc() - b.utc() }.to_atomic(),
        ('+', Some((a, k)), _, _, Some(d)) | ('+', _, Some((a, k)), Some(d), _) => a.plus(d, k).to_atomic(k),
        ('-', Some((a, k)), _, _, Some(d)) => a.plus(d.neg(), k).to_atomic(k),
        ('+', _, _, Some(a), Some(b)) => Duration { months: a.months + b.months, micros: a.micros + b.micros }.to_atomic(),
        ('-', _, _, Some(a), Some(b)) => Duration { months: a.months - b.months, micros: a.micros - b.micros }.to_atomic(),
        ('/', _, _, Some(a), Some(b)) => {
            let (n, d) = if a.months != 0 || b.months != 0 { (a.months as i128, b.months as i128) } else { (a.micros as i128, b.micros as i128) };
            if d == 0 {
                return Some(Err(XqError::new("FOAR0001", "Division of a duration by a zero duration")));
            }
            let q = Decimal::parse(&n.to_string()).unwrap_or_default().div(&Decimal::parse(&d.to_string()).unwrap_or_default()).unwrap_or_default();
            Atomic::Dec(q)
        }
        ('*', _, _, Some(a), None) | ('*', _, _, None, Some(a)) => {
            let Some(f) = num(if xd.is_some() { y } else { x }) else { return type_err() };
            if f.is_nan() || f.is_infinite() {
                return Some(Err(XqError::new("FODT0002", "Duration overflow / NaN factor")));
            }
            Duration { months: (a.months as f64 * f).round() as i64, micros: (a.micros as f64 * f).round() as i64 }.to_atomic()
        }
        ('/', _, _, Some(a), None) => {
            let Some(f) = num(y) else { return type_err() };
            if f == 0.0 || f.is_nan() {
                return Some(Err(XqError::new("FODT0002", "Division of a duration by zero / NaN")));
            }
            Duration { months: (a.months as f64 / f).round() as i64, micros: (a.micros as f64 / f).round() as i64 }.to_atomic()
        }
        _ => return type_err(),
    }))
}

/// Compare two dates / times / durations of the same kind.
pub fn compare(a: &Atomic, b: &Atomic) -> Option<Ordering> {
    if let (Some((x, kx)), Some((y, ky))) = (Instant::of(a), Instant::of(b)) {
        if kx == ky {
            return Some(x.utc().cmp(&y.utc()));
        }
        return None;
    }
    if let (Some(x), Some(y)) = (Duration::of(a), Duration::of(b)) {
        // months and microseconds compared together (30.4375 days per month for ordering)
        let k = |d: Duration| d.months as i128 * 2_629_800 * US as i128 + d.micros as i128;
        if x == y {
            return Some(Ordering::Equal);
        }
        return Some(k(x).cmp(&k(y)));
    }
    None
}

/// Duration component accessors (`years-from-duration`...).
pub fn duration_component(a: &Atomic, part: &str) -> XqResult<Atomic> {
    let d = Duration::of(a).ok_or_else(|| XqError::type_err(format!("Expected a duration, found {}", a.type_of().name())))?;
    let sign = if d.months < 0 || d.micros < 0 { -1 } else { 1 };
    let (m, us) = (d.months.abs(), d.micros.abs());
    Ok(match part {
        "years" => Atomic::Int(sign * (m / 12)),
        "months" => Atomic::Int(sign * (m % 12)),
        "days" => Atomic::Int(sign * (us / DAY_US)),
        "hours" => Atomic::Int(sign * (us % DAY_US / (3600 * US))),
        "minutes" => Atomic::Int(sign * (us % (3600 * US) / (60 * US))),
        _ => {
            let s = us % (60 * US);
            let v = Decimal::parse(&format!("{}.{:06}", s / US, s % US)).unwrap_or_default();
            Atomic::Dec(if sign < 0 { v.neg() } else { v })
        }
    })
}

/// `adjust-*-to-timezone`: `tz` = None removes the timezone.
pub fn adjust(a: &Atomic, tz: Option<Option<i32>>) -> XqResult<Atomic> {
    let (i, k) = Instant::of(a).ok_or_else(|| XqError::type_err(format!("Expected a date/time, found {}", a.type_of().name())))?;
    let target = tz.unwrap_or(Some(0));
    let out = match (i.tz, target) {
        (_, None) => Instant { local: i.local, tz: None },
        (None, Some(t)) => Instant { local: i.local, tz: Some(t) },
        (Some(_), Some(t)) => Instant { local: i.utc() + t as i64 * 60 * US, tz: Some(t) },
    };
    let out = if k == Kind::Date { Instant { local: out.local.div_euclid(DAY_US) * DAY_US, tz: out.tz } } else { out };
    Ok(out.to_atomic(k))
}

/// `fn:dateTime($date, $time)`.
pub fn date_time(d: &Atomic, t: &Atomic) -> XqResult<Atomic> {
    let (di, _) = Instant::of(d).ok_or_else(|| XqError::type_err("dateTime() expects an xs:date"))?;
    let (ti, _) = Instant::of(t).ok_or_else(|| XqError::type_err("dateTime() expects an xs:time"))?;
    let tz = match (di.tz, ti.tz) {
        (Some(a), Some(b)) if a != b => return Err(XqError::new("FORG0008", "dateTime(): the date and time have different timezones")),
        (a, b) => a.or(b),
    };
    Ok(Instant { local: di.local + ti.local, tz }.to_atomic(Kind::DateTime))
}

/// Timezone of a date/time as a dayTimeDuration (None when absent).
pub fn timezone_of(a: &Atomic) -> Option<Atomic> {
    let (i, _) = Instant::of(a)?;
    i.tz.map(|m| Duration { months: 0, micros: m as i64 * 60 * US }.to_atomic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> Atomic {
        Atomic::DateTime(s.into())
    }

    #[test]
    fn date_arithmetic() {
        let r = arith('-', &dt("2024-03-01T12:00:00Z"), &dt("2024-02-28T10:30:00Z")).unwrap().unwrap();
        assert_eq!(r.to_string(), "P2DT1H30M");
        let r = arith('+', &Atomic::Date("2024-01-31".into()), &Atomic::Duration("P1M".into())).unwrap().unwrap();
        assert_eq!(r.to_string(), "2024-02-29");
        let r = arith('-', &dt("2024-01-01T00:00:00+02:00"), &Atomic::Duration("PT90M".into())).unwrap().unwrap();
        assert_eq!(r.to_string(), "2023-12-31T22:30:00+02:00");
        let r = arith('*', &Atomic::Duration("PT1H30M".into()), &Atomic::Int(3)).unwrap().unwrap();
        assert_eq!(r.to_string(), "PT4H30M");
        let r = arith('/', &Atomic::Duration("P1Y".into()), &Atomic::Duration("P3M".into())).unwrap().unwrap();
        assert_eq!(r.to_string(), "4");
        let r = arith('+', &Atomic::Time("23:30:00".into()), &Atomic::Duration("PT1H".into())).unwrap().unwrap();
        assert_eq!(r.to_string(), "00:30:00");
        assert_eq!(Duration::parse("-P1DT0.5S").unwrap().format(), "-P1DT0.5S");
        assert_eq!(compare(&Atomic::Duration("PT60M".into()), &Atomic::Duration("PT1H".into())), Some(Ordering::Equal));
        assert_eq!(adjust(&dt("2024-01-01T10:00:00+02:00"), Some(Some(-300))).unwrap().to_string(), "2024-01-01T03:00:00-05:00");
    }
}
