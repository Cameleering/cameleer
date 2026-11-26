//! W3C Trace Context Level 1 (`traceparent`, `tracestate`) and W3C Baggage.
//!
//! Parsing follows the specifications' receiver rules: a `traceparent` that does not parse is
//! ignored (a new trace starts), a `tracestate` that does not parse is dropped while the
//! `traceparent` is kept, and baggage members that do not parse are skipped.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

/// The `traceparent` header.
pub const TRACEPARENT: &str = "traceparent";
/// The `tracestate` header.
pub const TRACESTATE: &str = "tracestate";
/// The `baggage` header.
pub const BAGGAGE: &str = "baggage";

/// Most `tracestate` list members a receiver keeps (W3C Trace Context, 3.3.1.1).
pub const TRACESTATE_MAX_MEMBERS: usize = 32;
/// Most baggage list members kept (W3C Baggage, 3.3.3 asks for at least 64).
pub const BAGGAGE_MAX_MEMBERS: usize = 180;
/// Most bytes of a `baggage` header (W3C Baggage asks for at least 8192).
pub const BAGGAGE_MAX_BYTES: usize = 8192;

/// The identity of a span as it travels between processes: trace id, span id, trace flags and
/// trace state.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SpanContext {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    /// Trace flags; bit 0 is `sampled`.
    pub flags: u8,
    /// The `tracestate` header value (validated, members joined with `,`), or empty.
    pub trace_state: String,
    /// True when the context came from another process (extracted from headers).
    pub remote: bool,
}

impl SpanContext {
    /// Whether both ids are non-zero.
    pub fn is_valid(&self) -> bool {
        self.trace_id != [0; 16] && self.span_id != [0; 8]
    }

    /// Whether the `sampled` flag is set.
    pub fn is_sampled(&self) -> bool {
        self.flags & 1 == 1
    }

    /// The trace id as 32 lowercase hex digits.
    pub fn trace_id_hex(&self) -> String {
        hex(&self.trace_id)
    }

    /// The span id as 16 lowercase hex digits.
    pub fn span_id_hex(&self) -> String {
        hex(&self.span_id)
    }

    /// The `traceparent` header value (version `00`).
    pub fn traceparent(&self) -> String {
        let mut s = String::with_capacity(55);
        s.push_str("00-");
        push_hex(&mut s, &self.trace_id);
        s.push('-');
        push_hex(&mut s, &self.span_id);
        s.push('-');
        push_hex(&mut s, &[self.flags]);
        s
    }

    /// Parses a `traceparent` header value. Version `00` must be exactly 55 characters; a
    /// higher version may carry more fields after a `-`, which are ignored; version `ff`, upper
    /// case hex and all-zero ids are invalid.
    pub fn parse_traceparent(value: &str) -> Option<SpanContext> {
        let v = value.trim_matches(|c| c == ' ' || c == '\t');
        let b = v.as_bytes();
        if b.len() < 55 {
            return None;
        }
        let version = parse_hex::<1>(&b[0..2])?[0];
        if version == 0xff {
            return None;
        }
        if version == 0 && b.len() != 55 {
            return None;
        }
        if b.len() > 55 && b[55] != b'-' {
            return None;
        }
        if b[2] != b'-' || b[35] != b'-' || b[52] != b'-' {
            return None;
        }
        let trace_id = parse_hex::<16>(&b[3..35])?;
        let span_id = parse_hex::<8>(&b[36..52])?;
        let flags = parse_hex::<1>(&b[53..55])?[0];
        let ctx = SpanContext { trace_id, span_id, flags, trace_state: String::new(), remote: true };
        ctx.is_valid().then_some(ctx)
    }
}

/// Validates a `tracestate` header value: list members `key=value`, at most
/// [`TRACESTATE_MAX_MEMBERS`], keys `[a-z0-9][a-z0-9_-*/]{0,255}` with an optional
/// `@system` part, values of printable ASCII without `,` and `=`, at most 256 characters, not
/// ending in a space. Returns the members joined with `,`, or None when the value is invalid
/// (the receiver then drops the whole header).
pub fn parse_tracestate(value: &str) -> Option<String> {
    let mut members: Vec<&str> = Vec::new();
    for m in value.split(',') {
        let m = m.trim_matches(|c| c == ' ' || c == '\t');
        if m.is_empty() {
            continue;
        }
        let (k, v) = m.split_once('=')?;
        if !valid_tracestate_key(k) || !valid_tracestate_value(v) {
            return None;
        }
        if members.iter().any(|o| o.split_once('=').map(|(ok, _)| ok) == Some(k)) {
            return None;
        }
        members.push(m);
    }
    if members.len() > TRACESTATE_MAX_MEMBERS {
        return None;
    }
    Some(members.join(","))
}

fn valid_tracestate_key(k: &str) -> bool {
    let simple = |s: &str, max: usize, first_digit: bool| {
        let b = s.as_bytes();
        !b.is_empty()
            && b.len() <= max
            && (b[0].is_ascii_lowercase() || (first_digit && b[0].is_ascii_digit()))
            && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-' | b'*' | b'/'))
    };
    match k.split_once('@') {
        None => simple(k, 256, true),
        Some((tenant, system)) => simple(tenant, 241, true) && simple(system, 14, false),
    }
}

fn valid_tracestate_value(v: &str) -> bool {
    let b = v.as_bytes();
    !b.is_empty() && b.len() <= 256 && b[b.len() - 1] != b' ' && b.iter().all(|&c| (0x20..=0x7e).contains(&c) && c != b',' && c != b'=')
}

/// One baggage list member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaggageEntry {
    pub key: String,
    /// The percent-decoded value.
    pub value: String,
    /// Member properties (`;k=v;flag`), as received.
    pub metadata: String,
}

/// Parses a `baggage` header: `key=value;properties` members separated by `,`; values are
/// percent-decoded. Members that do not parse are skipped; at most [`BAGGAGE_MAX_MEMBERS`]
/// members and [`BAGGAGE_MAX_BYTES`] bytes are read.
pub fn parse_baggage(value: &str) -> Vec<BaggageEntry> {
    let value = if value.len() > BAGGAGE_MAX_BYTES {
        // cut at the last member boundary inside the limit
        let cut = value[..floor_char(value, BAGGAGE_MAX_BYTES)].rfind(',').unwrap_or(0);
        &value[..cut]
    } else {
        value
    };
    let mut out: Vec<BaggageEntry> = Vec::new();
    for m in value.split(',') {
        if out.len() >= BAGGAGE_MAX_MEMBERS {
            break;
        }
        let (kv, meta) = match m.split_once(';') {
            Some((a, b)) => (a, b.trim()),
            None => (m, ""),
        };
        let Some((k, v)) = kv.split_once('=') else { continue };
        let k = k.trim();
        if k.is_empty() || !k.bytes().all(is_token_char) {
            continue;
        }
        let Some(v) = percent_decode(v.trim()) else { continue };
        out.retain(|e| e.key != k);
        out.push(BaggageEntry { key: k.to_string(), value: v, metadata: meta.to_string() });
    }
    out
}

/// Formats baggage entries as a `baggage` header (values percent-encoded).
pub fn format_baggage(entries: &[BaggageEntry]) -> String {
    let mut s = String::new();
    for e in entries {
        if !s.is_empty() {
            s.push(',');
        }
        s.push_str(&e.key);
        s.push('=');
        for b in e.value.bytes() {
            // baggage-octet: printable ASCII without space, `"`, `,`, `;`, `\` and `%`
            if (0x21..=0x7e).contains(&b) && !matches!(b, b'"' | b',' | b';' | b'\\' | b'%') {
                s.push(b as char);
            } else {
                s.push_str(&format!("%{:02X}", b));
            }
        }
        if !e.metadata.is_empty() {
            s.push(';');
            s.push_str(&e.metadata);
        }
    }
    s
}

fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn is_token_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = parse_hex::<1>(b.get(i + 1..i + 3)?).or_else(|| {
                // percent-encoding may use upper case hex
                let t = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                u8::from_str_radix(t, 16).ok().map(|x| [x])
            })?;
            out.push(h[0]);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Lower case hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    push_hex(&mut s, bytes);
    s
}

fn push_hex(s: &mut String, bytes: &[u8]) {
    const D: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
}

/// Parses exactly `2 * N` lowercase hex digits.
fn parse_hex<const N: usize>(b: &[u8]) -> Option<[u8; N]> {
    if b.len() != 2 * N {
        return None;
    }
    let d = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for i in 0..N {
        out[i] = (d(b[2 * i])? << 4) | d(b[2 * i + 1])?;
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// ids
// ---------------------------------------------------------------------------

static SEED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static RNG: Cell<u64> = const { Cell::new(0) };
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A random u64 from a per-thread generator (splitmix64). Each thread's state starts from the
/// time, the process id, the thread's stack address and a process-wide counter, so threads
/// never share a sequence. Ids need to be unique, not secret: this is not a cryptographic
/// generator.
pub fn random_u64() -> u64 {
    RNG.with(|c| {
        let mut s = c.get();
        if s == 0 {
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
            let local = 0u8;
            let n = SEED.fetch_add(1, Ordering::Relaxed);
            let mut m = t ^ ((std::process::id() as u64) << 32) ^ (&local as *const u8 as u64).rotate_left(17) ^ n.wrapping_mul(0xD6E8_FEB8_6659_FD93);
            s = splitmix(&mut m) | 1;
        }
        let r = splitmix(&mut s);
        c.set(s);
        r
    })
}

/// A new random, valid trace id.
pub fn new_trace_id() -> [u8; 16] {
    loop {
        let mut id = [0u8; 16];
        id[..8].copy_from_slice(&random_u64().to_be_bytes());
        id[8..].copy_from_slice(&random_u64().to_be_bytes());
        if id != [0; 16] {
            return id;
        }
    }
}

/// A new random, valid span id.
pub fn new_span_id() -> [u8; 8] {
    loop {
        let id = random_u64().to_be_bytes();
        if id != [0; 8] {
            return id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traceparent_round_trip_and_receiver_rules() {
        let tp = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let c = SpanContext::parse_traceparent(tp).unwrap();
        assert_eq!(c.trace_id_hex(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(c.span_id_hex(), "00f067aa0ba902b7");
        assert!(c.is_sampled() && c.remote);
        assert_eq!(c.traceparent(), tp);
        // upper case, all-zero ids, version ff, bad length, bad separators
        for bad in [
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
            "00_4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "",
        ] {
            assert!(SpanContext::parse_traceparent(bad).is_none(), "{bad}");
        }
        // a future version may carry more fields
        let f = SpanContext::parse_traceparent("cc-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00-what-ever").unwrap();
        assert!(!f.is_sampled());
        assert!(SpanContext::parse_traceparent("cc-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00x").is_none());
    }

    #[test]
    fn tracestate_validation() {
        assert_eq!(parse_tracestate("rojo=00f067aa0ba902b7, congo=t61rcWkgMzE").as_deref(), Some("rojo=00f067aa0ba902b7,congo=t61rcWkgMzE"));
        assert_eq!(parse_tracestate("t@sys=1,,").as_deref(), Some("t@sys=1"));
        assert!(parse_tracestate("Upper=1").is_none());
        assert!(parse_tracestate("a=1,a=2").is_none());
        assert!(parse_tracestate("a=x,y").is_none());
        let many: Vec<String> = (0..33).map(|i| format!("k{i}=v")).collect();
        assert!(parse_tracestate(&many.join(",")).is_none());
        assert!(parse_tracestate(&many[..32].join(",")).is_some());
    }

    #[test]
    fn baggage_parse_and_format() {
        let b = parse_baggage("userId=alice, serverNode=DF%2028,isProduction=false;ttl=1, bad, =x");
        assert_eq!(b.len(), 3);
        assert_eq!(b[1].value, "DF 28");
        assert_eq!(b[2].metadata, "ttl=1");
        assert_eq!(format_baggage(&b), "userId=alice,serverNode=DF%2028,isProduction=false;ttl=1");
        let big: Vec<String> = (0..300).map(|i| format!("k{i}=v")).collect();
        assert_eq!(parse_baggage(&big.join(",")).len(), BAGGAGE_MAX_MEMBERS);
    }

    #[test]
    fn ids_are_unique_across_threads() {
        let all: Vec<u64> = (0..8)
            .map(|_| std::thread::spawn(|| (0..2000).map(|_| u64::from_be_bytes(new_span_id())).collect::<Vec<_>>()))
            .flat_map(|h| h.join().unwrap())
            .collect();
        let set: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(set.len(), all.len());
    }
}
