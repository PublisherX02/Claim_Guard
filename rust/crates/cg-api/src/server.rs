//! The thin axum adapter: bytes in, `Api::handle` on a blocking thread, bytes out. No rule lives here.

use crate::{body_limit, Api, ApiRequest, ApiResponse};
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderName, HeaderValue, Response, StatusCode};
use axum::Router;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
struct App {
    api: Arc<Api>,
    behind_proxy: bool,
    streams: Arc<Streams>,
}

/// The client address used for the login throttle: the socket peer, or, only behind our own reverse proxy, the last hop it appended.
fn client_of(peer: &SocketAddr, headers: &HashMap<String, String>, behind_proxy: bool) -> String {
    if behind_proxy {
        if let Some(last) = headers.get("x-forwarded-for").and_then(|v| v.rsplit(',').next()).map(str::trim).filter(|s| !s.is_empty() && s.len() <= 64) {
            return last.to_string();
        }
    }
    peer.ip().to_string()
}

fn plain(status: u16, code: &str) -> Response<Body> {
    Response::builder().status(status).header("content-type", "application/json").header("cache-control", "no-store").body(Body::from(format!("{{\"error\": \"{code}\"}}"))).unwrap_or_default()
}

fn into_response(r: ApiResponse) -> Response<Body> {
    let mut b = Response::builder().status(StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
    for (n, v) in r.headers {
        if let (Ok(n), Ok(v)) = (HeaderName::from_bytes(n.as_bytes()), HeaderValue::from_str(&v)) {
            b = b.header(n, v);
        }
    }
    b.body(Body::from(r.body)).unwrap_or_default()
}

// ---- live updates (server-sent events) -----------------------------------------------------------------------------------------
// A console page that shows an inbox or the queue overview opens GET /api/v1/work/events?topics=inbox|queue. The stream carries no claim
// data at all, only the words "changed" and "end": every few seconds the server asks the ordinary, authenticated route
// /api/v1/work/changes (with the caller's own cookie) for a fingerprint of what that page shows, and says "changed" when the fingerprint
// differs. The page then re-reads the real data through the usual routes, so permissions, masking and the session check live in one place
// and a stream can never show anyone more than they could already fetch. A revoked or expired session ends the stream. The connection
// lifetime limit below also ends it after five minutes; the browser reconnects by itself and the first message ("hello") makes the page
// read once, so nothing that happened in the gap is missed.
pub const EVENTS_PATH: &str = "/api/v1/work/events";
const CHANGES_PATH: &str = "/api/v1/work/changes";
pub const STREAM_TICK: Duration = Duration::from_secs(3);
pub const STREAM_KEEPALIVE: Duration = Duration::from_secs(15);
pub const MAX_STREAMS: usize = 256;
pub const MAX_STREAMS_PER_CLIENT: usize = 16; // per address: several people may share one office address, and one person may have two tabs open

#[derive(Default)]
struct Streams {
    per_client: std::sync::Mutex<HashMap<String, usize>>,
    total: std::sync::atomic::AtomicUsize,
}

struct StreamSlot {
    streams: Arc<Streams>,
    client: String,
}

impl Streams {
    fn acquire(self: &Arc<Self>, client: &str) -> Option<StreamSlot> {
        use std::sync::atomic::Ordering::SeqCst;
        let mut m = self.per_client.lock().unwrap_or_else(|e| e.into_inner());
        if self.total.load(SeqCst) >= MAX_STREAMS || m.get(client).copied().unwrap_or(0) >= MAX_STREAMS_PER_CLIENT {
            return None;
        }
        *m.entry(client.to_string()).or_insert(0) += 1;
        self.total.fetch_add(1, SeqCst);
        Some(StreamSlot { streams: self.clone(), client: client.to_string() })
    }

    #[cfg(test)]
    fn open(&self) -> usize {
        self.total.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut m = self.streams.per_client.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = m.get_mut(&self.client) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.client);
            }
        }
        self.streams.total.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A response body fed from a channel: whatever the stream task sends is written to the client at once.
struct EventBody(tokio::sync::mpsc::Receiver<Bytes>);

impl hyper::body::Body for EventBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        self.0.poll_recv(cx).map(|o| o.map(|b| Ok(hyper::body::Frame::data(b))))
    }
}

async fn changes_call(api: Arc<Api>, headers: HashMap<String, String>, query: String, client: String, quiet: bool) -> Option<ApiResponse> {
    let request = ApiRequest { method: "GET".into(), path: CHANGES_PATH.into(), query, headers, body: vec![], client };
    tokio::task::spawn_blocking(move || if quiet { api.handle_quiet(request) } else { api.handle(request) }).await.ok()
}

fn signature_of(r: &ApiResponse) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(&r.body).ok().and_then(|v| v["signature"].as_str().map(str::to_string))
}

/// What the watcher tells a client: the same three things on every transport (server-sent events or WebSocket).
enum Note {
    Changed,
    KeepAlive,
    End,
}

/// Asks the ordinary, authenticated change route every few seconds and reports through `send` (false = the client is gone or does not read, so stop).
/// `closed` says the client went away between two ticks. The slot is held until this returns, so it is released when the session ends or the client leaves.
async fn watch(api: Arc<Api>, headers: HashMap<String, String>, query: String, client: String, mut last: Option<String>, slot: StreamSlot, closed: impl Fn() -> bool, send: impl Fn(Note) -> bool) {
    let _slot = slot;
    let mut quiet_for = Duration::ZERO;
    loop {
        tokio::time::sleep(STREAM_TICK).await;
        if closed() {
            break;
        }
        let Some(r) = changes_call(api.clone(), headers.clone(), query.clone(), client.clone(), true).await else { continue };
        match r.status {
            200 => {
                let now = signature_of(&r);
                if now != last {
                    last = now;
                    quiet_for = Duration::ZERO;
                    if !send(Note::Changed) {
                        break; // a client that does not read is dropped, not waited for
                    }
                } else {
                    quiet_for += STREAM_TICK;
                    if quiet_for >= STREAM_KEEPALIVE {
                        quiet_for = Duration::ZERO;
                        if !send(Note::KeepAlive) {
                            break;
                        }
                    }
                }
            }
            401 | 403 => {
                let _ = send(Note::End);
                break;
            }
            _ => {} // a passing failure (the database was busy): ask again at the next tick
        }
    }
}

/// The headers of the first (ordinary) answer that a stream response keeps: no-store, no sniffing, and so on.
fn kept_headers(first: &ApiResponse, mut out: axum::http::response::Builder) -> axum::http::response::Builder {
    for (n, v) in &first.headers {
        if !n.eq_ignore_ascii_case("content-type") && !n.eq_ignore_ascii_case("content-length") {
            if let (Ok(n), Ok(v)) = (HeaderName::from_bytes(n.as_bytes()), HeaderValue::from_str(v)) {
                out = out.header(n, v);
            }
        }
    }
    out
}

async fn events(app: App, headers: HashMap<String, String>, query: String, client: String) -> Response<Body> {
    // The first check is an ordinary request: a missing or bad session, a missing permission or a bad topic is answered as a normal error
    // and no stream is opened.
    let Some(first) = changes_call(app.api.clone(), headers.clone(), query.clone(), client.clone(), false).await else { return plain(500, "server_error") };
    if first.status != 200 {
        return into_response(first);
    }
    let Some(slot) = app.streams.acquire(&client) else { return plain(429, "too_many_streams") };
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(8);
    let _ = tx.try_send(Bytes::from_static(b"retry: 3000\nevent: hello\ndata: {}\n\n"));
    let out = kept_headers(&first, Response::builder().status(200)).header("content-type", "text/event-stream; charset=utf-8").header("x-accel-buffering", "no");
    let (api, last) = (app.api.clone(), signature_of(&first));
    let tx2 = tx.clone();
    tokio::spawn(watch(api, headers, query, client, last, slot, move || tx.is_closed(), move |n| {
        tx2.try_send(match n {
            Note::Changed => Bytes::from_static(b"event: changed\ndata: {}\n\n"),
            Note::KeepAlive => Bytes::from_static(b": keep-alive\n\n"),
            Note::End => Bytes::from_static(b"event: end\ndata: {\"reason\": \"session\"}\n\n"),
        })
        .is_ok()
    }));
    out.body(Body::new(EventBody(rx))).unwrap_or_default()
}

// ---- live updates over a WebSocket ------------------------------------------------------------------------------------------------
// The same watcher as the event stream, on a second transport: GET /api/v1/work/ws?topics=... with an upgrade handshake (RFC 6455). The messages are the
// same three words as text frames ({"event":"hello"}, {"event":"changed"}, {"event":"end"}), so a socket carries no claim data either, and the page
// re-reads through the ordinary routes. What is different, and checked here: a browser sends the Origin of the page that opened the socket and a
// WebSocket is not covered by the same-origin policy, so a socket whose Origin is not this site is refused (a page on another site must not be able to open
// one with the visitor's cookie); the first check is the same ordinary authenticated request; the same limits apply (the stream slots, five minutes);
// the client's frames must be masked and are only read for ping, pong and close; nothing else is accepted from the client.
pub const WS_PATH: &str = "/api/v1/work/ws";
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const WS_LIFETIME: Duration = Duration::from_secs(300);
const WS_MAX_CLIENT_PAYLOAD: u64 = 1024;

fn ws_accept_key(key: &str) -> String {
    use base64::Engine;
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(key.as_bytes());
    h.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(h.finalize())
}

/// A page's Origin is acceptable only when it is this site: scheme http or https and the same host (and port) as the Host header the request came in on.
fn ws_origin_ok(origin: &str, host: &str) -> bool {
    let rest = origin.strip_prefix("https://").or_else(|| origin.strip_prefix("http://"));
    matches!(rest, Some(r) if !host.is_empty() && r.eq_ignore_ascii_case(host))
}

/// One unmasked server frame (the payloads here are short: a few words, a ping, a close code).
fn ws_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0x80 | opcode, payload.len().min(125) as u8];
    f.extend_from_slice(&payload[..payload.len().min(125)]);
    f
}

fn ws_close_frame(code: u16) -> Vec<u8> {
    ws_frame(0x8, &code.to_be_bytes())
}

enum ClientFrame {
    Ping(Vec<u8>),
    Close,
    Ignored,
}

/// Reads one client frame. Err(code) is a protocol violation to answer with that close code; Err(0) means the connection ended.
async fn ws_read_frame<R: tokio::io::AsyncRead + Unpin>(rd: &mut R) -> Result<ClientFrame, u16> {
    use tokio::io::AsyncReadExt;
    let mut head = [0u8; 2];
    rd.read_exact(&mut head).await.map_err(|_| 0u16)?;
    let (opcode, masked, mut len) = (head[0] & 0x0f, head[1] & 0x80 != 0, u64::from(head[1] & 0x7f));
    if head[0] & 0x70 != 0 || !masked {
        return Err(1002); // reserved bits set, or an unmasked client frame
    }
    if len == 126 {
        let mut b = [0u8; 2];
        rd.read_exact(&mut b).await.map_err(|_| 0u16)?;
        len = u64::from(u16::from_be_bytes(b));
    } else if len == 127 {
        let mut b = [0u8; 8];
        rd.read_exact(&mut b).await.map_err(|_| 0u16)?;
        len = u64::from_be_bytes(b);
    }
    if len > WS_MAX_CLIENT_PAYLOAD || (opcode >= 0x8 && (len > 125 || head[0] & 0x80 == 0)) {
        return Err(1009);
    }
    let mut mask = [0u8; 4];
    rd.read_exact(&mut mask).await.map_err(|_| 0u16)?;
    let mut payload = vec![0u8; len as usize];
    rd.read_exact(&mut payload).await.map_err(|_| 0u16)?;
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
    Ok(match opcode {
        0x8 => ClientFrame::Close,
        0x9 => ClientFrame::Ping(payload),
        _ => ClientFrame::Ignored,
    })
}

async fn ws_session(upgraded: hyper::upgrade::Upgraded, mut notes: tokio::sync::mpsc::Receiver<Note>) {
    use tokio::io::AsyncWriteExt;
    let (mut rd, mut wr) = tokio::io::split(hyper_util::rt::TokioIo::new(upgraded));
    let (ftx, mut frames) = tokio::sync::mpsc::channel::<Result<ClientFrame, u16>>(4);
    let reader = tokio::spawn(async move {
        loop {
            let f = ws_read_frame(&mut rd).await;
            let stop = !matches!(f, Ok(ClientFrame::Ignored | ClientFrame::Ping(_)));
            if ftx.send(f).await.is_err() || stop {
                break;
            }
        }
    });
    let text = |w: &str| ws_frame(0x1, format!("{{\"event\":\"{w}\"}}").as_bytes());
    let lifetime = tokio::time::sleep(WS_LIFETIME);
    tokio::pin!(lifetime);
    let mut ok = wr.write_all(&text("hello")).await.is_ok();
    while ok {
        let out: Option<Vec<u8>> = tokio::select! {
            n = notes.recv() => match n {
                Some(Note::Changed) => Some(text("changed")),
                Some(Note::KeepAlive) => Some(ws_frame(0x9, b"")),
                Some(Note::End) => {
                    let _ = wr.write_all(&text("end")).await;
                    let _ = wr.write_all(&ws_close_frame(1008)).await;
                    break;
                }
                None => break,
            },
            f = frames.recv() => match f {
                Some(Ok(ClientFrame::Ping(p))) => Some(ws_frame(0xA, &p[..p.len().min(125)])),
                Some(Ok(ClientFrame::Ignored)) => None,
                Some(Ok(ClientFrame::Close)) => {
                    let _ = wr.write_all(&ws_close_frame(1000)).await;
                    break;
                }
                Some(Err(0)) | None => break,
                Some(Err(code)) => {
                    let _ = wr.write_all(&ws_close_frame(code)).await;
                    break;
                }
            },
            _ = &mut lifetime => {
                let _ = wr.write_all(&ws_close_frame(1001)).await;
                break;
            }
        };
        if let Some(bytes) = out {
            ok = wr.write_all(&bytes).await.is_ok();
        }
    }
    let _ = wr.shutdown().await;
    reader.abort();
    // `notes` drops here, so the watcher sees the channel closed at its next tick and frees the slot
}

async fn websocket(app: App, on_upgrade: hyper::upgrade::OnUpgrade, headers: HashMap<String, String>, query: String, client: String) -> Response<Body> {
    use base64::Engine;
    let h = |n: &str| headers.get(n).map(String::as_str).unwrap_or("");
    let lists = |name: &str, token: &str| h(name).split(',').any(|t| t.trim().eq_ignore_ascii_case(token));
    let key = h("sec-websocket-key");
    let key_ok = key.len() == 24 && base64::engine::general_purpose::STANDARD.decode(key).map(|b| b.len() == 16).unwrap_or(false);
    if !lists("connection", "upgrade") || !h("upgrade").eq_ignore_ascii_case("websocket") || h("sec-websocket-version") != "13" || !key_ok {
        return plain(400, "bad_websocket_handshake");
    }
    // a browser always sends Origin on a WebSocket; a request without one did not come from a page, so it carries no ambient cookie of a victim
    if let Some(origin) = headers.get("origin") {
        if !ws_origin_ok(origin, h("host")) {
            return plain(403, "origin_not_allowed");
        }
    }
    let Some(first) = changes_call(app.api.clone(), headers.clone(), query.clone(), client.clone(), false).await else { return plain(500, "server_error") };
    if first.status != 200 {
        return into_response(first);
    }
    let Some(slot) = app.streams.acquire(&client) else { return plain(429, "too_many_streams") };
    let (tx, rx) = tokio::sync::mpsc::channel::<Note>(8);
    let (api, last) = (app.api.clone(), signature_of(&first));
    let tx2 = tx.clone();
    tokio::spawn(watch(api, headers.clone(), query, client, last, slot, move || tx.is_closed(), move |n| tx2.try_send(n).is_ok()));
    tokio::spawn(async move {
        if let Ok(upgraded) = on_upgrade.await {
            ws_session(upgraded, rx).await;
        }
    });
    kept_headers(&first, Response::builder().status(101))
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-accept", ws_accept_key(key))
        .body(Body::empty())
        .unwrap_or_default()
}

async fn dispatch(State(app): State<App>, ConnectInfo(peer): ConnectInfo<SocketAddr>, mut req: Request) -> Response<Body> {
    let on_upgrade = (req.method() == axum::http::Method::GET && req.uri().path() == WS_PATH).then(|| hyper::upgrade::on(&mut req));
    let (parts, body) = req.into_parts();
    let mut headers: HashMap<String, String> = HashMap::new();
    for (n, v) in parts.headers.iter() {
        // a repeated header keeps its first value, so a second Cookie or Content-Length cannot override the first
        if let Ok(v) = v.to_str() {
            headers.entry(n.as_str().to_string()).or_insert_with(|| v.to_string());
        }
    }
    if let Some(on_upgrade) = on_upgrade {
        let client = client_of(&peer, &headers, app.behind_proxy);
        return websocket(app, on_upgrade, headers, parts.uri.query().unwrap_or("").to_string(), client).await;
    }
    if parts.method == axum::http::Method::GET && parts.uri.path() == EVENTS_PATH {
        let client = client_of(&peer, &headers, app.behind_proxy);
        return events(app, headers, parts.uri.query().unwrap_or("").to_string(), client).await;
    }
    let bytes: Bytes = match axum::body::to_bytes(body, body_limit(parts.uri.path()) + 1).await {
        Ok(b) => b,
        Err(_) => return plain(413, "payload_too_large"),
    };
    let client = client_of(&peer, &headers, app.behind_proxy);
    let api = app.api.clone();
    let request = ApiRequest { method: parts.method.as_str().to_string(), path: parts.uri.path().to_string(), query: parts.uri.query().unwrap_or("").to_string(), headers, body: bytes.to_vec(), client };
    match tokio::task::spawn_blocking(move || api.handle(request)).await {
        Ok(r) => into_response(r),
        Err(_) => plain(500, "server_error"),
    }
}

pub fn router(api: Arc<Api>, behind_proxy: bool) -> Router {
    Router::new().fallback(dispatch).with_state(App { api, behind_proxy, streams: Arc::new(Streams::default()) })
}

/// Connection limits. A client that sends its request headers slowly, or sends nothing, is cut off; the total number of open connections
/// and the size of a header block are bounded, so a few sockets cannot starve the server (slowloris) and a giant header cannot use memory.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_CONNECTIONS: usize = 2048;
pub const CONNECTION_LIFETIME: Duration = Duration::from_secs(300);

/// Serves until the shutdown future resolves, then lets in-flight requests finish (up to five seconds).
pub async fn serve(api: Arc<Api>, addr: SocketAddr, behind_proxy: bool, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_on(listener, api, behind_proxy, shutdown).await
}

pub async fn serve_on(listener: tokio::net::TcpListener, api: Arc<Api>, behind_proxy: bool, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    use hyper_util::rt::{TokioIo, TokioTimer};
    use tower::ServiceExt;
    let app = router(api, behind_proxy);
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let open = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    tokio::pin!(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(pair) => pair,
                Err(_) => { tokio::time::sleep(Duration::from_millis(50)).await; continue }
            },
            _ = &mut shutdown => break,
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else { continue }; // over the limit: drop the connection at once
        let _ = stream.set_nodelay(true);
        let (app, open) = (app.clone(), open.clone());
        open.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                req.extensions_mut().insert(ConnectInfo(peer));
                app.clone().oneshot(req)
            });
            let mut http = hyper::server::conn::http1::Builder::new();
            http.timer(TokioTimer::new()).header_read_timeout(HEADER_READ_TIMEOUT).max_buf_size(MAX_HEADER_BYTES).keep_alive(true);
            let _ = tokio::time::timeout(CONNECTION_LIFETIME, http.serve_connection(TokioIo::new(stream), service).with_upgrades()).await;
            open.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            drop(permit);
        });
    }
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while open.load(std::sync::atomic::Ordering::SeqCst) > 0 && std::time::Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_for_is_trusted_only_behind_the_proxy_and_only_its_last_hop() {
        let peer: SocketAddr = "10.1.1.1:5000".parse().unwrap();
        let h = HashMap::from([("x-forwarded-for".to_string(), "1.2.3.4, 9.9.9.9".to_string())]);
        assert_eq!(client_of(&peer, &h, false), "10.1.1.1");
        assert_eq!(client_of(&peer, &h, true), "9.9.9.9", "a client-supplied first hop must not choose its own throttle key");
        assert_eq!(client_of(&peer, &HashMap::new(), true), "10.1.1.1");
    }

    #[test]
    fn the_websocket_accept_key_is_the_one_in_the_rfc() {
        assert_eq!(ws_accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn a_socket_may_be_opened_only_by_a_page_of_this_site() {
        assert!(ws_origin_ok("https://claimguard.example", "claimguard.example"));
        assert!(ws_origin_ok("http://127.0.0.1:8080", "127.0.0.1:8080"));
        assert!(ws_origin_ok("https://ClaimGuard.Example", "claimguard.example"));
        for (origin, host) in [("https://evil.example", "claimguard.example"), ("null", "claimguard.example"), ("https://claimguard.example.evil.example", "claimguard.example"),
                               ("https://claimguard.example:444", "claimguard.example"), ("ftp://claimguard.example", "claimguard.example"), ("https://claimguard.example", ""),
                               ("https://claimguard.example/path", "claimguard.example")] {
            assert!(!ws_origin_ok(origin, host), "{origin} vs {host}");
        }
    }

    #[test]
    fn server_frames_are_unmasked_and_short() {
        assert_eq!(ws_frame(0x1, b"hi"), [0x81, 2, b'h', b'i']);
        assert_eq!(ws_close_frame(1000), [0x88, 2, 0x03, 0xe8]);
        assert_eq!(ws_frame(0x9, b""), [0x89, 0]);
    }

    fn masked(opcode: u8, fin: bool, payload: &[u8]) -> Vec<u8> {
        let mask = [1u8, 2, 3, 4];
        let mut f = vec![if fin { 0x80 } else { 0 } | opcode, 0x80 | payload.len() as u8];
        f.extend_from_slice(&mask);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        f
    }

    #[tokio::test]
    async fn client_frames_are_read_masked_and_everything_else_is_a_violation() {
        let mut ping = &masked(0x9, true, b"abc")[..];
        assert!(matches!(ws_read_frame(&mut ping).await, Ok(ClientFrame::Ping(p)) if p == b"abc"));
        let mut close = &masked(0x8, true, &[0x03, 0xe8])[..];
        assert!(matches!(ws_read_frame(&mut close).await, Ok(ClientFrame::Close)));
        let mut text = &masked(0x1, true, b"hello")[..];
        assert!(matches!(ws_read_frame(&mut text).await, Ok(ClientFrame::Ignored)));
        let mut unmasked = &[0x81u8, 2, b'h', b'i'][..];
        assert!(matches!(ws_read_frame(&mut unmasked).await, Err(1002)), "a client frame must be masked");
        let mut reserved = &[0xc1u8, 0x80, 1, 2, 3, 4][..];
        assert!(matches!(ws_read_frame(&mut reserved).await, Err(1002)), "reserved bits must be zero");
        let mut big = &[0x81u8, 0x80 | 126, 0x10, 0x00, 1, 2, 3, 4][..];
        assert!(matches!(ws_read_frame(&mut big).await, Err(1009)), "a large client message is refused");
        let mut fragmented_ping = &masked(0x9, false, b"x")[..];
        assert!(matches!(ws_read_frame(&mut fragmented_ping).await, Err(1009)), "control frames cannot be fragmented");
        let mut cut = &masked(0x1, true, b"hello")[..4];
        assert!(matches!(ws_read_frame(&mut cut).await, Err(0)), "a cut connection ends the session");
    }

    #[test]
    fn stream_slots_are_limited_per_address_and_in_total_and_come_back_when_dropped() {
        let s = Arc::new(Streams::default());
        let mine: Vec<StreamSlot> = (0..MAX_STREAMS_PER_CLIENT).map(|_| s.acquire("10.0.0.1").expect("within the per-address limit")).collect();
        assert!(s.acquire("10.0.0.1").is_none(), "one address cannot hold more than the per-address limit");
        assert!(s.acquire("10.0.0.2").is_some(), "another address is not affected");
        assert_eq!(s.open(), MAX_STREAMS_PER_CLIENT);
        drop(mine);
        assert_eq!(s.open(), 0);
        assert!(s.acquire("10.0.0.1").is_some(), "slots return when streams end");
        let many: Vec<StreamSlot> = (0..MAX_STREAMS).map(|i| s.acquire(&format!("10.1.{}.{}", i / 200, i % 200)).expect("within the total")).collect();
        assert!(s.acquire("192.168.0.1").is_none(), "the total is capped");
        drop(many);
    }
}
