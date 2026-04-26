// SPDX-License-Identifier: AGPL-3.0-only
//
// WebSocket tunnel client — opens the long-lived WSS to the cloud,
// reads framed requests, dispatches, sends framed responses.
//
// M4 baseline:
//   - Single attempt connect; if WS closes for any reason the future
//     ends (no reconnect). M7 adds reconnect with exponential backoff.
//   - No idempotency cache; M7 adds X-Idempotency-Key handling.
//   - Per-request body accumulated in a single Vec<u8>; M5+ may stream
//     for large payloads (screenshot/fs).
//
// The wire protocol is owned by `framing.rs`; this module just runs
// the WebSocket and the request<->response loop.

use super::framing::{RequestMeta, ResponseMeta};
use super::dispatch;

use futures_util::{SinkExt, StreamExt};
use hbb_common::anyhow::{anyhow, bail, Context, Result};
use hbb_common::log;
use std::collections::HashMap;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
};

/// X-Piglet-Version header value. Mirrors what cyberdriver.py used to
/// send so the cloud's logging + analytics treat this as a Cyberdriver
/// agent, not a foreign client.
const VERSION: &str = concat!("cyberdriver-rs/", env!("CARGO_PKG_VERSION"));

/// Open the tunnel and run the request loop until the WS closes.
pub async fn run(api_key: String, api_base: String, fingerprint: String) -> Result<()> {
    let url = format!("{}/tunnel/ws", api_base);
    log::info!(
        "cyberdesk_tunnel: connecting to {} as fingerprint {}",
        url,
        fingerprint
    );

    let mut request = url
        .as_str()
        .into_client_request()
        .context("building WebSocket client request")?;
    let headers = request.headers_mut();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", api_key))
            .map_err(|e| anyhow!("invalid Authorization header value: {e}"))?,
    );
    headers.insert(
        "x-piglet-fingerprint",
        HeaderValue::from_str(&fingerprint)
            .map_err(|e| anyhow!("invalid X-Piglet-Fingerprint header value: {e}"))?,
    );
    headers.insert("x-piglet-version", HeaderValue::from_static(VERSION));
    headers.insert(
        "x-piglet-hostname",
        HeaderValue::from_str(&hostname())
            .unwrap_or_else(|_| HeaderValue::from_static("cyberdriver")),
    );

    let (ws, response) = connect_async(request)
        .await
        .context("WebSocket handshake failed")?;
    log::info!(
        "cyberdesk_tunnel: connected (HTTP {})",
        response.status().as_u16()
    );

    let (mut write, mut read) = ws.split();

    // Per-request inbound state. Cloud sends:
    //   text(meta JSON)  ->  [binary chunks]  ->  text("end")
    let mut pending_meta: Option<RequestMeta> = None;
    let mut pending_body: Vec<u8> = Vec::new();

    while let Some(msg) = read.next().await {
        let msg = msg.context("WebSocket read error")?;

        match msg {
            Message::Text(text) => {
                let text_str = text.as_str();

                if text_str == "end" {
                    let meta = match pending_meta.take() {
                        Some(m) => m,
                        None => {
                            log::warn!(
                                "cyberdesk_tunnel: received 'end' with no pending request \
                                 metadata; ignoring"
                            );
                            continue;
                        }
                    };
                    let body = std::mem::take(&mut pending_body);

                    let (status, response_body, content_type) =
                        dispatch::dispatch(&meta, &body);

                    log::info!(
                        "cyberdesk_tunnel: {} {} -> {} ({} bytes, request_id={})",
                        meta.method,
                        meta.path,
                        status,
                        response_body.len(),
                        meta.request_id
                    );

                    let mut response_headers = HashMap::new();
                    response_headers
                        .insert("Content-Type".to_string(), content_type.to_string());
                    response_headers
                        .insert("Content-Length".to_string(), response_body.len().to_string());

                    let resp_meta = ResponseMeta {
                        status,
                        headers: response_headers,
                        request_id: meta.request_id,
                    };

                    let resp_meta_json = serde_json::to_string(&resp_meta)
                        .context("serializing response metadata")?;
                    write
                        .send(Message::Text(resp_meta_json.into()))
                        .await
                        .context("sending response metadata frame")?;

                    if !response_body.is_empty() {
                        write
                            .send(Message::Binary(response_body.into()))
                            .await
                            .context("sending response body binary frame")?;
                    }

                    write
                        .send(Message::Text("end".into()))
                        .await
                        .context("sending response 'end' sentinel")?;
                } else {
                    if pending_meta.is_some() {
                        log::warn!(
                            "cyberdesk_tunnel: new request metadata while previous request was \
                             still in flight; discarding partial body and starting over (cloud \
                             wire-protocol error)"
                        );
                        pending_body.clear();
                    }

                    let meta: RequestMeta = serde_json::from_str(text_str).with_context(|| {
                        format!("invalid request metadata JSON: {}", text_str)
                    })?;
                    pending_meta = Some(meta);
                }
            }

            Message::Binary(data) => {
                if pending_meta.is_none() {
                    log::warn!(
                        "cyberdesk_tunnel: received {} bytes binary data with no pending \
                         request metadata; dropping",
                        data.len()
                    );
                    continue;
                }
                pending_body.extend_from_slice(&data);
            }

            Message::Close(frame) => {
                if let Some(f) = &frame {
                    let code = u16::from(f.code);
                    log::info!(
                        "cyberdesk_tunnel: server closed connection: code={} reason={:?}",
                        code,
                        f.reason
                    );
                    // 4001 = auth (no retry), 4008 = rate limit
                    // (M7 will distinguish).
                    if let Err(err) = write.send(Message::Close(frame.clone())).await {
                        log::warn!("cyberdesk_tunnel: failed to send WebSocket close frame: {err}");
                    }
                    if code == 4001 {
                        bail!("cyberdesk_tunnel: server rejected auth (close 4001); refusing to retry");
                    }
                } else {
                    log::info!("cyberdesk_tunnel: server closed connection (no close frame)");
                    if let Err(err) = write.send(Message::Close(None)).await {
                        log::warn!("cyberdesk_tunnel: failed to send WebSocket close frame: {err}");
                    }
                }
                break;
            }

            Message::Ping(payload) => {
                // tungstenite auto-pongs by default but be explicit.
                let _ = write.send(Message::Pong(payload)).await;
            }

            Message::Pong(_) | Message::Frame(_) => {}
        }
    }

    log::info!("cyberdesk_tunnel: read loop ended");
    Ok(())
}

/// Best-effort local hostname for the X-Piglet-Hostname header. Used
/// only for cloud-side display (`Machine.hostname` column).
fn hostname() -> String {
    // Try standard env vars first (faster than syscalls); fall back
    // to a generic name. Real hostname() crate adds a dep we don't
    // need here.
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "cyberdriver".to_string())
}
