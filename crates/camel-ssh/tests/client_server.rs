//! End-to-end tests of the SSH client against the embedded server.

use std::fs;
use std::time::Duration;

use camel_ssh::client::{ClientConfig, HostKeyCheck, SshClient};
use camel_ssh::known_hosts::KnownHosts;
use camel_ssh::sftp::{FileAttrs, SftpClient};
use camel_ssh::testing::{temp_dir, SshServer, SshServerConfig};
use camel_ssh::transport::{SUPPORTED_CIPHERS, SUPPORTED_KEX, SUPPORTED_MACS};
use camel_ssh::{SshError, SshPrivateKey};
use camel_tls::crypto::ec::EcCurve;

const RSA_PEM: &str = include_str!("fixtures/rsa2048.pem");
const RSA_PKCS8: &str = include_str!("fixtures/rsa2048_pk8.pem");
const EC_PEM: &str = include_str!("fixtures/ec.pem");

fn rsa() -> SshPrivateKey {
    SshPrivateKey::from_text(RSA_PEM).unwrap()
}

fn server_with(user_keys: &[&SshPrivateKey], host_keys: Vec<SshPrivateKey>) -> SshServer {
    let mut cfg = SshServerConfig::default().user("alice", "wonderland");
    for k in user_keys {
        cfg = cfg.authorized_key("alice", k.public_key());
    }
    for k in host_keys {
        cfg = cfg.host_key(k);
    }
    SshServer::start(cfg).unwrap()
}

fn base(server: &SshServer) -> ClientConfig {
    let known = KnownHosts::parse(&server.known_hosts_text("127.0.0.1"));
    let mut c = ClientConfig::new("127.0.0.1", server.port(), "alice").host_key_check(HostKeyCheck::KnownHosts { known, strict: true });
    c.timeout = Some(Duration::from_secs(20));
    c
}

#[test]
fn every_cipher_and_kex_with_ed25519_host_key() {
    let server = server_with(&[], vec![]);
    for cipher in SUPPORTED_CIPHERS {
        for kex in SUPPORTED_KEX {
            let mut cfg = base(&server).password("wonderland");
            cfg.transport.algorithms.ciphers = vec![cipher.to_string()];
            cfg.transport.algorithms.kex = vec![kex.to_string()];
            let mut c = SshClient::connect(&cfg).unwrap_or_else(|e| panic!("{} {}: {}", cipher, kex, e));
            assert_eq!(&c.transport().negotiated().cipher_c2s, cipher);
            assert_eq!(&c.transport().negotiated().kex, kex);
            assert!(c.transport().strict_kex());
            assert_eq!(c.execute("echo hi", None).unwrap().stdout, b"hi\n");
            c.disconnect();
        }
    }
    for mac in SUPPORTED_MACS {
        let mut cfg = base(&server).password("wonderland");
        cfg.transport.algorithms.ciphers = vec!["aes128-ctr".into()];
        cfg.transport.algorithms.macs = vec![mac.to_string()];
        let mut c = SshClient::connect(&cfg).unwrap();
        assert_eq!(&c.transport().negotiated().mac_s2c, mac);
        assert_eq!(c.execute("pwd", None).unwrap().stdout, b"/\n");
    }
}

#[test]
fn every_host_key_algorithm() {
    let host_keys = vec![SshPrivateKey::generate_ed25519(), SshPrivateKey::generate_ecdsa(EcCurve::P256), SshPrivateKey::generate_ecdsa(EcCurve::P384), rsa()];
    let server = server_with(&[], host_keys);
    for alg in ["ssh-ed25519", "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp384", "rsa-sha2-512", "rsa-sha2-256"] {
        let mut cfg = base(&server).password("wonderland");
        cfg.transport.algorithms.host_key = vec![alg.to_string()];
        let mut c = SshClient::connect(&cfg).unwrap_or_else(|e| panic!("{}: {}", alg, e));
        assert_eq!(c.transport().negotiated().host_key, alg);
        assert!(c.host_key().supports_algorithm(alg));
        assert_eq!(c.execute("echo ok", None).unwrap().exit_status, Some(0));
    }
}

#[test]
fn host_key_verification() {
    let server = server_with(&[], vec![]);
    let pw = |c: ClientConfig| c.password("wonderland");
    // trusted (hashed entry)
    let hashed = KnownHosts::entry_line("127.0.0.1", server.port(), &server.host_key(), true);
    let c = pw(ClientConfig::new("127.0.0.1", server.port(), "alice")).host_key_check(HostKeyCheck::KnownHosts { known: KnownHosts::parse(&hashed), strict: true });
    assert!(SshClient::connect(&c).is_ok());
    // unknown host: rejected when strict, accepted otherwise
    let c = pw(ClientConfig::new("127.0.0.1", server.port(), "alice")).host_key_check(HostKeyCheck::KnownHosts { known: KnownHosts::new(), strict: true });
    match SshClient::connect(&c) {
        Err(SshError::HostKey(m)) => assert!(m.contains("no ssh-ed25519 host key is known"), "{}", m),
        other => panic!("{:?}", other.err()),
    }
    let c = pw(ClientConfig::new("127.0.0.1", server.port(), "alice")).host_key_check(HostKeyCheck::KnownHosts { known: KnownHosts::new(), strict: false });
    assert!(SshClient::connect(&c).is_ok());
    // changed key: always rejected
    let other = SshPrivateKey::generate_ed25519().public_key();
    let line = KnownHosts::entry_line("127.0.0.1", server.port(), &other, false);
    let c = pw(ClientConfig::new("127.0.0.1", server.port(), "alice")).host_key_check(HostKeyCheck::KnownHosts { known: KnownHosts::parse(&line), strict: false });
    match SshClient::connect(&c) {
        Err(SshError::HostKey(m)) => assert!(m.contains("HAS CHANGED"), "{}", m),
        other => panic!("{:?}", other.err()),
    }
    // fingerprint pinning
    let fp = server.host_key().fingerprint();
    let c = pw(ClientConfig::new("127.0.0.1", server.port(), "alice")).host_key_check(HostKeyCheck::Fingerprints(vec![fp]));
    assert!(SshClient::connect(&c).is_ok());
    let c = pw(ClientConfig::new("127.0.0.1", server.port(), "alice")).host_key_check(HostKeyCheck::Fingerprints(vec![other.fingerprint()]));
    assert!(matches!(SshClient::connect(&c), Err(SshError::HostKey(_))));
}

#[test]
fn authentication_methods() {
    let ed = SshPrivateKey::generate_ed25519();
    let ec = SshPrivateKey::from_text(EC_PEM).unwrap();
    let rsa_pk8 = SshPrivateKey::from_text(RSA_PKCS8).unwrap();
    assert_eq!(rsa_pk8.public_key(), rsa().public_key());
    let server = server_with(&[&ed, &ec, &rsa_pk8], vec![]);
    // password good / bad
    let c = SshClient::connect(&base(&server).password("wonderland")).unwrap();
    assert_eq!(c.auth_method(), "keyboard-interactive");
    let mut cfg = base(&server).password("wonderland");
    cfg.preferred_auth = vec!["password".into()];
    assert_eq!(SshClient::connect(&cfg).unwrap().auth_method(), "password");
    match SshClient::connect(&base(&server).password("wrong")) {
        Err(SshError::Auth(m)) => assert!(m.contains("keyboard-interactive,password"), "{}", m),
        other => panic!("{:?}", other.err()),
    }
    let mut cfg = base(&server).password("wonderland");
    cfg.username = "mallory".into();
    assert!(matches!(SshClient::connect(&cfg), Err(SshError::Auth(_))));
    // public keys: Ed25519, ECDSA (SEC1 PEM), RSA (PKCS#8 PEM, rsa-sha2-512 via server-sig-algs)
    for k in [&ed, &ec, &rsa_pk8] {
        let c = SshClient::connect(&base(&server).private_key(k.clone())).unwrap();
        assert_eq!(c.auth_method(), "publickey");
    }
    // an unknown key fails, then falls back to the password
    let stranger = SshPrivateKey::generate_ed25519();
    assert!(matches!(SshClient::connect(&base(&server).private_key(stranger.clone())), Err(SshError::Auth(_))));
    let c = SshClient::connect(&base(&server).private_key(stranger).password("wonderland")).unwrap();
    assert_eq!(c.auth_method(), "keyboard-interactive");
    // the OpenSSH key file format round trip
    let back = SshPrivateKey::from_text(&ed.to_openssh_pem("alice").unwrap()).unwrap();
    assert_eq!(SshClient::connect(&base(&server).private_key(back)).unwrap().auth_method(), "publickey");
    let log = server.auth_log();
    assert!(log.contains(&"alice:password:fail".to_string()));
    assert!(log.contains(&"alice:publickey:ok".to_string()));
}

#[test]
fn exec_shell_and_large_streams() {
    let root = temp_dir("exec");
    fs::write(root.join("a.txt"), "file A\n").unwrap();
    let server = SshServer::start(SshServerConfig { banner: Some("Welcome\n".into()), ..SshServerConfig::default().user("alice", "wonderland").root(&root) }).unwrap();
    let mut cfg = base(&server).password("wonderland");
    cfg.transport.rekey_limit = 512 * 1024;
    let mut c = SshClient::connect(&cfg).unwrap();
    assert_eq!(c.banner(), Some("Welcome\n"));
    let r = c.execute("echo 'hello world'", None).unwrap();
    assert_eq!((r.stdout, r.exit_status), (b"hello world\n".to_vec(), Some(0)));
    let r = c.execute("cat a.txt missing.txt", None).unwrap();
    assert_eq!(r.stdout, b"file A\n");
    assert_eq!(r.stderr, b"cat: missing.txt: No such file or directory\n");
    assert_eq!(r.exit_status, Some(1));
    assert_eq!(c.execute("exit 42", None).unwrap().exit_status, Some(42));
    assert_eq!(c.execute("ls", None).unwrap().stdout, b"a.txt\n");
    assert_eq!(c.execute("/bin/rm -rf /tmp", None).unwrap().exit_status, Some(127));
    // 5 MB through cat: window adjustments both ways and several rekeys
    let big: Vec<u8> = (0..5_000_000u32).map(|i| (i % 253) as u8).collect();
    let r = c.execute("cat", Some(&big)).unwrap();
    assert!(r.stdout == big);
    assert!(c.transport().kex_count() >= 10, "kex count {}", c.transport().kex_count());
    // interactive shell
    let id = c.shell().unwrap();
    c.write(id, b"echo one\npwd\nexit 5\n").unwrap();
    let r = c.wait_close(id).unwrap();
    assert_eq!(r.stdout, b"one\n/\n");
    assert_eq!(r.exit_status, Some(5));
    // keep-alive and concurrent channels on one connection
    assert!(c.is_alive());
    let a = c.exec("cat").unwrap();
    let b = c.exec("cat").unwrap();
    c.write(b, b"to b").unwrap();
    c.write(a, b"to a").unwrap();
    c.send_eof(a).unwrap();
    c.send_eof(b).unwrap();
    assert_eq!(c.wait_close(b).unwrap().stdout, b"to b");
    assert_eq!(c.wait_close(a).unwrap().stdout, b"to a");
    // unknown subsystem is refused without breaking the connection
    assert!(matches!(c.subsystem("nope"), Err(SshError::Channel(_))));
    assert_eq!(c.execute("echo still", None).unwrap().stdout, b"still\n");
    assert!(server.commands().contains(&"exit 42".to_string()));
    c.disconnect();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn sftp_operations() {
    let root = temp_dir("sftp");
    let server = SshServer::start(SshServerConfig::default().user("alice", "wonderland").root(&root)).unwrap();
    let mut cfg = base(&server).password("wonderland");
    cfg.transport.rekey_limit = 1 << 20;
    let mut s = SftpClient::connect(&cfg).unwrap();
    assert_eq!(s.version(), 3);
    assert!(s.has_extension("posix-rename@openssh.com"));
    assert_eq!(s.realpath(".").unwrap(), "/");
    assert_eq!(s.realpath("a/../b/./c").unwrap(), "/b/c");
    s.mkdirs("in/sub/deep").unwrap();
    assert!(root.join("in/sub/deep").is_dir());
    assert!(s.stat("in/sub").unwrap().is_dir());
    // pipelined write and read of 3 MB (with odd chunk sizes and rekeying)
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i * 31 % 256) as u8).collect();
    s.chunk_size = 30000;
    s.write_file("in/big.bin", &big, false).unwrap();
    assert!(fs::read(root.join("in/big.bin")).unwrap() == big);
    s.max_outstanding = 3;
    assert!(s.read_file("in/big.bin").unwrap() == big);
    assert!(s.ssh().transport().kex_count() > 3);
    s.write_file("in/a.txt", b"hello", false).unwrap();
    s.write_file("in/a.txt", b" world", true).unwrap();
    assert_eq!(s.read_file("in/a.txt").unwrap(), b"hello world");
    s.write_file("in/a.txt", b"new", false).unwrap();
    assert_eq!(fs::read(root.join("in/a.txt")).unwrap(), b"new");
    assert_eq!(s.read_file("empty").err().map(|e| matches!(e, SshError::Sftp { code: 2, .. })), Some(true));
    s.write_file("empty.txt", b"", false).unwrap();
    assert_eq!(s.read_file("empty.txt").unwrap(), b"");
    let a = s.stat("in/a.txt").unwrap();
    assert_eq!(a.size, Some(3));
    assert!(a.is_file() && a.mtime.unwrap() > 0);
    s.setstat("in/a.txt", &FileAttrs { permissions: Some(0o600), ..Default::default() }).unwrap();
    assert_eq!(s.lstat("in/a.txt").unwrap().permissions.unwrap() & 0o777, 0o600);
    let mut names: Vec<String> = s.read_dir("in").unwrap().into_iter().map(|e| e.name).collect();
    names.sort();
    assert_eq!(names, vec!["a.txt", "big.bin", "sub"]);
    let entries = s.read_dir("in").unwrap();
    assert!(entries.iter().any(|e| e.longname.starts_with("d") && e.name == "sub"));
    // rename refuses to overwrite, posix-rename replaces
    s.write_file("in/b.txt", b"B", false).unwrap();
    assert!(matches!(s.rename("in/b.txt", "in/a.txt"), Err(SshError::Sftp { code: 4, .. })));
    s.rename("in/b.txt", "in/sub/b.txt").unwrap();
    s.posix_rename("in/sub/b.txt", "in/a.txt").unwrap();
    assert_eq!(fs::read(root.join("in/a.txt")).unwrap(), b"B");
    s.write_file("x", b"x", false).unwrap();
    s.rename_overwrite("x", "in/a.txt").unwrap();
    assert!(!s.exists("x").unwrap());
    // errors and the root jail
    assert!(matches!(s.remove("nope"), Err(SshError::Sftp { code: 2, .. })));
    assert!(s.rmdir("in").is_err());
    assert!(matches!(s.stat("../../../../etc/passwd"), Err(SshError::Sftp { code: 2, .. })));
    // handles
    let h = s.open("in/a.txt", camel_ssh::sftp::open_flags::READ, &FileAttrs::default()).unwrap();
    assert_eq!(s.fstat(&h).unwrap().size, Some(1));
    assert_eq!(s.read_at(&h, 0, 10).unwrap(), Some(b"x".to_vec()));
    assert_eq!(s.read_at(&h, 1, 10).unwrap(), None);
    s.close_handle(&h).unwrap();
    s.remove("in/a.txt").unwrap();
    s.remove("in/big.bin").unwrap();
    s.rmdir("in/sub/deep").unwrap();
    s.rmdir("in/sub").unwrap();
    s.rmdir("in").unwrap();
    assert!(!root.join("in").exists());
    // the subsystem can be started next to exec channels
    assert_eq!(s.ssh().execute("echo side", None).unwrap().stdout, b"side\n");
    s.close();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn errors_are_reported_cleanly() {
    // nothing listening
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut cfg = ClientConfig::new("127.0.0.1", port, "x").password("y");
    cfg.connect_timeout = Duration::from_secs(2);
    assert!(matches!(SshClient::connect(&cfg), Err(SshError::Io(_))));
    // no common algorithm
    let mut scfg = SshServerConfig::default().user("alice", "wonderland");
    scfg.transport.algorithms.ciphers = vec!["aes256-gcm@openssh.com".into()];
    let server = SshServer::start(scfg).unwrap();
    let mut cfg = base(&server).password("wonderland");
    cfg.transport.algorithms.ciphers = vec!["aes128-ctr".into()];
    match SshClient::connect(&cfg) {
        Err(SshError::Negotiation(m)) => assert!(m.contains("cipher"), "{}", m),
        other => panic!("{:?}", other.err()),
    }
    // the sftp subsystem is refused when the server has no root
    let c = SshClient::connect(&base(&server).password("wonderland")).unwrap();
    assert!(SftpClient::new(c).is_err());
}

#[test]
fn agent_authentication() {
    let ed = SshPrivateKey::generate_ed25519();
    let rsa_key = rsa();
    let stranger = SshPrivateKey::generate_ed25519();
    let server = server_with(&[&ed, &rsa_key], vec![]);
    // our test agent: the first identity is unknown to the server, the next ones are tried
    let agent = camel_ssh::agent::TestAgent::start(vec![stranger, rsa_key.clone(), ed.clone()]).unwrap();
    let mut cfg = base(&server);
    cfg.agent = Some(agent.path().to_path_buf());
    let c = SshClient::connect(&cfg).unwrap();
    assert_eq!(c.auth_method(), "publickey");
    let mut a = camel_ssh::agent::AgentClient::connect(agent.path()).unwrap();
    assert_eq!(a.identities().unwrap().len(), 3);
    // OpenSSH's ssh-agent, when installed
    let (Some(agent_bin), Some(add)) = (["/usr/bin/ssh-agent", "/usr/local/bin/ssh-agent"].iter().find(|p| std::path::Path::new(p).exists()), ["/usr/bin/ssh-add", "/usr/local/bin/ssh-add"].iter().find(|p| std::path::Path::new(p).exists())) else {
        return;
    };
    let dir = temp_dir("ssh-agent");
    let sock = dir.join("s");
    let mut child = std::process::Command::new(agent_bin).arg("-D").arg("-a").arg(&sock).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    for _ in 0..100 {
        if sock.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let keyfile = dir.join("id");
    std::fs::write(&keyfile, rsa_key.to_openssh_pem("t").unwrap()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&keyfile, std::fs::Permissions::from_mode(0o600)).unwrap();
    let st = std::process::Command::new(add).arg(&keyfile).env("SSH_AUTH_SOCK", &sock).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().unwrap();
    assert!(st.success());
    let mut cfg = base(&server);
    cfg.agent = Some(sock.clone());
    let c = SshClient::connect(&cfg).unwrap();
    assert_eq!(c.auth_method(), "publickey");
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
