//! Interoperability with OpenSSH, when its binaries are installed (skipped otherwise):
//! our client against an `sshd` started on an ephemeral port with a temporary
//! configuration, and the OpenSSH `ssh` / `sftp` / `scp` clients against our server.

use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use camel_ssh::client::{ClientConfig, HostKeyCheck, SshClient};
use camel_ssh::keys::SshPrivateKey;
use camel_ssh::known_hosts::KnownHosts;
use camel_ssh::sftp::SftpClient;
use camel_ssh::testing::{temp_dir, SshServer, SshServerConfig};
use camel_ssh::transport::{SUPPORTED_CIPHERS, SUPPORTED_KEX};

fn find(bin: &str) -> Option<PathBuf> {
    for dir in ["/usr/bin", "/usr/sbin", "/usr/local/bin", "/usr/local/sbin", "/bin", "/sbin"] {
        let p = Path::new(dir).join(bin);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn keygen(keygen: &Path, kind: &str, bits: Option<&str>, path: &Path) {
    let mut c = Command::new(keygen);
    c.args(["-q", "-t", kind, "-N", "", "-C", "test", "-f"]).arg(path);
    if let Some(b) = bits {
        c.args(["-b", b]);
    }
    let st = c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert!(st.success(), "ssh-keygen -t {} failed", kind);
}

fn current_user() -> String {
    Command::new("id").arg("-un").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "root".into())
}

struct Sshd {
    child: Child,
    port: u16,
    dir: PathBuf,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Starts sshd with public key authentication for the current user, or None when not possible.
fn start_sshd(user_keys: &[String]) -> Option<Sshd> {
    start_sshd_with(user_keys, &|_| String::new())
}

/// `prep` runs after the host keys exist (in the sshd directory) and returns extra
/// `sshd_config` lines.
fn start_sshd_with(user_keys: &[String], prep: &dyn Fn(&Path) -> String) -> Option<Sshd> {
    let sshd = find("sshd")?;
    let kg = find("ssh-keygen")?;
    let is_root = current_user() == "root";
    if is_root && !Path::new("/run/sshd").exists() && fs::create_dir_all("/run/sshd").is_err() {
        eprintln!("skipping sshd interop: no privilege separation directory");
        return None;
    }
    let dir = temp_dir("sshd");
    for (kind, bits) in [("ed25519", None), ("ecdsa", Some("256")), ("rsa", Some("2048"))] {
        keygen(&kg, kind, bits, &dir.join(format!("host_{}", kind)));
    }
    fs::write(dir.join("authorized_keys"), user_keys.join("\n") + "\n").unwrap();
    let extra = prep(&dir);
    for _attempt in 0..3 {
        let port = free_port();
        let cfg = format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_ed25519\nHostKey {d}/host_ecdsa\nHostKey {d}/host_rsa\n\
             PidFile {d}/sshd.pid\nAuthorizedKeysFile {d}/authorized_keys\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n\
             UsePAM no\nStrictModes no\nPermitRootLogin prohibit-password\nSubsystem sftp internal-sftp\nLogLevel ERROR\n\
             PubkeyAcceptedAlgorithms +rsa-sha2-256,rsa-sha2-512\n{extra}",
            port = port,
            d = dir.display(),
            extra = extra
        );
        fs::write(dir.join("sshd_config"), cfg).unwrap();
        let mut child = Command::new(&sshd)
            .args(["-D", "-e", "-f"])
            .arg(dir.join("sshd_config"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Some(Sshd { child, port, dir });
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    eprintln!("skipping sshd interop: sshd did not start");
    let _ = fs::remove_dir_all(&dir);
    None
}

fn openssh_key(kg: &Path, dir: &Path, kind: &str, bits: Option<&str>) -> (SshPrivateKey, String) {
    let path = dir.join(format!("user_{}", kind));
    keygen(kg, kind, bits, &path);
    let key = SshPrivateKey::from_text(&fs::read_to_string(&path).unwrap()).unwrap();
    let public = fs::read_to_string(path.with_extension("pub")).unwrap();
    assert_eq!(key.public_key().to_openssh("test"), public.trim(), "public key derived from the private key file");
    (key, public.trim().to_string())
}

#[test]
fn our_client_against_openssh_sshd() {
    let kg = match find("ssh-keygen") {
        Some(k) => k,
        None => return eprintln!("skipping: ssh-keygen not installed"),
    };
    let kdir = temp_dir("interop-keys");
    let (ed, ed_pub) = openssh_key(&kg, &kdir, "ed25519", None);
    let (ec, ec_pub) = openssh_key(&kg, &kdir, "ecdsa", Some("256"));
    let (rsa, rsa_pub) = openssh_key(&kg, &kdir, "rsa", Some("2048"));
    // PEM (PKCS#1) RSA keys as well
    let pem_path = kdir.join("user_rsa_pem");
    let st = Command::new(&kg).args(["-q", "-t", "rsa", "-b", "2048", "-m", "PEM", "-N", "", "-C", "pem", "-f"]).arg(&pem_path).status().unwrap();
    assert!(st.success());
    let pem_text = fs::read_to_string(&pem_path).unwrap();
    assert!(pem_text.starts_with("-----BEGIN RSA PRIVATE KEY-----"));
    let pem = SshPrivateKey::from_text(&pem_text).unwrap();
    let pem_pub = fs::read_to_string(pem_path.with_extension("pub")).unwrap();
    let sshd = match start_sshd(&[ed_pub.clone(), ec_pub, rsa_pub, pem_pub.trim().to_string()]) {
        Some(s) => s,
        None => return,
    };
    let user = current_user();
    // host keys as known_hosts via ssh-keyscan-like derivation from the files
    let mut known = String::new();
    for kind in ["ed25519", "ecdsa", "rsa"] {
        let line = fs::read_to_string(sshd.dir.join(format!("host_{}.pub", kind))).unwrap();
        let mut it = line.split_whitespace();
        known.push_str(&format!("[127.0.0.1]:{} {} {}\n", sshd.port, it.next().unwrap(), it.next().unwrap()));
    }
    let check = HostKeyCheck::KnownHosts { known: KnownHosts::parse(&known), strict: true };
    let base = ClientConfig::new("127.0.0.1", sshd.port, &user).host_key_check(check);

    // every cipher x kex, with each host key algorithm and user key type
    let host_algs = ["ssh-ed25519", "ecdsa-sha2-nistp256", "rsa-sha2-512", "rsa-sha2-256"];
    let keys = [&ed, &ec, &rsa, &pem];
    let mut n = 0;
    for (ci, cipher) in SUPPORTED_CIPHERS.iter().enumerate() {
        for (ki, kex) in SUPPORTED_KEX.iter().enumerate() {
            let mut cfg = base.clone().private_key(keys[(ci + ki) % keys.len()].clone());
            cfg.transport.algorithms.ciphers = vec![cipher.to_string()];
            cfg.transport.algorithms.kex = vec![kex.to_string()];
            cfg.transport.algorithms.host_key = vec![host_algs[(ci + ki) % host_algs.len()].to_string()];
            cfg.transport.algorithms.macs = vec![["hmac-sha2-256", "hmac-sha2-256-etm@openssh.com", "hmac-sha2-512-etm@openssh.com"][ki % 3].to_string()];
            let mut c = SshClient::connect(&cfg).unwrap_or_else(|e| panic!("{} / {} : {}", cipher, kex, e));
            assert!(c.transport().strict_kex());
            let r = c.execute("echo hello; echo oops >&2; exit 3", None).unwrap();
            assert_eq!(r.stdout, b"hello\n");
            assert_eq!(r.stderr, b"oops\n");
            assert_eq!(r.exit_status, Some(3));
            c.disconnect();
            n += 1;
        }
    }
    assert_eq!(n, SUPPORTED_CIPHERS.len() * SUPPORTED_KEX.len());

    // stdin, large output, rekeying, multiple channels and SFTP on one connection
    let mut cfg = base.clone().private_key(ed.clone());
    cfg.transport.rekey_limit = 256 * 1024;
    let mut c = SshClient::connect(&cfg).unwrap();
    let big: Vec<u8> = (0..1_500_000u32).map(|i| (i % 251) as u8).collect();
    let r = c.execute("cat", Some(&big)).unwrap();
    assert_eq!(r.stdout.len(), big.len());
    assert!(r.stdout == big);
    assert!(c.transport().kex_count() > 2, "rekeyed {} times", c.transport().kex_count());
    let r = c.execute("head -c 300000 /dev/zero | tr '\\0' x", None).unwrap();
    assert_eq!(r.stdout.len(), 300000);
    let work = temp_dir("interop-sftp");
    let mut sftp = SftpClient::new(c).unwrap();
    assert!(sftp.has_extension("posix-rename@openssh.com"));
    let w = work.display().to_string();
    sftp.mkdirs(&format!("{}/a/b", w)).unwrap();
    sftp.write_file(&format!("{}/a/b/f.bin", w), &big, false).unwrap();
    assert_eq!(fs::read(work.join("a/b/f.bin")).unwrap(), big);
    sftp.write_file(&format!("{}/a/b/f.bin", w), b"tail", true).unwrap();
    let back = sftp.read_file(&format!("{}/a/b/f.bin", w)).unwrap();
    assert_eq!(back.len(), big.len() + 4);
    assert!(back.ends_with(b"tail"));
    assert_eq!(sftp.stat(&format!("{}/a/b/f.bin", w)).unwrap().size, Some(big.len() as u64 + 4));
    let names: Vec<String> = sftp.read_dir(&format!("{}/a/b", w)).unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, vec!["f.bin"]);
    sftp.write_file(&format!("{}/a/x.txt", w), b"x", false).unwrap();
    sftp.posix_rename(&format!("{}/a/b/f.bin", w), &format!("{}/a/y.bin", w)).unwrap();
    assert_eq!(sftp.realpath(&format!("{}/a/b/..", w)).unwrap(), format!("{}/a", fs::canonicalize(&work).unwrap().display()));
    sftp.remove(&format!("{}/a/y.bin", w)).unwrap();
    sftp.remove(&format!("{}/a/x.txt", w)).unwrap();
    sftp.rmdir(&format!("{}/a/b", w)).unwrap();
    assert!(!sftp.exists(&format!("{}/a/b", w)).unwrap());
    sftp.close();

    // a wrong host key is refused; an unauthorized user key is refused
    let other = SshPrivateKey::generate_ed25519().public_key();
    let bad = ClientConfig::new("127.0.0.1", sshd.port, &user)
        .private_key(ed.clone())
        .host_key_check(HostKeyCheck::KnownHosts { known: KnownHosts::parse(&KnownHosts::entry_line("127.0.0.1", sshd.port, &other, true)), strict: true });
    let mut bad = bad;
    bad.transport.algorithms.host_key = vec!["ssh-ed25519".into()];
    assert!(matches!(SshClient::connect(&bad), Err(camel_ssh::SshError::HostKey(_))));
    let stranger = base.clone().private_key(SshPrivateKey::generate_ed25519());
    assert!(matches!(SshClient::connect(&stranger), Err(camel_ssh::SshError::Auth(_))));
    let _ = fs::remove_dir_all(&kdir);
    let _ = fs::remove_dir_all(&work);
}

fn ssh_args(dir: &Path, key: &Path) -> Vec<String> {
    vec![
        "-F".into(),
        "/dev/null".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        format!("UserKnownHostsFile={}", dir.join("known_hosts").display()),
        "-o".into(),
        "IdentitiesOnly=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-o".into(),
        "LogLevel=ERROR".into(),
        "-i".into(),
        key.display().to_string(),
    ]
}

#[test]
fn openssh_clients_against_our_server() {
    let (ssh, kg) = match (find("ssh"), find("ssh-keygen")) {
        (Some(a), Some(b)) => (a, b),
        _ => return eprintln!("skipping: OpenSSH client not installed"),
    };
    let dir = temp_dir("interop-ours");
    let root = dir.join("root");
    fs::create_dir_all(&root).unwrap();
    let (_ed, ed_pub) = openssh_key(&kg, &dir, "ed25519", None);
    let (_ec, ec_pub) = openssh_key(&kg, &dir, "ecdsa", Some("256"));
    let cfg = SshServerConfig::default()
        .user("test", "secret")
        .authorized_key("test", camel_ssh::SshPublicKey::from_openssh(&ed_pub).unwrap())
        .authorized_key("test", camel_ssh::SshPublicKey::from_openssh(&ec_pub).unwrap())
        .host_key(SshPrivateKey::generate_ed25519())
        .host_key(SshPrivateKey::generate_ecdsa(camel_tls::crypto::ec::EcCurve::P256))
        .root(&root);
    let server = SshServer::start(cfg).unwrap();
    fs::write(dir.join("known_hosts"), server.known_hosts_text("127.0.0.1")).unwrap();
    let port = server.port().to_string();
    let run = |key: &str, extra: &[&str], cmd: &str, stdin: Option<&[u8]>| -> (i32, Vec<u8>, Vec<u8>) {
        let mut c = Command::new(&ssh);
        c.args(ssh_args(&dir, &dir.join(key))).args(["-p", &port]).args(extra).arg("test@127.0.0.1").arg(cmd);
        c.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        if let Some(input) = stdin {
            let mut si = child.stdin.take().unwrap();
            let data = input.to_vec();
            std::thread::spawn(move || {
                let _ = si.write_all(&data);
            });
        }
        let out = child.wait_with_output().unwrap();
        (out.status.code().unwrap_or(-1), out.stdout, out.stderr)
    };
    for cipher in SUPPORTED_CIPHERS {
        for kex in SUPPORTED_KEX {
            for hk in ["ssh-ed25519", "ecdsa-sha2-nistp256"] {
                let (code, out, err) = run(
                    "user_ed25519",
                    &["-o", &format!("Ciphers={}", cipher), "-o", &format!("KexAlgorithms={}", kex), "-o", &format!("HostKeyAlgorithms={}", hk)],
                    "echo hello world",
                    None,
                );
                assert_eq!((code, String::from_utf8_lossy(&out).to_string()), (0, "hello world\n".into()), "{} {} {}: {}", cipher, kex, hk, String::from_utf8_lossy(&err));
            }
        }
    }
    for mac in ["hmac-sha2-256", "hmac-sha2-512", "hmac-sha2-256-etm@openssh.com", "hmac-sha2-512-etm@openssh.com"] {
        let (code, out, err) = run("user_ecdsa", &["-o", "Ciphers=aes256-ctr", "-o", &format!("MACs={}", mac)], "pwd", None);
        assert_eq!((code, out), (0, b"/\n".to_vec()), "{}: {}", mac, String::from_utf8_lossy(&err));
    }
    let (code, _, err) = run("user_ed25519", &[], "exit 7", None);
    assert_eq!(code, 7, "{}", String::from_utf8_lossy(&err));
    let (code, _, err) = run("user_ed25519", &[], "nosuchcmd", None);
    assert_eq!(code, 127);
    assert!(String::from_utf8_lossy(&err).contains("command not found"));
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 256) as u8).collect();
    let (code, out, _) = run("user_ed25519", &["-o", "RekeyLimit=512K"], "cat", Some(&big));
    assert_eq!(code, 0);
    assert!(out == big, "cat roundtrip of 3 MB with rekeying");

    // sftp batch mode
    if let Some(sftp) = find("sftp") {
        fs::write(dir.join("local.txt"), "local content\n").unwrap();
        let batch = format!(
            "mkdir up\nput {local} up/remote.txt\nrename up/remote.txt up/moved.txt\nls up\nget up/moved.txt {back}\nchmod 600 up/moved.txt\n",
            local = dir.join("local.txt").display(),
            back = dir.join("back.txt").display()
        );
        fs::write(dir.join("batch"), batch).unwrap();
        let out = Command::new(&sftp)
            .args(ssh_args(&dir, &dir.join("user_ed25519")))
            .args(["-P", &port, "-b"])
            .arg(dir.join("batch"))
            .arg("test@127.0.0.1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "sftp failed: {}", String::from_utf8_lossy(&out.stderr));
        assert!(String::from_utf8_lossy(&out.stdout).contains("moved.txt"));
        assert_eq!(fs::read_to_string(root.join("up/moved.txt")).unwrap(), "local content\n");
        assert_eq!(fs::read_to_string(dir.join("back.txt")).unwrap(), "local content\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(root.join("up/moved.txt")).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
    // scp uses the SFTP protocol by default since OpenSSH 9.0
    if let Some(scp) = find("scp") {
        let big_path = dir.join("big.bin");
        fs::write(&big_path, &big).unwrap();
        let out = Command::new(&scp)
            .args(ssh_args(&dir, &dir.join("user_ed25519")))
            .args(["-P", &port])
            .arg(&big_path)
            .arg("test@127.0.0.1:big-copy.bin")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "scp failed: {}", String::from_utf8_lossy(&out.stderr));
        assert!(fs::read(root.join("big-copy.bin")).unwrap() == big);
    }
    // a key the server does not know is rejected
    let (_x, _) = openssh_key(&kg, &dir, "rsa", Some("2048"));
    let (code, _, _) = run("user_rsa", &[], "echo hi", None);
    assert_eq!(code, 255);
    server.stop();
    let _ = fs::remove_dir_all(&dir);
}

fn compressible(n: usize) -> Vec<u8> {
    (0..n).map(|i| b"the quick brown fox jumps over the lazy dog\n"[i % 44]).collect()
}

#[test]
fn zlib_compression_ours_and_openssh() {
    let comp = vec!["zlib@openssh.com".to_string(), "zlib".to_string(), "none".to_string()];
    // our client <-> our server, delayed and immediate zlib
    for algs in [vec!["zlib@openssh.com".to_string()], vec!["zlib".to_string()]] {
        let mut cfg = SshServerConfig::default().user("test", "secret");
        cfg.transport.compression = comp.clone();
        let server = SshServer::start(cfg).unwrap();
        let mut ccfg = ClientConfig::new("127.0.0.1", server.port(), "test").password("secret");
        ccfg.transport.compression = algs.clone();
        let mut c = SshClient::connect(&ccfg).unwrap();
        assert_eq!(c.transport().negotiated().comp_c2s, algs[0]);
        let data = compressible(300_000);
        let r = c.execute("cat", Some(&data)).unwrap();
        assert_eq!(r.stdout, data);
        // rekeying keeps the compression stream going
        c.transport_mut().rekey().unwrap();
        assert_eq!(c.execute("echo after rekey", None).unwrap().stdout, b"after rekey\n");
        c.disconnect();
        server.stop();
    }
    // OpenSSH ssh -C against our server
    if let (Some(ssh), Some(kg)) = (find("ssh"), find("ssh-keygen")) {
        let dir = temp_dir("interop-comp");
        let (_ed, ed_pub) = openssh_key(&kg, &dir, "ed25519", None);
        let mut cfg = SshServerConfig::default().authorized_key("test", camel_ssh::SshPublicKey::from_openssh(&ed_pub).unwrap()).host_key(SshPrivateKey::generate_ed25519());
        cfg.transport.compression = comp.clone();
        let server = SshServer::start(cfg).unwrap();
        fs::write(dir.join("known_hosts"), server.known_hosts_text("127.0.0.1")).unwrap();
        let data = compressible(200_000);
        let mut c = Command::new(&ssh);
        c.args(ssh_args(&dir, &dir.join("user_ed25519"))).args(["-p", &server.port().to_string(), "-C", "test@127.0.0.1", "cat"]);
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let mut si = child.stdin.take().unwrap();
        let d2 = data.clone();
        std::thread::spawn(move || {
            let _ = si.write_all(&d2);
        });
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout == data, "ssh -C cat: {} of {} bytes", out.stdout.len(), data.len());
        server.stop();
    }
    // our client against OpenSSH sshd (Compression yes = delayed zlib@openssh.com)
    if let Some(kg) = find("ssh-keygen") {
        let kdir = temp_dir("interop-comp-keys");
        let (ed, ed_pub) = openssh_key(&kg, &kdir, "ed25519", None);
        let sshd = match start_sshd(&[ed_pub]) {
            Some(s) => s,
            None => return,
        };
        let mut cfg = ClientConfig::new("127.0.0.1", sshd.port, &current_user()).private_key(ed);
        cfg.transport.compression = vec!["zlib@openssh.com".into()];
        let mut c = SshClient::connect(&cfg).unwrap();
        assert_eq!(c.transport().negotiated().comp_s2c, "zlib@openssh.com");
        let data = compressible(250_000);
        let r = c.execute("cat", Some(&data)).unwrap();
        assert!(r.stdout == data, "sshd cat: {} of {} bytes", r.stdout.len(), data.len());
        let r = c.execute("head -c 100000 /dev/zero | tr '\\0' x", None).unwrap();
        assert_eq!(r.stdout, vec![b'x'; 100_000]);
        c.disconnect();
    }
}

#[test]
fn openssh_certificates_user_and_host() {
    use camel_ssh::cert::{SshCertificate, HOST_CERT, USER_CERT};
    let now = camel_ssh::cert::now_secs();
    // our client <-> our server: user certificate + host certificate trusted via @cert-authority
    let user_ca = SshPrivateKey::generate_ed25519();
    let host_ca = SshPrivateKey::generate_ecdsa(camel_tls::crypto::ec::EcCurve::P256);
    let user_key = SshPrivateKey::generate_ed25519();
    let host_key = SshPrivateKey::generate_ed25519();
    let user_cert = SshCertificate::sign(&user_key.public_key(), &user_ca, USER_CERT, "alice@test", &["alice"], now - 60, now + 3600, 1).unwrap();
    let host_cert = SshCertificate::sign(&host_key.public_key(), &host_ca, HOST_CERT, "host", &["127.0.0.1"], now - 60, now + 3600, 2).unwrap();
    let mut cfg = SshServerConfig::default().host_key(host_key.clone());
    cfg.trusted_user_ca_keys = vec![user_ca.public_key()];
    cfg.transport.host_certificates = vec![host_cert.clone()];
    let server = SshServer::start(cfg).unwrap();
    let known = format!("@cert-authority 127.0.0.1,[127.0.0.1]:* {}\n", host_ca.public_key().to_openssh(""));
    let check = HostKeyCheck::KnownHosts { known: KnownHosts::parse(&known), strict: true };
    let mut ccfg = ClientConfig::new("127.0.0.1", server.port(), "alice").private_key(user_key.clone()).host_key_check(check.clone());
    ccfg.certificates = vec![user_cert.clone()];
    let mut c = SshClient::connect(&ccfg).unwrap();
    assert!(c.transport().negotiated().host_key.ends_with("-cert-v01@openssh.com"));
    assert_eq!(c.execute("echo cert", None).unwrap().stdout, b"cert\n");
    c.disconnect();
    // wrong principal / plain key without authorized_keys entry: refused
    let mut wrong = ccfg.clone();
    wrong.username = "bob".into();
    assert!(SshClient::connect(&wrong).is_err());
    // an untrusted host CA is refused under strict checking
    let other = format!("@cert-authority * {}\n", SshPrivateKey::generate_ed25519().public_key().to_openssh(""));
    let mut bad = ccfg.clone();
    bad.host_key_check = HostKeyCheck::KnownHosts { known: KnownHosts::parse(&other), strict: true };
    assert!(SshClient::connect(&bad).is_err());
    server.stop();

    let (ssh, kg) = match (find("ssh"), find("ssh-keygen")) {
        (Some(a), Some(b)) => (a, b),
        _ => return eprintln!("skipping: OpenSSH not installed"),
    };
    let dir = temp_dir("interop-certs");
    // CA keys and certificates made by ssh-keygen
    keygen(&kg, "ed25519", None, &dir.join("user_ca"));
    keygen(&kg, "ed25519", None, &dir.join("host_ca"));
    let (ukey, _upub) = openssh_key(&kg, &dir, "ed25519", None);
    let user = current_user();
    let st = Command::new(&kg).args(["-q", "-s"]).arg(dir.join("user_ca")).args(["-I", "camel-user", "-n", &format!("{},test", user), "-V", "-5m:+1h"]).arg(dir.join("user_ed25519.pub")).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert!(st.success());
    let ucert = SshCertificate::from_openssh(&fs::read_to_string(dir.join("user_ed25519-cert.pub")).unwrap()).unwrap();
    assert!(ucert.signature_valid());
    assert_eq!(ucert.cert_type, USER_CERT);
    let user_ca_pub = camel_ssh::SshPublicKey::from_openssh(&fs::read_to_string(dir.join("user_ca.pub")).unwrap()).unwrap();
    let host_ca_pub_line = fs::read_to_string(dir.join("host_ca.pub")).unwrap();

    // OpenSSH ssh (user certificate, @cert-authority known_hosts) against our server
    let our_host = SshPrivateKey::generate_ed25519();
    fs::write(dir.join("our_host.pub"), our_host.public_key().to_openssh("host") + "\n").unwrap();
    let st = Command::new(&kg).args(["-q", "-s"]).arg(dir.join("host_ca")).args(["-I", "our-host", "-h", "-n", "127.0.0.1", "-V", "-5m:+1h"]).arg(dir.join("our_host.pub")).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert!(st.success());
    let our_host_cert = SshCertificate::from_openssh(&fs::read_to_string(dir.join("our_host-cert.pub")).unwrap()).unwrap();
    let mut cfg = SshServerConfig::default().host_key(our_host);
    cfg.trusted_user_ca_keys = vec![user_ca_pub.clone()];
    cfg.transport.host_certificates = vec![our_host_cert];
    let server = SshServer::start(cfg).unwrap();
    fs::write(dir.join("known_hosts"), format!("@cert-authority [127.0.0.1]:{} {}", server.port(), host_ca_pub_line)).unwrap();
    let out = Command::new(&ssh)
        .args(ssh_args(&dir, &dir.join("user_ed25519")))
        .args(["-o", &format!("CertificateFile={}", dir.join("user_ed25519-cert.pub").display()), "-p", &server.port().to_string(), "test@127.0.0.1", "echo via certificate"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!((out.status.code(), String::from_utf8_lossy(&out.stdout).to_string()), (Some(0), "via certificate\n".into()), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(server.auth_log().iter().any(|l| l == "test:publickey:ok"), "{:?}", server.auth_log());
    server.stop();

    // our client (user certificate, host CA) against sshd with TrustedUserCAKeys + HostCertificate
    let kgc = kg.clone();
    let sshd = match start_sshd_with(&[], &move |d: &Path| {
        fs::copy(dir.join("user_ca.pub"), d.join("user_ca.pub")).unwrap();
        let st = Command::new(&kgc).args(["-q", "-s"]).arg(dir.join("host_ca")).args(["-I", "sshd-host", "-h", "-n", "127.0.0.1", "-V", "-5m:+1h"]).arg(d.join("host_ed25519.pub")).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
        assert!(st.success());
        format!("TrustedUserCAKeys {}/user_ca.pub\nHostCertificate {}/host_ed25519-cert.pub\n", d.display(), d.display())
    }) {
        Some(s) => s,
        None => return,
    };
    let known = format!("@cert-authority [127.0.0.1]:{} {}", sshd.port, host_ca_pub_line);
    let mut cfg = ClientConfig::new("127.0.0.1", sshd.port, &user).private_key(ukey).host_key_check(HostKeyCheck::KnownHosts { known: KnownHosts::parse(&known), strict: true });
    cfg.certificates = vec![ucert];
    let mut c = SshClient::connect(&cfg).unwrap();
    assert_eq!(c.transport().negotiated().host_key, "ssh-ed25519-cert-v01@openssh.com");
    assert_eq!(c.execute("echo sshd cert", None).unwrap().stdout, b"sshd cert\n");
    c.disconnect();
}

/// A TCP echo server (each connection echoed until EOF); returns its port.
fn echo_server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut r = s.try_clone().unwrap();
                let mut w = s;
                let _ = std::io::copy(&mut r, &mut w);
                let _ = w.shutdown(std::net::Shutdown::Write);
            });
        }
    });
    port
}

/// Sends `data` through `port` and reads the echo back.
fn through(port: u16, data: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let deadline = Instant::now() + Duration::from_secs(10);
    let s = loop {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("connect {}: {}", port, e),
        }
    };
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut w = s.try_clone().unwrap();
    let d = data.to_vec();
    let h = std::thread::spawn(move || {
        w.write_all(&d).unwrap();
        w.shutdown(std::net::Shutdown::Write).unwrap();
    });
    let mut out = Vec::new();
    let mut r = s;
    let _ = r.read_to_end(&mut out);
    h.join().unwrap();
    out
}

#[test]
fn port_forwarding_ours_and_openssh() {
    let echo = echo_server();
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    // our client <-> our server
    let server = SshServer::start(SshServerConfig::default().user("test", "secret")).unwrap();
    let ccfg = ClientConfig::new("127.0.0.1", server.port(), "test").password("secret");
    let mut c = SshClient::connect(&ccfg).unwrap();
    let id = c.open_direct_tcpip("127.0.0.1", echo, "127.0.0.1", 1234).unwrap();
    c.write(id, b"direct").unwrap();
    c.send_eof(id).unwrap();
    assert_eq!(c.wait_close(id).unwrap().stdout, b"direct");
    let mut fwd = c.start_local_forward("127.0.0.1:0", "127.0.0.1", echo).unwrap();
    let lp = fwd.local_port.unwrap();
    assert_eq!(through(lp, &data), data);
    assert_eq!(through(lp, b"second connection"), b"second connection");
    fwd.stop();
    let c = SshClient::connect(&ccfg).unwrap();
    let mut rfwd = c.start_remote_forward(0, "127.0.0.1", echo).unwrap();
    let rp = rfwd.remote_port.unwrap();
    assert_eq!(through(rp, &data), data);
    rfwd.stop();
    server.stop();

    let (ssh, kg) = match (find("ssh"), find("ssh-keygen")) {
        (Some(a), Some(b)) => (a, b),
        _ => return eprintln!("skipping: OpenSSH not installed"),
    };
    // OpenSSH ssh -L / -R against our server
    let dir = temp_dir("interop-fwd");
    let (ukey, upub) = openssh_key(&kg, &dir, "ed25519", None);
    let server = SshServer::start(SshServerConfig::default().authorized_key("test", camel_ssh::SshPublicKey::from_openssh(&upub).unwrap()).host_key(SshPrivateKey::generate_ed25519())).unwrap();
    fs::write(dir.join("known_hosts"), server.known_hosts_text("127.0.0.1")).unwrap();
    for (flag, check_port) in [("-L", free_port()), ("-R", free_port())] {
        let spec = format!("{}:127.0.0.1:{}", check_port, echo);
        let mut child = Command::new(&ssh)
            .args(ssh_args(&dir, &dir.join("user_ed25519")))
            .args(["-N", "-o", "ExitOnForwardFailure=yes", flag, &spec, "-p", &server.port().to_string(), "test@127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        assert_eq!(through(check_port, &data), data, "ssh {}", flag);
        let _ = child.kill();
        let _ = child.wait();
    }
    server.stop();

    // our client against OpenSSH sshd: local and remote forwards
    let sshd = match start_sshd(&[upub]) {
        Some(s) => s,
        None => return,
    };
    let cfg = ClientConfig::new("127.0.0.1", sshd.port, &current_user()).private_key(ukey);
    let mut fwd = SshClient::connect(&cfg).unwrap().start_local_forward("127.0.0.1:0", "127.0.0.1", echo).unwrap();
    assert_eq!(through(fwd.local_port.unwrap(), &data), data);
    fwd.stop();
    let mut rfwd = SshClient::connect(&cfg).unwrap().start_remote_forward(0, "127.0.0.1", echo).unwrap();
    assert_eq!(through(rfwd.remote_port.unwrap(), &data), data);
    rfwd.stop();
}
