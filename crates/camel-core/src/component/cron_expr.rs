//! Cron expressions (Quartz and Unix syntax) and next-fire-time computation.
//!
//! Pure functions, no threads: [`CronExpression::parse`] then
//! [`CronExpression::next_after`] (epoch seconds) or [`CronExpression::next_after_millis`].
//!
//! Supported syntax:
//!
//! - **Quartz**, 6 or 7 fields: `sec min hour day-of-month month day-of-week [year]`.
//!   Day-of-week is `1-7` (`SUN`=1 ... `SAT`=7).
//! - **Unix**, 5 fields: `min hour day-of-month month day-of-week` (seconds = 0).
//!   Day-of-week is `0-7` (`0` and `7` are Sunday).
//! - Per field: `*`, `?` (day fields), lists `a,b`, ranges `a-b` (wrapping ranges like
//!   `FRI-MON` or `22-2` are allowed), steps `*/5`, `0/5`, `10-30/5`, month names
//!   `JAN`-`DEC`, day names `SUN`-`SAT` (case-insensitive).
//! - Day-of-month: `L` (last day), `L-3` (third-to-last day), `LW` (last weekday),
//!   `15W` (weekday nearest the 15th, not crossing the month).
//! - Day-of-week: `5L` (last Thursday in Quartz numbering), `L` alone (Saturday),
//!   `6#3` (third Friday).
//! - Macros: `@yearly`/`@annually`, `@monthly`, `@weekly`, `@daily`/`@midnight`, `@hourly`.
//!
//! When both day-of-month and day-of-week are restricted (neither `*` nor `?`) a day
//! matches when either matches (Unix semantics; Quartz rejects such expressions).
//!
//! [`CronExpression::parse`] evaluates in UTC; [`CronExpression::parse_with_time_zone`] and
//! [`CronExpression::with_time_zone`] take any [`TimeZone`]: fixed offsets (`UTC`, `GMT+02:00`,
//! `UTC-5`, `+0530`) and region ids read from the zoneinfo database (`Europe/Paris`).
//! Daylight saving transitions are handled like Quartz (`java.util.Calendar`): a fire time that
//! falls in a spring-forward gap fires once, shifted forward by the gap length (02:30 becomes
//! 03:30), and a fire time in a fall-back overlap fires once, at the later (standard time)
//! occurrence.

use std::fmt;
use std::sync::Arc;

use crate::error::{CamelError, Result};
use crate::tz::TimeZone;
use crate::util_time::{civil_from_epoch, epoch_from_civil};

/// Highest year considered when searching for the next fire time (as in Quartz).
pub const MAX_YEAR: i64 = 2199;
const MIN_YEAR: i64 = 1970;

/// Day-of-month specification.
#[derive(Clone, Debug, Default, PartialEq)]
struct DomSpec {
    /// Unrestricted (`*` or `?`).
    any: bool,
    /// Explicit days (bit n = day n, 1..=31).
    days: u64,
    /// `L` / `L-n`: offsets from the last day of month.
    last_offsets: Vec<u32>,
    /// `LW`.
    last_weekday: bool,
    /// `nW`.
    nearest_weekday: Vec<u32>,
}

/// Day-of-week specification (weekday 0 = Sunday).
#[derive(Clone, Debug, Default, PartialEq)]
struct DowSpec {
    any: bool,
    /// Explicit weekdays (bit n, 0 = Sunday).
    days: u64,
    /// `nL`: last given weekday of the month.
    last_of: Vec<u32>,
    /// `n#k`: k-th given weekday of the month.
    nth: Vec<(u32, u32)>,
}

/// A parsed cron expression. See the [module documentation](self) for the syntax.
#[derive(Clone, PartialEq)]
pub struct CronExpression {
    text: String,
    seconds: u64,
    minutes: u64,
    hours: u64,
    dom: DomSpec,
    months: u64,
    dow: DowSpec,
    /// None = every year.
    years: Option<Vec<i64>>,
    zone: Arc<TimeZone>,
}

impl fmt::Debug for CronExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CronExpression({:?}, zone={})", self.text, self.zone.id())
    }
}

impl fmt::Display for CronExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

const MONTH_NAMES: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
const DAY_NAMES: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

fn err(expr: &str, msg: impl fmt::Display) -> CamelError {
    CamelError::new(crate::error::kinds::PARSE, format!("Invalid cron expression '{}': {}", expr, msg))
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Second,
    Minute,
    Hour,
    DayOfMonth,
    Month,
    DayOfWeek,
    Year,
}

impl Field {
    fn name(self) -> &'static str {
        match self {
            Field::Second => "seconds",
            Field::Minute => "minutes",
            Field::Hour => "hours",
            Field::DayOfMonth => "day-of-month",
            Field::Month => "month",
            Field::DayOfWeek => "day-of-week",
            Field::Year => "year",
        }
    }
}

struct FieldParser<'a> {
    expr: &'a str,
    field: Field,
    min: i64,
    max: i64,
    /// Unix day-of-week numbering (0-7, Sunday = 0 or 7).
    unix_dow: bool,
}

impl<'a> FieldParser<'a> {
    fn error(&self, msg: impl fmt::Display) -> CamelError {
        err(self.expr, format!("{} field: {}", self.field.name(), msg))
    }

    /// Parse a single value (number or name), returned in the field's raw numbering.
    fn value(&self, s: &str) -> Result<i64> {
        let up = s.trim().to_ascii_uppercase();
        if up.is_empty() {
            return Err(self.error("missing value"));
        }
        let named = match self.field {
            Field::Month => MONTH_NAMES.iter().position(|n| *n == up).map(|i| i as i64 + 1),
            Field::DayOfWeek => DAY_NAMES.iter().position(|n| *n == up).map(|i| if self.unix_dow { i as i64 } else { i as i64 + 1 }),
            _ => None,
        };
        let v = match named {
            Some(v) => v,
            None => up.parse::<i64>().map_err(|_| self.error(format!("invalid value '{}'", s)))?,
        };
        if v < self.min || v > self.max {
            return Err(self.error(format!("value {} out of range {}-{}", v, self.min, self.max)));
        }
        Ok(v)
    }

    /// Parse a list item into the set of raw values it denotes.
    fn item(&self, item: &str) -> Result<Vec<i64>> {
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => {
                let step: i64 = s.trim().parse().map_err(|_| self.error(format!("invalid step '{}'", s)))?;
                if step <= 0 {
                    return Err(self.error(format!("step must be positive: '{}'", s)));
                }
                (r.trim(), step)
            }
            None => (item.trim(), 1),
        };
        let (start, end) = if range == "*" || range == "?" {
            (self.min, self.max)
        } else if let Some((a, b)) = range.split_once('-') {
            (self.value(a)?, self.value(b)?)
        } else {
            let v = self.value(range)?;
            // `5/15` means from 5 to the max with step 15
            (v, if item.contains('/') { self.max } else { v })
        };
        let mut out = Vec::new();
        if start <= end {
            let mut v = start;
            while v <= end {
                out.push(v);
                v += step;
            }
        } else {
            // wrapping range, e.g. 22-2 or FRI-MON
            let span = self.max - self.min + 1;
            let len = (self.max - start + 1) + (end - self.min + 1);
            let mut i = 0;
            while i < len {
                out.push(self.min + (start - self.min + i).rem_euclid(span));
                i += step;
            }
        }
        Ok(out)
    }

    fn bits(&self, text: &str) -> Result<u64> {
        let mut bits = 0u64;
        for item in text.split(',') {
            for v in self.item(item)? {
                bits |= 1 << v;
            }
        }
        Ok(bits)
    }
}

fn is_unrestricted(s: &str) -> bool {
    s == "*" || s == "?"
}

impl CronExpression {
    /// Parse a Quartz (6/7 fields), Unix (5 fields) or macro (`@daily`) cron expression.
    pub fn parse(expression: &str) -> Result<CronExpression> {
        let text = expression.trim();
        let expanded = match text.to_ascii_lowercase().as_str() {
            "@yearly" | "@annually" => "0 0 0 1 1 ?".to_string(),
            "@monthly" => "0 0 0 1 * ?".to_string(),
            "@weekly" => "0 0 0 ? * 1".to_string(),
            "@daily" | "@midnight" => "0 0 0 * * ?".to_string(),
            "@hourly" => "0 0 * * * ?".to_string(),
            s if s.starts_with('@') => return Err(err(text, "unknown macro")),
            _ => text.to_string(),
        };
        let fields: Vec<&str> = expanded.split_whitespace().collect();
        let (unix, sec, rest): (bool, &str, &[&str]) = match fields.len() {
            5 => (true, "0", &fields[..]),
            6 | 7 => (false, fields[0], &fields[1..]),
            n => return Err(err(text, format!("expected 5, 6 or 7 fields but found {}", n))),
        };
        let p = |field, min, max| FieldParser { expr: text, field, min, max, unix_dow: unix };
        let seconds = p(Field::Second, 0, 59).bits(sec)?;
        let minutes = p(Field::Minute, 0, 59).bits(rest[0])?;
        let hours = p(Field::Hour, 0, 23).bits(rest[1])?;
        let dom = Self::parse_dom(text, rest[2])?;
        let months = p(Field::Month, 1, 12).bits(rest[3])?;
        let dow = Self::parse_dow(text, rest[4], unix)?;
        let years = match rest.get(5) {
            None => None,
            Some(y) if is_unrestricted(y) => None,
            Some(y) => {
                let fp = p(Field::Year, MIN_YEAR, MAX_YEAR);
                let mut v = Vec::new();
                for item in y.split(',') {
                    v.extend(fp.item(item)?);
                }
                v.sort_unstable();
                v.dedup();
                Some(v)
            }
        };
        if rest[2] == "?" && rest[4] == "?" {
            return Err(err(text, "'?' can only be used in one of day-of-month and day-of-week"));
        }
        Ok(CronExpression { text: text.to_string(), seconds, minutes, hours, dom, months, dow, years, zone: Arc::new(TimeZone::utc()) })
    }

    /// Parse an expression evaluated in the given time zone (see [`parse_time_zone`]).
    pub fn parse_with_time_zone(expression: &str, time_zone: Option<&str>) -> Result<CronExpression> {
        let c = CronExpression::parse(expression)?;
        match time_zone {
            Some(tz) if !tz.trim().is_empty() => Ok(c.with_time_zone(parse_zone(tz)?)),
            _ => Ok(c),
        }
    }

    /// Like [`CronExpression::parse_with_time_zone`] but without a time zone the expression is
    /// evaluated in the default zone ([`TimeZone::local`]), like Quartz uses the JVM default.
    pub fn parse_with_default_zone(expression: &str, time_zone: Option<&str>) -> Result<CronExpression> {
        match time_zone {
            Some(tz) if !tz.trim().is_empty() => Self::parse_with_time_zone(expression, Some(tz)),
            _ => Ok(CronExpression::parse(expression)?.with_time_zone(TimeZone::local())),
        }
    }

    fn parse_dom(expr: &str, text: &str) -> Result<DomSpec> {
        let fp = FieldParser { expr, field: Field::DayOfMonth, min: 1, max: 31, unix_dow: false };
        let mut spec = DomSpec::default();
        if is_unrestricted(text) {
            spec.any = true;
            return Ok(spec);
        }
        for item in text.split(',') {
            let up = item.trim().to_ascii_uppercase();
            if up == "L" {
                spec.last_offsets.push(0);
            } else if up == "LW" || up == "WL" {
                spec.last_weekday = true;
            } else if let Some(off) = up.strip_prefix("L-") {
                let n: u32 = off.parse().map_err(|_| fp.error(format!("invalid last-day offset '{}'", item)))?;
                if n > 30 {
                    return Err(fp.error("last-day offset must be at most 30"));
                }
                spec.last_offsets.push(n);
            } else if let Some(d) = up.strip_suffix('W') {
                let v = fp.value(d)?;
                spec.nearest_weekday.push(v as u32);
            } else {
                for v in fp.item(&up)? {
                    spec.days |= 1 << v;
                }
            }
        }
        Ok(spec)
    }

    fn parse_dow(expr: &str, text: &str, unix: bool) -> Result<DowSpec> {
        let (min, max) = if unix { (0, 7) } else { (1, 7) };
        let fp = FieldParser { expr, field: Field::DayOfWeek, min, max, unix_dow: unix };
        let norm = |v: i64| -> u32 {
            if unix {
                (v % 7) as u32
            } else {
                (v - 1) as u32
            }
        };
        let mut spec = DowSpec::default();
        if is_unrestricted(text) {
            spec.any = true;
            return Ok(spec);
        }
        for item in text.split(',') {
            let up = item.trim().to_ascii_uppercase();
            if up == "L" {
                // `L` alone in day-of-week means the last day of the week: Saturday
                spec.days |= 1 << 6;
            } else if let Some(d) = up.strip_suffix('L') {
                spec.last_of.push(norm(fp.value(d)?));
            } else if let Some((d, k)) = up.split_once('#') {
                let k: u32 = k.parse().map_err(|_| fp.error(format!("invalid '#' value in '{}'", item)))?;
                if !(1..=5).contains(&k) {
                    return Err(fp.error("the value after '#' must be between 1 and 5"));
                }
                spec.nth.push((norm(fp.value(d)?), k));
            } else {
                for v in fp.item(&up)? {
                    spec.days |= 1 << norm(v);
                }
            }
        }
        Ok(spec)
    }

    /// Evaluate the expression at a fixed UTC offset (seconds east of UTC).
    pub fn with_offset_seconds(mut self, offset: i32) -> Self {
        self.zone = Arc::new(fixed_zone(offset));
        self
    }

    /// Evaluate the expression in the given time zone.
    pub fn with_time_zone(mut self, zone: Arc<TimeZone>) -> Self {
        self.zone = zone;
        self
    }

    /// The time zone the expression is evaluated in.
    pub fn time_zone(&self) -> &Arc<TimeZone> {
        &self.zone
    }

    /// The UTC offset (seconds) of the expression's time zone at the current instant.
    pub fn offset_seconds(&self) -> i32 {
        self.zone.offset_at(crate::util_time::now_millis().div_euclid(1000))
    }

    /// The expression text as given.
    pub fn expression(&self) -> &str {
        &self.text
    }

    /// Whether the expression matches the given instant (epoch seconds).
    pub fn matches(&self, epoch_secs: i64) -> bool {
        let local = self.zone.to_local(epoch_secs);
        let (y, mo, d, h, mi, s) = civil_from_epoch(local);
        self.year_ok(y)
            && self.months & (1 << mo) != 0
            && self.day_ok(y, mo, d)
            && self.hours & (1 << h) != 0
            && self.minutes & (1 << mi) != 0
            && self.seconds & (1 << s) != 0
    }

    /// The next fire time strictly after `epoch_secs`, or None if there is none (before year 2199).
    pub fn next_after(&self, epoch_secs: i64) -> Option<i64> {
        let mut local = self.zone.to_local(epoch_secs) + 1;
        loop {
            let candidate = self.next_local_from(local)?;
            let utc = self.zone.resolve_local(candidate);
            if utc > epoch_secs {
                return Some(utc);
            }
            local = candidate + 1;
        }
    }

    /// The first matching local time (epoch seconds of the wall clock) at or after `t`.
    fn next_local_from(&self, mut t: i64) -> Option<i64> {
        loop {
            let (y, mo, d, h, mi, s) = civil_from_epoch(t);
            if y > MAX_YEAR {
                return None;
            }
            if !self.year_ok(y) {
                let next = self.years.as_ref().and_then(|ys| ys.iter().find(|&&yy| yy > y).copied())?;
                t = epoch_from_civil(next, 1, 1, 0, 0, 0);
                continue;
            }
            if self.months & (1 << mo) == 0 {
                t = match next_bit(self.months, mo + 1, 12) {
                    Some(m) => epoch_from_civil(y, m, 1, 0, 0, 0),
                    None => epoch_from_civil(y + 1, 1, 1, 0, 0, 0),
                };
                continue;
            }
            if !self.day_ok(y, mo, d) {
                t = epoch_from_civil(y, mo, d, 0, 0, 0) + 86_400;
                continue;
            }
            if self.hours & (1 << h) == 0 {
                let day_start = epoch_from_civil(y, mo, d, 0, 0, 0);
                t = match next_bit(self.hours, h + 1, 23) {
                    Some(hh) => day_start + hh as i64 * 3600,
                    None => day_start + 86_400,
                };
                continue;
            }
            if self.minutes & (1 << mi) == 0 {
                let hour_start = epoch_from_civil(y, mo, d, h, 0, 0);
                t = match next_bit(self.minutes, mi + 1, 59) {
                    Some(m) => hour_start + m as i64 * 60,
                    None => hour_start + 3600,
                };
                continue;
            }
            if self.seconds & (1 << s) == 0 {
                let minute_start = epoch_from_civil(y, mo, d, h, mi, 0);
                t = match next_bit(self.seconds, s + 1, 59) {
                    Some(ss) => minute_start + ss as i64,
                    None => minute_start + 60,
                };
                continue;
            }
            return Some(t);
        }
    }

    /// The next fire time (epoch millis) strictly after `epoch_millis`.
    pub fn next_after_millis(&self, epoch_millis: i64) -> Option<i64> {
        self.next_after(epoch_millis.div_euclid(1000)).map(|s| s * 1000)
    }

    /// The next `n` fire times (epoch seconds) after `epoch_secs`.
    pub fn next_n(&self, epoch_secs: i64, n: usize) -> Vec<i64> {
        let mut out = Vec::with_capacity(n);
        let mut t = epoch_secs;
        while out.len() < n {
            match self.next_after(t) {
                Some(x) => {
                    out.push(x);
                    t = x;
                }
                None => break,
            }
        }
        out
    }

    fn year_ok(&self, y: i64) -> bool {
        match &self.years {
            None => true,
            Some(ys) => ys.binary_search(&y).is_ok(),
        }
    }

    fn day_ok(&self, y: i64, m: u32, d: u32) -> bool {
        let dom_any = self.dom.any;
        let dow_any = self.dow.any;
        if dom_any && dow_any {
            return true;
        }
        let dom = || dom_matches(&self.dom, y, m, d);
        let dow = || dow_matches(&self.dow, y, m, d);
        match (dom_any, dow_any) {
            (false, false) => dom() || dow(),
            (false, true) => dom(),
            (true, false) => dow(),
            (true, true) => true,
        }
    }
}

fn next_bit(bits: u64, from: u32, max: u32) -> Option<u32> {
    (from..=max).find(|&i| bits & (1 << i) != 0)
}

/// Number of days of a month.
pub fn days_in_month(y: i64, m: u32) -> u32 {
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

/// Day of week of a civil date (0 = Sunday).
pub fn weekday(y: i64, m: u32, d: u32) -> u32 {
    let days = epoch_from_civil(y, m, d, 0, 0, 0).div_euclid(86_400);
    (days + 4).rem_euclid(7) as u32
}

fn nearest_weekday(y: i64, m: u32, n: u32) -> Option<u32> {
    let last = days_in_month(y, m);
    if n > last {
        return None;
    }
    Some(match weekday(y, m, n) {
        6 => {
            if n == 1 {
                3
            } else {
                n - 1
            }
        }
        0 => {
            if n == last {
                n - 2
            } else {
                n + 1
            }
        }
        _ => n,
    })
}

fn dom_matches(spec: &DomSpec, y: i64, m: u32, d: u32) -> bool {
    let last = days_in_month(y, m);
    if spec.days & (1 << d) != 0 {
        return true;
    }
    if spec.last_offsets.iter().any(|&o| o < last && d == last - o) {
        return true;
    }
    if spec.last_weekday {
        let lw = match weekday(y, m, last) {
            6 => last - 1,
            0 => last - 2,
            _ => last,
        };
        if d == lw {
            return true;
        }
    }
    spec.nearest_weekday.iter().any(|&n| nearest_weekday(y, m, n) == Some(d))
}

fn dow_matches(spec: &DowSpec, y: i64, m: u32, d: u32) -> bool {
    let wd = weekday(y, m, d);
    if spec.days & (1 << wd) != 0 {
        return true;
    }
    let last = days_in_month(y, m);
    if spec.last_of.iter().any(|&w| w == wd && d + 7 > last) {
        return true;
    }
    spec.nth.iter().any(|&(w, k)| w == wd && (d - 1) / 7 + 1 == k)
}

fn fixed_zone(offset: i32) -> TimeZone {
    let a = offset.abs();
    let id = if offset == 0 {
        "UTC".to_string()
    } else {
        format!("GMT{}{:02}:{:02}", if offset < 0 { '-' } else { '+' }, a / 3600, (a % 3600) / 60)
    };
    TimeZone::fixed(&id, offset)
}

/// Parse a time zone option: fixed offsets and region ids (`Europe/Paris`) from the zoneinfo
/// database. A `+` decoded as a space in an endpoint uri (`GMT 02:00`) is accepted.
pub fn parse_zone(tz: &str) -> Result<Arc<TimeZone>> {
    if let Ok(off) = parse_time_zone(tz) {
        return Ok(Arc::new(fixed_zone(off)));
    }
    TimeZone::parse(&tz.trim().replace(' ', "+"))
}

/// Parse a fixed-offset time zone into a UTC offset in seconds: `UTC`, `GMT`, `Z`, `Etc/UTC`,
/// `GMT+02:00`, `GMT-5`, `UTC+0530`, `+02:00`. Regional names are rejected (see [`parse_zone`]).
pub fn parse_time_zone(tz: &str) -> Result<i32> {
    // a `+` in an endpoint uri is decoded as a space: `GMT+02:00` arrives as `GMT 02:00`
    let t = tz.trim().replace(' ', "+");
    let t = t.as_str();
    let bad = || CamelError::illegal_argument(format!("Unsupported time zone '{}': only UTC/GMT and fixed offsets such as GMT+02:00 are supported", tz));
    let up = t.to_ascii_uppercase();
    let rest = if matches!(up.as_str(), "UTC" | "GMT" | "Z" | "ETC/UTC" | "ETC/GMT" | "UT" | "ZULU" | "UCT" | "ETC/UCT" | "ETC/ZULU") {
        return Ok(0);
    } else if let Some(r) = up.strip_prefix("GMT").or_else(|| up.strip_prefix("UTC")).or_else(|| up.strip_prefix("UT")) {
        r
    } else {
        up.as_str()
    };
    let (sign, digits) = match rest.chars().next() {
        Some('+') => (1, &rest[1..]),
        Some('-') => (-1, &rest[1..]),
        _ => return Err(bad()),
    };
    let (h, m) = if let Some((h, m)) = digits.split_once(':') {
        (h, m)
    } else if digits.len() > 2 {
        digits.split_at(digits.len() - 2)
    } else {
        (digits, "0")
    };
    let h: i32 = h.parse().map_err(|_| bad())?;
    let m: i32 = m.parse().map_err(|_| bad())?;
    if h > 18 || m > 59 {
        return Err(bad());
    }
    Ok(sign * (h * 3600 + m * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        epoch_from_civil(y, mo, d, h, mi, s)
    }

    fn next(expr: &str, from: i64) -> i64 {
        CronExpression::parse(expr).unwrap().next_after(from).unwrap()
    }

    fn fmt(t: i64) -> String {
        let (y, mo, d, h, mi, s) = civil_from_epoch(t);
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s)
    }

    fn seq_c(c: &CronExpression, from: i64, n: usize) -> Vec<String> {
        c.next_n(from, n).into_iter().map(fmt).collect()
    }

    fn seq(expr: &str, from: i64, n: usize) -> Vec<String> {
        CronExpression::parse(expr).unwrap().next_n(from, n).into_iter().map(fmt).collect()
    }

    #[test]
    fn every_second_and_steps() {
        let base = ts(2024, 1, 1, 0, 0, 0);
        assert_eq!(next("* * * * * ?", base), base + 1);
        assert_eq!(next("*/1 * * * * ?", base), base + 1);
        assert_eq!(seq("0/5 * * * * ?", base, 3), ["2024-01-01 00:00:05", "2024-01-01 00:00:10", "2024-01-01 00:00:15"]);
        assert_eq!(seq("10-20/5 * * * * ?", ts(2024, 1, 1, 0, 0, 16), 2), ["2024-01-01 00:00:20", "2024-01-01 00:01:10"]);
        assert_eq!(seq("15/20 * * * * ?", base, 4)[3], "2024-01-01 00:01:15");
    }

    #[test]
    fn unix_five_fields() {
        let base = ts(2024, 3, 10, 10, 7, 30);
        assert_eq!(fmt(next("*/15 * * * *", base)), "2024-03-10 10:15:00");
        assert_eq!(fmt(next("0 9 * * 1-5", base)), "2024-03-11 09:00:00"); // 2024-03-10 is a Sunday
        assert_eq!(fmt(next("0 0 * * 0", base)), "2024-03-17 00:00:00");
        assert_eq!(fmt(next("0 0 * * 7", base)), "2024-03-17 00:00:00");
        assert_eq!(fmt(next("30 2 1 * *", base)), "2024-04-01 02:30:00");
    }

    #[test]
    fn quartz_examples() {
        let base = ts(2024, 5, 15, 13, 0, 0); // Wednesday
        assert_eq!(fmt(next("0 0 12 * * ?", base)), "2024-05-16 12:00:00");
        assert_eq!(fmt(next("0 15 10 ? * MON-FRI", base)), "2024-05-16 10:15:00");
        assert_eq!(fmt(next("0 0/5 14 * * ?", base)), "2024-05-15 14:00:00");
        assert_eq!(fmt(next("0 0/5 14,18 * * ?", ts(2024, 5, 15, 14, 57, 0))), "2024-05-15 18:00:00");
        assert_eq!(fmt(next("0 10,44 14 ? 3 WED", base)), "2025-03-05 14:10:00");
        assert_eq!(fmt(next("0 15 10 15 * ?", base)), "2024-06-15 10:15:00");
        assert_eq!(fmt(next("0 15 10 * * ? 2026", base)), "2026-01-01 10:15:00");
        assert_eq!(fmt(next("0 0 0 1 1 ? 2025-2027", ts(2025, 6, 1, 0, 0, 0))), "2026-01-01 00:00:00");
        assert!(CronExpression::parse("0 0 0 1 1 ? 2020").unwrap().next_after(base).is_none());
    }

    #[test]
    fn month_and_day_names() {
        let base = ts(2024, 1, 1, 0, 0, 0); // Monday
        assert_eq!(fmt(next("0 0 0 1 jul ?", base)), "2024-07-01 00:00:00");
        assert_eq!(fmt(next("0 0 0 ? * sat,sun", base)), "2024-01-06 00:00:00");
        assert_eq!(fmt(next("0 0 0 * NOV-FEB ?", ts(2024, 3, 1, 0, 0, 0))), "2024-11-01 00:00:00");
        assert_eq!(seq("0 0 0 ? * FRI-MON", base, 4), ["2024-01-05 00:00:00", "2024-01-06 00:00:00", "2024-01-07 00:00:00", "2024-01-08 00:00:00"]);
        assert_eq!(seq("0 0 22-2 * * ?", ts(2024, 1, 1, 21, 0, 0), 5).last().unwrap(), "2024-01-02 02:00:00");
    }

    #[test]
    fn last_day_of_month_and_leap_years() {
        assert_eq!(fmt(next("0 0 0 L * ?", ts(2024, 1, 31, 0, 0, 0))), "2024-02-29 00:00:00");
        assert_eq!(fmt(next("0 0 0 L * ?", ts(2023, 1, 31, 0, 0, 0))), "2023-02-28 00:00:00");
        assert_eq!(fmt(next("0 0 0 L-2 * ?", ts(2024, 4, 1, 0, 0, 0))), "2024-04-28 00:00:00");
        assert_eq!(fmt(next("0 0 0 29 2 ?", ts(2024, 3, 1, 0, 0, 0))), "2028-02-29 00:00:00");
        assert_eq!(fmt(next("0 0 0 29 2 ?", ts(2096, 3, 1, 0, 0, 0))), "2104-02-29 00:00:00"); // 2100 is not leap
        assert_eq!(fmt(next("0 0 0 31 * ?", ts(2024, 4, 1, 0, 0, 0))), "2024-05-31 00:00:00");
        assert_eq!(fmt(next("0 0 0 30 * ?", ts(2024, 1, 30, 0, 0, 0))), "2024-03-30 00:00:00");
        assert_eq!(fmt(next("59 59 23 31 12 ?", ts(2024, 6, 1, 0, 0, 0))), "2024-12-31 23:59:59");
        assert_eq!(fmt(next("* * * * * ?", ts(2024, 12, 31, 23, 59, 59))), "2025-01-01 00:00:00");
        assert!(CronExpression::parse("0 0 0 30 2 ?").unwrap().next_after(0).is_none());
    }

    #[test]
    fn weekday_specials() {
        // 2024-06-15 is a Saturday -> 15W fires on Friday the 14th
        assert_eq!(fmt(next("0 0 0 15W * ?", ts(2024, 6, 1, 0, 0, 0))), "2024-06-14 00:00:00");
        // 2024-09-01 is a Sunday -> 1W fires on Monday the 2nd
        assert_eq!(fmt(next("0 0 0 1W * ?", ts(2024, 8, 31, 0, 0, 0))), "2024-09-02 00:00:00");
        // 2024-06-01 is a Saturday -> 1W does not cross into May: Monday the 3rd
        assert_eq!(fmt(next("0 0 0 1W * ?", ts(2024, 5, 31, 0, 0, 0))), "2024-06-03 00:00:00");
        // 2024-03-31 is a Sunday -> LW = Friday 29th
        assert_eq!(fmt(next("0 0 0 LW * ?", ts(2024, 3, 1, 0, 0, 0))), "2024-03-29 00:00:00");
        // 2024-06-30 is a Sunday -> 30W = Friday 28th
        assert_eq!(fmt(next("0 0 0 30W * ?", ts(2024, 6, 1, 0, 0, 0))), "2024-06-28 00:00:00");
        // last Friday (6L in Quartz numbering)
        assert_eq!(fmt(next("0 0 0 ? * 6L", ts(2024, 5, 1, 0, 0, 0))), "2024-05-31 00:00:00");
        assert_eq!(fmt(next("0 0 0 ? * FRIL", ts(2024, 6, 1, 0, 0, 0))), "2024-06-28 00:00:00");
        // third Friday (6#3)
        assert_eq!(fmt(next("0 0 0 ? * 6#3", ts(2024, 5, 1, 0, 0, 0))), "2024-05-17 00:00:00");
        // second Monday (MON#2), and 5th Monday skips months without one
        assert_eq!(fmt(next("0 0 0 ? * MON#2", ts(2024, 1, 1, 0, 0, 0))), "2024-01-08 00:00:00");
        assert_eq!(fmt(next("0 0 0 ? * 2#5", ts(2024, 2, 1, 0, 0, 0))), "2024-04-29 00:00:00");
        // unix: last friday and thanksgiving style 4th thursday
        assert_eq!(fmt(next("0 12 ? 11 4#4", ts(2024, 1, 1, 0, 0, 0))), "2024-11-28 12:00:00");
        // L alone in day-of-week = Saturday
        assert_eq!(fmt(next("0 0 0 ? * L", ts(2024, 1, 1, 0, 0, 0))), "2024-01-06 00:00:00");
    }

    #[test]
    fn dom_and_dow_both_restricted_is_or() {
        // 1st of month or any Monday
        assert_eq!(seq("0 0 1 * 1", ts(2024, 1, 2, 0, 0, 0), 3), ["2024-01-08 00:00:00", "2024-01-15 00:00:00", "2024-01-22 00:00:00"]);
        assert_eq!(seq("0 0 1 * 1", ts(2024, 1, 29, 0, 0, 0), 1), ["2024-02-01 00:00:00"]);
    }

    #[test]
    fn macros() {
        let base = ts(2024, 5, 15, 13, 20, 0);
        assert_eq!(fmt(next("@hourly", base)), "2024-05-15 14:00:00");
        assert_eq!(fmt(next("@daily", base)), "2024-05-16 00:00:00");
        assert_eq!(fmt(next("@midnight", base)), "2024-05-16 00:00:00");
        assert_eq!(fmt(next("@weekly", base)), "2024-05-19 00:00:00");
        assert_eq!(fmt(next("@monthly", base)), "2024-06-01 00:00:00");
        assert_eq!(fmt(next("@yearly", base)), "2025-01-01 00:00:00");
        assert_eq!(fmt(next("@annually", base)), "2025-01-01 00:00:00");
    }

    #[test]
    fn time_zones() {
        assert_eq!(parse_time_zone("UTC").unwrap(), 0);
        assert_eq!(parse_time_zone("GMT+02:00").unwrap(), 7200);
        assert_eq!(parse_time_zone("GMT-5").unwrap(), -18000);
        assert_eq!(parse_time_zone("UTC+0530").unwrap(), 19800);
        assert_eq!(parse_time_zone("+01:00").unwrap(), 3600);
        assert_eq!(parse_time_zone("GMT 02:00").unwrap(), 7200);
        assert!(parse_time_zone("Europe/Paris").is_err());
        // 12:00 at GMT+02:00 = 10:00 UTC
        let c = CronExpression::parse_with_time_zone("0 0 12 * * ?", Some("GMT+02:00")).unwrap();
        assert_eq!(fmt(c.next_after(ts(2024, 1, 1, 0, 0, 0)).unwrap()), "2024-01-01 10:00:00");
        assert_eq!(fmt(c.next_after(ts(2024, 1, 1, 10, 0, 0)).unwrap()), "2024-01-02 10:00:00");
        // midnight at GMT-05:00 = 05:00 UTC; day boundaries follow local time
        let c = CronExpression::parse_with_time_zone("0 0 0 1 * ?", Some("GMT-05:00")).unwrap();
        assert_eq!(fmt(c.next_after(ts(2024, 1, 1, 4, 0, 0)).unwrap()), "2024-01-01 05:00:00");
        assert_eq!(fmt(c.next_after(ts(2024, 1, 1, 5, 0, 0)).unwrap()), "2024-02-01 05:00:00");
        assert!(c.matches(ts(2024, 2, 1, 5, 0, 0)));
        assert!(!c.matches(ts(2024, 2, 1, 0, 0, 0)));
    }

    #[test]
    fn region_time_zones_and_dst() {
        // daily 12:00 Paris: 11:00 UTC in winter, 10:00 UTC in summer
        let c = CronExpression::parse_with_time_zone("0 0 12 * * ?", Some("Europe/Paris")).unwrap();
        assert_eq!(c.time_zone().id(), "Europe/Paris");
        assert_eq!(seq_c(&c, ts(2024, 3, 30, 0, 0, 0), 2), vec!["2024-03-30 11:00:00", "2024-03-31 10:00:00"]);
        // 02:30 does not exist on 2024-03-31 in Paris: fires once at 03:30 CEST (01:30 UTC)
        let c = CronExpression::parse_with_time_zone("0 30 2 * * ?", Some("Europe/Paris")).unwrap();
        assert_eq!(
            seq_c(&c, ts(2024, 3, 29, 12, 0, 0), 3),
            vec!["2024-03-30 01:30:00", "2024-03-31 01:30:00", "2024-04-01 00:30:00"]
        );
        // every 30 minutes across the gap: 01:30 CET, then 03:00 CEST (02:00 and 02:30 are
        // shifted to 03:00 and 03:30 and so coincide with real times)
        let c = CronExpression::parse_with_time_zone("0 0/30 * * * ?", Some("Europe/Paris")).unwrap();
        assert_eq!(
            seq_c(&c, ts(2024, 3, 31, 0, 15, 0), 4),
            vec!["2024-03-31 00:30:00", "2024-03-31 01:00:00", "2024-03-31 01:30:00", "2024-03-31 02:00:00"]
        );
        // 01:30 happens twice on 2024-11-03 in New York: fires once (EST, 06:30 UTC)
        let c = CronExpression::parse_with_time_zone("0 30 1 * * ?", Some("America/New_York")).unwrap();
        assert_eq!(
            seq_c(&c, ts(2024, 11, 2, 12, 0, 0), 3),
            vec!["2024-11-03 06:30:00", "2024-11-04 06:30:00", "2024-11-05 06:30:00"]
        );
        // hourly across the fall-back: 00:00 EDT, 01:00 EST, 02:00 EST (no duplicate)
        let c = CronExpression::parse_with_time_zone("0 0 * * * ?", Some("America/New_York")).unwrap();
        assert_eq!(
            seq_c(&c, ts(2024, 11, 3, 3, 30, 0), 3),
            vec!["2024-11-03 04:00:00", "2024-11-03 06:00:00", "2024-11-03 07:00:00"]
        );
        assert!(c.matches(ts(2024, 11, 3, 6, 0, 0)));
        assert!(CronExpression::parse_with_time_zone("0 0 * * * ?", Some("Nowhere/City")).is_err());
        // the `+` of an offset decoded as a space
        assert_eq!(parse_zone("GMT 02:00").unwrap().offset_at(0), 7200);
    }

    #[test]
    fn millis_and_matches() {
        let c = CronExpression::parse("*/2 * * * * ?").unwrap();
        assert_eq!(c.next_after_millis(1_000_500), Some(1_002_000));
        assert_eq!(c.next_after_millis(1_001_999), Some(1_002_000));
        assert!(c.matches(1_002));
        assert!(!c.matches(1_003));
    }

    #[test]
    fn invalid_expressions() {
        for bad in [
            "",
            "* * *",
            "60 * * * * ?",
            "* 60 * * * ?",
            "* * 24 * * ?",
            "* * * 32 * ?",
            "* * * * 13 ?",
            "* * * ? * 8",
            "* * * ? * 0",
            "* * * ? * ?",
            "*/0 * * * * ?",
            "* * * ? * 6#6",
            "@sometimes",
            "a * * * * ?",
            "* * * * * ? 1969",
        ] {
            assert!(CronExpression::parse(bad).is_err(), "expected error for {:?}", bad);
        }
    }
}
