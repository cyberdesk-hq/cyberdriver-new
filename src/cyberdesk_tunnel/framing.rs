// SPDX-License-Identifier: AGPL-3.0-only
//
// Wire types for the JSON-meta + binary-chunks + "end" framing the
// Cyberdesk cloud uses on /tunnel/ws. Mirrors
// apps/shared/src/shared/services/connection_manager.py exactly.
//
// Framing per request (cloud -> agent):
//   1. text frame with `RequestMeta` JSON
//   2. zero or more binary frames containing the body bytes (split into
//      chunks of `settings.max_chunk_size` server-side)
//   3. text frame containing the literal string "end"
//
// Framing per response (agent -> cloud) is symmetric using
// `ResponseMeta`.
//
// See scratch/tunnel-proto/ for the standalone prototype this module
// was extracted from. The discoveries.md log notes one critical
// gotcha: the cloud sends `query: ""` (empty string) when there are no
// query params, NOT `query: {}`, so `headers` and `query` MUST be
// declared as raw `serde_json::Value` to deserialize. Typed maps will
// blow up the moment a real Cyberdesk request comes through.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Request metadata received from the cloud as the first text frame
/// of each request.
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // headers/query unused by current dispatch but kept for future M5+ work
pub struct RequestMeta {
    pub method: String,
    pub path: String,
    /// Inbound HTTP headers proxied from the original request. Empty
    /// is sent as `{}`. Kept as raw `Value` because the cloud's shape
    /// occasionally varies (see module doc).
    #[serde(default)]
    pub headers: Value,
    /// Query string. Empty is sent as `""` (string), present is sent
    /// as a map. `Value` accepts both.
    #[serde(default)]
    pub query: Value,
    /// Used to match this request to its eventual response on the
    /// cloud side.
    #[serde(rename = "requestId")]
    pub request_id: String,
}

/// Response metadata sent back as the first text frame of each
/// response. Body bytes follow as binary frames; an "end" text frame
/// terminates.
#[derive(Debug, Serialize)]
pub struct ResponseMeta {
    pub status: u16,
    pub headers: HashMap<String, String>,
    #[serde(rename = "requestId")]
    pub request_id: String,
}
