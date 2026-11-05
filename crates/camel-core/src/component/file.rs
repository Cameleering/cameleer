//! `file:` component - reads and writes files (Camel file component).
//!
//! **Consumer** (`from("file:inbox?...")`), a scheduled poll consumer (`initialDelay`
//! default 1000ms, `delay` default 500ms plus all [`ScheduledPollConfig`] options):
//! `noop`, `delete`, `move` (default `.camel`), `moveFailed`, `preMove`, `include`/`exclude`
//! (regex on the file name), `includeExt`/`excludeExt`, `antInclude`/`antExclude`
//! (ant paths on the relative path, `antFilterCaseSensitive`), `fileName` (only consume
//! this file), `recursive`, `minDepth`/`maxDepth`, `maxMessagesPerPoll`,
//! `eagerMaxMessagesPerPoll`, `sortBy` (`file:name`, `file:modified`, `file:size`, with
//! `reverse:`/`ignoreCase:` prefixes, groups separated by `;`), `shuffle`,
//! `readLock` (`none`, `markerFile`, `changed`, `fileLock` (marker based), `rename`,
//! `idempotent*`), `readLockCheckInterval`, `readLockTimeout`, `readLockMinLength`,
//! `readLockMinAge`, `readLockMarkerFile`, `readLockDeleteOrphanLockFiles`,
//! `idempotent` (+ `idempotentKey`, `idempotentRepository=#ref`), `inProgressRepository`,
//! `doneFileName`, `charset`, `startingDirectoryMustExist`, `directoryMustExist`,
//! `autoCreate`, `includeHiddenFiles`, `includeHiddenDirs`, `filter=#ref`
//! ([`GenericFileFilter`]).
//!
//! **Producer**: `fileExist` (`Override`, `Append`, `Fail`, `Ignore`, `Move`, `TryRename`),
//! `moveExisting`, `tempFileName`, `tempPrefix`, `autoCreate`, `fileName`, `charset`,
//! `appendChars`, `doneFileName`, `keepLastModified`, `flatten`, `allowNullBody`,
//! `eagerDeleteTargetFile`, `jailStartingDirectory`.
//!
//! Expressions (`move`, `fileName`, `doneFileName`, ...) support the file language
//! tokens `${file:name}`, `${file:name.noext}`, `${file:name.ext}`, `${file:onlyname}`,
//! `${file:onlyname.noext}`, `${file:ext}`, `${file:parent}`, `${file:path}`,
//! `${file:absolute}`, `${file:absolute.path}`, `${file:length}`, `${file:size}`,
//! `${file:modified}`, `${date:now:FORMAT}`, `${date:file:FORMAT}`, `${exchangeId}`,
//! `${id}`, `${camelId}`; any other `${...}` is evaluated by the Simple language.
//!
//! The body of consumed files is `Value::Bytes` with the file content.

use std::any::Any;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Component as PathComponent, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::context::{CamelContext, WeakContext};
use crate::error::{kinds, CamelError, Result};
use crate::message::{props, Exchange};
use crate::spi::{Component, Consumer, Endpoint, PollingConsumer, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::Value;

use super::scheduled::{ScheduledPollConfig, ScheduledPollConsumer};
use super::support::{new_exchange, process_consumer_exchange, StopFlag};

/// Header names set by the file component.
pub mod headers {
    pub const FILE_NAME: &str = "CamelFileName";
    pub const FILE_NAME_ONLY: &str = "CamelFileNameOnly";
    pub const FILE_ABSOLUTE: &str = "CamelFileAbsolute";
    pub const FILE_ABSOLUTE_PATH: &str = "CamelFileAbsolutePath";
    pub const FILE_RELATIVE_PATH: &str = "CamelFileRelativePath";
    pub const FILE_PATH: &str = "CamelFilePath";
    pub const FILE_PARENT: &str = "CamelFileParent";
    pub const FILE_LENGTH: &str = "CamelFileLength";
    pub const FILE_LAST_MODIFIED: &str = "CamelFileLastModified";
    pub const FILE_NAME_PRODUCED: &str = "CamelFileNameProduced";
    pub const FILE_NAME_CONSUMED: &str = "CamelFileNameConsumed";
    pub const OVERRULE_FILE_NAME: &str = "CamelOverruleFileName";
}

/// Exception kind for file operation failures.
pub const GENERIC_FILE_OPERATION_FAILED: &str = "GenericFileOperationFailedException";

fn file_error(msg: impl Into<String>) -> CamelError {
    CamelError::new(GENERIC_FILE_OPERATION_FAILED, msg)
}

fn io_error(what: &str, path: &Path, e: std::io::Error) -> CamelError {
    let kind = if e.kind() == std::io::ErrorKind::NotFound { kinds::FILE_NOT_FOUND } else { kinds::IO };
    CamelError::new(kind, format!("{} {}: {}", what, path.display(), e))
}

// ---------------------------------------------------------------------------
// File info + file language
// ---------------------------------------------------------------------------

/// Information about a file being consumed or produced.
#[derive(Clone, Debug, PartialEq)]
pub struct FileInfo {
    /// Name relative to the starting directory, `/` separated (e.g. `sub/a.txt`).
    pub relative: String,
    /// Absolute path of the file.
    pub absolute: PathBuf,
    /// Starting directory joined with the relative name (as configured, may be relative).
    pub path: PathBuf,
    /// Whether the endpoint's starting directory is absolute.
    pub endpoint_absolute: bool,
    pub length: u64,
    /// Last modified, epoch millis.
    pub modified: i64,
}

impl FileInfo {
    fn from_path(start: &Path, relative: &str) -> Result<FileInfo> {
        let path = start.join(relative);
        let meta = fs::metadata(&path).map_err(|e| io_error("Cannot read file", &path, e))?;
        Ok(FileInfo {
            relative: relative.to_string(),
            absolute: absolutize(&path),
            path,
            endpoint_absolute: start.is_absolute(),
            length: meta.len(),
            modified: meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0),
        })
    }

    /// Reconstruct from the file headers of an exchange (used by producers).
    fn from_headers(ex: &Exchange) -> FileInfo {
        let h = |k: &str| ex.header(k).map(|v| v.to_string());
        let relative = h(headers::FILE_NAME).unwrap_or_default();
        let absolute = h(headers::FILE_ABSOLUTE_PATH).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(&relative));
        FileInfo {
            path: h(headers::FILE_PATH).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(&relative)),
            relative,
            absolute,
            endpoint_absolute: ex.header(headers::FILE_ABSOLUTE).map(|v| v.is_truthy()).unwrap_or(false),
            length: ex.header(headers::FILE_LENGTH).and_then(|v| v.as_i64()).unwrap_or(0).max(0) as u64,
            modified: ex.header(headers::FILE_LAST_MODIFIED).and_then(|v| v.as_i64()).unwrap_or(0),
        }
    }

    pub fn only_name(&self) -> String {
        self.relative.rsplit('/').next().unwrap_or("").to_string()
    }

    pub fn parent(&self) -> String {
        self.path.parent().map(|p| p.display().to_string()).unwrap_or_default()
    }
}

fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Strip the extension(s) of the last path segment: `single` strips only the last one.
fn strip_ext(name: &str, single: bool) -> String {
    let (dir, only) = match name.rfind('/') {
        Some(i) => (&name[..=i], &name[i + 1..]),
        None => ("", name),
    };
    let cut = if single { only.rfind('.') } else { only.find('.') };
    match cut {
        Some(i) if i > 0 => format!("{}{}", dir, &only[..i]),
        _ => name.to_string(),
    }
}

fn ext_of(name: &str, single: bool) -> String {
    let only = name.rsplit('/').next().unwrap_or("");
    let cut = if single { only.rfind('.') } else { only.find('.') };
    match cut {
        Some(i) if i > 0 => only[i + 1..].to_string(),
        _ => String::new(),
    }
}

fn file_function(f: &str, info: &FileInfo) -> Option<String> {
    let name = &info.relative;
    let only = info.only_name();
    Some(match f {
        "name" => name.clone(),
        "name.noext" => strip_ext(name, false),
        "name.noext.single" => strip_ext(name, true),
        "name.ext" | "ext" => ext_of(name, false),
        "name.ext.single" => ext_of(name, true),
        "onlyname" => only.clone(),
        "onlyname.noext" => strip_ext(&only, false),
        "onlyname.noext.single" => strip_ext(&only, true),
        "parent" => info.parent(),
        "path" => info.path.display().to_string(),
        "absolute" => info.endpoint_absolute.to_string(),
        "absolute.path" => info.absolute.display().to_string(),
        "length" | "size" => info.length.to_string(),
        "modified" => info.modified.to_string(),
        _ => return None,
    })
}

/// Whether `f` (the part after `file:`) is a known file language function.
pub fn is_file_function(f: &str) -> bool {
    let info = FileInfo::from_headers(&Exchange::default());
    file_function(f, &info).is_some()
}

/// Evaluate a file language function (`name`, `name.noext`, `onlyname`, `ext`, `parent`,
/// `length`, `modified`...) from the `CamelFile*` headers of the exchange, as the Simple
/// language does for `${file:...}`. Size and modification time are returned as numbers.
pub fn file_function_value(f: &str, ex: &Exchange) -> Option<Value> {
    let info = FileInfo::from_headers(ex);
    match f {
        "length" | "size" => Some(Value::Int(info.length as i64)),
        "modified" => Some(Value::Int(info.modified)),
        "onlyname" if ex.header(headers::FILE_NAME_ONLY).is_some() => ex.header(headers::FILE_NAME_ONLY).map(|v| Value::String(v.to_string())),
        _ => file_function(f, &info).map(Value::String),
    }
}

/// Evaluate a file-language expression against an exchange (and optional file).
pub fn evaluate_file_expression(text: &str, ex: &Exchange, info: Option<&FileInfo>) -> Result<String> {
    if !text.contains("${") && !text.contains("$simple{") {
        return Ok(text.to_string());
    }
    let owned;
    let info = match info {
        Some(i) => i,
        None => {
            owned = FileInfo::from_headers(ex);
            &owned
        }
    };
    let mut out = String::new();
    let mut rest = text;
    let mut needs_simple = false;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let token = after[..end].trim();
        let replaced = if let Some(f) = token.strip_prefix("file:") {
            file_function(f, info)
        } else if let Some(fmt) = token.strip_prefix("date:now:") {
            Some(crate::util_time::format_millis(crate::util_time::now_millis(), fmt))
        } else if let Some(fmt) = token.strip_prefix("date:file:") {
            Some(crate::util_time::format_millis(info.modified, fmt))
        } else {
            match token {
                "exchangeId" => Some(ex.exchange_id.to_string()),
                "id" | "messageId" => Some(ex.message.message_id.to_string()),
                "camelId" | "camelContext.name" => ex.context.as_ref().map(|c| c.name().to_string()),
                _ => None,
            }
        };
        match replaced {
            Some(r) => out.push_str(&r),
            None => {
                needs_simple = true;
                out.push_str(&rest[start..start + 2 + end + 1]);
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    if needs_simple {
        let ctx = ex.context()?;
        let expr = crate::builder::ExpressionDefinition::language("simple", out).resolve_expression(ctx)?;
        return Ok(expr.evaluate(ex)?.to_string());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Matching: ant paths and regex
// ---------------------------------------------------------------------------

/// Match a `/`-separated path against an ant style pattern (`**`, `*`, `?`).
pub fn ant_match(pattern: &str, path: &str, case_sensitive: bool) -> bool {
    let (p, t) = if case_sensitive { (pattern.to_string(), path.to_string()) } else { (pattern.to_lowercase(), path.to_lowercase()) };
    let pat: Vec<&str> = p.trim_start_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    let segs: Vec<&str> = t.trim_start_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    fn rec(p: &[&str], s: &[&str]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(&"**") => (0..=s.len()).any(|i| rec(&p[1..], &s[i..])),
            Some(seg) => !s.is_empty() && crate::util::wildcard_match(s[0], seg) && rec(&p[1..], &s[1..]),
        }
    }
    let pat: Vec<&str> = if p.ends_with('/') { pat.into_iter().chain(std::iter::once("**")).collect() } else { pat };
    rec(&pat, &segs)
}

fn ant_any(patterns: &[String], path: &str, case_sensitive: bool) -> bool {
    patterns.iter().any(|p| ant_match(p.trim(), path, case_sensitive))
}

/// A file name pattern: a real regex when the regex engine supports it, otherwise a
/// translation of common regex constructs (`.*`, `.`, `\.`, `(a|b)`, `^`, `$`, `.+`) to globs.
#[derive(Clone, Debug)]
pub enum NamePattern {
    Regex(crate::regex::Regex),
    Globs(Vec<String>),
}

impl NamePattern {
    pub fn new(pattern: &str) -> Result<NamePattern> {
        if let Ok(r) = crate::regex::Regex::new(pattern) {
            return Ok(NamePattern::Regex(r));
        }
        regex_to_globs(pattern).map(NamePattern::Globs)
    }
    pub fn matches(&self, name: &str) -> bool {
        match self {
            NamePattern::Regex(r) => r.is_full_match(name),
            NamePattern::Globs(g) => {
                let n = name.to_lowercase();
                g.iter().any(|g| crate::util::wildcard_match(&n, g))
            }
        }
    }
}

fn regex_to_globs(re: &str) -> Result<Vec<String>> {
    let unsupported = || CamelError::new(kinds::EXPRESSION_ILLEGAL_SYNTAX, format!("Unsupported file name regex: {}", re));
    let re = re.strip_prefix('^').unwrap_or(re);
    let re = re.strip_suffix('$').unwrap_or(re);
    // expand alternation groups first: a(b|c)d -> abd, acd
    let mut variants: Vec<String> = vec![String::new()];
    let chars: Vec<char> = re.chars().collect();
    let mut i = 0;
    let mut literal = String::new();
    let flush = |variants: &mut Vec<String>, lit: &mut String| {
        for v in variants.iter_mut() {
            v.push_str(lit);
        }
        lit.clear();
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if i + 1 < chars.len() => {
                let n = chars[i + 1];
                if n.is_ascii_alphanumeric() {
                    return Err(unsupported());
                }
                literal.push(n);
                i += 2;
                continue;
            }
            '.' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    literal.push('*');
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '+' {
                    literal.push_str("?*");
                    i += 2;
                } else {
                    literal.push('?');
                    i += 1;
                }
                continue;
            }
            '(' => {
                let close = chars[i..].iter().position(|&c| c == ')').map(|p| p + i).ok_or_else(unsupported)?;
                let inner: String = chars[i + 1..close].iter().collect();
                let inner = inner.strip_prefix("?:").unwrap_or(&inner).to_string();
                if inner.contains(['(', '[', '\\', '*', '+', '?', '.']) {
                    return Err(unsupported());
                }
                flush(&mut variants, &mut literal);
                let mut nv = Vec::new();
                for v in &variants {
                    for alt in inner.split('|') {
                        nv.push(format!("{}{}", v, alt));
                    }
                }
                variants = nv;
                i = close + 1;
                continue;
            }
            '*' | '+' | '?' | '[' | ']' | '{' | '}' | '|' | ')' | '^' | '$' => return Err(unsupported()),
            _ => literal.push(c),
        }
        i += 1;
    }
    flush(&mut variants, &mut literal);
    Ok(variants.into_iter().map(|v| v.to_lowercase()).collect())
}

// ---------------------------------------------------------------------------
// Repositories and filters
// ---------------------------------------------------------------------------

/// Idempotent repository used by the file consumer (`idempotentRepository=#ref`).
pub trait IdempotentRepository: Send + Sync {
    /// Add a key; returns false if it was already present.
    fn add(&self, key: &str) -> bool;
    fn contains(&self, key: &str) -> bool;
    fn remove(&self, key: &str) -> bool;
    fn clear(&self);
}

/// In-memory LRU idempotent repository (Camel `MemoryIdempotentRepository`, default 1000 entries).
pub struct MemoryIdempotentRepository {
    max: usize,
    state: Mutex<(HashSet<String>, VecDeque<String>)>,
}

impl MemoryIdempotentRepository {
    pub fn new(max: usize) -> Self {
        MemoryIdempotentRepository { max: max.max(1), state: Mutex::new((HashSet::new(), VecDeque::new())) }
    }
    pub fn size(&self) -> usize {
        self.state.lock().unwrap().0.len()
    }
}

impl Default for MemoryIdempotentRepository {
    fn default() -> Self {
        Self::new(1000)
    }
}

impl IdempotentRepository for MemoryIdempotentRepository {
    fn add(&self, key: &str) -> bool {
        let mut s = self.state.lock().unwrap();
        if !s.0.insert(key.to_string()) {
            return false;
        }
        s.1.push_back(key.to_string());
        while s.1.len() > self.max {
            if let Some(old) = s.1.pop_front() {
                s.0.remove(&old);
            }
        }
        true
    }
    fn contains(&self, key: &str) -> bool {
        self.state.lock().unwrap().0.contains(key)
    }
    fn remove(&self, key: &str) -> bool {
        let mut s = self.state.lock().unwrap();
        let r = s.0.remove(key);
        s.1.retain(|k| k != key);
        r
    }
    fn clear(&self) {
        let mut s = self.state.lock().unwrap();
        s.0.clear();
        s.1.clear();
    }
}

/// Custom file filter (`filter=#ref`); bind as `Arc<dyn GenericFileFilter>`.
pub trait GenericFileFilter: Send + Sync {
    fn accept(&self, file: &FileInfo, is_directory: bool) -> bool;
}

impl<F: Fn(&FileInfo, bool) -> bool + Send + Sync> GenericFileFilter for F {
    fn accept(&self, file: &FileInfo, is_directory: bool) -> bool {
        self(file, is_directory)
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// What to do when the producer's target file exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileExist {
    Override,
    Append,
    Fail,
    Ignore,
    Move,
    TryRename,
}

impl FileExist {
    pub fn parse(s: &str) -> Result<FileExist> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "override" => FileExist::Override,
            "append" => FileExist::Append,
            "fail" => FileExist::Fail,
            "ignore" => FileExist::Ignore,
            "move" => FileExist::Move,
            "tryrename" => FileExist::TryRename,
            _ => return Err(CamelError::illegal_argument(format!("Invalid fileExist: {}", s))),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadLock {
    None,
    MarkerFile,
    Changed,
    FileLock,
    Rename,
}

#[derive(Clone, Debug)]
struct SortKey {
    reverse: bool,
    ignore_case: bool,
    key: String,
}

fn latin1(charset: &Option<String>) -> bool {
    matches!(charset.as_deref().map(|c| c.to_ascii_lowercase()).as_deref(), Some("iso-8859-1") | Some("latin1") | Some("iso8859_1"))
}

/// Full file endpoint configuration.
#[derive(Clone)]
pub struct FileConfig {
    pub directory: PathBuf,
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
    pub shuffle: bool,
    pub read_lock: ReadLock,
    pub read_lock_check_interval: Duration,
    pub read_lock_timeout: Duration,
    pub read_lock_min_length: u64,
    pub read_lock_min_age: Duration,
    pub read_lock_marker_file: bool,
    pub read_lock_delete_orphans: bool,
    pub idempotent: bool,
    pub idempotent_key: Option<String>,
    pub done_file_name: Option<String>,
    pub charset: Option<String>,
    pub starting_directory_must_exist: bool,
    pub directory_must_exist: bool,
    pub auto_create: bool,
    pub include_hidden_files: bool,
    pub include_hidden_dirs: bool,
    pub filter: Option<Arc<dyn GenericFileFilter>>,
    pub scheduler: ScheduledPollConfig,
    // producer
    pub file_exist: FileExist,
    pub move_existing: Option<String>,
    pub temp_file_name: Option<String>,
    pub temp_prefix: Option<String>,
    pub append_chars: Option<String>,
    pub keep_last_modified: bool,
    pub flatten: bool,
    pub allow_null_body: bool,
    pub eager_delete_target_file: bool,
    pub jail_starting_directory: bool,
}

fn take_list(params: &mut Parameters, key: &str) -> Vec<String> {
    params.take(key).map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default()
}

fn take_nonempty(params: &mut Parameters, key: &str) -> Option<String> {
    params.take(key).filter(|s| !s.is_empty())
}

impl FileConfig {
    fn from_params(ctx: &CamelContext, directory: &str, params: &mut Parameters) -> Result<FileConfig> {
        let noop = params.take_bool("noop", false)?;
        let delete = params.take_bool("delete", false)?;
        let move_expr = take_nonempty(params, "move");
        if delete && move_expr.is_some() {
            return Err(CamelError::illegal_argument("You cannot set both delete=true and move options"));
        }
        let read_lock = match params.take("readLock").as_deref().map(|s| s.to_string()) {
            None => ReadLock::None,
            Some(s) => match s.as_str() {
                "none" | "idempotent" | "idempotent-changed" | "idempotent-rename" => ReadLock::None,
                "markerFile" => ReadLock::MarkerFile,
                "changed" => ReadLock::Changed,
                "fileLock" => ReadLock::FileLock,
                "rename" => ReadLock::Rename,
                other => return Err(CamelError::illegal_argument(format!("Unsupported readLock: {}", other))),
            },
        };
        let sort_by = match params.take("sortBy") {
            None => Vec::new(),
            Some(s) => s
                .split(';')
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
                .collect(),
        };
        let filter = match params.take("filter") {
            None => None,
            Some(r) => {
                let name = r.trim_start_matches('#').trim_start_matches("bean:");
                let f = ctx.registry().lookup::<Arc<dyn GenericFileFilter>>(name).ok_or_else(|| CamelError::no_such_bean(name))?;
                Some((*f).clone())
            }
        };
        let idempotent_opt = params.take("idempotent");
        let idempotent = match idempotent_opt {
            Some(v) => v.is_empty() || v.eq_ignore_ascii_case("true"),
            None => noop,
        };
        let recursive = params.take_bool("recursive", false)?;
        let include = take_nonempty(params, "include").map(|p| NamePattern::new(&p)).transpose()?;
        let exclude = take_nonempty(params, "exclude").map(|p| NamePattern::new(&p)).transpose()?;
        let scheduler = ScheduledPollConfig::from_params(params, Duration::from_millis(1000), Duration::from_millis(500))?;
        let cfg = FileConfig {
            directory: PathBuf::from(directory),
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
            recursive,
            min_depth: params.take_usize("minDepth", 0)?,
            max_depth: params.take_usize("maxDepth", usize::MAX)?,
            max_messages_per_poll: params.take_usize("maxMessagesPerPoll", 0)?,
            sort_by,
            shuffle: params.take_bool("shuffle", false)?,
            read_lock,
            read_lock_check_interval: params.take_duration("readLockCheckInterval", Duration::from_millis(1000))?,
            read_lock_timeout: params.take_duration("readLockTimeout", Duration::from_millis(10_000))?,
            read_lock_min_length: params.take_u64("readLockMinLength", 1)?,
            read_lock_min_age: params.take_duration("readLockMinAge", Duration::ZERO)?,
            read_lock_marker_file: params.take_bool("readLockMarkerFile", true)?,
            read_lock_delete_orphans: params.take_bool("readLockDeleteOrphanLockFiles", true)?,
            idempotent,
            idempotent_key: take_nonempty(params, "idempotentKey"),
            done_file_name: take_nonempty(params, "doneFileName"),
            charset: take_nonempty(params, "charset"),
            starting_directory_must_exist: params.take_bool("startingDirectoryMustExist", false)?,
            directory_must_exist: params.take_bool("directoryMustExist", false)?,
            auto_create: params.take_bool("autoCreate", true)?,
            include_hidden_files: params.take_bool("includeHiddenFiles", false)?,
            include_hidden_dirs: params.take_bool("includeHiddenDirs", false)?,
            filter,
            scheduler,
            file_exist: FileExist::parse(&params.take_string("fileExist", "Override"))?,
            move_existing: take_nonempty(params, "moveExisting"),
            temp_file_name: take_nonempty(params, "tempFileName"),
            temp_prefix: take_nonempty(params, "tempPrefix"),
            append_chars: params.take("appendChars").map(|s| s.replace("\\n", "\n").replace("\\r", "\r").replace("\\t", "\t")),
            keep_last_modified: params.take_bool("keepLastModified", false)?,
            flatten: params.take_bool("flatten", false)?,
            allow_null_body: params.take_bool("allowNullBody", false)?,
            eager_delete_target_file: params.take_bool("eagerDeleteTargetFile", true)?,
            jail_starting_directory: params.take_bool("jailStartingDirectory", true)?,
        };
        if cfg.file_exist == FileExist::Move && cfg.move_existing.is_none() {
            return Err(CamelError::illegal_argument("You must configure moveExisting option when fileExist=Move"));
        }
        if cfg.file_exist == FileExist::TryRename && cfg.temp_file_name.is_none() && cfg.temp_prefix.is_none() {
            return Err(CamelError::illegal_argument("You must configure tempFileName option when fileExist=TryRename"));
        }
        for k in [
            "eagerMaxMessagesPerPoll",
            "chmod",
            "chmodDirectory",
            "forceWrites",
            "bufferSize",
            "readLockLoggingLevel",
            "readLockRemoveOnCommit",
            "readLockRemoveOnRollback",
            "readLockIdempotentReleaseDelay",
            "probeContentType",
            "extendedAttributes",
            "copyAndDeleteOnRenameFail",
            "renameUsingCopy",
            "synchronous",
            "lazyStartProducer",
            "inProgressRepository",
            "processStrategy",
            "exclusiveReadLockStrategy",
            "onCompletionExceptionHandler",
            "idempotentEager",
            "directoryMustExist",
        ] {
            params.take(k);
        }
        Ok(cfg)
    }
}

// ---------------------------------------------------------------------------
// Component / endpoint
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct FileComponent;

impl Component for FileComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let dir = if remaining.is_empty() { "." } else { remaining };
        let repo: Arc<dyn IdempotentRepository> = match params.take("idempotentRepository") {
            Some(r) => {
                let name = r.trim_start_matches('#').trim_start_matches("bean:");
                let repo = ctx.registry().lookup::<Arc<dyn IdempotentRepository>>(name).ok_or_else(|| CamelError::no_such_bean(name))?;
                (*repo).clone()
            }
            None => Arc::new(MemoryIdempotentRepository::default()),
        };
        let config = FileConfig::from_params(ctx, dir, params)?;
        Ok(Arc::new(FileEndpoint {
            uri: uri.to_string(),
            ctx: ctx.downgrade(),
            shared: Arc::new(FileShared { config, repo, in_progress: Mutex::new(HashSet::new()) }),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct FileShared {
    config: FileConfig,
    repo: Arc<dyn IdempotentRepository>,
    in_progress: Mutex<HashSet<PathBuf>>,
}

/// A file endpoint.
pub struct FileEndpoint {
    uri: String,
    ctx: WeakContext,
    shared: Arc<FileShared>,
}

impl FileEndpoint {
    pub fn config(&self) -> &FileConfig {
        &self.shared.config
    }
    /// The idempotent repository used by consumers of this endpoint.
    pub fn idempotent_repository(&self) -> Arc<dyn IdempotentRepository> {
        self.shared.repo.clone()
    }
}

impl Endpoint for FileEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(FileProducer { shared: self.shared.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let core = Arc::new(ConsumerCore { shared: self.shared.clone(), ctx: self.ctx.clone(), stop: StopFlag::new(), logger: self.uri.clone() });
        let c2 = core.clone();
        let p2 = processor.clone();
        let mut poll = ScheduledPollConsumer::new(self.uri.clone(), self.shared.config.scheduler.clone(), move || c2.poll(p2.as_ref()));
        let (ctx, p3, logger) = (self.ctx.clone(), processor, self.uri.clone());
        poll = poll.with_empty_message(move || {
            let mut ex = new_exchange(&ctx);
            process_consumer_exchange(&logger, p3.as_ref(), &mut ex);
        });
        Ok(Arc::new(FileConsumer { core, poll }))
    }
    fn create_polling_consumer(&self) -> Result<Arc<dyn PollingConsumer>> {
        let core = Arc::new(ConsumerCore { shared: self.shared.clone(), ctx: self.ctx.clone(), stop: StopFlag::new(), logger: self.uri.clone() });
        Ok(Arc::new(FilePollingConsumer { core }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Consumer
// ---------------------------------------------------------------------------

struct ConsumerCore {
    shared: Arc<FileShared>,
    ctx: WeakContext,
    stop: Arc<StopFlag>,
    logger: String,
}

fn lock_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".camelLock");
    PathBuf::from(s)
}

fn rename_or_copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error("Cannot create directory", parent, e))?;
    }
    if to.exists() {
        let _ = fs::remove_file(to);
    }
    if fs::rename(from, to).is_err() {
        fs::copy(from, to).map_err(|e| io_error("Cannot copy file to", to, e))?;
        fs::remove_file(from).map_err(|e| io_error("Cannot delete file", from, e))?;
    }
    Ok(())
}

/// Lexically normalize a path (resolve `.` and `..`).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            PathComponent::CurDir => {}
            PathComponent::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl ConsumerCore {
    fn cfg(&self) -> &FileConfig {
        &self.shared.config
    }

    fn start(&self) -> Result<()> {
        self.stop.reset();
        let c = self.cfg();
        if !c.directory.exists() {
            if c.starting_directory_must_exist || c.directory_must_exist {
                return Err(CamelError::new(
                    kinds::FILE_NOT_FOUND,
                    format!("Starting directory does not exist: {}", c.directory.display()),
                ));
            }
            if c.auto_create {
                fs::create_dir_all(&c.directory).map_err(|e| io_error("Cannot create directory", &c.directory, e))?;
            }
        }
        if c.read_lock == ReadLock::MarkerFile && c.read_lock_delete_orphans {
            self.delete_orphan_locks(&c.directory, 1);
        }
        Ok(())
    }

    fn delete_orphan_locks(&self, dir: &Path, depth: usize) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if self.cfg().recursive && depth < self.cfg().max_depth {
                    self.delete_orphan_locks(&p, depth + 1);
                }
            } else if p.to_string_lossy().ends_with(".camelLock") {
                let _ = fs::remove_file(&p);
            }
        }
    }

    /// Evaluate an expression in the context of a file (with a synthetic exchange if needed).
    fn eval(&self, text: &str, ex: Option<&Exchange>, info: &FileInfo) -> Result<String> {
        match ex {
            Some(ex) => evaluate_file_expression(text, ex, Some(info)),
            None => {
                let mut ex = new_exchange(&self.ctx);
                set_file_headers(&mut ex, info);
                evaluate_file_expression(text, &ex, Some(info))
            }
        }
    }

    fn done_file_for(&self, info: &FileInfo) -> Result<Option<PathBuf>> {
        let Some(d) = &self.cfg().done_file_name else { return Ok(None) };
        // the done file name is resolved for the file's name only, in the same directory
        let only = FileInfo { relative: info.only_name(), ..info.clone() };
        let name = self.eval(d, None, &only)?;
        Ok(Some(info.absolute.parent().map(|p| p.join(&name)).unwrap_or_else(|| PathBuf::from(name))))
    }

    fn accept_name(&self, rel: &str, only: &str) -> bool {
        let c = self.cfg();
        if only.ends_with(".camelLock") {
            return false;
        }
        if only.starts_with('.') && !c.include_hidden_files {
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
        if !c.ant_include.is_empty() && !ant_any(&c.ant_include, rel, c.ant_case_sensitive) {
            return false;
        }
        if !c.ant_exclude.is_empty() && ant_any(&c.ant_exclude, rel, c.ant_case_sensitive) {
            return false;
        }
        true
    }

    fn walk(&self, dir: &Path, rel: &str, depth: usize, out: &mut Vec<FileInfo>, fixed: &Option<String>) -> Result<()> {
        let c = self.cfg();
        let rd = match fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_error("Cannot list directory", dir, e)),
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().to_string();
            let child_rel = if rel.is_empty() { name.clone() } else { format!("{}/{}", rel, name) };
            let ft = match e.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            let is_dir = ft.is_dir() || (ft.is_symlink() && e.path().is_dir());
            if is_dir {
                if c.recursive && depth < c.max_depth && (!name.starts_with('.') || c.include_hidden_dirs) {
                    if let Some(f) = &c.filter {
                        if let Ok(info) = FileInfo::from_path(&c.directory, &child_rel) {
                            if !f.accept(&info, true) {
                                continue;
                            }
                        }
                    }
                    self.walk(&e.path(), &child_rel, depth + 1, out, fixed)?;
                }
                continue;
            }
            if depth < c.min_depth || depth > c.max_depth || (!c.recursive && depth > 1) {
                continue;
            }
            if let Some(f) = fixed {
                if &child_rel != f && &name != f {
                    continue;
                }
            } else if !self.accept_name(&child_rel, &name) {
                continue;
            }
            let info = match FileInfo::from_path(&c.directory, &child_rel) {
                Ok(i) => i,
                Err(_) => continue,
            };
            if let Some(f) = &c.filter {
                if !f.accept(&info, false) {
                    continue;
                }
            }
            if let Some(done) = self.done_file_for(&info)? {
                if done == info.absolute || !done.exists() {
                    continue;
                }
            }
            if c.idempotent && self.shared.repo.contains(&self.idempotent_key(&info)?) {
                continue;
            }
            if self.shared.in_progress.lock().unwrap().contains(&info.absolute) {
                continue;
            }
            out.push(info);
        }
        Ok(())
    }

    fn idempotent_key(&self, info: &FileInfo) -> Result<String> {
        match &self.cfg().idempotent_key {
            Some(k) => self.eval(k, None, info),
            None => Ok(info.absolute.display().to_string()),
        }
    }

    /// List the files eligible for consumption, sorted and limited.
    fn scan(&self) -> Result<Vec<FileInfo>> {
        let c = self.cfg();
        if !c.directory.exists() {
            if c.directory_must_exist {
                return Err(CamelError::new(kinds::FILE_NOT_FOUND, format!("Directory does not exist: {}", c.directory.display())));
            }
            return Ok(Vec::new());
        }
        let fixed = match &c.file_name {
            Some(f) => Some(evaluate_file_expression(f, &new_exchange(&self.ctx), None)?),
            None => None,
        };
        let mut files = Vec::new();
        self.walk(&c.directory, "", 1, &mut files, &fixed)?;
        if c.shuffle {
            let mut seed = crate::util_time::now_millis() as u64 | 1;
            for i in (1..files.len()).rev() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                files.swap(i, (seed % (i as u64 + 1)) as usize);
            }
        } else if !c.sort_by.is_empty() {
            files.sort_by(|a, b| {
                for k in &c.sort_by {
                    let ord = compare_by(a, b, k);
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                }
                std::cmp::Ordering::Equal
            });
        }
        if c.max_messages_per_poll > 0 && files.len() > c.max_messages_per_poll {
            files.truncate(c.max_messages_per_poll);
        }
        Ok(files)
    }

    fn acquire_read_lock(&self, info: &FileInfo) -> Result<bool> {
        let c = self.cfg();
        match c.read_lock {
            ReadLock::None => Ok(true),
            ReadLock::MarkerFile | ReadLock::FileLock => {
                if !c.read_lock_marker_file && c.read_lock == ReadLock::MarkerFile {
                    return Ok(true);
                }
                match fs::OpenOptions::new().write(true).create_new(true).open(lock_path(&info.absolute)) {
                    Ok(_) => Ok(true),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                    Err(e) => Err(io_error("Cannot create lock file for", &info.absolute, e)),
                }
            }
            ReadLock::Rename => {
                let tmp = info.absolute.with_extension(format!(
                    "{}camelExclusiveReadLock",
                    info.absolute.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
                ));
                if fs::rename(&info.absolute, &tmp).is_err() {
                    return Ok(false);
                }
                fs::rename(&tmp, &info.absolute).map_err(|e| io_error("Cannot rename back", &tmp, e))?;
                Ok(true)
            }
            ReadLock::Changed => {
                // Camel's FileChangedExclusiveReadLockStrategy extends the marker file strategy:
                // the marker file is taken first (readLockMarkerFile=true by default).
                if c.read_lock_marker_file {
                    match fs::OpenOptions::new().write(true).create_new(true).open(lock_path(&info.absolute)) {
                        Ok(_) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
                        Err(e) => return Err(io_error("Cannot create lock file for", &info.absolute, e)),
                    }
                }
                let acquired = self.wait_until_unchanged(info);
                if !acquired && c.read_lock_marker_file {
                    let _ = fs::remove_file(lock_path(&info.absolute));
                }
                Ok(acquired)
            }
        }
    }

    /// The `changed` read lock check, as in Camel: without `readLockMinAge` the file must keep
    /// the same length and modification time between two checks `readLockCheckInterval` apart;
    /// with `readLockMinAge` the file only has to be last modified at least that long ago (so an
    /// old file is taken at once). In both cases its length must be at least `readLockMinLength`.
    /// Gives up after `readLockTimeout` (0 = never).
    fn wait_until_unchanged(&self, info: &FileInfo) -> bool {
        let c = self.cfg();
        let start = Instant::now();
        let min_age = c.read_lock_min_age.as_millis() as i64;
        let stat = || -> Option<(u64, i64)> {
            let m = fs::metadata(&info.absolute).ok()?;
            let modified = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis() as i64;
            Some((m.len(), modified))
        };
        let mut last: Option<(u64, i64)> = None;
        loop {
            if !c.read_lock_timeout.is_zero() && start.elapsed() > c.read_lock_timeout {
                crate::camel_warn!(&self.logger, "Cannot acquire read lock within {} millis. Will skip the file: {}", c.read_lock_timeout.as_millis(), info.absolute.display());
                return false;
            }
            let Some((len, modified)) = stat() else { return false };
            let unchanged = last == Some((len, modified));
            let old_enough = crate::util_time::now_millis() - modified >= min_age;
            if len >= c.read_lock_min_length && ((min_age == 0 && unchanged) || (min_age > 0 && old_enough)) {
                return true;
            }
            last = Some((len, modified));
            if self.stop.sleep(c.read_lock_check_interval) {
                return false;
            }
        }
    }

    fn release_read_lock(&self, info: &FileInfo) {
        let c = self.cfg();
        if matches!(c.read_lock, ReadLock::MarkerFile | ReadLock::FileLock) || (c.read_lock == ReadLock::Changed && c.read_lock_marker_file) {
            let _ = fs::remove_file(lock_path(&info.absolute));
        }
    }

    /// Resolve a move-style expression into a target path. A value without `${` is a
    /// directory: the file keeps its relative name inside it.
    fn move_target(&self, expr: &str, ex: Option<&Exchange>, info: &FileInfo) -> Result<PathBuf> {
        let text = if expr.contains("${") { expr.to_string() } else { format!("{}/${{file:name}}", expr.trim_end_matches('/')) };
        let name = self.eval(&text, ex, info)?;
        let p = PathBuf::from(&name);
        Ok(if p.is_absolute() { p } else { normalize(&absolutize(&self.cfg().directory).join(p)) })
    }

    /// Claim, read and turn a file into an exchange. Returns None if the file could not be claimed.
    fn begin(&self, info: FileInfo) -> Result<Option<(Exchange, FileInfo)>> {
        if !self.shared.in_progress.lock().unwrap().insert(info.absolute.clone()) {
            return Ok(None);
        }
        let original = info.absolute.clone();
        let r = (|| -> Result<Option<(Exchange, FileInfo)>> {
            let mut info = info;
            if !self.acquire_read_lock(&info)? {
                return Ok(None);
            }
            if let Some(pre) = &self.cfg().pre_move {
                // the file keeps its (original) relative name; only its location changes
                let target = self.move_target(pre, None, &info)?;
                rename_or_copy(&info.absolute, &target)?;
                self.release_read_lock(&info);
                info.absolute = target.clone();
                info.path = target;
            }
            let bytes = match fs::read(&info.absolute) {
                Ok(b) => b,
                Err(e) => {
                    self.release_read_lock(&info);
                    return Err(io_error("Cannot read file", &info.absolute, e));
                }
            };
            let mut ex = new_exchange(&self.ctx);
            ex.set_body(if latin1(&self.cfg().charset) { Value::String(bytes.iter().map(|&b| b as char).collect()) } else { Value::Bytes(bytes) });
            if let Some(cs) = &self.cfg().charset {
                ex.set_property(props::CHARSET_NAME, cs.clone());
            }
            set_file_headers(&mut ex, &info);
            ex.set_header(headers::FILE_NAME_CONSUMED, info.relative.clone());
            Ok(Some((ex, info.clone())))
        })();
        if !matches!(r, Ok(Some(_))) {
            self.shared.in_progress.lock().unwrap().remove(&original);
        }
        r
    }

    /// Run the completion strategy (commit or rollback) for a processed file.
    fn complete(&self, ex: &Exchange, info: &FileInfo, original: &Path) {
        let c = self.cfg();
        let result = (|| -> Result<()> {
            if ex.is_failed() || ex.rollback_only {
                if let Some(mf) = &c.move_failed {
                    let target = self.move_target(mf, Some(ex), info)?;
                    rename_or_copy(&info.absolute, &target)?;
                } else if c.idempotent {
                    self.shared.repo.remove(&self.idempotent_key(info)?);
                }
                return Ok(());
            }
            if c.idempotent {
                self.shared.repo.add(&self.idempotent_key(info)?);
            }
            if c.delete {
                if info.absolute.exists() {
                    fs::remove_file(&info.absolute).map_err(|e| io_error("Cannot delete file", &info.absolute, e))?;
                }
            } else if !c.noop {
                let expr = c.move_expr.clone().unwrap_or_else(|| ".camel".to_string());
                let target = self.move_target(&expr, Some(ex), info)?;
                if target != info.absolute {
                    rename_or_copy(&info.absolute, &target)?;
                }
            }
            if let Some(done) = self.done_file_for(&FileInfo { absolute: original.to_path_buf(), ..info.clone() })? {
                if !c.noop {
                    let _ = fs::remove_file(done);
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            crate::camel_warn!(&self.logger, "Error during commit/rollback of file {}: {}", info.absolute.display(), e);
        }
        self.release_read_lock(&FileInfo { absolute: original.to_path_buf(), ..info.clone() });
        self.release_read_lock(info);
        let mut ip = self.shared.in_progress.lock().unwrap();
        ip.remove(original);
        ip.remove(&info.absolute);
    }

    fn poll(&self, processor: &dyn Processor) -> Result<usize> {
        let files = self.scan()?;
        let mut n = 0;
        for f in files {
            if self.stop.is_stopped() {
                break;
            }
            let original = f.absolute.clone();
            let Some((mut ex, info)) = self.begin(f)? else { continue };
            process_consumer_exchange(&self.logger, processor, &mut ex);
            self.complete(&ex, &info, &original);
            n += 1;
        }
        Ok(n)
    }
}

fn compare_by(a: &FileInfo, b: &FileInfo, k: &SortKey) -> std::cmp::Ordering {
    let ord = match k.key.as_str() {
        "modified" | "lastModified" => a.modified.cmp(&b.modified),
        "length" | "size" => a.length.cmp(&b.length),
        key => {
            let (x, y) = (file_function(key, a).unwrap_or_default(), file_function(key, b).unwrap_or_default());
            if k.ignore_case {
                x.to_lowercase().cmp(&y.to_lowercase())
            } else {
                x.cmp(&y)
            }
        }
    };
    if k.reverse {
        ord.reverse()
    } else {
        ord
    }
}

fn set_file_headers(ex: &mut Exchange, info: &FileInfo) {
    ex.set_header(headers::FILE_NAME, info.relative.clone());
    ex.set_header(headers::FILE_NAME_ONLY, info.only_name());
    ex.set_header(headers::FILE_ABSOLUTE, info.endpoint_absolute);
    ex.set_header(headers::FILE_ABSOLUTE_PATH, info.absolute.display().to_string());
    ex.set_header(headers::FILE_RELATIVE_PATH, info.relative.clone());
    ex.set_header(headers::FILE_PATH, info.path.display().to_string());
    ex.set_header(headers::FILE_PARENT, info.parent());
    ex.set_header(headers::FILE_LENGTH, Value::from(info.length as i64));
    ex.set_header(headers::FILE_LAST_MODIFIED, Value::from(info.modified));
}

/// The scheduled file consumer.
pub struct FileConsumer {
    core: Arc<ConsumerCore>,
    poll: ScheduledPollConsumer,
}

impl FileConsumer {
    /// Trigger a poll now (e.g. with `startScheduler=false`).
    pub fn poll_once(&self) -> Result<usize> {
        self.poll.poll_once()
    }
}

impl Service for FileConsumer {
    fn start(&self) -> Result<()> {
        self.core.start()?;
        self.poll.start()
    }
    fn stop(&self) -> Result<()> {
        self.core.stop.stop();
        self.poll.stop()
    }
    fn as_suspendable(&self) -> Option<&dyn crate::spi::SuspendableService> {
        self.poll.as_suspendable()
    }
}

/// Polling consumer: each `receive` returns the next eligible file (the completion
/// strategy - move/delete/idempotent - is applied immediately on receive).
struct FilePollingConsumer {
    core: Arc<ConsumerCore>,
}

impl Service for FilePollingConsumer {
    fn start(&self) -> Result<()> {
        self.core.start()
    }
    fn stop(&self) -> Result<()> {
        self.core.stop.stop();
        Ok(())
    }
}

impl PollingConsumer for FilePollingConsumer {
    fn receive(&self, timeout: Option<Duration>) -> Result<Option<Exchange>> {
        let deadline = timeout.and_then(super::support::deadline_after);
        loop {
            for f in self.core.scan()? {
                let original = f.absolute.clone();
                if let Some((ex, info)) = self.core.begin(f)? {
                    self.core.complete(&ex, &info, &original);
                    return Ok(Some(ex));
                }
            }
            match (timeout, deadline) {
                (None, _) => return Ok(None),
                (Some(t), _) if t.is_zero() => return Ok(None),
                (Some(_), Some(d)) if Instant::now() >= d => return Ok(None),
                _ => {}
            }
            let wait = self.core.cfg().scheduler.delay.clamp(Duration::from_millis(1), Duration::from_millis(50));
            if self.core.stop.sleep(wait) {
                return Ok(None);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Producer
// ---------------------------------------------------------------------------

struct FileProducer {
    shared: Arc<FileShared>,
}

impl Service for FileProducer {}

fn write_bytes(path: &Path, data: &[u8], append: bool) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .open(path)
        .map_err(|e| io_error("Cannot write file", path, e))?;
    f.write_all(data).map_err(|e| io_error("Cannot write file", path, e))
}

impl FileProducer {
    fn cfg(&self) -> &FileConfig {
        &self.shared.config
    }

    fn target_name(&self, ex: &mut Exchange) -> Result<String> {
        let mut name = if let Some(v) = ex.message.headers.remove(headers::OVERRULE_FILE_NAME) {
            evaluate_file_expression(&v.to_string(), ex, None)?
        } else if let Some(f) = &self.cfg().file_name {
            evaluate_file_expression(f, ex, None)?
        } else if let Some(v) = ex.header(headers::FILE_NAME) {
            v.to_string()
        } else {
            ex.message.message_id.to_string()
        };
        if self.cfg().flatten {
            name = name.rsplit(['/', '\\']).next().unwrap_or("").to_string();
        }
        if name.is_empty() {
            return Err(file_error("Cannot write file with empty file name"));
        }
        Ok(name)
    }

    fn produced_info(&self, target: &Path, rel: &str) -> FileInfo {
        FileInfo {
            relative: rel.to_string(),
            absolute: absolutize(target),
            path: target.to_path_buf(),
            endpoint_absolute: self.cfg().directory.is_absolute(),
            length: fs::metadata(target).map(|m| m.len()).unwrap_or(0),
            modified: crate::util_time::now_millis(),
        }
    }
}

impl Processor for FileProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let c = self.cfg().clone();
        let name = self.target_name(ex)?;
        let start = absolutize(&c.directory);
        let name_path = PathBuf::from(&name);
        let target = normalize(&if name_path.is_absolute() { name_path.clone() } else { start.join(&name_path) });
        if c.jail_starting_directory && !name_path.is_absolute() && !target.starts_with(normalize(&start)) {
            return Err(file_error(format!(
                "Cannot write file with name: {} as the filename is jailed to the starting directory: {}",
                name,
                c.directory.display()
            )));
        }
        let parent = target.parent().map(Path::to_path_buf).unwrap_or_else(|| start.clone());
        if !parent.exists() {
            if c.auto_create {
                fs::create_dir_all(&parent).map_err(|e| io_error("Cannot create directory", &parent, e))?;
            } else {
                return Err(file_error(format!("Cannot write file: {} as the parent directory does not exist", target.display())));
            }
        }
        // body
        let mut data = match ex.body() {
            Value::Null if !c.allow_null_body => {
                return Err(CamelError::new("GenericFileWriteException", format!("Cannot write null body to file: {}", target.display())))
            }
            Value::String(s) if latin1(&c.charset) => s.chars().map(|ch| if (ch as u32) < 256 { ch as u8 } else { b'?' }).collect(),
            other => other.to_bytes(),
        };
        if let Some(a) = &c.append_chars {
            data.extend_from_slice(a.as_bytes());
        }
        let exists = target.exists();
        let rel = target.strip_prefix(normalize(&start)).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_else(|_| name.clone());
        if exists {
            match c.file_exist {
                FileExist::Fail => {
                    return Err(file_error(format!("File already exist: {}. Cannot write new file.", target.display())));
                }
                FileExist::Ignore => {
                    crate::camel_trace!("file", "An existing file already exists: {}. Ignore and do not override it.", target.display());
                    return Ok(());
                }
                FileExist::Move => {
                    let existing = self.produced_info(&target, &rel);
                    let expr = c.move_existing.clone().unwrap_or_default();
                    let text = if expr.contains("${") { expr } else { format!("{}/${{file:name}}", expr.trim_end_matches('/')) };
                    let mut tmp_ex = ex.clone();
                    set_file_headers(&mut tmp_ex, &existing);
                    let moved = evaluate_file_expression(&text, &tmp_ex, Some(&existing))?;
                    let mp = PathBuf::from(moved);
                    let mp = if mp.is_absolute() { mp } else { normalize(&start.join(mp)) };
                    rename_or_copy(&target, &mp)?;
                }
                _ => {}
            }
        }
        let append = c.file_exist == FileExist::Append;
        let temp = match (&c.temp_file_name, &c.temp_prefix) {
            (Some(t), _) => {
                let info = self.produced_info(&target, &rel);
                let mut tmp_ex = ex.clone();
                set_file_headers(&mut tmp_ex, &info);
                Some(parent.join(evaluate_file_expression(t, &tmp_ex, Some(&info))?))
            }
            (None, Some(p)) => Some(parent.join(format!("{}{}", p, target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()))),
            _ => None,
        };
        match temp {
            Some(tmp) if !append => {
                if tmp.exists() {
                    let _ = fs::remove_file(&tmp);
                }
                if exists && c.eager_delete_target_file && c.file_exist == FileExist::Override {
                    let _ = fs::remove_file(&target);
                }
                write_bytes(&tmp, &data, false)?;
                if c.file_exist != FileExist::TryRename && target.exists() {
                    fs::remove_file(&target).map_err(|e| io_error("Cannot delete existing file", &target, e))?;
                }
                fs::rename(&tmp, &target).map_err(|e| io_error("Cannot rename temporary file to", &target, e))?;
            }
            _ => write_bytes(&target, &data, append)?,
        }
        if c.keep_last_modified {
            if let Some(m) = ex.header(headers::FILE_LAST_MODIFIED).and_then(|v| v.as_i64()) {
                if let Ok(f) = fs::OpenOptions::new().write(true).open(&target) {
                    let _ = f.set_modified(UNIX_EPOCH + Duration::from_millis(m.max(0) as u64));
                }
            }
        }
        if let Some(d) = &c.done_file_name {
            let info = self.produced_info(&target, &target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
            let mut tmp_ex = ex.clone();
            set_file_headers(&mut tmp_ex, &info);
            let done = parent.join(evaluate_file_expression(d, &tmp_ex, Some(&info))?);
            write_bytes(&done, b"", false)?;
        }
        ex.set_header(headers::FILE_NAME_PRODUCED, absolutize(&target).display().to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ant_matching() {
        assert!(ant_match("**/*.txt", "a.txt", true));
        assert!(ant_match("**/*.txt", "x/y/a.txt", true));
        assert!(!ant_match("*.txt", "x/a.txt", true));
        assert!(ant_match("x/**/b?.log", "x/1/2/b1.log", true));
        assert!(!ant_match("**/*.TXT", "a.txt", true));
        assert!(ant_match("**/*.TXT", "a.txt", false));
        assert!(ant_match("sub/", "sub/a/b.txt", true));
    }

    #[test]
    fn regex_fallback() {
        let p = NamePattern::new(".*\\.txt").unwrap();
        assert!(p.matches("hello.txt"));
        assert!(!p.matches("hello.xml"));
        let p = NamePattern::new("^report-.*\\.(csv|xml)$").unwrap();
        assert!(p.matches("report-1.csv"));
        assert!(p.matches("report-2.xml"));
        assert!(!p.matches("report-2.txt"));
        assert!(NamePattern::new("a[0-9]+").is_err() || NamePattern::new("a[0-9]+").unwrap().matches("a12"));
    }

    #[test]
    fn file_functions() {
        let info = FileInfo {
            relative: "sub/report.tar.gz".into(),
            absolute: PathBuf::from("/data/in/sub/report.tar.gz"),
            path: PathBuf::from("in/sub/report.tar.gz"),
            endpoint_absolute: false,
            length: 12,
            modified: 0,
        };
        let ex = Exchange::default();
        assert_eq!(evaluate_file_expression("${file:name.noext}", &ex, Some(&info)).unwrap(), "sub/report");
        assert_eq!(evaluate_file_expression("${file:onlyname.noext.single}", &ex, Some(&info)).unwrap(), "report.tar");
        assert_eq!(evaluate_file_expression("${file:ext}", &ex, Some(&info)).unwrap(), "tar.gz");
        assert_eq!(evaluate_file_expression("done/${file:onlyname}", &ex, Some(&info)).unwrap(), "done/report.tar.gz");
        assert_eq!(evaluate_file_expression("${date:file:yyyy}-${file:size}", &ex, Some(&info)).unwrap(), "1970-12");
    }
}
