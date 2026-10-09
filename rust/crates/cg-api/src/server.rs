//! The thin axum adapter: bytes in, `Api::handle` on a blocking thread, bytes out. No rule lives here.

use crate::{Api, ApiRequest, ApiResponse, MAX_BODY_BYTES};
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderName, HeaderValue, Response, StatusCode};
use axum::Router;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
struct App {
    api: Arc<Api>,
    behind_proxy: bool,
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

async fn dispatch(State(app): State<App>, ConnectInfo(peer): ConnectInfo<SocketAddr>, req: Request) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let mut headers: HashMap<String, String> = HashMap::new();
    for (n, v) in parts.headers.iter() {
        // a repeated header keeps its first value, so a second Cookie or Content-Length cannot override the first
        if let Ok(v) = v.to_str() {
            headers.entry(n.as_str().to_string()).or_insert_with(|| v.to_string());
        }
    }
    let bytes: Bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES + 1).await {
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
    Router::new().fallback(dispatch).with_state(App { api, behind_proxy })
}

/// Serves until the shutdown future resolves, then lets in-flight requests finish.
pub async fn serve(api: Arc<Api>, addr: SocketAddr, behind_proxy: bool, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(api, behind_proxy).into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(shutdown).await
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
}
