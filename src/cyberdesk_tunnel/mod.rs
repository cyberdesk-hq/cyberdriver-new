// SPDX-License-Identifier: AGPL-3.0-only
//
// cyberdesk_tunnel — Cyberdriver's reverse-tunnel client.
//
// Opens a long-lived WebSocket from the agent out to the Cyberdesk
// cloud (`apps/websockets` /tunnel/ws), accepts HTTP-shaped requests
// framed as JSON-meta + binary chunks + "end" sentinel, dispatches them
// against a small in-process surface (display/input/fs/shell/internal),
// and sends responses back in the same framing. See
// scratch/tunnel-proto/ (deleted post-M4) for the standalone prototype
// this was forged from.
//
// Gated by the `cyberdesk` Cargo feature so non-Cyberdesk RustDesk
// builds (and our cyberdesk-connect-only build profile) don't pay the
// dep cost or behavior change.

use hbb_common::{
    anyhow::{bail, Context, Result},
    config, log,
    password_security::{decrypt_str_or_original, encrypt_str_or_original},
};
use serde_derive::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::Duration,
};

mod client;
mod dispatch;
mod display;
mod framing;
mod fs;
mod input;
mod internal;
mod shell;

const API_KEY_ENC_VERSION: &str = "00";
const API_KEY_MAX_LEN: usize = 4096;
pub const REMOTE_KEEPALIVE_FOR_OPTION: &str = "cyberdesk_remote_keepalive_for";
const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_secs(1);
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(16);
const STABLE_CONNECTION_RESET_AFTER: Duration = Duration::from_secs(10);
const MAX_BACKOFF_WARNING_THRESHOLD: u32 = 3;

static TUNNEL_CONFIG_REVISION: AtomicU64 = AtomicU64::new(0);
static TUNNEL_TASK_ACTIVE: AtomicBool = AtomicBool::new(false);
static TUNNEL_CONFIG_CHANGE_TX: Mutex<Option<hbb_common::tokio::sync::watch::Sender<u64>>> =
    Mutex::new(None);

fn path_without_query(path: &str) -> &str {
    path.split_once('?').map(|(path, _)| path).unwrap_or(path)
}

fn parse_json<T: for<'de> serde::Deserialize<'de>>(body: &[u8]) -> Result<T> {
    if body.is_empty() {
        bail!("missing JSON request body");
    }
    Ok(serde_json::from_slice(body).context("invalid JSON request body")?)
}

/// Entry point called from `src/server.rs::start_server` during
/// service-mode bootstrap. Non-blocking — spawns a background task on
/// RustDesk's existing tokio runtime and returns immediately.
///
/// Behavior is controlled by env vars (M4 baseline; M7 will move
/// these to LocalConfig so the Settings UI can edit them at runtime):
///
/// | Var                       | Meaning                                              |
/// |---------------------------|------------------------------------------------------|
/// | `CYBERDESK_AGENT_KEY`     | Optional env `ak_*`; falls back to LocalConfig.      |
/// | `CYBERDESK_API_BASE`      | Tunnel WS base URL. Default: branded API server.     |
/// | `CYBERDESK_FINGERPRINT`   | Stable machine UUID. Default: persisted random UUID. |
///
/// If `CYBERDESK_AGENT_KEY` is unset, the tunnel does not start. This
/// is the correct default for client-mode installs (the laptop case)
/// and for any build that doesn't want Cyberdesk control.
pub fn spawn_if_enabled() {
    maybe_reset_identity_from_env();
    maybe_reset_fingerprint_from_env();

    if configured_api_key().is_none() {
        log::info!(
            "cyberdesk_tunnel: API key not set; tunnel disabled (this is fine for \
             client-mode installs)"
        );
        return;
    }

    let api_base = configured_api_base();

    if TUNNEL_TASK_ACTIVE.swap(true, Ordering::SeqCst) {
        log::info!("cyberdesk_tunnel: tunnel task already running; signaling config change");
        signal_tunnel_config_changed();
        return;
    }

    log::info!(
        "cyberdesk_tunnel: spawning tunnel client (api_base={})",
        api_base
    );
    internal::spawn_keepalive_loop();
    let (config_change_tx, mut config_change_rx) =
        hbb_common::tokio::sync::watch::channel(TUNNEL_CONFIG_REVISION.load(Ordering::SeqCst));
    if let Ok(mut tx) = TUNNEL_CONFIG_CHANGE_TX.lock() {
        *tx = Some(config_change_tx);
    }

    // Schedule onto RustDesk's existing tokio runtime via hbb_common's
    // re-export. We deliberately do NOT create a new runtime here.
    hbb_common::tokio::spawn(async move {
        let mut backoff = INITIAL_RECONNECT_BACKOFF;
        let mut max_backoff_failures = 0_u32;
        let dispatch_semaphore = client::dispatch_semaphore();
        loop {
            let Some(api_key) = configured_api_key() else {
                log::info!("cyberdesk_tunnel: API key cleared; tunnel idle until reconfigured");
                if config_change_rx.changed().await.is_err() {
                    break;
                }
                backoff = INITIAL_RECONNECT_BACKOFF;
                max_backoff_failures = 0;
                continue;
            };
            let api_base = configured_api_base();
            let fingerprint =
                std::env::var("CYBERDESK_FINGERPRINT").unwrap_or_else(|_| persistent_fingerprint());
            let machine_name = crate::cyberdesk_cli::machine_name_from_env();
            let remote_keepalive_for = configured_remote_keepalive_for();
            let result = hbb_common::tokio::select! {
                result = client::run(
                    api_key.clone(),
                    api_base.clone(),
                    fingerprint.clone(),
                    machine_name,
                    remote_keepalive_for,
                    dispatch_semaphore.clone(),
                ) => result,
                changed = config_change_rx.changed() => {
                    if changed.is_err() {
                        log::info!("cyberdesk_tunnel: config change channel closed; reconnecting");
                    } else if configured_api_key().is_none() {
                        log::info!("cyberdesk_tunnel: API key cleared; closing active tunnel");
                    } else {
                        log::info!("cyberdesk_tunnel: config changed; reconnecting active tunnel");
                    }
                    backoff = INITIAL_RECONNECT_BACKOFF;
                    max_backoff_failures = 0;
                    continue;
                }
            };
            let mut retry_after = None;
            match &result {
                Ok(()) => {
                    log::info!("cyberdesk_tunnel: client exited cleanly; reconnecting");
                    backoff = INITIAL_RECONNECT_BACKOFF;
                    max_backoff_failures = 0;
                }
                Err(e) => {
                    retry_after = e
                        .downcast_ref::<client::RateLimited>()
                        .map(|e| e.retry_after());
                    let message = format!("{e:?}");
                    log::error!("cyberdesk_tunnel: client exited with error: {message}");
                    if client::is_non_retryable_auth_error(e) {
                        log::error!(
                            "cyberdesk_tunnel: connection rejected; tunnel will not reconnect"
                        );
                        break;
                    }
                    if retry_after.is_some() {
                        backoff = INITIAL_RECONNECT_BACKOFF;
                        max_backoff_failures = 0;
                    } else if let Some(connected_for) = client::connected_for_error(e)
                        .filter(|connected_for| *connected_for >= STABLE_CONNECTION_RESET_AFTER)
                    {
                        log::info!(
                            "cyberdesk_tunnel: tunnel was stable for {:.1}s before dropping; resetting reconnect backoff",
                            connected_for.as_secs_f64()
                        );
                        backoff = INITIAL_RECONNECT_BACKOFF;
                        max_backoff_failures = 0;
                    } else if backoff >= MAX_RECONNECT_BACKOFF {
                        max_backoff_failures = max_backoff_failures.saturating_add(1);
                        if should_log_max_backoff_warning(max_backoff_failures) {
                            log::error!(
                                "cyberdesk_tunnel: max reconnect backoff failed {} times; continuing to retry because no supervisor is guaranteed",
                                max_backoff_failures
                            );
                        }
                    } else {
                        max_backoff_failures = 0;
                    }
                }
            };

            let sleep_for = retry_after.unwrap_or_else(|| jittered_backoff(backoff));
            hbb_common::tokio::time::sleep(sleep_for).await;
            if retry_after.is_none() {
                backoff = std::cmp::min(backoff * 2, MAX_RECONNECT_BACKOFF);
            }
        }
        TUNNEL_TASK_ACTIVE.store(false, Ordering::SeqCst);
        if let Ok(mut tx) = TUNNEL_CONFIG_CHANGE_TX.lock() {
            *tx = None;
        }
    });
}

fn signal_tunnel_config_changed() {
    let revision = TUNNEL_CONFIG_REVISION.fetch_add(1, Ordering::SeqCst) + 1;
    if let Ok(tx) = TUNNEL_CONFIG_CHANGE_TX.lock() {
        if let Some(tx) = tx.as_ref() {
            let _ = tx.send(revision);
        }
    }
}

fn jittered_backoff(base: Duration) -> Duration {
    let jitter_range_ms = (base.as_millis() * 30 / 100) as u64;
    if jitter_range_ms == 0 {
        return base;
    }
    let jitter_ms = random_jitter_ms(jitter_range_ms * 2 + 1) as i64 - jitter_range_ms as i64;
    if jitter_ms >= 0 {
        base + Duration::from_millis(jitter_ms as u64)
    } else {
        base.saturating_sub(Duration::from_millis((-jitter_ms) as u64))
    }
}

fn should_log_max_backoff_warning(failures: u32) -> bool {
    failures == MAX_BACKOFF_WARNING_THRESHOLD
        || (failures > MAX_BACKOFF_WARNING_THRESHOLD && failures % 10 == 0)
}

fn random_jitter_ms(range_ms: u64) -> u64 {
    fastrand::u64(0..range_ms)
}

fn default_api_base() -> String {
    let api_server = crate::cyberdesk_branding::API_SERVER;
    if let Some(rest) = api_server.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = api_server.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        api_server.to_string()
    }
}

pub(crate) fn configured_api_key() -> Option<String> {
    std::env::var("CYBERDESK_AGENT_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let value = config::LocalConfig::get_option("cyberdesk_api_key");
            match decode_configured_api_key(&value) {
                Some((api_key, should_store)) => {
                    if should_store {
                        let _ = store_configured_api_key(api_key.clone());
                    }
                    Some(api_key)
                }
                None => {
                    if !value.trim().is_empty() {
                        config::LocalConfig::set_option(
                            "cyberdesk_api_key".to_string(),
                            String::new(),
                        );
                    }
                    None
                }
            }
        })
}

fn decode_configured_api_key(value: &str) -> Option<(String, bool)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let (api_key, decrypted, should_store) =
        decrypt_str_or_original(value, API_KEY_ENC_VERSION);
    if value.starts_with(API_KEY_ENC_VERSION) && !decrypted {
        log::error!("cyberdesk_tunnel: stored Cyberdesk API key could not be decrypted");
        return None;
    }
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        None
    } else {
        Some((api_key, should_store))
    }
}

pub(crate) fn store_configured_api_key(api_key: String) -> Result<(), &'static str> {
    let encrypted = encrypt_str_or_original(&api_key, API_KEY_ENC_VERSION, API_KEY_MAX_LEN);
    if encrypted.is_empty() {
        log::error!("cyberdesk_tunnel: refusing to store oversized Cyberdesk API key");
        return Err("Cyberdesk API key is too large to store securely");
    }
    config::LocalConfig::set_option("cyberdesk_api_key".to_string(), encrypted);
    signal_tunnel_config_changed();
    Ok(())
}

pub(crate) fn clear_configured_api_key() {
    config::LocalConfig::set_option("cyberdesk_api_key".to_string(), String::new());
    signal_tunnel_config_changed();
}

pub(crate) fn configured_api_base() -> String {
    std::env::var("CYBERDESK_API_BASE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let value = config::LocalConfig::get_option("cyberdesk_api_base");
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        })
        .unwrap_or_else(default_api_base)
}

pub(crate) fn store_configured_api_base(api_base: String) {
    config::LocalConfig::set_option("cyberdesk_api_base".to_string(), api_base);
    signal_tunnel_config_changed();
}

pub(crate) fn configured_remote_keepalive_for() -> Option<String> {
    std::env::var("CYBERDESK_REMOTE_KEEPALIVE_FOR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let value = config::LocalConfig::get_option(REMOTE_KEEPALIVE_FOR_OPTION);
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        })
}

pub(crate) fn store_configured_remote_keepalive_for(machine_id: Option<String>) {
    config::LocalConfig::set_option(
        REMOTE_KEEPALIVE_FOR_OPTION.to_string(),
        machine_id.unwrap_or_default(),
    );
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TunnelConfig {
    #[serde(default)]
    fingerprint: String,
}

pub fn config_path() -> PathBuf {
    config::Config::path("cyberdesk_tunnel.toml")
}

pub fn current_fingerprint() -> Option<String> {
    let tunnel_config = config::load_path::<TunnelConfig>(config_path());
    if tunnel_config.fingerprint.is_empty() {
        None
    } else {
        Some(tunnel_config.fingerprint)
    }
}

pub fn reset_fingerprint() -> Result<()> {
    let path = config_path();
    let mut tunnel_config = config::load_path::<TunnelConfig>(path.clone());
    tunnel_config.fingerprint.clear();
    if let Err(err) = config::store_path(path, &tunnel_config) {
        log::error!("cyberdesk_tunnel: failed to reset fingerprint: {err}");
        return Err(err);
    }
    Ok(())
}

pub fn generate_new_identity() -> Result<String> {
    reset_fingerprint()?;
    match config::Config::generate_new_identity_id() {
        Some(id) => {
            log::info!("cyberdesk_tunnel: generated new Cyberdriver identity id {}", id);
            Ok(id)
        }
        None => bail!("failed to generate new Cyberdriver identity id"),
    }
}

fn maybe_reset_identity_from_env() {
    if matches!(
        std::env::var("CYBERDRIVER_NEW_IDENTITY")
            .or_else(|_| std::env::var("CYBERDRIVER_RESET_IDENTITY")),
        Ok(value) if value == "1" || value.eq_ignore_ascii_case("true")
    ) {
        match generate_new_identity() {
            Ok(id) => log::info!(
                "cyberdesk_tunnel: generated new identity from environment; rustdesk_peer_id={id}"
            ),
            Err(err) => log::error!(
                "cyberdesk_tunnel: failed to generate new identity from environment: {err}"
            ),
        }
        std::env::remove_var("CYBERDRIVER_NEW_IDENTITY");
        std::env::remove_var("CYBERDRIVER_RESET_IDENTITY");
    }
}

fn maybe_reset_fingerprint_from_env() {
    if matches!(
        std::env::var("CYBERDRIVER_RESET_FINGERPRINT"),
        Ok(value) if value == "1" || value.eq_ignore_ascii_case("true")
    ) {
        match reset_fingerprint() {
            Ok(_) => log::info!(
                "cyberdesk_tunnel: reset fingerprint from CYBERDRIVER_RESET_FINGERPRINT"
            ),
            Err(err) => log::error!(
                "cyberdesk_tunnel: failed to reset fingerprint from CYBERDRIVER_RESET_FINGERPRINT: {err}"
            ),
        }
        std::env::remove_var("CYBERDRIVER_RESET_FINGERPRINT");
    }
}

fn persistent_fingerprint() -> String {
    let path = config_path();
    let mut tunnel_config = config::load_path::<TunnelConfig>(path.clone());
    if !tunnel_config.fingerprint.is_empty() {
        return tunnel_config.fingerprint;
    }

    let legacy_path = if let Some((fingerprint, legacy_path)) = legacy_fingerprint() {
        tunnel_config.fingerprint = fingerprint;
        Some(legacy_path)
    } else {
        tunnel_config.fingerprint = uuid::Uuid::new_v4().to_string();
        None
    };
    match config::store_path(path, &tunnel_config) {
        Ok(()) => {
            if let Some(legacy_path) = legacy_path {
                log::info!(
                    "cyberdesk_tunnel: migrated legacy Cyberdriver fingerprint from {}",
                    legacy_path.display()
                );
            }
        }
        Err(err) => log::error!("cyberdesk_tunnel: failed to store fingerprint: {err}"),
    }
    tunnel_config.fingerprint
}

fn legacy_fingerprint() -> Option<(String, PathBuf)> {
    for path in legacy_config_paths() {
        let data = match std::fs::read_to_string(&path) {
            Ok(data) => data,
            Err(_) => continue,
        };
        let value: serde_json::Value = match serde_json::from_str(&data) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let fingerprint = value
            .get("fingerprint")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .unwrap_or_default();
        if !fingerprint.is_empty() {
            return Some((fingerprint.to_string(), path));
        }
    }
    None
}

fn legacy_config_paths() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mut candidates = Vec::new();
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(local_app_data)
                    .join(".cyberdriver")
                    .join("config.json"),
            );
        }
        if let Some(user_profile) = std::env::var_os("USERPROFILE") {
            candidates.push(
                PathBuf::from(user_profile)
                    .join("AppData")
                    .join("Local")
                    .join(".cyberdriver")
                    .join("config.json"),
            );
        }
        for users_root in windows_users_roots() {
            let mut user_candidates = Vec::new();
            if let Ok(entries) = std::fs::read_dir(users_root) {
                for entry in entries.flatten() {
                    let profile = entry.path();
                    if !profile.is_dir() || is_windows_system_profile(&profile) {
                        continue;
                    }
                    let config = profile
                        .join("AppData")
                        .join("Local")
                        .join(".cyberdriver")
                        .join("config.json");
                    if let Ok(metadata) = std::fs::metadata(&config) {
                        let modified = metadata.modified().ok();
                        user_candidates.push((modified, config));
                    }
                }
            }
            user_candidates.sort_by(|left, right| right.0.cmp(&left.0));
            candidates.extend(user_candidates.into_iter().map(|(_, path)| path));
        }
        dedupe_paths(candidates)
    }
    #[cfg(not(windows))]
    {
        let Some(base) = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
            })
        else {
            return Vec::new();
        };
        vec![base.join(".cyberdriver").join("config.json")]
    }
}

#[cfg(windows)]
fn windows_users_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(system_drive) = std::env::var_os("SystemDrive") {
        roots.push(PathBuf::from(system_drive).join("Users"));
    }
    if let Some(home_drive) = std::env::var_os("HOMEDRIVE") {
        roots.push(PathBuf::from(home_drive).join("Users"));
    }
    for letter in b'A'..=b'Z' {
        roots.push(PathBuf::from(format!("{}:\\Users", letter as char)));
    }
    dedupe_paths(roots)
}

#[cfg(windows)]
fn is_windows_system_profile(profile: &std::path::Path) -> bool {
    let Some(name) = profile.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "all users" | "default" | "default user" | "public"
    )
}

#[cfg(windows)]
fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for path in paths {
        let key = path.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key) {
            deduped.push(path);
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use super::{
        decode_configured_api_key, should_log_max_backoff_warning, API_KEY_ENC_VERSION,
        API_KEY_MAX_LEN,
    };
    use hbb_common::password_security::encrypt_str_or_original;

    #[test]
    fn decode_configured_api_key_omits_empty_values() {
        assert_eq!(decode_configured_api_key(""), None);
        assert_eq!(decode_configured_api_key("   "), None);

        let encrypted_empty = encrypt_str_or_original("", API_KEY_ENC_VERSION, API_KEY_MAX_LEN);
        assert_eq!(decode_configured_api_key(&encrypted_empty), None);
    }

    #[test]
    fn decode_configured_api_key_trims_plaintext_and_marks_for_migration() {
        assert_eq!(
            decode_configured_api_key("  ak_test  "),
            Some(("ak_test".to_string(), true))
        );
    }

    #[test]
    fn decode_configured_api_key_reads_encrypted_value() {
        let encrypted = encrypt_str_or_original("ak_encrypted", API_KEY_ENC_VERSION, API_KEY_MAX_LEN);
        assert_eq!(
            decode_configured_api_key(&encrypted),
            Some(("ak_encrypted".to_string(), false))
        );
    }

    #[test]
    fn decode_configured_api_key_rejects_undecryptable_encrypted_value() {
        let mut encrypted =
            encrypt_str_or_original("ak_encrypted", API_KEY_ENC_VERSION, API_KEY_MAX_LEN);
        encrypted.push('x');

        assert_eq!(decode_configured_api_key(&encrypted), None);
    }

    #[test]
    fn max_backoff_warning_logs_at_threshold_and_periodically() {
        assert!(!should_log_max_backoff_warning(1));
        assert!(!should_log_max_backoff_warning(2));
        assert!(should_log_max_backoff_warning(3));
        assert!(!should_log_max_backoff_warning(4));
        assert!(should_log_max_backoff_warning(10));
        assert!(!should_log_max_backoff_warning(11));
        assert!(should_log_max_backoff_warning(20));
    }
}
