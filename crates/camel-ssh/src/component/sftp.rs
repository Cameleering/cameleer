//! `sftp:` component (Camel `camel-ftp` SFTP flavour).
//!
//! `sftp://[username[:password]@]host[:port]/directoryname[?options]` — the directory
//! is relative to the login (home) directory; use `//abs/dir` for an absolute one.
//!
//! **Connection options**: see [the component module](super) (`password`,
//! `privateKeyFile`, `knownHostsFile`, `strictHostKeyChecking`, ...) plus
//! `disconnect`, `soTimeout` (5 min), `timeout` (accepted), `sendNoop` (check pooled
//! connections, default true), `maximumReconnectAttempts` (3), `reconnectDelay` (1s),
//! `throwExceptionOnConnectFailed`, `autoCreate` (true), `charset`, `bulkRequests`
//! (outstanding pipelined read/write requests, default 16), `chmod` / `chmodDirectory`
//! (octal permissions of uploaded files / created directories).
//!
//! **Consumer** (scheduled poll, `initialDelay` 1000ms, `delay` 500ms and all scheduler
//! options): `noop`, `delete`, `move`, `moveFailed`, `preMove`, `include`/`exclude`
//! (regex), `includeExt`/`excludeExt`, `antInclude`/`antExclude`, `antFilterCaseSensitive`,
//! `fileName`, `recursive`, `minDepth`/`maxDepth`, `maxMessagesPerPoll`, `sortBy`,
//! `idempotent` (+ `idempotentKey`, `idempotentRepository=#ref`), `download`,
//! `ignoreFileNotFoundOrPermissionError`. Consumed files stay on the server unless
//! `delete`, `move` or `noop`/`idempotent` is configured (as in Camel).
//!
//! **Producer**: file name from `CamelOverruleFileName`, `CamelFileName`, the `fileName`
//! option or the message id; `fileExist` (`Override`, `Append`, `Fail`, `Ignore`,
//! `Move` + `moveExisting`, `TryRename`), `tempFileName`/`tempPrefix`,
//! `eagerDeleteTargetFile`, `allowNullBody`, `flatten`, `doneFileName`.
//!
//! **Headers**: `CamelFileName`, `CamelFileNameOnly`, `CamelFileAbsolute`,
//! `CamelFileAbsolutePath`, `CamelFileRelativePath`, `CamelFilePath`, `CamelFileParent`,
//! `CamelFileLength`, `CamelFileLastModified`, `CamelFileNameConsumed`, `CamelFileHost`;
//! the producer sets `CamelFileNameProduced`.

use std::any::Any;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::file::{
    ant_match, evaluate_file_expression, headers, FileExist, FileInfo, IdempotentRepository, MemoryIdempotentRepository, NamePattern,
    GENERIC_FILE_OPERATION_FAILED,
};
use camel_core::component::scheduled::{ScheduledPollConfig, ScheduledPollConsumer};
use camel_core::component::support::process_consumer_exchange;
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{kinds, CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

use super::{camel_error, client_config, yes_no, Authority, HostKeyOptions};
use crate::client::{ClientConfig, SshClient};
use crate::error::SshError;
use crate::sftp::{FileAttrs, SftpClient};

/// Header: host of the remote file.
pub const FILE_HOST: &str = "CamelFileHost";

#[derive(Clone, Debug)]
struct SortKey {
    reverse: bool,
    ignore_case: bool,
    key: String,
}

/// Configuration of an `sftp` endpoint.
#[derive(Clone)]
pub struct SftpConfig {
    pub client: ClientConfig,
    /// Starting directory (relative to the home directory unless it starts with `/`).
    pub directory: String,
    pub disconnect: bool,
    pub send_noop: bool,
    pub maximum_reconnect_attempts: u32,
    pub reconnect_delay: Duration,
    pub throw_exception_on_connect_failed: bool,
    pub auto_create: bool,
    pub charset: Option<String>,
    pub bulk_requests: usize,
    pub chmod: Option<u32>,
    pub chmod_directory: Option<u32>,
    // consumer
    pub noop: bool,
    pub delete: bool,
    pub move_expr: Option<String>,
    pub move_failed: Option<String>,
    pub pre_move: Option<String>,
    pub include: Option<NamePattern>,
    pub exclude: Option<NamePattern>,
    pub include_ext: Vec<String>,
    pub exclude_ext: Vec<String>,
    pub ant_include: Vec<String>,
    pub ant_exclude: Vec<String>,
    pub ant_case_sensitive: bool,
    pub file_name: Option<String>,
    pub recursive: bool,
    pub min_depth: usize,
    pub max_depth: usize,
    pub max_messages_per_poll: usize,
    sort_by: Vec<SortKey>,
    pub idempotent: bool,
    pub idempotent_key: Option<String>,
    pub download: bool,
    pub ignore_not_found: bool,
    pub scheduler: ScheduledPollConfig,
    // producer
    pub file_exist: FileExist,
    pub move_existing: Option<String>,
    pub temp_file_name: Option<String>,
    pub temp_prefix: Option<String>,
    pub eager_delete_target_file: bool,
    pub allow_null_body: bool,
    pub flatten: bool,
    pub done_file_name: Option<String>,
}

fn take_nonempty(p: &mut Parameters, k: &str) -> Option<String> {
    p.take(k).filter(|s| !s.is_empty())
}

fn take_list(p: &mut Parameters, k: &str) -> Vec<String> {
    p.take(k).map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default()
}

fn take_octal(p: &mut Parameters, k: &str) -> Result<Option<u32>> {
    match take_nonempty(p, k) {
        Some(v) => u32::from_str_radix(v.trim_start_matches("0o"), 8).map(Some).map_err(|_| CamelError::illegal_argument(format!("Invalid octal {}: {}", k, v))),
        None => Ok(None),
    }
}

fn parse_sort(s: &str) -> Vec<SortKey> {
    s.split(';')
        .filter(|g| !g.trim().is_empty())
        .map(|g| {
            let mut g = g.trim();
            let (mut reverse, mut ignore_case) = (false, false);
            loop {
                if let Some(r) = g.strip_prefix("reverse:") {
                    reverse = true;
                    g = r;
                } else if let Some(r) = g.strip_prefix("ignoreCase:") {
                    ignore_case = true;
                    g = r;
                } else {
                    break;
                }
            }
            let key = g.trim_start_matches("${").trim_end_matches('}').trim_start_matches("file:").to_string();
            SortKey { reverse, ignore_case, key }
        })
        .collect()
}

/// Options accepted for compatibility with Camel but without effect.
const IGNORED: &[&str] = &[
    "lazyStartProducer",
    "bridgeErrorHandler",
    "exceptionHandler",
    "exchangePattern",
    "synchronous",
    "account",
    "binary",
    "passiveMode",
    "stepwise",
    "useList",
    "fastExistsCheck",
    "existDirCheckUsingLs",
    "streamDownload",
    "localWorkDirectory",
    "transferLoggingLevel",
    "transferLoggingIntervalSeconds",
    "transferLoggingVerbose",
    "handleDirectoryParserAbsoluteResult",
    "separator",
    "resumeDownload",
    "bufferSize",
    "readLockCheckInterval",
    "readLockTimeout",
    "readLockMinLength",
    "readLockMinAge",
    "readLockLoggingLevel",
    "readLockDeleteOrphanLockFiles",
    "readLockMarkerFile",
    "keepLastModified",
    "jailStartingDirectory",
    "eagerMaxMessagesPerPoll",
    "shuffle",
    "processStrategy",
    "inProgressRepository",
    "startingDirectoryMustExist",
    "startingDirectoryMustHaveAccess",
    "directoryMustExist",
    "moveExistingFileStrategy",
    "filter",
    "filterDirectory",
    "filterFile",
    "jschLoggingLevel",
    "serverMessageLoggingLevel",
    "runLoggingLevel",
    "serverAliveInterval",
    "serverAliveCountMax",
    "bindAddress",
    "proxy",
    "keyPair",
    "filenameEncoding",
    "publicKeyAcceptedAlgorithms",
    "compression",
    "timeout",
    "siteCommand",
    "autoCreateDirectory",
    "ignoreFileNotFoundOrPermissionError",
    "allowNullBody",
];

impl SftpConfig {
    /// Parses `remaining` (`[user[:password]@]host[:port]/dir`) and the endpoint options.
    pub fn parse(ctx: &CamelContext, remaining: &str, params: &mut Parameters) -> Result<SftpConfig> {
        let a = Authority::parse(remaining, 22)?;
        let delete = params.take_bool("delete", false)?;
        let move_expr = take_nonempty(params, "move");
        if delete && move_expr.is_some() {
            return Err(CamelError::illegal_argument("You cannot set both delete=true and move options"));
        }
        let noop = params.take_bool("noop", false)?;
        let idempotent = match params.take("idempotent") {
            Some(v) => v.is_empty() || v.eq_ignore_ascii_case("true"),
            None => noop,
        };
        if let Some(rl) = params.take("readLock") {
            if !matches!(rl.as_str(), "none" | "") {
                camel_core::camel_warn!("sftp", "readLock={} is not supported by rust-camel sftp and is ignored", rl);
            }
        }
        let username = params.take("username").or(a.user).unwrap_or_else(|| std::env::var("USER").unwrap_or_else(|_| "anonymous".into()));
        let password = params.take("password").or(a.password);
        let hk = HostKeyOptions {
            known_hosts_file: take_nonempty(params, "knownHostsFile"),
            known_hosts_text: match take_nonempty(params, "knownHostsUri") {
                Some(u) => Some(std::fs::read_to_string(super::file_path(&u)).unwrap_or_default()),
                None => take_nonempty(params, "knownHosts"),
            },
            use_user_known_hosts: params.take_bool("useUserKnownHostsFile", true)?,
            strict: yes_no(&params.take_string("strictHostKeyChecking", "no"))?,
        };
        let mut client = client_config(ctx, &a.host, a.port, username, password, params, hk)?;
        client.timeout = Some(params.take_duration("soTimeout", Duration::from_secs(300))?).filter(|d| !d.is_zero());
        if let Some(c) = params.get("compression").and_then(|c| c.parse::<u32>().ok()) {
            if c > 0 {
                camel_core::camel_warn!("sftp", "compression is not supported by rust-camel sftp and is ignored");
            }
        }
        let include = take_nonempty(params, "include").map(|p| NamePattern::new(&p)).transpose()?;
        let exclude = take_nonempty(params, "exclude").map(|p| NamePattern::new(&p)).transpose()?;
        let scheduler = ScheduledPollConfig::from_params(params, Duration::from_millis(1000), Duration::from_millis(500))?;
        let cfg = SftpConfig {
            client,
            directory: a.path.trim_end_matches('/').to_string(),
            disconnect: params.take_bool("disconnect", false)?,
            send_noop: params.take_bool("sendNoop", true)?,
            maximum_reconnect_attempts: params.take_u64("maximumReconnectAttempts", 3)? as u32,
            reconnect_delay: params.take_duration("reconnectDelay", Duration::from_millis(1000))?,
            throw_exception_on_connect_failed: params.take_bool("throwExceptionOnConnectFailed", false)?,
            auto_create: params.take_bool("autoCreate", true)?,
            charset: take_nonempty(params, "charset"),
            bulk_requests: params.take_usize("bulkRequests", 16)?.max(1),
            chmod: take_octal(params, "chmod")?,
            chmod_directory: take_octal(params, "chmodDirectory")?,
            noop,
            delete,
            move_expr,
            move_failed: take_nonempty(params, "moveFailed"),
            pre_move: take_nonempty(params, "preMove"),
            include,
            exclude,
            include_ext: take_list(params, "includeExt"),
            exclude_ext: take_list(params, "excludeExt"),
            ant_include: take_list(params, "antInclude"),
            ant_exclude: take_list(params, "antExclude"),
            ant_case_sensitive: params.take_bool("antFilterCaseSensitive", true)?,
            file_name: take_nonempty(params, "fileName"),
            recursive: params.take_bool("recursive", false)?,
            min_depth: params.take_usize("minDepth", 0)?,
            max_depth: params.take_usize("maxDepth", usize::MAX)?,
            max_messages_per_poll: params.take_usize("maxMessagesPerPoll", 0)?,
            sort_by: params.take("sortBy").map(|s| parse_sort(&s)).unwrap_or_default(),
            idempotent,
            idempotent_key: take_nonempty(params, "idempotentKey"),
            download: params.take_bool("download", true)?,
            ignore_not_found: params.take_bool("ignoreFileNotFoundOrPermissionError", false)?,
            scheduler,
            file_exist: FileExist::parse(&params.take_string("fileExist", "Override"))?,
            move_existing: take_nonempty(params, "moveExisting"),
            temp_file_name: take_nonempty(params, "tempFileName"),
            temp_prefix: take_nonempty(params, "tempPrefix"),
            eager_delete_target_file: params.take_bool("eagerDeleteTargetFile", true)?,
            allow_null_body: params.take_bool("allowNullBody", false)?,
            flatten: params.take_bool("flatten", false)?,
            done_file_name: take_nonempty(params, "doneFileName"),
        };
        if cfg.file_exist == FileExist::Move && cfg.move_existing.is_none() {
            return Err(CamelError::illegal_argument("You must configure moveExisting option when fileExist=Move"));
        }
        for k in IGNORED {
            params.take(k);
        }
        params.take_prefixed("ftpClient.");
        Ok(cfg)
    }

    fn absolute_dir(&self) -> bool {
        self.directory.starts_with('/')
    }
}

/// Join remote path segments with `/`, resolving `.` and `..`.
pub fn join_path(base: &str, rel: &str) -> String {
    let abs = if rel.starts_with('/') { true } else { base.starts_with('/') };
    let mut segs: Vec<&str> = Vec::new();
    let parts: Vec<&str> = if rel.starts_with('/') { rel.split('/').collect() } else { base.split('/').chain(rel.split('/')).collect() };
    for s in parts {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    let j = segs.join("/");
    if abs {
        format!("/{}", j)
    } else {
        j
    }
}

fn split_parent(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// Path argument for the server: the home directory is `.`.
fn arg(path: &str) -> &str {
    if path.is_empty() {
        "."
    } else {
        path
    }
}

// ---------------------------------------------------------------------------
// connection handling
// ---------------------------------------------------------------------------

/// A remote directory entry.
#[derive(Clone, Debug)]
struct RemoteFile {
    name: String,
    size: u64,
    modified: i64,
    is_dir: bool,
}

struct Session {
    sftp: SftpClient,
}

fn op_err(what: &str) -> impl FnOnce(SshError) -> CamelError + '_ {
    move |e| camel_error(e, what)
}

impl Session {
    fn list(&mut self, dir: &str) -> Result<Vec<RemoteFile>> {
        let entries = self.sftp.read_dir(arg(dir)).map_err(op_err(&format!("Cannot list directory: {}", dir)))?;
        let mut out = Vec::new();
        for e in entries {
            let mut attrs = e.attrs;
            if attrs.is_symlink() {
                // follow links to know whether they point to files or directories
                match self.sftp.stat(&join_path(dir, &e.name)) {
                    Ok(a) => attrs = a,
                    Err(_) => continue,
                }
            }
            out.push(RemoteFile {
                name: e.name,
                size: attrs.size.unwrap_or(0),
                modified: attrs.mtime.map(|m| m as i64 * 1000).unwrap_or(0),
                is_dir: attrs.is_dir(),
            });
        }
        Ok(out)
    }

    fn mkdirs(&mut self, dir: &str, chmod: Option<u32>) -> Result<()> {
        if dir.is_empty() || dir == "/" || dir == "." {
            return Ok(());
        }
        let existed = self.sftp.exists(dir).map_err(op_err(dir))?;
        if existed {
            return Ok(());
        }
        self.sftp.mkdirs(dir).map_err(op_err(&format!("Cannot create directory: {}", dir)))?;
        if let Some(m) = chmod {
            let _ = self.sftp.setstat(dir, &FileAttrs { permissions: Some(m), ..Default::default() });
        }
        Ok(())
    }

    fn exists(&mut self, path: &str) -> Result<bool> {
        self.sftp.exists(path).map_err(op_err(path))
    }

    /// Rename, replacing an existing target (like Camel's SFTP move).
    fn rename(&mut self, from: &str, to: &str, chmod_dir: Option<u32>) -> Result<()> {
        let (parent, _) = split_parent(to);
        self.mkdirs(parent, chmod_dir)?;
        self.sftp.rename_overwrite(from, to).map_err(op_err(&format!("Cannot rename file: {} to: {}", from, to)))
    }
}

struct Connection {
    cfg: Arc<SftpConfig>,
    session: Mutex<Option<Session>>,
    created_dir: Mutex<bool>,
}

impl Connection {
    fn new(cfg: Arc<SftpConfig>) -> Self {
        Connection { cfg, session: Mutex::new(None), created_dir: Mutex::new(false) }
    }

    fn open(&self) -> Result<Session> {
        let c = &self.cfg;
        let mut attempt = 0;
        loop {
            let r = SshClient::connect(&c.client).and_then(SftpClient::new);
            match r {
                Ok(mut sftp) => {
                    sftp.max_outstanding = c.bulk_requests;
                    return Ok(Session { sftp });
                }
                Err(e) => {
                    let fatal_config = matches!(e, SshError::HostKey(_) | SshError::Auth(_) | SshError::Key(_) | SshError::Negotiation(_));
                    attempt += 1;
                    if fatal_config || attempt > c.maximum_reconnect_attempts {
                        let msg = format!("Cannot connect to sftp://{}@{}:{}", c.client.username, c.client.host, c.client.port);
                        let mut err = camel_error(e, &msg);
                        if err.kind == kinds::SOCKET_TIMEOUT || err.kind == GENERIC_FILE_OPERATION_FAILED {
                            err.kind = kinds::IO.to_string();
                        }
                        return Err(err);
                    }
                    std::thread::sleep(c.reconnect_delay.min(Duration::from_secs(60)));
                }
            }
        }
    }

    /// Run `f` with a connected session (reusing a pooled connection when alive).
    fn with<T>(&self, create_dir: bool, f: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
        let mut guard = self.session.lock().unwrap();
        if let Some(s) = guard.as_mut() {
            if self.cfg.send_noop && s.sftp.realpath(".").is_err() {
                *guard = None;
            }
        }
        if guard.is_none() {
            *guard = Some(self.open()?);
        }
        let s = guard.as_mut().unwrap();
        if create_dir && self.cfg.auto_create && !*self.created_dir.lock().unwrap() {
            s.mkdirs(&self.cfg.directory, self.cfg.chmod_directory)?;
            *self.created_dir.lock().unwrap() = true;
        }
        let r = f(s);
        let broken = match &r {
            Err(e) => e.kind == kinds::IO || e.kind == kinds::SOCKET_TIMEOUT || e.kind == kinds::CONNECT,
            Ok(_) => false,
        };
        if broken || self.cfg.disconnect {
            if let Some(s) = guard.take() {
                s.sftp.close();
            }
        }
        r
    }

    fn close(&self) {
        if let Some(s) = self.session.lock().unwrap().take() {
            s.sftp.close();
        }
    }
}

// ---------------------------------------------------------------------------
// component / endpoint
// ---------------------------------------------------------------------------

/// The `sftp` component.
#[derive(Default)]
pub struct SftpComponent;

impl Component for SftpComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let repo: Arc<dyn IdempotentRepository> = match params.take("idempotentRepository") {
            Some(r) => {
                let name = r.trim_start_matches('#').trim_start_matches("bean:");
                let repo = ctx.registry().lookup::<Arc<dyn IdempotentRepository>>(name).ok_or_else(|| CamelError::no_such_bean(name))?;
                (*repo).clone()
            }
            None => Arc::new(MemoryIdempotentRepository::default()),
        };
        let cfg = Arc::new(SftpConfig::parse(ctx, remaining, params)?);
        Ok(Arc::new(SftpEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg, repo, in_progress: Arc::new(Mutex::new(HashSet::new())) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An `sftp` endpoint.
pub struct SftpEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: Arc<SftpConfig>,
    repo: Arc<dyn IdempotentRepository>,
    in_progress: Arc<Mutex<HashSet<String>>>,
}

impl SftpEndpoint {
    pub fn config(&self) -> &SftpConfig {
        &self.cfg
    }
    /// The idempotent repository used by consumers of this endpoint.
    pub fn idempotent_repository(&self) -> Arc<dyn IdempotentRepository> {
        self.repo.clone()
    }
}

impl Endpoint for SftpEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(SftpProducer { conn: Connection::new(self.cfg.clone()) }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let core = Arc::new(ConsumerCore {
            cfg: self.cfg.clone(),
            conn: Connection::new(self.cfg.clone()),
            ctx: self.ctx.clone(),
            repo: self.repo.clone(),
            in_progress: self.in_progress.clone(),
            logger: self.uri.clone(),
        });
        let c2 = core.clone();
        let p2 = processor.clone();
        let mut poll = ScheduledPollConsumer::new(self.uri.clone(), self.cfg.scheduler.clone(), move || c2.poll(p2.as_ref()));
        let (ctx, logger) = (self.ctx.clone(), self.uri.clone());
        poll = poll.with_empty_message(move || {
            let mut ex = Exchange::new(ctx.upgrade());
            process_consumer_exchange(&logger, processor.as_ref(), &mut ex);
        });
        Ok(Arc::new(SftpConsumer { core, poll }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// consumer
// ---------------------------------------------------------------------------

struct ConsumerCore {
    cfg: Arc<SftpConfig>,
    conn: Connection,
    ctx: WeakContext,
    repo: Arc<dyn IdempotentRepository>,
    in_progress: Arc<Mutex<HashSet<String>>>,
    logger: String,
}

struct Candidate {
    rel: String,
    file: RemoteFile,
}

impl ConsumerCore {
    fn host(&self) -> &str {
        &self.cfg.client.host
    }

    fn info(&self, rel: &str, f: &RemoteFile) -> FileInfo {
        let full = join_path(&self.cfg.directory, rel);
        FileInfo {
            relative: rel.to_string(),
            absolute: PathBuf::from(&full),
            path: PathBuf::from(&full),
            endpoint_absolute: self.cfg.absolute_dir(),
            length: f.size,
            modified: f.modified,
        }
    }

    fn eval(&self, text: &str, ex: Option<&Exchange>, info: &FileInfo) -> Result<String> {
        match ex {
            Some(ex) => evaluate_file_expression(text, ex, Some(info)),
            None => {
                let mut ex = Exchange::new(self.ctx.upgrade());
                set_file_headers(&mut ex, info, self.host());
                evaluate_file_expression(text, &ex, Some(info))
            }
        }
    }

    fn accept_name(&self, rel: &str, only: &str) -> bool {
        let c = &self.cfg;
        if only.starts_with('.') {
            return false;
        }
        if let Some(i) = &c.include {
            if !i.matches(only) {
                return false;
            }
        }
        if let Some(e) = &c.exclude {
            if e.matches(only) {
                return false;
            }
        }
        let lower = only.to_ascii_lowercase();
        if !c.include_ext.is_empty() && !c.include_ext.iter().any(|x| lower.ends_with(&format!(".{}", x.to_ascii_lowercase()))) {
            return false;
        }
        if c.exclude_ext.iter().any(|x| lower.ends_with(&format!(".{}", x.to_ascii_lowercase()))) {
            return false;
        }
        if !c.ant_include.is_empty() && !c.ant_include.iter().any(|p| ant_match(p, rel, c.ant_case_sensitive)) {
            return false;
        }
        if c.ant_exclude.iter().any(|p| ant_match(p, rel, c.ant_case_sensitive)) {
            return false;
        }
        true
    }

    fn idempotent_key(&self, info: &FileInfo) -> Result<String> {
        match &self.cfg.idempotent_key {
            Some(k) => self.eval(k, None, info),
            None => Ok(format!("{}:{}{}", self.host(), if info.endpoint_absolute { "" } else { "~/" }, info.absolute.display())),
        }
    }

    fn walk(&self, s: &mut Session, rel_dir: &str, depth: usize, fixed: &Option<String>, out: &mut Vec<Candidate>) -> Result<()> {
        let c = &self.cfg;
        let dir = join_path(&c.directory, rel_dir);
        let mut entries = match s.list(&dir) {
            Ok(e) => e,
            Err(e) if (c.ignore_not_found || depth > 1) && e.kind == GENERIC_FILE_OPERATION_FAILED => {
                camel_core::camel_debug!(&self.logger, "Cannot list {}: {}", dir, e);
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for f in entries {
            let rel = if rel_dir.is_empty() { f.name.clone() } else { format!("{}/{}", rel_dir, f.name) };
            if f.is_dir {
                if c.recursive && depth < c.max_depth && !f.name.starts_with('.') {
                    self.walk(s, &rel, depth + 1, fixed, out)?;
                }
                continue;
            }
            if depth < c.min_depth || depth > c.max_depth {
                continue;
            }
            match fixed {
                Some(name) if &rel != name && &f.name != name => continue,
                Some(_) => {}
                None if !self.accept_name(&rel, &f.name) => continue,
                None => {}
            }
            let info = self.info(&rel, &f);
            if c.idempotent && self.repo.contains(&self.idempotent_key(&info)?) {
                continue;
            }
            if self.in_progress.lock().unwrap().contains(&info.absolute.display().to_string()) {
                continue;
            }
            out.push(Candidate { rel, file: f });
        }
        Ok(())
    }

    fn move_target(&self, expr: &str, ex: Option<&Exchange>, info: &FileInfo) -> Result<String> {
        let text = if expr.contains("${") { expr.to_string() } else { format!("{}/${{file:name}}", expr.trim_end_matches('/')) };
        let name = self.eval(&text, ex, info)?;
        Ok(join_path(&self.cfg.directory, &name))
    }

    fn poll(&self, processor: &dyn Processor) -> Result<usize> {
        let r = self.conn.with(true, |s| self.poll_session(s, processor));
        match r {
            Err(e) if !self.cfg.throw_exception_on_connect_failed && e.message.starts_with("Cannot connect") => {
                camel_core::camel_warn!(&self.logger, "{}", e);
                Ok(0)
            }
            r => r,
        }
    }

    fn poll_session(&self, s: &mut Session, processor: &dyn Processor) -> Result<usize> {
        let c = &self.cfg;
        let fixed = match &c.file_name {
            Some(f) => Some(evaluate_file_expression(f, &Exchange::new(self.ctx.upgrade()), None)?),
            None => None,
        };
        let mut files = Vec::new();
        self.walk(s, "", 1, &fixed, &mut files)?;
        if !c.sort_by.is_empty() {
            files.sort_by(|a, b| {
                for k in &c.sort_by {
                    let ord = match k.key.as_str() {
                        "modified" | "lastModified" => a.file.modified.cmp(&b.file.modified),
                        "size" | "length" => a.file.size.cmp(&b.file.size),
                        "onlyname" => cmp_str(&a.file.name, &b.file.name, k.ignore_case),
                        _ => cmp_str(&a.rel, &b.rel, k.ignore_case),
                    };
                    let ord = if k.reverse { ord.reverse() } else { ord };
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                }
                std::cmp::Ordering::Equal
            });
        }
        if c.max_messages_per_poll > 0 {
            files.truncate(c.max_messages_per_poll);
        }
        let mut n = 0;
        for cand in files {
            let mut info = self.info(&cand.rel, &cand.file);
            let key = info.absolute.display().to_string();
            if !self.in_progress.lock().unwrap().insert(key.clone()) {
                continue;
            }
            let r = self.process_file(s, processor, &mut info);
            self.in_progress.lock().unwrap().remove(&key);
            match r {
                Ok(()) => n += 1,
                Err(e) if c.ignore_not_found && e.kind == GENERIC_FILE_OPERATION_FAILED => {
                    camel_core::camel_debug!(&self.logger, "Ignoring {}: {}", key, e);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }

    fn process_file(&self, s: &mut Session, processor: &dyn Processor, info: &mut FileInfo) -> Result<()> {
        let c = &self.cfg;
        if let Some(pre) = &c.pre_move {
            let target = self.move_target(pre, None, info)?;
            let from = info.absolute.display().to_string();
            s.rename(&from, &target, c.chmod_directory)?;
            info.absolute = PathBuf::from(&target);
            info.path = PathBuf::from(&target);
        }
        let full = info.absolute.display().to_string();
        let mut ex = Exchange::new(self.ctx.upgrade());
        if c.download {
            let data = s.sftp.read_file(&full).map_err(op_err(&format!("Cannot retrieve file: {}", full)))?;
            let body = match c.charset.as_deref().map(|x| x.to_ascii_lowercase()) {
                Some(cs) if cs == "iso-8859-1" || cs == "latin1" => Value::String(data.iter().map(|&b| b as char).collect()),
                Some(_) => Value::String(String::from_utf8_lossy(&data).into_owned()),
                None => Value::Bytes(data),
            };
            ex.set_body(body);
        }
        set_file_headers(&mut ex, info, self.host());
        ex.set_header(headers::FILE_NAME_CONSUMED, info.relative.clone());
        process_consumer_exchange(&self.logger, processor, &mut ex);
        // commit / rollback
        if ex.is_failed() || ex.rollback_only {
            if let Some(mf) = &c.move_failed {
                let target = self.move_target(mf, Some(&ex), info)?;
                s.rename(&full, &target, c.chmod_directory)?;
            } else if c.idempotent {
                self.repo.remove(&self.idempotent_key(info)?);
            }
            return Ok(());
        }
        if c.idempotent {
            self.repo.add(&self.idempotent_key(info)?);
        }
        if c.delete {
            s.sftp.remove(&full).map_err(op_err(&format!("Cannot delete file: {}", full)))?;
        } else if let (Some(m), false) = (&c.move_expr, c.noop) {
            let target = self.move_target(m, Some(&ex), info)?;
            if target != full {
                s.rename(&full, &target, c.chmod_directory)?;
            }
        }
        Ok(())
    }
}

fn cmp_str(a: &str, b: &str, ignore_case: bool) -> std::cmp::Ordering {
    if ignore_case {
        a.to_lowercase().cmp(&b.to_lowercase())
    } else {
        a.cmp(b)
    }
}

fn set_file_headers(ex: &mut Exchange, info: &FileInfo, host: &str) {
    ex.set_header(headers::FILE_NAME, info.relative.clone());
    ex.set_header(headers::FILE_NAME_ONLY, info.only_name());
    ex.set_header(headers::FILE_ABSOLUTE, info.endpoint_absolute);
    ex.set_header(headers::FILE_ABSOLUTE_PATH, info.absolute.display().to_string());
    ex.set_header(headers::FILE_RELATIVE_PATH, info.relative.clone());
    ex.set_header(headers::FILE_PATH, info.path.display().to_string());
    ex.set_header(headers::FILE_PARENT, info.parent());
    ex.set_header(headers::FILE_LENGTH, Value::Int(info.length as i64));
    ex.set_header(headers::FILE_LAST_MODIFIED, Value::Int(info.modified));
    ex.set_header(FILE_HOST, host.to_string());
}

/// The scheduled SFTP consumer.
pub struct SftpConsumer {
    core: Arc<ConsumerCore>,
    poll: ScheduledPollConsumer,
}

impl SftpConsumer {
    /// Run one poll now; returns the number of processed files.
    pub fn poll_once(&self) -> Result<usize> {
        self.poll.poll_once()
    }
}

impl Service for SftpConsumer {
    fn start(&self) -> Result<()> {
        self.poll.start()
    }
    fn stop(&self) -> Result<()> {
        self.poll.stop()?;
        self.core.conn.close();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// producer
// ---------------------------------------------------------------------------

struct SftpProducer {
    conn: Connection,
}

impl Service for SftpProducer {
    fn stop(&self) -> Result<()> {
        self.conn.close();
        Ok(())
    }
}

impl SftpProducer {
    fn file_name(&self, ex: &Exchange) -> Result<String> {
        let c = &self.conn.cfg;
        let mut name = if let Some(v) = ex.header(headers::OVERRULE_FILE_NAME).filter(|v| !v.is_null()) {
            evaluate_file_expression(&v.to_string(), ex, None)?
        } else if let Some(v) = ex.header(headers::FILE_NAME).filter(|v| !v.is_null()) {
            v.to_string()
        } else if let Some(f) = &c.file_name {
            evaluate_file_expression(f, ex, None)?
        } else {
            ex.message.message_id.clone()
        };
        if c.flatten {
            name = name.rsplit('/').next().unwrap_or("").to_string();
        }
        if name.is_empty() || name.ends_with('/') {
            return Err(CamelError::illegal_argument(format!("Cannot write to a directory name: {}", name)));
        }
        Ok(name)
    }
}

fn gfo(msg: String) -> CamelError {
    CamelError::new(GENERIC_FILE_OPERATION_FAILED, msg)
}

impl Processor for SftpProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let c = self.conn.cfg.clone();
        let name = self.file_name(ex)?;
        let target = join_path(&c.directory, &name);
        let data: Vec<u8> = match ex.body() {
            Value::Null if !c.allow_null_body => return Err(gfo(format!("Cannot write null body to file: {}", target))),
            Value::Null => Vec::new(),
            Value::String(s) if matches!(c.charset.as_deref().map(|x| x.to_ascii_lowercase()).as_deref(), Some("iso-8859-1") | Some("latin1")) => {
                s.chars().map(|ch| if (ch as u32) < 256 { ch as u8 } else { b'?' }).collect()
            }
            other => other.to_bytes(),
        };
        let mut info_ex = ex.clone();
        info_ex.set_header(headers::FILE_NAME, name.clone());
        let produced = self.conn.with(true, |s| -> Result<String> {
            let (parent, only) = split_parent(&target);
            let (parent, only) = (parent.to_string(), only.to_string());
            if c.auto_create {
                s.mkdirs(&parent, c.chmod_directory)?;
            }
            let exists = s.exists(&target)?;
            let mut append = false;
            if exists {
                match c.file_exist {
                    FileExist::Fail => return Err(gfo(format!("File already exist: {}. Cannot write new file.", target))),
                    FileExist::Ignore => return Ok(target.clone()),
                    FileExist::Append => append = true,
                    FileExist::Move => {
                        let expr = c.move_existing.clone().unwrap_or_default();
                        let text = if expr.contains("${") { expr } else { format!("{}/${{file:name}}", expr.trim_end_matches('/')) };
                        let dest = join_path(&c.directory, &evaluate_file_expression(&text, &info_ex, None)?);
                        s.rename(&target, &dest, c.chmod_directory)?;
                    }
                    FileExist::Override | FileExist::TryRename => {}
                }
            }
            let temp = match (&c.temp_file_name, &c.temp_prefix) {
                (Some(t), _) => Some(evaluate_file_expression(t, &info_ex, None)?),
                (None, Some(p)) => Some(format!("{}{}", p, only)),
                _ => None,
            };
            let still_exists = exists && c.file_exist != FileExist::Move;
            match temp {
                Some(t) if !append => {
                    // the temporary file lives in the same directory as the target
                    let tmp = join_path(&parent, &t);
                    s.sftp.write_file(&tmp, &data, false).map_err(op_err(&format!("Cannot write file: {}", tmp)))?;
                    if still_exists && c.file_exist == FileExist::TryRename {
                        s.sftp.rename(&tmp, &target).map_err(op_err(&format!("Cannot rename file: {} to: {}", tmp, target)))?;
                    } else {
                        s.sftp.rename_overwrite(&tmp, &target).map_err(op_err(&format!("Cannot rename file: {} to: {}", tmp, target)))?;
                    }
                }
                _ => s.sftp.write_file(&target, &data, append).map_err(op_err(&format!("Cannot write file: {}", target)))?,
            }
            if let Some(m) = c.chmod {
                s.sftp.setstat(&target, &FileAttrs { permissions: Some(m), ..Default::default() }).map_err(op_err(&format!("Cannot chmod file: {}", target)))?;
            }
            if let Some(d) = &c.done_file_name {
                let done = evaluate_file_expression(d, &info_ex, None)?;
                let done_full = join_path(&parent, &done);
                s.sftp.write_file(&done_full, b"", false).map_err(op_err(&format!("Cannot write done file: {}", done_full)))?;
            }
            Ok(target.clone())
        })?;
        ex.set_header(headers::FILE_NAME_PRODUCED, produced);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(kv: &[(&str, &str)]) -> Parameters {
        Parameters::new(kv.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect())
    }

    #[test]
    fn config_parsing() {
        let ctx = CamelContext::new();
        let mut p = params(&[("password", "secret"), ("noop", "true"), ("delay", "100"), ("strictHostKeyChecking", "no"), ("useUserKnownHostsFile", "false"), ("chmod", "640"), ("bulkRequests", "4")]);
        let c = SftpConfig::parse(&ctx, "admin@localhost:2222/inbox/", &mut p).unwrap();
        assert!(p.is_empty(), "{:?}", p.as_map());
        assert_eq!((c.client.username.as_str(), c.client.password.as_deref(), c.client.port, c.directory.as_str()), ("admin", Some("secret"), 2222, "inbox"));
        assert!(c.noop && c.idempotent);
        assert_eq!((c.chmod, c.bulk_requests), (Some(0o640), 4));
        assert!(matches!(c.client.host_key_check, crate::client::HostKeyCheck::AcceptAny));
        let mut p = params(&[("delete", "true"), ("move", "done")]);
        assert!(SftpConfig::parse(&ctx, "h/x", &mut p).is_err());
        let mut p = params(&[("strictHostKeyChecking", "yes"), ("useUserKnownHostsFile", "false")]);
        let c = SftpConfig::parse(&ctx, "u:p%40ss@h//abs/dir", &mut p).unwrap();
        assert_eq!(c.client.password.as_deref(), Some("p@ss"));
        assert_eq!(c.directory, "/abs/dir");
        assert!(matches!(c.client.host_key_check, crate::client::HostKeyCheck::KnownHosts { strict: true, .. }));
        let mut p = params(&[("ciphers", "des-cbc")]);
        assert!(SftpConfig::parse(&ctx, "h/x", &mut p).is_err());
        assert_eq!(join_path("/abs/in", "../done/a.txt"), "/abs/done/a.txt");
        assert_eq!(split_parent("a/b/c"), ("a/b", "c"));
    }
}
