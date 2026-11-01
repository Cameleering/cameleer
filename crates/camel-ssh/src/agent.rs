//! SSH agent protocol (draft-miller-ssh-agent): a client that lists identities and asks
//! the agent (`SSH_AUTH_SOCK`, a Unix socket) to sign, used for `publickey`
//! authentication, plus a minimal agent server for tests.

use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::{SshError, SshResult};
use crate::keys::{SshPrivateKey, SshPublicKey};
use crate::wire::{Reader, Writer};

pub const FAILURE: u8 = 5;
pub const SUCCESS: u8 = 6;
pub const REQUEST_IDENTITIES: u8 = 11;
pub const IDENTITIES_ANSWER: u8 = 12;
pub const SIGN_REQUEST: u8 = 13;
pub const SIGN_RESPONSE: u8 = 14;
/// Sign flags for RSA keys.
pub const RSA_SHA2_256: u32 = 2;
pub const RSA_SHA2_512: u32 = 4;

fn agent_err(msg: impl Into<String>) -> SshError {
    SshError::Auth(format!("ssh-agent: {}", msg.into()))
}

/// An identity held by the agent.
#[derive(Clone, Debug)]
pub struct AgentIdentity {
    pub blob: Vec<u8>,
    pub comment: String,
}

impl AgentIdentity {
    pub fn public_key(&self) -> SshResult<SshPublicKey> {
        SshPublicKey::from_blob(&self.blob)
    }
}

/// A connection to an SSH agent.
pub struct AgentClient {
    #[cfg(unix)]
    stream: UnixStream,
}

fn read_msg<R: Read>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_be_bytes(len) as usize;
    if n == 0 || n > 256 * 1024 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid agent message length"));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

fn write_msg<W: Write>(w: &mut W, body: &[u8]) -> std::io::Result<()> {
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(body);
    w.write_all(&out)?;
    w.flush()
}

impl AgentClient {
    /// Connects to the agent socket at `path`.
    #[cfg(unix)]
    pub fn connect(path: &Path) -> SshResult<AgentClient> {
        let stream = UnixStream::connect(path).map_err(|e| agent_err(format!("cannot connect to {}: {}", path.display(), e)))?;
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
        Ok(AgentClient { stream })
    }

    #[cfg(not(unix))]
    pub fn connect(path: &Path) -> SshResult<AgentClient> {
        Err(agent_err(format!("agent sockets are not supported on this platform ({})", path.display())))
    }

    /// Connects to `$SSH_AUTH_SOCK`.
    pub fn from_env() -> SshResult<AgentClient> {
        let p = std::env::var_os("SSH_AUTH_SOCK").ok_or_else(|| agent_err("SSH_AUTH_SOCK is not set"))?;
        AgentClient::connect(Path::new(&p))
    }

    #[cfg(unix)]
    fn call(&mut self, body: &[u8]) -> SshResult<Vec<u8>> {
        write_msg(&mut self.stream, body).map_err(|e| agent_err(e.to_string()))?;
        read_msg(&mut self.stream).map_err(|e| agent_err(e.to_string()))
    }

    #[cfg(not(unix))]
    fn call(&mut self, _body: &[u8]) -> SshResult<Vec<u8>> {
        Err(agent_err("not supported"))
    }

    /// The agent's identities.
    pub fn identities(&mut self) -> SshResult<Vec<AgentIdentity>> {
        let resp = self.call(&[REQUEST_IDENTITIES])?;
        if resp[0] != IDENTITIES_ANSWER {
            return Err(agent_err(format!("unexpected answer {} to REQUEST_IDENTITIES", resp[0])));
        }
        let mut r = Reader::new(&resp[1..]);
        let n = r.u32()?;
        let mut out = Vec::new();
        for _ in 0..n.min(1024) {
            let blob = r.string()?.to_vec();
            let comment = String::from_utf8_lossy(r.string()?).into_owned();
            out.push(AgentIdentity { blob, comment });
        }
        Ok(out)
    }

    /// Asks the agent to sign `data` with the key `blob`; returns the signature blob.
    pub fn sign(&mut self, blob: &[u8], data: &[u8], flags: u32) -> SshResult<Vec<u8>> {
        let req = Writer::msg(SIGN_REQUEST).string(blob).string(data).u32(flags).finish();
        let resp = self.call(&req)?;
        if resp[0] != SIGN_RESPONSE {
            return Err(agent_err("the agent refused to sign"));
        }
        Ok(Reader::new(&resp[1..]).string()?.to_vec())
    }
}

/// The signature algorithm the agent uses for `key` with `flags`.
pub fn algorithm_for(key: &SshPublicKey, flags: u32) -> &'static str {
    match key {
        SshPublicKey::Rsa { .. } if flags & RSA_SHA2_512 != 0 => "rsa-sha2-512",
        SshPublicKey::Rsa { .. } if flags & RSA_SHA2_256 != 0 => "rsa-sha2-256",
        SshPublicKey::Rsa { .. } => "ssh-rsa",
        other => other.signature_algorithms()[0],
    }
}

/// A minimal SSH agent serving keys on a Unix socket (identities and signing only).
pub struct TestAgent {
    path: PathBuf,
    stop: Arc<Mutex<bool>>,
}

impl TestAgent {
    /// Starts an agent holding `keys` on a socket in a fresh temporary directory.
    #[cfg(unix)]
    pub fn start(keys: Vec<SshPrivateKey>) -> SshResult<TestAgent> {
        let dir = crate::testing::temp_dir("agent");
        let path = dir.join("agent.sock");
        let l = UnixListener::bind(&path).map_err(|e| agent_err(e.to_string()))?;
        let stop = Arc::new(Mutex::new(false));
        let st = stop.clone();
        let keys = Arc::new(keys);
        std::thread::spawn(move || {
            for c in l.incoming() {
                if *st.lock().unwrap() {
                    break;
                }
                let Ok(mut s) = c else { continue };
                let keys = keys.clone();
                std::thread::spawn(move || {
                    while let Ok(m) = read_msg(&mut s) {
                        let resp = match m[0] {
                            REQUEST_IDENTITIES => {
                                let mut w = Writer::msg(IDENTITIES_ANSWER);
                                w.u32(keys.len() as u32);
                                for k in keys.iter() {
                                    w.string(&k.public_key().to_blob()).str("test-agent");
                                }
                                w.finish()
                            }
                            SIGN_REQUEST => {
                                let mut r = Reader::new(&m[1..]);
                                let parsed = (|| -> SshResult<Vec<u8>> {
                                    let blob = r.string()?.to_vec();
                                    let data = r.string()?.to_vec();
                                    let flags = r.u32()?;
                                    let k = keys.iter().find(|k| k.public_key().to_blob() == blob).ok_or_else(|| agent_err("unknown key"))?;
                                    let alg = algorithm_for(&k.public_key(), flags);
                                    k.sign(alg, &data)
                                })();
                                match parsed {
                                    Ok(sig) => Writer::msg(SIGN_RESPONSE).string(&sig).finish(),
                                    Err(_) => vec![FAILURE],
                                }
                            }
                            _ => vec![FAILURE],
                        };
                        if write_msg(&mut s, &resp).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        Ok(TestAgent { path, stop })
    }

    /// The socket path (for `SSH_AUTH_SOCK` / `agentSocket`).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestAgent {
    fn drop(&mut self) {
        *self.stop.lock().unwrap() = true;
        #[cfg(unix)]
        let _ = UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}
