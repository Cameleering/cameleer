//! Time zones without external crates.
//!
//! [`TimeZone`] gives the UTC offset of any instant for:
//!
//! - fixed offsets (`UTC`, `GMT`, `Z`, `GMT+02:00`, `UTC-5`, `+0530`),
//! - named (region) zones such as `Europe/Paris`, read from the TZif database
//!   (RFC 8536 versions 1, 2 and 3) in `$TZDIR`, `/usr/share/zoneinfo`, `/usr/lib/zoneinfo`
//!   or `/usr/share/lib/zoneinfo`. Instants after the last stored transition use the POSIX
//!   TZ footer rule of the file (v2+), so future dates follow the current DST rules,
//! - POSIX TZ strings such as `CET-1CEST,M3.5.0,M10.5.0/3`.
//!
//! [`TimeZone::local`] is the process default zone (the equivalent of the JVM default time
//! zone): the `TZ` environment variable when set (`:Europe/Paris`, `Europe/Paris`, a POSIX
//! string or a TZif file path), otherwise `/etc/localtime`, otherwise UTC.
//!
//! Local (wall clock) times are converted back to instants with [`TimeZone::local_to_utc`],
//! which reports gaps (spring forward) and overlaps (fall back);
//! [`TimeZone::resolve_local`] resolves them like `java.util.GregorianCalendar` (and therefore
//! Quartz and `SimpleDateFormat`): a time in a gap is shifted forward by the gap length and an
//! ambiguous time maps to the later (standard time) instant.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::{CamelError, Result};
use crate::util_time::{civil_from_epoch, epoch_from_civil};

/// A local time type: UTC offset, DST flag and abbreviation (`CEST`).
#[derive(Clone, Debug, PartialEq)]
struct LocalType {
    offset: i32,
    is_dst: bool,
    abbr: String,
}

/// The day part of a POSIX TZ rule.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RuleDate {
    /// `Jn`: Julian day 1..=365, February 29 is never counted.
    Julian1(u16),
    /// `n`: zero-based day of year 0..=365, February 29 counted in leap years.
    Julian0(u16),
    /// `Mm.w.d`: day `d` (0 = Sunday) of week `w` (1..=5, 5 = last) of month `m`.
    MonthWeekDay(u8, u8, u8),
}

/// A POSIX TZ transition rule: a date and a local time of day in seconds
/// (may be negative or exceed 24 hours, RFC 8536 version 3).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rule {
    date: RuleDate,
    time: i32,
}

/// A parsed POSIX TZ string.
#[derive(Clone, Debug, PartialEq)]
struct PosixTz {
    std: LocalType,
    dst: Option<(LocalType, Rule, Rule)>,
}

#[derive(Clone, Debug)]
struct TzData {
    transitions: Vec<i64>,
    indices: Vec<u8>,
    types: Vec<LocalType>,
    footer: Option<PosixTz>,
}

#[derive(Clone, Debug)]
enum Kind {
    Fixed(LocalType),
    Tzif(TzData),
    Posix(PosixTz),
}

/// A time zone (fixed offset, TZif region zone or POSIX rule).
#[derive(Clone, Debug)]
pub struct TimeZone {
    id: String,
    kind: Kind,
}

/// The result of mapping a local (wall clock) time to UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalResult {
    /// Exactly one instant.
    Single(i64),
    /// The local time occurs twice (fall back): earlier and later instants.
    Ambiguous(i64, i64),
    /// The local time does not exist (spring forward). `shifted` interprets it with the offset
    /// in force before the gap (i.e. moved forward by the gap length), `transition` is the
    /// instant the gap starts.
    Gap { shifted: i64, transition: i64 },
}

impl TimeZone {
    /// The UTC zone.
    pub fn utc() -> TimeZone {
        TimeZone::fixed("UTC", 0)
    }

    /// A fixed offset zone with the given id (also used as abbreviation).
    pub fn fixed(id: &str, offset_secs: i32) -> TimeZone {
        TimeZone { id: id.to_string(), kind: Kind::Fixed(LocalType { offset: offset_secs, is_dst: false, abbr: id.to_string() }) }
    }

    /// The zone id (`Europe/Paris`, `UTC`, `GMT+02:00`).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether the zone has a single constant offset.
    pub fn is_fixed(&self) -> bool {
        matches!(self.kind, Kind::Fixed(_))
    }

    /// Parse a time zone id: fixed offsets (`UTC`, `GMT+2`, `GMT+02:00`, `+01:00`), region ids
    /// read from the zoneinfo database (`Europe/Paris`) or POSIX TZ strings. Results are cached.
    pub fn parse(id: &str) -> Result<Arc<TimeZone>> {
        let id = id.trim();
        static CACHE: OnceLock<Mutex<HashMap<String, Arc<TimeZone>>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Some(z) = cache.lock().unwrap().get(id) {
            return Ok(z.clone());
        }
        let z = Arc::new(Self::parse_uncached(id)?);
        cache.lock().unwrap().insert(id.to_string(), z.clone());
        Ok(z)
    }

    fn parse_uncached(id: &str) -> Result<TimeZone> {
        if id.is_empty() {
            return Err(CamelError::illegal_argument("Empty time zone id"));
        }
        if let Some(off) = parse_fixed_offset(id) {
            let canonical = if off == 0 {
                match id.to_ascii_uppercase().as_str() {
                    "GMT" | "GMT0" | "ETC/GMT" => "GMT".to_string(),
                    _ => "UTC".to_string(),
                }
            } else {
                let a = off.abs();
                let mut s = format!("GMT{}{:02}:{:02}", if off < 0 { '-' } else { '+' }, a / 3600, (a % 3600) / 60);
                if a % 60 != 0 {
                    s.push_str(&format!(":{:02}", a % 60));
                }
                s
            };
            return Ok(TimeZone::fixed(&canonical, off));
        }
        let named = java_short_id(id).unwrap_or(id);
        if let Some(path) = find_zone_file(named) {
            let data = std::fs::read(&path)
                .map_err(|e| CamelError::illegal_argument(format!("Cannot read time zone file {}: {}", path.display(), e)))?;
            return Self::from_tzif(id, &data);
        }
        if let Some(p) = parse_posix_tz(id) {
            return Ok(TimeZone { id: id.to_string(), kind: Kind::Posix(p) });
        }
        Err(CamelError::illegal_argument(format!(
            "Unknown time zone: {} (no zoneinfo file found in {})",
            id,
            zoneinfo_dirs().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        )))
    }

    /// Like [`TimeZone::parse`] but unknown ids fall back to UTC, like Java's
    /// `TimeZone.getTimeZone` (which returns GMT for unknown ids).
    pub fn parse_lenient(id: &str) -> Arc<TimeZone> {
        Self::parse(id).unwrap_or_else(|_| Arc::new(TimeZone::utc()))
    }

    /// Parse TZif data (RFC 8536 version 1, 2 or 3).
    pub fn from_tzif(id: &str, data: &[u8]) -> Result<TimeZone> {
        let err = |m: &str| CamelError::illegal_argument(format!("Invalid TZif data for {}: {}", id, m));
        let header = |at: usize| -> Option<(u8, [usize; 6])> {
            let h = data.get(at..at + 44)?;
            if &h[..4] != b"TZif" {
                return None;
            }
            let mut c = [0usize; 6];
            for (i, v) in c.iter_mut().enumerate() {
                let o = 20 + i * 4;
                *v = u32::from_be_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]) as usize;
            }
            Some((h[4], c))
        };
        let (version, counts) = header(0).ok_or_else(|| err("bad header"))?;
        // counts: isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt
        let block_len = |c: &[usize; 6], tsize: usize| c[3] * tsize + c[3] + c[4] * 6 + c[5] + c[2] * (tsize + 4) + c[1] + c[0];
        let (start, counts, tsize) = if version >= b'2' {
            let v2 = 44 + block_len(&counts, 4);
            let (_, c2) = header(v2).ok_or_else(|| err("bad v2 header"))?;
            (v2 + 44, c2, 8)
        } else {
            (44, counts, 4)
        };
        let [_, _, _, timecnt, typecnt, charcnt] = counts;
        if typecnt == 0 {
            return Err(err("no local time types"));
        }
        let need = start + block_len(&counts, tsize);
        if data.len() < need {
            return Err(err("truncated"));
        }
        let mut p = start;
        let mut transitions = Vec::with_capacity(timecnt);
        for _ in 0..timecnt {
            let t = if tsize == 8 {
                i64::from_be_bytes(data[p..p + 8].try_into().unwrap())
            } else {
                i32::from_be_bytes(data[p..p + 4].try_into().unwrap()) as i64
            };
            transitions.push(t);
            p += tsize;
        }
        let indices: Vec<u8> = data[p..p + timecnt].to_vec();
        p += timecnt;
        if indices.iter().any(|&i| i as usize >= typecnt) {
            return Err(err("bad transition type index"));
        }
        let ttinfo = &data[p..p + typecnt * 6];
        p += typecnt * 6;
        let chars = &data[p..p + charcnt];
        let mut types = Vec::with_capacity(typecnt);
        for i in 0..typecnt {
            let e = &ttinfo[i * 6..i * 6 + 6];
            let offset = i32::from_be_bytes([e[0], e[1], e[2], e[3]]);
            let ai = e[5] as usize;
            let abbr = chars.get(ai..).map(|s| {
                let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
                String::from_utf8_lossy(&s[..end]).to_string()
            });
            types.push(LocalType { offset, is_dst: e[4] != 0, abbr: abbr.unwrap_or_default() });
        }
        let footer = if version >= b'2' {
            let rest = &data[need..];
            let text = String::from_utf8_lossy(rest);
            let text = text.trim_start_matches('\n');
            let line = text.split('\n').next().unwrap_or("");
            if line.is_empty() {
                None
            } else {
                parse_posix_tz(line)
            }
        } else {
            None
        };
        Ok(TimeZone { id: id.to_string(), kind: Kind::Tzif(TzData { transitions, indices, types, footer }) })
    }

    /// The process default time zone: `TZ` env, then `/etc/localtime`, then UTC. Cached.
    pub fn local() -> Arc<TimeZone> {
        static LOCAL: OnceLock<Arc<TimeZone>> = OnceLock::new();
        LOCAL.get_or_init(|| Arc::new(Self::from_tz_env(std::env::var("TZ").ok().as_deref()))).clone()
    }

    /// Resolve a `TZ` environment value like the C library does (`None` = unset).
    pub fn from_tz_env(value: Option<&str>) -> TimeZone {
        match value {
            None => {
                let p = Path::new("/etc/localtime");
                let id = std::fs::read_link(p)
                    .ok()
                    .and_then(|t| {
                        let s = t.to_string_lossy().to_string();
                        s.find("zoneinfo/").map(|i| s[i + 9..].to_string())
                    })
                    .unwrap_or_else(|| "localtime".to_string());
                match std::fs::read(p).ok().and_then(|d| Self::from_tzif(&id, &d).ok()) {
                    Some(z) => z,
                    None => TimeZone::utc(),
                }
            }
            Some(v) => {
                let v = v.trim();
                let v = v.strip_prefix(':').unwrap_or(v);
                if v.is_empty() {
                    return TimeZone::utc();
                }
                if v.starts_with('/') {
                    if let Some(z) = std::fs::read(v).ok().and_then(|d| Self::from_tzif(v, &d).ok()) {
                        return z;
                    }
                }
                Self::parse(v).map(|z| (*z).clone()).unwrap_or_else(|_| TimeZone::utc())
            }
        }
    }

    fn local_type_at(&self, t: i64) -> LocalType {
        match &self.kind {
            Kind::Fixed(lt) => lt.clone(),
            Kind::Posix(p) => p.local_type_at(t),
            Kind::Tzif(d) => {
                if d.transitions.is_empty() || t < d.transitions[0] {
                    if d.transitions.is_empty() {
                        if let Some(f) = &d.footer {
                            return f.local_type_at(t);
                        }
                    }
                    return d.types[0].clone();
                }
                let last = *d.transitions.last().unwrap();
                if t >= last {
                    if let Some(f) = &d.footer {
                        return f.local_type_at(t);
                    }
                }
                let i = match d.transitions.binary_search(&t) {
                    Ok(i) => i,
                    Err(i) => i - 1,
                };
                d.types[d.indices[i] as usize].clone()
            }
        }
    }

    /// UTC offset in seconds at the instant `utc_secs` (epoch seconds).
    pub fn offset_at(&self, utc_secs: i64) -> i32 {
        self.local_type_at(utc_secs).offset
    }

    /// Whether daylight saving time is in force at the instant.
    pub fn is_dst_at(&self, utc_secs: i64) -> bool {
        self.local_type_at(utc_secs).is_dst
    }

    /// The abbreviation in force at the instant (`CET`, `CEST`, `EST`, `UTC`, `GMT+02:00`).
    pub fn abbreviation_at(&self, utc_secs: i64) -> String {
        let a = self.local_type_at(utc_secs).abbr;
        if a.is_empty() || a.starts_with(['+', '-']) {
            // numeric abbreviations ("+03") are rendered like Java: GMT+03:00
            let o = self.offset_at(utc_secs);
            if o == 0 {
                return "UTC".to_string();
            }
            let x = o.abs();
            return format!("GMT{}{:02}:{:02}", if o < 0 { '-' } else { '+' }, x / 3600, (x % 3600) / 60);
        }
        a
    }

    /// Map a local time (epoch seconds of the wall clock reading, as if it were UTC) to UTC.
    pub fn local_to_utc(&self, local: i64) -> LocalResult {
        if let Kind::Fixed(lt) = &self.kind {
            return LocalResult::Single(local - lt.offset as i64);
        }
        const WINDOW: i64 = 2 * 86_400;
        let before = self.offset_at(local - WINDOW) as i64;
        let after = self.offset_at(local + WINDOW) as i64;
        let mut valid: Vec<i64> = Vec::with_capacity(3);
        for o in [before, after, self.offset_at(local) as i64] {
            let u = local - o;
            if self.offset_at(u) as i64 == o && !valid.contains(&u) {
                valid.push(u);
            }
        }
        valid.sort_unstable();
        match valid.len() {
            0 => {
                // gap: the transition is between local-before and local-after
                let shifted = local - before;
                let mut lo = local - after;
                let mut hi = shifted;
                while hi - lo > 1 {
                    let mid = lo + (hi - lo) / 2;
                    if self.offset_at(mid) as i64 == before {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                LocalResult::Gap { shifted, transition: hi }
            }
            1 => LocalResult::Single(valid[0]),
            _ => LocalResult::Ambiguous(valid[0], *valid.last().unwrap()),
        }
    }

    /// Map a local time to UTC like `java.util.GregorianCalendar` (used by Quartz and
    /// `SimpleDateFormat`): gaps shift forward, ambiguous times take the later instant.
    pub fn resolve_local(&self, local: i64) -> i64 {
        match self.local_to_utc(local) {
            LocalResult::Single(u) => u,
            LocalResult::Ambiguous(_, later) => later,
            LocalResult::Gap { shifted, .. } => shifted,
        }
    }

    /// The local wall clock time (epoch seconds as if UTC) of an instant.
    pub fn to_local(&self, utc_secs: i64) -> i64 {
        utc_secs + self.offset_at(utc_secs) as i64
    }
}

impl PartialEq for TimeZone {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

/// Parse a fixed offset id into seconds: `UTC`, `GMT`, `Z`, `UT`, `Etc/UTC`, `GMT+2`, `GMT+02:00`,
/// `UTC-5`, `GMT 02:00`, `+0100`, `+01:00`, `-05:30:15`. Returns `None` for other ids.
pub fn parse_fixed_offset(id: &str) -> Option<i32> {
    let t = id.trim();
    let upper = t.to_ascii_uppercase();
    if matches!(upper.as_str(), "UTC" | "GMT" | "Z" | "UT" | "ETC/UTC" | "ETC/GMT" | "ETC/UCT" | "UCT" | "ZULU" | "ETC/ZULU" | "GMT0" | "UTC0")
    {
        return Some(0);
    }
    let rest = if upper.starts_with("GMT") || upper.starts_with("UTC") {
        t[3..].trim_start()
    } else if upper.starts_with("UT") && t.len() > 2 && t[2..].starts_with(['+', '-']) {
        &t[2..]
    } else {
        t
    };
    let (sign, digits) = match rest.chars().next() {
        Some('+') => (1, &rest[1..]),
        Some('-') => (-1, &rest[1..]),
        Some(c) if c.is_ascii_digit() && rest.len() < t.len() => (1, rest),
        _ => return None,
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit() || c == ':') {
        return None;
    }
    let (h, m, s) = if digits.contains(':') {
        let parts: Vec<&str> = digits.split(':').collect();
        if parts.len() > 3 || parts.iter().any(|p| p.is_empty() || p.len() > 2) {
            return None;
        }
        let n = |i: usize| parts.get(i).map(|p| p.parse::<i32>().unwrap_or(0)).unwrap_or(0);
        (n(0), n(1), n(2))
    } else {
        let n = |s: &str| s.parse::<i32>().ok();
        match digits.len() {
            1 | 2 => (n(digits)?, 0, 0),
            3 => (n(&digits[..1])?, n(&digits[1..])?, 0),
            4 => (n(&digits[..2])?, n(&digits[2..])?, 0),
            6 => (n(&digits[..2])?, n(&digits[2..4])?, n(&digits[4..])?),
            _ => return None,
        }
    };
    if h > 18 || m > 59 || s > 59 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60 + s))
}

/// Java's legacy three-letter ids that map to region zones.
fn java_short_id(id: &str) -> Option<&'static str> {
    Some(match id {
        "ACT" => "Australia/Darwin",
        "AET" => "Australia/Sydney",
        "AGT" => "America/Argentina/Buenos_Aires",
        "ART" => "Africa/Cairo",
        "AST" => "America/Anchorage",
        "BET" => "America/Sao_Paulo",
        "BST" => "Asia/Dhaka",
        "CAT" => "Africa/Harare",
        "CNT" => "America/St_Johns",
        "CST" => "America/Chicago",
        "CTT" => "Asia/Shanghai",
        "EAT" => "Africa/Addis_Ababa",
        "ECT" => "Europe/Paris",
        "IET" => "America/Indiana/Indianapolis",
        "IST" => "Asia/Kolkata",
        "JST" => "Asia/Tokyo",
        "MIT" => "Pacific/Apia",
        "NET" => "Asia/Yerevan",
        "NST" => "Pacific/Auckland",
        "PLT" => "Asia/Karachi",
        "PNT" => "America/Phoenix",
        "PRT" => "America/Puerto_Rico",
        "PST" => "America/Los_Angeles",
        "SST" => "Pacific/Guadalcanal",
        "VST" => "Asia/Ho_Chi_Minh",
        _ => return None,
    })
}

/// The directories searched for TZif files (`$TZDIR` first).
pub fn zoneinfo_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(d) = std::env::var("TZDIR") {
        if !d.is_empty() {
            v.push(PathBuf::from(d));
        }
    }
    for d in ["/usr/share/zoneinfo", "/usr/lib/zoneinfo", "/usr/share/lib/zoneinfo", "/etc/zoneinfo"] {
        v.push(PathBuf::from(d));
    }
    v
}

/// Whether a zoneinfo database is available (tests skip when it is not).
pub fn zoneinfo_available() -> bool {
    find_zone_file("Europe/Paris").is_some()
}

fn find_zone_file(id: &str) -> Option<PathBuf> {
    let valid = !id.is_empty()
        && !id.starts_with('/')
        && id.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'));
    if !valid {
        return None;
    }
    for dir in zoneinfo_dirs() {
        let p = dir.join(id);
        if p.is_file() {
            if let Ok(mut f) = std::fs::File::open(&p) {
                let mut magic = [0u8; 4];
                if std::io::Read::read_exact(&mut f, &mut magic).is_ok() && &magic == b"TZif" {
                    return Some(p);
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// POSIX TZ strings
// ---------------------------------------------------------------------------

struct PosixParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl PosixParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn name(&mut self) -> Option<String> {
        if self.peek() == Some(b'<') {
            let start = self.i + 1;
            let end = start + self.s[start..].iter().position(|&b| b == b'>')?;
            self.i = end + 1;
            let n = String::from_utf8_lossy(&self.s[start..end]).to_string();
            return if n.len() >= 3 { Some(n) } else { None };
        }
        let start = self.i;
        while matches!(self.peek(), Some(b) if b.is_ascii_alphabetic()) {
            self.i += 1;
        }
        if self.i - start < 3 {
            return None;
        }
        Some(String::from_utf8_lossy(&self.s[start..self.i]).to_string())
    }
    fn number(&mut self) -> Option<i32> {
        let start = self.i;
        while matches!(self.peek(), Some(b) if b.is_ascii_digit()) {
            self.i += 1;
        }
        if start == self.i {
            return None;
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
    }
    /// `[+-]hh[:mm[:ss]]` in seconds.
    fn hms(&mut self, max_hours: i32) -> Option<i32> {
        let sign = match self.peek() {
            Some(b'+') => {
                self.i += 1;
                1
            }
            Some(b'-') => {
                self.i += 1;
                -1
            }
            _ => 1,
        };
        let h = self.number()?;
        if h > max_hours {
            return None;
        }
        let mut secs = h * 3600;
        if self.peek() == Some(b':') {
            self.i += 1;
            secs += self.number()? * 60;
            if self.peek() == Some(b':') {
                self.i += 1;
                secs += self.number()?;
            }
        }
        Some(sign * secs)
    }
    fn rule(&mut self) -> Option<Rule> {
        let date = match self.peek()? {
            b'J' => {
                self.i += 1;
                let n = self.number()?;
                if !(1..=365).contains(&n) {
                    return None;
                }
                RuleDate::Julian1(n as u16)
            }
            b'M' => {
                self.i += 1;
                let m = self.number()?;
                if self.peek() != Some(b'.') {
                    return None;
                }
                self.i += 1;
                let w = self.number()?;
                if self.peek() != Some(b'.') {
                    return None;
                }
                self.i += 1;
                let d = self.number()?;
                if !(1..=12).contains(&m) || !(1..=5).contains(&w) || !(0..=6).contains(&d) {
                    return None;
                }
                RuleDate::MonthWeekDay(m as u8, w as u8, d as u8)
            }
            b if b.is_ascii_digit() => {
                let n = self.number()?;
                if n > 365 {
                    return None;
                }
                RuleDate::Julian0(n as u16)
            }
            _ => return None,
        };
        let time = if self.peek() == Some(b'/') {
            self.i += 1;
            self.hms(167)?
        } else {
            7200
        };
        Some(Rule { date, time })
    }
}

/// Parse a POSIX TZ string (`EST5EDT,M3.2.0,M11.1.0`, `<+03>-3`, `CET-1CEST,M3.5.0,M10.5.0/3`).
fn parse_posix_tz(s: &str) -> Option<PosixTz> {
    let mut p = PosixParser { s: s.as_bytes(), i: 0 };
    let std_name = p.name()?;
    let std_off = -p.hms(24)?;
    let std = LocalType { offset: std_off, is_dst: false, abbr: std_name };
    if p.peek().is_none() {
        return Some(PosixTz { std, dst: None });
    }
    let dst_name = p.name()?;
    let dst_off = match p.peek() {
        Some(b',') | None => std_off + 3600,
        _ => -p.hms(24)?,
    };
    let (start, end) = if p.peek() == Some(b',') {
        p.i += 1;
        let start = p.rule()?;
        if p.peek() != Some(b',') {
            return None;
        }
        p.i += 1;
        let end = p.rule()?;
        (start, end)
    } else {
        // POSIX default (US rules)
        (Rule { date: RuleDate::MonthWeekDay(3, 2, 0), time: 7200 }, Rule { date: RuleDate::MonthWeekDay(11, 1, 0), time: 7200 })
    };
    if p.peek().is_some() {
        return None;
    }
    Some(PosixTz { std, dst: Some((LocalType { offset: dst_off, is_dst: true, abbr: dst_name }, start, end)) })
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
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

impl Rule {
    /// Local epoch seconds (wall clock of the offset in force before the transition) of the
    /// transition in `year`.
    fn local_time(&self, year: i64) -> i64 {
        let jan1 = epoch_from_civil(year, 1, 1, 0, 0, 0);
        let day_start = match self.date {
            RuleDate::Julian1(n) => {
                let mut d = n as i64 - 1;
                if is_leap(year) && n >= 60 {
                    d += 1;
                }
                jan1 + d * 86_400
            }
            RuleDate::Julian0(n) => jan1 + n as i64 * 86_400,
            RuleDate::MonthWeekDay(m, w, d) => {
                let first = epoch_from_civil(year, m as u32, 1, 0, 0, 0);
                let first_dow = (first.div_euclid(86_400) + 4).rem_euclid(7); // 0 = Sunday
                let mut day = 1 + (d as i64 - first_dow).rem_euclid(7) + (w as i64 - 1) * 7;
                let dim = days_in_month(year, m as u32) as i64;
                while day > dim {
                    day -= 7;
                }
                first + (day - 1) * 86_400
            }
        };
        day_start + self.time as i64
    }
}

impl PosixTz {
    fn local_type_at(&self, t: i64) -> LocalType {
        let Some((dst, start, end)) = &self.dst else {
            return self.std.clone();
        };
        let (year, ..) = civil_from_epoch(t + self.std.offset as i64);
        // The last transition at or before t among the surrounding years decides (this handles
        // both hemispheres and rule times that cross a year boundary).
        let mut last: Option<(i64, bool)> = None;
        for y in [year - 1, year, year + 1] {
            let s = start.local_time(y) - self.std.offset as i64;
            let e = end.local_time(y) - dst.offset as i64;
            for (when, is_dst) in [(s, true), (e, false)] {
                if when <= t && last.map(|(w, _)| when >= w).unwrap_or(true) {
                    last = Some((when, is_dst));
                }
            }
        }
        match last {
            Some((_, true)) => dst.clone(),
            _ => self.std.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i64, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        epoch_from_civil(y, mo, d, h, mi, 0)
    }

    #[test]
    fn fixed_offsets() {
        assert_eq!(parse_fixed_offset("UTC"), Some(0));
        assert_eq!(parse_fixed_offset("GMT+02:00"), Some(7200));
        assert_eq!(parse_fixed_offset("GMT-5"), Some(-18000));
        assert_eq!(parse_fixed_offset("UTC+0530"), Some(19800));
        assert_eq!(parse_fixed_offset("+01:00"), Some(3600));
        assert_eq!(parse_fixed_offset("GMT 02:00"), Some(7200));
        assert_eq!(parse_fixed_offset("Europe/Paris"), None);
        assert_eq!(parse_fixed_offset("EST5EDT"), None);
        assert_eq!(TimeZone::parse("GMT+2").unwrap().id(), "GMT+02:00");
    }

    #[test]
    fn posix_rules() {
        let z = TimeZone { id: "x".into(), kind: Kind::Posix(parse_posix_tz("CET-1CEST,M3.5.0,M10.5.0/3").unwrap()) };
        // 2024-03-31 01:00 UTC = 03:00 CEST
        assert_eq!(z.offset_at(at(2024, 3, 31, 0, 59)), 3600);
        assert_eq!(z.offset_at(at(2024, 3, 31, 1, 0)), 7200);
        assert_eq!(z.offset_at(at(2024, 10, 27, 0, 59)), 7200);
        assert_eq!(z.offset_at(at(2024, 10, 27, 1, 0)), 3600);
        assert_eq!(z.abbreviation_at(at(2024, 7, 1, 0, 0)), "CEST");
        // far future
        assert_eq!(z.offset_at(at(2100, 7, 1, 0, 0)), 7200);
        let ny = TimeZone { id: "y".into(), kind: Kind::Posix(parse_posix_tz("EST5EDT,M3.2.0,M11.1.0").unwrap()) };
        assert_eq!(ny.offset_at(at(2024, 11, 3, 5, 59)), -4 * 3600);
        assert_eq!(ny.offset_at(at(2024, 11, 3, 6, 0)), -5 * 3600);
        // southern hemisphere (Australia/Sydney)
        let syd = TimeZone { id: "s".into(), kind: Kind::Posix(parse_posix_tz("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap()) };
        assert_eq!(syd.offset_at(at(2024, 1, 15, 0, 0)), 11 * 3600);
        assert_eq!(syd.offset_at(at(2024, 6, 15, 0, 0)), 10 * 3600);
        assert_eq!(syd.offset_at(at(2024, 12, 15, 0, 0)), 11 * 3600);
        let q = parse_posix_tz("<+03>-3").unwrap();
        assert_eq!(q.std.offset, 3 * 3600);
        assert!(q.dst.is_none());
        assert!(parse_posix_tz("EST5EDT,M3.2.0").is_none());
        // version 3 extension: negative and > 24h transition times
        let p = parse_posix_tz("<-03>3<-02>,M3.5.0/-2,M10.5.0/-1").unwrap();
        assert_eq!(p.dst.as_ref().unwrap().1.time, -7200);
        assert!(parse_posix_tz("XXX3YYY,J60/170,300").is_none());
        assert!(parse_posix_tz("XXX3YYY,J60/167,300").is_some());
    }

    #[test]
    fn tzif_paris_and_new_york() {
        if !zoneinfo_available() {
            eprintln!("zoneinfo database not available: skipping");
            return;
        }
        let paris = TimeZone::parse("Europe/Paris").unwrap();
        // spring forward 2024-03-31 02:00 CET -> 03:00 CEST (01:00 UTC)
        assert_eq!(paris.offset_at(at(2024, 3, 31, 0, 59)), 3600);
        assert_eq!(paris.offset_at(at(2024, 3, 31, 1, 0)), 7200);
        assert_eq!(paris.abbreviation_at(at(2024, 3, 31, 1, 0)), "CEST");
        assert_eq!(paris.abbreviation_at(at(2024, 1, 1, 0, 0)), "CET");
        // gap: 02:30 local does not exist
        match paris.local_to_utc(at(2024, 3, 31, 2, 30)) {
            LocalResult::Gap { shifted, transition } => {
                assert_eq!(shifted, at(2024, 3, 31, 1, 30)); // = 03:30 CEST
                assert_eq!(transition, at(2024, 3, 31, 1, 0));
            }
            other => panic!("{:?}", other),
        }
        // overlap: 02:30 on 2024-10-27 happens twice
        assert_eq!(paris.local_to_utc(at(2024, 10, 27, 2, 30)), LocalResult::Ambiguous(at(2024, 10, 27, 0, 30), at(2024, 10, 27, 1, 30)));
        assert_eq!(paris.resolve_local(at(2024, 10, 27, 2, 30)), at(2024, 10, 27, 1, 30));
        assert_eq!(paris.local_to_utc(at(2024, 7, 14, 12, 0)), LocalResult::Single(at(2024, 7, 14, 10, 0)));

        let ny = TimeZone::parse("America/New_York").unwrap();
        // fall back 2024-11-03 02:00 EDT -> 01:00 EST (06:00 UTC)
        assert_eq!(ny.offset_at(at(2024, 11, 3, 5, 59)), -4 * 3600);
        assert_eq!(ny.offset_at(at(2024, 11, 3, 6, 0)), -5 * 3600);
        assert_eq!(ny.abbreviation_at(at(2024, 11, 3, 6, 0)), "EST");
        assert_eq!(ny.local_to_utc(at(2024, 11, 3, 1, 30)), LocalResult::Ambiguous(at(2024, 11, 3, 5, 30), at(2024, 11, 3, 6, 30)));
        // historical (before 1970) and far future (POSIX footer)
        assert_eq!(ny.offset_at(at(1960, 1, 1, 0, 0)), -5 * 3600);
        assert_eq!(ny.offset_at(at(2090, 7, 1, 12, 0)), -4 * 3600);
        assert_eq!(paris.offset_at(at(2090, 12, 1, 12, 0)), 3600);
        // zones without DST
        assert_eq!(TimeZone::parse("Asia/Tokyo").unwrap().offset_at(at(2024, 7, 1, 0, 0)), 9 * 3600);
        assert_eq!(TimeZone::parse("Asia/Kolkata").unwrap().offset_at(at(2030, 7, 1, 0, 0)), 19800);
        // java short ids
        assert_eq!(TimeZone::parse("ECT").unwrap().offset_at(at(2024, 7, 1, 0, 0)), 7200);
        assert!(TimeZone::parse("Nowhere/City").is_err());
        assert!(TimeZone::parse("../etc/passwd").is_err());
        assert_eq!(TimeZone::parse_lenient("Nowhere/City").offset_at(0), 0);
    }

    #[test]
    fn tz_env_values() {
        assert_eq!(TimeZone::from_tz_env(Some("")).offset_at(0), 0);
        let z = TimeZone::from_tz_env(Some("CET-1CEST,M3.5.0,M10.5.0/3"));
        assert_eq!(z.offset_at(at(2024, 7, 1, 0, 0)), 7200);
        let z = TimeZone::from_tz_env(Some("GMT+3"));
        assert_eq!(z.offset_at(0), 3 * 3600);
        if zoneinfo_available() {
            let z = TimeZone::from_tz_env(Some(":America/New_York"));
            assert_eq!(z.offset_at(at(2024, 1, 1, 0, 0)), -5 * 3600);
        }
    }

    #[test]
    fn tzif_v1_only_data() {
        // Hand-built v1 file: one transition at t=1000 from type 0 (+1h) to type 1 (+2h DST).
        let mut d = Vec::new();
        d.extend_from_slice(b"TZif");
        d.push(0);
        d.extend_from_slice(&[0u8; 15]);
        for c in [0u32, 0, 0, 1, 2, 8] {
            d.extend_from_slice(&c.to_be_bytes());
        }
        d.extend_from_slice(&1000i32.to_be_bytes());
        d.push(1);
        d.extend_from_slice(&3600i32.to_be_bytes());
        d.extend_from_slice(&[0, 0]);
        d.extend_from_slice(&7200i32.to_be_bytes());
        d.extend_from_slice(&[1, 4]);
        d.extend_from_slice(b"AAA\0BBB\0");
        let z = TimeZone::from_tzif("test", &d).unwrap();
        assert_eq!(z.offset_at(999), 3600);
        assert_eq!(z.offset_at(1000), 7200);
        assert_eq!(z.abbreviation_at(5000), "BBB");
        assert!(z.is_dst_at(5000));
        assert!(TimeZone::from_tzif("bad", b"TZjf").is_err());
    }
}
