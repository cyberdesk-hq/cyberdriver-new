// SPDX-License-Identifier: AGPL-3.0-only
//
// Windows user-context helper for Cyberdesk tunnel requests.
//
// The installed Cyberdriver service runs in Session 0, so service-side shell and
// filesystem operations cannot see the logged-in user's profile, HKCU, network
// credentials, or mapped drives. For workflow-facing shell/fs endpoints we keep a
// small helper process alive in the active user session and talk to it over
// restricted named pipes.

use super::{framing::RequestMeta, fs, path_without_query, shell};
use hbb_common::{
    anyhow::{anyhow, Context, Result},
    base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::File,
    io::{Read, Write},
    sync::{mpsc, Mutex},
    thread,
    time::Duration,
};

use winapi::{
    shared::{minwindef::DWORD, ntdef::NULL, winerror::WAIT_TIMEOUT},
    um::{
        handleapi::CloseHandle, processthreadsapi::TerminateProcess, synchapi::WaitForSingleObject,
    },
};

const PIPE_CONNECTION_TIMEOUT_MS: u32 = 10_000;
const MAX_HELPER_MESSAGE_BYTES: usize = 160 * 1024 * 1024;
const HELPER_TIMEOUT_SECONDS: u64 = 30;

static USER_WORKER: Mutex<Option<UserContextWorker>> = Mutex::new(None);

#[derive(Debug, Serialize, Deserialize)]
struct HelperRequest {
    method: String,
    path: String,
    query: Value,
    body_base64: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct HelperResponse {
    launched: bool,
    status: u16,
    content_type: String,
    body_base64: String,
    error: Option<String>,
}

struct UserContextWorker {
    session_id: DWORD,
    process_handle: usize,
    input: File,
    output: File,
}

struct HandleGuard(winapi::shared::ntdef::HANDLE);

impl HandleGuard {
    fn new(handle: winapi::shared::ntdef::HANDLE) -> Self {
        Self(handle)
    }

    fn raw(&self) -> winapi::shared::ntdef::HANDLE {
        self.0
    }
}

impl Drop for HandleGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

struct ProcessHandleGuard {
    handle: winapi::shared::ntdef::HANDLE,
    armed: bool,
}

impl ProcessHandleGuard {
    fn new(handle: winapi::shared::ntdef::HANDLE) -> Self {
        Self {
            handle,
            armed: true,
        }
    }

    fn disarm(mut self) -> winapi::shared::ntdef::HANDLE {
        self.armed = false;
        self.handle
    }
}

impl Drop for ProcessHandleGuard {
    fn drop(&mut self) {
        if self.armed && !self.handle.is_null() {
            unsafe {
                TerminateProcess(self.handle, 1);
                CloseHandle(self.handle);
            }
        }
    }
}

impl Drop for UserContextWorker {
    fn drop(&mut self) {
        let handle = self.process_handle as winapi::shared::ntdef::HANDLE;
        if !handle.is_null() {
            unsafe {
                TerminateProcess(handle, 0);
                CloseHandle(handle);
            }
        }
    }
}

pub(super) fn dispatch(meta: &RequestMeta, body: &[u8]) -> Option<(u16, Vec<u8>, &'static str)> {
    if !supports_user_context(meta) {
        return None;
    }
    match dispatch_user_context(meta, body) {
        Ok(response) => Some(response),
        Err(err) => {
            hbb_common::log::warn!(
                "cyberdesk_tunnel: user-context helper unavailable; falling back to service context: {err:#}"
            );
            None
        }
    }
}

pub(crate) fn run_helper(args: &[String]) -> Result<()> {
    let [input_pipe_name, output_pipe_name] = args else {
        return Err(anyhow!(
            "usage: --cyberdesk-user-helper <input_pipe> <output_pipe>"
        ));
    };
    let mut input = crate::server::terminal_helper::open_pipe(input_pipe_name, true)?;
    let mut output = crate::server::terminal_helper::open_pipe(output_pipe_name, false)?;
    loop {
        let request_bytes = match read_message(&mut input) {
            Ok(bytes) => bytes,
            Err(err) if is_pipe_closed_error(&err) => break,
            Err(err) => return Err(err),
        };
        let response = match run_helper_inner(&request_bytes) {
            Ok(response) => response,
            Err(err) => HelperResponse {
                launched: true,
                status: 500,
                content_type: "application/json".to_string(),
                body_base64: BASE64_STANDARD.encode(
                    serde_json::json!({
                        "error": format!("user-context helper failed: {err:#}"),
                        "execution_context": "user",
                    })
                    .to_string(),
                ),
                error: Some(format!("{err:#}")),
            },
        };
        write_message(&mut output, &serde_json::to_vec(&response)?)
            .context("writing helper response")?;
    }
    Ok(())
}

fn dispatch_user_context(meta: &RequestMeta, body: &[u8]) -> Result<(u16, Vec<u8>, &'static str)> {
    let session_id = active_user_session_id()?;
    let request = HelperRequest {
        method: meta.method.clone(),
        path: meta.path.clone(),
        query: meta.query.clone(),
        body_base64: BASE64_STANDARD.encode(body),
    };
    let request_bytes = serde_json::to_vec(&request)?;
    let timeout = helper_timeout(meta, body);

    let response_bytes = {
        let mut worker_guard = USER_WORKER
            .lock()
            .map_err(|_| anyhow!("user-context worker lock poisoned"))?;
        ensure_worker(&mut worker_guard, session_id)?;
        let worker = worker_guard
            .as_mut()
            .ok_or_else(|| anyhow!("user-context worker unavailable after launch"))?;

        match send_worker_request(&mut worker.input, &mut worker.output, &request_bytes, timeout)
        {
            Ok(response) => response,
            Err(err) => {
                *worker_guard = None;
                return Ok((
                    500,
                    with_execution_context(
                        serde_json::json!({"error": format!("user-context worker failed: {err:#}")})
                            .to_string()
                            .into_bytes(),
                        "user",
                        Some("user-context worker failed after request dispatch"),
                    ),
                    "application/json",
                ));
            }
        }
    };
    let response: HelperResponse =
        serde_json::from_slice(&response_bytes).context("decoding user-context helper response")?;
    let body = BASE64_STANDARD
        .decode(response.body_base64.as_bytes())
        .context("decoding user-context helper body")?;
    let body = with_execution_context(body, "user", response.error.as_deref());
    Ok((
        response.status,
        body,
        content_type_static(&response.content_type),
    ))
}

fn ensure_worker(worker: &mut Option<UserContextWorker>, session_id: DWORD) -> Result<()> {
    if worker
        .as_ref()
        .map(|worker| worker.session_id == session_id && process_is_running(worker.process_handle))
        .unwrap_or(false)
    {
        return Ok(());
    }
    *worker = None;

    let user_token = HandleGuard::new(user_token_for_session(session_id)?);
    let user_token_wrapper =
        crate::server::terminal_helper::UserToken::new(user_token.raw() as usize);
    let pipe_id = uuid::Uuid::new_v4();
    let input_pipe_name = format!(r"\\.\pipe\cyberdesk_user_in_{}", pipe_id);
    let output_pipe_name = format!(r"\\.\pipe\cyberdesk_user_out_{}", pipe_id);
    let input_pipe_handle = crate::server::terminal_helper::OwnedHandle::new(
        crate::server::terminal_helper::create_named_pipe_server(
            &input_pipe_name,
            false,
            user_token_wrapper,
        )?,
    );
    let output_pipe_handle = crate::server::terminal_helper::OwnedHandle::new(
        crate::server::terminal_helper::create_named_pipe_server(
            &output_pipe_name,
            true,
            user_token_wrapper,
        )?,
    );
    let cmd = format!(
        "\"{}\" --cyberdesk-user-helper {} {}",
        std::env::current_exe()?.display(),
        quote_arg(&input_pipe_name),
        quote_arg(&output_pipe_name)
    );
    let handle = crate::platform::windows::launch_user_process_in_session(session_id, &cmd, false)
        .context("launching persistent user-context worker")?;
    let process_guard = ProcessHandleGuard::new(handle);
    let input = crate::server::terminal_helper::wait_for_pipe_connection(
        input_pipe_handle,
        &input_pipe_name,
        PIPE_CONNECTION_TIMEOUT_MS,
    )?;
    let output = crate::server::terminal_helper::wait_for_pipe_connection(
        output_pipe_handle,
        &output_pipe_name,
        PIPE_CONNECTION_TIMEOUT_MS,
    )?;
    let handle = process_guard.disarm();

    *worker = Some(UserContextWorker {
        session_id,
        process_handle: handle as usize,
        input,
        output,
    });
    Ok(())
}

fn send_worker_request(
    input: &mut File,
    output: &mut File,
    request: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>> {
    let mut output_clone = output
        .try_clone()
        .context("cloning user-context worker output pipe for read")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(read_message(&mut output_clone));
    });
    write_message(input, request).context("sending user-context worker request")?;
    rx.recv_timeout(timeout)
        .context("timed out waiting for user-context worker response")?
        .context("reading user-context worker response")
}

fn run_helper_inner(request_bytes: &[u8]) -> Result<HelperResponse> {
    let request: HelperRequest =
        serde_json::from_slice(request_bytes).context("decoding helper request")?;
    let body = BASE64_STANDARD
        .decode(request.body_base64.as_bytes())
        .context("decoding helper request body")?;
    let meta = RequestMeta {
        method: request.method,
        path: request.path,
        headers: Value::Object(Default::default()),
        query: request.query,
        request_id: "cyberdesk-user-helper".to_string(),
    };
    let (status, body, content_type) = dispatch_supported(&meta, &body);
    Ok(HelperResponse {
        launched: true,
        status,
        content_type: content_type.to_string(),
        body_base64: BASE64_STANDARD.encode(body),
        error: None,
    })
}

fn dispatch_supported(meta: &RequestMeta, body: &[u8]) -> (u16, Vec<u8>, &'static str) {
    let path = path_without_query(&meta.path);
    match (meta.method.as_str(), path) {
        ("GET", "/computer/fs/list" | "computer/fs/list") => match fs::list(meta) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("fs list failed: {err:#}")),
        },
        ("GET", "/computer/fs/read" | "computer/fs/read") => match fs::read(meta) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("fs read failed: {err:#}")),
        },
        ("POST", "/computer/fs/write" | "computer/fs/write") => match fs::write(body) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("fs write failed: {err:#}")),
        },
        ("POST", "/computer/shell/powershell/exec") => match shell::exec(body) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("powershell exec failed: {err:#}")),
        },
        _ => json_error(501, "unsupported user-context helper route".to_string()),
    }
}

fn supports_user_context(meta: &RequestMeta) -> bool {
    let path = path_without_query(&meta.path);
    matches!(
        (meta.method.as_str(), path),
        ("GET", "/computer/fs/list" | "computer/fs/list")
            | ("GET", "/computer/fs/read" | "computer/fs/read")
            | ("POST", "/computer/fs/write" | "computer/fs/write")
            | ("POST", "/computer/shell/powershell/exec")
    )
}

fn active_user_session_id() -> Result<DWORD> {
    let session_id = crate::platform::windows::get_current_session_id(true);
    if session_id == u32::MAX {
        return Err(anyhow!("no active Windows user session"));
    }
    let _token = HandleGuard::new(user_token_for_session(session_id)?);
    Ok(session_id)
}

fn user_token_for_session(session_id: DWORD) -> Result<winapi::shared::ntdef::HANDLE> {
    let token = crate::platform::windows::get_user_token(session_id, true);
    if token == NULL {
        return Err(anyhow!(
            "failed to obtain user token for Windows session {session_id}"
        ));
    }
    Ok(token)
}

fn quote_arg(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn helper_timeout(meta: &RequestMeta, body: &[u8]) -> Duration {
    let route = path_without_query(&meta.path);
    let seconds = if route.ends_with("/powershell/exec") {
        serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|value| value.get("timeout").and_then(Value::as_f64))
            .map(|timeout| timeout.clamp(0.1, 180.0).ceil() as u64 + 15)
            .unwrap_or(HELPER_TIMEOUT_SECONDS)
    } else {
        HELPER_TIMEOUT_SECONDS
    };
    Duration::from_secs(seconds)
}

fn write_message(writer: &mut File, payload: &[u8]) -> Result<()> {
    if payload.len() > MAX_HELPER_MESSAGE_BYTES {
        return Err(anyhow!("helper message exceeds maximum size"));
    }
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

fn read_message(reader: &mut File) -> Result<Vec<u8>> {
    let mut len_bytes = [0_u8; 4];
    reader.read_exact(&mut len_bytes)?;
    let len = u32::from_le_bytes(len_bytes) as usize;
    if len > MAX_HELPER_MESSAGE_BYTES {
        return Err(anyhow!("helper message exceeds maximum size"));
    }
    let mut payload = vec![0_u8; len];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

fn is_pipe_closed_error(err: &hbb_common::anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>()
        .map(|err| {
            matches!(
                err.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
            )
        })
        .unwrap_or(false)
}

fn process_is_running(process_handle: usize) -> bool {
    let handle = process_handle as winapi::shared::ntdef::HANDLE;
    !handle.is_null() && unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT
}

fn json_error(status: u16, message: String) -> (u16, Vec<u8>, &'static str) {
    let body = serde_json::to_vec(&serde_json::json!({ "error": message }))
        .unwrap_or_else(|_| br#"{"error":"failed to serialize user-context error"}"#.to_vec());
    (status, body, "application/json")
}

fn content_type_static(content_type: &str) -> &'static str {
    match content_type {
        "image/png" => "image/png",
        _ => "application/json",
    }
}

fn with_execution_context(
    mut body: Vec<u8>,
    context: &str,
    fallback_reason: Option<&str>,
) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(map) = value.as_object_mut() else {
        return body;
    };
    map.insert(
        "execution_context".to_string(),
        serde_json::Value::String(context.to_string()),
    );
    if let Some(reason) = fallback_reason {
        map.insert(
            "execution_context_note".to_string(),
            serde_json::Value::String(reason.to_string()),
        );
    }
    if let Ok(updated) = serde_json::to_vec(&value) {
        body = updated;
    }
    body
}
