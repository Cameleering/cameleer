//! # rust-camel SSH
//!
//! A std-only SSH-2 implementation (client and test server) with SFTP v3 and the
//! Camel `sftp` and `ssh` components. All cryptography comes from `camel-tls`
//! plus Ed25519, AES-CTR and `chacha20-poly1305@openssh.com` implemented here.
//!
//! | layer | module |
//! |---|---|
//! | transport (RFC 4253): version exchange, packets, KEX, rekeying, strict KEX | [`transport`] |
//! | authentication (RFC 4252/4256) and connection protocol (RFC 4254) client | [`client`] |
//! | keys, signatures, OpenSSH/PEM private keys | [`keys`] |
//! | `known_hosts` | [`known_hosts`] |
//! | SFTP v3 client and server | [`sftp`] |
//! | `sftp` and `ssh` Camel components | [`component`] |
//! | embedded SSH/SFTP server for tests | [`testing`] |
//!
//! # Algorithms
//!
//! * key exchange: `curve25519-sha256` (+ `@libssh.org` alias), `ecdh-sha2-nistp256`
//! * host keys: `ssh-ed25519`, `ecdsa-sha2-nistp256`, `ecdsa-sha2-nistp384`, `rsa-sha2-512`, `rsa-sha2-256`
//! * ciphers: `chacha20-poly1305@openssh.com`, `aes128-gcm@openssh.com`, `aes256-gcm@openssh.com`,
//!   `aes128-ctr`, `aes192-ctr`, `aes256-ctr`
//! * MACs (CTR ciphers): `hmac-sha2-256(-etm@openssh.com)`, `hmac-sha2-512(-etm@openssh.com)`
//! * compression: `none` only
//! * user authentication: `publickey` (Ed25519, ECDSA P-256/P-384, RSA with SHA-2; local keys,
//!   also passphrase-protected, or an SSH agent via [`agent`]), `password`,
//!   `keyboard-interactive`
//!
//! ```no_run
//! use camel_ssh::client::{ClientConfig, HostKeyCheck, SshClient};
//! use camel_ssh::known_hosts::KnownHosts;
//! use camel_ssh::sftp::SftpClient;
//!
//! let known = KnownHosts::load(std::path::Path::new("/home/me/.ssh/known_hosts")).unwrap();
//! let cfg = ClientConfig::new("example.com", 22, "me")
//!     .password("secret")
//!     .host_key_check(HostKeyCheck::KnownHosts { known, strict: true });
//! let mut ssh = SshClient::connect(&cfg).unwrap();
//! let out = ssh.execute("uname -a", None).unwrap();
//! println!("{} (exit {:?})", String::from_utf8_lossy(&out.stdout), out.exit_status);
//! let mut sftp = SftpClient::new(ssh).unwrap();
//! sftp.write_file("hello.txt", b"Hello", false).unwrap();
//! assert_eq!(sftp.read_file("hello.txt").unwrap(), b"Hello");
//! ```
//!
//! # Security notes
//!
//! This implementation has not been audited. Ed25519 signing, X25519, P-256
//! and ChaCha20-Poly1305 avoid secret-dependent branches; AES (CTR and GCM) uses
//! table lookups (cache-timing exposure, prefer ChaCha20-Poly1305 on shared
//! hosts) and RSA signing uses neither CRT nor blinding (see `camel-tls`).
//! Secrets are not zeroised on drop. Passphrase-protected private keys,
//! certificates (`*-cert-v01@openssh.com`), agent forwarding, port forwarding,
//! compression and GSSAPI are not supported.

pub mod agent;
pub mod client;
pub mod component;
pub mod crypto;
pub mod error;
pub mod keys;
pub mod known_hosts;
pub mod sftp;
pub mod testing;
pub mod transport;
pub mod wire;
pub mod zlib;

use std::sync::Arc;

use camel_core::context::CamelContext;

pub use client::{ClientConfig, ExecResult, HostKeyCheck, SshClient};
pub use error::{SshError, SshResult};
pub use keys::{SshPrivateKey, SshPublicKey};
pub use known_hosts::KnownHosts;
pub use sftp::SftpClient;

/// Registers the `sftp` and `ssh` components (lazily created). Schemes that
/// already have a component are left untouched, so call this **before**
/// `camel_protocols::register` (which installs an erroring `sftp` placeholder),
/// or use [`register_override`].
pub fn register(ctx: &CamelContext) {
    if !ctx.has_component("sftp") {
        ctx.add_component_factory("sftp", || Arc::new(component::sftp::SftpComponent));
    }
    if !ctx.has_component("ssh") {
        ctx.add_component_factory("ssh", || Arc::new(component::ssh::SshComponent));
    }
}

/// Registers the `sftp` and `ssh` components, replacing existing registrations.
pub fn register_override(ctx: &CamelContext) {
    ctx.add_component("sftp", component::sftp::SftpComponent);
    ctx.add_component("ssh", component::ssh::SshComponent);
}
