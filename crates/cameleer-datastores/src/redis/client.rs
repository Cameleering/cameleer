//! Redis client: connection setup (TLS, `HELLO 3` / `AUTH`, `SELECT`), commands,
//! pipelining, a small connection pool per node and pub/sub subscriptions, plus the
//! basics of Redis Sentinel (master discovery with `SENTINEL get-master-addr-by-name`,
//! re-resolved after connection failures) and Redis Cluster (key-slot routing with a slot
//! cache learnt from `MOVED` redirections, one-shot `ASK` redirections with `ASKING`).

use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, Write};
use std::sync::Mutex;
use std::time::Duration;

use cameleer_core::error::{CamelError, Result};
use cameleer_tls::camel_support::EndpointTls;
use cameleer_tls::net::NetStream;

use super::resp::{encode_command, read_resp, Resp};
use crate::kinds;

/// Connection settings.
#[derive(Clone, Debug)]
pub struct RedisConfig {
    pub host: String,
    pub port: u16,
    /// ACL user name (`AUTH user password`); `None` = `AUTH password`.
    pub username: Option<String>,
    pub password: Option<String>,
    /// Database index (`SELECT`).
    pub database: i64,
    /// Protocol version: 2 (RESP2) or 3 (RESP3 via `HELLO 3`).
    pub protocol: u8,
    pub tls: Option<EndpointTls>,
    pub connect_timeout: Duration,
    /// Socket read timeout for command replies (`None` = wait forever).
    pub read_timeout: Option<Duration>,
    /// Client name (`CLIENT SETNAME` / `HELLO ... SETNAME`).
    pub client_name: Option<String>,
    /// Sentinel nodes; with [`RedisConfig::master_name`] the master address is asked from them.
    pub sentinels: Vec<(String, u16)>,
    /// Name of the master monitored by the sentinels.
    pub master_name: Option<String>,
    /// Cluster mode: route commands by key slot and follow `MOVED` / `ASK`.
    pub cluster: bool,
}

impl RedisConfig {
    pub fn new(host: &str, port: u16) -> RedisConfig {
        RedisConfig {
            host: host.to_string(),
            port,
            username: None,
            password: None,
            database: 0,
            protocol: 2,
            tls: None,
            connect_timeout: Duration::from_secs(10),
            read_timeout: Some(Duration::from_secs(30)),
            client_name: None,
            sentinels: Vec::new(),
            master_name: None,
            cluster: false,
        }
    }

    /// The same settings for another node.
    pub fn for_node(&self, host: &str, port: u16) -> RedisConfig {
        RedisConfig { host: host.to_string(), port, ..self.clone() }
    }
}

fn io_err(e: std::io::Error, cfg: &RedisConfig) -> CamelError {
    if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) {
        return CamelError::new(kinds::REDIS_COMMAND_TIMEOUT, format!("Redis command timed out on {}:{}", cfg.host, cfg.port));
    }
    cameleer_tls::camel_support::io_to_camel(e, &format!("redis {}:{}", cfg.host, cfg.port))
}

/// Turns an error reply into a `RedisSystemException`.
pub fn reply_error(r: &Resp) -> Option<CamelError> {
    r.error_message().map(|m| CamelError::new(kinds::REDIS_SYSTEM, m))
}

/// Converts command arguments.
pub fn args<I, A>(items: I) -> Vec<Vec<u8>>
where
    I: IntoIterator<Item = A>,
    A: AsRef<[u8]>,
{
    items.into_iter().map(|a| a.as_ref().to_vec()).collect()
}

/// One connection to a Redis server.
pub struct RedisConnection {
    cfg: RedisConfig,
    stream: NetStream,
    reader: BufReader<NetStream>,
    broken: bool,
}

impl RedisConnection {
    /// Connects, authenticates and selects the database.
    pub fn connect(cfg: &RedisConfig) -> Result<RedisConnection> {
        let stream = match &cfg.tls {
            Some(t) => t.connect(&cfg.host, cfg.port, cfg.connect_timeout)?,
            None => {
                let tcp = cameleer_tls::net::connect_tcp(&cfg.host, cfg.port, cfg.connect_timeout).map_err(|e| {
                    CamelError::new(kinds::REDIS_CONNECTION_FAILURE, format!("Unable to connect to Redis {}:{}: {}", cfg.host, cfg.port, e))
                })?;
                NetStream::plain(tcp)
            }
        };
        let _ = stream.set_nodelay(true);
        stream.set_read_timeout(cfg.read_timeout).map_err(|e| io_err(e, cfg))?;
        let reader = BufReader::new(stream.try_clone().map_err(|e| io_err(e, cfg))?);
        let mut c = RedisConnection { cfg: cfg.clone(), stream, reader, broken: false };
        let auth_fail = |e: CamelError| CamelError::new(kinds::REDIS_CONNECTION_FAILURE, format!("Redis authentication failed: {}", e.message));
        if cfg.protocol >= 3 {
            let mut hello = args(["HELLO", "3"]);
            if let Some(p) = &cfg.password {
                hello.extend(args(["AUTH", cfg.username.as_deref().unwrap_or("default"), p.as_str()]));
            }
            if let Some(n) = &cfg.client_name {
                hello.extend(args(["SETNAME", n.as_str()]));
            }
            c.command(&hello).map_err(auth_fail)?;
        } else {
            if let Some(p) = &cfg.password {
                let a = match &cfg.username {
                    Some(u) => args(["AUTH", u.as_str(), p.as_str()]),
                    None => args(["AUTH", p.as_str()]),
                };
                c.command(&a).map_err(auth_fail)?;
            }
            if let Some(n) = &cfg.client_name {
                c.command(&args(["CLIENT", "SETNAME", n.as_str()]))?;
            }
        }
        if cfg.database != 0 {
            c.command(&args(["SELECT".to_string(), cfg.database.to_string()]))?;
        }
        Ok(c)
    }

    /// Whether an I/O error happened (the connection must not be reused).
    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// Sets the read timeout.
    pub fn set_read_timeout(&self, d: Option<Duration>) -> Result<()> {
        self.stream.set_read_timeout(d).map_err(|e| io_err(e, &self.cfg))
    }

    /// Sends commands without reading replies.
    pub fn send(&mut self, cmds: &[Vec<Vec<u8>>]) -> Result<()> {
        let mut out = Vec::new();
        for c in cmds {
            encode_command(c, &mut out);
        }
        let r = self.stream.write_all(&out).and_then(|_| self.stream.flush());
        r.map_err(|e| {
            self.broken = true;
            io_err(e, &self.cfg)
        })
    }

    /// Reads one reply (push messages included).
    pub fn read(&mut self) -> Result<Resp> {
        read_resp(&mut self.reader).map_err(|e| {
            // also on timeouts: a partially read reply cannot be resynchronized
            self.broken = true;
            io_err(e, &self.cfg)
        })
    }

    fn read_reply(&mut self) -> Result<Resp> {
        loop {
            let r = self.read()?;
            // RESP3 push messages (e.g. client tracking) are out of band
            if !matches!(r, Resp::Push(_)) {
                return Ok(r);
            }
        }
    }

    /// Runs a command; an error reply becomes `RedisSystemException`.
    pub fn command(&mut self, args: &[Vec<u8>]) -> Result<Resp> {
        self.send(&[args.to_vec()])?;
        let r = self.read_reply()?;
        match reply_error(&r) {
            Some(e) => Err(e),
            None => Ok(r),
        }
    }

    /// Sends all commands at once and reads all replies (error replies are returned as values).
    pub fn pipeline(&mut self, cmds: &[Vec<Vec<u8>>]) -> Result<Vec<Resp>> {
        self.send(cmds)?;
        (0..cmds.len()).map(|_| self.read_reply()).collect()
    }
}

/// A message received on a subscription.
#[derive(Clone, Debug, PartialEq)]
pub struct PubSubMessage {
    pub channel: String,
    /// The matching pattern for `PSUBSCRIBE` deliveries.
    pub pattern: Option<String>,
    pub payload: Vec<u8>,
}

/// A connection in subscribed state.
pub struct Subscription {
    conn: RedisConnection,
    pending: usize,
}

impl Subscription {
    /// Waits up to `timeout` for the next message (`Ok(None)` on timeout).
    pub fn next_message(&mut self, timeout: Duration) -> Result<Option<PubSubMessage>> {
        self.conn.stream.set_read_timeout(Some(timeout.max(Duration::from_millis(1)))).map_err(|e| io_err(e, &self.conn.cfg))?;
        loop {
            // wait for data without consuming a partial reply on timeout
            match std::io::BufRead::fill_buf(&mut self.conn.reader) {
                Ok([]) => return Err(CamelError::io("Redis subscription connection closed")),
                Ok(_) => {}
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Ok(None),
                Err(e) => return Err(io_err(e, &self.conn.cfg)),
            }
            self.conn.stream.set_read_timeout(Some(Duration::from_secs(30))).map_err(|e| io_err(e, &self.conn.cfg))?;
            let r = self.conn.read()?;
            self.conn.stream.set_read_timeout(Some(timeout.max(Duration::from_millis(1)))).map_err(|e| io_err(e, &self.conn.cfg))?;
            let Some(items) = r.items() else {
                if let Some(e) = reply_error(&r) {
                    return Err(e);
                }
                continue;
            };
            let kind = items.first().and_then(|k| k.as_text()).unwrap_or_default().to_ascii_lowercase();
            match kind.as_str() {
                "message" if items.len() >= 3 => {
                    return Ok(Some(PubSubMessage {
                        channel: items[1].as_text().unwrap_or_default(),
                        pattern: None,
                        payload: items[2].as_bytes().unwrap_or_default().to_vec(),
                    }))
                }
                "pmessage" if items.len() >= 4 => {
                    return Ok(Some(PubSubMessage {
                        channel: items[2].as_text().unwrap_or_default(),
                        pattern: items[1].as_text(),
                        payload: items[3].as_bytes().unwrap_or_default().to_vec(),
                    }))
                }
                _ => {
                    self.pending = self.pending.saturating_sub(1);
                }
            }
        }
    }

    /// Unsubscribes and closes the connection.
    pub fn close(self) {
        let _ = self.conn.stream.shutdown(std::net::Shutdown::Both);
    }

    /// A handle that can close the socket from another thread (to interrupt a wait).
    pub fn closer(&self) -> Result<NetStream> {
        self.conn.stream.try_clone().map_err(|e| io_err(e, &self.conn.cfg))
    }
}

type Node = (String, u16);

/// A pooled Redis client (cheap to share behind an `Arc`).
pub struct RedisClient {
    cfg: RedisConfig,
    pool: Mutex<HashMap<Node, Vec<RedisConnection>>>,
    /// Master address resolved through the sentinels.
    master: Mutex<Option<Node>>,
    /// Cluster slot cache (slot → node), learnt from `MOVED` redirections.
    slots: Mutex<BTreeMap<u16, Node>>,
}

impl RedisClient {
    pub fn new(cfg: RedisConfig) -> RedisClient {
        RedisClient { cfg, pool: Mutex::new(HashMap::new()), master: Mutex::new(None), slots: Mutex::new(BTreeMap::new()) }
    }

    pub fn config(&self) -> &RedisConfig {
        &self.cfg
    }

    /// Asks the sentinels for the address of the master.
    pub fn resolve_master(&self) -> Result<Node> {
        let name = self.cfg.master_name.clone().unwrap_or_else(|| "mymaster".into());
        let mut last = None;
        for (h, p) in &self.cfg.sentinels {
            let mut scfg = RedisConfig::new(h, *p);
            scfg.connect_timeout = self.cfg.connect_timeout;
            scfg.read_timeout = self.cfg.read_timeout;
            scfg.tls = self.cfg.tls.clone();
            let r = RedisConnection::connect(&scfg).and_then(|mut c| c.command(&args(["SENTINEL", "get-master-addr-by-name", name.as_str()])));
            match r {
                Ok(reply) => {
                    let items = reply.items().unwrap_or_default();
                    if let (Some(host), Some(port)) = (items.first().and_then(|x| x.as_text()), items.get(1).and_then(|x| x.as_text()).and_then(|p| p.parse::<u16>().ok())) {
                        let node = (host, port);
                        *self.master.lock().unwrap() = Some(node.clone());
                        return Ok(node);
                    }
                    last = Some(CamelError::new(kinds::REDIS_CONNECTION_FAILURE, format!("Sentinel {}:{} does not know master '{}'", h, p, name)));
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| CamelError::new(kinds::REDIS_CONNECTION_FAILURE, "No sentinels configured")))
    }

    /// The default node: the sentinel-resolved master, or the configured host.
    pub fn primary(&self) -> Result<Node> {
        if self.cfg.master_name.is_some() && !self.cfg.sentinels.is_empty() {
            if let Some(m) = self.master.lock().unwrap().clone() {
                return Ok(m);
            }
            return self.resolve_master();
        }
        Ok((self.cfg.host.clone(), self.cfg.port))
    }

    /// Runs `f` with a pooled connection to `node` (broken connections are dropped).
    pub fn with_node<T>(&self, node: &Node, f: impl FnOnce(&mut RedisConnection) -> Result<T>) -> Result<T> {
        let pooled = self.pool.lock().unwrap().get_mut(node).and_then(|v| v.pop());
        let mut conn = match pooled {
            Some(c) => c,
            None => match RedisConnection::connect(&self.cfg.for_node(&node.0, node.1)) {
                Ok(c) => c,
                Err(e) => {
                    // the master may have failed over: ask the sentinels again next time
                    *self.master.lock().unwrap() = None;
                    return Err(e);
                }
            },
        };
        let r = f(&mut conn);
        if !conn.is_broken() {
            let mut p = self.pool.lock().unwrap();
            let v = p.entry(node.clone()).or_default();
            if v.len() < 8 {
                v.push(conn);
            }
        } else {
            *self.master.lock().unwrap() = None;
        }
        r
    }

    /// Runs `f` with a pooled connection to the default node.
    pub fn with_connection<T>(&self, f: impl FnOnce(&mut RedisConnection) -> Result<T>) -> Result<T> {
        let node = self.primary()?;
        self.with_node(&node, f)
    }

    /// Runs one command (routed by key slot and following redirections in cluster mode).
    pub fn command(&self, args: &[Vec<u8>]) -> Result<Resp> {
        if !self.cfg.cluster {
            return self.with_connection(|c| c.command(args));
        }
        let slot = args.get(1).map(|k| super::cluster::key_slot(k));
        let mut node = match slot.and_then(|s| self.slots.lock().unwrap().get(&s).cloned()) {
            Some(n) => n,
            None => self.primary()?,
        };
        let mut asking = false;
        for _ in 0..5 {
            let r = if asking {
                self.with_node(&node, |c| {
                    let mut replies = c.pipeline(&[vec![b"ASKING".to_vec()], args.to_vec()])?;
                    let r = replies.pop().unwrap_or(Resp::Null);
                    match reply_error(&r) {
                        Some(e) => Err(e),
                        None => Ok(r),
                    }
                })
            } else {
                self.with_node(&node, |c| c.command(args))
            };
            match r {
                Err(e) if e.kind == kinds::REDIS_SYSTEM => match super::cluster::parse_redirect(&e.message) {
                    Some(super::cluster::Redirect::Moved(s, h, p)) => {
                        node = (h, p);
                        self.slots.lock().unwrap().insert(s, node.clone());
                        asking = false;
                    }
                    Some(super::cluster::Redirect::Ask(_, h, p)) => {
                        node = (h, p);
                        asking = true;
                    }
                    None => return Err(e),
                },
                other => return other,
            }
        }
        Err(CamelError::new(kinds::REDIS_SYSTEM, "Too many cluster redirections"))
    }

    /// Runs one command given as text arguments.
    pub fn cmd(&self, parts: &[&str]) -> Result<Resp> {
        self.command(&args(parts.iter()))
    }

    /// Pipelines commands on one connection.
    pub fn pipeline(&self, cmds: &[Vec<Vec<u8>>]) -> Result<Vec<Resp>> {
        self.with_connection(|c| c.pipeline(cmds))
    }

    /// `PUBLISH channel message`, returns the number of receivers.
    pub fn publish(&self, channel: &str, payload: &[u8]) -> Result<i64> {
        Ok(self.command(&[b"PUBLISH".to_vec(), channel.as_bytes().to_vec(), payload.to_vec()])?.as_int().unwrap_or(0))
    }

    /// Opens a dedicated connection subscribed to `channels` (SUBSCRIBE) and `patterns` (PSUBSCRIBE).
    /// Returns once the server confirmed the subscriptions.
    pub fn subscribe(&self, channels: &[String], patterns: &[String]) -> Result<Subscription> {
        let node = self.primary()?;
        let mut conn = RedisConnection::connect(&self.cfg.for_node(&node.0, node.1))?;
        let mut cmds = Vec::new();
        if !channels.is_empty() {
            let mut c = vec![b"SUBSCRIBE".to_vec()];
            c.extend(channels.iter().map(|s| s.as_bytes().to_vec()));
            cmds.push(c);
        }
        if !patterns.is_empty() {
            let mut c = vec![b"PSUBSCRIBE".to_vec()];
            c.extend(patterns.iter().map(|s| s.as_bytes().to_vec()));
            cmds.push(c);
        }
        conn.send(&cmds)?;
        let expected = channels.len() + patterns.len();
        for _ in 0..expected {
            let r = conn.read()?;
            if let Some(e) = reply_error(&r) {
                return Err(e);
            }
        }
        Ok(Subscription { conn, pending: 0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_helper() {
        assert_eq!(args(["a", "bc"]), vec![b"a".to_vec(), b"bc".to_vec()]);
    }
}
