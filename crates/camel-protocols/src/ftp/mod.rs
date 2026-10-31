//! `ftp:` component (Camel `camel-ftp`, plain FTP only).
//!
//! `ftp://[username@]host[:port]/directoryname[?options]` — the directory is relative to
//! the login directory (use `//abs/dir` for an absolute directory).
//!
//! **Connection options**: `username`, `password`, `account` (accepted), `passiveMode`
//! (false = active mode via `PORT`/`EPRT`), `binary` (false = `TYPE A` with CRLF
//! conversion), `stepwise` (true: `CWD` into each directory and use plain names),
//! `disconnect` (close the connection after each poll / send), `connectTimeout` (10s),
//! `soTimeout` (5 min), `timeout` (data timeout, 30s), `sendNoop` (true: check pooled
//! connections with `NOOP`), `maximumReconnectAttempts` (3), `reconnectDelay` (1s),
//! `throwExceptionOnConnectFailed`, `siteCommand` (`SITE` commands after login,
//! newline separated), `autoCreate` (true: create the directory), `charset`
//! (consumer: body as text; producer: encoding of string bodies).
//!
//! **Consumer** (scheduled poll, `initialDelay` 1000ms, `delay` 500ms and all scheduler
//! options): `noop`, `delete`, `move`, `moveFailed`, `preMove`, `include`/`exclude`
//! (regex), `includeExt`/`excludeExt`, `antInclude`/`antExclude`, `antFilterCaseSensitive`,
//! `fileName`, `recursive`, `minDepth`/`maxDepth`, `maxMessagesPerPoll`, `sortBy`
//! (`file:name`/`file:modified`/`file:size`, `reverse:`/`ignoreCase:` prefixes),
//! `idempotent` (+ `idempotentKey`, `idempotentRepository=#ref`), `download` (false = no
//! body), `ignoreFileNotFoundOrPermissionError`. As in Camel, consumed files are left
//! untouched on the server unless `delete`, `move` or `noop`/`idempotent` is configured.
//!
//! **Producer**: file name from header `CamelOverruleFileName`, `CamelFileName`, the
//! `fileName` option (file language) or the message id; `fileExist`
//! (`Override`, `Append`, `Fail`, `Ignore`, `Move` + `moveExisting`, `TryRename`),
//! `tempFileName`/`tempPrefix` (upload to a temporary name then rename),
//! `eagerDeleteTargetFile`, `allowNullBody`, `flatten`, `doneFileName`.
//!
//! **Headers** (as the file component): `CamelFileName`, `CamelFileNameOnly`,
//! `CamelFileAbsolute`, `CamelFileAbsolutePath`, `CamelFileRelativePath`, `CamelFilePath`,
//! `CamelFileParent`, `CamelFileLength`, `CamelFileLastModified`, `CamelFileNameConsumed`,
//! `CamelFileHost`, `CamelFtpReplyCode`, `CamelFtpReplyString`; the producer sets
//! `CamelFileNameProduced`.
//!
//! `ftps:` and `sftp:` are not supported (no TLS / SSH in a std-only build).

pub mod client;
pub mod listing;

use std::any::Any;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::file::{
    ant_match, evaluate_file_expression, headers, FileExist, FileInfo, IdempotentRepository, MemoryIdempotentRepository, NamePattern,
};
use camel_core::component::scheduled::{ScheduledPollConfig, ScheduledPollConsumer};
use camel_core::component::support::process_consumer_exchange;
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{kinds, CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

pub use client::{ftp_error, FtpClient, FtpReply};
pub use listing::RemoteFile;

use crate::common::{take_ignored, Authority, COMMON_IGNORED};

/// Header: host of the remote file.
pub const FILE_HOST: &str = "CamelFileHost";
/// Header: last FTP reply code.
pub const FTP_REPLY_CODE: &str = "CamelFtpReplyCode";
/// Header: last FTP reply text.
pub const FTP_REPLY_STRING: &str = "CamelFtpReplyString";

#[derive(Clone, Debug)]
struct SortKey {
    reverse: bool,
    ignore_case: bool,
    key: String,
}

/// Configuration of an `ftp` endpoint.
#[derive(Clone)]
pub struct FtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// Starting directory (relative to the login directory unless it starts with `/`).
    pub directory: String,
    pub passive_mode: bool,
    pub binary: bool,
    pub stepwise: bool,
    pub disconnect: bool,
    pub connect_timeout: Duration,
    pub so_timeout: Duration,
    pub data_timeout: Duration,
    pub send_noop: bool,
    pub maximum_reconnect_attempts: u32,
    pub reconnect_delay: Duration,
    pub throw_exception_on_connect_failed: bool,
    pub site_commands: Vec<String>,
    pub auto_create: bool,
    pub charset: Option<String>,
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

impl FtpConfig {
    /// Parse `remaining` (`[user[:password]@]host[:port]/dir`) and the endpoint options.
    pub fn parse(remaining: &str, params: &mut Parameters) -> Result<FtpConfig> {
        let a = Authority::parse(remaining, 21)?;
        if a.host.is_empty() {
            return Err(CamelError::illegal_argument(format!("Hostname must be configured: ftp://{}", remaining)));
        }
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
        let sort_by = params
            .take("sortBy")
            .map(|s| {
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
            })
            .unwrap_or_default();
        if let Some(rl) = params.take("readLock") {
            if !matches!(rl.as_str(), "none" | "") {
                camel_core::camel_warn!("ftp", "readLock={} is not supported by rust-camel ftp and is ignored", rl);
            }
        }
        let username = params.take("username").or(a.user).unwrap_or_else(|| "anonymous".to_string());
        let password = params.take("password").or(a.password).unwrap_or_default();
        let include = take_nonempty(params, "include").map(|p| NamePattern::new(&p)).transpose()?;
        let exclude = take_nonempty(params, "exclude").map(|p| NamePattern::new(&p)).transpose()?;
        let scheduler = ScheduledPollConfig::from_params(params, Duration::from_millis(1000), Duration::from_millis(500))?;
        let cfg = FtpConfig {
            host: a.host,
            port: a.port,
            username,
            password,
            directory: a.path.trim_end_matches('/').to_string(),
            passive_mode: params.take_bool("passiveMode", false)?,
            binary: params.take_bool("binary", false)?,
            stepwise: params.take_bool("stepwise", true)?,
            disconnect: params.take_bool("disconnect", false)?,
            connect_timeout: params.take_duration("connectTimeout", Duration::from_secs(10))?,
            so_timeout: params.take_duration("soTimeout", Duration::from_secs(300))?,
            data_timeout: params.take_duration("timeout", Duration::from_secs(30))?,
            send_noop: params.take_bool("sendNoop", true)?,
            maximum_reconnect_attempts: params.take_u64("maximumReconnectAttempts", 3)? as u32,
            reconnect_delay: params.take_duration("reconnectDelay", Duration::from_millis(1000))?,
            throw_exception_on_connect_failed: params.take_bool("throwExceptionOnConnectFailed", false)?,
            site_commands: params.take("siteCommand").map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()).unwrap_or_default(),
            auto_create: params.take_bool("autoCreate", true)?,
            charset: take_nonempty(params, "charset"),
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
            sort_by,
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
        take_ignored(params, COMMON_IGNORED);
        take_ignored(
            params,
            &[
                "account",
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
                "chmod",
                "resumeDownload",
                "activePortRange",
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
                "dataTimeout",
                "ignoreFileNotFoundOrPermissionError",
                "allowNullBody",
                "moveExistingFileStrategy",
                "sendEmptyMessageWhenIdle",
                "filter",
                "filterDirectory",
                "filterFile",
            ],
        );
        params.take_prefixed("ftpClient.");
        params.take_prefixed("ftpClientConfig.");
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

// ---------------------------------------------------------------------------
// connection handling
// ---------------------------------------------------------------------------

struct Session {
    client: FtpClient,
    home: String,
}

impl Session {
    /// Prepare an operation on `path`: in stepwise mode change into the parent directory
    /// and return the plain name, otherwise return the path unchanged.
    fn target(&mut self, stepwise: bool, path: &str) -> Result<String> {
        if !stepwise {
            return Ok(path.to_string());
        }
        let (dir, name) = split_parent(path);
        self.client.cwd(&self.home)?;
        if dir.starts_with('/') {
            self.client.cwd("/")?;
        }
        for seg in dir.split('/').filter(|s| !s.is_empty()) {
            self.client.cwd(seg)?;
        }
        Ok(name.to_string())
    }

    fn list(&mut self, stepwise: bool, dir: &str) -> Result<Vec<RemoteFile>> {
        if stepwise {
            self.client.cwd(&self.home)?;
            if dir.starts_with('/') {
                self.client.cwd("/")?;
            }
            for seg in dir.split('/').filter(|s| !s.is_empty()) {
                self.client.cwd(seg)?;
            }
            self.client.list("")
        } else {
            self.client.list(dir)
        }
    }

    /// Change to the login directory and create `dir` (relative to it) with its parents.
    fn mkdirs(&mut self, dir: &str) -> Result<()> {
        self.client.cwd(&self.home)?;
        if dir.is_empty() || dir == "/" {
            return Ok(());
        }
        self.client.mkdirs(dir)
    }
}

struct Connection {
    cfg: Arc<FtpConfig>,
    session: Mutex<Option<Session>>,
    created_dir: Mutex<bool>,
}

impl Connection {
    fn new(cfg: Arc<FtpConfig>) -> Self {
        Connection { cfg, session: Mutex::new(None), created_dir: Mutex::new(false) }
    }

    fn open(&self) -> Result<Session> {
        let c = &self.cfg;
        let mut attempt = 0;
        loop {
            let r = (|| -> Result<Session> {
                let mut client = FtpClient::connect(&c.host, c.port, c.connect_timeout, c.so_timeout)?;
                client.set_passive(c.passive_mode);
                client.set_data_timeout(c.data_timeout);
                client.login(&c.username, &c.password)?;
                client.set_binary(c.binary)?;
                for s in &c.site_commands {
                    client.send_command(&format!("SITE {}", s))?;
                }
                let home = client.pwd().unwrap_or_else(|_| "/".to_string());
                Ok(Session { client, home })
            })();
            match r {
                Ok(s) => return Ok(s),
                Err(e) => {
                    attempt += 1;
                    if attempt > c.maximum_reconnect_attempts {
                        return Err(e);
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
            if self.cfg.send_noop && !s.client.noop() {
                *guard = None;
            }
        }
        if guard.is_none() {
            *guard = Some(self.open()?);
        }
        let s = guard.as_mut().unwrap();
        if create_dir && self.cfg.auto_create && !*self.created_dir.lock().unwrap() {
            s.mkdirs(&self.cfg.directory)?;
            *self.created_dir.lock().unwrap() = true;
        }
        let r = f(s);
        let broken = match &r {
            Err(e) => e.kind == kinds::IO || e.kind == kinds::SOCKET_TIMEOUT || e.kind == kinds::CONNECT || e.message.contains("Connection closed"),
            Ok(_) => false,
        };
        if broken || self.cfg.disconnect {
            if let Some(s) = guard.take() {
                s.client.quit();
            }
        }
        r
    }

    fn close(&self) {
        if let Some(s) = self.session.lock().unwrap().take() {
            s.client.quit();
        }
    }
}

// ---------------------------------------------------------------------------
// component / endpoint
// ---------------------------------------------------------------------------

/// The `ftp` component.
#[derive(Default)]
pub struct FtpComponent;

impl Component for FtpComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let repo: Arc<dyn IdempotentRepository> = match params.take("idempotentRepository") {
            Some(r) => {
                let name = r.trim_start_matches('#').trim_start_matches("bean:");
                let repo = ctx.registry().lookup::<Arc<dyn IdempotentRepository>>(name).ok_or_else(|| CamelError::no_such_bean(name))?;
                (*repo).clone()
            }
            None => Arc::new(MemoryIdempotentRepository::default()),
        };
        let cfg = Arc::new(FtpConfig::parse(remaining, params)?);
        Ok(Arc::new(FtpEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg, repo, in_progress: Arc::new(Mutex::new(HashSet::new())) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An `ftp` endpoint.
pub struct FtpEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: Arc<FtpConfig>,
    repo: Arc<dyn IdempotentRepository>,
    in_progress: Arc<Mutex<HashSet<String>>>,
}

impl FtpEndpoint {
    pub fn config(&self) -> &FtpConfig {
        &self.cfg
    }
    /// The idempotent repository used by consumers of this endpoint.
    pub fn idempotent_repository(&self) -> Arc<dyn IdempotentRepository> {
        self.repo.clone()
    }
}

impl Endpoint for FtpEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(FtpProducer { conn: Connection::new(self.cfg.clone()) }))
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
        Ok(Arc::new(FtpConsumer { core, poll }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// consumer
// ---------------------------------------------------------------------------

struct ConsumerCore {
    cfg: Arc<FtpConfig>,
    conn: Connection,
    ctx: WeakContext,
    repo: Arc<dyn IdempotentRepository>,
    in_progress: Arc<Mutex<HashSet<String>>>,
    logger: String,
}

/// A candidate remote file: path relative to the starting directory plus listing info.
struct Candidate {
    rel: String,
    file: RemoteFile,
}

impl ConsumerCore {
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
                set_file_headers(&mut ex, info, &self.cfg.host);
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
            None => Ok(format!("{}:{}{}", self.cfg.host, if info.endpoint_absolute { "" } else { "~/" }, info.absolute.display())),
        }
    }

    fn walk(&self, s: &mut Session, rel_dir: &str, depth: usize, fixed: &Option<String>, out: &mut Vec<Candidate>) -> Result<()> {
        let c = &self.cfg;
        let dir = join_path(&c.directory, rel_dir);
        let mut entries = match s.list(c.stepwise, &dir) {
            Ok(e) => e,
            Err(e) if c.ignore_not_found || depth > 1 => {
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

    fn rename(&self, s: &mut Session, from: &str, to: &str) -> Result<()> {
        let (parent, _) = split_parent(to);
        // full paths are relative to the login directory (mkdirs changes to it)
        s.mkdirs(parent)?;
        // remove an existing target first (like Camel's FTP move)
        if s.client.size(to)?.is_some() {
            let _ = s.client.dele(to);
        }
        s.client.rename(from, to)
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
                Ok(true) => n += 1,
                Ok(false) => {}
                Err(e) if c.ignore_not_found && e.kind == camel_core::component::file::GENERIC_FILE_OPERATION_FAILED && e.message.contains(" 55") => {
                    camel_core::camel_debug!(&self.logger, "Ignoring {}: {}", key, e);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }

    fn process_file(&self, s: &mut Session, processor: &dyn Processor, info: &mut FileInfo) -> Result<bool> {
        let c = &self.cfg;
        let stepwise = c.stepwise;
        if let Some(pre) = &c.pre_move {
            let target = self.move_target(pre, None, info)?;
            let from = info.absolute.display().to_string();
            self.rename(s, &from, &target)?;
            info.absolute = PathBuf::from(&target);
            info.path = PathBuf::from(&target);
        }
        let full = info.absolute.display().to_string();
        let mut ex = Exchange::new(self.ctx.upgrade());
        if c.download {
            let arg = s.target(stepwise, &full)?;
            let data = s.client.retr(&arg)?;
            let body = match c.charset.as_deref().map(|x| x.to_ascii_lowercase()) {
                Some(cs) if cs == "iso-8859-1" || cs == "latin1" => Value::String(data.iter().map(|&b| b as char).collect()),
                Some(_) => Value::String(String::from_utf8_lossy(&data).into_owned()),
                None => Value::Bytes(data),
            };
            ex.set_body(body);
        }
        set_file_headers(&mut ex, info, &c.host);
        ex.set_header(headers::FILE_NAME_CONSUMED, info.relative.clone());
        let last = s.client.last_reply().clone();
        ex.set_header(FTP_REPLY_CODE, Value::Int(last.code as i64));
        ex.set_header(FTP_REPLY_STRING, last.text);
        process_consumer_exchange(&self.logger, processor, &mut ex);
        // commit / rollback
        if ex.is_failed() || ex.rollback_only {
            if let Some(mf) = &c.move_failed {
                let target = self.move_target(mf, Some(&ex), info)?;
                self.rename(s, &full, &target)?;
            } else if c.idempotent {
                self.repo.remove(&self.idempotent_key(info)?);
            }
            return Ok(true);
        }
        if c.idempotent {
            self.repo.add(&self.idempotent_key(info)?);
        }
        if c.delete {
            let arg = s.target(stepwise, &full)?;
            s.client.dele(&arg)?;
        } else if let (Some(m), false) = (&c.move_expr, c.noop) {
            let target = self.move_target(m, Some(&ex), info)?;
            if target != full {
                self.rename(s, &full, &target)?;
            }
        }
        Ok(true)
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

/// The scheduled FTP consumer.
pub struct FtpConsumer {
    core: Arc<ConsumerCore>,
    poll: ScheduledPollConsumer,
}

impl FtpConsumer {
    /// Run one poll now; returns the number of processed files.
    pub fn poll_once(&self) -> Result<usize> {
        self.poll.poll_once()
    }
}

impl Service for FtpConsumer {
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

struct FtpProducer {
    conn: Connection,
}

impl Service for FtpProducer {
    fn stop(&self) -> Result<()> {
        self.conn.close();
        Ok(())
    }
}

impl FtpProducer {
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

impl Processor for FtpProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let c = self.conn.cfg.clone();
        let name = self.file_name(ex)?;
        let target = join_path(&c.directory, &name);
        let data: Vec<u8> = match ex.body() {
            Value::Null if !c.allow_null_body => {
                return Err(ftp_error(format!("Cannot write null body to file: {}", target)));
            }
            Value::Null => Vec::new(),
            Value::String(s) if matches!(c.charset.as_deref().map(|x| x.to_ascii_lowercase()).as_deref(), Some("iso-8859-1") | Some("latin1")) => {
                s.chars().map(|ch| if (ch as u32) < 256 { ch as u8 } else { b'?' }).collect()
            }
            other => other.to_bytes(),
        };
        let mut info_ex = ex.clone();
        info_ex.set_header(headers::FILE_NAME, name.clone());
        let (reply, produced) = self.conn.with(true, |s| -> Result<(Option<FtpReply>, String)> {
            let (parent, only) = split_parent(&target);
            let (parent, only) = (parent.to_string(), only.to_string());
            if c.auto_create {
                s.mkdirs(&parent)?;
            }
            let arg = s.target(c.stepwise, &target)?;
            let exists = s.client.exists(&arg)?;
            let mut append = false;
            if exists {
                match c.file_exist {
                    FileExist::Fail => {
                        return Err(ftp_error(format!("File already exist: {}. Cannot write new file.", target)));
                    }
                    FileExist::Ignore => return Ok((None, target.clone())),
                    FileExist::Append => append = true,
                    FileExist::Move => {
                        let expr = c.move_existing.clone().unwrap_or_default();
                        let text = if expr.contains("${") { expr } else { format!("{}/${{file:name}}", expr.trim_end_matches('/')) };
                        let dest = join_path(&c.directory, &evaluate_file_expression(&text, &info_ex, None)?);
                        // full paths are relative to the login directory (mkdirs changes to it)
                        s.mkdirs(split_parent(&dest).0)?;
                        if s.client.size(&dest)?.is_some() {
                            let _ = s.client.dele(&dest);
                        }
                        s.client.rename(&target, &dest)?;
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
                    let tmp_arg = s.target(c.stepwise, &join_path(&parent, &t))?;
                    let arg = s.target(c.stepwise, &target)?;
                    s.client.stor(&tmp_arg, &data, false)?;
                    if still_exists && c.file_exist != FileExist::TryRename {
                        let _ = s.client.dele(&arg);
                    }
                    s.client.rename(&tmp_arg, &arg)?;
                }
                _ => {
                    let arg = s.target(c.stepwise, &target)?;
                    s.client.stor(&arg, &data, append)?
                }
            }
            let reply = s.client.last_reply().clone();
            if let Some(d) = &c.done_file_name {
                let done = evaluate_file_expression(d, &info_ex, None)?;
                let done_full = join_path(&parent, &done);
                let done_arg = s.target(c.stepwise, &done_full)?;
                s.client.stor(&done_arg, b"", false)?;
            }
            Ok((Some(reply), target.clone()))
        })?;
        if let Some(r) = reply {
            ex.set_header(FTP_REPLY_CODE, Value::Int(r.code as i64));
            ex.set_header(FTP_REPLY_STRING, r.text);
        }
        ex.set_header(headers::FILE_NAME_PRODUCED, produced);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(join_path("in", "a.txt"), "in/a.txt");
        assert_eq!(join_path("", "a.txt"), "a.txt");
        assert_eq!(join_path("/abs/in", "../done/a.txt"), "/abs/done/a.txt");
        assert_eq!(join_path("in", "/x/y"), "/x/y");
        assert_eq!(split_parent("/a"), ("/", "a"));
        assert_eq!(split_parent("a/b/c"), ("a/b", "c"));
    }

    #[test]
    fn config() {
        let mut p = Parameters::new(
            [("password", "secret"), ("passiveMode", "true"), ("binary", "true"), ("noop", "true"), ("delay", "100")]
                .into_iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        );
        let c = FtpConfig::parse("admin@localhost:2121/inbox/", &mut p).unwrap();
        assert!(p.is_empty());
        assert_eq!((c.username.as_str(), c.password.as_str(), c.port, c.directory.as_str()), ("admin", "secret", 2121, "inbox"));
        assert!(c.passive_mode && c.binary && c.noop && c.idempotent);
        let mut p = Parameters::new([("delete", "true"), ("move", "done")].into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect());
        assert!(FtpConfig::parse("h/x", &mut p).is_err());
    }
}
