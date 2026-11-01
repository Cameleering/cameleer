//! Camel components: [`sftp`] (file transfer over SFTP, same consumer/producer
//! options and headers as the `ftp` component) and [`ssh`] (run commands).
//!
//! Connection options shared by both components (Camel names):
//!
//! | option | meaning |
//! |---|---|
//! | `username`, `password` | credentials (also from `user:password@` in the URI) |
//! | `privateKeyFile` / `privateKeyUri` / `privateKey` | private key: path, `file:` URI, or the key text / `#bean` (an `SshPrivateKey` or `String` bound in the registry); OpenSSH or PEM format, unencrypted |
//! | `useAgent` / `agentSocket` | authenticate with the identities of an SSH agent (`SSH_AUTH_SOCK` or the given socket) |
//! | `privateKeyPassphrase` | decrypts passphrase-protected keys (OpenSSH bcrypt-pbkdf, PKCS#8 PBES2, legacy PEM) |
//! | `knownHostsFile` / `knownHostsUri` / `knownHosts` | `known_hosts` path, URI or text |
//! | `useUserKnownHostsFile` | also load `~/.ssh/known_hosts` (default true, sftp only) |
//! | `strictHostKeyChecking` | `yes`: the host key must be known; `no` (Camel default): unknown hosts are accepted. Changed or revoked keys are always rejected |
//! | `preferredAuthentications` | e.g. `publickey,keyboard-interactive,password` |
//! | `ciphers`, `keyExchangeProtocols`, `serverHostKeys`, `macs` | algorithm lists (comma separated) |
//! | `connectTimeout` | TCP connect timeout (10s) |

pub mod sftp;
pub mod ssh;

use std::path::{Path, PathBuf};
use std::time::Duration;

use camel_core::context::CamelContext;
use camel_core::error::{kinds, CamelError, Result};
use camel_core::util::Parameters;

use crate::client::{ClientConfig, HostKeyCheck};
use crate::error::SshError;
use crate::keys::SshPrivateKey;
use crate::known_hosts::KnownHosts;
use crate::transport::{parse_algorithm_list, SUPPORTED_CIPHERS, SUPPORTED_HOST_KEY, SUPPORTED_KEX, SUPPORTED_MACS};

/// Maps an SSH error to a Camel exception: SFTP status errors become
/// `GenericFileOperationFailedException`, timeouts `SocketTimeoutException`,
/// refused connections `ConnectException`, everything else `IOException`.
pub fn camel_error(e: SshError, what: &str) -> CamelError {
    let msg = if what.is_empty() { e.to_string() } else { format!("{}: {}", what, e) };
    match &e {
        SshError::Sftp { .. } | SshError::Channel(_) => CamelError::new(camel_core::component::file::GENERIC_FILE_OPERATION_FAILED, msg),
        SshError::Timeout(_) => CamelError::new(kinds::SOCKET_TIMEOUT, msg),
        SshError::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => CamelError::new(kinds::CONNECT, msg),
        SshError::Key(_) | SshError::Negotiation(_) => CamelError::illegal_argument(msg),
        _ => CamelError::io(msg),
    }
}

fn take_nonempty(p: &mut Parameters, k: &str) -> Option<String> {
    p.take(k).filter(|s| !s.is_empty())
}

fn file_path(uri: &str) -> PathBuf {
    let s = uri.strip_prefix("file://").or_else(|| uri.strip_prefix("file:")).unwrap_or(uri);
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(s)
}

fn read_text(path: &Path, what: &str) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| CamelError::illegal_argument(format!("Cannot read {} {}: {}", what, path.display(), e)))
}

fn key_from_text(text: &str, passphrase: Option<&str>) -> Result<SshPrivateKey> {
    SshPrivateKey::from_text_with_passphrase(text, passphrase).map_err(|e| CamelError::illegal_argument(format!("Cannot load private key: {}", e)))
}

/// Options that select how the host key is checked.
pub(crate) struct HostKeyOptions {
    pub known_hosts_file: Option<String>,
    pub known_hosts_text: Option<String>,
    pub use_user_known_hosts: bool,
    pub strict: bool,
}

/// Parses the connection options into a [`ClientConfig`].
pub(crate) fn client_config(
    ctx: &CamelContext,
    host: &str,
    port: u16,
    username: String,
    password: Option<String>,
    params: &mut Parameters,
    hk: HostKeyOptions,
) -> Result<ClientConfig> {
    let mut cfg = ClientConfig::new(host, port, &username);
    cfg.password = password;
    let passphrase_value = take_nonempty(params, "privateKeyPassphrase");
    let passphrase = passphrase_value.as_deref();
    if let Some(f) = take_nonempty(params, "privateKeyFile") {
        cfg.private_keys.push(key_from_text(&read_text(&file_path(&f), "private key file")?, passphrase)?);
    }
    if let Some(u) = take_nonempty(params, "privateKeyUri") {
        cfg.private_keys.push(key_from_text(&read_text(&file_path(&u), "private key")?, passphrase)?);
    }
    if let Some(k) = take_nonempty(params, "privateKey") {
        if let Some(name) = k.strip_prefix('#') {
            let name = name.trim_start_matches("bean:");
            if let Some(key) = ctx.registry().lookup::<SshPrivateKey>(name) {
                cfg.private_keys.push((*key).clone());
            } else if let Some(text) = ctx.registry().lookup::<String>(name) {
                cfg.private_keys.push(key_from_text(&text, passphrase)?);
            } else {
                return Err(CamelError::no_such_bean(name));
            }
        } else {
            cfg.private_keys.push(key_from_text(&k, passphrase)?);
        }
    }
    let use_agent = params.take_bool("useAgent", false)?;
    if let Some(sock) = take_nonempty(params, "agentSocket") {
        cfg.agent = Some(file_path(&sock));
    } else if use_agent {
        cfg.agent = Some(std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from).ok_or_else(|| CamelError::illegal_argument("useAgent=true but SSH_AUTH_SOCK is not set"))?);
    }
    let mut known = String::new();
    let mut have_known = false;
    if let Some(f) = &hk.known_hosts_file {
        let p = file_path(f);
        known.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
        known.push('\n');
        have_known = true;
    }
    if let Some(t) = &hk.known_hosts_text {
        known.push_str(t);
        known.push('\n');
        have_known = true;
    }
    if hk.use_user_known_hosts {
        if let Some(home) = std::env::var_os("HOME") {
            if let Ok(t) = std::fs::read_to_string(Path::new(&home).join(".ssh/known_hosts")) {
                known.push_str(&t);
                have_known = true;
            }
        }
    }
    cfg.host_key_check = if hk.strict || have_known {
        HostKeyCheck::KnownHosts { known: KnownHosts::parse(&known), strict: hk.strict }
    } else {
        HostKeyCheck::AcceptAny
    };
    if let Some(p) = take_nonempty(params, "preferredAuthentications") {
        cfg.preferred_auth = p.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    let alg = |e: SshError| CamelError::illegal_argument(e.to_string());
    if let Some(v) = take_nonempty(params, "ciphers") {
        cfg.transport.algorithms.ciphers = parse_algorithm_list("cipher", &v, SUPPORTED_CIPHERS).map_err(alg)?;
    }
    for k in ["keyExchangeProtocols", "kex"] {
        if let Some(v) = take_nonempty(params, k) {
            cfg.transport.algorithms.kex = parse_algorithm_list("key exchange", &v, SUPPORTED_KEX).map_err(alg)?;
        }
    }
    for k in ["serverHostKeys", "signatures"] {
        if let Some(v) = take_nonempty(params, k) {
            cfg.transport.algorithms.host_key = parse_algorithm_list("host key", &v, SUPPORTED_HOST_KEY).map_err(alg)?;
        }
    }
    if let Some(v) = take_nonempty(params, "macs") {
        cfg.transport.algorithms.macs = parse_algorithm_list("MAC", &v, SUPPORTED_MACS).map_err(alg)?;
    }
    // OpenSSH user certificate for the private key (`ssh-keygen -s` output, `*-cert.pub`)
    for k in ["certificateFile", "certFile"] {
        if let Some(path) = take_nonempty(params, k) {
            let text = std::fs::read_to_string(file_path(&path)).map_err(|e| CamelError::illegal_argument(format!("Cannot read {} {}: {}", k, path, e)))?;
            cfg.certificates.push(crate::cert::SshCertificate::from_openssh(text.trim()).map_err(|e| CamelError::illegal_argument(format!("Invalid certificate {}: {}", path, e)))?);
        }
    }
    if let Some(v) = take_nonempty(params, "compressionAlgorithms") {
        cfg.transport.compression = parse_algorithm_list("compression", &v, crate::transport::SUPPORTED_COMPRESSION).map_err(alg)?;
    }
    cfg.connect_timeout = params.take_duration("connectTimeout", Duration::from_secs(10))?;
    Ok(cfg)
}

/// `[user[:password]@]host[:port][/path]` of an endpoint URI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Authority {
    pub user: Option<String>,
    pub password: Option<String>,
    pub host: String,
    pub port: u16,
    /// Path without the leading `/` (so `//abs` gives `/abs`).
    pub path: String,
}

impl Authority {
    pub fn parse(remaining: &str, default_port: u16) -> Result<Authority> {
        use camel_core::util::url_decode;
        let remaining = remaining.trim_start_matches("//");
        let (auth, path) = match remaining.find('/') {
            Some(i) => (&remaining[..i], &remaining[i + 1..]),
            None => (remaining, ""),
        };
        let (userinfo, hostport) = match auth.rfind('@') {
            Some(i) => (Some(&auth[..i]), &auth[i + 1..]),
            None => (None, auth),
        };
        let (user, password) = match userinfo {
            None => (None, None),
            Some(u) => match u.find(':') {
                Some(i) => (Some(url_decode(&u[..i])), Some(url_decode(&u[i + 1..]))),
                None => (Some(url_decode(u)), None),
            },
        };
        let bad = || CamelError::illegal_argument(format!("Invalid host:port: {}", hostport));
        let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
            let end = rest.find(']').ok_or_else(bad)?;
            let port = match rest[end + 1..].strip_prefix(':') {
                Some(p) => p.parse().map_err(|_| bad())?,
                None => default_port,
            };
            (rest[..end].to_string(), port)
        } else {
            match hostport.rfind(':') {
                Some(i) => (hostport[..i].to_string(), hostport[i + 1..].parse().map_err(|_| bad())?),
                None => (hostport.to_string(), default_port),
            }
        };
        if host.is_empty() {
            return Err(CamelError::illegal_argument(format!("Hostname must be configured: {}", remaining)));
        }
        Ok(Authority { user: user.filter(|u| !u.is_empty()), password, host, port, path: path.to_string() })
    }
}

/// Parses `yes`/`no`/`true`/`false` (Camel uses strings for `strictHostKeyChecking`).
pub(crate) fn yes_no(v: &str) -> Result<bool> {
    match v.to_ascii_lowercase().as_str() {
        "yes" | "true" => Ok(true),
        "no" | "false" | "" => Ok(false),
        "ask" => Err(CamelError::illegal_argument("strictHostKeyChecking=ask is not supported (no interactive prompt)")),
        other => Err(CamelError::illegal_argument(format!("Invalid yes/no value: {}", other))),
    }
}
