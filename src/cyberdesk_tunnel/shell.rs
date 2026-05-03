// SPDX-License-Identifier: AGPL-3.0-only
//
// Shell endpoints for Cyberdesk's HTTP-over-WS tunnel.
//
// This mirrors Cyberdriver 1.x's current PowerShell contract: callers can run
// one-off commands or keep a stateful PowerShell process by reusing session_id.

use super::parse_json;
use hbb_common::anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::{
    collections::HashMap,
    fmt,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError},
        Arc, Mutex, MutexGuard, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

const MAX_COMMAND_CHARS: usize = 32 * 1024;
const MAX_OUTPUT_CHARS: usize = 64 * 1024;
const MAX_TIMEOUT_SECONDS: f64 = 180.0;
const MAX_SESSIONS: usize = 16;
const TRUNCATED_MARKER: &str = "\n...<truncated>";

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

struct OutputReader {
    output: Arc<Mutex<CappedOutput>>,
    done: Receiver<()>,
}

struct PowerShellSession {
    child: Child,
    stdin: ChildStdin,
    stdout: Receiver<Vec<u8>>,
    stderr: Receiver<Vec<u8>>,
}

#[derive(Debug)]
struct SessionCommandError {
    message: String,
    timed_out: bool,
}

impl SessionCommandError {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            timed_out: false,
        }
    }

    fn timed_out(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            timed_out: true,
        }
    }
}

impl fmt::Display for SessionCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

static POWERSHELL_SESSIONS: OnceLock<Mutex<HashMap<String, Arc<Mutex<PowerShellSession>>>>> =
    OnceLock::new();

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
    let use_persistent_session = request.same_session && request.session_id.is_some();
    let session_id = session_id_or_new(request.session_id)?;
    let result = if use_persistent_session {
        run_session_command(
            &request.command,
            request.working_directory.as_deref(),
            timeout,
            session_id,
        )?
    } else {
        run_command(
            &request.command,
            request.working_directory.as_deref(),
            Some(timeout),
            Some(session_id),
        )?
    };

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
        "create" => {
            let session_id = create_session(request.session_id)?;
            Ok(serde_json::to_vec(&json!({
                "session_id": session_id,
                "message": "Session created"
            }))?)
        }
        "destroy" => {
            let session_id = require_session_id(request.session_id)?;
            let removed = destroy_session(&session_id);
            Ok(serde_json::to_vec(&json!({
                "message": if removed { "Session destroyed" } else { "Session not found" },
                "session_id": session_id
            }))?)
        }
        _ => bail!("invalid action. Must be 'create' or 'destroy'"),
    }
}

fn run_session_command(
    command: &str,
    working_directory: Option<&str>,
    timeout_seconds: f64,
    session_id: String,
) -> Result<PowerShellExecResponse> {
    let session = {
        let mut guard = lock_sessions();
        if let Some(session) = guard.get(&session_id) {
            session.clone()
        } else {
            if guard.len() >= MAX_SESSIONS {
                bail!("too many active PowerShell sessions");
            }
            let session = Arc::new(Mutex::new(spawn_session()?));
            guard.insert(session_id.clone(), session.clone());
            session
        }
    };

    let resolved_working_directory = resolve_session_working_directory(working_directory)?;
    let marker = format!("__CYBERDRIVER_END_{}__", uuid::Uuid::new_v4().simple());
    let wrapped = wrap_session_command(command, resolved_working_directory.as_deref(), &marker);
    let timeout = Duration::from_secs_f64(timeout_seconds.clamp(0.1, MAX_TIMEOUT_SECONDS));
    let session_status = {
        let mut session = lock_session(&session);
        session
            .child
            .try_wait()
            .context("failed checking PowerShell session")?
    };
    if let Some(status) = session_status {
        remove_and_terminate_session(&session_id, &session);
        bail!("PowerShell session exited before command (status={status})");
    }

    let command_result = (|| -> std::result::Result<(String, String, i32), SessionCommandError> {
        let mut session = lock_session(&session);
        discard_chunks(&session.stdout);
        session.stdin.write_all(wrapped.as_bytes()).map_err(|err| {
            SessionCommandError::failed(format!(
                "failed to write command to PowerShell session: {err}"
            ))
        })?;
        session.stdin.flush().map_err(|err| {
            SessionCommandError::failed(format!("failed to flush PowerShell session stdin: {err}"))
        })?;
        collect_session_command(&mut session, &marker, timeout)
    })();

    match command_result {
        Ok((stdout, stderr, exit_code)) => Ok(PowerShellExecResponse {
            stdout,
            stderr,
            exit_code,
            session_id,
            timeout_reached: None,
            error: None,
        }),
        Err(err) => {
            remove_and_terminate_session(&session_id, &session);
            let exit_code = if err.timed_out { 124 } else { -1 };
            let timeout_reached = Some(err.timed_out);
            let error = if err.timed_out {
                "PowerShell session command timed out; session was destroyed"
            } else {
                "PowerShell session command failed; session was destroyed"
            };
            Ok(PowerShellExecResponse {
                stdout: String::new(),
                stderr: err.to_string(),
                exit_code,
                session_id,
                timeout_reached,
                error: Some(error.to_string()),
            })
        }
    }
}

fn create_session(requested_session_id: Option<String>) -> Result<String> {
    let session_id = session_id_or_new(requested_session_id)?;
    let mut guard = lock_sessions();
    if guard.contains_key(&session_id) {
        return Ok(session_id);
    }
    if guard.len() >= MAX_SESSIONS {
        bail!("too many active PowerShell sessions");
    }
    guard.insert(session_id.clone(), Arc::new(Mutex::new(spawn_session()?)));
    Ok(session_id)
}

fn session_id_or_new(session_id: Option<String>) -> Result<String> {
    match session_id {
        Some(value) => normalize_session_id(&value),
        None => Ok(uuid::Uuid::new_v4().to_string()),
    }
}

fn require_session_id(session_id: Option<String>) -> Result<String> {
    let value = session_id.context("missing required 'session_id'")?;
    normalize_session_id(&value)
}

fn normalize_session_id(session_id: &str) -> Result<String> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        bail!("session_id must not be empty");
    }
    let parsed = uuid::Uuid::parse_str(session_id).context("session_id must be a UUID")?;
    Ok(parsed.to_string())
}

fn destroy_session(session_id: &str) -> bool {
    let session = {
        let mut guard = lock_sessions();
        guard.remove(session_id)
    };
    if let Some(session) = session {
        let mut session = lock_session(&session);
        terminate_process_tree(&mut session.child);
        true
    } else {
        false
    }
}

fn spawn_session() -> Result<PowerShellSession> {
    let executable = powershell_executable();
    let mut powershell = Command::new(executable);
    powershell
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-NoExit",
            "-Command",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        powershell.process_group(0);
    }
    let mut child = powershell
        .spawn()
        .with_context(|| format!("failed to spawn {executable} session"))?;
    let stdin = child
        .stdin
        .take()
        .context("PowerShell session stdin pipe was not captured")?;
    let stdout = child
        .stdout
        .take()
        .map(spawn_chunk_reader)
        .context("PowerShell session stdout pipe was not captured")?;
    let stderr = child
        .stderr
        .take()
        .map(spawn_chunk_reader)
        .context("PowerShell session stderr pipe was not captured")?;
    Ok(PowerShellSession {
        child,
        stdin,
        stdout,
        stderr,
    })
}

fn resolve_session_working_directory(working_directory: Option<&str>) -> Result<Option<PathBuf>> {
    match working_directory {
        Some(dir) if !dir.trim().is_empty() => Ok(Some(resolve_working_directory(Some(dir))?)),
        _ => Ok(None),
    }
}

fn wrap_session_command(command: &str, working_directory: Option<&Path>, marker: &str) -> String {
    let mut wrapped = String::new();
    if let Some(dir) = working_directory {
        wrapped.push_str("Set-Location -LiteralPath ");
        wrapped.push_str(&powershell_single_quote(&dir.display().to_string()));
        wrapped.push_str("\r\n");
    }
    wrapped.push_str(command);
    wrapped.push_str("\r\n");
    wrapped.push_str("$__cyberdriver_exit = if ($global:LASTEXITCODE -is [int]) { $global:LASTEXITCODE } else { 0 }\r\n");
    wrapped.push_str("Write-Output (");
    wrapped.push_str(&powershell_single_quote(marker));
    wrapped.push_str(" + [string]$__cyberdriver_exit)\r\n");
    wrapped
}

fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn collect_session_command(
    session: &mut PowerShellSession,
    marker: &str,
    timeout: Duration,
) -> std::result::Result<(String, String, i32), SessionCommandError> {
    let start = Instant::now();
    let mut stdout = String::new();
    let mut stderr = String::new();
    loop {
        drain_chunks(&session.stderr, &mut stderr);
        if let Some((clean_stdout, exit_code)) = take_marked_stdout(&stdout, marker) {
            discard_chunks(&session.stdout);
            drain_chunks(&session.stderr, &mut stderr);
            return Ok((
                truncate_output(clean_stdout),
                truncate_output(stderr),
                exit_code,
            ));
        }
        let remaining = timeout.checked_sub(start.elapsed()).ok_or_else(|| {
            SessionCommandError::timed_out("PowerShell session command timed out")
        })?;
        match session
            .stdout
            .recv_timeout(remaining.min(Duration::from_millis(100)))
        {
            Ok(chunk) => {
                stdout.push_str(&String::from_utf8_lossy(&chunk));
                cap_session_stdout(&mut stdout, marker);
            }
            Err(RecvTimeoutError::Timeout) => {
                if start.elapsed() >= timeout {
                    return Err(SessionCommandError::timed_out(
                        "PowerShell session command timed out",
                    ));
                }
                if let Some(status) = session.child.try_wait().map_err(|err| {
                    SessionCommandError::failed(format!(
                        "failed waiting for PowerShell session: {err}"
                    ))
                })? {
                    return Err(SessionCommandError::failed(format!(
                        "PowerShell session exited before command marker (status={status})"
                    )));
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(SessionCommandError::failed(
                    "PowerShell session stdout closed before command marker",
                ));
            }
        }
    }
}

fn cap_session_stdout(output: &mut String, marker: &str) {
    if output.len() <= MAX_OUTPUT_CHARS * 2 {
        return;
    }

    let tail_len = marker.len().saturating_add(32);
    let mut tail_start = output.len().saturating_sub(tail_len);
    while !output.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let tail = output[tail_start..].to_string();
    truncate_at_char_boundary(output, MAX_OUTPUT_CHARS);
    output.push_str(TRUNCATED_MARKER);
    output.push_str(&tail);
}

fn sessions() -> &'static Mutex<HashMap<String, Arc<Mutex<PowerShellSession>>>> {
    POWERSHELL_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_sessions() -> MutexGuard<'static, HashMap<String, Arc<Mutex<PowerShellSession>>>> {
    match sessions().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn lock_session(session: &Arc<Mutex<PowerShellSession>>) -> MutexGuard<'_, PowerShellSession> {
    match session.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn remove_and_terminate_session(session_id: &str, session: &Arc<Mutex<PowerShellSession>>) {
    let removed = {
        let mut guard = lock_sessions();
        match guard.get(session_id) {
            Some(current) if Arc::ptr_eq(current, session) => guard.remove(session_id),
            _ => None,
        }
    };
    if let Some(session) = removed {
        let mut session = lock_session(&session);
        terminate_process_tree(&mut session.child);
    }
}

fn spawn_chunk_reader<R>(mut reader: R) -> Receiver<Vec<u8>>
where
    R: Read + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if tx.send(buffer[..read].to_vec()).is_err() {
                        break;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
    rx
}

fn drain_chunks(rx: &Receiver<Vec<u8>>, output: &mut String) {
    while let Ok(chunk) = rx.try_recv() {
        if output.ends_with(TRUNCATED_MARKER) {
            continue;
        }
        output.push_str(&String::from_utf8_lossy(&chunk));
        if output.len() > MAX_OUTPUT_CHARS * 2 {
            truncate_at_char_boundary(output, MAX_OUTPUT_CHARS * 2);
            output.push_str(TRUNCATED_MARKER);
            break;
        }
    }
}

fn discard_chunks(rx: &Receiver<Vec<u8>>) {
    while rx.try_recv().is_ok() {}
}

fn take_marked_stdout(stdout: &str, marker: &str) -> Option<(String, i32)> {
    let marker_pos = stdout.find(marker)?;
    let before = stdout[..marker_pos].trim_end().to_string();
    let after = &stdout[marker_pos + marker.len()..];
    let after = after.trim_start();
    let exit_len = after
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
        .map(char::len_utf8)
        .sum();
    if exit_len == 0 {
        return None;
    }
    if !after[exit_len..].contains('\n') {
        return None;
    }
    let exit_code = after[..exit_len].parse::<i32>().unwrap_or(-1);
    Some((before, exit_code))
}

fn run_command(
    command: &str,
    working_directory: Option<&str>,
    timeout_seconds: Option<f64>,
    session_id: Option<String>,
) -> Result<PowerShellExecResponse> {
    let executable = powershell_executable();
    let mut powershell = Command::new(executable);
    powershell
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
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        powershell.process_group(0);
    }
    let mut child = powershell
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
            terminate_process_tree(&mut child);
            let wait_error = child
                .wait()
                .err()
                .map(|err| format!("failed to wait for timed-out PowerShell process: {err}"));
            let stdout = collect_output_after_timeout(stdout_reader);
            let stderr = collect_output_after_timeout(stderr_reader);
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

fn spawn_output_reader<R>(mut reader: R) -> OutputReader
where
    R: Read + Send + 'static,
{
    let output = Arc::new(Mutex::new(CappedOutput {
        bytes: Vec::with_capacity(MAX_OUTPUT_CHARS.min(8 * 1024)),
        truncated: false,
    }));
    let thread_output = output.clone();
    let (done_tx, done_rx) = mpsc::channel();

    thread::spawn(move || {
        let mut buffer = [0_u8; 8 * 1024];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let mut output = lock_output(&thread_output);
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
                    lock_output(&thread_output).truncated = true;
                    break;
                }
            }
        }

        let _ = done_tx.send(());
    });

    OutputReader {
        output,
        done: done_rx,
    }
}

fn collect_output(reader: OutputReader) -> String {
    match reader.done.recv() {
        Ok(()) => capped_output_to_string(snapshot_output(&reader.output, false)),
        Err(_) => "failed to collect PowerShell output: reader thread panicked".to_string(),
    }
}

fn collect_output_after_timeout(reader: OutputReader) -> String {
    match reader.done.recv_timeout(Duration::from_millis(100)) {
        Ok(()) => capped_output_to_string(snapshot_output(&reader.output, false)),
        Err(RecvTimeoutError::Timeout) => {
            capped_output_to_string(snapshot_output(&reader.output, true))
        }
        Err(RecvTimeoutError::Disconnected) => {
            "failed to collect PowerShell output: reader thread panicked".to_string()
        }
    }
}

fn snapshot_output(output: &Arc<Mutex<CappedOutput>>, mark_truncated: bool) -> CappedOutput {
    let output = lock_output(output);
    CappedOutput {
        bytes: output.bytes.clone(),
        truncated: output.truncated || (mark_truncated && !output.bytes.is_empty()),
    }
}

fn lock_output(output: &Arc<Mutex<CappedOutput>>) -> MutexGuard<'_, CappedOutput> {
    match output.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn terminate_process_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{}", child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
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
        truncate_at_char_boundary(&mut output, MAX_OUTPUT_CHARS);
        output.push_str(TRUNCATED_MARKER);
    }
    output
}

fn truncate_at_char_boundary(output: &mut String, max_len: usize) {
    let mut truncate_at = max_len;
    while !output.is_char_boundary(truncate_at) {
        truncate_at -= 1;
    }
    output.truncate(truncate_at);
}

fn default_true() -> bool {
    true
}
