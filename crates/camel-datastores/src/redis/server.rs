//! Embedded Redis server for tests: RESP2/RESP3 (`HELLO`), `AUTH` (password or ACL
//! users), `SELECT`, key expiry, strings, hashes, lists (incl. blocking pops), sets,
//! sorted sets, geo commands (`GEOADD`, `GEODIST`, `GEOHASH`, `GEOPOS`, `GEORADIUS`,
//! `GEORADIUSBYMEMBER`), `MULTI`/`EXEC`, pub/sub (`SUBSCRIBE`, `PSUBSCRIBE`, `PUBLISH`) and
//! TLS. It can also play a cluster node ([`RedisServer::set_cluster`]: `MOVED` redirections
//! for keys outside its slot range, `CLUSTER SLOTS` / `KEYSLOT` / `INFO`) or a Sentinel
//! ([`RedisServer::set_sentinel_master`]: `SENTINEL get-master-addr-by-name`).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{self, BufReader, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use camel_tls::net::NetStream;
use camel_tls::TlsServerConfig;

use super::resp::{format_double, read_resp, Resp};
use crate::util::{next_id, now_millis, TcpServer};

/// Server settings.
#[derive(Clone, Default)]
pub struct RedisServerConfig {
    /// `requirepass` (user `default`).
    pub password: Option<String>,
    /// Extra ACL users (`AUTH user password`).
    pub users: BTreeMap<String, String>,
    pub tls: Option<Arc<TlsServerConfig>>,
}

#[derive(Clone, Debug)]
enum RVal {
    Str(Vec<u8>),
    List(VecDeque<Vec<u8>>),
    Hash(BTreeMap<Vec<u8>, Vec<u8>>),
    Set(BTreeSet<Vec<u8>>),
    ZSet(BTreeMap<Vec<u8>, f64>),
}

#[derive(Clone, Debug)]
struct Entry {
    val: RVal,
    expire_at: Option<Instant>,
}

type Db = HashMap<Vec<u8>, Entry>;

struct Subscriber {
    id: u64,
    writer: Arc<Mutex<NetStream>>,
    resp3: bool,
    channels: Mutex<BTreeSet<Vec<u8>>>,
    patterns: Mutex<BTreeSet<Vec<u8>>>,
}

/// Cluster role: own slot range and the other nodes `(first, last, "host:port")`.
#[derive(Clone, Default)]
struct ClusterState {
    own: (u16, u16),
    me: String,
    peers: Vec<(u16, u16, String)>,
}

struct Shared {
    cfg: RedisServerConfig,
    cluster: Mutex<Option<ClusterState>>,
    sentinel: Mutex<BTreeMap<String, String>>,
    dbs: Mutex<Vec<Db>>,
    cv: Condvar,
    subs: Mutex<Vec<Arc<Subscriber>>>,
    commands: AtomicU64,
}

/// A running embedded Redis server (stopped on drop).
pub struct RedisServer {
    shared: Arc<Shared>,
    tcp: TcpServer,
}

impl RedisServer {
    /// Starts a server without authentication on an ephemeral port.
    pub fn start() -> io::Result<RedisServer> {
        Self::start_with(RedisServerConfig::default())
    }

    /// Starts a server with the given settings.
    pub fn start_with(cfg: RedisServerConfig) -> io::Result<RedisServer> {
        let tls = cfg.tls.clone();
        let shared = Arc::new(Shared { cfg, cluster: Mutex::new(None), sentinel: Mutex::new(BTreeMap::new()), dbs: Mutex::new((0..16).map(|_| Db::new()).collect()), cv: Condvar::new(), subs: Mutex::new(Vec::new()), commands: AtomicU64::new(0) });
        let sh = shared.clone();
        let tcp = TcpServer::start("redis-server", tls, move |s| {
            let _ = serve(&sh, s);
        })?;
        Ok(RedisServer { shared, tcp })
    }

    pub fn port(&self) -> u16 {
        self.tcp.port()
    }

    /// `host:port`.
    pub fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port())
    }

    /// Number of commands processed.
    pub fn command_count(&self) -> u64 {
        self.shared.commands.load(Ordering::SeqCst)
    }

    /// Number of open client connections.
    pub fn connection_count(&self) -> usize {
        self.tcp.connection_count()
    }

    /// Number of active pub/sub subscriptions (channels + patterns).
    pub fn subscription_count(&self) -> usize {
        self.shared.subs.lock().unwrap().iter().map(|s| s.channels.lock().unwrap().len() + s.patterns.lock().unwrap().len()).sum()
    }

    /// Runs a command directly (database 0, RESP2, authenticated).
    pub fn exec(&self, args: &[&str]) -> Resp {
        let a: Vec<Vec<u8>> = args.iter().map(|s| s.as_bytes().to_vec()).collect();
        let mut st = ConnState { db: 0, resp3: false, authed: true, multi: None, sub: None, asking: false };
        let r = execute(&self.shared, &mut st, &a, None);
        self.shared.cv.notify_all();
        r
    }

    /// Waits until `f` is true (polling), returns the final result.
    pub fn wait_until(&self, timeout: Duration, f: impl Fn(&RedisServer) -> bool) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if f(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        f(self)
    }

    pub fn stop(&self) {
        self.tcp.stop();
    }

    /// Makes this server a cluster node owning slots `first..=last`; keys of other slots are
    /// answered with `MOVED <slot> <host:port>` of the peer owning them.
    pub fn set_cluster(&self, first: u16, last: u16, peers: Vec<(u16, u16, String)>) {
        *self.shared.cluster.lock().unwrap() = Some(ClusterState { own: (first, last), me: self.address(), peers });
    }

    /// Makes this server a Sentinel monitoring master `name` at `host:port`.
    pub fn set_sentinel_master(&self, name: &str, address: &str) {
        self.shared.sentinel.lock().unwrap().insert(name.to_string(), address.to_string());
    }
}

/// Commands whose first argument is not a key (no cluster redirection).
fn keyless(name: &str) -> bool {
    matches!(name, "DBSIZE" | "FLUSHDB" | "KEYS" | "RANDOMKEY" | "MOVE" | "SCAN" | "TIME" | "LASTSAVE" | "SAVE")
}

struct ConnState {
    db: usize,
    resp3: bool,
    authed: bool,
    multi: Option<Vec<Vec<Vec<u8>>>>,
    sub: Option<Arc<Subscriber>>,
    /// `ASKING` was sent: the next command may touch a migrating slot.
    asking: bool,
}

fn serve(sh: &Arc<Shared>, stream: NetStream) -> io::Result<()> {
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let mut reader = BufReader::new(stream);
    let mut st = ConnState { db: 0, resp3: false, authed: sh.cfg.password.is_none() && sh.cfg.users.is_empty(), multi: None, sub: None, asking: false };
    let result = loop {
        let req = match read_resp(&mut reader) {
            Ok(r) => r,
            Err(e) => break Err(e),
        };
        let args: Vec<Vec<u8>> = match req.items() {
            Some(items) => items.iter().map(|i| i.as_bytes().map(|b| b.to_vec()).unwrap_or_else(|| i.to_value().to_string().into_bytes())).collect(),
            None => continue,
        };
        if args.is_empty() {
            continue;
        }
        sh.commands.fetch_add(1, Ordering::SeqCst);
        let name = upper(&args[0]);
        let reply = execute(sh, &mut st, &args, Some(&writer));
        sh.cv.notify_all();
        if let Some(r) = reply_opt(reply) {
            let mut w = writer.lock().unwrap();
            if w.write_all(&r.to_bytes()).and_then(|_| w.flush()).is_err() {
                break Ok(());
            }
        }
        if name == "QUIT" {
            break Ok(());
        }
    };
    if let Some(s) = st.sub.take() {
        sh.subs.lock().unwrap().retain(|x| x.id != s.id);
    }
    result
}

/// Marker for "no reply" (subscription confirmations are written directly).
fn reply_opt(r: Resp) -> Option<Resp> {
    match &r {
        Resp::Push(items) if items.is_empty() => None,
        _ => Some(r),
    }
}

fn no_reply() -> Resp {
    Resp::Push(Vec::new())
}

fn upper(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_ascii_uppercase()
}

fn wrongtype() -> Resp {
    Resp::err("WRONGTYPE Operation against a key holding the wrong kind of value")
}

fn syntax() -> Resp {
    Resp::err("ERR syntax error")
}

fn not_int() -> Resp {
    Resp::err("ERR value is not an integer or out of range")
}

fn wrong_args(name: &str) -> Resp {
    Resp::err(format!("ERR wrong number of arguments for '{}' command", name.to_ascii_lowercase()))
}

fn int_arg(b: &[u8]) -> Option<i64> {
    std::str::from_utf8(b).ok()?.parse().ok()
}

fn float_arg(b: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(b).ok()?.to_ascii_lowercase();
    match s.as_str() {
        "+inf" | "inf" => Some(f64::INFINITY),
        "-inf" => Some(f64::NEG_INFINITY),
        _ => s.parse().ok(),
    }
}

/// Score bound: (value, exclusive).
fn score_bound(b: &[u8]) -> Option<(f64, bool)> {
    match b.strip_prefix(b"(") {
        Some(rest) => float_arg(rest).map(|f| (f, true)),
        None => float_arg(b).map(|f| (f, false)),
    }
}

fn in_range(s: f64, min: (f64, bool), max: (f64, bool)) -> bool {
    (if min.1 { s > min.0 } else { s >= min.0 }) && (if max.1 { s < max.0 } else { s <= max.0 })
}

/// Redis glob matching (`*`, `?`, `[abc]`, `[^a]`, `\x`).
pub fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    fn m(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') => (0..=t.len()).any(|i| m(&p[1..], &t[i..])),
            Some(b'?') => !t.is_empty() && m(&p[1..], &t[1..]),
            Some(b'[') => {
                let Some(close) = p.iter().skip(1).position(|c| *c == b']').map(|i| i + 1) else {
                    return !t.is_empty() && t[0] == b'[' && m(&p[1..], &t[1..]);
                };
                if t.is_empty() {
                    return false;
                }
                let mut set = &p[1..close];
                let neg = set.first() == Some(&b'^');
                if neg {
                    set = &set[1..];
                }
                let mut hit = false;
                let mut i = 0;
                while i < set.len() {
                    if i + 2 < set.len() && set[i + 1] == b'-' {
                        if set[i] <= t[0] && t[0] <= set[i + 2] {
                            hit = true;
                        }
                        i += 3;
                    } else {
                        if set[i] == t[0] {
                            hit = true;
                        }
                        i += 1;
                    }
                }
                hit != neg && m(&p[close + 1..], &t[1..])
            }
            Some(b'\\') if p.len() > 1 => !t.is_empty() && t[0] == p[1] && m(&p[2..], &t[1..]),
            Some(c) => !t.is_empty() && t[0] == *c && m(&p[1..], &t[1..]),
        }
    }
    m(pattern, text)
}

fn purge(db: &mut Db, key: &[u8]) {
    if let Some(e) = db.get(key) {
        if e.expire_at.is_some_and(|t| t <= Instant::now()) {
            db.remove(key);
        }
    }
}

fn live<'a>(db: &'a mut Db, key: &[u8]) -> Option<&'a mut Entry> {
    purge(db, key);
    db.get_mut(key)
}

fn null(st: &ConnState) -> Resp {
    if st.resp3 { Resp::Null } else { Resp::Bulk(None) }
}

fn bulk_list<I: IntoIterator<Item = Vec<u8>>>(items: I) -> Resp {
    Resp::array(items.into_iter().map(Resp::bulk).collect())
}

fn set_reply(st: &ConnState, items: Vec<Vec<u8>>) -> Resp {
    let v: Vec<Resp> = items.into_iter().map(Resp::bulk).collect();
    if st.resp3 { Resp::Set(v) } else { Resp::array(v) }
}

fn double_reply(st: &ConnState, d: f64) -> Resp {
    if st.resp3 { Resp::Double(d) } else { Resp::bulk(format_double(d)) }
}

fn norm_range(start: i64, end: i64, len: usize) -> Option<(usize, usize)> {
    let len = len as i64;
    let mut s = if start < 0 { len + start } else { start };
    let mut e = if end < 0 { len + end } else { end };
    s = s.max(0);
    e = e.min(len - 1);
    if s > e || s >= len {
        None
    } else {
        Some((s as usize, e as usize))
    }
}

fn zsorted(z: &BTreeMap<Vec<u8>, f64>) -> Vec<(Vec<u8>, f64)> {
    let mut v: Vec<(Vec<u8>, f64)> = z.iter().map(|(k, s)| (k.clone(), *s)).collect();
    v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0)));
    v
}

fn zitems(items: Vec<(Vec<u8>, f64)>, with_scores: bool, st: &ConnState) -> Resp {
    let mut out = Vec::new();
    for (m, s) in items {
        if with_scores && st.resp3 {
            out.push(Resp::array(vec![Resp::bulk(m), Resp::Double(s)]));
        } else {
            out.push(Resp::bulk(m));
            if with_scores {
                out.push(Resp::bulk(format_double(s)));
            }
        }
    }
    Resp::array(out)
}

macro_rules! get_typed {
    ($db:expr, $key:expr, $variant:ident) => {
        match live($db, $key) {
            None => None,
            Some(Entry { val: RVal::$variant(x), .. }) => Some(x),
            Some(_) => return wrongtype(),
        }
    };
}

macro_rules! get_or_create {
    ($db:expr, $key:expr, $variant:ident, $default:expr) => {{
        purge($db, $key);
        let e = $db.entry($key.to_vec()).or_insert_with(|| Entry { val: RVal::$variant($default), expire_at: None });
        match &mut e.val {
            RVal::$variant(x) => x,
            _ => return wrongtype(),
        }
    }};
}

fn cleanup_empty(db: &mut Db, key: &[u8]) {
    let empty = match db.get(key).map(|e| &e.val) {
        Some(RVal::List(l)) => l.is_empty(),
        Some(RVal::Hash(h)) => h.is_empty(),
        Some(RVal::Set(s)) => s.is_empty(),
        Some(RVal::ZSet(z)) => z.is_empty(),
        _ => false,
    };
    if empty {
        db.remove(key);
    }
}

fn publish(sh: &Shared, channel: &[u8], payload: &[u8]) -> i64 {
    let subs: Vec<Arc<Subscriber>> = sh.subs.lock().unwrap().clone();
    let mut n = 0;
    for s in subs {
        let mut msgs = Vec::new();
        if s.channels.lock().unwrap().contains(channel) {
            msgs.push(vec![Resp::bulk("message"), Resp::bulk(channel.to_vec()), Resp::bulk(payload.to_vec())]);
        }
        for p in s.patterns.lock().unwrap().iter() {
            if glob_match(p, channel) {
                msgs.push(vec![Resp::bulk("pmessage"), Resp::bulk(p.clone()), Resp::bulk(channel.to_vec()), Resp::bulk(payload.to_vec())]);
            }
        }
        for m in msgs {
            n += 1;
            let r = if s.resp3 { Resp::Push(m) } else { Resp::array(m) };
            let mut w = s.writer.lock().unwrap();
            let _ = w.write_all(&r.to_bytes()).and_then(|_| w.flush());
        }
    }
    n
}

fn subscribe_cmd(sh: &Shared, st: &mut ConnState, name: &str, targets: &[Vec<u8>], writer: Option<&Arc<Mutex<NetStream>>>) -> Resp {
    let Some(writer) = writer else { return Resp::err("ERR pub/sub needs a connection") };
    let sub = match &st.sub {
        Some(s) => s.clone(),
        None => {
            let s = Arc::new(Subscriber { id: next_id(), writer: writer.clone(), resp3: st.resp3, channels: Mutex::new(BTreeSet::new()), patterns: Mutex::new(BTreeSet::new()) });
            sh.subs.lock().unwrap().push(s.clone());
            st.sub = Some(s.clone());
            s
        }
    };
    let pattern = name.starts_with('P');
    let unsub = name.contains("UNSUB");
    let kind = name.to_ascii_lowercase();
    let targets: Vec<Vec<u8>> = if unsub && targets.is_empty() {
        if pattern { sub.patterns.lock().unwrap().iter().cloned().collect() } else { sub.channels.lock().unwrap().iter().cloned().collect() }
    } else {
        targets.to_vec()
    };
    let mut replies = Vec::new();
    for t in &targets {
        {
            let set = if pattern { &sub.patterns } else { &sub.channels };
            let mut set = set.lock().unwrap();
            if unsub {
                set.remove(t);
            } else {
                set.insert(t.clone());
            }
        }
        let count = sub.channels.lock().unwrap().len() + sub.patterns.lock().unwrap().len();
        replies.push(vec![Resp::bulk(kind.clone()), Resp::bulk(t.clone()), Resp::Int(count as i64)]);
    }
    if targets.is_empty() {
        replies.push(vec![Resp::bulk(kind.clone()), null(st), Resp::Int(0)]);
    }
    let mut w = writer.lock().unwrap();
    for r in replies {
        let r = if st.resp3 { Resp::Push(r) } else { Resp::array(r) };
        let _ = w.write_all(&r.to_bytes());
    }
    let _ = w.flush();
    drop(w);
    if sub.channels.lock().unwrap().is_empty() && sub.patterns.lock().unwrap().is_empty() {
        sh.subs.lock().unwrap().retain(|x| x.id != sub.id);
        st.sub = None;
    }
    no_reply()
}

fn check_auth(sh: &Shared, user: &str, pass: &str) -> bool {
    if user == "default" {
        if let Some(p) = &sh.cfg.password {
            return p == pass;
        }
        return sh.cfg.users.get("default").is_some_and(|p| p == pass) || sh.cfg.users.is_empty();
    }
    sh.cfg.users.get(user).is_some_and(|p| p == pass)
}

fn execute(sh: &Shared, st: &mut ConnState, args: &[Vec<u8>], writer: Option<&Arc<Mutex<NetStream>>>) -> Resp {
    let name = upper(&args[0]);
    let a = &args[1..];
    // connection-level commands
    match name.as_str() {
        "AUTH" => {
            let (user, pass) = match a.len() {
                1 => ("default".to_string(), String::from_utf8_lossy(&a[0]).into_owned()),
                2 => (String::from_utf8_lossy(&a[0]).into_owned(), String::from_utf8_lossy(&a[1]).into_owned()),
                _ => return wrong_args(&name),
            };
            if sh.cfg.password.is_none() && sh.cfg.users.is_empty() {
                return Resp::err("ERR AUTH <password> called without any password configured for the default user. Are you sure your configuration is correct?");
            }
            return if check_auth(sh, &user, &pass) {
                st.authed = true;
                Resp::ok()
            } else {
                Resp::err("WRONGPASS invalid username-password pair or user is disabled.")
            };
        }
        "HELLO" => {
            let mut i = 0;
            let mut proto = if st.resp3 { 3 } else { 2 };
            if let Some(v) = a.first() {
                match int_arg(v) {
                    Some(p @ (2 | 3)) => proto = p,
                    _ => return Resp::err("NOPROTO unsupported protocol version"),
                }
                i = 1;
            }
            while i < a.len() {
                match upper(&a[i]).as_str() {
                    "AUTH" if i + 2 < a.len() => {
                        let (u, p) = (String::from_utf8_lossy(&a[i + 1]).into_owned(), String::from_utf8_lossy(&a[i + 2]).into_owned());
                        if !check_auth(sh, &u, &p) {
                            return Resp::err("WRONGPASS invalid username-password pair or user is disabled.");
                        }
                        st.authed = true;
                        i += 3;
                    }
                    "SETNAME" if i + 1 < a.len() => i += 2,
                    _ => return syntax(),
                }
            }
            if !st.authed {
                return Resp::err("NOAUTH HELLO must be called with the client already authenticated, otherwise the HELLO <proto> AUTH <user> <pass> option can be used to authenticate the client and select the RESP protocol version at the same time");
            }
            st.resp3 = proto == 3;
            let info = vec![
                (Resp::bulk("server"), Resp::bulk("redis")),
                (Resp::bulk("version"), Resp::bulk("7.2.0")),
                (Resp::bulk("proto"), Resp::Int(proto)),
                (Resp::bulk("id"), Resp::Int(next_id() as i64)),
                (Resp::bulk("mode"), Resp::bulk("standalone")),
                (Resp::bulk("role"), Resp::bulk("master")),
                (Resp::bulk("modules"), Resp::array(vec![])),
            ];
            return if st.resp3 { Resp::Map(info) } else { Resp::array(info.into_iter().flat_map(|(k, v)| [k, v]).collect()) };
        }
        "QUIT" => return Resp::ok(),
        "ASKING" => {
            st.asking = true;
            return Resp::ok();
        }
        _ => {}
    }
    if !st.authed {
        return Resp::err("NOAUTH Authentication required.");
    }
    if st.sub.is_some() && !st.resp3 && !matches!(name.as_str(), "SUBSCRIBE" | "PSUBSCRIBE" | "UNSUBSCRIBE" | "PUNSUBSCRIBE" | "PING" | "RESET") {
        return Resp::err(format!("ERR Can't execute '{}': only (P|S)SUBSCRIBE / (P|S)UNSUBSCRIBE / PING / QUIT / RESET are allowed in this context", name.to_ascii_lowercase()));
    }
    // transactions
    if let Some(q) = st.multi.as_mut() {
        match name.as_str() {
            "EXEC" => {
                let q = st.multi.take().unwrap_or_default();
                let replies = q.iter().map(|c| execute(sh, st, c, writer)).collect();
                return Resp::array(replies);
            }
            "DISCARD" => {
                st.multi = None;
                return Resp::ok();
            }
            "MULTI" => return Resp::err("ERR MULTI calls can not be nested"),
            "WATCH" => return Resp::err("ERR WATCH inside MULTI is not allowed"),
            _ => {
                q.push(args.to_vec());
                return Resp::Simple("QUEUED".into());
            }
        }
    }
    match name.as_str() {
        "MULTI" => {
            st.multi = Some(Vec::new());
            return Resp::ok();
        }
        "EXEC" => return Resp::err("ERR EXEC without MULTI"),
        "DISCARD" => return Resp::err("ERR DISCARD without MULTI"),
        "WATCH" | "UNWATCH" => return Resp::ok(),
        "SUBSCRIBE" | "PSUBSCRIBE" | "UNSUBSCRIBE" | "PUNSUBSCRIBE" => return subscribe_cmd(sh, st, &name, a, writer),
        "PUBLISH" => {
            if a.len() != 2 {
                return wrong_args(&name);
            }
            return Resp::Int(publish(sh, &a[0], &a[1]));
        }
        "PING" => {
            if st.sub.is_some() && !st.resp3 {
                return Resp::array(vec![Resp::bulk("pong"), Resp::bulk(a.first().cloned().unwrap_or_default())]);
            }
            return match a.first() {
                Some(m) => Resp::bulk(m.clone()),
                None => Resp::Simple("PONG".into()),
            };
        }
        "ECHO" => return a.first().map(|m| Resp::bulk(m.clone())).unwrap_or_else(|| wrong_args(&name)),
        "SELECT" => {
            return match a.first().and_then(|d| int_arg(d)) {
                Some(d) if (0..16).contains(&d) => {
                    st.db = d as usize;
                    Resp::ok()
                }
                _ => Resp::err("ERR DB index is out of range"),
            }
        }
        "CLIENT" | "CONFIG" | "COMMAND" | "INFO" | "RESET" => {
            let sub = a.first().map(|s| upper(s)).unwrap_or_default();
            return match (name.as_str(), sub.as_str()) {
                ("CLIENT", "GETNAME") => null(st),
                ("CLIENT", "ID") => Resp::Int(1),
                ("CONFIG", "GET") | ("COMMAND", _) => Resp::array(vec![]),
                ("INFO", _) => Resp::bulk("# Server\r\nredis_version:7.2.0\r\nredis_mode:standalone\r\n"),
                ("RESET", _) => {
                    st.multi = None;
                    Resp::Simple("RESET".into())
                }
                _ => Resp::ok(),
            };
        }
        "FLUSHALL" => {
            for d in sh.dbs.lock().unwrap().iter_mut() {
                d.clear();
            }
            return Resp::ok();
        }
        "CLUSTER" => {
            let Some(c) = sh.cluster.lock().unwrap().clone() else { return Resp::err("ERR This instance has cluster support disabled") };
            let sub = a.first().map(|s| upper(s)).unwrap_or_default();
            let node = |addr: &str| {
                let (h, p) = addr.rsplit_once(':').unwrap_or((addr, "0"));
                Resp::array(vec![Resp::bulk(h), Resp::Int(p.parse().unwrap_or(0))])
            };
            return match sub.as_str() {
                "KEYSLOT" if a.len() == 2 => Resp::Int(super::cluster::key_slot(&a[1]) as i64),
                "SLOTS" => {
                    let mut v = vec![Resp::array(vec![Resp::Int(c.own.0 as i64), Resp::Int(c.own.1 as i64), node(&c.me)])];
                    v.extend(c.peers.iter().map(|(f, l, addr)| Resp::array(vec![Resp::Int(*f as i64), Resp::Int(*l as i64), node(addr)])));
                    Resp::array(v)
                }
                "INFO" => Resp::bulk("cluster_state:ok\r\ncluster_slots_assigned:16384\r\ncluster_known_nodes:".to_string() + &(c.peers.len() + 1).to_string() + "\r\n"),
                _ => syntax(),
            };
        }
        "SENTINEL" => {
            let sub = a.first().map(|s| upper(s)).unwrap_or_default();
            let masters = sh.sentinel.lock().unwrap().clone();
            if masters.is_empty() {
                return Resp::err("ERR unknown command 'sentinel'");
            }
            return match sub.as_str() {
                "GET-MASTER-ADDR-BY-NAME" if a.len() == 2 => match masters.get(String::from_utf8_lossy(&a[1]).as_ref()) {
                    Some(addr) => {
                        let (h, p) = addr.rsplit_once(':').unwrap_or((addr, "0"));
                        bulk_list([h.as_bytes().to_vec(), p.as_bytes().to_vec()])
                    }
                    None => null(st),
                },
                "MASTERS" => Resp::array(
                    masters
                        .iter()
                        .map(|(n, addr)| {
                            let (h, p) = addr.rsplit_once(':').unwrap_or((addr, "0"));
                            bulk_list([b"name".to_vec(), n.as_bytes().to_vec(), b"ip".to_vec(), h.as_bytes().to_vec(), b"port".to_vec(), p.as_bytes().to_vec(), b"flags".to_vec(), b"master".to_vec()])
                        })
                        .collect(),
                ),
                _ => syntax(),
            };
        }
        "BLPOP" | "BRPOP" | "BRPOPLPUSH" | "BLMOVE" => return blocking(sh, st, &name, a),
        _ => {}
    }
    if let (Some(c), Some(key)) = (sh.cluster.lock().unwrap().as_ref(), a.first()) {
        if !keyless(&name) {
            let slot = super::cluster::key_slot(key);
            if slot < c.own.0 || slot > c.own.1 {
                if !st.asking {
                    return match c.peers.iter().find(|(f, l, _)| (*f..=*l).contains(&slot)) {
                        Some((_, _, addr)) => Resp::err(format!("MOVED {} {}", slot, addr)),
                        None => Resp::err(format!("CLUSTERDOWN Hash slot {} not served", slot)),
                    };
                }
            }
        }
    }
    st.asking = false;
    let mut dbs = sh.dbs.lock().unwrap();
    let cur = st.db;
    if name == "MOVE" {
        if a.len() != 2 {
            return wrong_args(&name);
        }
        let Some(target) = int_arg(&a[1]).filter(|d| (0..16).contains(d)) else { return Resp::err("ERR DB index is out of range") };
        let target = target as usize;
        if target == cur {
            return Resp::err("ERR source and destination objects are the same");
        }
        purge(&mut dbs[cur], &a[0]);
        purge(&mut dbs[target], &a[0]);
        if !dbs[cur].contains_key(&a[0]) || dbs[target].contains_key(&a[0]) {
            return Resp::Int(0);
        }
        let e = dbs[cur].remove(&a[0]).unwrap();
        dbs[target].insert(a[0].clone(), e);
        return Resp::Int(1);
    }
    let db = &mut dbs[cur];
    data_command(db, st, &name, a)
}

fn blocking(sh: &Shared, st: &mut ConnState, name: &str, a: &[Vec<u8>]) -> Resp {
    if a.len() < 2 {
        return wrong_args(name);
    }
    let Some(timeout) = a.last().and_then(|t| float_arg(t)).filter(|t| *t >= 0.0) else { return Resp::err("ERR timeout is not a float or out of range") };
    let deadline = if timeout == 0.0 { None } else { Some(Instant::now() + Duration::from_secs_f64(timeout)) };
    let mut dbs = sh.dbs.lock().unwrap();
    loop {
        let db = &mut dbs[st.db];
        match name {
            "BLPOP" | "BRPOP" => {
                for k in &a[..a.len() - 1] {
                    let r = data_command(db, st, if name == "BLPOP" { "LPOP" } else { "RPOP" }, std::slice::from_ref(k));
                    if !r.is_null() {
                        if matches!(r, Resp::Error(_)) {
                            return r;
                        }
                        return Resp::array(vec![Resp::bulk(k.clone()), r]);
                    }
                }
            }
            _ => {
                let args: Vec<Vec<u8>> = if name == "BRPOPLPUSH" { a[..2].to_vec() } else { a[..a.len() - 1].to_vec() };
                let r = data_command(db, st, if name == "BRPOPLPUSH" { "RPOPLPUSH" } else { "LMOVE" }, &args);
                if !r.is_null() {
                    return r;
                }
            }
        }
        let wait = match deadline {
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    return if st.resp3 { Resp::Null } else { Resp::Array(None) };
                }
                (d - now).min(Duration::from_millis(100))
            }
            None => Duration::from_millis(100),
        };
        dbs = sh.cv.wait_timeout(dbs, wait).unwrap().0;
    }
}

#[allow(clippy::cognitive_complexity)]
fn data_command(db: &mut Db, st: &ConnState, name: &str, a: &[Vec<u8>]) -> Resp {
    let need = |n: usize| a.len() >= n;
    macro_rules! need {
        ($n:expr) => {
            if !need($n) {
                return wrong_args(name);
            }
        };
    }
    match name {
        // ---------------------------------------------------------------- keys
        "DEL" | "UNLINK" => {
            need!(1);
            let mut n = 0;
            for k in a {
                purge(db, k);
                if db.remove(k).is_some() {
                    n += 1;
                }
            }
            Resp::Int(n)
        }
        "EXISTS" => {
            need!(1);
            Resp::Int(a.iter().filter(|k| live(db, k).is_some()).count() as i64)
        }
        "TYPE" => {
            need!(1);
            Resp::Simple(
                match live(db, &a[0]).map(|e| &e.val) {
                    None => "none",
                    Some(RVal::Str(_)) => "string",
                    Some(RVal::List(_)) => "list",
                    Some(RVal::Hash(_)) => "hash",
                    Some(RVal::Set(_)) => "set",
                    Some(RVal::ZSet(_)) => "zset",
                }
                .into(),
            )
        }
        "EXPIRE" | "PEXPIRE" | "EXPIREAT" | "PEXPIREAT" => {
            need!(2);
            let Some(n) = int_arg(&a[1]) else { return not_int() };
            let ms = match name {
                "EXPIRE" => n.saturating_mul(1000),
                "PEXPIRE" => n,
                "EXPIREAT" => n.saturating_mul(1000) - now_millis(),
                _ => n - now_millis(),
            };
            match live(db, &a[0]) {
                None => Resp::Int(0),
                Some(e) => {
                    if ms <= 0 {
                        db.remove(&a[0]);
                    } else {
                        e.expire_at = Some(Instant::now() + Duration::from_millis(ms as u64));
                    }
                    Resp::Int(1)
                }
            }
        }
        "PERSIST" => {
            need!(1);
            match live(db, &a[0]) {
                Some(e) if e.expire_at.is_some() => {
                    e.expire_at = None;
                    Resp::Int(1)
                }
                _ => Resp::Int(0),
            }
        }
        "TTL" | "PTTL" => {
            need!(1);
            match live(db, &a[0]) {
                None => Resp::Int(-2),
                Some(Entry { expire_at: None, .. }) => Resp::Int(-1),
                Some(Entry { expire_at: Some(t), .. }) => {
                    let ms = t.saturating_duration_since(Instant::now()).as_millis() as i64;
                    Resp::Int(if name == "TTL" { (ms + 500) / 1000 } else { ms })
                }
            }
        }
        "KEYS" => {
            need!(1);
            let now = Instant::now();
            let mut keys: Vec<Vec<u8>> = db.iter().filter(|(_, e)| e.expire_at.is_none_or(|t| t > now)).map(|(k, _)| k.clone()).filter(|k| glob_match(&a[0], k)).collect();
            keys.sort();
            bulk_list(keys)
        }
        "RANDOMKEY" => {
            let now = Instant::now();
            match db.iter().filter(|(_, e)| e.expire_at.is_none_or(|t| t > now)).map(|(k, _)| k.clone()).min() {
                Some(k) => Resp::bulk(k),
                None => null(st),
            }
        }
        "RENAME" | "RENAMENX" => {
            need!(2);
            let Some(e) = live(db, &a[0]).cloned() else { return Resp::err("ERR no such key") };
            if name == "RENAMENX" && live(db, &a[1]).is_some() {
                return Resp::Int(0);
            }
            db.remove(&a[0]);
            db.insert(a[1].clone(), e);
            if name == "RENAME" { Resp::ok() } else { Resp::Int(1) }
        }
        "DBSIZE" => {
            let now = Instant::now();
            Resp::Int(db.values().filter(|e| e.expire_at.is_none_or(|t| t > now)).count() as i64)
        }
        "FLUSHDB" => {
            db.clear();
            Resp::ok()
        }
        "SORT" => {
            need!(1);
            let alpha = a[1..].iter().any(|x| upper(x) == "ALPHA");
            let desc = a[1..].iter().any(|x| upper(x) == "DESC");
            let mut items: Vec<Vec<u8>> = match live(db, &a[0]).map(|e| &e.val) {
                None => vec![],
                Some(RVal::List(l)) => l.iter().cloned().collect(),
                Some(RVal::Set(s)) => s.iter().cloned().collect(),
                Some(RVal::ZSet(z)) => z.keys().cloned().collect(),
                Some(_) => return wrongtype(),
            };
            if alpha {
                items.sort();
            } else {
                let mut parsed = Vec::new();
                for i in &items {
                    match float_arg(i) {
                        Some(f) => parsed.push((f, i.clone())),
                        None => return Resp::err("ERR One or more scores can't be converted into double"),
                    }
                }
                parsed.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
                items = parsed.into_iter().map(|p| p.1).collect();
            }
            if desc {
                items.reverse();
            }
            bulk_list(items)
        }
        // ---------------------------------------------------------------- strings
        "GET" => {
            need!(1);
            match get_typed!(db, &a[0], Str) {
                Some(v) => Resp::bulk(v.clone()),
                None => null(st),
            }
        }
        "GETDEL" => {
            need!(1);
            let r = match get_typed!(db, &a[0], Str) {
                Some(v) => Resp::bulk(v.clone()),
                None => return null(st),
            };
            db.remove(&a[0]);
            r
        }
        "SET" => {
            need!(2);
            let (mut nx, mut xx, mut get, mut keepttl) = (false, false, false, false);
            let mut ttl: Option<i64> = None;
            let mut i = 2;
            while i < a.len() {
                match upper(&a[i]).as_str() {
                    "NX" => nx = true,
                    "XX" => xx = true,
                    "GET" => get = true,
                    "KEEPTTL" => keepttl = true,
                    o @ ("EX" | "PX" | "EXAT" | "PXAT") => {
                        let Some(n) = a.get(i + 1).and_then(|v| int_arg(v)) else { return not_int() };
                        ttl = Some(match o {
                            "EX" => n * 1000,
                            "PX" => n,
                            "EXAT" => n * 1000 - now_millis(),
                            _ => n - now_millis(),
                        });
                        i += 1;
                    }
                    _ => return syntax(),
                }
                i += 1;
            }
            if ttl.is_some_and(|t| t <= 0) && !matches!(upper(a.get(2).map(|x| x.as_slice()).unwrap_or_default()).as_str(), "EXAT" | "PXAT") {
                return Resp::err("ERR invalid expire time in 'set' command");
            }
            let old = live(db, &a[0]).cloned();
            let old_reply = match &old {
                Some(Entry { val: RVal::Str(v), .. }) => Resp::bulk(v.clone()),
                Some(_) if get => return wrongtype(),
                _ => null(st),
            };
            if (nx && old.is_some()) || (xx && old.is_none()) {
                return if get { old_reply } else { null(st) };
            }
            let expire_at = match ttl {
                Some(t) => Some(Instant::now() + Duration::from_millis(t.max(0) as u64)),
                None if keepttl => old.and_then(|o| o.expire_at),
                None => None,
            };
            db.insert(a[0].clone(), Entry { val: RVal::Str(a[1].clone()), expire_at });
            if get { old_reply } else { Resp::ok() }
        }
        "SETEX" | "PSETEX" => {
            need!(3);
            let Some(n) = int_arg(&a[1]).filter(|n| *n > 0) else { return Resp::err(format!("ERR invalid expire time in '{}' command", name.to_ascii_lowercase())) };
            let ms = if name == "SETEX" { n * 1000 } else { n };
            db.insert(a[0].clone(), Entry { val: RVal::Str(a[2].clone()), expire_at: Some(Instant::now() + Duration::from_millis(ms as u64)) });
            Resp::ok()
        }
        "SETNX" => {
            need!(2);
            if live(db, &a[0]).is_some() {
                return Resp::Int(0);
            }
            db.insert(a[0].clone(), Entry { val: RVal::Str(a[1].clone()), expire_at: None });
            Resp::Int(1)
        }
        "GETSET" => {
            need!(2);
            let old = match get_typed!(db, &a[0], Str) {
                Some(v) => Resp::bulk(v.clone()),
                None => null(st),
            };
            db.insert(a[0].clone(), Entry { val: RVal::Str(a[1].clone()), expire_at: None });
            old
        }
        "MGET" => {
            need!(1);
            Resp::array(
                a.iter()
                    .map(|k| match live(db, k).map(|e| &e.val) {
                        Some(RVal::Str(v)) => Resp::bulk(v.clone()),
                        _ => null(st),
                    })
                    .collect(),
            )
        }
        "MSET" | "MSETNX" => {
            if a.is_empty() || a.len() % 2 != 0 {
                return wrong_args(name);
            }
            if name == "MSETNX" && a.chunks(2).any(|p| live(db, &p[0]).is_some()) {
                return Resp::Int(0);
            }
            for p in a.chunks(2) {
                db.insert(p[0].clone(), Entry { val: RVal::Str(p[1].clone()), expire_at: None });
            }
            if name == "MSET" { Resp::ok() } else { Resp::Int(1) }
        }
        "APPEND" => {
            need!(2);
            let s = get_or_create!(db, &a[0], Str, Vec::new());
            s.extend_from_slice(&a[1]);
            Resp::Int(s.len() as i64)
        }
        "STRLEN" => {
            need!(1);
            Resp::Int(get_typed!(db, &a[0], Str).map(|s| s.len()).unwrap_or(0) as i64)
        }
        "INCR" | "DECR" | "INCRBY" | "DECRBY" => {
            need!(if name.ends_with("BY") { 2 } else { 1 });
            let by = if name.ends_with("BY") {
                match int_arg(&a[1]) {
                    Some(n) => n,
                    None => return not_int(),
                }
            } else {
                1
            };
            let by = if name.starts_with("DECR") { -by } else { by };
            let s = get_or_create!(db, &a[0], Str, b"0".to_vec());
            let Some(cur) = int_arg(s) else { return not_int() };
            let Some(n) = cur.checked_add(by) else { return Resp::err("ERR increment or decrement would overflow") };
            *s = n.to_string().into_bytes();
            Resp::Int(n)
        }
        "INCRBYFLOAT" => {
            need!(2);
            let Some(by) = float_arg(&a[1]) else { return Resp::err("ERR value is not a valid float") };
            let s = get_or_create!(db, &a[0], Str, b"0".to_vec());
            let Some(cur) = float_arg(s) else { return Resp::err("ERR value is not a valid float") };
            let v = format_double(cur + by);
            *s = v.clone().into_bytes();
            Resp::bulk(v)
        }
        "GETRANGE" | "SUBSTR" => {
            need!(3);
            let (Some(s0), Some(e0)) = (int_arg(&a[1]), int_arg(&a[2])) else { return not_int() };
            let v = get_typed!(db, &a[0], Str).cloned().unwrap_or_default();
            match norm_range(s0, e0, v.len()) {
                Some((s, e)) => Resp::bulk(v[s..=e].to_vec()),
                None => Resp::bulk(Vec::new()),
            }
        }
        "SETRANGE" => {
            need!(3);
            let Some(off) = int_arg(&a[1]).filter(|o| *o >= 0) else { return Resp::err("ERR offset is out of range") };
            let s = get_or_create!(db, &a[0], Str, Vec::new());
            let off = off as usize;
            if s.len() < off + a[2].len() {
                s.resize(off + a[2].len(), 0);
            }
            s[off..off + a[2].len()].copy_from_slice(&a[2]);
            Resp::Int(s.len() as i64)
        }
        "GETBIT" => {
            need!(2);
            let Some(off) = int_arg(&a[1]).filter(|o| *o >= 0) else { return Resp::err("ERR bit offset is not an integer or out of range") };
            let v = get_typed!(db, &a[0], Str).cloned().unwrap_or_default();
            let (byte, bit) = ((off / 8) as usize, 7 - (off % 8) as u32);
            Resp::Int(v.get(byte).map(|b| ((b >> bit) & 1) as i64).unwrap_or(0))
        }
        "SETBIT" => {
            need!(3);
            let Some(off) = int_arg(&a[1]).filter(|o| *o >= 0) else { return Resp::err("ERR bit offset is not an integer or out of range") };
            let on = match a[2].as_slice() {
                b"1" => true,
                b"0" => false,
                _ => return Resp::err("ERR bit is not an integer or out of range"),
            };
            let s = get_or_create!(db, &a[0], Str, Vec::new());
            let (byte, bit) = ((off / 8) as usize, 7 - (off % 8) as u32);
            if s.len() <= byte {
                s.resize(byte + 1, 0);
            }
            let old = (s[byte] >> bit) & 1;
            if on {
                s[byte] |= 1 << bit;
            } else {
                s[byte] &= !(1 << bit);
            }
            Resp::Int(old as i64)
        }
        // ---------------------------------------------------------------- hashes
        "HSET" | "HMSET" => {
            if a.len() < 3 || a.len() % 2 == 0 {
                return wrong_args(name);
            }
            let h = get_or_create!(db, &a[0], Hash, BTreeMap::new());
            let mut n = 0;
            for p in a[1..].chunks(2) {
                if h.insert(p[0].clone(), p[1].clone()).is_none() {
                    n += 1;
                }
            }
            if name == "HSET" { Resp::Int(n) } else { Resp::ok() }
        }
        "HSETNX" => {
            need!(3);
            let h = get_or_create!(db, &a[0], Hash, BTreeMap::new());
            if h.contains_key(&a[1]) {
                return Resp::Int(0);
            }
            h.insert(a[1].clone(), a[2].clone());
            Resp::Int(1)
        }
        "HGET" => {
            need!(2);
            match get_typed!(db, &a[0], Hash).and_then(|h| h.get(&a[1])) {
                Some(v) => Resp::bulk(v.clone()),
                None => null(st),
            }
        }
        "HMGET" => {
            need!(2);
            let h = get_typed!(db, &a[0], Hash).cloned().unwrap_or_default();
            Resp::array(a[1..].iter().map(|f| h.get(f).map(|v| Resp::bulk(v.clone())).unwrap_or_else(|| null(st))).collect())
        }
        "HGETALL" => {
            need!(1);
            let h = get_typed!(db, &a[0], Hash).cloned().unwrap_or_default();
            if st.resp3 {
                Resp::Map(h.into_iter().map(|(k, v)| (Resp::bulk(k), Resp::bulk(v))).collect())
            } else {
                bulk_list(h.into_iter().flat_map(|(k, v)| [k, v]))
            }
        }
        "HDEL" => {
            need!(2);
            let n = match get_typed!(db, &a[0], Hash) {
                Some(h) => a[1..].iter().filter(|f| h.remove(*f).is_some()).count(),
                None => 0,
            };
            cleanup_empty(db, &a[0]);
            Resp::Int(n as i64)
        }
        "HEXISTS" => {
            need!(2);
            Resp::Int(get_typed!(db, &a[0], Hash).is_some_and(|h| h.contains_key(&a[1])) as i64)
        }
        "HLEN" => {
            need!(1);
            Resp::Int(get_typed!(db, &a[0], Hash).map(|h| h.len()).unwrap_or(0) as i64)
        }
        "HKEYS" => {
            need!(1);
            bulk_list(get_typed!(db, &a[0], Hash).map(|h| h.keys().cloned().collect::<Vec<_>>()).unwrap_or_default())
        }
        "HVALS" => {
            need!(1);
            bulk_list(get_typed!(db, &a[0], Hash).map(|h| h.values().cloned().collect::<Vec<_>>()).unwrap_or_default())
        }
        "HINCRBY" => {
            need!(3);
            let Some(by) = int_arg(&a[2]) else { return not_int() };
            let h = get_or_create!(db, &a[0], Hash, BTreeMap::new());
            let cur = match h.get(&a[1]) {
                Some(v) => match int_arg(v) {
                    Some(n) => n,
                    None => return Resp::err("ERR hash value is not an integer"),
                },
                None => 0,
            };
            let n = cur + by;
            h.insert(a[1].clone(), n.to_string().into_bytes());
            Resp::Int(n)
        }
        "HINCRBYFLOAT" => {
            need!(3);
            let Some(by) = float_arg(&a[2]) else { return Resp::err("ERR value is not a valid float") };
            let h = get_or_create!(db, &a[0], Hash, BTreeMap::new());
            let cur = h.get(&a[1]).and_then(|v| float_arg(v)).unwrap_or(0.0);
            let v = format_double(cur + by);
            h.insert(a[1].clone(), v.clone().into_bytes());
            Resp::bulk(v)
        }
        // ---------------------------------------------------------------- lists
        "LPUSH" | "RPUSH" | "LPUSHX" | "RPUSHX" => {
            need!(2);
            if name.ends_with('X') && live(db, &a[0]).is_none() {
                return Resp::Int(0);
            }
            let l = get_or_create!(db, &a[0], List, VecDeque::new());
            for v in &a[1..] {
                if name.starts_with('L') {
                    l.push_front(v.clone());
                } else {
                    l.push_back(v.clone());
                }
            }
            Resp::Int(l.len() as i64)
        }
        "LPOP" | "RPOP" => {
            need!(1);
            let count = match a.get(1) {
                Some(c) => match int_arg(c).filter(|c| *c >= 0) {
                    Some(c) => Some(c as usize),
                    None => return Resp::err("ERR value is out of range, must be positive"),
                },
                None => None,
            };
            let Some(l) = get_typed!(db, &a[0], List) else { return if count.is_some() { if st.resp3 { Resp::Null } else { Resp::Array(None) } } else { null(st) } };
            let mut out = Vec::new();
            for _ in 0..count.unwrap_or(1) {
                let v = if name == "LPOP" { l.pop_front() } else { l.pop_back() };
                match v {
                    Some(v) => out.push(v),
                    None => break,
                }
            }
            cleanup_empty(db, &a[0]);
            match count {
                Some(_) => bulk_list(out),
                None => out.pop().map(Resp::bulk).unwrap_or_else(|| null(st)),
            }
        }
        "LLEN" => {
            need!(1);
            Resp::Int(get_typed!(db, &a[0], List).map(|l| l.len()).unwrap_or(0) as i64)
        }
        "LRANGE" => {
            need!(3);
            let (Some(s0), Some(e0)) = (int_arg(&a[1]), int_arg(&a[2])) else { return not_int() };
            let l = get_typed!(db, &a[0], List).cloned().unwrap_or_default();
            match norm_range(s0, e0, l.len()) {
                Some((s, e)) => bulk_list(l.into_iter().skip(s).take(e - s + 1)),
                None => Resp::array(vec![]),
            }
        }
        "LINDEX" => {
            need!(2);
            let Some(i) = int_arg(&a[1]) else { return not_int() };
            let l = get_typed!(db, &a[0], List).cloned().unwrap_or_default();
            let idx = if i < 0 { l.len() as i64 + i } else { i };
            match (idx >= 0).then(|| l.get(idx as usize)).flatten() {
                Some(v) => Resp::bulk(v.clone()),
                None => null(st),
            }
        }
        "LSET" => {
            need!(3);
            let Some(i) = int_arg(&a[1]) else { return not_int() };
            let Some(l) = get_typed!(db, &a[0], List) else { return Resp::err("ERR no such key") };
            let idx = if i < 0 { l.len() as i64 + i } else { i };
            match (idx >= 0).then(|| l.get_mut(idx as usize)).flatten() {
                Some(slot) => {
                    *slot = a[2].clone();
                    Resp::ok()
                }
                None => Resp::err("ERR index out of range"),
            }
        }
        "LTRIM" => {
            need!(3);
            let (Some(s0), Some(e0)) = (int_arg(&a[1]), int_arg(&a[2])) else { return not_int() };
            if let Some(l) = get_typed!(db, &a[0], List) {
                *l = match norm_range(s0, e0, l.len()) {
                    Some((s, e)) => l.iter().skip(s).take(e - s + 1).cloned().collect(),
                    None => VecDeque::new(),
                };
            }
            cleanup_empty(db, &a[0]);
            Resp::ok()
        }
        "LREM" => {
            need!(3);
            let Some(count) = int_arg(&a[1]) else { return not_int() };
            let Some(l) = get_typed!(db, &a[0], List) else { return Resp::Int(0) };
            let mut removed = 0i64;
            let limit = if count == 0 { i64::MAX } else { count.abs() };
            if count >= 0 {
                let mut out = VecDeque::new();
                for v in l.drain(..) {
                    if v == a[2] && removed < limit {
                        removed += 1;
                    } else {
                        out.push_back(v);
                    }
                }
                *l = out;
            } else {
                let mut out = VecDeque::new();
                for v in l.drain(..).rev() {
                    if v == a[2] && removed < limit {
                        removed += 1;
                    } else {
                        out.push_front(v);
                    }
                }
                *l = out;
            }
            cleanup_empty(db, &a[0]);
            Resp::Int(removed)
        }
        "LINSERT" => {
            need!(4);
            let before = match upper(&a[1]).as_str() {
                "BEFORE" => true,
                "AFTER" => false,
                _ => return syntax(),
            };
            let Some(l) = get_typed!(db, &a[0], List) else { return Resp::Int(0) };
            match l.iter().position(|v| *v == a[2]) {
                Some(p) => {
                    l.insert(if before { p } else { p + 1 }, a[3].clone());
                    Resp::Int(l.len() as i64)
                }
                None => Resp::Int(-1),
            }
        }
        "RPOPLPUSH" | "LMOVE" => {
            need!(2);
            let (from_left, to_left) = if name == "LMOVE" {
                need!(4);
                (upper(&a[2]) == "LEFT", upper(&a[3]) == "LEFT")
            } else {
                (false, true)
            };
            if let Some(Entry { val, .. }) = live(db, &a[1]) {
                if !matches!(val, RVal::List(_)) {
                    return wrongtype();
                }
            }
            let v = match get_typed!(db, &a[0], List) {
                Some(l) => {
                    if from_left {
                        l.pop_front()
                    } else {
                        l.pop_back()
                    }
                }
                None => None,
            };
            let Some(v) = v else { return null(st) };
            cleanup_empty(db, &a[0]);
            let dst = get_or_create!(db, &a[1], List, VecDeque::new());
            if to_left {
                dst.push_front(v.clone());
            } else {
                dst.push_back(v.clone());
            }
            Resp::bulk(v)
        }
        // ---------------------------------------------------------------- sets
        "SADD" => {
            need!(2);
            let s = get_or_create!(db, &a[0], Set, BTreeSet::new());
            Resp::Int(a[1..].iter().filter(|m| s.insert((*m).clone())).count() as i64)
        }
        "SREM" => {
            need!(2);
            let n = match get_typed!(db, &a[0], Set) {
                Some(s) => a[1..].iter().filter(|m| s.remove(*m)).count(),
                None => 0,
            };
            cleanup_empty(db, &a[0]);
            Resp::Int(n as i64)
        }
        "SMEMBERS" => {
            need!(1);
            let s = get_typed!(db, &a[0], Set).cloned().unwrap_or_default();
            set_reply(st, s.into_iter().collect())
        }
        "SISMEMBER" => {
            need!(2);
            Resp::Int(get_typed!(db, &a[0], Set).is_some_and(|s| s.contains(&a[1])) as i64)
        }
        "SCARD" => {
            need!(1);
            Resp::Int(get_typed!(db, &a[0], Set).map(|s| s.len()).unwrap_or(0) as i64)
        }
        "SPOP" | "SRANDMEMBER" => {
            need!(1);
            let count = a.get(1).and_then(|c| int_arg(c));
            let Some(s) = get_typed!(db, &a[0], Set) else { return if count.is_some() { Resp::array(vec![]) } else { null(st) } };
            let n = count.map(|c| c.unsigned_abs() as usize).unwrap_or(1);
            // deterministic-but-varying choice: rotate by a counter
            let start = (next_id() as usize) % s.len().max(1);
            let picked: Vec<Vec<u8>> = s.iter().cycle().skip(start).take(n.min(s.len())).cloned().collect();
            if name == "SPOP" {
                for p in &picked {
                    s.remove(p);
                }
            }
            cleanup_empty(db, &a[0]);
            match count {
                Some(_) => bulk_list(picked),
                None => picked.into_iter().next().map(Resp::bulk).unwrap_or_else(|| null(st)),
            }
        }
        "SMOVE" => {
            need!(3);
            let moved = match get_typed!(db, &a[0], Set) {
                Some(s) => s.remove(&a[2]),
                None => false,
            };
            if !moved {
                return Resp::Int(0);
            }
            cleanup_empty(db, &a[0]);
            get_or_create!(db, &a[1], Set, BTreeSet::new()).insert(a[2].clone());
            Resp::Int(1)
        }
        "SUNION" | "SINTER" | "SDIFF" | "SUNIONSTORE" | "SINTERSTORE" | "SDIFFSTORE" => {
            let store = name.ends_with("STORE");
            need!(if store { 2 } else { 1 });
            let keys = if store { &a[1..] } else { a };
            let mut sets = Vec::new();
            for k in keys {
                sets.push(get_typed!(db, k, Set).cloned().unwrap_or_default());
            }
            let mut it = sets.into_iter();
            let first = it.next().unwrap_or_default();
            let result: BTreeSet<Vec<u8>> = it.fold(first, |acc, s| match &name[..5] {
                "SUNIO" => acc.union(&s).cloned().collect(),
                "SINTE" => acc.intersection(&s).cloned().collect(),
                _ => acc.difference(&s).cloned().collect(),
            });
            if store {
                let n = result.len();
                db.remove(&a[0]);
                if n > 0 {
                    db.insert(a[0].clone(), Entry { val: RVal::Set(result), expire_at: None });
                }
                Resp::Int(n as i64)
            } else {
                set_reply(st, result.into_iter().collect())
            }
        }
        // ---------------------------------------------------------------- sorted sets
        "ZADD" => {
            need!(3);
            let (mut nx, mut xx, mut ch, mut incr) = (false, false, false, false);
            let mut i = 1;
            while i < a.len() {
                match upper(&a[i]).as_str() {
                    "NX" => nx = true,
                    "XX" => xx = true,
                    "CH" => ch = true,
                    "INCR" => incr = true,
                    "GT" | "LT" => {}
                    _ => break,
                }
                i += 1;
            }
            let rest = &a[i..];
            if rest.is_empty() || rest.len() % 2 != 0 {
                return syntax();
            }
            let mut pairs = Vec::new();
            for p in rest.chunks(2) {
                match float_arg(&p[0]) {
                    Some(f) => pairs.push((f, p[1].clone())),
                    None => return Resp::err("ERR value is not a valid float"),
                }
            }
            let z = get_or_create!(db, &a[0], ZSet, BTreeMap::new());
            let mut n = 0;
            let mut last = None;
            for (score, m) in pairs {
                let exists = z.contains_key(&m);
                if (nx && exists) || (xx && !exists) {
                    continue;
                }
                let new = if incr { z.get(&m).copied().unwrap_or(0.0) + score } else { score };
                let changed = z.get(&m) != Some(&new);
                z.insert(m, new);
                last = Some(new);
                if !exists || (ch && changed) {
                    n += 1;
                }
            }
            cleanup_empty(db, &a[0]);
            if incr {
                return last.map(|d| double_reply(st, d)).unwrap_or_else(|| null(st));
            }
            Resp::Int(n)
        }
        "ZREM" => {
            need!(2);
            let n = match get_typed!(db, &a[0], ZSet) {
                Some(z) => a[1..].iter().filter(|m| z.remove(*m).is_some()).count(),
                None => 0,
            };
            cleanup_empty(db, &a[0]);
            Resp::Int(n as i64)
        }
        "ZSCORE" => {
            need!(2);
            match get_typed!(db, &a[0], ZSet).and_then(|z| z.get(&a[1]).copied()) {
                Some(s) => double_reply(st, s),
                None => null(st),
            }
        }
        "ZCARD" => {
            need!(1);
            Resp::Int(get_typed!(db, &a[0], ZSet).map(|z| z.len()).unwrap_or(0) as i64)
        }
        "ZCOUNT" => {
            need!(3);
            let (Some(min), Some(max)) = (score_bound(&a[1]), score_bound(&a[2])) else { return Resp::err("ERR min or max is not a float") };
            Resp::Int(get_typed!(db, &a[0], ZSet).map(|z| z.values().filter(|s| in_range(**s, min, max)).count()).unwrap_or(0) as i64)
        }
        "ZINCRBY" => {
            need!(3);
            let Some(by) = float_arg(&a[1]) else { return Resp::err("ERR value is not a valid float") };
            let z = get_or_create!(db, &a[0], ZSet, BTreeMap::new());
            let v = z.get(&a[2]).copied().unwrap_or(0.0) + by;
            z.insert(a[2].clone(), v);
            double_reply(st, v)
        }
        "ZRANGE" | "ZREVRANGE" => {
            need!(3);
            let (Some(s0), Some(e0)) = (int_arg(&a[1]), int_arg(&a[2])) else { return not_int() };
            let with = a[3..].iter().any(|x| upper(x) == "WITHSCORES");
            let rev = name == "ZREVRANGE" || a[3..].iter().any(|x| upper(x) == "REV");
            let mut items = get_typed!(db, &a[0], ZSet).map(|z| zsorted(z)).unwrap_or_default();
            if rev {
                items.reverse();
            }
            match norm_range(s0, e0, items.len()) {
                Some((s, e)) => zitems(items[s..=e].to_vec(), with, st),
                None => Resp::array(vec![]),
            }
        }
        "ZRANGEBYSCORE" | "ZREVRANGEBYSCORE" => {
            need!(3);
            let rev = name.starts_with("ZREV");
            let (lo, hi) = if rev { (&a[2], &a[1]) } else { (&a[1], &a[2]) };
            let (Some(min), Some(max)) = (score_bound(lo), score_bound(hi)) else { return Resp::err("ERR min or max is not a float") };
            let with = a[3..].iter().any(|x| upper(x) == "WITHSCORES");
            let limit = a.iter().position(|x| upper(x) == "LIMIT").and_then(|p| Some((int_arg(a.get(p + 1)?)?, int_arg(a.get(p + 2)?)?)));
            let mut items: Vec<(Vec<u8>, f64)> = get_typed!(db, &a[0], ZSet).map(|z| zsorted(z)).unwrap_or_default().into_iter().filter(|(_, s)| in_range(*s, min, max)).collect();
            if rev {
                items.reverse();
            }
            if let Some((off, cnt)) = limit {
                let off = off.max(0) as usize;
                items = items.into_iter().skip(off).take(if cnt < 0 { usize::MAX } else { cnt as usize }).collect();
            }
            zitems(items, with, st)
        }
        "ZRANK" | "ZREVRANK" => {
            need!(2);
            let mut items = get_typed!(db, &a[0], ZSet).map(|z| zsorted(z)).unwrap_or_default();
            if name == "ZREVRANK" {
                items.reverse();
            }
            match items.iter().position(|(m, _)| *m == a[1]) {
                Some(p) => Resp::Int(p as i64),
                None => null(st),
            }
        }
        "ZREMRANGEBYRANK" | "ZREMRANGEBYSCORE" => {
            need!(3);
            let Some(z) = get_typed!(db, &a[0], ZSet) else { return Resp::Int(0) };
            let sorted = zsorted(z);
            let victims: Vec<Vec<u8>> = if name == "ZREMRANGEBYRANK" {
                let (Some(s0), Some(e0)) = (int_arg(&a[1]), int_arg(&a[2])) else { return not_int() };
                match norm_range(s0, e0, sorted.len()) {
                    Some((s, e)) => sorted[s..=e].iter().map(|p| p.0.clone()).collect(),
                    None => vec![],
                }
            } else {
                let (Some(min), Some(max)) = (score_bound(&a[1]), score_bound(&a[2])) else { return Resp::err("ERR min or max is not a float") };
                sorted.iter().filter(|(_, s)| in_range(*s, min, max)).map(|p| p.0.clone()).collect()
            };
            for v in &victims {
                z.remove(v);
            }
            cleanup_empty(db, &a[0]);
            Resp::Int(victims.len() as i64)
        }
        "ZUNIONSTORE" | "ZINTERSTORE" => {
            need!(3);
            let Some(n) = int_arg(&a[1]).filter(|n| *n > 0) else { return Resp::err("ERR at least 1 input key is needed for 'zunionstore' command") };
            let n = n as usize;
            if a.len() < 2 + n {
                return syntax();
            }
            let mut sets = Vec::new();
            for k in &a[2..2 + n] {
                let z = match live(db, k).map(|e| &e.val) {
                    None => BTreeMap::new(),
                    Some(RVal::ZSet(z)) => z.clone(),
                    Some(RVal::Set(s)) => s.iter().map(|m| (m.clone(), 1.0)).collect(),
                    Some(_) => return wrongtype(),
                };
                sets.push(z);
            }
            let mut it = sets.into_iter();
            let mut acc = it.next().unwrap_or_default();
            for z in it {
                if name == "ZUNIONSTORE" {
                    for (m, s) in z {
                        *acc.entry(m).or_insert(0.0) += s;
                    }
                } else {
                    acc = acc.into_iter().filter_map(|(m, s)| z.get(&m).map(|s2| (m, s + s2))).collect();
                }
            }
            let len = acc.len();
            db.remove(&a[0]);
            if len > 0 {
                db.insert(a[0].clone(), Entry { val: RVal::ZSet(acc), expire_at: None });
            }
            Resp::Int(len as i64)
        }
        // ---------------------------------------------------------------- geo
        "GEOADD" => {
            need!(4);
            let mut i = 1;
            while i < a.len() && matches!(upper(&a[i]).as_str(), "NX" | "XX" | "CH") {
                i += 1;
            }
            let rest = &a[i..];
            if rest.is_empty() || rest.len() % 3 != 0 {
                return syntax();
            }
            let mut items = Vec::new();
            for t in rest.chunks(3) {
                let (Some(lon), Some(lat)) = (float_arg(&t[0]), float_arg(&t[1])) else { return Resp::err("ERR value is not a valid float") };
                if !super::geo::valid(lon, lat) {
                    return Resp::err(format!("ERR invalid longitude,latitude pair {:.6},{:.6}", lon, lat));
                }
                items.push((super::geo::encode(lon, lat) as f64, t[2].clone()));
            }
            let z = get_or_create!(db, &a[0], ZSet, BTreeMap::new());
            let mut n = 0;
            for (score, m) in items {
                if z.insert(m, score).is_none() {
                    n += 1;
                }
            }
            Resp::Int(n)
        }
        "GEODIST" => {
            need!(3);
            let factor = match a.get(3) {
                Some(u) => match super::geo::unit_factor(&String::from_utf8_lossy(u)) {
                    Some(f) => f,
                    None => return Resp::err("ERR unsupported unit provided. please use M, KM, FT, MI"),
                },
                None => 1.0,
            };
            let Some(z) = get_typed!(db, &a[0], ZSet) else { return null(st) };
            match (z.get(&a[1]), z.get(&a[2])) {
                (Some(s1), Some(s2)) => {
                    let (lo1, la1) = super::geo::decode(*s1 as u64);
                    let (lo2, la2) = super::geo::decode(*s2 as u64);
                    Resp::bulk(format!("{:.4}", super::geo::distance(lo1, la1, lo2, la2) / factor))
                }
                _ => null(st),
            }
        }
        "GEOHASH" | "GEOPOS" => {
            need!(1);
            let z = get_typed!(db, &a[0], ZSet).cloned().unwrap_or_default();
            Resp::array(
                a[1..]
                    .iter()
                    .map(|m| match z.get(m) {
                        Some(s) if name == "GEOHASH" => Resp::bulk(super::geo::geohash_text(*s as u64)),
                        Some(s) => {
                            let (lon, lat) = super::geo::decode(*s as u64);
                            Resp::array(vec![Resp::bulk(format!("{:.17}", lon)), Resp::bulk(format!("{:.17}", lat))])
                        }
                        None if name == "GEOHASH" => null(st),
                        None => Resp::Array(None),
                    })
                    .collect(),
            )
        }
        "GEORADIUS" | "GEORADIUSBYMEMBER" | "GEORADIUS_RO" | "GEORADIUSBYMEMBER_RO" => {
            let by_member = name.starts_with("GEORADIUSBYMEMBER");
            let fixed = if by_member { 4 } else { 5 };
            need!(fixed);
            let z = get_typed!(db, &a[0], ZSet).cloned().unwrap_or_default();
            let (lon, lat) = if by_member {
                match z.get(&a[1]) {
                    Some(s) => super::geo::decode(*s as u64),
                    None => return Resp::err("ERR could not decode requested zset member"),
                }
            } else {
                match (float_arg(&a[1]), float_arg(&a[2])) {
                    (Some(lo), Some(la)) => (lo, la),
                    _ => return Resp::err("ERR value is not a valid float"),
                }
            };
            let (r_idx, u_idx) = if by_member { (2, 3) } else { (3, 4) };
            let Some(radius) = float_arg(&a[r_idx]) else { return Resp::err("ERR need numeric radius") };
            let Some(factor) = super::geo::unit_factor(&String::from_utf8_lossy(&a[u_idx])) else { return Resp::err("ERR unsupported unit provided. please use M, KM, FT, MI") };
            let (mut wc, mut wd, mut wh, mut count, mut desc) = (false, false, false, None, None);
            let mut i = fixed;
            while i < a.len() {
                match upper(&a[i]).as_str() {
                    "WITHCOORD" => wc = true,
                    "WITHDIST" => wd = true,
                    "WITHHASH" => wh = true,
                    "ASC" => desc = Some(false),
                    "DESC" => desc = Some(true),
                    "COUNT" => {
                        i += 1;
                        match a.get(i).and_then(|c| int_arg(c)).filter(|c| *c > 0) {
                            Some(c) => count = Some(c as usize),
                            None => return Resp::err("ERR COUNT must be > 0"),
                        }
                    }
                    "ANY" => {}
                    _ => return syntax(),
                }
                i += 1;
            }
            let mut hits: Vec<(f64, Vec<u8>, f64)> = z
                .iter()
                .filter_map(|(m, s)| {
                    let (mlo, mla) = super::geo::decode(*s as u64);
                    let d = super::geo::distance(lon, lat, mlo, mla);
                    (d <= radius * factor).then(|| (d, m.clone(), *s))
                })
                .collect();
            if desc.is_some() || count.is_some() {
                hits.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
                if desc == Some(true) {
                    hits.reverse();
                }
            }
            if let Some(c) = count {
                hits.truncate(c);
            }
            Resp::array(
                hits.into_iter()
                    .map(|(d, m, s)| {
                        if !(wc || wd || wh) {
                            return Resp::bulk(m);
                        }
                        let mut v = vec![Resp::bulk(m)];
                        if wd {
                            v.push(Resp::bulk(format!("{:.4}", d / factor)));
                        }
                        if wh {
                            v.push(Resp::Int(s as i64));
                        }
                        if wc {
                            let (lo, la) = super::geo::decode(s as u64);
                            v.push(Resp::array(vec![Resp::bulk(format!("{:.17}", lo)), Resp::bulk(format!("{:.17}", la))]));
                        }
                        Resp::array(v)
                    })
                    .collect(),
            )
        }
        _ => Resp::err(format!("ERR unknown command '{}', with args beginning with: ", name.to_ascii_lowercase())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob() {
        assert!(glob_match(b"news.*", b"news.sport"));
        assert!(!glob_match(b"news.*", b"new.sport"));
        assert!(glob_match(b"h?llo", b"hello"));
        assert!(glob_match(b"h[ae]llo", b"hallo"));
        assert!(!glob_match(b"h[^e]llo", b"hello"));
        assert!(glob_match(b"h[a-c]llo", b"hbllo"));
        assert!(glob_match(b"*", b""));
    }

    #[test]
    fn direct_commands() {
        let s = RedisServer::start().unwrap();
        assert_eq!(s.exec(&["SET", "a", "1"]), Resp::ok());
        assert_eq!(s.exec(&["INCRBY", "a", "5"]), Resp::Int(6));
        assert_eq!(s.exec(&["LPUSH", "l", "x", "y"]), Resp::Int(2));
        assert_eq!(s.exec(&["LRANGE", "l", "0", "-1"]), bulk_list([b"y".to_vec(), b"x".to_vec()]));
        assert_eq!(s.exec(&["GET", "l"]), wrongtype());
        assert_eq!(s.exec(&["ZADD", "z", "2", "b", "1", "a"]), Resp::Int(2));
        assert_eq!(s.exec(&["ZRANGE", "z", "0", "-1", "WITHSCORES"]), bulk_list([b"a".to_vec(), b"1".to_vec(), b"b".to_vec(), b"2".to_vec()]));
        assert_eq!(s.exec(&["ZRANGEBYSCORE", "z", "(1", "+inf"]), bulk_list([b"b".to_vec()]));
        assert_eq!(s.exec(&["SADD", "s1", "a", "b"]), Resp::Int(2));
        assert_eq!(s.exec(&["SADD", "s2", "b", "c"]), Resp::Int(2));
        assert_eq!(s.exec(&["SINTER", "s1", "s2"]), bulk_list([b"b".to_vec()]));
        assert_eq!(s.exec(&["SET", "e", "v", "PX", "1"]), Resp::ok());
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(s.exec(&["EXISTS", "e"]), Resp::Int(0));
        assert_eq!(s.exec(&["MULTI"]), Resp::ok());
    }
}
