// SPDX-License-Identifier: AGPL-3.0-only
//
// Request dispatcher — routes (method, path) tuples to in-process
// handlers and returns (status, body, content_type).
//
// M4 baseline only handles `GET /computer/display/dimensions` with a
// hardcoded response. Future milestones add real handlers:
//   M5: GET /computer/display/screenshot     (libs/scrap)
//   M6: POST /computer/input/{mouse,keyboard} (libs/enigo)
//       POST /computer/copy_to_clipboard      (libs/clipboard)
//   M7: GET/POST /computer/fs/*               (tokio::fs)
//       POST /computer/shell/powershell/*     (tokio::process)
//       /internal/{shutdown,diagnostics,update,keepalive/*}
//
// As of M4 every other path returns 501 + a small JSON error body so
// the caller can clearly tell "your tunnel is alive, this endpoint is
// just not implemented yet" from "the tunnel is broken."

use super::framing::RequestMeta;
use serde_json::json;

/// Route a single request to its handler. `body` is the accumulated
/// inbound binary body (may be empty).
pub fn dispatch(meta: &RequestMeta, _body: &[u8]) -> (u16, Vec<u8>, &'static str) {
    match (meta.method.as_str(), meta.path.as_str()) {
        ("GET", "/computer/display/dimensions") => {
            // M4 hardcoded; M5 will replace with a real
            // scrap::Display::primary() lookup.
            let body = serde_json::to_vec(&json!({"width": 1920, "height": 1080}))
                .expect("static JSON serializes");
            (200, body, "application/json")
        }
        _ => (
            501,
            br#"{"error":"not implemented in cyberdesk_tunnel yet (M4 baseline)"}"#.to_vec(),
            "application/json",
        ),
    }
}
