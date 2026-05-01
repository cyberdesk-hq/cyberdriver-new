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
    let machine_name = crate::cyberdesk_cli::machine_name_from_env();

    log::info!(
        "cyberdesk_tunnel: spawning tunnel client (api_base={}, fingerprint={})",
        api_base,
        fingerprint
    );

    // Schedule onto RustDesk's existing tokio runtime via hbb_common's
    // re-export. We deliberately do NOT create a new runtime here.
    hbb_common::tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        let dispatch_semaphore = client::dispatch_semaphore();
        loop {
            let result = client::run(
                api_key.clone(),
                api_base.clone(),
                fingerprint.clone(),
                machine_name.clone(),
                dispatch_semaphore.clone(),
            )
            .await;
            match result {
                Ok(()) => {
                    log::info!("cyberdesk_tunnel: client exited cleanly; reconnecting");
                }
                Err(e) => {
                    let message = format!("{e:?}");
                    log::error!("cyberdesk_tunnel: client exited with error: {message}");
                    if is_non_retryable_auth_error(&message) {
                        log::error!("cyberdesk_tunnel: auth rejected; tunnel will not reconnect");
                        break;
                    }
                }
            };

            hbb_common::tokio::time::sleep(backoff).await;
            backoff = std::cmp::min(backoff * 2, Duration::from_secs(16));
        }
    });
}

fn is_non_retryable_auth_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("close 4001")
        || lower.contains("server rejected auth")
        || contains_auth_status(&lower, "401", "unauthorized")
        || contains_auth_status(&lower, "403", "forbidden")
}

fn contains_auth_status(message: &str, status: &str, status_text: &str) -> bool {
    let status_patterns = [
        format!("http {status}"),
        format!("http status {status}"),
        format!("status {status}"),
        format!("status: {status}"),
        format!("status={status}"),
        format!("{status} {status_text}"),
        format!("{status}: {status_text}"),
        format!("{status} ({status_text})"),
        format!("({status} {status_text})"),
    ];

    status_patterns
        .iter()
        .any(|pattern| message.contains(pattern))
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
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let value = config::LocalConfig::get_option("cyberdesk_api_key");
            if value.trim().is_empty() {
                None
            } else {
                Some(value)
            }
        })
}

pub(crate) fn configured_api_base() -> String {
    std::env::var("CYBERDESK_API_BASE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let value = config::LocalConfig::get_option("cyberdesk_api_base");
            if value.trim().is_empty() {
                None
            } else {
                Some(value)
            }
        })
        .unwrap_or_else(default_api_base)
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

pub fn reset_fingerprint() {
    let path = config_path();
    let mut tunnel_config = config::load_path::<TunnelConfig>(path.clone());
    tunnel_config.fingerprint.clear();
    if let Err(err) = config::store_path(path, &tunnel_config) {
        log::error!("cyberdesk_tunnel: failed to reset fingerprint: {err}");
    }
}

fn maybe_reset_fingerprint_from_env() {
    if matches!(
        std::env::var("CYBERDRIVER_RESET_FINGERPRINT"),
        Ok(value) if value == "1" || value.eq_ignore_ascii_case("true")
    ) {
        reset_fingerprint();
        std::env::remove_var("CYBERDRIVER_RESET_FINGERPRINT");
        log::info!("cyberdesk_tunnel: reset fingerprint from CYBERDRIVER_RESET_FINGERPRINT");
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
