//! Camel `azure-files` semantics (a remote file component like FTP) for `azure-storage-files`:
//!
//! Consumer (enabled by any of these options; plain polling otherwise):
//! * `recursive` (descend sub-directories; `minDepth` / `maxDepth`, the polled directory is
//!   depth 1), `include` / `exclude` (regular expressions on the file name),
//! * `noop=true` leaves files in place and implies `idempotent=true`: an idempotent repository
//!   (`idempotentRepository=#bean` = an `Arc<dyn cameleer_core::component::file::IdempotentRepository>`,
//!   in-memory by default) keyed by the relative path (`idempotentKey=name-etag` also keys by
//!   ETag, so changed files are consumed again) skips files already consumed,
//! * `preMove` (rename before processing), `move` (after success; default none) and
//!   `moveFailed` (after a failure): directory names relative to the file's directory (a value
//!   starting with `/` is relative to the share), done with the Azure Files Rename API
//!   (`PUT ...?comp=rename`, `x-ms-file-rename-source`), creating missing directories,
//! * `delete=true` deletes processed files; files of failed exchanges stay (or are moved with
//!   `moveFailed`).
//!
//! Exchanges carry `CamelFileName` / `CamelAzureStorageFilesFileName` (the path relative to the
//! polled directory, e.g. `sub/a.txt`), `CamelFileNameOnly`, `CamelFileParent`,
//! `CamelFileRelativePath`, `CamelFileLength`.
//!
//! Producer `uploadFile`: the file name (`CamelFileName` may contain sub-directories, created
//! with `autoCreate=true`, the default; `flatten=true` keeps only the last segment); `fileExist`
//! = `Override` (default), `Append` (resize + write at the end), `Fail`, `Ignore`, `Move`
//! (rename the existing file into `moveExisting`); `tempPrefix` uploads to a temporary name in
//! the same directory then renames it (atomic replace). `renameFile` (Cameleer operation):
//! renames the file to `CamelAzureStorageFilesRenameTo` (path relative to the share).

use std::sync::Arc;

use cameleer_core::component::file::{IdempotentRepository, MemoryIdempotentRepository};
use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::regex::Regex;
use cameleer_core::spi::Processor;
use cameleer_core::value::Value;

use super::files::{headers as fh, join, FilesService};
use crate::endpoint::{process, set_batch};
use crate::util::{header_str, Opts};

/// Extra header names.
pub mod headers {
    pub const RENAME_TO: &str = "CamelAzureStorageFilesRenameTo";
}

/// The FTP-like options.
pub struct FtpOptions {
    pub recursive: bool,
    pub min_depth: usize,
    pub max_depth: usize,
    pub include: Option<Regex>,
    pub exclude: Option<Regex>,
    pub noop: bool,
    pub idempotent: bool,
    pub idempotent_by_etag: bool,
    pub repo: Arc<dyn IdempotentRepository>,
    pub pre_move: Option<String>,
    pub move_to: Option<String>,
    pub move_failed: Option<String>,
    pub file_exist: String,
    pub move_existing: Option<String>,
    pub temp_prefix: Option<String>,
    pub auto_create: bool,
    pub flatten: bool,
    explicit: bool,
}

fn full_match(re: &Regex, s: &str) -> bool {
    re.find(s) == Some((0, s.len()))
}

impl FtpOptions {
    pub fn from_opts(o: &mut Opts) -> Result<FtpOptions> {
        let keys = ["recursive", "minDepth", "maxDepth", "include", "exclude", "noop", "idempotent", "idempotentKey", "idempotentRepository", "preMove", "move", "moveFailed"];
        let explicit = keys.iter().any(|k| o.params.get(k).is_some());
        let noop = o.bool("noop", false)?;
        let repo: Arc<dyn IdempotentRepository> = match o.get("idempotentRepository") {
            Some(r) => {
                let name = r.trim_start_matches('#').trim_start_matches("bean:").to_string();
                let repo = o.context().registry().lookup::<Arc<dyn IdempotentRepository>>(&name).ok_or_else(|| CamelError::no_such_bean(&name))?;
                (*repo).clone()
            }
            None => Arc::new(MemoryIdempotentRepository::default()),
        };
        let re = |v: Option<String>| v.map(|p| Regex::new(&p)).transpose();
        Ok(FtpOptions {
            recursive: o.bool("recursive", false)?,
            min_depth: o.i64("minDepth", 0)?.max(0) as usize,
            max_depth: o.i64("maxDepth", i64::from(i32::MAX))?.max(0) as usize,
            include: re(o.get("include"))?,
            exclude: re(o.get("exclude"))?,
            idempotent: o.bool("idempotent", noop)?,
            idempotent_by_etag: o.get("idempotentKey").is_some_and(|k| k.contains("etag") || k.contains("modified")),
            noop,
            repo,
            pre_move: o.get("preMove"),
            move_to: o.get("move"),
            move_failed: o.get("moveFailed"),
            file_exist: o.string("fileExist", "Override"),
            move_existing: o.get("moveExisting"),
            temp_prefix: o.get("tempPrefix"),
            auto_create: o.bool("autoCreate", true)?,
            flatten: o.bool("flatten", false)?,
            explicit,
        })
    }

    /// Whether the FTP-like consumer applies.
    pub fn enabled(&self) -> bool {
        self.explicit
    }
}

/// Target path of a move option for a file at `rel_dir/name` (both relative to the share).
fn move_target(opt: &str, file_dir: &str, name: &str) -> String {
    let dir = match opt.strip_prefix('/') {
        Some(abs) => abs.trim_matches('/').to_string(),
        None => join(Some(file_dir), opt.trim_matches('/')),
    };
    join(Some(&dir), name)
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

impl FilesService {
    /// Create the directories of `dir` (relative to the share) that are missing.
    fn ensure_dirs(&self, share: &str, dir: &str) -> Result<()> {
        let mut acc = String::new();
        for seg in dir.split('/').filter(|s| !s.is_empty()) {
            acc = join(Some(&acc), seg);
            match self.send("PUT", &join(Some(share), &acc), "restype=directory", &[], Vec::new()) {
                Ok(_) => {}
                Err(e) if super::error_code(&e).as_deref() == Some("ResourceAlreadyExists") => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Rename `from` to `to` (paths relative to the share) with the Rename File API.
    pub fn rename(&self, share: &str, from: &str, to: &str, replace: bool) -> Result<()> {
        self.ensure_dirs(share, parent(to))?;
        let src = self.azure.url("file", &crate::util::uri_encode(&join(Some(share), from), true), "")?;
        self.send("PUT", &join(Some(share), to), "comp=rename", &[("x-ms-file-rename-source", src.to_string()), ("x-ms-file-rename-replace-if-exists", replace.to_string())], Vec::new())?;
        Ok(())
    }

    fn exists(&self, share: &str, path: &str) -> Result<Option<i64>> {
        match self.send("HEAD", &join(Some(share), path), "", &[], Vec::new()) {
            Ok(r) => Ok(Some(r.header("Content-Length").and_then(|l| l.parse().ok()).unwrap_or(0))),
            Err(e) if matches!(super::error_code(&e).as_deref(), Some("ResourceNotFound" | "Http404")) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `uploadFile` with the `fileExist` / `tempPrefix` / `autoCreate` / `flatten` semantics.
    pub fn upload_ftp(&self, ex: &mut Exchange) -> Result<()> {
        let share = self.share_of(Some(ex))?;
        let mut path = self.file_path(ex)?;
        if self.ftp.flatten {
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            path = join(header_str(ex, fh::DIRECTORY_NAME).or_else(|| self.directory.clone()).as_deref(), &name);
        }
        let data = ex.body().to_bytes();
        let existing = self.exists(&share, &path)?;
        if let Some(len) = existing {
            match self.ftp.file_exist.to_ascii_lowercase().as_str() {
                "fail" => return Err(crate::util::cloud_error(crate::util::kinds::SHARE_STORAGE, format!("File already exist: {}. Cannot write new file.", path))),
                "ignore" => return Ok(()),
                "append" => {
                    let full = join(Some(&share), &path);
                    let new_len = len as usize + data.len();
                    self.send("PUT", &full, "comp=properties", &[("x-ms-content-length", new_len.to_string())], Vec::new())?;
                    let mut etag = String::new();
                    for (i, chunk) in data.chunks(4 * 1024 * 1024).enumerate() {
                        let start = len as usize + i * 4 * 1024 * 1024;
                        let r = self.send("PUT", &full, "comp=range", &[("x-ms-range", format!("bytes={}-{}", start, start + chunk.len() - 1)), ("x-ms-write", "update".into())], chunk.to_vec())?;
                        etag = r.header("ETag").unwrap_or("").to_string();
                    }
                    ex.set_header(fh::ETAG, etag);
                    return Ok(());
                }
                "move" => {
                    let to = self.ftp.move_existing.clone().ok_or_else(|| CamelError::illegal_argument("fileExist=Move requires the moveExisting option"))?;
                    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                    self.rename(&share, &path, &move_target(&to, parent(&path), &name), true)?;
                }
                _ => {}
            }
        } else if self.ftp.auto_create {
            self.ensure_dirs(&share, parent(&path))?;
        }
        let etag = match &self.ftp.temp_prefix {
            Some(prefix) => {
                let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                let tmp = join(Some(parent(&path)), &format!("{}{}", prefix, name));
                let e = self.upload(&share, &tmp, &data, header_str(ex, fh::CONTENT_TYPE))?;
                self.rename(&share, &tmp, &path, true)?;
                e
            }
            None => self.upload(&share, &path, &data, header_str(ex, fh::CONTENT_TYPE))?,
        };
        ex.set_header(fh::ETAG, etag);
        ex.set_header("CamelFileNameProduced", path);
        Ok(())
    }

    /// The `renameFile` operation.
    pub fn rename_op(&self, ex: &mut Exchange) -> Result<()> {
        let share = self.share_of(Some(ex))?;
        let from = self.file_path(ex)?;
        let to = header_str(ex, headers::RENAME_TO).ok_or_else(|| CamelError::illegal_argument("renameFile requires the CamelAzureStorageFilesRenameTo header"))?;
        let replace = self.ftp.file_exist.eq_ignore_ascii_case("override");
        self.rename(&share, &from, to.trim_start_matches('/'), replace)?;
        ex.set_header("CamelFileNameProduced", to);
        Ok(())
    }

    /// Files (paths relative to the share, relative to the polled directory) to consume.
    fn walk(&self, share: &str, base: &str, rel: &str, depth: usize, out: &mut Vec<(String, String)>) -> Result<()> {
        let dir = join(Some(base), rel);
        for e in self.list(share, Some(&dir).filter(|d| !d.is_empty()).map(|d| d.as_str()), self.prefix.as_deref())? {
            let name = e.get("name").map(|n| n.to_string()).unwrap_or_default();
            let child_rel = join(Some(rel), &name);
            if e.get("type").map(|t| t.to_string()).as_deref() == Some("directory") {
                let skip = [&self.ftp.move_to, &self.ftp.move_failed, &self.ftp.pre_move].iter().any(|m| m.as_deref().is_some_and(|m| !m.starts_with('/') && m.trim_matches('/') == name));
                if self.ftp.recursive && depth < self.ftp.max_depth && !skip {
                    self.walk(share, base, &child_rel, depth + 1, out)?;
                }
                continue;
            }
            if depth < self.ftp.min_depth || depth > self.ftp.max_depth {
                continue;
            }
            if self.ftp.include.as_ref().is_some_and(|r| !full_match(r, &name)) || self.ftp.exclude.as_ref().is_some_and(|r| full_match(r, &name)) {
                continue;
            }
            out.push((join(Some(base), &child_rel), child_rel));
        }
        Ok(())
    }

    /// The FTP-like consumer poll.
    pub fn poll_ftp(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let share = self.share_of(None)?;
        let base = self.directory.clone().unwrap_or_default().trim_matches('/').to_string();
        let mut files = Vec::new();
        self.walk(&share, &base, "", 1, &mut files)?;
        let mut batch = Vec::new();
        for (path, rel) in files {
            if self.max_messages_per_poll > 0 && batch.len() as i64 >= self.max_messages_per_poll {
                break;
            }
            let r = self.send("GET", &join(Some(&share), &path), "", &[], Vec::new())?;
            let key = if self.ftp.idempotent_by_etag { format!("{}-{}", rel, r.header("ETag").unwrap_or("")) } else { rel.clone() };
            if self.ftp.idempotent && !self.ftp.repo.add(&key) {
                continue;
            }
            batch.push((path, rel, key, r));
        }
        let total = batch.len();
        for (i, (mut path, rel, key, r)) in batch.into_iter().enumerate() {
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            if let Some(pm) = &self.ftp.pre_move {
                let to = move_target(pm, parent(&path), &name);
                self.rename(&share, &path, &to, true)?;
                path = to;
            }
            let mut ex = Exchange::new(ctx.clone());
            Self::file_headers(&mut ex, &r);
            ex.set_header(fh::FILE_NAME, rel.clone());
            ex.set_header(fh::SHARE_NAME, share.clone());
            if !base.is_empty() {
                ex.set_header(fh::DIRECTORY_NAME, base.clone());
            }
            ex.set_header("CamelFileName", rel.clone());
            ex.set_header("CamelFileNameOnly", name.clone());
            ex.set_header("CamelFileRelativePath", rel.clone());
            ex.set_header("CamelFileParent", parent(&path).to_string());
            ex.set_header("CamelFileLength", r.body.len() as i64);
            ex.set_body(Value::Bytes(r.body));
            set_batch(&mut ex, i, total);
            let ok = process(logger, processor, &mut ex);
            let result = if ok {
                if self.delete {
                    self.send("DELETE", &join(Some(&share), &path), "", &[], Vec::new()).map(|_| ())
                } else if let Some(m) = &self.ftp.move_to {
                    self.rename(&share, &path, &move_target(m, parent(&path), &name), true)
                } else {
                    Ok(())
                }
            } else {
                if self.ftp.idempotent {
                    // a failed file is retried on the next poll unless it is moved away
                    self.ftp.repo.remove(&key);
                }
                match &self.ftp.move_failed {
                    Some(m) => self.rename(&share, &path, &move_target(m, parent(&path), &name), true),
                    None => Ok(()),
                }
            };
            if let Err(e) = result {
                cameleer_core::camel_warn!(logger, "Error completing Azure file {}: {}", path, e);
            }
        }
        Ok(total)
    }
}
