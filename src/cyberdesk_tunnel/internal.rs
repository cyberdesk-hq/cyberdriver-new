// SPDX-License-Identifier: AGPL-3.0-only
//
// Internal control-plane endpoints for Cyberdesk's HTTP-over-WS tunnel.

use hbb_common::anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;
use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
static KEEPALIVE_ENABLED: AtomicBool = AtomicBool::new(true);
static LAST_REMOTE_ACTIVITY_SECS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Serialize)]
struct Diagnostics {
    version: &'static str,
    timestamp: u64,
    platform: &'static str,
    hostname: String,
    process_id: u32,
    shutdown_requested: bool,
    keepalive_enabled: bool,
    last_remote_activity_secs: u64,
}

pub fn diagnostics() -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&Diagnostics {
        version: crate::VERSION,
        timestamp: now_secs(),
        platform: platform_name(),
        hostname: crate::common::hostname(),
        process_id: std::process::id(),
        shutdown_requested: SHUTDOWN_REQUESTED.load(Ordering::SeqCst),
        keepalive_enabled: KEEPALIVE_ENABLED.load(Ordering::SeqCst),
        last_remote_activity_secs: LAST_REMOTE_ACTIVITY_SECS.load(Ordering::SeqCst),
    })?)
}

pub fn keepalive_activity() -> Result<(u16, Vec<u8>, &'static str)> {
    LAST_REMOTE_ACTIVITY_SECS.store(now_secs(), Ordering::SeqCst);
    Ok((204, Vec::new(), "application/json"))
}

pub fn keepalive_enable() -> Result<(u16, Vec<u8>, &'static str)> {
    KEEPALIVE_ENABLED.store(true, Ordering::SeqCst);
    Ok((204, Vec::new(), "application/json"))
}

pub fn keepalive_disable() -> Result<(u16, Vec<u8>, &'static str)> {
    KEEPALIVE_ENABLED.store(false, Ordering::SeqCst);
    Ok((204, Vec::new(), "application/json"))
}

pub fn shutdown_enabled() -> bool {
    matches!(
        std::env::var("CYBERDESK_ENABLE_INTERNAL_SHUTDOWN"),
        Ok(value) if value == "1" || value.eq_ignore_ascii_case("true")
    )
}

pub fn shutdown(body: &[u8]) -> Result<Vec<u8>> {
    let payload = parse_json_value(body).unwrap_or_else(|_| json!({}));
    let reason = payload
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_string();
    let source = payload
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let pid = std::process::id();

    let already = SHUTDOWN_REQUESTED.swap(true, Ordering::SeqCst);
    if !already {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(250));
            std::process::exit(0);
        });
    }

    Ok(serde_json::to_vec(&json!({
        "status": if already { "already_shutting_down" } else { "shutting_down" },
        "pid": pid,
        "reason": reason,
        "source": source,
    }))?)
}

pub fn update(body: &[u8]) -> Result<Vec<u8>> {
    let payload = parse_json_value(body).unwrap_or_else(|_| json!({}));
    let requested_version = payload
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("latest")
        .to_string();
    let restart = payload
        .get("restart")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    crate::updater::manually_check_update().context("failed to trigger Cyberdriver updater")?;

    Ok(serde_json::to_vec(&json!({
        "status": "update_check_started",
        "version": requested_version,
        "restart": restart,
    }))?)
}

fn parse_json_value(body: &[u8]) -> Result<serde_json::Value> {
    if body.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(body).context("invalid JSON request body")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn platform_name() -> &'static str {
    #[cfg(windows)]
    return "windows";
    #[cfg(target_os = "macos")]
    return "macos";
    #[cfg(target_os = "linux")]
    return "linux";
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    return "unknown";
}
