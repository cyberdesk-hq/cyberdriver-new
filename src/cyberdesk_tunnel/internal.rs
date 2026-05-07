// SPDX-License-Identifier: AGPL-3.0-only
//
// Internal control-plane endpoints for Cyberdesk's HTTP-over-WS tunnel.

use hbb_common::{
    anyhow::{Context, Result},
    config::{Config, LocalConfig},
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
static KEEPALIVE_ENABLED: AtomicBool = AtomicBool::new(true);
static LAST_REMOTE_ACTIVITY_SECS: AtomicU64 = AtomicU64::new(0);
static KEEPALIVE_LOOP_STARTED: AtomicBool = AtomicBool::new(false);

const MAX_DIAGNOSTIC_LOG_BYTES: u64 = 64 * 1024;
const KEEPALIVE_ENABLED_OPTION: &str = "cyberdesk_keepalive_enabled";
const KEEPALIVE_THRESHOLD_MINUTES_OPTION: &str = "cyberdesk_keepalive_threshold_minutes";
const DEFAULT_KEEPALIVE_THRESHOLD_MINUTES: u64 = 3;

#[derive(Debug, Serialize)]
struct Diagnostics {
    version: &'static str,
    timestamp: u64,
    platform: &'static str,
    hostname: String,
    process_id: u32,
    machine_id: String,
    rustdesk_peer_id: String,
    api_key_configured: bool,
    api_base: String,
    cyberdesk_environment: String,
    desktop_api_server: String,
    rendezvous_server: String,
    relay_server: String,
    platform_additions: Value,
    config_path: String,
    tunnel_config_path: String,
    log_dir: String,
    latest_log_path: Option<String>,
    latest_log_tail: Option<String>,
    display_dimensions: Option<Value>,
    shutdown_requested: bool,
    keepalive_enabled: bool,
    last_remote_activity_secs: u64,
}

pub fn diagnostics() -> Result<Vec<u8>> {
    let log_dir = Config::log_path();
    let latest_log = latest_log_file(&log_dir);
    let latest_log_tail = latest_log
        .as_ref()
        .and_then(|path| tail_file(path, MAX_DIAGNOSTIC_LOG_BYTES).ok());
    let display_dimensions = super::display::dimensions()
        .ok()
        .and_then(|body| serde_json::from_slice::<Value>(&body).ok());
    Ok(serde_json::to_vec(&Diagnostics {
        version: crate::VERSION,
        timestamp: now_secs(),
        platform: platform_name(),
        hostname: crate::common::hostname(),
        process_id: std::process::id(),
        machine_id: crate::cyberdesk_tunnel::current_fingerprint().unwrap_or_default(),
        rustdesk_peer_id: Config::get_id(),
        api_key_configured: crate::cyberdesk_tunnel::configured_api_key().is_some(),
        api_base: crate::cyberdesk_tunnel::configured_api_base(),
        cyberdesk_environment: LocalConfig::get_option(
            crate::cyberdesk_branding::ENVIRONMENT_OPTION,
        ),
        desktop_api_server: Config::get_option("api-server"),
        rendezvous_server: Config::get_option("custom-rendezvous-server"),
        relay_server: Config::get_option("relay-server"),
        platform_additions: platform_additions(),
        config_path: Config::file().display().to_string(),
        tunnel_config_path: crate::cyberdesk_tunnel::config_path().display().to_string(),
        log_dir: log_dir.display().to_string(),
        latest_log_path: latest_log.map(|path| path.display().to_string()),
        latest_log_tail,
        display_dimensions,
        shutdown_requested: SHUTDOWN_REQUESTED.load(Ordering::SeqCst),
        keepalive_enabled: keepalive_enabled(),
        last_remote_activity_secs: LAST_REMOTE_ACTIVITY_SECS.load(Ordering::SeqCst),
    })?)
}

pub fn record_remote_activity() {
    LAST_REMOTE_ACTIVITY_SECS.store(now_secs(), Ordering::SeqCst);
}

pub fn keepalive_activity() -> Result<(u16, Vec<u8>, &'static str)> {
    record_remote_activity();
    Ok((204, Vec::new(), "application/json"))
}

pub fn keepalive_enable() -> Result<(u16, Vec<u8>, &'static str)> {
    KEEPALIVE_ENABLED.store(true, Ordering::SeqCst);
    LocalConfig::set_option(KEEPALIVE_ENABLED_OPTION.to_string(), "Y".to_string());
    Ok((204, Vec::new(), "application/json"))
}

pub fn keepalive_disable() -> Result<(u16, Vec<u8>, &'static str)> {
    KEEPALIVE_ENABLED.store(false, Ordering::SeqCst);
    LocalConfig::set_option(KEEPALIVE_ENABLED_OPTION.to_string(), "N".to_string());
    Ok((204, Vec::new(), "application/json"))
}

pub fn keepalive_enabled() -> bool {
    KEEPALIVE_ENABLED.load(Ordering::SeqCst)
        && LocalConfig::get_option(KEEPALIVE_ENABLED_OPTION) != "N"
}

pub fn spawn_keepalive_loop() {
    if KEEPALIVE_LOOP_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    record_remote_activity();
    hbb_common::tokio::spawn(async move {
        loop {
            hbb_common::tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if !keepalive_enabled() {
                continue;
            }
            let now = now_secs();
            let last = LAST_REMOTE_ACTIVITY_SECS.load(Ordering::SeqCst);
            let threshold_secs = keepalive_threshold_secs();
            if last > 0 && now.saturating_sub(last) >= threshold_secs {
                super::input::perform_keepalive_tick();
                LAST_REMOTE_ACTIVITY_SECS.store(now_secs(), Ordering::SeqCst);
            }
        }
    });
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

pub fn update(_body: &[u8]) -> Result<Vec<u8>> {
    crate::updater::manually_check_update().context("failed to trigger Cyberdriver updater")?;

    Ok(serde_json::to_vec(&json!({
        "status": "update_check_started",
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

fn keepalive_threshold_secs() -> u64 {
    let minutes = LocalConfig::get_option(KEEPALIVE_THRESHOLD_MINUTES_OPTION)
        .parse::<u64>()
        .unwrap_or(DEFAULT_KEEPALIVE_THRESHOLD_MINUTES)
        .clamp(1, 60);
    minutes * 60
}

fn latest_log_file(log_dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(log_dir).ok()?;
    entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            let modified = metadata.modified().ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn tail_file(path: &Path, max_bytes: u64) -> Result<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let mut file = fs::File::open(path).context("failed to open log file")?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max_bytes {
        file.seek(SeekFrom::Start(len - max_bytes))
            .context("failed to seek log file")?;
    }
    let mut bytes = Vec::new();
    file.take(max_bytes)
        .read_to_end(&mut bytes)
        .context("failed to read log file")?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
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

fn platform_additions() -> Value {
    #[cfg(windows)]
    {
        return Value::Object(crate::virtual_display_manager::get_platform_additions());
    }
    #[cfg(not(windows))]
    {
        Value::Object(serde_json::Map::new())
    }
}
