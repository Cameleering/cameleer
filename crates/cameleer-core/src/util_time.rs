//! Date/time helpers without external crates.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::tz::TimeZone;

/// Convert epoch seconds (UTC) into (year, month, day, hour, minute, second).
pub fn civil_from_epoch(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = ((rem / 3600) as u32, ((rem % 3600) / 60) as u32, (rem % 60) as u32);
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d, h, mi, s)
}

/// Convert a civil date (UTC) to epoch seconds.
pub fn epoch_from_civil(y: i64, m: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + h as i64 * 3600 + mi as i64 * 60 + s as i64
}

/// Current time in epoch millis.
pub fn now_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Format epoch millis using a Java `SimpleDateFormat` pattern in the default time zone
/// ([`TimeZone::local`], i.e. `TZ` or `/etc/localtime`), like Java's default.
pub fn format_millis(millis: i64, pattern: &str) -> String {
    format_in_zone(millis, pattern, &TimeZone::local())
}

/// Format epoch millis using a Java `SimpleDateFormat` pattern in UTC.
pub fn format_millis_utc(millis: i64, pattern: &str) -> String {
    format_in_zone(millis, pattern, &TimeZone::utc())
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// Format epoch millis using a Java `SimpleDateFormat` pattern in the given time zone.
/// Supports `G y Y M L d D E u a H k K h m s S z Z X` and quoted text. `z` renders the zone
/// abbreviation in force at that instant (`CET`/`CEST`, `UTC`, `GMT+02:00`).
pub fn format_in_zone(millis: i64, pattern: &str, zone: &TimeZone) -> String {
    let offset_secs = zone.offset_at(millis.div_euclid(1000)) as i64;
    let offset_min = offset_secs / 60;
    let local = millis + offset_secs * 1000;
    let secs = local.div_euclid(1000);
    let ms = local.rem_euclid(1000);
    let (y, mo, d, h, mi, s) = civil_from_epoch(secs);
    let days = secs.div_euclid(86_400);
    let dow = ((days + 3).rem_euclid(7)) as usize; // 0 = Monday (1970-01-01 was a Thursday)
    let doy = days - epoch_from_civil(y, 1, 1, 0, 0, 0).div_euclid(86_400) + 1;
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let pad = |v: i64, n: usize| format!("{:0width$}", v, width = n);
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            if i + 1 < chars.len() && chars[i + 1] == '\'' {
                out.push('\'');
                i += 2;
                continue;
            }
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if i + 1 < chars.len() && chars[i + 1] == '\'' {
                        out.push('\'');
                        i += 2;
                        continue;
                    }
                    break;
                }
                out.push(chars[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        if !c.is_ascii_alphabetic() {
            out.push(c);
            i += 1;
            continue;
        }
        let mut n = 1;
        while i + n < chars.len() && chars[i + n] == c {
            n += 1;
        }
        let tz = |colon: bool, z_for_zero: bool| -> String {
            if offset_min == 0 && z_for_zero {
                return "Z".to_string();
            }
            let sign = if offset_min < 0 { '-' } else { '+' };
            let a = offset_min.abs();
            if colon {
                format!("{}{:02}:{:02}", sign, a / 60, a % 60)
            } else {
                format!("{}{:02}{:02}", sign, a / 60, a % 60)
            }
        };
        match c {
            'G' => out.push_str("AD"),
            'u' => out.push_str(&(dow as i64 + 1).to_string()),
            'y' | 'Y' => {
                if n == 2 {
                    out.push_str(&pad(y.rem_euclid(100), 2));
                } else {
                    out.push_str(&pad(y, n));
                }
            }
            'M' | 'L' => match n {
                1 => out.push_str(&mo.to_string()),
                2 => out.push_str(&pad(mo as i64, 2)),
                3 => out.push_str(&MONTHS[(mo - 1) as usize][..3]),
                _ => out.push_str(MONTHS[(mo - 1) as usize]),
            },
            'd' => out.push_str(&pad(d as i64, n)),
            'D' => out.push_str(&pad(doy, n)),
            'E' => {
                if n >= 4 {
                    out.push_str(DAYS[dow])
                } else {
                    out.push_str(&DAYS[dow][..3])
                }
            }
            'a' => out.push_str(if h < 12 { "AM" } else { "PM" }),
            'H' => out.push_str(&pad(h as i64, n)),
            'k' => out.push_str(&pad(if h == 0 { 24 } else { h as i64 }, n)),
            'K' => out.push_str(&pad((h % 12) as i64, n)),
            'h' => out.push_str(&pad(if h % 12 == 0 { 12 } else { (h % 12) as i64 }, n)),
            'm' => out.push_str(&pad(mi as i64, n)),
            's' => out.push_str(&pad(s as i64, n)),
            'S' => {
                let v = format!("{:03}", ms);
                if n <= 3 {
                    out.push_str(&v[..n]);
                } else {
                    out.push_str(&v);
                    out.push_str(&"0".repeat(n - 3));
                }
            }
            'z' => out.push_str(&zone.abbreviation_at(millis.div_euclid(1000))),
            'Z' => out.push_str(&tz(false, false)),
            'X' => out.push_str(&if n >= 3 { tz(true, true) } else { tz(false, true) }),
            other => {
                for _ in 0..n {
                    out.push(other)
                }
            }
        }
        i += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn civil_roundtrip() {
        assert_eq!(civil_from_epoch(0), (1970, 1, 1, 0, 0, 0));
        let e = epoch_from_civil(2024, 2, 29, 13, 45, 10);
        assert_eq!(civil_from_epoch(e), (2024, 2, 29, 13, 45, 10));
        assert_eq!(format_millis_utc(e * 1000 + 7, "yyyy-MM-dd'T'HH:mm:ss.SSS"), "2024-02-29T13:45:10.007");
    }

    #[test]
    fn format_named_zones() {
        let paris = TimeZone::parse("Europe/Paris").unwrap();
        let before = epoch_from_civil(2024, 3, 31, 0, 59, 0) * 1000;
        let after = epoch_from_civil(2024, 3, 31, 1, 0, 0) * 1000;
        assert_eq!(format_in_zone(before, "yyyy-MM-dd HH:mm z Z XXX", &paris), "2024-03-31 01:59 CET +0100 +01:00");
        assert_eq!(format_in_zone(after, "yyyy-MM-dd HH:mm z Z XXX", &paris), "2024-03-31 03:00 CEST +0200 +02:00");
        let ny = TimeZone::parse("America/New_York").unwrap();
        let t = epoch_from_civil(2024, 11, 3, 6, 30, 0) * 1000;
        assert_eq!(format_in_zone(t, "HH:mm z", &ny), "01:30 EST");
        assert_eq!(format_in_zone(t - 3_600_000, "HH:mm z", &ny), "01:30 EDT");
        assert_eq!(format_millis_utc(t, "HH:mm z X"), "06:30 UTC Z");
    }
}

/// Current UTC time as `xsd:dateTime` with milliseconds (`2024-01-02T03:04:05.678Z`).
pub fn now_utc() -> String {
    format_utc_millis(now_millis())
}

/// Format epoch milliseconds as `xsd:dateTime` (UTC).
pub fn format_utc_millis(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_epoch(ms.div_euclid(1000));
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, h, mi, s, ms.rem_euclid(1000))
}
