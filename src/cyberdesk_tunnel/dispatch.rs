// SPDX-License-Identifier: AGPL-3.0-only
//
// Request dispatcher — routes (method, path) tuples to in-process
// handlers and returns (status, body, content_type).
//
// M5 handles display endpoints with libs/scrap:
//   GET /computer/display/dimensions
//   GET /computer/display/screenshot
// M6 handles the first input slice:
//   M6: POST /computer/input/{mouse,keyboard} (libs/enigo)
// M7 handles the first read-only filesystem slice:
//   GET /computer/fs/list
//   GET /computer/fs/read
// M7 filesystem write:
//   POST /computer/fs/write
// M7 shell/internal basics:
//   POST /computer/shell/powershell/{simple,test,exec,session}
//   GET /internal/diagnostics
//   POST /internal/{shutdown,keepalive/remote/*}
// Future milestones add:
//       POST /internal/update
//
// Unknown paths return 501 + a small JSON error body so
// the caller can clearly tell "your tunnel is alive, this endpoint is
// just not implemented yet" from "the tunnel is broken."

use super::{display, framing::RequestMeta, fs, input, internal, path_without_query, shell};
use serde_json::json;

/// Request provenance marker for routes that must only be reachable through the
/// authenticated Cyberdesk cloud WebSocket. This module is not wired to any
/// localhost HTTP listener; keeping the dispatcher behind this wrapper makes
/// accidental reuse from a local server a compile-time-visible decision.
pub(super) struct ReverseTunnelRequest<'a> {
    meta: &'a RequestMeta,
    body: &'a [u8],
}

impl<'a> ReverseTunnelRequest<'a> {
    pub(super) fn from_websocket_frames(meta: &'a RequestMeta, body: &'a [u8]) -> Self {
        Self { meta, body }
    }
}

/// Route a single reverse-tunnel request to its handler. `body` is the
/// accumulated inbound binary body (may be empty).
pub(super) fn dispatch(request: ReverseTunnelRequest<'_>) -> (u16, Vec<u8>, &'static str) {
    let meta = request.meta;
    let body = request.body;
    let path = path_without_query(&meta.path);
    match (meta.method.as_str(), path) {
        ("GET", "/computer/display/dimensions") => match display::dimensions() {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(500, format!("display dimensions failed: {err:#}")),
        },
        ("GET", "/computer/display/screenshot") => match display::screenshot() {
            Ok(body) => (200, body, "image/png"),
            Err(err) => json_error(500, format!("display screenshot failed: {err:#}")),
        },
        ("GET", "/computer/input/mouse/position") => match input::mouse_position() {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(500, format!("mouse position failed: {err:#}")),
        },
        ("POST", "/computer/input/mouse/move") => match input::mouse_move(body) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("mouse move failed: {err:#}")),
        },
        ("POST", "/computer/input/mouse/click") => match input::mouse_click(body) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("mouse click failed: {err:#}")),
        },
        ("POST", "/computer/input/mouse/scroll") => match input::mouse_scroll(body) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("mouse scroll failed: {err:#}")),
        },
        ("POST", "/computer/input/mouse/drag") => match input::mouse_drag(body) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("mouse drag failed: {err:#}")),
        },
        ("POST", "/computer/input/keyboard/type") => match input::keyboard_type(body) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("keyboard type failed: {err:#}")),
        },
        ("POST", "/computer/input/keyboard/key") => match input::keyboard_key(body) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("keyboard key failed: {err:#}")),
        },
        ("POST", "/computer/copy_to_clipboard") => match input::copy_to_clipboard(body) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(err.status(), format!("copy to clipboard failed: {err:#}")),
        },
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
        ("POST", "/computer/shell/powershell/simple") => match shell::simple() {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(500, format!("powershell simple failed: {err:#}")),
        },
        ("POST", "/computer/shell/powershell/test") => match shell::test() {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(500, format!("powershell test failed: {err:#}")),
        },
        ("POST", "/computer/shell/powershell/exec") => match shell::exec(body) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("powershell exec failed: {err:#}")),
        },
        ("POST", "/computer/shell/powershell/session") => match shell::session(body) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("powershell session failed: {err:#}")),
        },
        ("GET", "/internal/diagnostics") => match internal::diagnostics() {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(500, format!("diagnostics failed: {err:#}")),
        },
        ("POST", "/internal/shutdown") => {
            if !internal::shutdown_enabled() {
                json_error(
                    403,
                    "internal shutdown is disabled on this agent".to_string(),
                )
            } else {
                match internal::shutdown(body) {
                    Ok(body) => (200, body, "application/json"),
                    Err(err) => json_error(500, format!("shutdown failed: {err:#}")),
                }
            }
        }
        ("POST", "/internal/keepalive/remote/activity") => match internal::keepalive_activity() {
            Ok(response) => response,
            Err(err) => json_error(500, format!("keepalive activity failed: {err:#}")),
        },
        ("POST", "/internal/keepalive/remote/enable") => match internal::keepalive_enable() {
            Ok(response) => response,
            Err(err) => json_error(500, format!("keepalive enable failed: {err:#}")),
        },
        ("POST", "/internal/keepalive/remote/disable") => match internal::keepalive_disable() {
            Ok(response) => response,
            Err(err) => json_error(500, format!("keepalive disable failed: {err:#}")),
        },
        _ => (
            501,
            br#"{"error":"not implemented in cyberdesk_tunnel yet"}"#.to_vec(),
            "application/json",
        ),
    }
}

fn json_error(status: u16, message: String) -> (u16, Vec<u8>, &'static str) {
    let body = serde_json::to_vec(&json!({ "error": message }))
        .unwrap_or_else(|_| br#"{"error":"failed to serialize cyberdesk_tunnel error"}"#.to_vec());
    (status, body, "application/json")
}
