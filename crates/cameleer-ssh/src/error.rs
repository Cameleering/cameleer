//! Error type of the SSH stack.

use std::fmt;

/// Errors raised by the SSH transport, authentication, connection and SFTP layers.
#[derive(Debug)]
pub enum SshError {
    /// Underlying socket error.
    Io(std::io::Error),
    /// Malformed or unexpected protocol data.
    Protocol(String),
    /// The peer sent `SSH_MSG_DISCONNECT` (or closed the connection).
    Disconnected { reason: u32, message: String },
    /// No common algorithm could be negotiated.
    Negotiation(String),
    /// The server host key was rejected (unknown, changed, bad signature).
    HostKey(String),
    /// All authentication methods failed.
    Auth(String),
    /// A channel could not be opened or a channel request failed.
    Channel(String),
    /// An SFTP request failed with a status code (`SSH_FX_*`).
    Sftp { code: u32, message: String },
    /// Invalid key material or configuration.
    Key(String),
    /// An operation timed out.
    Timeout(String),
}

impl SshError {
    pub fn protocol(m: impl Into<String>) -> SshError {
        SshError::Protocol(m.into())
    }
    /// True for errors after which the connection is no longer usable.
    pub fn is_fatal(&self) -> bool {
        !matches!(self, SshError::Sftp { .. } | SshError::Channel(_))
    }
}

impl fmt::Display for SshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SshError::Io(e) => write!(f, "I/O error: {}", e),
            SshError::Protocol(m) => write!(f, "SSH protocol error: {}", m),
            SshError::Disconnected { reason, message } => write!(f, "Disconnected by peer (reason {}): {}", reason, message),
            SshError::Negotiation(m) => write!(f, "SSH algorithm negotiation failed: {}", m),
            SshError::HostKey(m) => write!(f, "Host key verification failed: {}", m),
            SshError::Auth(m) => write!(f, "Authentication failed: {}", m),
            SshError::Channel(m) => write!(f, "SSH channel error: {}", m),
            SshError::Sftp { code, message } => write!(f, "SFTP error {} ({}): {}", code, crate::sftp::status_name(*code), message),
            SshError::Key(m) => write!(f, "Invalid key: {}", m),
            SshError::Timeout(m) => write!(f, "Timeout: {}", m),
        }
    }
}

impl std::error::Error for SshError {}

impl From<std::io::Error> for SshError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => SshError::Timeout(e.to_string()),
            _ => SshError::Io(e),
        }
    }
}

/// Result alias of the SSH stack.
pub type SshResult<T> = Result<T, SshError>;
