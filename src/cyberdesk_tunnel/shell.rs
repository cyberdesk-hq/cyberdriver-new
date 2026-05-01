// SPDX-License-Identifier: AGPL-3.0-only
//
// Shell endpoints for Cyberdesk's HTTP-over-WS tunnel.
//
// This mirrors Cyberdriver 1.x's current PowerShell contract: commands
// are stateless and run as separate subprocesses. The `session_id` is
// returned for API compatibility, but no persistent shell actor is kept
// yet; the stateful actor remains a later M7 item.

use super::parse_json;
use hbb_common::anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(not(windows))]
use std::sync::OnceLock;
use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_COMMAND_CHARS: usize = 32 * 1024;
const MAX_OUTPUT_CHARS: usize = 64 * 1024;
const MAX_TIMEOUT_SECONDS: f64 = 180.0;

#[derive(Debug, Deserialize)]
struct PowerShellExecRequest {
    command: String,
    #[serde(default = "default_true")]
    same_session: bool,
    working_directory: Option<String>,
    session_id: Option<String>,
    timeout: Option<f64>,
}

#[derive(Debug, Serialize)]
struct PowerShellExecResponse {
    stdout: String,
    stderr: String,
    exit_code: i32,
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_reached: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

struct CappedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

pub fn simple() -> Result<Vec<u8>> {
    let result = run_command("Write-Output 'Hello World'", None, Some(5.0), None)?;
    Ok(serde_json::to_vec(&json!({
        "returncode": result.exit_code,
        "stdout": result.stdout,
        "stderr": result.stderr,
    }))?)
}

pub fn test() -> Result<Vec<u8>> {
    let result = run_command(
        "Write-Output 'Hello from PowerShell'",
        None,
        Some(5.0),
        None,
    )?;
    Ok(serde_json::to_vec(&json!({
        "test": "complete",
        "output": if result.stdout.is_empty() { Vec::<String>::new() } else { result.stdout.lines().map(|s| s.to_string()).collect::<Vec<_>>() },
        "stderr": result.stderr,
        "exit_code": result.exit_code,
    }))?)
}

pub fn exec(body: &[u8]) -> Result<Vec<u8>> {
    let request: PowerShellExecRequest = parse_json(body)?;
    if request.command.trim().is_empty() {
        bail!("missing 'command' field");
    }
    if request.command.len() > MAX_COMMAND_CHARS {
        bail!("command exceeds {MAX_COMMAND_CHARS} character limit");
    }

    let timeout = request
        .timeout
        .unwrap_or(30.0)
        .clamp(0.1, MAX_TIMEOUT_SECONDS);
    let session_id = request
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let result = run_command(
        &request.command,
        request.working_directory.as_deref(),
        Some(timeout),
        Some(session_id),
    )?;

    // `same_session` is accepted for compatibility with Cyberdriver 1.x. Sessions are
    // intentionally stateless in this slice.
    let _ = request.same_session;

    Ok(serde_json::to_vec(&result)?)
}

pub fn session(body: &[u8]) -> Result<Vec<u8>> {
    #[derive(Debug, Deserialize)]
    struct SessionRequest {
        action: String,
        session_id: Option<String>,
    }

    let request: SessionRequest = parse_json(body)?;
    match request.action.as_str() {
        "create" => Ok(serde_json::to_vec(&json!({
            "session_id": uuid::Uuid::new_v4().to_string(),
            "message": "Session ID generated (sessions are stateless)"
        }))?),
        "destroy" => Ok(serde_json::to_vec(&json!({
            "message": "Session destroyed (no-op in stateless mode)",
            "session_id": request.session_id
        }))?),
        _ => bail!("invalid action. Must be 'create' or 'destroy'"),
    }
}

fn run_command(
    command: &str,
    working_directory: Option<&str>,
    timeout_seconds: Option<f64>,
    session_id: Option<String>,
) -> Result<PowerShellExecResponse> {
    let executable = powershell_executable();
    let mut child = Command::new(executable)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-OutputFormat",
            "Text",
            "-Command",
            command,
        ])
        .current_dir(resolve_working_directory(working_directory)?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn {executable}"))?;

    let stdout_reader = child
        .stdout
        .take()
        .map(spawn_output_reader)
        .context("PowerShell stdout pipe was not captured")?;
    let stderr_reader = child
        .stderr
        .take()
        .map(spawn_output_reader)
        .context("PowerShell stderr pipe was not captured")?;

    let timeout = Duration::from_secs_f64(
        timeout_seconds
            .unwrap_or(30.0)
            .clamp(0.1, MAX_TIMEOUT_SECONDS),
    );
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().context("failed waiting for PowerShell")? {
            let stdout = collect_output(stdout_reader);
            let stderr = collect_output(stderr_reader);
            return Ok(PowerShellExecResponse {
                stdout,
                stderr,
                exit_code: status.code().unwrap_or(-1),
                session_id: session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                timeout_reached: None,
                error: None,
            });
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let wait_error = child
                .wait()
                .err()
                .map(|err| format!("failed to wait for timed-out PowerShell process: {err}"));
            let stdout = collect_output(stdout_reader);
            let stderr = collect_output(stderr_reader);
            let timeout_message = wait_error.unwrap_or_else(|| {
                format!(
                    "Command timeout reached after {:.1} seconds. Process was terminated.",
                    timeout.as_secs_f64()
                )
            });
            let stderr = if stderr.is_empty() {
                timeout_message
            } else {
                format!("{stderr}\n{timeout_message}")
            };
            return Ok(PowerShellExecResponse {
                stdout,
                stderr,
                exit_code: 124,
                session_id: session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                timeout_reached: Some(true),
                error: None,
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn spawn_output_reader<R>(mut reader: R) -> JoinHandle<CappedOutput>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = CappedOutput {
            bytes: Vec::with_capacity(MAX_OUTPUT_CHARS.min(8 * 1024)),
            truncated: false,
        };
        let mut buffer = [0_u8; 8 * 1024];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let remaining = MAX_OUTPUT_CHARS.saturating_sub(output.bytes.len());
                    if remaining > 0 {
                        output
                            .bytes
                            .extend_from_slice(&buffer[..read.min(remaining)]);
                    }
                    if read > remaining {
                        output.truncated = true;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    output.truncated = true;
                    break;
                }
            }
        }

        output
    })
}

fn collect_output(reader: JoinHandle<CappedOutput>) -> String {
    match reader.join() {
        Ok(output) => capped_output_to_string(output),
        Err(_) => "failed to collect PowerShell output: reader thread panicked".to_string(),
    }
}

fn capped_output_to_string(output: CappedOutput) -> String {
    let mut text = truncate_output(String::from_utf8_lossy(&output.bytes).trim().to_string());
    if output.truncated && !text.ends_with("...<truncated>") {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("...<truncated>");
    }
    text
}

fn powershell_executable() -> &'static str {
    #[cfg(windows)]
    {
        "powershell"
    }
    #[cfg(not(windows))]
    {
        static POWERSHELL_EXECUTABLE: OnceLock<&'static str> = OnceLock::new();
        *POWERSHELL_EXECUTABLE.get_or_init(|| {
            if Command::new("pwsh").arg("-Version").output().is_ok() {
                "pwsh"
            } else {
                "powershell"
            }
        })
    }
}

fn resolve_working_directory(working_directory: Option<&str>) -> Result<PathBuf> {
    match working_directory {
        Some(dir) if !dir.trim().is_empty() => {
            let path = PathBuf::from(dir);
            Ok(path.canonicalize().unwrap_or(path))
        }
        _ => std::env::current_dir().context("failed to get current directory"),
    }
}

fn truncate_output(mut output: String) -> String {
    if output.len() > MAX_OUTPUT_CHARS {
        let mut truncate_at = MAX_OUTPUT_CHARS;
        while !output.is_char_boundary(truncate_at) {
            truncate_at -= 1;
        }
        output.truncate(truncate_at);
        output.push_str("\n...<truncated>");
    }
    output
}

fn default_true() -> bool {
    true
}
