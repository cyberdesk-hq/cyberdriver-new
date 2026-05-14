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
use hbb_common::anyhow::{anyhow, Context, Error, Result};
use hbb_common::{
    config::Config,
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
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{HeaderValue, StatusCode},
        Error as WsError, Message,
    },
};

/// X-Piglet-Version header value. Keep this as a plain semantic version so
/// Cyberdesk can compare legacy Python agents and the RustDesk-based client
/// without string-prefix special cases.
const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_REQUEST_BODY_BYTES: usize = 150 * 1024 * 1024;
const MAX_IN_FLIGHT_DISPATCHES: usize = 4;
const MAX_IDEMPOTENCY_ENTRIES: usize = 128;
const MAX_IDEMPOTENCY_BODY_BYTES: usize = 2 * 1024 * 1024;
const PING_INTERVAL: Duration = Duration::from_secs(20);
const PONG_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

#[derive(Debug)]
pub(super) struct RateLimited {
    retry_after: Duration,
}

impl RateLimited {
    fn new(retry_after: Duration) -> Self {
        Self { retry_after }
    }

    pub(super) fn retry_after(&self) -> Duration {
        self.retry_after
    }
}

impl fmt::Display for RateLimited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cyberdesk_tunnel: server rate limited connection (close 4008; retry-after={})",
            self.retry_after.as_secs()
        )
    }
}

impl std::error::Error for RateLimited {}

#[derive(Debug)]
pub(super) struct AuthRejected {
    code: u16,
}

impl AuthRejected {
    fn new(code: u16) -> Self {
        Self { code }
    }
}

impl fmt::Display for AuthRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cyberdesk_tunnel: server rejected auth (close {}); refusing to retry",
            self.code
        )
    }
}

impl std::error::Error for AuthRejected {}

#[derive(Debug)]
pub(super) struct MachineLimitReached {
    reason: String,
}

impl MachineLimitReached {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for MachineLimitReached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cyberdesk_tunnel: machine limit reached; {}",
            self.reason
        )
    }
}

impl std::error::Error for MachineLimitReached {}

#[derive(Debug, PartialEq, Eq)]
enum CloseFrameDecision {
    Reconnect,
    AuthRejected,
    RateLimited(Duration),
    MachineLimitReached(String),
}

#[derive(Debug)]
pub(super) struct ConnectedTunnelError {
    connected_for: Duration,
    source: Error,
}

impl ConnectedTunnelError {
    fn new(connected_for: Duration, source: Error) -> Self {
        Self {
            connected_for,
            source,
        }
    }

    pub(super) fn connected_for(&self) -> Duration {
        self.connected_for
    }
}

impl fmt::Display for ConnectedTunnelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cyberdesk_tunnel: connected tunnel failed after {:.1}s: {}",
            self.connected_for.as_secs_f64(),
            self.source
        )
    }
}

impl std::error::Error for ConnectedTunnelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

pub(super) fn is_non_retryable_auth_error(error: &Error) -> bool {
    if error.downcast_ref::<AuthRejected>().is_some() {
        return true;
    }
    if error.downcast_ref::<MachineLimitReached>().is_some() {
        return true;
    }

    matches!(
        error.downcast_ref::<WsError>(),
        Some(WsError::Http(response))
            if response.status() == StatusCode::UNAUTHORIZED
                || response.status() == StatusCode::FORBIDDEN
    )
}

pub(super) fn connected_for_error(error: &Error) -> Option<Duration> {
    error
        .downcast_ref::<ConnectedTunnelError>()
        .map(|error| error.connected_for())
}

pub(super) fn dispatch_semaphore() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(MAX_IN_FLIGHT_DISPATCHES))
}

/// Open the tunnel and run the request loop until the WS closes.
pub async fn run(
    api_key: String,
    api_base: String,
    fingerprint: String,
    machine_name: Option<String>,
    remote_keepalive_for: Option<String>,
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
        "x-cyberdriver-peer-id",
        HeaderValue::from_str(&Config::get_id())
            .map_err(|e| anyhow!("invalid X-Cyberdriver-Peer-Id header value: {e}"))?,
    );
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
    if let Some(remote_keepalive_for) = remote_keepalive_for.as_deref() {
        headers.insert(
            "x-remote-keepalive-for",
            HeaderValue::from_str(remote_keepalive_for)
                .map_err(|e| anyhow!("invalid X-Remote-Keepalive-For header value: {e}"))?,
        );
    }

    let (ws, response) = connect_async(request)
        .await
        .map_err(websocket_handshake_error)?;
    log::info!(
        "cyberdesk_tunnel: connected (HTTP {})",
        response.status().as_u16()
    );
    super::mark_tunnel_connected();

    let (mut write, mut read) = ws.split();
    let connected_at = Instant::now();
    let mut ping_interval = tokio::time::interval(PING_INTERVAL);
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping_interval.tick().await;
    let mut ping_state = PingState::default();

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
                Some(msg) => msg.map_err(|err| connected_error(connected_at, anyhow!("WebSocket read error: {err}")))?,
                None => break,
            },
            maybe_response = response_rx.recv() => {
                let Some(response) = maybe_response else {
                    break;
                };
                if let Some((key, response)) = send_response(&mut write, response)
                    .await
                    .map_err(|err| connected_error(connected_at, err))?
                {
                    idempotency_cache.insert(key, &response);
                }
                continue;
            },
            _ = ping_interval.tick() => {
                let now = Instant::now();
                if ping_state.timed_out(now, PONG_TIMEOUT) {
                    return Err(connected_error(
                        connected_at,
                        anyhow!(
                            "WebSocket pong timed out after {}s",
                            PONG_TIMEOUT.as_secs()
                        ),
                    ));
                }
                if !ping_state.awaiting_pong() {
                    write
                        .send(Message::Ping(Vec::new().into()))
                        .await
                        .map_err(|err| connected_error(connected_at, anyhow!("sending WebSocket ping failed: {err}")))?;
                    ping_state.mark_sent(now);
                    log::debug!("cyberdesk_tunnel: WebSocket ping sent");
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
                    .await
                    .map_err(|err| connected_error(connected_at, err))?;
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
                    match classify_close_frame(code, f.reason.as_ref()) {
                        CloseFrameDecision::Reconnect => {}
                        CloseFrameDecision::AuthRejected => {
                            return Err(AuthRejected::new(code).into());
                        }
                        CloseFrameDecision::MachineLimitReached(reason) => {
                            return Err(MachineLimitReached::new(reason).into());
                        }
                        CloseFrameDecision::RateLimited(retry_after) => {
                            return Err(RateLimited::new(retry_after).into());
                        }
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

            Message::Pong(_) => {
                ping_state.mark_pong_received();
                log::debug!("cyberdesk_tunnel: WebSocket pong received");
            }
            Message::Frame(_) => {}
        }
    }

    log::info!("cyberdesk_tunnel: read loop ended");
    Ok(())
}

fn classify_close_frame(code: u16, reason: &str) -> CloseFrameDecision {
    match code {
        4001 => CloseFrameDecision::AuthRejected,
        4008 => CloseFrameDecision::RateLimited(
            retry_after_from_reason(reason).unwrap_or_else(|| Duration::from_secs(60)),
        ),
        4009 => CloseFrameDecision::MachineLimitReached(reason.to_string()),
        _ => CloseFrameDecision::Reconnect,
    }
}

fn websocket_handshake_error(err: WsError) -> Error {
    match &err {
        WsError::Http(response) => {
            let status = response.status();
            let body = response
                .body()
                .as_ref()
                .map(|body| String::from_utf8_lossy(body).trim().to_string())
                .filter(|body| !body.is_empty())
                .unwrap_or_default();
            if body.is_empty() {
                anyhow!("WebSocket handshake failed: HTTP {}", status)
            } else {
                anyhow!("WebSocket handshake failed: HTTP {}: {}", status, body)
            }
        }
        _ => anyhow!("WebSocket handshake failed: {err}"),
    }
}

fn connected_error(connected_at: Instant, source: Error) -> Error {
    ConnectedTunnelError::new(connected_at.elapsed(), source).into()
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

fn retry_after_from_reason(reason: &str) -> Option<Duration> {
    let mut wait_window = 0_u8;
    let mut retry_after_window = 0_u8;
    for token in reason.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '=')) {
        if token.is_empty() {
            continue;
        }
        if let Some(value) = token
            .strip_prefix("retry-after=")
            .or_else(|| token.strip_prefix("Retry-After="))
        {
            if let Ok(seconds) = value.parse::<u64>() {
                return Some(bounded_retry_after(seconds));
            }
            continue;
        }
        if token.eq_ignore_ascii_case("retry-after") {
            retry_after_window = 3;
            continue;
        }
        if token.eq_ignore_ascii_case("wait") {
            wait_window = 3;
            continue;
        }
        if wait_window > 0 || retry_after_window > 0 {
            if let Ok(seconds) = token.parse::<u64>() {
                return Some(bounded_retry_after(seconds));
            }
            wait_window = wait_window.saturating_sub(1);
            retry_after_window = retry_after_window.saturating_sub(1);
        }
    }
    None
}

fn bounded_retry_after(seconds: u64) -> Duration {
    Duration::from_secs(seconds.clamp(1, MAX_RETRY_AFTER.as_secs()))
}

#[derive(Debug, Default)]
struct PingState {
    sent_at: Option<Instant>,
}

impl PingState {
    fn awaiting_pong(&self) -> bool {
        self.sent_at.is_some()
    }

    fn mark_sent(&mut self, now: Instant) {
        self.sent_at = Some(now);
    }

    fn mark_pong_received(&mut self) {
        self.sent_at = None;
    }

    fn timed_out(&self, now: Instant, timeout: Duration) -> bool {
        self.sent_at
            .map(|sent_at| now.duration_since(sent_at) >= timeout)
            .unwrap_or(false)
    }
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
    use super::{
        classify_close_frame, connected_for_error, is_non_retryable_auth_error, log_path,
        retry_after_from_reason, AuthRejected, CloseFrameDecision, ConnectedTunnelError,
        MachineLimitReached, PingState, MAX_RETRY_AFTER,
    };
    use hbb_common::anyhow::anyhow;
    use std::time::{Duration, Instant};
    use tokio_tungstenite::tungstenite::{
        http::{Response, StatusCode},
        Error as WsError,
    };

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

    #[test]
    fn retry_after_parses_retry_after_token() {
        assert_eq!(
            retry_after_from_reason("rate limited; retry-after=42"),
            Some(Duration::from_secs(42))
        );
        assert_eq!(
            retry_after_from_reason("rate limited; Retry-After: 43"),
            Some(Duration::from_secs(43))
        );
    }

    #[test]
    fn retry_after_parses_server_wait_reason() {
        assert_eq!(
            retry_after_from_reason(
                "Rate limited: Too many reconnection attempts. Wait 60 seconds."
            ),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            retry_after_from_reason(
                "Rate limited: Too many reconnection attempts. Wait for 61 seconds."
            ),
            Some(Duration::from_secs(61))
        );
        assert_eq!(
            retry_after_from_reason("Rate limited. Retry-After: approximately 62 seconds."),
            Some(Duration::from_secs(62))
        );
    }

    #[test]
    fn retry_after_bounds_extreme_values() {
        assert_eq!(
            retry_after_from_reason("retry-after=0"),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            retry_after_from_reason("retry-after=999999"),
            Some(MAX_RETRY_AFTER)
        );
    }

    #[test]
    fn ping_state_times_out_until_pong_received() {
        let start = Instant::now();
        let mut state = PingState::default();
        assert!(!state.awaiting_pong());
        assert!(!state.timed_out(start + Duration::from_secs(30), Duration::from_secs(20)));

        state.mark_sent(start);
        assert!(state.awaiting_pong());
        assert!(!state.timed_out(start + Duration::from_secs(19), Duration::from_secs(20)));
        assert!(state.timed_out(start + Duration::from_secs(20), Duration::from_secs(20)));

        state.mark_pong_received();
        assert!(!state.awaiting_pong());
        assert!(!state.timed_out(start + Duration::from_secs(60), Duration::from_secs(20)));
    }

    #[test]
    fn close_frame_1001_reconnects_after_service_restart() {
        assert_eq!(
            classify_close_frame(1001, "Server restarting"),
            CloseFrameDecision::Reconnect
        );
    }

    #[test]
    fn close_frame_4008_uses_server_wait_duration() {
        assert_eq!(
            classify_close_frame(
                4008,
                "Rate limited: Too many reconnection attempts. Wait 60 seconds."
            ),
            CloseFrameDecision::RateLimited(Duration::from_secs(60))
        );
    }

    #[test]
    fn close_frame_4001_and_4009_are_non_retryable() {
        assert_eq!(
            classify_close_frame(4001, "Invalid or expired API key"),
            CloseFrameDecision::AuthRejected
        );
        assert_eq!(
            classify_close_frame(4009, "machine limit reached"),
            CloseFrameDecision::MachineLimitReached("machine limit reached".to_string())
        );
    }

    #[test]
    fn auth_and_machine_limit_errors_are_non_retryable() {
        let auth_error = anyhow!(AuthRejected::new(4001));
        let limit_error = anyhow!(MachineLimitReached::new("machine limit reached"));
        assert!(is_non_retryable_auth_error(&auth_error));
        assert!(is_non_retryable_auth_error(&limit_error));
    }

    #[test]
    fn http_401_and_403_handshake_failures_are_non_retryable() {
        let unauthorized = anyhow!(WsError::Http(
            Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(None::<Vec<u8>>)
                .expect("test response should build")
        ));
        let forbidden = anyhow!(WsError::Http(
            Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(None::<Vec<u8>>)
                .expect("test response should build")
        ));
        assert!(is_non_retryable_auth_error(&unauthorized));
        assert!(is_non_retryable_auth_error(&forbidden));
    }

    #[test]
    fn connected_transport_errors_preserve_stable_duration() {
        let error = anyhow!(ConnectedTunnelError::new(
            Duration::from_secs(12),
            anyhow!("transport reset")
        ));
        assert_eq!(connected_for_error(&error), Some(Duration::from_secs(12)));
    }
}
