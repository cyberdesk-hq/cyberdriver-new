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

use hbb_common::log;

mod client;
mod dispatch;
mod framing;

/// Entry point called from `src/server.rs::start_server` during
/// service-mode bootstrap. Non-blocking — spawns a background task on
/// RustDesk's existing tokio runtime and returns immediately.
///
/// Behavior is controlled by env vars (M4 baseline; M7 will move
/// these to LocalConfig so the Settings UI can edit them at runtime):
///
/// | Var                       | Meaning                                              |
/// |---------------------------|------------------------------------------------------|
/// | `CYBERDESK_AGENT_KEY`     | Required `ak_*`. Without it, this function no-ops.   |
/// | `CYBERDESK_API_BASE`      | Tunnel WS base URL. Default `ws://localhost:8080`.   |
/// | `CYBERDESK_FINGERPRINT`   | Stable machine UUID. Default: random per-run.        |
///
/// If `CYBERDESK_AGENT_KEY` is unset, the tunnel does not start. This
/// is the correct default for client-mode installs (the laptop case)
/// and for any build that doesn't want Cyberdesk control.
pub fn spawn_if_enabled() {
    let api_key = match std::env::var("CYBERDESK_AGENT_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            log::info!(
                "cyberdesk_tunnel: CYBERDESK_AGENT_KEY not set; tunnel disabled (this is fine \
                 for client-mode installs)"
            );
            return;
        }
    };

    let api_base = std::env::var("CYBERDESK_API_BASE")
        .unwrap_or_else(|_| "ws://localhost:8080".to_string());

    let fingerprint = std::env::var("CYBERDESK_FINGERPRINT")
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());

    log::info!(
        "cyberdesk_tunnel: spawning tunnel client (api_base={}, fingerprint={})",
        api_base,
        fingerprint
    );

    // Schedule onto RustDesk's existing tokio runtime via hbb_common's
    // re-export. We deliberately do NOT create a new runtime here.
    hbb_common::tokio::spawn(async move {
        match client::run(api_key, api_base, fingerprint).await {
            Ok(()) => log::info!("cyberdesk_tunnel: client exited cleanly"),
            Err(e) => log::error!("cyberdesk_tunnel: client exited with error: {e:?}"),
        }
    });
}
