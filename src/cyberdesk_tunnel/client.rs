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

use super::{
    dispatch::{self, ReverseTunnelRequest},
    framing::{RequestMeta, ResponseMeta},
    path_without_query,
};

use futures_util::{Sink, SinkExt, StreamExt};
use hbb_common::anyhow::{anyhow, bail, Context, Result};
use hbb_common::{
    log,
    tokio::{
        self,
        sync::{mpsc, OwnedSemaphorePermit, Semaphore},
    },
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
};

/// X-Piglet-Version header value. Mirrors what cyberdriver.py used to
/// send so the cloud's logging + analytics treat this as a Cyberdriver
/// agent, not a foreign client.
const VERSION: &str = concat!("cyberdriver-rs/", env!("CARGO_PKG_VERSION"));
const MAX_REQUEST_BODY_BYTES: usize = 150 * 1024 * 1024;
const MAX_IN_FLIGHT_DISPATCHES: usize = 4;
const MAX_IDEMPOTENCY_ENTRIES: usize = 128;
const MAX_IDEMPOTENCY_BODY_BYTES: usize = 2 * 1024 * 1024;

pub(super) fn dispatch_semaphore() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(MAX_IN_FLIGHT_DISPATCHES))
}

/// Open the tunnel and run the request loop until the WS closes.
pub async fn run(
    api_key: String,
    api_base: String,
    fingerprint: String,
    machine_name: Option<String>,
    dispatch_semaphore: Arc<Semaphore>,
) -> Result<()> {
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
    if let Some(machine_name) = machine_name.as_deref() {
        headers.insert(
            "x-cyberdriver-name",
            HeaderValue::from_str(machine_name)
                .map_err(|e| anyhow!("invalid X-Cyberdriver-Name header value: {e}"))?,
        );
    }

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
    let mut pending_error: Option<(u16, Vec<u8>, &'static str)> = None;
    let mut idempotency_cache = IdempotencyCache::default();
    let (response_tx, mut response_rx) = mpsc::unbounded_channel();

    loop {
        let msg = tokio::select! {
            maybe_msg = read.next() => match maybe_msg {
                Some(msg) => msg.context("WebSocket read error")?,
                None => break,
            },
            maybe_response = response_rx.recv() => {
                let Some(response) = maybe_response else {
                    break;
                };
                if let Some((key, response)) = send_response(&mut write, response).await? {
                    idempotency_cache.insert(key, &response);
                }
                continue;
            }
        };

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
                    let method = meta.method.clone();
                    let log_path = log_path(&meta.path);
                    let request_id = meta.request_id.clone();
                    let idempotency_key = idempotency_cache_key(&meta, &body);
                    let (response, response_idempotency_key) = match pending_error.take() {
                        Some(response) => (response, None),
                        None => match idempotency_key
                            .as_deref()
                            .and_then(|key| idempotency_cache.get(key))
                        {
                            Some(response) => (response, idempotency_key),
                            None => {
                                let dispatch_idempotency_key =
                                    idempotency_key.as_deref().map(ToOwned::to_owned);
                                match dispatch_semaphore.clone().try_acquire_owned() {
                                    Ok(permit) => {
                                        spawn_dispatch(
                                            meta,
                                            body,
                                            dispatch_idempotency_key,
                                            response_tx.clone(),
                                            permit,
                                        );
                                        continue;
                                    }
                                    Err(_) => (too_many_in_flight_response(), None),
                                }
                            }
                        },
                    };

                    send_response(
                        &mut write,
                        DispatchResponse {
                            method,
                            log_path,
                            request_id,
                            idempotency_key: response_idempotency_key,
                            response,
                        },
                    )
                    .await?;
                } else {
                    if pending_meta.is_some() {
                        log::warn!(
                            "cyberdesk_tunnel: new request metadata while previous request was \
                             still in flight; discarding partial body and starting over (cloud \
                             wire-protocol error)"
                        );
                        pending_meta = None;
                        pending_body.clear();
                        pending_error = None;
                    }

                    let meta: RequestMeta = match serde_json::from_str(text_str) {
                        Ok(meta) => meta,
                        Err(err) => {
                            log::warn!(
                                "cyberdesk_tunnel: invalid request metadata JSON: {}; \
                                 ignoring ({err})",
                                text_str
                            );
                            continue;
                        }
                    };
                    pending_meta = Some(meta);
                    pending_error = None;
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
                if pending_error.is_some() {
                    continue;
                }
                if pending_body.len().saturating_add(data.len()) > MAX_REQUEST_BODY_BYTES {
                    log::warn!(
                        "cyberdesk_tunnel: request body exceeded {} byte limit; dropping body",
                        MAX_REQUEST_BODY_BYTES
                    );
                    pending_body.clear();
                    pending_error = Some(request_body_too_large_response());
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
                    if let Err(err) = write.send(Message::Close(frame.clone())).await {
                        log::warn!("cyberdesk_tunnel: failed to send WebSocket close frame: {err}");
                    }
                    if code == 4001 || code == 403 {
                        bail!(
                            "cyberdesk_tunnel: server rejected auth (close {code}); refusing to retry"
                        );
                    }
                    if code == 4008 {
                        let retry_after = retry_after_from_reason(f.reason.as_ref()).unwrap_or(16);
                        bail!(
                            "cyberdesk_tunnel: server rate limited connection (close 4008; retry-after={retry_after})"
                        );
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

type ResponseTuple = (u16, Vec<u8>, &'static str);

struct DispatchResponse {
    method: String,
    log_path: String,
    request_id: String,
    idempotency_key: Option<String>,
    response: ResponseTuple,
}

fn spawn_dispatch(
    meta: RequestMeta,
    body: Vec<u8>,
    idempotency_key: Option<String>,
    response_tx: mpsc::UnboundedSender<DispatchResponse>,
    permit: OwnedSemaphorePermit,
) {
    let method = meta.method.clone();
    let log_path = log_path(&meta.path);
    let request_id = meta.request_id.clone();
    tokio::spawn(async move {
        let response = run_dispatch(meta, body).await;
        let _ = response_tx.send(DispatchResponse {
            method,
            log_path,
            request_id,
            idempotency_key,
            response,
        });
        drop(permit);
    });
}

async fn send_response<W>(
    write: &mut W,
    dispatch_response: DispatchResponse,
) -> Result<Option<(String, ResponseTuple)>>
where
    W: Sink<Message> + Unpin,
    <W as Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    let DispatchResponse {
        method,
        log_path,
        request_id,
        idempotency_key,
        response,
    } = dispatch_response;
    let (status, response_body, content_type) = response;
    let cached_response = idempotency_key
        .as_ref()
        .map(|_| (status, response_body.clone(), content_type));

    log::info!(
        "cyberdesk_tunnel: {} {} -> {} ({} bytes, request_id={})",
        method,
        log_path,
        status,
        response_body.len(),
        request_id
    );

    let mut response_headers = HashMap::new();
    response_headers.insert("Content-Type".to_string(), content_type.to_string());
    response_headers.insert(
        "Content-Length".to_string(),
        response_body.len().to_string(),
    );

    let resp_meta = ResponseMeta {
        status,
        headers: response_headers,
        request_id,
    };

    let resp_meta_json =
        serde_json::to_string(&resp_meta).context("serializing response metadata")?;
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

    Ok(idempotency_key.zip(cached_response))
}

async fn run_dispatch(meta: RequestMeta, body: Vec<u8>) -> ResponseTuple {
    match tokio::task::spawn_blocking(move || {
        let request = ReverseTunnelRequest::from_websocket_frames(&meta, &body);
        dispatch::dispatch(request)
    })
    .await
    {
        Ok(response) => response,
        Err(err) => {
            log::error!("cyberdesk_tunnel: dispatch task failed: {err}");
            (
                500,
                br#"{"error":"cyberdesk_tunnel dispatch failed"}"#.to_vec(),
                "application/json",
            )
        }
    }
}

#[derive(Default)]
struct IdempotencyCache {
    order: VecDeque<String>,
    entries: HashMap<String, (u16, Vec<u8>, &'static str)>,
}

impl IdempotencyCache {
    fn get(&self, key: &str) -> Option<(u16, Vec<u8>, &'static str)> {
        self.entries.get(key).cloned()
    }

    fn insert(&mut self, key: String, response: &(u16, Vec<u8>, &'static str)) {
        if response.0 >= 500 {
            return;
        }
        if response.1.len() > MAX_IDEMPOTENCY_BODY_BYTES {
            return;
        }
        if !self.entries.contains_key(&key) {
            self.order.push_back(key.clone());
        }
        self.entries.insert(key, response.clone());
        while self.entries.len() > MAX_IDEMPOTENCY_ENTRIES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
    }
}

fn idempotency_cache_key(meta: &RequestMeta, body: &[u8]) -> Option<String> {
    let raw_key = header_value(&meta.headers, "x-idempotency-key")
        .or_else(|| header_value(&meta.headers, "idempotency-key"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)?;

    let mut hasher = Sha256::new();
    hasher.update(meta.method.as_bytes());
    hasher.update([0]);
    hasher.update(meta.path.as_bytes());
    hasher.update([0]);
    let query = serde_json::to_vec(&meta.query).unwrap_or_default();
    hasher.update(query);
    hasher.update([0]);
    hasher.update(body);
    let request_hash = hex::encode(hasher.finalize());

    Some(format!("{raw_key}:{request_hash}"))
}

fn header_value<'a>(headers: &'a Value, name: &str) -> Option<&'a str> {
    let map = headers.as_object()?;
    map.iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| value.as_str())
}

fn retry_after_from_reason(reason: &str) -> Option<u64> {
    for token in reason.split(|ch: char| ch == ';' || ch == ',' || ch.is_whitespace()) {
        let value = token
            .strip_prefix("retry-after=")
            .or_else(|| token.strip_prefix("Retry-After="))?;
        if let Ok(seconds) = value.parse::<u64>() {
            return Some(seconds.clamp(1, 60));
        }
    }
    None
}

/// Best-effort local hostname for the X-Piglet-Hostname header. Used
/// only for cloud-side display (`Machine.hostname` column).
fn hostname() -> String {
    crate::common::hostname()
}

fn request_body_too_large_response() -> (u16, Vec<u8>, &'static str) {
    (
        413,
        format!(
            r#"{{"error":"request body exceeds {} byte limit"}}"#,
            MAX_REQUEST_BODY_BYTES
        )
        .into_bytes(),
        "application/json",
    )
}

fn too_many_in_flight_response() -> (u16, Vec<u8>, &'static str) {
    log::warn!(
        "cyberdesk_tunnel: rejecting request because {} dispatches are already in flight",
        MAX_IN_FLIGHT_DISPATCHES
    );
    (
        429,
        format!(
            r#"{{"error":"too many in-flight tunnel requests (limit {})"}}"#,
            MAX_IN_FLIGHT_DISPATCHES
        )
        .into_bytes(),
        "application/json",
    )
}

fn log_path(path: &str) -> String {
    let route = path_without_query(path);
    if is_filesystem_route(route) {
        return route.to_string();
    }
    path.to_string()
}

fn is_filesystem_route(route: &str) -> bool {
    matches!(
        route,
        "/computer/fs/list" | "/computer/fs/read" | "computer/fs/list" | "computer/fs/read"
    )
}

#[cfg(test)]
mod tests {
    use super::log_path;

    #[test]
    fn log_path_redacts_filesystem_query() {
        assert_eq!(
            log_path("/computer/fs/read?path=/home/alice/.ssh/id_rsa"),
            "/computer/fs/read"
        );
    }

    #[test]
    fn log_path_redacts_legacy_filesystem_query() {
        assert_eq!(
            log_path("computer/fs/list?path=/home/alice"),
            "computer/fs/list"
        );
    }

    #[test]
    fn log_path_preserves_non_filesystem_query() {
        assert_eq!(
            log_path("/computer/display/screenshot?format=png"),
            "/computer/display/screenshot?format=png"
        );
    }
}
