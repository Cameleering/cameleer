//! SFTP v3 client over an SSH `sftp` subsystem channel, with pipelined
//! (several outstanding requests) reads and writes.

use std::collections::{BTreeMap, HashMap};

use super::{fxp, open_flags, status, FileAttrs};
use crate::client::{ClientConfig, SshClient};
use crate::error::{SshError, SshResult};
use crate::wire::{Reader, Writer};

/// One directory entry returned by `READDIR`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    /// `ls -l` style line as sent by the server.
    pub longname: String,
    pub attrs: FileAttrs,
}

const MAX_SFTP_PACKET: usize = 256 * 1024 + 1024;

/// An SFTP session.
pub struct SftpClient {
    ssh: SshClient,
    chan: u32,
    rbuf: Vec<u8>,
    next_id: u32,
    version: u32,
    extensions: Vec<(String, String)>,
    /// Maximum number of outstanding read/write requests (Camel `bulkRequests`).
    pub max_outstanding: usize,
    /// Bytes per READ / WRITE request.
    pub chunk_size: usize,
}

fn status_err(code: u32, message: String) -> SshError {
    SshError::Sftp { code, message }
}

impl SftpClient {
    /// Connects, authenticates and starts the `sftp` subsystem.
    pub fn connect(cfg: &ClientConfig) -> SshResult<SftpClient> {
        SftpClient::new(SshClient::connect(cfg)?)
    }

    /// Starts the `sftp` subsystem on an existing connection.
    pub fn new(mut ssh: SshClient) -> SshResult<SftpClient> {
        let chan = ssh.subsystem("sftp")?;
        let mut c = SftpClient {
            ssh,
            chan,
            rbuf: Vec::new(),
            next_id: 1,
            version: 0,
            extensions: Vec::new(),
            max_outstanding: 16,
            chunk_size: 32768,
        };
        let mut init = Writer::new();
        init.u32(5).u8(fxp::INIT).u32(3);
        c.ssh.write(chan, &init.buf)?;
        let (t, body) = c.recv_packet()?;
        if t != fxp::VERSION {
            return Err(SshError::protocol(format!("expected SSH_FXP_VERSION, got {}", t)));
        }
        let mut r = Reader::new(&body);
        c.version = r.u32()?;
        while !r.is_empty() {
            let name = r.str()?;
            let data = r.str()?;
            c.extensions.push((name, data));
        }
        Ok(c)
    }

    /// The protocol version announced by the server.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Extensions announced by the server (`name`, `data`).
    pub fn extensions(&self) -> &[(String, String)] {
        &self.extensions
    }

    /// Whether the server announced an extension.
    pub fn has_extension(&self, name: &str) -> bool {
        self.extensions.iter().any(|(n, _)| n == name)
    }

    /// The underlying SSH connection.
    pub fn ssh(&mut self) -> &mut SshClient {
        &mut self.ssh
    }

    /// Closes the subsystem channel and the connection.
    pub fn close(mut self) {
        let _ = self.ssh.close_channel(self.chan);
        self.ssh.disconnect();
    }

    // ------------------------------------------------------------ packets

    fn recv_packet(&mut self) -> SshResult<(u8, Vec<u8>)> {
        loop {
            if self.rbuf.len() >= 4 {
                let len = u32::from_be_bytes([self.rbuf[0], self.rbuf[1], self.rbuf[2], self.rbuf[3]]) as usize;
                if len == 0 || len > MAX_SFTP_PACKET {
                    return Err(SshError::protocol(format!("invalid SFTP packet length {}", len)));
                }
                if self.rbuf.len() >= 4 + len {
                    let t = self.rbuf[4];
                    let body = self.rbuf[5..4 + len].to_vec();
                    self.rbuf.drain(..4 + len);
                    return Ok((t, body));
                }
            }
            let more = self.ssh.read_some(self.chan)?;
            if more.is_empty() {
                return Err(status_err(status::CONNECTION_LOST, "SFTP channel closed by the server".into()));
            }
            self.rbuf.extend_from_slice(&more);
        }
    }

    /// Sends a request built by `f` (after type and id); returns its id.
    fn send(&mut self, t: u8, f: impl FnOnce(&mut Writer)) -> SshResult<u32> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let mut body = Writer::new();
        body.u8(t).u32(id);
        f(&mut body);
        let mut pkt = Writer::new();
        pkt.string(&body.buf);
        self.ssh.write(self.chan, &pkt.buf)?;
        Ok(id)
    }

    /// Receives the next response: (id, type, rest of the body after the id).
    fn recv_response(&mut self) -> SshResult<(u32, u8, Vec<u8>)> {
        let (t, body) = self.recv_packet()?;
        if body.len() < 4 {
            return Err(SshError::protocol("short SFTP response"));
        }
        let id = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
        Ok((id, t, body[4..].to_vec()))
    }

    fn reply(&mut self, id: u32) -> SshResult<(u8, Vec<u8>)> {
        loop {
            let (rid, t, rest) = self.recv_response()?;
            if rid == id {
                return Ok((t, rest));
            }
            // a stale reply of an abandoned pipelined request: skip it
        }
    }

    fn parse_status(rest: &[u8]) -> (u32, String) {
        let mut r = Reader::new(rest);
        let code = r.u32().unwrap_or(status::FAILURE);
        let msg = r.str().unwrap_or_default();
        (code, msg)
    }

    fn expect_status(&mut self, id: u32, what: &str) -> SshResult<()> {
        let (t, rest) = self.reply(id)?;
        if t != fxp::STATUS {
            return Err(SshError::protocol(format!("expected SSH_FXP_STATUS for {}, got {}", what, t)));
        }
        match Self::parse_status(&rest) {
            (status::OK, _) => Ok(()),
            (code, m) => Err(status_err(code, format!("{}: {}", what, m))),
        }
    }

    fn unexpected(t: u8, rest: &[u8], what: &str) -> SshError {
        if t == fxp::STATUS {
            let (code, m) = Self::parse_status(rest);
            status_err(code, format!("{}: {}", what, m))
        } else {
            SshError::protocol(format!("unexpected SFTP response {} for {}", t, what))
        }
    }

    fn expect_handle(&mut self, id: u32, what: &str) -> SshResult<Vec<u8>> {
        let (t, rest) = self.reply(id)?;
        if t == fxp::HANDLE {
            return Ok(Reader::new(&rest).string()?.to_vec());
        }
        Err(Self::unexpected(t, &rest, what))
    }

    fn expect_attrs(&mut self, id: u32, what: &str) -> SshResult<FileAttrs> {
        let (t, rest) = self.reply(id)?;
        if t == fxp::ATTRS {
            return FileAttrs::decode(&mut Reader::new(&rest));
        }
        Err(Self::unexpected(t, &rest, what))
    }

    fn expect_names(&mut self, id: u32, what: &str) -> SshResult<Option<Vec<DirEntry>>> {
        let (t, rest) = self.reply(id)?;
        if t == fxp::NAME {
            let mut r = Reader::new(&rest);
            let n = r.u32()?;
            let mut out = Vec::with_capacity(n.min(1024) as usize);
            for _ in 0..n {
                let name = r.str()?;
                let longname = r.str()?;
                let attrs = FileAttrs::decode(&mut r)?;
                out.push(DirEntry { name, longname, attrs });
            }
            return Ok(Some(out));
        }
        if t == fxp::STATUS && Self::parse_status(&rest).0 == status::EOF {
            return Ok(None);
        }
        Err(Self::unexpected(t, &rest, what))
    }

    // ------------------------------------------------------------ operations

    /// Canonicalises a path on the server (`"."` gives the home directory).
    pub fn realpath(&mut self, path: &str) -> SshResult<String> {
        let id = self.send(fxp::REALPATH, |w| {
            w.str(path);
        })?;
        let names = self.expect_names(id, &format!("realpath {}", path))?.unwrap_or_default();
        names.into_iter().next().map(|e| e.name).ok_or_else(|| SshError::protocol("empty REALPATH reply"))
    }

    /// Attributes of a path, following symbolic links.
    pub fn stat(&mut self, path: &str) -> SshResult<FileAttrs> {
        let id = self.send(fxp::STAT, |w| {
            w.str(path);
        })?;
        self.expect_attrs(id, &format!("stat {}", path))
    }

    /// Attributes of a path, not following symbolic links.
    pub fn lstat(&mut self, path: &str) -> SshResult<FileAttrs> {
        let id = self.send(fxp::LSTAT, |w| {
            w.str(path);
        })?;
        self.expect_attrs(id, &format!("lstat {}", path))
    }

    /// Attributes of an open handle.
    pub fn fstat(&mut self, handle: &[u8]) -> SshResult<FileAttrs> {
        let id = self.send(fxp::FSTAT, |w| {
            w.string(handle);
        })?;
        self.expect_attrs(id, "fstat")
    }

    /// Changes attributes (permissions, times, size) of a path.
    pub fn setstat(&mut self, path: &str, attrs: &FileAttrs) -> SshResult<()> {
        let id = self.send(fxp::SETSTAT, |w| {
            w.str(path);
            attrs.encode(w);
        })?;
        self.expect_status(id, &format!("setstat {}", path))
    }

    /// Whether a path exists (`STAT` succeeds).
    pub fn exists(&mut self, path: &str) -> SshResult<bool> {
        match self.stat(path) {
            Ok(_) => Ok(true),
            Err(SshError::Sftp { code: status::NO_SUCH_FILE, .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Opens a file with `SSH_FXF_*` flags; returns the handle.
    pub fn open(&mut self, path: &str, flags: u32, attrs: &FileAttrs) -> SshResult<Vec<u8>> {
        let id = self.send(fxp::OPEN, |w| {
            w.str(path).u32(flags);
            attrs.encode(w);
        })?;
        self.expect_handle(id, &format!("open {}", path))
    }

    /// Closes a file or directory handle.
    pub fn close_handle(&mut self, handle: &[u8]) -> SshResult<()> {
        let id = self.send(fxp::CLOSE, |w| {
            w.string(handle);
        })?;
        self.expect_status(id, "close")
    }

    /// Reads up to `len` bytes at `offset`; `None` at end of file.
    pub fn read_at(&mut self, handle: &[u8], offset: u64, len: u32) -> SshResult<Option<Vec<u8>>> {
        let id = self.send(fxp::READ, |w| {
            w.string(handle).u64(offset).u32(len);
        })?;
        let (t, rest) = self.reply(id)?;
        match t {
            fxp::DATA => Ok(Some(Reader::new(&rest).string()?.to_vec())),
            fxp::STATUS if Self::parse_status(&rest).0 == status::EOF => Ok(None),
            _ => Err(Self::unexpected(t, &rest, "read")),
        }
    }

    /// Writes `data` at `offset`.
    pub fn write_at(&mut self, handle: &[u8], offset: u64, data: &[u8]) -> SshResult<()> {
        let id = self.send(fxp::WRITE, |w| {
            w.string(handle).u64(offset).string(data);
        })?;
        self.expect_status(id, "write")
    }

    /// Lists a directory (without `.` and `..`).
    pub fn read_dir(&mut self, path: &str) -> SshResult<Vec<DirEntry>> {
        let id = self.send(fxp::OPENDIR, |w| {
            w.str(path);
        })?;
        let handle = self.expect_handle(id, &format!("opendir {}", path))?;
        let mut out = Vec::new();
        let r = loop {
            let id = self.send(fxp::READDIR, |w| {
                w.string(&handle);
            })?;
            match self.expect_names(id, &format!("readdir {}", path)) {
                Ok(Some(entries)) => out.extend(entries.into_iter().filter(|e| e.name != "." && e.name != "..")),
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        let c = self.close_handle(&handle);
        r?;
        c?;
        Ok(out)
    }

    /// Removes a file.
    pub fn remove(&mut self, path: &str) -> SshResult<()> {
        let id = self.send(fxp::REMOVE, |w| {
            w.str(path);
        })?;
        self.expect_status(id, &format!("remove {}", path))
    }

    /// Creates a directory.
    pub fn mkdir(&mut self, path: &str) -> SshResult<()> {
        let id = self.send(fxp::MKDIR, |w| {
            w.str(path);
            FileAttrs::default().encode(w);
        })?;
        self.expect_status(id, &format!("mkdir {}", path))
    }

    /// Creates a directory and its missing parents.
    pub fn mkdirs(&mut self, path: &str) -> SshResult<()> {
        let abs = path.starts_with('/');
        let mut cur = String::new();
        for seg in path.split('/').filter(|s| !s.is_empty() && *s != ".") {
            if abs || !cur.is_empty() {
                cur.push('/');
            }
            cur.push_str(seg);
            match self.stat(&cur) {
                Ok(a) if a.is_dir() => continue,
                Ok(_) => return Err(status_err(status::FAILURE, format!("mkdirs {}: {} is not a directory", path, cur))),
                Err(SshError::Sftp { code: status::NO_SUCH_FILE, .. }) => {
                    if let Err(e) = self.mkdir(&cur) {
                        // a concurrent creator may have won the race
                        if !self.stat(&cur).map(|a| a.is_dir()).unwrap_or(false) {
                            return Err(e);
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Removes an (empty) directory.
    pub fn rmdir(&mut self, path: &str) -> SshResult<()> {
        let id = self.send(fxp::RMDIR, |w| {
            w.str(path);
        })?;
        self.expect_status(id, &format!("rmdir {}", path))
    }

    /// SFTP v3 `RENAME` (fails when the target exists on most servers).
    pub fn rename(&mut self, from: &str, to: &str) -> SshResult<()> {
        let id = self.send(fxp::RENAME, |w| {
            w.str(from).str(to);
        })?;
        self.expect_status(id, &format!("rename {} to {}", from, to))
    }

    /// `posix-rename@openssh.com`: atomically replaces an existing target.
    pub fn posix_rename(&mut self, from: &str, to: &str) -> SshResult<()> {
        if !self.has_extension("posix-rename@openssh.com") {
            return Err(status_err(status::OP_UNSUPPORTED, "posix-rename@openssh.com not supported by the server".into()));
        }
        let id = self.send(fxp::EXTENDED, |w| {
            w.str("posix-rename@openssh.com").str(from).str(to);
        })?;
        self.expect_status(id, &format!("posix-rename {} to {}", from, to))
    }

    /// Renames, replacing an existing target (posix-rename when available, else remove + rename).
    pub fn rename_overwrite(&mut self, from: &str, to: &str) -> SshResult<()> {
        if self.has_extension("posix-rename@openssh.com") {
            return self.posix_rename(from, to);
        }
        if self.exists(to)? {
            self.remove(to)?;
        }
        self.rename(from, to)
    }

    /// Reads a symbolic link target.
    pub fn readlink(&mut self, path: &str) -> SshResult<String> {
        let id = self.send(fxp::READLINK, |w| {
            w.str(path);
        })?;
        let names = self.expect_names(id, &format!("readlink {}", path))?.unwrap_or_default();
        names.into_iter().next().map(|e| e.name).ok_or_else(|| SshError::protocol("empty READLINK reply"))
    }

    /// Reads a whole file with pipelined READ requests.
    pub fn read_file(&mut self, path: &str) -> SshResult<Vec<u8>> {
        let handle = self.open(path, open_flags::READ, &FileAttrs::default())?;
        let r = self.read_all(&handle);
        let c = self.close_handle(&handle);
        let data = r?;
        c?;
        Ok(data)
    }

    fn read_all(&mut self, handle: &[u8]) -> SshResult<Vec<u8>> {
        let chunk = self.chunk_size.max(1) as u64;
        let mut next_off = 0u64;
        let mut eof = false;
        let mut outstanding: HashMap<u32, (u64, u64)> = HashMap::new();
        let mut chunks: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        let mut retry: Vec<(u64, u64)> = Vec::new();
        let mut error = None;
        loop {
            while error.is_none() && outstanding.len() < self.max_outstanding.max(1) {
                let (off, len) = match retry.pop() {
                    Some(r) => r,
                    None if !eof => {
                        let r = (next_off, chunk);
                        next_off += chunk;
                        r
                    }
                    None => break,
                };
                let id = self.send(fxp::READ, |w| {
                    w.string(handle).u64(off).u32(len as u32);
                })?;
                outstanding.insert(id, (off, len));
            }
            if outstanding.is_empty() {
                break;
            }
            let (id, t, rest) = self.recv_response()?;
            let (off, len) = match outstanding.remove(&id) {
                Some(x) => x,
                None => continue,
            };
            match t {
                fxp::DATA => {
                    let d = Reader::new(&rest).string()?.to_vec();
                    let n = d.len() as u64;
                    if n == 0 {
                        eof = true;
                    } else {
                        if n < len {
                            // short read: request the remainder
                            retry.push((off + n, len - n));
                        }
                        chunks.insert(off, d);
                    }
                }
                fxp::STATUS => {
                    let (code, m) = Self::parse_status(&rest);
                    if code == status::EOF {
                        eof = true;
                        retry.retain(|(o, _)| *o < off);
                    } else if error.is_none() {
                        error = Some(status_err(code, format!("read: {}", m)));
                    }
                }
                _ => {
                    if error.is_none() {
                        error = Some(SshError::protocol(format!("unexpected SFTP response {} for read", t)));
                    }
                }
            }
        }
        if let Some(e) = error {
            return Err(e);
        }
        let mut out = Vec::new();
        for (off, d) in chunks {
            if off != out.len() as u64 {
                if off > out.len() as u64 {
                    break; // data beyond a gap (past EOF) is ignored
                }
                continue;
            }
            out.extend_from_slice(&d);
        }
        Ok(out)
    }

    /// Writes a whole file (create/truncate, or append) with pipelined WRITE requests.
    pub fn write_file(&mut self, path: &str, data: &[u8], append: bool) -> SshResult<()> {
        let flags = open_flags::WRITE | open_flags::CREAT | if append { open_flags::APPEND } else { open_flags::TRUNC };
        let handle = self.open(path, flags, &FileAttrs::default())?;
        let start = if append { self.fstat(&handle).ok().and_then(|a| a.size).unwrap_or(0) } else { 0 };
        let r = self.write_all(&handle, start, data);
        let c = self.close_handle(&handle);
        r?;
        c
    }

    fn write_all(&mut self, handle: &[u8], start: u64, data: &[u8]) -> SshResult<()> {
        let chunk = self.chunk_size.max(1);
        let mut outstanding = 0usize;
        let mut ids = std::collections::HashSet::new();
        let mut off = 0usize;
        let mut error = None;
        while off < data.len() || outstanding > 0 {
            while error.is_none() && off < data.len() && outstanding < self.max_outstanding.max(1) {
                let end = (off + chunk).min(data.len());
                let id = self.send(fxp::WRITE, |w| {
                    w.string(handle).u64(start + off as u64).string(&data[off..end]);
                })?;
                ids.insert(id);
                outstanding += 1;
                off = end;
            }
            if outstanding == 0 {
                break;
            }
            let (id, t, rest) = self.recv_response()?;
            if !ids.remove(&id) {
                continue;
            }
            outstanding -= 1;
            if t != fxp::STATUS {
                error.get_or_insert(SshError::protocol(format!("unexpected SFTP response {} for write", t)));
                continue;
            }
            let (code, m) = Self::parse_status(&rest);
            if code != status::OK {
                error.get_or_insert(status_err(code, format!("write: {}", m)));
            }
        }
        match error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
