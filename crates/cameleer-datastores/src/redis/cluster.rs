//! Redis Cluster key slots: CRC16 (XMODEM) of the key or of its `{hash tag}`, modulo
//! 16384, and parsing of `MOVED` / `ASK` redirections.

/// CRC16-CCITT (XMODEM) as used by Redis Cluster.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// The cluster slot of a key (`{tag}` hash tags honoured).
pub fn key_slot(key: &[u8]) -> u16 {
    let k = match key.iter().position(|&c| c == b'{') {
        Some(s) => match key[s + 1..].iter().position(|&c| c == b'}') {
            Some(e) if e > 0 => &key[s + 1..s + 1 + e],
            _ => key,
        },
        None => key,
    };
    crc16(k) % 16384
}

/// A cluster redirection parsed from an error reply.
#[derive(Clone, Debug, PartialEq)]
pub enum Redirect {
    /// `MOVED <slot> <host:port>`: the slot lives on another node from now on.
    Moved(u16, String, u16),
    /// `ASK <slot> <host:port>`: retry this command once on that node after `ASKING`.
    Ask(u16, String, u16),
}

/// Parses `MOVED` / `ASK` error messages.
pub fn parse_redirect(msg: &str) -> Option<Redirect> {
    let mut it = msg.split_whitespace();
    let kind = it.next()?;
    let slot: u16 = it.next()?.parse().ok()?;
    let addr = it.next()?;
    let (h, p) = addr.rsplit_once(':')?;
    let host = if h.is_empty() { "127.0.0.1".to_string() } else { h.to_string() };
    let port: u16 = p.parse().ok()?;
    match kind {
        "MOVED" => Some(Redirect::Moved(slot, host, port)),
        "ASK" => Some(Redirect::Ask(slot, host, port)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots() {
        assert_eq!(crc16(b"123456789"), 0x31C3);
        assert_eq!(key_slot(b"foo"), 12182);
        assert_eq!(key_slot(b"{user1000}.following"), key_slot(b"{user1000}.followers"));
        assert_eq!(key_slot(b"{user1000}.following"), key_slot(b"user1000"));
        assert_eq!(key_slot(b"foo{}{bar}"), crc16(b"foo{}{bar}") % 16384);
        assert_eq!(parse_redirect("MOVED 3999 127.0.0.1:6381"), Some(Redirect::Moved(3999, "127.0.0.1".into(), 6381)));
        assert_eq!(parse_redirect("ASK 3999 :6381"), Some(Redirect::Ask(3999, "127.0.0.1".into(), 6381)));
        assert_eq!(parse_redirect("ERR nope"), None);
    }
}
