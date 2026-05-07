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
use std::{path::PathBuf, time::Duration};

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
    maybe_reset_fingerprint_from_env();

    let api_key = match configured_api_key() {
        Some(k) => k,
        _ => {
            log::info!(
                "cyberdesk_tunnel: API key not set; tunnel disabled (this is fine for \
                 client-mode installs)"
            );
            return;
        }
    };

    let api_base = configured_api_base();

    let fingerprint =
        std::env::var("CYBERDESK_FINGERPRINT").unwrap_or_else(|_| persistent_fingerprint());

    log::info!(
        "cyberdesk_tunnel: spawning tunnel client (api_base={}, fingerprint={})",
        api_base,
        fingerprint
    );
    internal::spawn_keepalive_loop();

    // Schedule onto RustDesk's existing tokio runtime via hbb_common's
    // re-export. We deliberately do NOT create a new runtime here.
    hbb_common::tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        let mut max_backoff_failures = 0_u8;
        let dispatch_semaphore = client::dispatch_semaphore();
        loop {
            let machine_name = crate::cyberdesk_cli::machine_name_from_env();
            let result = client::run(
                api_key.clone(),
                api_base.clone(),
                fingerprint.clone(),
                machine_name,
                dispatch_semaphore.clone(),
            )
            .await;
            let mut retry_after = None;
            match &result {
                Ok(()) => {
                    log::info!("cyberdesk_tunnel: client exited cleanly; reconnecting");
                    backoff = Duration::from_secs(1);
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
                        backoff = Duration::from_secs(1);
                        max_backoff_failures = 0;
                    } else if backoff >= Duration::from_secs(16) {
                        max_backoff_failures = max_backoff_failures.saturating_add(1);
                        if max_backoff_failures >= 3 {
                            log::error!(
                                "cyberdesk_tunnel: max reconnect backoff failed 3 times; exiting for service manager restart"
                            );
                            std::process::exit(75);
                        }
                    } else {
                        max_backoff_failures = 0;
                    }
                }
            };

            let sleep_for = retry_after.unwrap_or_else(|| jittered_backoff(backoff));
            hbb_common::tokio::time::sleep(sleep_for).await;
            if retry_after.is_none() {
                backoff = std::cmp::min(backoff * 2, Duration::from_secs(16));
            }
        }
    });
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
    Ok(())
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
    let path = legacy_config_path()?;
    let data = std::fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&data).ok()?;
    let fingerprint = value.get("fingerprint")?.as_str()?.trim();
    if fingerprint.is_empty() {
        return None;
    }
    Some((fingerprint.to_string(), path))
}

fn legacy_config_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .map(|base| base.join(".cyberdriver").join("config.json"))
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
            })?;
        Some(base.join(".cyberdriver").join("config.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_configured_api_key, API_KEY_ENC_VERSION, API_KEY_MAX_LEN};
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
}
