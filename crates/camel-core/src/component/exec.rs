//! `exec:` component - executes a system command (std::process).
//!
//! `exec:executable?args=...&workingDir=...&timeout=...&outFile=...&useStderrOnEmptyStdout=false&exitValues=0,1`
//!
//! The message body (if not null) is written to the process' stdin. The result body is
//! the command's stdout (or the content of `outFile`, or stderr when stdout is empty and
//! `useStderrOnEmptyStdout=true`). Headers: `CamelExecExitValue`, `CamelExecStderr`.
//! The headers `CamelExecCommandExecutable`, `CamelExecCommandArgs` (string or list),
//! `CamelExecCommandWorkingDir`, `CamelExecCommandTimeout`, `CamelExecCommandOutFile` and
//! `CamelExecUseStderrOnEmptyStdout` override the endpoint options.

use std::any::Any;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::context::CamelContext;
use crate::error::{kinds, CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Component, Endpoint, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::Value;

use super::support::split_args;

pub mod headers {
    pub const EXEC_COMMAND_EXECUTABLE: &str = "CamelExecCommandExecutable";
    pub const EXEC_COMMAND_ARGS: &str = "CamelExecCommandArgs";
    pub const EXEC_COMMAND_WORKING_DIR: &str = "CamelExecCommandWorkingDir";
    pub const EXEC_COMMAND_TIMEOUT: &str = "CamelExecCommandTimeout";
    pub const EXEC_COMMAND_OUT_FILE: &str = "CamelExecCommandOutFile";
    pub const EXEC_USE_STDERR_ON_EMPTY_STDOUT: &str = "CamelExecUseStderrOnEmptyStdout";
    pub const EXEC_EXIT_VALUE: &str = "CamelExecExitValue";
    pub const EXEC_STDERR: &str = "CamelExecStderr";
}

#[derive(Default)]
pub struct ExecComponent;

#[derive(Clone, Debug)]
struct ExecConfig {
    executable: String,
    args: Vec<String>,
    working_dir: Option<String>,
    timeout: Option<Duration>,
    out_file: Option<String>,
    use_stderr_on_empty_stdout: bool,
    exit_values: Vec<i64>,
}

impl Component for ExecComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        if remaining.is_empty() {
            return Err(CamelError::illegal_argument("exec: executable is required"));
        }
        let args = params.take("args").map(|a| split_args(&a)).unwrap_or_default();
        let timeout = params.take("timeout").map(|t| crate::util::parse_duration(&t)).transpose()?.filter(|d| !d.is_zero());
        let exit_values = params
            .take("exitValues")
            .map(|s| s.split(',').filter(|x| !x.trim().is_empty()).map(|x| x.trim().parse::<i64>()).collect::<std::result::Result<Vec<_>, _>>())
            .transpose()
            .map_err(|_| CamelError::illegal_argument("exitValues must be a comma separated list of numbers"))?
            .unwrap_or_default();
        let config = ExecConfig {
            executable: remaining.to_string(),
            args,
            working_dir: params.take("workingDir").filter(|s| !s.is_empty()),
            timeout,
            out_file: params.take("outFile").filter(|s| !s.is_empty()),
            use_stderr_on_empty_stdout: params.take_bool("useStderrOnEmptyStdout", false)?,
            exit_values,
        };
        for k in ["binding", "commandExecutor", "commandLogLevel", "lazyStartProducer"] {
            params.take(k);
        }
        Ok(Arc::new(ExecEndpoint { uri: uri.to_string(), config }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct ExecEndpoint {
    uri: String,
    config: ExecConfig,
}

impl Endpoint for ExecEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(ExecProducer { config: self.config.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct ExecProducer {
    config: ExecConfig,
}

impl Service for ExecProducer {}

fn read_all(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        buf
    })
}

impl Processor for ExecProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let mut c = self.config.clone();
        let h = |ex: &Exchange, k: &str| ex.header(k).cloned();
        if let Some(v) = h(ex, headers::EXEC_COMMAND_EXECUTABLE) {
            c.executable = v.to_string();
        }
        if let Some(v) = h(ex, headers::EXEC_COMMAND_ARGS) {
            c.args = match v {
                Value::Array(a) => a.iter().map(|x| x.to_string()).collect(),
                other => split_args(&other.to_string()),
            };
        }
        if let Some(v) = h(ex, headers::EXEC_COMMAND_WORKING_DIR) {
            c.working_dir = Some(v.to_string());
        }
        if let Some(v) = h(ex, headers::EXEC_COMMAND_TIMEOUT) {
            c.timeout = v.as_i64().filter(|t| *t > 0).map(|t| Duration::from_millis(t as u64));
        }
        if let Some(v) = h(ex, headers::EXEC_COMMAND_OUT_FILE) {
            c.out_file = Some(v.to_string());
        }
        if let Some(v) = h(ex, headers::EXEC_USE_STDERR_ON_EMPTY_STDOUT) {
            c.use_stderr_on_empty_stdout = v.is_truthy();
        }
        let mut cmd = Command::new(&c.executable);
        cmd.args(&c.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(d) = &c.working_dir {
            cmd.current_dir(d);
        }
        let mut child = cmd.spawn().map_err(|e| {
            CamelError::new("ExecException", format!("Unable to execute command {} {:?}: {}", c.executable, c.args, e)).with_cause(CamelError::io(e.to_string()))
        })?;
        let input = if ex.body().is_null() { None } else { Some(ex.body().to_bytes()) };
        let stdin = child.stdin.take();
        let writer = std::thread::spawn(move || {
            if let (Some(mut s), Some(data)) = (stdin, input) {
                let _ = s.write_all(&data);
            }
        });
        let out = read_all(child.stdout.take().expect("stdout piped"));
        let err = read_all(child.stderr.take().expect("stderr piped"));
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) => {
                    if let Some(t) = c.timeout {
                        if start.elapsed() >= t {
                            let _ = child.kill();
                            let _ = child.wait();
                            break None;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => return Err(CamelError::io(format!("Error waiting for {}: {}", c.executable, e))),
            }
        };
        let _ = writer.join();
        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
        let Some(status) = status else {
            return Err(CamelError::new(
                "ExecException",
                format!("The command {} timed out after {} millis", c.executable, c.timeout.unwrap_or_default().as_millis()),
            )
            .with_cause(CamelError::new(kinds::TIMEOUT, "timeout")));
        };
        let exit = status.code().unwrap_or(-1) as i64;
        if !c.exit_values.is_empty() && !c.exit_values.contains(&exit) {
            return Err(CamelError::new(
                "ExecException",
                format!("The command {} exited with unexpected exit value {}. Stderr: {}", c.executable, exit, String::from_utf8_lossy(&stderr)),
            ));
        }
        let stderr_s = String::from_utf8_lossy(&stderr).to_string();
        let body = if let Some(f) = &c.out_file {
            let path = match &c.working_dir {
                Some(d) if !std::path::Path::new(f).is_absolute() => std::path::Path::new(d).join(f),
                _ => std::path::PathBuf::from(f),
            };
            std::fs::read(&path).map_err(|e| CamelError::io(format!("Cannot read outFile {}: {}", path.display(), e)))?
        } else if stdout.is_empty() && c.use_stderr_on_empty_stdout {
            stderr.clone()
        } else {
            stdout
        };
        ex.set_body(String::from_utf8_lossy(&body).to_string());
        ex.set_header(headers::EXEC_EXIT_VALUE, Value::from(exit));
        ex.set_header(headers::EXEC_STDERR, stderr_s);
        Ok(())
    }
}
