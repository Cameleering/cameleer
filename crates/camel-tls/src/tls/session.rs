//! Session resumption state: the client session cache, server ticket encryption
//! (TLS 1.3 PSK tickets and TLS 1.2 RFC 5077 tickets) and the TLS 1.2 server
//! session-ID cache.

use super::codec::{put_u16, put_vec16, put_vec24, put_vec8, Reader};
use super::{CipherSuite, TlsVersion};
use crate::crypto::aes::AesGcm;
use crate::crypto::rng;
use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// A resumable session remembered by a client.
#[derive(Clone, Debug)]
pub(crate) struct ClientSession {
    pub version: TlsVersion,
    pub suite: CipherSuite,
    /// TLS 1.3: the PSK; TLS 1.2: the master secret.
    pub secret: Vec<u8>,
    /// TLS 1.3 ticket or TLS 1.2 RFC 5077 ticket (empty when ID based).
    pub ticket: Vec<u8>,
    /// TLS 1.2 session ID.
    pub session_id: Vec<u8>,
    pub ems: bool,
    pub age_add: u32,
    pub received_ms: u64,
    pub lifetime_secs: u32,
    pub server_name: String,
    pub peer_certs: Vec<Vec<u8>>,
    /// TLS 1.3 tickets: the server's max_early_data_size (0 = no 0-RTT).
    pub max_early_data: u32,
}

impl ClientSession {
    pub fn expired(&self, now: u64) -> bool {
        now >= self.received_ms + self.lifetime_secs as u64 * 1000
    }
}

/// Client-side session cache shared by all connections made with one
/// [`TlsClientConfig`](super::config::TlsClientConfig) (keyed by `host:port` or an
/// explicit key, e.g. the FTPS control connection for its data connections).
pub struct ClientSessionCache {
    map: Mutex<HashMap<String, ClientSession>>,
    capacity: usize,
}

impl fmt::Debug for ClientSessionCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClientSessionCache({} entries)", self.len())
    }
}

impl Default for ClientSessionCache {
    fn default() -> Self {
        ClientSessionCache::new(256)
    }
}

impl ClientSessionCache {
    pub fn new(capacity: usize) -> ClientSessionCache {
        ClientSessionCache { map: Mutex::new(HashMap::new()), capacity: capacity.max(1) }
    }

    pub(crate) fn get(&self, key: &str) -> Option<ClientSession> {
        let mut m = self.map.lock().ok()?;
        let now = now_ms();
        match m.get(key) {
            Some(s) if s.expired(now) => {
                m.remove(key);
                None
            }
            Some(s) => Some(s.clone()),
            None => None,
        }
    }

    pub(crate) fn put(&self, key: &str, s: ClientSession) {
        if let Ok(mut m) = self.map.lock() {
            if m.len() >= self.capacity && !m.contains_key(key) {
                let now = now_ms();
                m.retain(|_, v| !v.expired(now));
                if m.len() >= self.capacity {
                    if let Some(k) = m.iter().min_by_key(|(_, v)| v.received_ms).map(|(k, _)| k.clone()) {
                        m.remove(&k);
                    }
                }
            }
            m.insert(key.to_string(), s);
        }
    }

    /// Forgets the session stored under `key`.
    pub fn remove(&self, key: &str) {
        if let Ok(mut m) = self.map.lock() {
            m.remove(key);
        }
    }

    /// Whether a (not expired) session is stored under `key`.
    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn len(&self) -> usize {
        self.map.lock().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut m) = self.map.lock() {
            m.clear();
        }
    }
}

/// Session state carried in a server ticket or the server session cache.
#[derive(Clone, Debug)]
pub(crate) struct ServerSession {
    pub version: TlsVersion,
    pub suite: CipherSuite,
    pub secret: Vec<u8>,
    pub ems: bool,
    pub created: u64,
    pub age_add: u32,
    /// Identifies the full handshake the session descends from.
    pub origin: Vec<u8>,
    pub peer_certs: Vec<Vec<u8>>,
}

impl ServerSession {
    fn encode(&self) -> Vec<u8> {
        let mut out = vec![if self.version == TlsVersion::Tls13 { 13 } else { 12 }];
        put_u16(&mut out, self.suite.id());
        out.extend_from_slice(&self.created.to_be_bytes());
        out.extend_from_slice(&self.age_add.to_be_bytes());
        out.push(self.ems as u8);
        put_vec8(&mut out, &self.secret);
        put_vec8(&mut out, &self.origin);
        out.push(self.peer_certs.len().min(255) as u8);
        for c in self.peer_certs.iter().take(255) {
            put_vec24(&mut out, c);
        }
        out
    }

    fn decode(data: &[u8]) -> Option<ServerSession> {
        let mut r = Reader::new(data, "ticket");
        let version = match r.u8().ok()? {
            13 => TlsVersion::Tls13,
            12 => TlsVersion::Tls12,
            _ => return None,
        };
        let suite = CipherSuite::from_id(r.u16().ok()?)?;
        let created = u64::from_be_bytes(r.take(8).ok()?.try_into().ok()?);
        let age_add = u32::from_be_bytes(r.take(4).ok()?.try_into().ok()?);
        let ems = r.u8().ok()? != 0;
        let secret = r.vec8().ok()?.to_vec();
        let origin = r.vec8().ok()?.to_vec();
        let n = r.u8().ok()?;
        let mut peer_certs = Vec::new();
        for _ in 0..n {
            peer_certs.push(r.vec24().ok()?.to_vec());
        }
        r.finish().ok()?;
        Some(ServerSession { version, suite, secret, ems, created, age_add, origin, peer_certs })
    }
}

/// Server-side resumption state: the ticket encryption key (AES-256-GCM) and the
/// TLS 1.2 session-ID cache. Shared by clones of a server configuration.
pub struct ServerSessions {
    key_name: [u8; 16],
    aead: AesGcm,
    pub(crate) lifetime_secs: u32,
    pub(crate) tickets: bool,
    cache: Mutex<HashMap<Vec<u8>, ServerSession>>,
    capacity: usize,
}

impl fmt::Debug for ServerSessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ServerSessions(tickets: {}, lifetime: {}s)", self.tickets, self.lifetime_secs)
    }
}

impl ServerSessions {
    pub fn new(lifetime_secs: u32, tickets: bool) -> ServerSessions {
        let mut key_name = [0u8; 16];
        rng::fill(&mut key_name);
        let key = rng::random_bytes(32);
        ServerSessions {
            key_name,
            aead: AesGcm::new(&key),
            lifetime_secs: lifetime_secs.clamp(1, 604_800),
            tickets,
            cache: Mutex::new(HashMap::new()),
            capacity: 4096,
        }
    }

    pub(crate) fn seal(&self, s: &ServerSession) -> Vec<u8> {
        let mut nonce = [0u8; 12];
        rng::fill(&mut nonce);
        let mut out = self.key_name.to_vec();
        out.extend_from_slice(&nonce);
        out.extend(self.aead.seal(&nonce, &self.key_name, &s.encode()));
        out
    }

    pub(crate) fn open(&self, ticket: &[u8]) -> Option<ServerSession> {
        if ticket.len() < 16 + 12 + 16 || ticket[..16] != self.key_name {
            return None;
        }
        let nonce: [u8; 12] = ticket[16..28].try_into().ok()?;
        let plain = self.aead.open(&nonce, &self.key_name, &ticket[28..])?;
        let s = ServerSession::decode(&plain)?;
        self.fresh(&s).then_some(s)
    }

    fn fresh(&self, s: &ServerSession) -> bool {
        let now = now_ms() / 1000;
        now >= s.created.saturating_sub(60) && now < s.created + self.lifetime_secs as u64
    }

    pub(crate) fn cache_put(&self, id: &[u8], s: ServerSession) {
        if let Ok(mut m) = self.cache.lock() {
            if m.len() >= self.capacity {
                m.retain(|_, v| self.fresh(v));
                if m.len() >= self.capacity {
                    m.clear();
                }
            }
            m.insert(id.to_vec(), s);
        }
    }

    pub(crate) fn cache_get(&self, id: &[u8]) -> Option<ServerSession> {
        let m = self.cache.lock().ok()?;
        m.get(id).filter(|s| self.fresh(s)).cloned()
    }
}

/// TLS 1.3 NewSessionTicket body.
pub(crate) fn new_session_ticket13(lifetime: u32, age_add: u32, nonce: &[u8], ticket: &[u8], max_early_data: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&lifetime.to_be_bytes());
    body.extend_from_slice(&age_add.to_be_bytes());
    put_vec8(&mut body, nonce);
    put_vec16(&mut body, ticket);
    if max_early_data > 0 {
        // early_data extension with max_early_data_size
        put_u16(&mut body, 8);
        put_u16(&mut body, super::ext::EARLY_DATA);
        put_u16(&mut body, 4);
        body.extend_from_slice(&max_early_data.to_be_bytes());
    } else {
        put_u16(&mut body, 0);
    }
    body
}

/// Random 16-byte session origin identifier.
pub(crate) fn new_origin() -> Vec<u8> {
    rng::random_bytes(16)
}
