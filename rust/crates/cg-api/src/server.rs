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
    let api = app.api.clone();
    let mut last = signature_of(&first);
    let mut out = Response::builder().status(200);
    for (n, v) in &first.headers {
        if !n.eq_ignore_ascii_case("content-type") && !n.eq_ignore_ascii_case("content-length") {
            if let (Ok(n), Ok(v)) = (HeaderName::from_bytes(n.as_bytes()), HeaderValue::from_str(v)) {
                out = out.header(n, v);
            }
        }
    }
    out = out.header("content-type", "text/event-stream; charset=utf-8").header("x-accel-buffering", "no");
    tokio::spawn(async move {
        let _slot = slot; // released when the client goes away, the session ends or the connection is cut
        let mut quiet_for = Duration::ZERO;
        loop {
            tokio::time::sleep(STREAM_TICK).await;
            if tx.is_closed() {
                break;
            }
            let Some(r) = changes_call(api.clone(), headers.clone(), query.clone(), client.clone(), true).await else { continue };
            match r.status {
                200 => {
                    let now = signature_of(&r);
                    if now != last {
                        last = now;
                        quiet_for = Duration::ZERO;
                        if tx.try_send(Bytes::from_static(b"event: changed\ndata: {}\n\n")).is_err() {
                            break; // a client that does not read is dropped, not waited for
                        }
                    } else {
                        quiet_for += STREAM_TICK;
                        if quiet_for >= STREAM_KEEPALIVE {
                            quiet_for = Duration::ZERO;
                            if tx.try_send(Bytes::from_static(b": keep-alive\n\n")).is_err() {
                                break;
                            }
                        }
                    }
                }
                401 | 403 => {
                    let _ = tx.try_send(Bytes::from_static(b"event: end\ndata: {\"reason\": \"session\"}\n\n"));
                    break;
                }
                _ => {} // a passing failure (the database was busy): ask again at the next tick
            }
        }
    });
    out.body(Body::new(EventBody(rx))).unwrap_or_default()
}

async fn dispatch(State(app): State<App>, ConnectInfo(peer): ConnectInfo<SocketAddr>, req: Request) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let mut headers: HashMap<String, String> = HashMap::new();
    for (n, v) in parts.headers.iter() {
        // a repeated header keeps its first value, so a second Cookie or Content-Length cannot override the first
        if let Ok(v) = v.to_str() {
            headers.entry(n.as_str().to_string()).or_insert_with(|| v.to_string());
        }
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
            let _ = tokio::time::timeout(CONNECTION_LIFETIME, http.serve_connection(TokioIo::new(stream), service)).await;
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
