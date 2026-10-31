//! Date/time helpers without external crates.

use std::time::{SystemTime, UNIX_EPOCH};

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

/// Format epoch millis using a Java `SimpleDateFormat`-like pattern (UTC).
/// Supported letters: yyyy, yy, MM, dd, HH, mm, ss, SSS, and literal text in quotes.
pub fn format_millis(millis: i64, pattern: &str) -> String {
    let secs = millis.div_euclid(1000);
    let ms = millis.rem_euclid(1000);
    let (y, mo, d, h, mi, s) = civil_from_epoch(secs);
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            i += 1;
            while i < chars.len() && chars[i] != '\'' {
                out.push(chars[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        let mut n = 1;
        while i + n < chars.len() && chars[i + n] == c {
            n += 1;
        }
        match c {
            'y' => {
                if n == 2 {
                    out.push_str(&format!("{:02}", y % 100))
                } else {
                    out.push_str(&format!("{:04}", y))
                }
            }
            'M' => out.push_str(&format!("{:0width$}", mo, width = n.min(2))),
            'd' => out.push_str(&format!("{:0width$}", d, width = n.min(2))),
            'H' => out.push_str(&format!("{:0width$}", h, width = n.min(2))),
            'm' => out.push_str(&format!("{:0width$}", mi, width = n.min(2))),
            's' => out.push_str(&format!("{:0width$}", s, width = n.min(2))),
            'S' => out.push_str(&format!("{:03}", ms)),
            'X' | 'Z' => out.push('Z'),
            _ => {
                for _ in 0..n {
                    out.push(c)
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
        assert_eq!(format_millis(e * 1000 + 7, "yyyy-MM-dd'T'HH:mm:ss.SSS"), "2024-02-29T13:45:10.007");
    }
}
