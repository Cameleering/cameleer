//! OpenSSH `known_hosts` files: plain and hashed (`|1|salt|hash`) host
//! patterns, `*`/`?` wildcards, `!` negation, `[host]:port` entries and the
//! `@revoked` marker and `@cert-authority` lines (host certificates signed by a listed CA
//! are trusted for the matching hosts).

use std::path::Path;

use cameleer_tls::crypto::hash::HashAlg;
use cameleer_tls::pem::{base64_decode, base64_encode};

use crate::keys::SshPublicKey;

/// Result of looking up a host key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// A matching entry with this key exists.
    Trusted,
    /// The host has no entry for this key type.
    Unknown,
    /// The host is listed with a different key of the same type (possible MITM).
    Changed,
    /// The key is marked `@revoked`.
    Revoked,
}

#[derive(Clone, Debug)]
enum Hosts {
    Hashed { salt: Vec<u8>, hash: Vec<u8> },
    Patterns(Vec<String>),
}

#[derive(Clone, Debug)]
struct Entry {
    revoked: bool,
    hosts: Hosts,
    key: SshPublicKey,
}

/// A parsed `known_hosts` file.
#[derive(Clone, Debug, Default)]
pub struct KnownHosts {
    entries: Vec<Entry>,
    authorities: Vec<Entry>,
}

/// The name under which a host is recorded: `host` for port 22, `[host]:port` otherwise.
pub fn host_name(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{}]:{}", host, port)
    }
}

fn wildcard(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    fn rec(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => (0..=t.len()).any(|i| rec(&p[1..], &t[i..])),
            Some('?') => !t.is_empty() && rec(&p[1..], &t[1..]),
            Some(c) => !t.is_empty() && t[0].eq_ignore_ascii_case(c) && rec(&p[1..], &t[1..]),
        }
    }
    rec(&p, &t)
}

impl Hosts {
    fn matches(&self, name: &str) -> bool {
        match self {
            Hosts::Hashed { salt, hash } => cameleer_tls::util::ct_eq(&HashAlg::Sha1.hmac(salt, name.as_bytes()), hash),
            Hosts::Patterns(pats) => {
                let mut matched = false;
                for p in pats {
                    if let Some(neg) = p.strip_prefix('!') {
                        if wildcard(neg, name) {
                            return false;
                        }
                    } else if wildcard(p, name) {
                        matched = true;
                    }
                }
                matched
            }
        }
    }
}

impl KnownHosts {
    /// An empty list.
    pub fn new() -> KnownHosts {
        KnownHosts::default()
    }

    /// Parses the text of a `known_hosts` file; unparsable lines are skipped.
    pub fn parse(text: &str) -> KnownHosts {
        let mut entries = Vec::new();
        let mut authorities = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut words: Vec<&str> = line.split_whitespace().collect();
            let mut revoked = false;
            let mut ca = false;
            if let Some(m) = words.first().filter(|w| w.starts_with('@')) {
                match *m {
                    "@revoked" => revoked = true,
                    "@cert-authority" => ca = true,
                    _ => continue,
                }
                words.remove(0);
            }
            if words.len() < 3 {
                continue;
            }
            let hosts = if let Some(rest) = words[0].strip_prefix("|1|") {
                let mut it = rest.split('|');
                match (it.next().and_then(base64_decode), it.next().and_then(base64_decode)) {
                    (Some(salt), Some(hash)) => Hosts::Hashed { salt, hash },
                    _ => continue,
                }
            } else {
                Hosts::Patterns(words[0].split(',').map(|s| s.to_string()).collect())
            };
            if let Ok(key) = SshPublicKey::from_openssh(&format!("{} {}", words[1], words[2])) {
                if ca {
                    authorities.push(Entry { revoked, hosts, key });
                } else {
                    entries.push(Entry { revoked, hosts, key });
                }
            }
        }
        KnownHosts { entries, authorities }
    }

    /// Loads a file; a missing file gives an empty list.
    pub fn load(path: &Path) -> std::io::Result<KnownHosts> {
        match std::fs::read_to_string(path) {
            Ok(t) => Ok(KnownHosts::parse(&t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KnownHosts::new()),
            Err(e) => Err(e),
        }
    }

    /// Number of usable entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adds an entry in memory.
    pub fn add(&mut self, host: &str, port: u16, key: &SshPublicKey) {
        self.entries.push(Entry { revoked: false, hosts: Hosts::Patterns(vec![host_name(host, port)]), key: key.clone() });
    }

    /// Checks `key` presented by `host:port`.
    pub fn check(&self, host: &str, port: u16, key: &SshPublicKey) -> HostKeyStatus {
        let name = host_name(host, port);
        let matching: Vec<&Entry> = self.entries.iter().filter(|e| e.hosts.matches(&name)).collect();
        if self.entries.iter().any(|e| e.revoked && &e.key == key) {
            return HostKeyStatus::Revoked;
        }
        if matching.iter().any(|e| !e.revoked && &e.key == key) {
            return HostKeyStatus::Trusted;
        }
        if matching.iter().any(|e| !e.revoked && e.key.key_type() == key.key_type()) {
            return HostKeyStatus::Changed;
        }
        HostKeyStatus::Unknown
    }

    /// Whether some `@cert-authority` line covers `host:port`.
    pub fn has_cert_authority(&self, host: &str, port: u16) -> bool {
        let name = host_name(host, port);
        self.authorities.iter().any(|e| e.hosts.matches(&name))
    }

    /// Checks a host certificate: issued by a CA listed for the host, a host certificate,
    /// valid now, with the host name among its principals; revoked CAs / keys are refused.
    pub fn check_certificate(&self, host: &str, port: u16, cert: &crate::cert::SshCertificate) -> HostKeyStatus {
        let name = host_name(host, port);
        if self.entries.iter().any(|e| e.revoked && (e.key == cert.key || e.key == cert.signature_key)) {
            return HostKeyStatus::Revoked;
        }
        let trusted_ca = self.authorities.iter().any(|e| !e.revoked && e.key == cert.signature_key && e.hosts.matches(&name));
        if trusted_ca && cert.validate(crate::cert::HOST_CERT, host, crate::cert::now_secs()).is_ok() {
            return HostKeyStatus::Trusted;
        }
        HostKeyStatus::Unknown
    }

    /// A `known_hosts` line for the host, optionally hashed.
    pub fn entry_line(host: &str, port: u16, key: &SshPublicKey, hashed: bool) -> String {
        let name = host_name(host, port);
        let h = if hashed {
            let salt = cameleer_tls::crypto::rng::random_bytes(20);
            let mac = HashAlg::Sha1.hmac(&salt, name.as_bytes());
            format!("|1|{}|{}", base64_encode(&salt), base64_encode(&mac))
        } else {
            name
        };
        format!("{} {}", h, key.to_openssh(""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::SshPrivateKey;

    #[test]
    fn plain_hashed_wildcards_and_revoked() {
        let k1 = SshPrivateKey::generate_ed25519().public_key();
        let k2 = SshPrivateKey::generate_ed25519().public_key();
        let k3 = SshPrivateKey::generate_ed25519().public_key();
        let text = format!(
            "# comment\n{}\n{}\n*.example.com,!bad.example.com {}\n@revoked * {}\n@cert-authority *.x {}\ngarbage line\n",
            KnownHosts::entry_line("server", 22, &k1, false),
            KnownHosts::entry_line("127.0.0.1", 2222, &k1, true),
            k2.to_openssh(""),
            k3.to_openssh(""),
            k1.to_openssh("")
        );
        let kh = KnownHosts::parse(&text);
        assert_eq!(kh.len(), 4);
        assert_eq!(kh.check("server", 22, &k1), HostKeyStatus::Trusted);
        assert_eq!(kh.check("SERVER", 22, &k1), HostKeyStatus::Trusted);
        assert_eq!(kh.check("server", 22, &k2), HostKeyStatus::Changed);
        assert_eq!(kh.check("server", 2200, &k1), HostKeyStatus::Unknown);
        assert_eq!(kh.check("127.0.0.1", 2222, &k1), HostKeyStatus::Trusted);
        assert_eq!(kh.check("127.0.0.1", 2223, &k1), HostKeyStatus::Unknown);
        assert_eq!(kh.check("a.example.com", 22, &k2), HostKeyStatus::Trusted);
        assert_eq!(kh.check("bad.example.com", 22, &k2), HostKeyStatus::Unknown);
        assert_eq!(kh.check("anything", 22, &k3), HostKeyStatus::Revoked);
        let e = SshPrivateKey::generate_ecdsa(cameleer_tls::crypto::ec::EcCurve::P256).public_key();
        assert_eq!(kh.check("server", 22, &e), HostKeyStatus::Unknown);
    }
}
