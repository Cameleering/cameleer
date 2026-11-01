//! A small std-only DNS client (RFC 1035) for `SRV` and `TXT` lookups over UDP (with TCP
//! fallback for truncated replies), used by `mongodb+srv://` connection strings, and an
//! embedded DNS server ([`DnsServer`]) for tests.
//!
//! The name server comes from the explicit setting, the `CAMEL_DNS_SERVER` environment
//! variable, or the first `nameserver` of `/etc/resolv.conf`.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use camel_core::error::{CamelError, Result};

/// Record type `TXT`.
pub const TYPE_TXT: u16 = 16;
/// Record type `SRV`.
pub const TYPE_SRV: u16 = 33;
/// Record type `A`.
pub const TYPE_A: u16 = 1;

/// An `SRV` record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrvRecord {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

static QUERY_ID: AtomicU16 = AtomicU16::new(0x1234);

fn dns_error(m: impl Into<String>) -> CamelError {
    CamelError::new("UnknownHostException", m.into())
}

/// The name server to use (`host:port`).
pub fn default_server() -> String {
    if let Ok(s) = std::env::var("CAMEL_DNS_SERVER") {
        if !s.is_empty() {
            return s;
        }
    }
    if let Ok(conf) = std::fs::read_to_string("/etc/resolv.conf") {
        for line in conf.lines() {
            let mut it = line.split_whitespace();
            if it.next() == Some("nameserver") {
                if let Some(ip) = it.next() {
                    return if ip.contains(':') { format!("[{}]:53", ip) } else { format!("{}:53", ip) };
                }
            }
        }
    }
    "127.0.0.1:53".to_string()
}

fn encode_name(name: &str, out: &mut Vec<u8>) {
    for label in name.trim_end_matches('.').split('.').filter(|l| !l.is_empty()) {
        out.push(label.len().min(63) as u8);
        out.extend_from_slice(&label.as_bytes()[..label.len().min(63)]);
    }
    out.push(0);
}

/// Builds a query message.
pub fn build_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut q = Vec::with_capacity(64);
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    q.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]);
    encode_name(name, &mut q);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&1u16.to_be_bytes());
    q
}

/// Reads a (possibly compressed) name at `pos`; returns the name and the position after it.
fn read_name(msg: &[u8], mut pos: usize) -> Result<(String, usize)> {
    let mut labels = Vec::new();
    let mut end = None;
    let mut jumps = 0;
    loop {
        let len = *msg.get(pos).ok_or_else(|| dns_error("truncated DNS name"))? as usize;
        if len & 0xC0 == 0xC0 {
            let ptr = ((len & 0x3F) << 8) | *msg.get(pos + 1).ok_or_else(|| dns_error("truncated DNS pointer"))? as usize;
            if end.is_none() {
                end = Some(pos + 2);
            }
            jumps += 1;
            if jumps > 32 {
                return Err(dns_error("DNS name compression loop"));
            }
            pos = ptr;
            continue;
        }
        if len == 0 {
            pos += 1;
            break;
        }
        let label = msg.get(pos + 1..pos + 1 + len).ok_or_else(|| dns_error("truncated DNS label"))?;
        labels.push(String::from_utf8_lossy(label).into_owned());
        pos += 1 + len;
    }
    Ok((labels.join("."), end.unwrap_or(pos)))
}

/// A resource record of a reply: (name, type, rdata start, rdata length).
struct Rr {
    rtype: u16,
    start: usize,
    len: usize,
}

fn parse_reply(msg: &[u8], id: u16) -> Result<(bool, Vec<Rr>)> {
    if msg.len() < 12 {
        return Err(dns_error("short DNS reply"));
    }
    if u16::from_be_bytes([msg[0], msg[1]]) != id {
        return Err(dns_error("DNS reply id mismatch"));
    }
    let flags = u16::from_be_bytes([msg[2], msg[3]]);
    let truncated = flags & 0x0200 != 0;
    let rcode = flags & 0x000F;
    if rcode == 3 {
        return Err(dns_error("DNS name does not exist (NXDOMAIN)"));
    }
    if rcode != 0 {
        return Err(dns_error(format!("DNS server failure (rcode {})", rcode)));
    }
    let qd = u16::from_be_bytes([msg[4], msg[5]]) as usize;
    let an = u16::from_be_bytes([msg[6], msg[7]]) as usize;
    let mut pos = 12;
    for _ in 0..qd {
        pos = read_name(msg, pos)?.1 + 4;
    }
    let mut out = Vec::new();
    for _ in 0..an {
        pos = read_name(msg, pos)?.1;
        let h = msg.get(pos..pos + 10).ok_or_else(|| dns_error("truncated DNS record"))?;
        let rtype = u16::from_be_bytes([h[0], h[1]]);
        let len = u16::from_be_bytes([h[8], h[9]]) as usize;
        pos += 10;
        if pos + len > msg.len() {
            return Err(dns_error("truncated DNS rdata"));
        }
        out.push(Rr { rtype, start: pos, len });
        pos += len;
    }
    Ok((truncated, out))
}

/// Sends a query and returns the raw reply (UDP, TCP when truncated).
pub fn query(server: &str, name: &str, qtype: u16, timeout: Duration) -> Result<Vec<u8>> {
    let addr: SocketAddr = server.to_socket_addrs().map_err(|e| dns_error(format!("bad DNS server {}: {}", server, e)))?.next().ok_or_else(|| dns_error(format!("bad DNS server {}", server)))?;
    let id = QUERY_ID.fetch_add(1, Ordering::Relaxed);
    let q = build_query(id, name, qtype);
    let bind = if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let sock = UdpSocket::bind(bind).map_err(|e| dns_error(e.to_string()))?;
    sock.set_read_timeout(Some(timeout)).map_err(|e| dns_error(e.to_string()))?;
    let mut buf = vec![0u8; 4096];
    let mut reply = None;
    for _ in 0..3 {
        sock.send_to(&q, addr).map_err(|e| dns_error(format!("DNS query to {} failed: {}", addr, e)))?;
        match sock.recv_from(&mut buf) {
            Ok((n, from)) if from == addr && n >= 2 && u16::from_be_bytes([buf[0], buf[1]]) == id => {
                reply = Some(buf[..n].to_vec());
                break;
            }
            Ok(_) => continue,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(dns_error(format!("DNS query to {} failed: {}", addr, e))),
        }
    }
    let reply = reply.ok_or_else(|| dns_error(format!("DNS query for {} timed out ({})", name, addr)))?;
    let (truncated, _) = parse_reply(&reply, id)?;
    if !truncated {
        return Ok(reply);
    }
    // TCP retry (2-byte length prefix)
    let mut s = TcpStream::connect_timeout(&addr, timeout).map_err(|e| dns_error(e.to_string()))?;
    s.set_read_timeout(Some(timeout)).map_err(|e| dns_error(e.to_string()))?;
    let mut framed = (q.len() as u16).to_be_bytes().to_vec();
    framed.extend_from_slice(&q);
    s.write_all(&framed).map_err(|e| dns_error(e.to_string()))?;
    let mut len = [0u8; 2];
    s.read_exact(&mut len).map_err(|e| dns_error(e.to_string()))?;
    let mut msg = vec![0u8; u16::from_be_bytes(len) as usize];
    s.read_exact(&mut msg).map_err(|e| dns_error(e.to_string()))?;
    Ok(msg)
}

/// `SRV` lookup (sorted by priority, then weight descending).
pub fn lookup_srv(server: &str, name: &str, timeout: Duration) -> Result<Vec<SrvRecord>> {
    let msg = query(server, name, TYPE_SRV, timeout)?;
    let id = u16::from_be_bytes([msg[0], msg[1]]);
    let (_, rrs) = parse_reply(&msg, id)?;
    let mut out = Vec::new();
    for rr in rrs.iter().filter(|r| r.rtype == TYPE_SRV && r.len >= 7) {
        let d = &msg[rr.start..rr.start + 6];
        let (target, _) = read_name(&msg, rr.start + 6)?;
        out.push(SrvRecord { priority: u16::from_be_bytes([d[0], d[1]]), weight: u16::from_be_bytes([d[2], d[3]]), port: u16::from_be_bytes([d[4], d[5]]), target });
    }
    out.sort_by(|a, b| a.priority.cmp(&b.priority).then(b.weight.cmp(&a.weight)));
    Ok(out)
}

/// `TXT` lookup: one string per record (its character-strings concatenated).
pub fn lookup_txt(server: &str, name: &str, timeout: Duration) -> Result<Vec<String>> {
    let msg = match query(server, name, TYPE_TXT, timeout) {
        Ok(m) => m,
        Err(e) if e.message.contains("NXDOMAIN") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let id = u16::from_be_bytes([msg[0], msg[1]]);
    let (_, rrs) = parse_reply(&msg, id)?;
    let mut out = Vec::new();
    for rr in rrs.iter().filter(|r| r.rtype == TYPE_TXT) {
        let mut s = String::new();
        let mut p = rr.start;
        while p < rr.start + rr.len {
            let l = msg[p] as usize;
            s.push_str(&String::from_utf8_lossy(&msg[p + 1..(p + 1 + l).min(rr.start + rr.len)]));
            p += 1 + l;
        }
        out.push(s);
    }
    Ok(out)
}

/// A record served by [`DnsServer`].
#[derive(Clone, Debug)]
pub enum DnsRecord {
    Srv(SrvRecord),
    Txt(String),
    A([u8; 4]),
}

/// An embedded UDP DNS server answering configured `SRV` / `TXT` / `A` records (tests).
pub struct DnsServer {
    addr: SocketAddr,
    records: Arc<Mutex<HashMap<(String, u16), Vec<DnsRecord>>>>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
    /// Queries received `(name, type)`.
    pub queries: Arc<Mutex<Vec<(String, u16)>>>,
}

fn encode_record(name: &str, r: &DnsRecord, out: &mut Vec<u8>) {
    encode_name(name, out);
    let (ty, rdata) = match r {
        DnsRecord::Srv(s) => {
            let mut d = Vec::new();
            d.extend_from_slice(&s.priority.to_be_bytes());
            d.extend_from_slice(&s.weight.to_be_bytes());
            d.extend_from_slice(&s.port.to_be_bytes());
            encode_name(&s.target, &mut d);
            (TYPE_SRV, d)
        }
        DnsRecord::Txt(t) => {
            let mut d = Vec::new();
            for chunk in t.as_bytes().chunks(255) {
                d.push(chunk.len() as u8);
                d.extend_from_slice(chunk);
            }
            if t.is_empty() {
                d.push(0);
            }
            (TYPE_TXT, d)
        }
        DnsRecord::A(ip) => (TYPE_A, ip.to_vec()),
    };
    out.extend_from_slice(&ty.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&60u32.to_be_bytes());
    out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    out.extend_from_slice(&rdata);
}

impl DnsServer {
    /// Starts a server on `127.0.0.1` with an ephemeral port.
    pub fn start() -> io::Result<DnsServer> {
        let sock = UdpSocket::bind("127.0.0.1:0")?;
        sock.set_read_timeout(Some(Duration::from_millis(50)))?;
        let addr = sock.local_addr()?;
        let records: Arc<Mutex<HashMap<(String, u16), Vec<DnsRecord>>>> = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let queries = Arc::new(Mutex::new(Vec::new()));
        let (recs, st, qs) = (records.clone(), stop.clone(), queries.clone());
        let thread = std::thread::Builder::new().name("dns-server".into()).spawn(move || {
            let mut buf = [0u8; 1500];
            while !st.load(Ordering::SeqCst) {
                let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
                let msg = &buf[..n];
                if n < 12 {
                    continue;
                }
                let Ok((name, p)) = read_name(msg, 12) else { continue };
                if p + 4 > n {
                    continue;
                }
                let qtype = u16::from_be_bytes([msg[p], msg[p + 1]]);
                let key = (name.to_ascii_lowercase(), qtype);
                qs.lock().unwrap().push(key.clone());
                let answers = recs.lock().unwrap().get(&key).cloned();
                let known_name = recs.lock().unwrap().keys().any(|(k, _)| *k == key.0);
                let mut out = Vec::new();
                out.extend_from_slice(&msg[0..2]);
                let rcode = if answers.is_none() && !known_name { 3 } else { 0 };
                out.extend_from_slice(&(0x8180u16 | rcode).to_be_bytes());
                out.extend_from_slice(&1u16.to_be_bytes());
                let list = answers.unwrap_or_default();
                out.extend_from_slice(&(list.len() as u16).to_be_bytes());
                out.extend_from_slice(&[0, 0, 0, 0]);
                out.extend_from_slice(&msg[12..p + 4]);
                for r in &list {
                    encode_record(&name, r, &mut out);
                }
                let _ = sock.send_to(&out, from);
            }
        })?;
        Ok(DnsServer { addr, records, stop, thread: Mutex::new(Some(thread)), queries })
    }

    /// `127.0.0.1:port`.
    pub fn address(&self) -> String {
        self.addr.to_string()
    }

    /// Adds a record for `name`.
    pub fn add(&self, name: &str, record: DnsRecord) {
        let ty = match &record {
            DnsRecord::Srv(_) => TYPE_SRV,
            DnsRecord::Txt(_) => TYPE_TXT,
            DnsRecord::A(_) => TYPE_A,
        };
        self.records.lock().unwrap().entry((name.trim_end_matches('.').to_ascii_lowercase(), ty)).or_default().push(record);
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.lock().unwrap().take() {
            let _ = t.join();
        }
    }
}

impl Drop for DnsServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srv_and_txt_lookups() {
        let s = DnsServer::start().unwrap();
        s.add("_mongodb._tcp.cluster0.example.com", DnsRecord::Srv(SrvRecord { priority: 0, weight: 5, port: 27017, target: "a.example.com".into() }));
        s.add("_mongodb._tcp.cluster0.example.com", DnsRecord::Srv(SrvRecord { priority: 0, weight: 10, port: 27018, target: "b.example.com.".into() }));
        s.add("cluster0.example.com", DnsRecord::Txt("replicaSet=rs0&authSource=admin".into()));
        let t = Duration::from_secs(2);
        let srv = lookup_srv(&s.address(), "_mongodb._tcp.cluster0.example.com", t).unwrap();
        assert_eq!(srv.len(), 2);
        assert_eq!(srv[0].target, "b.example.com");
        assert_eq!(srv[0].port, 27018);
        assert_eq!(lookup_txt(&s.address(), "cluster0.example.com", t).unwrap(), vec!["replicaSet=rs0&authSource=admin"]);
        assert!(lookup_txt(&s.address(), "none.example.com", t).unwrap().is_empty());
        assert!(lookup_srv(&s.address(), "_mongodb._tcp.none.example.com", t).is_err());
        // compression pointers in names
        let mut msg = vec![0u8; 12];
        msg.extend_from_slice(&[3, b'f', b'o', b'o', 3, b'c', b'o', b'm', 0]);
        msg.extend_from_slice(&[3, b'b', b'a', b'r', 0xC0, 12]);
        assert_eq!(read_name(&msg, 21).unwrap(), ("bar.foo.com".to_string(), 27));
    }
}
