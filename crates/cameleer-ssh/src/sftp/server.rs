//! Server side of SFTP v3 serving a local directory as the file system root
//! (the user's home directory is `/`). Paths are confined to the root: `..`
//! never climbs above it, and paths whose canonical location (after following
//! symbolic links) leaves the root are refused. `SYMLINK` is not supported.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use super::{fxp, open_flags, status, FileAttrs};
use crate::wire::{Reader, Writer};

enum Handle {
    File { file: File, append: bool },
    Dir { entries: Vec<(String, FileAttrs)>, pos: usize },
}

/// An SFTP server session: feed it channel data, send back what it returns.
pub struct SftpServer {
    root: PathBuf,
    canonical_root: PathBuf,
    handles: HashMap<u32, Handle>,
    next_handle: u32,
    buf: Vec<u8>,
}

/// Attributes of a local file.
pub fn attrs_of(meta: &fs::Metadata) -> FileAttrs {
    let secs = |t: std::io::Result<std::time::SystemTime>| t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as u32).unwrap_or(0);
    #[cfg(unix)]
    let (uid, gid, mode) = {
        use std::os::unix::fs::MetadataExt;
        (meta.uid(), meta.gid(), meta.mode())
    };
    #[cfg(not(unix))]
    let (uid, gid, mode) = (0, 0, if meta.is_dir() { 0o040755 } else { 0o100644 });
    FileAttrs { size: Some(meta.len()), uid: Some(uid), gid: Some(gid), permissions: Some(mode), atime: Some(secs(meta.accessed())), mtime: Some(secs(meta.modified())) }
}

fn mode_string(mode: u32) -> String {
    let t = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        _ => '-',
    };
    let mut s = String::from(t);
    for shift in [6, 3, 0] {
        let b = (mode >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}

/// `ls -l` style line for a directory entry.
pub fn long_name(name: &str, a: &FileAttrs) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mtime = a.mtime.unwrap_or(0) as i64;
    let (_, m, d) = cameleer_tls::asn1::civil_from_days(mtime.div_euclid(86400));
    let secs = mtime.rem_euclid(86400);
    format!(
        "{} {:>4} {:<8} {:<8} {:>8} {} {:>2} {:02}:{:02} {}",
        mode_string(a.permissions.unwrap_or(0)),
        1,
        a.uid.unwrap_or(0),
        a.gid.unwrap_or(0),
        a.size.unwrap_or(0),
        MONTHS[(m as usize).clamp(1, 12) - 1],
        d,
        secs / 3600,
        (secs / 60) % 60,
        name
    )
}

fn io_status(e: &std::io::Error) -> u32 {
    match e.kind() {
        ErrorKind::NotFound => status::NO_SUCH_FILE,
        ErrorKind::PermissionDenied => status::PERMISSION_DENIED,
        _ => status::FAILURE,
    }
}

type Reply = Result<Vec<u8>, (u32, String)>;

fn fail<T>(code: u32, msg: impl Into<String>) -> Result<T, (u32, String)> {
    Err((code, msg.into()))
}

impl SftpServer {
    /// Serves `root` (which must exist).
    pub fn new(root: &Path) -> SftpServer {
        let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        SftpServer { root: root.to_path_buf(), canonical_root, handles: HashMap::new(), next_handle: 1, buf: Vec::new() }
    }

    /// Normalised virtual path (`/a/b`) of a client path.
    pub fn virtual_path(path: &str) -> String {
        let mut segs: Vec<&str> = Vec::new();
        for s in path.split('/') {
            match s {
                "" | "." => {}
                ".." => {
                    segs.pop();
                }
                s => segs.push(s),
            }
        }
        format!("/{}", segs.join("/"))
    }

    fn local(&self, path: &str) -> Result<PathBuf, (u32, String)> {
        let v = Self::virtual_path(path);
        let p = self.root.join(v.trim_start_matches('/'));
        // refuse locations outside the root (symbolic links)
        let probe = if p.exists() { Some(p.clone()) } else { p.parent().map(|x| x.to_path_buf()) };
        if let Some(probe) = probe {
            if let Ok(c) = fs::canonicalize(&probe) {
                if !c.starts_with(&self.canonical_root) {
                    return fail(status::PERMISSION_DENIED, format!("{}: outside the served directory", v));
                }
            }
        }
        Ok(p)
    }

    /// Consumes channel data; returns the bytes to send back (complete responses).
    pub fn feed(&mut self, data: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while self.buf.len() >= 4 {
            let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
            if self.buf.len() < 4 + len {
                break;
            }
            let pkt: Vec<u8> = self.buf.drain(..4 + len).skip(4).collect();
            if pkt.is_empty() {
                continue;
            }
            let body = self.handle(pkt[0], &pkt[1..]);
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            out.extend_from_slice(&body);
        }
        out
    }

    fn handle(&mut self, t: u8, body: &[u8]) -> Vec<u8> {
        if t == fxp::INIT {
            let mut w = Writer::msg(fxp::VERSION);
            w.u32(3).str("posix-rename@openssh.com").str("1");
            return w.finish();
        }
        let mut r = Reader::new(body);
        let id = match r.u32() {
            Ok(id) => id,
            Err(_) => return Writer::msg(fxp::STATUS).u32(0).u32(status::BAD_MESSAGE).str("bad packet").str("").finish(),
        };
        match self.dispatch(t, id, &mut r) {
            Ok(reply) => reply,
            Err((code, msg)) => Writer::msg(fxp::STATUS).u32(id).u32(code).str(&msg).str("en").finish(),
        }
    }

    fn ok(id: u32) -> Vec<u8> {
        Writer::msg(fxp::STATUS).u32(id).u32(status::OK).str("Success").str("en").finish()
    }

    fn names(id: u32, entries: &[(String, FileAttrs)]) -> Vec<u8> {
        let mut w = Writer::msg(fxp::NAME);
        w.u32(id).u32(entries.len() as u32);
        for (n, a) in entries {
            w.str(n).str(&long_name(n, a));
            a.encode(&mut w);
        }
        w.finish()
    }

    fn handle_of(&mut self, r: &mut Reader) -> Result<(u32, &mut Handle), (u32, String)> {
        let h = r.string().map_err(|_| (status::BAD_MESSAGE, "bad handle".to_string()))?;
        let key = if h.len() == 4 { u32::from_be_bytes([h[0], h[1], h[2], h[3]]) } else { 0 };
        match self.handles.get_mut(&key) {
            Some(x) => Ok((key, x)),
            None => fail(status::FAILURE, "invalid handle"),
        }
    }

    fn new_handle(&mut self, h: Handle) -> Vec<u8> {
        let k = self.next_handle;
        self.next_handle += 1;
        self.handles.insert(k, h);
        k.to_be_bytes().to_vec()
    }

    fn dispatch(&mut self, t: u8, id: u32, r: &mut Reader) -> Reply {
        let bad = |_| (status::BAD_MESSAGE, "malformed request".to_string());
        let io = |e: std::io::Error| (io_status(&e), e.to_string());
        match t {
            fxp::OPEN => {
                let path = r.str().map_err(bad)?;
                let flags = r.u32().map_err(bad)?;
                let attrs = FileAttrs::decode(r).map_err(bad)?;
                let p = self.local(&path)?;
                let mut o = OpenOptions::new();
                o.read(flags & open_flags::READ != 0).write(flags & (open_flags::WRITE | open_flags::APPEND) != 0);
                if flags & open_flags::APPEND != 0 {
                    o.append(true);
                }
                if flags & open_flags::CREAT != 0 {
                    if flags & open_flags::EXCL != 0 {
                        o.create_new(true);
                    } else {
                        o.create(true);
                    }
                }
                if flags & open_flags::TRUNC != 0 {
                    o.truncate(true);
                }
                let file = o.open(&p).map_err(|e| (io_status(&e), format!("{}: {}", path, e)))?;
                if let Some(perm) = attrs.permissions {
                    set_mode(&p, perm);
                }
                let h = self.new_handle(Handle::File { file, append: flags & open_flags::APPEND != 0 });
                Ok(Writer::msg(fxp::HANDLE).u32(id).string(&h).finish())
            }
            fxp::CLOSE => {
                let (k, _) = self.handle_of(r)?;
                self.handles.remove(&k);
                Ok(Self::ok(id))
            }
            fxp::READ => {
                let (_, h) = self.handle_of(r)?;
                let off = r.u64().map_err(bad)?;
                let len = (r.u32().map_err(bad)? as usize).min(256 * 1024);
                let file = match h {
                    Handle::File { file, .. } => file,
                    _ => return fail(status::FAILURE, "not a file handle"),
                };
                file.seek(SeekFrom::Start(off)).map_err(io)?;
                let mut data = vec![0u8; len];
                let mut n = 0;
                while n < len {
                    let k = file.read(&mut data[n..]).map_err(io)?;
                    if k == 0 {
                        break;
                    }
                    n += k;
                }
                if n == 0 && len > 0 {
                    return fail(status::EOF, "End of file");
                }
                data.truncate(n);
                Ok(Writer::msg(fxp::DATA).u32(id).string(&data).finish())
            }
            fxp::WRITE => {
                let (_, h) = self.handle_of(r)?;
                let off = r.u64().map_err(bad)?;
                let data = r.string().map_err(bad)?;
                match h {
                    Handle::File { file, append } => {
                        if !*append {
                            file.seek(SeekFrom::Start(off)).map_err(io)?;
                        }
                        file.write_all(data).map_err(io)?;
                    }
                    _ => return fail(status::FAILURE, "not a file handle"),
                }
                Ok(Self::ok(id))
            }
            fxp::STAT | fxp::LSTAT => {
                let path = r.str().map_err(bad)?;
                let p = self.local(&path)?;
                let m = if t == fxp::STAT { fs::metadata(&p) } else { fs::symlink_metadata(&p) };
                let m = m.map_err(|e| (io_status(&e), format!("{}: {}", path, e)))?;
                let mut w = Writer::msg(fxp::ATTRS);
                w.u32(id);
                attrs_of(&m).encode(&mut w);
                Ok(w.finish())
            }
            fxp::FSTAT => {
                let (_, h) = self.handle_of(r)?;
                let m = match h {
                    Handle::File { file, .. } => file.metadata().map_err(io)?,
                    _ => return fail(status::FAILURE, "not a file handle"),
                };
                let mut w = Writer::msg(fxp::ATTRS);
                w.u32(id);
                attrs_of(&m).encode(&mut w);
                Ok(w.finish())
            }
            fxp::SETSTAT | fxp::FSETSTAT => {
                let p = if t == fxp::SETSTAT {
                    let path = r.str().map_err(bad)?;
                    self.local(&path)?
                } else {
                    // FSETSTAT: apply through the open file
                    let (_, h) = self.handle_of(r)?;
                    let attrs = FileAttrs::decode(r).map_err(bad)?;
                    if let Handle::File { file, .. } = h {
                        if let Some(s) = attrs.size {
                            file.set_len(s).map_err(io)?;
                        }
                        if let Some(m) = attrs.mtime {
                            file.set_modified(UNIX_EPOCH + Duration::from_secs(m as u64)).map_err(io)?;
                        }
                    }
                    return Ok(Self::ok(id));
                };
                let attrs = FileAttrs::decode(r).map_err(bad)?;
                if let Some(s) = attrs.size {
                    OpenOptions::new().write(true).open(&p).and_then(|f| f.set_len(s)).map_err(io)?;
                }
                if let Some(perm) = attrs.permissions {
                    if !set_mode(&p, perm) {
                        return fail(status::FAILURE, "cannot change permissions");
                    }
                }
                if let Some(m) = attrs.mtime {
                    File::options()
                        .write(true)
                        .open(&p)
                        .and_then(|f| f.set_modified(UNIX_EPOCH + Duration::from_secs(m as u64)))
                        .or_else(|e| if p.is_dir() { Ok(()) } else { Err(e) })
                        .map_err(io)?;
                }
                Ok(Self::ok(id))
            }
            fxp::OPENDIR => {
                let path = r.str().map_err(bad)?;
                let p = self.local(&path)?;
                let rd = fs::read_dir(&p).map_err(|e| (io_status(&e), format!("{}: {}", path, e)))?;
                let mut entries = Vec::new();
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if let Ok(m) = fs::symlink_metadata(e.path()) {
                        entries.push((name, attrs_of(&m)));
                    }
                }
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                let h = self.new_handle(Handle::Dir { entries, pos: 0 });
                Ok(Writer::msg(fxp::HANDLE).u32(id).string(&h).finish())
            }
            fxp::READDIR => {
                let (_, h) = self.handle_of(r)?;
                match h {
                    Handle::Dir { entries, pos } => {
                        if *pos >= entries.len() {
                            return fail(status::EOF, "End of directory");
                        }
                        let end = (*pos + 64).min(entries.len());
                        let batch = entries[*pos..end].to_vec();
                        *pos = end;
                        Ok(Self::names(id, &batch))
                    }
                    _ => fail(status::FAILURE, "not a directory handle"),
                }
            }
            fxp::REMOVE => {
                let path = r.str().map_err(bad)?;
                let p = self.local(&path)?;
                if p.is_dir() {
                    return fail(status::FAILURE, format!("{}: is a directory", path));
                }
                fs::remove_file(&p).map_err(|e| (io_status(&e), format!("{}: {}", path, e)))?;
                Ok(Self::ok(id))
            }
            fxp::MKDIR => {
                let path = r.str().map_err(bad)?;
                let attrs = FileAttrs::decode(r).map_err(bad)?;
                let p = self.local(&path)?;
                fs::create_dir(&p).map_err(|e| (io_status(&e), format!("{}: {}", path, e)))?;
                if let Some(perm) = attrs.permissions {
                    set_mode(&p, perm);
                }
                Ok(Self::ok(id))
            }
            fxp::RMDIR => {
                let path = r.str().map_err(bad)?;
                let p = self.local(&path)?;
                fs::remove_dir(&p).map_err(|e| (io_status(&e), format!("{}: {}", path, e)))?;
                Ok(Self::ok(id))
            }
            fxp::REALPATH => {
                let path = r.str().map_err(bad)?;
                let v = Self::virtual_path(&path);
                Ok(Self::names(id, &[(v, FileAttrs::default())]))
            }
            fxp::RENAME => {
                let from = r.str().map_err(bad)?;
                let to = r.str().map_err(bad)?;
                let (pf, pt) = (self.local(&from)?, self.local(&to)?);
                if fs::symlink_metadata(&pt).is_ok() {
                    return fail(status::FAILURE, format!("{}: target exists", to));
                }
                fs::rename(&pf, &pt).map_err(|e| (io_status(&e), format!("{}: {}", from, e)))?;
                Ok(Self::ok(id))
            }
            fxp::READLINK => {
                let path = r.str().map_err(bad)?;
                let p = self.local(&path)?;
                let target = fs::read_link(&p).map_err(|e| (io_status(&e), format!("{}: {}", path, e)))?;
                Ok(Self::names(id, &[(target.to_string_lossy().into_owned(), FileAttrs::default())]))
            }
            fxp::EXTENDED => {
                let name = r.str().map_err(bad)?;
                if name == "posix-rename@openssh.com" {
                    let from = r.str().map_err(bad)?;
                    let to = r.str().map_err(bad)?;
                    let (pf, pt) = (self.local(&from)?, self.local(&to)?);
                    fs::rename(&pf, &pt).map_err(|e| (io_status(&e), format!("{}: {}", from, e)))?;
                    return Ok(Self::ok(id));
                }
                fail(status::OP_UNSUPPORTED, format!("extension {} not supported", name))
            }
            _ => fail(status::OP_UNSUPPORTED, format!("operation {} not supported", t)),
        }
    }
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode & 0o7777)).is_ok()
}

#[cfg(not(unix))]
fn set_mode(_p: &Path, _mode: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_paths_stay_in_root() {
        assert_eq!(SftpServer::virtual_path("."), "/");
        assert_eq!(SftpServer::virtual_path("../../etc/passwd"), "/etc/passwd");
        assert_eq!(SftpServer::virtual_path("/a/./b/../c"), "/a/c");
        assert_eq!(mode_string(0o100644), "-rw-r--r--");
        assert_eq!(mode_string(0o040755), "drwxr-xr-x");
        let l = long_name("x.txt", &FileAttrs { size: Some(5), mtime: Some(0), permissions: Some(0o100600), ..Default::default() });
        assert!(l.starts_with("-rw------- ") && l.ends_with(" Jan  1 00:00 x.txt"), "{}", l);
    }
}
