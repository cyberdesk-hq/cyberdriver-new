// SPDX-License-Identifier: AGPL-3.0-only
//
// Request dispatcher — routes (method, path) tuples to in-process
// handlers and returns (status, body, content_type).
//
// M5 handles display endpoints with libs/scrap:
//   GET /computer/display/dimensions
//   GET /computer/display/screenshot
// Future milestones add:
//   M6: POST /computer/input/{mouse,keyboard} (libs/enigo)
//       POST /computer/copy_to_clipboard      (libs/clipboard)
//   M7: GET/POST /computer/fs/*               (tokio::fs)
//       POST /computer/shell/powershell/*     (tokio::process)
//       /internal/{shutdown,diagnostics,update,keepalive/*}
//
// Unknown paths return 501 + a small JSON error body so
// the caller can clearly tell "your tunnel is alive, this endpoint is
// just not implemented yet" from "the tunnel is broken."

use super::{display, framing::RequestMeta};
use serde_json::json;

/// Route a single request to its handler. `body` is the accumulated
/// inbound binary body (may be empty).
pub fn dispatch(meta: &RequestMeta, _body: &[u8]) -> (u16, Vec<u8>, &'static str) {
    match (meta.method.as_str(), meta.path.as_str()) {
        ("GET", "/computer/display/dimensions") => match display::dimensions() {
            Ok(body) => (200, body, "application/json"),
            Err(err) => json_error(500, format!("display dimensions failed: {err:#}")),
        },
        ("GET", "/computer/display/screenshot") => match display::screenshot() {
            Ok(body) => (200, body, "image/png"),
            Err(err) => json_error(500, format!("display screenshot failed: {err:#}")),
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
