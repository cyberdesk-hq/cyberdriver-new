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
// Future milestones add:
//       POST /computer/fs/write               (tokio::fs)
//       POST /computer/shell/powershell/*     (tokio::process)
//       /internal/{shutdown,diagnostics,update,keepalive/*}
//
// Unknown paths return 501 + a small JSON error body so
// the caller can clearly tell "your tunnel is alive, this endpoint is
// just not implemented yet" from "the tunnel is broken."

use super::{display, framing::RequestMeta, fs, input};
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
        ("GET", "/computer/fs/list") => match fs::list(meta) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("fs list failed: {err:#}")),
        },
        ("GET", "/computer/fs/read") => match fs::read(meta) {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(400, format!("fs read failed: {err:#}")),
        },
        _ => (
            501,
            br#"{"error":"not implemented in cyberdesk_tunnel yet"}"#.to_vec(),
            "application/json",
        ),
    }
}

fn path_without_query(path: &str) -> &str {
    path.split_once('?').map(|(path, _)| path).unwrap_or(path)
}

fn json_error(status: u16, message: String) -> (u16, Vec<u8>, &'static str) {
    let body = serde_json::to_vec(&json!({ "error": message }))
        .unwrap_or_else(|_| br#"{"error":"failed to serialize cyberdesk_tunnel error"}"#.to_vec());
    (status, body, "application/json")
}
