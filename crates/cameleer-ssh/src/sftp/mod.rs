//! SFTP version 3 (draft-ietf-secsh-filexfer-02): protocol constants, file
//! attributes, the [`client::SftpClient`] and the server side used by the
//! embedded test server ([`server::SftpServer`]).

pub mod client;
pub mod server;

use crate::error::SshResult;
use crate::wire::{Reader, Writer};

pub use client::{DirEntry, SftpClient};

/// Packet types.
pub mod fxp {
    pub const INIT: u8 = 1;
    pub const VERSION: u8 = 2;
    pub const OPEN: u8 = 3;
    pub const CLOSE: u8 = 4;
    pub const READ: u8 = 5;
    pub const WRITE: u8 = 6;
    pub const LSTAT: u8 = 7;
    pub const FSTAT: u8 = 8;
    pub const SETSTAT: u8 = 9;
    pub const FSETSTAT: u8 = 10;
    pub const OPENDIR: u8 = 11;
    pub const READDIR: u8 = 12;
    pub const REMOVE: u8 = 13;
    pub const MKDIR: u8 = 14;
    pub const RMDIR: u8 = 15;
    pub const REALPATH: u8 = 16;
    pub const STAT: u8 = 17;
    pub const RENAME: u8 = 18;
    pub const READLINK: u8 = 19;
    pub const SYMLINK: u8 = 20;
    pub const STATUS: u8 = 101;
    pub const HANDLE: u8 = 102;
    pub const DATA: u8 = 103;
    pub const NAME: u8 = 104;
    pub const ATTRS: u8 = 105;
    pub const EXTENDED: u8 = 200;
    pub const EXTENDED_REPLY: u8 = 201;
}

/// Status codes (`SSH_FX_*`).
pub mod status {
    pub const OK: u32 = 0;
    pub const EOF: u32 = 1;
    pub const NO_SUCH_FILE: u32 = 2;
    pub const PERMISSION_DENIED: u32 = 3;
    pub const FAILURE: u32 = 4;
    pub const BAD_MESSAGE: u32 = 5;
    pub const NO_CONNECTION: u32 = 6;
    pub const CONNECTION_LOST: u32 = 7;
    pub const OP_UNSUPPORTED: u32 = 8;
}

/// `SSH_FXF_*` open flags.
pub mod open_flags {
    pub const READ: u32 = 0x01;
    pub const WRITE: u32 = 0x02;
    pub const APPEND: u32 = 0x04;
    pub const CREAT: u32 = 0x08;
    pub const TRUNC: u32 = 0x10;
    pub const EXCL: u32 = 0x20;
}

/// Symbolic name of a status code.
pub fn status_name(code: u32) -> &'static str {
    match code {
        0 => "SSH_FX_OK",
        1 => "SSH_FX_EOF",
        2 => "SSH_FX_NO_SUCH_FILE",
        3 => "SSH_FX_PERMISSION_DENIED",
        4 => "SSH_FX_FAILURE",
        5 => "SSH_FX_BAD_MESSAGE",
        6 => "SSH_FX_NO_CONNECTION",
        7 => "SSH_FX_CONNECTION_LOST",
        8 => "SSH_FX_OP_UNSUPPORTED",
        _ => "unknown",
    }
}

const ATTR_SIZE: u32 = 0x1;
const ATTR_UIDGID: u32 = 0x2;
const ATTR_PERMISSIONS: u32 = 0x4;
const ATTR_ACMODTIME: u32 = 0x8;
const ATTR_EXTENDED: u32 = 0x8000_0000;

/// File attributes (`ATTRS`); absent fields are `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileAttrs {
    pub size: Option<u64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// Full `st_mode` (file type bits + permissions).
    pub permissions: Option<u32>,
    pub atime: Option<u32>,
    pub mtime: Option<u32>,
}

impl FileAttrs {
    /// Whether the mode marks a directory.
    pub fn is_dir(&self) -> bool {
        self.permissions.map(|p| p & 0o170000 == 0o040000).unwrap_or(false)
    }
    /// Whether the mode marks a regular file (true when unknown).
    pub fn is_file(&self) -> bool {
        self.permissions.map(|p| p & 0o170000 == 0o100000).unwrap_or(true)
    }
    /// Whether the mode marks a symbolic link.
    pub fn is_symlink(&self) -> bool {
        self.permissions.map(|p| p & 0o170000 == 0o120000).unwrap_or(false)
    }

    pub fn encode(&self, w: &mut Writer) {
        let mut flags = 0;
        if self.size.is_some() {
            flags |= ATTR_SIZE;
        }
        if self.uid.is_some() && self.gid.is_some() {
            flags |= ATTR_UIDGID;
        }
        if self.permissions.is_some() {
            flags |= ATTR_PERMISSIONS;
        }
        if self.atime.is_some() && self.mtime.is_some() {
            flags |= ATTR_ACMODTIME;
        }
        w.u32(flags);
        if let Some(s) = self.size {
            w.u64(s);
        }
        if let (Some(u), Some(g)) = (self.uid, self.gid) {
            w.u32(u).u32(g);
        }
        if let Some(p) = self.permissions {
            w.u32(p);
        }
        if let (Some(a), Some(m)) = (self.atime, self.mtime) {
            w.u32(a).u32(m);
        }
    }

    pub fn decode(r: &mut Reader) -> SshResult<FileAttrs> {
        let flags = r.u32()?;
        let mut a = FileAttrs::default();
        if flags & ATTR_SIZE != 0 {
            a.size = Some(r.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            a.uid = Some(r.u32()?);
            a.gid = Some(r.u32()?);
        }
        if flags & ATTR_PERMISSIONS != 0 {
            a.permissions = Some(r.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            a.atime = Some(r.u32()?);
            a.mtime = Some(r.u32()?);
        }
        if flags & ATTR_EXTENDED != 0 {
            let n = r.u32()?;
            for _ in 0..n {
                r.string()?;
                r.string()?;
            }
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attrs_roundtrip() {
        let a = FileAttrs { size: Some(42), uid: Some(1), gid: Some(2), permissions: Some(0o100644), atime: Some(5), mtime: Some(6) };
        let mut w = Writer::new();
        a.encode(&mut w);
        let b = FileAttrs::decode(&mut Reader::new(&w.buf)).unwrap();
        assert_eq!(a, b);
        assert!(b.is_file() && !b.is_dir());
        let mut w = Writer::new();
        FileAttrs::default().encode(&mut w);
        assert_eq!(w.buf, vec![0, 0, 0, 0]);
    }
}
