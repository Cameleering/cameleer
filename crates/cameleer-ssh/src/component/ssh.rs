//! `ssh:` component (Camel `cameleer-ssh`).
//!
//! `ssh://[username[:password]@]host[:port][?options]`
//!
//! * **Producer**: the message body is the command; the reply body is its stdout,
//!   header `CamelSshStderr` its stderr and `CamelSshExitValue` its exit status.
//!   Headers `CamelSshUsername` / `CamelSshPassword` override the credentials for one
//!   exchange. The connection is kept open and reused between exchanges.
//! * **Consumer**: runs `pollCommand` on a schedule (`initialDelay` 1000ms, `delay`
//!   500ms and all scheduler options) and emits one exchange per run.
//!
//! Options: `username`, `password`, `pollCommand`, `timeout` (socket read timeout,
//! 30000ms), `channelType` (`exec` default, or `shell`: the command is written to an
//! interactive shell followed by end of input), `certResource` / `privateKeyFile`
//! (private key path, `file:` prefix allowed), `certResourcePassword` (rejected when the
//! key is encrypted), `knownHostsResource` / `knownHostsFile` (`known_hosts` file),
//! `failOnUnknownHost` / `strictHostKeyChecking` (default false: unknown hosts are
//! accepted, changed keys always rejected), `kex`, `ciphers`, `macs`, `signatures`,
//! `charset` (decoding of stdout/stderr; default UTF-8 with a fallback to bytes),
//! plus the shared options of [the component module](super). `shellPrompt`,
//! `sleepForShellPrompt`, `keyPairProvider`, `keyType`, `clientBuilder` and
//! `compressions` are accepted and ignored.

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::component::scheduled::{ScheduledPollConfig, ScheduledPollConsumer};
use cameleer_core::component::support::process_consumer_exchange;
use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use super::{camel_error, client_config, yes_no, Authority, HostKeyOptions};
use crate::client::{ClientConfig, ExecResult, SshClient};

/// Header: stderr of the command.
pub const SSH_STDERR: &str = "CamelSshStderr";
/// Header: exit status of the command.
pub const SSH_EXIT_VALUE: &str = "CamelSshExitValue";
/// Header: user name overriding the endpoint configuration.
pub const SSH_USERNAME: &str = "CamelSshUsername";
/// Header: password overriding the endpoint configuration.
pub const SSH_PASSWORD: &str = "CamelSshPassword";

/// Configuration of an `ssh` endpoint.
#[derive(Clone)]
pub struct SshConfig {
    pub client: ClientConfig,
    pub poll_command: Option<String>,
    /// `exec` or `shell`.
    pub channel_type: String,
    pub charset: Option<String>,
    pub scheduler: ScheduledPollConfig,
}

const IGNORED: &[&str] = &[
    "lazyStartProducer",
    "bridgeErrorHandler",
    "exceptionHandler",
    "exchangePattern",
    "synchronous",
    "shellPrompt",
    "sleepForShellPrompt",
    "keyPairProvider",
    "keyType",
    "clientBuilder",
    "compressions",
    "certResourcePassword",
];

impl SshConfig {
    /// Parses `[user[:password]@]host[:port]` and the endpoint options.
    pub fn parse(ctx: &CamelContext, remaining: &str, params: &mut Parameters) -> Result<SshConfig> {
        let a = Authority::parse(remaining.trim_end_matches('/'), 22)?;
        let username = params.take("username").or(a.user).unwrap_or_else(|| std::env::var("USER").unwrap_or_else(|_| "root".into()));
        let password = params.take("password").or(a.password);
        if let Some(cert) = params.take("certResource").filter(|s| !s.is_empty()) {
            params.insert("privateKeyFile", cert.trim_start_matches("classpath:").to_string());
        }
        if let Some(pw) = params.take("certResourcePassword").filter(|s| !s.is_empty()) {
            params.insert("privateKeyPassphrase", pw);
        }
        let strict = match (params.take("failOnUnknownHost"), params.take("strictHostKeyChecking")) {
            (Some(v), _) => yes_no(&v)?,
            (None, Some(v)) => yes_no(&v)?,
            (None, None) => false,
        };
        let hk = HostKeyOptions {
            known_hosts_file: params.take("knownHostsResource").or_else(|| params.take("knownHostsFile")).filter(|s| !s.is_empty()),
            known_hosts_text: params.take("knownHosts").filter(|s| !s.is_empty()),
            use_user_known_hosts: params.take_bool("useUserKnownHostsFile", false)?,
            strict,
        };
        let mut client = client_config(ctx, &a.host, a.port, username, password, params, hk)?;
        client.timeout = Some(params.take_duration("timeout", Duration::from_millis(30000))?).filter(|d| !d.is_zero());
        let channel_type = params.take_string("channelType", "exec");
        if channel_type != "exec" && channel_type != "shell" {
            return Err(CamelError::illegal_argument(format!("Unsupported channelType: {} (exec or shell)", channel_type)));
        }
        let scheduler = ScheduledPollConfig::from_params(params, Duration::from_millis(1000), Duration::from_millis(500))?;
        let cfg = SshConfig { client, poll_command: params.take("pollCommand").filter(|s| !s.is_empty()), channel_type, charset: params.take("charset"), scheduler };
        for k in IGNORED {
            params.take(k);
        }
        Ok(cfg)
    }
}

/// The `ssh` component.
#[derive(Default)]
pub struct SshComponent;

impl Component for SshComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let cfg = Arc::new(SshConfig::parse(ctx, remaining, params)?);
        Ok(Arc::new(SshEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An `ssh` endpoint.
pub struct SshEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: Arc<SshConfig>,
}

impl SshEndpoint {
    pub fn config(&self) -> &SshConfig {
        &self.cfg
    }
}

/// Runs commands over a pooled connection.
struct Runner {
    cfg: Arc<SshConfig>,
    conn: Mutex<Option<SshClient>>,
}

fn to_value(data: Vec<u8>, charset: &Option<String>) -> Value {
    match charset.as_deref().map(|c| c.to_ascii_lowercase()) {
        Some(c) if c == "iso-8859-1" || c == "latin1" => Value::String(data.iter().map(|&b| b as char).collect()),
        _ => match String::from_utf8(data) {
            Ok(s) => Value::String(s),
            Err(e) => Value::Bytes(e.into_bytes()),
        },
    }
}

impl Runner {
    fn run_on(client: &mut SshClient, channel_type: &str, command: &str) -> crate::error::SshResult<ExecResult> {
        if channel_type == "shell" {
            let id = client.shell()?;
            let mut input = command.as_bytes().to_vec();
            if !input.ends_with(b"\n") {
                input.push(b'\n');
            }
            client.write(id, &input)?;
            client.send_eof(id)?;
            client.wait_close(id)
        } else {
            client.execute(command, None)
        }
    }

    fn run(&self, command: &str, creds: Option<(String, Option<String>)>) -> Result<ExecResult> {
        let c = &self.cfg;
        let what = format!("Cannot execute command on ssh://{}:{}", c.client.host, c.client.port);
        if let Some((user, password)) = creds {
            // one-off connection with the overriding credentials
            let mut cfg = c.client.clone();
            cfg.username = user;
            if password.is_some() {
                cfg.password = password;
            }
            let mut client = SshClient::connect(&cfg).map_err(|e| camel_error(e, &what))?;
            let r = Self::run_on(&mut client, &c.channel_type, command).map_err(|e| camel_error(e, &what));
            client.disconnect();
            return r;
        }
        let mut guard = self.conn.lock().unwrap();
        for attempt in 0..2 {
            if guard.is_none() {
                *guard = Some(SshClient::connect(&c.client).map_err(|e| camel_error(e, &what))?);
            }
            let client = guard.as_mut().unwrap();
            match Self::run_on(client, &c.channel_type, command) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    let fatal = e.is_fatal();
                    if fatal {
                        if let Some(old) = guard.take() {
                            old.disconnect();
                        }
                    }
                    // a pooled connection may have been closed by the server: retry once
                    if !fatal || attempt == 1 {
                        return Err(camel_error(e, &what));
                    }
                }
            }
        }
        unreachable!("loop returns")
    }

    fn close(&self) {
        if let Some(c) = self.conn.lock().unwrap().take() {
            c.disconnect();
        }
    }

    fn fill(&self, ex: &mut Exchange, r: ExecResult) {
        ex.set_body(to_value(r.stdout, &self.cfg.charset));
        ex.set_header(SSH_STDERR, to_value(r.stderr, &self.cfg.charset));
        ex.set_header(SSH_EXIT_VALUE, Value::Int(r.exit_status.map(|v| v as i64).unwrap_or(-1)));
    }
}

impl Endpoint for SshEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(SshProducer { runner: Runner { cfg: self.cfg.clone(), conn: Mutex::new(None) } }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let command = self
            .cfg
            .poll_command
            .clone()
            .ok_or_else(|| CamelError::illegal_argument(format!("pollCommand must be configured for an ssh consumer: {}", self.uri)))?;
        let runner = Arc::new(Runner { cfg: self.cfg.clone(), conn: Mutex::new(None) });
        let (r2, ctx, logger) = (runner.clone(), self.ctx.clone(), self.uri.clone());
        let poll = ScheduledPollConsumer::new(self.uri.clone(), self.cfg.scheduler.clone(), move || {
            let result = r2.run(&command, None)?;
            let mut ex = Exchange::new(ctx.upgrade());
            r2.fill(&mut ex, result);
            process_consumer_exchange(&logger, processor.as_ref(), &mut ex);
            Ok(1)
        });
        Ok(Arc::new(SshConsumer { runner, poll }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct SshProducer {
    runner: Runner,
}

impl Service for SshProducer {
    fn stop(&self) -> Result<()> {
        self.runner.close();
        Ok(())
    }
}

impl Processor for SshProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let command = match ex.body() {
            Value::Null => return Err(CamelError::illegal_argument("The ssh producer needs a command in the message body")),
            v => v.to_string(),
        };
        let user = ex.header(SSH_USERNAME).filter(|v| !v.is_null()).map(|v| v.to_string());
        let password = ex.header(SSH_PASSWORD).filter(|v| !v.is_null()).map(|v| v.to_string());
        let creds = match (user, password) {
            (None, None) => None,
            (u, p) => Some((u.unwrap_or_else(|| self.runner.cfg.client.username.clone()), p)),
        };
        let r = self.runner.run(&command, creds)?;
        self.runner.fill(ex, r);
        Ok(())
    }
}

/// The scheduled `pollCommand` consumer.
pub struct SshConsumer {
    runner: Arc<Runner>,
    poll: ScheduledPollConsumer,
}

impl SshConsumer {
    /// Runs the poll command once now.
    pub fn poll_once(&self) -> Result<usize> {
        self.poll.poll_once()
    }
}

impl Service for SshConsumer {
    fn start(&self) -> Result<()> {
        self.poll.start()
    }
    fn stop(&self) -> Result<()> {
        self.poll.stop()?;
        self.runner.close();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parsing() {
        let ctx = CamelContext::new();
        let mut p = Parameters::new(
            [("password", "pw"), ("pollCommand", "date"), ("delay", "10"), ("failOnUnknownHost", "true"), ("timeout", "5000"), ("shellPrompt", "$")]
                .into_iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        );
        let c = SshConfig::parse(&ctx, "bob@example.com:2200", &mut p).unwrap();
        assert!(p.is_empty(), "{:?}", p.as_map());
        assert_eq!((c.client.username.as_str(), c.client.port, c.poll_command.as_deref()), ("bob", 2200, Some("date")));
        assert_eq!(c.client.timeout, Some(Duration::from_secs(5)));
        assert!(matches!(c.client.host_key_check, crate::client::HostKeyCheck::KnownHosts { strict: true, .. }));
        let mut p = Parameters::new([("channelType", "sftp".to_string())].into_iter().map(|(a, b)| (a.to_string(), b)).collect());
        assert!(SshConfig::parse(&ctx, "h", &mut p).is_err());
        assert_eq!(to_value(vec![0xff, 0], &None), Value::Bytes(vec![0xff, 0]));
    }
}
