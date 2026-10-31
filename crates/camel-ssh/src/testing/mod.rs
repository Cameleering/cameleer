//! Test support: an embedded SSH/SFTP server ([`SshServer`]) and helpers.

pub mod server;

pub use server::{run_builtin, CommandHandler, CommandOutput, SshServer, SshServerConfig};

use std::path::PathBuf;

/// Creates a fresh, empty temporary directory.
pub fn temp_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "camel-ssh-{}-{}-{}-{}",
        name,
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed),
        camel_core::util_time::now_millis()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create temp dir");
    d
}
