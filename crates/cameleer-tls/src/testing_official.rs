//! Helpers for the official-client cross-verification suites (`tests/official_*.rs`).
//!
//! Those suites start a Cameleer embedded server, emulator or route on an ephemeral port
//! and drive it with independently written, officially published clients (librdkafka /
//! kcat, pymongo, the Cassandra and Elasticsearch Python drivers, boto3, the Azure SDK for
//! Python, gcloud / gsutil / bq, grpcio, Go's crypto/tls and net/http) run as subprocesses.
//! The client scripts live in `scripts/official-clients/`; `scripts/official-clients.sh`
//! installs the clients. The suites do nothing unless `CAMEL_OFFICIAL_CLIENTS=1` is set.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Whether the official-client suites are enabled (`CAMEL_OFFICIAL_CLIENTS=1`); prints a
/// skip notice otherwise.
pub fn enabled() -> bool {
    let on = std::env::var("CAMEL_OFFICIAL_CLIENTS").map(|v| v == "1").unwrap_or(false);
    if !on {
        eprintln!("skipped: set CAMEL_OFFICIAL_CLIENTS=1 after `scripts/official-clients.sh install`");
    }
    on
}

/// The workspace root (two levels above a crate's manifest directory).
pub fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir.pop();
    dir
}

/// A client script under `scripts/official-clients/`.
pub fn script(name: &str) -> PathBuf {
    workspace_root().join("scripts/official-clients").join(name)
}

/// The Python interpreter that sees the distribution's `python3-*` client packages
/// (`CAMEL_OFFICIAL_PYTHON`, default `/usr/bin/python3.12`, the Ubuntu 24.04 system Python).
pub fn python() -> String {
    std::env::var("CAMEL_OFFICIAL_PYTHON").unwrap_or_else(|_| {
        if Path::new("/usr/bin/python3.12").exists() {
            "/usr/bin/python3.12".into()
        } else {
            "python3".into()
        }
    })
}

/// The Go toolchain (`CAMEL_OFFICIAL_GO`, default `/usr/local/go/bin/go` or `go`).
pub fn go() -> String {
    std::env::var("CAMEL_OFFICIAL_GO").unwrap_or_else(|_| {
        if Path::new("/usr/local/go/bin/go").exists() {
            "/usr/local/go/bin/go".into()
        } else {
            "go".into()
        }
    })
}

/// A fresh scratch directory for a test (certificates, client configuration files).
pub fn temp_dir(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("camel-official-{}-{}-{}", tag, std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// PEM files of the mutual-TLS test PKI ([`super::mtls`]) for external clients.
pub struct PemFiles {
    pub dir: PathBuf,
    /// The CA certificate that issued the server and client certificates.
    pub ca: PathBuf,
    /// The `localhost` / `127.0.0.1` server certificate and key.
    pub server_cert: PathBuf,
    pub server_key: PathBuf,
    /// The trusted client certificate (`CN=camel-client`) and key.
    pub client_cert: PathBuf,
    pub client_key: PathBuf,
    /// A client certificate and key from an unrelated CA.
    pub untrusted_cert: PathBuf,
    pub untrusted_key: PathBuf,
}

/// Writes the mutual-TLS test PKI to PEM files.
pub fn pem_files(tag: &str) -> PemFiles {
    let dir = temp_dir(tag);
    let p = super::mtls();
    let w = |name: &str, data: &str| {
        let path = dir.join(name);
        std::fs::write(&path, data).expect("write pem");
        path
    };
    PemFiles {
        ca: w("ca.pem", &p.ca.cert_pem),
        server_cert: w("server.pem", &p.server.cert_pem),
        server_key: w("server.key", &p.server.key_pem),
        client_cert: w("client.pem", &p.client.cert_pem),
        client_key: w("client.key", &p.client.key_pem),
        untrusted_cert: w("untrusted.pem", &p.untrusted_client.cert_pem),
        untrusted_key: w("untrusted.key", &p.untrusted_client.key_pem),
        dir,
    }
}

/// Server TLS parameters from the mutual-TLS PKI without client authentication.
pub fn server_params() -> crate::params::SslContextParameters {
    let p = super::mtls();
    crate::params::SslContextParameters::new().key_pair_pem(&p.server.cert_pem, &p.server.key_pem).trust_pem(&p.ca.cert_pem)
}

/// The result of a client run.
#[derive(Debug)]
pub struct Run {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    /// Panics with the client's output unless it exited with status 0.
    pub fn ok(self) -> Run {
        assert_eq!(self.status, 0, "client failed\n--- stdout ---\n{}\n--- stderr ---\n{}", self.stdout, self.stderr);
        self
    }

    /// The stdout lines that start with `prefix` (prefix removed).
    pub fn values(&self, prefix: &str) -> Vec<String> {
        self.stdout.lines().filter_map(|l| l.strip_prefix(prefix)).map(|s| s.trim().to_string()).collect()
    }

    /// The single stdout value for `prefix`.
    pub fn value(&self, prefix: &str) -> String {
        let v = self.values(prefix);
        assert!(!v.is_empty(), "no `{}` line in output\n{}\n{}", prefix, self.stdout, self.stderr);
        v[0].clone()
    }
}

/// Runs a command (with a timeout), feeding `stdin`, and returns its output.
pub fn run(cmd: &mut Command, stdin: Option<&[u8]>, timeout: Duration) -> Run {
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap_or_else(|e| panic!("cannot start {:?}: {}", cmd, e));
    if let Some(input) = stdin {
        use std::io::Write;
        let mut pipe = child.stdin.take().unwrap();
        let data = input.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&data);
        });
    }
    let out_pipe = child.stdout.take().unwrap();
    let err_pipe = child.stderr.take().unwrap();
    let out_t = std::thread::spawn(move || read_all(out_pipe));
    let err_t = std::thread::spawn(move || read_all(err_pipe));
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().expect("wait") {
            break s.code().unwrap_or(-1);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break -9;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let r = Run { status, stdout: out_t.join().unwrap(), stderr: err_t.join().unwrap() };
    if status == -9 {
        panic!("client timed out after {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}", timeout, r.stdout, r.stderr);
    }
    r
}

fn read_all(mut r: impl std::io::Read) -> String {
    let mut buf = Vec::new();
    let _ = r.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Runs `scripts/official-clients/<name>` with the system Python and `args`.
pub fn python_script(name: &str, args: &[&str], env: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new(python());
    cmd.arg("-u").arg("-I").arg("-W").arg("ignore").arg(script(name)).args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    run(&mut cmd, None, Duration::from_secs(180))
}

/// Builds the Go program in `scripts/official-clients/<dir>` (cached by the Go build cache)
/// and returns the binary path.
pub fn go_build(dir: &str) -> PathBuf {
    let src = script(dir);
    let out = temp_dir("go").join(dir);
    let mut cmd = Command::new(go());
    cmd.current_dir(&src).arg("build").arg("-o").arg(&out).arg(".").env("GOFLAGS", "-mod=mod").env("GO111MODULE", "on").env("GOTOOLCHAIN", "local");
    run(&mut cmd, None, Duration::from_secs(300)).ok();
    out
}

/// `Output` of a finished command as text (convenience for `Command::output`).
pub fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
