//! Interoperability tests with GnuPG (skipped when `gpg` is not installed).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use camel_dataformats::pgp::{self, Signer, WriteOptions};

struct Gpg {
    home: PathBuf,
    /// `home` as the installed gpg expects it in `GNUPGHOME`.
    home_env: String,
}

impl Gpg {
    fn new(tag: &str) -> Option<Gpg> {
        let version = match Command::new("gpg").arg("--version").stderr(Stdio::null()).output() {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            _ => {
                eprintln!("gpg not installed: skipping interop test");
                return None;
            }
        };
        let home = std::env::temp_dir().join(format!("cdf-gpg-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).ok()?;
        }
        std::fs::write(home.join("gpg-agent.conf"), "allow-loopback-pinentry\ns2k-count 65536\n").ok()?;
        // Git for Windows ships an MSYS gpg (its `Home:` is a POSIX path) that reads
        // `GNUPGHOME=C:\...` as a relative path; give it `/c/...` instead.
        let msys = version.lines().any(|l| l.starts_with("Home: /"));
        let home_env = if msys { msys_path(&home) } else { home.to_string_lossy().to_string() };
        Some(Gpg { home, home_env })
    }

    /// Run gpg with stdin; returns (success, stdout, stderr).
    fn run(&self, args: &[&str], stdin: &[u8]) -> (bool, Vec<u8>, String) {
        use std::io::Write;
        let mut child = Command::new("gpg")
            .env("GNUPGHOME", &self.home_env)
            .args(["--batch", "--yes", "--no-tty", "--pinentry-mode", "loopback", "--trust-model", "always"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn gpg");
        let mut si = child.stdin.take().unwrap();
        let data = stdin.to_vec();
        let t = std::thread::spawn(move || {
            let _ = si.write_all(&data);
        });
        let out = child.wait_with_output().expect("gpg output");
        t.join().unwrap();
        (out.status.success(), out.stdout, String::from_utf8_lossy(&out.stderr).to_string())
    }

    fn ok(&self, args: &[&str], stdin: &[u8]) -> Vec<u8> {
        let (ok, out, err) = self.run(args, stdin);
        assert!(ok, "gpg {:?} failed: {}", args, err);
        out
    }

    fn fingerprint(&self, uid: &str) -> String {
        let out = String::from_utf8(self.ok(&["--with-colons", "--list-keys", uid], b"")).unwrap();
        out.lines().find(|l| l.starts_with("fpr:")).unwrap().split(':').nth(9).unwrap().to_string()
    }
}

impl Drop for Gpg {
    fn drop(&mut self) {
        let _ = Command::new("gpgconf").env("GNUPGHOME", &self.home_env).args(["--kill", "all"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// `C:\a\b` -> `/c/a/b`, the form MSYS programs take; other paths only get `/` separators.
fn msys_path(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        format!("/{}{}", (b[0] as char).to_ascii_lowercase(), &s[2..])
    } else {
        s
    }
}

fn write(dir: &Path, name: &str, data: &[u8]) -> String {
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p.to_string_lossy().replace('\\', "/")
}

/// Our Ed25519/Cv25519 keys imported into gpg: we encrypt+sign, gpg decrypts+verifies, and
/// the other way round.
#[test]
fn gpg_interop_with_our_keys() {
    let g = match Gpg::new("ours") {
        Some(g) => g,
        None => return,
    };
    let alice = pgp::generate_ed25519("Alice <alice@example.com>", Some("alicepw")).unwrap();
    g.ok(&["--import"], alice.public_keyring.as_bytes());
    g.ok(&["--passphrase", "alicepw", "--import"], alice.secret_keyring.as_bytes());

    // ours -> gpg
    let pubring = pgp::parse_keyring(alice.public_keyring.as_bytes()).unwrap();
    let secring = pgp::parse_keyring(alice.secret_keyring.as_bytes()).unwrap();
    let signer = Signer { key: &secring[0].primary, secret: secring[0].primary.unlock(Some("alicepw")).unwrap() };
    let msg = pgp::encrypt(b"Hello from rust-camel", &[&pubring[0].subkeys[0]], &[signer], &WriteOptions::default()).unwrap();
    let (ok, out, err) = g.run(&["--passphrase", "alicepw", "--status-fd", "2", "--decrypt"], &msg);
    assert!(ok, "gpg decrypt failed: {}", err);
    assert_eq!(out, b"Hello from rust-camel");
    assert!(err.contains("GOODSIG"), "{}", err);
    assert!(err.contains("DECRYPTION_OKAY"), "{}", err);

    // armored, ZLIB, AES-128, unsigned
    let opts = WriteOptions { sym: 7, compression: 2, ..Default::default() };
    let bin = pgp::encrypt(b"armored", &[&pubring[0].subkeys[0]], &[], &opts).unwrap();
    let armored = pgp::packet::armor("MESSAGE", &bin);
    assert_eq!(g.ok(&["--passphrase", "alicepw", "--decrypt"], armored.as_bytes()), b"armored");

    // gpg -> ours
    let enc = g.ok(&["--passphrase", "alicepw", "--armor", "--sign", "--encrypt", "-r", "alice@example.com", "-u", "alice@example.com"], b"Hello from gpg");
    let d = pgp::decrypt(&enc, &[(secring[0].clone(), Some("alicepw".into()))], &pubring).unwrap();
    assert_eq!(d.data, b"Hello from gpg");
    assert!(d.signatures.iter().any(|s| s.valid), "{:?}", d.signatures);

    // gpg sign only (compressed signed message) -> ours verify
    let signed = g.ok(&["--passphrase", "alicepw", "--sign", "-u", "alice@example.com"], b"signed data");
    let d = pgp::decrypt(&signed, &[], &pubring).unwrap();
    assert_eq!(d.data, b"signed data");
    assert!(d.signatures[0].valid);
}

/// Keys generated by gpg (RSA, Ed25519/Cv25519, NIST P-256) exported and used by us.
#[test]
fn gpg_interop_with_gpg_keys() {
    let g = match Gpg::new("theirs") {
        Some(g) => g,
        None => return,
    };
    let specs = [("Rsa User <rsa@example.com>", "rsa2048", "rsa2048"), ("Ed User <ed@example.com>", "ed25519", "cv25519"), ("Nist User <nist@example.com>", "nistp256", "nistp256")];
    for (uid, prim, sub) in specs {
        g.ok(&["--passphrase", "pw", "--quick-gen-key", uid, prim, "sign,cert", "never"], b"");
        let fpr = g.fingerprint(uid);
        g.ok(&["--passphrase", "pw", "--quick-add-key", &fpr, sub, "encr", "never"], b"");
        let public = g.ok(&["--armor", "--export", &fpr], b"");
        let secret = g.ok(&["--passphrase", "pw", "--armor", "--export-secret-keys", &fpr], b"");
        let pubring = pgp::parse_keyring(&public).unwrap();
        let secring = pgp::parse_keyring(&secret).unwrap();
        assert_eq!(pubring[0].primary.fingerprint_hex(), fpr, "{}", uid);
        let enc_key = pubring[0].subkeys.iter().find(|k| k.can_encrypt()).unwrap_or_else(|| panic!("no encryption key for {}", uid));

        // ours -> gpg (encrypt to their key, sign with their secret key)
        let sk = &secring[0].primary;
        let signer = Signer { key: sk, secret: sk.unlock(Some("pw")).unwrap_or_else(|e| panic!("{}: {}", uid, e)) };
        let msg = pgp::encrypt(format!("to {}", uid).as_bytes(), &[enc_key], &[signer], &WriteOptions::default()).unwrap();
        let (ok, out, err) = g.run(&["--passphrase", "pw", "--status-fd", "2", "--decrypt"], &msg);
        assert!(ok, "{}: gpg decrypt failed: {}", uid, err);
        assert_eq!(out, format!("to {}", uid).as_bytes());
        assert!(err.contains("GOODSIG"), "{}: {}", uid, err);

        // gpg -> ours
        let enc = g.ok(&["--passphrase", "pw", "--sign", "--encrypt", "-r", &fpr, "-u", &fpr], b"from gpg");
        let d = pgp::decrypt(&enc, &[(secring[0].clone(), Some("pw".into()))], &pubring).unwrap_or_else(|e| panic!("{}: {}", uid, e));
        assert_eq!(d.data, b"from gpg");
        assert!(d.signatures.iter().any(|s| s.valid), "{}: {:?}", uid, d.signatures);
    }
}

/// The `pgp` data format in a route with keyrings exported by gpg (files), then decrypted by gpg.
#[test]
fn gpg_route_with_keyring_files() {
    use camel_core::prelude::*;
    let g = match Gpg::new("route") {
        Some(g) => g,
        None => return,
    };
    g.ok(&["--passphrase", "pw", "--quick-gen-key", "Route <route@example.com>", "ed25519", "sign,cert", "never"], b"");
    let fpr = g.fingerprint("route@example.com");
    g.ok(&["--passphrase", "pw", "--quick-add-key", &fpr, "cv25519", "encr", "never"], b"");
    let public = g.ok(&["--export", &fpr], b"");
    let secret = g.ok(&["--passphrase", "pw", "--export-secret-keys", &fpr], b"");
    let pubfile = write(&g.home, "pub.gpg", &public);
    let secfile = write(&g.home, "sec.gpg", &secret);

    let ctx = CamelContext::new();
    camel_dataformats::register(&ctx);
    let yaml = format!(
        r#"
- from:
    uri: direct:enc
    steps:
      - marshal:
          pgp:
            keyFileName: "{pubfile}"
            keyUserid: route@example.com
            signatureKeyFileName: "{secfile}"
            signatureKeyUserid: route@example.com
            signaturePassword: pw
            armored: true
      - to: mock:enc
- from:
    uri: direct:dec
    steps:
      - unmarshal:
          pgp:
            keyFileName: "{secfile}"
            password: pw
            signatureKeyFileName: "{pubfile}"
            signatureVerificationOption: required
      - to: mock:dec
"#
    );
    camel_yaml::add_routes_from_yaml(&ctx, &yaml).unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:enc", "route secret").unwrap();
    let enc = ctx.mock_endpoint("mock:enc").unwrap().received_exchanges()[0].body().to_string();
    assert!(enc.starts_with("-----BEGIN PGP MESSAGE-----"));
    let (ok, out, err) = g.run(&["--passphrase", "pw", "--status-fd", "2", "--decrypt"], enc.as_bytes());
    assert!(ok && err.contains("GOODSIG"), "{}", err);
    assert_eq!(out, b"route secret");
    t.send_body("direct:dec", enc.clone()).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:dec").unwrap().received_exchanges()[0].body().to_bytes(), b"route secret");
    ctx.stop().unwrap();
}
