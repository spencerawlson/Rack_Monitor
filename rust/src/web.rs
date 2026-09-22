//! HTTP server: the dashboard page, its JSON API and the live stream.
//!
//! Updates are delivered as Server-Sent Events. One serialised snapshot per
//! push interval is shared by every connected browser, so ten open windows
//! cost no more serialisation than one.
//!
//! When bound to loopback, requests must name a loopback host. A web page
//! elsewhere cannot then use DNS rebinding to read the metrics through the
//! visitor's browser.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::json;
use tokio_stream::Stream;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::WatchStream;

use crate::service::Service;

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLES_CSS: &str = include_str!("../web/styles.css");

pub const APP_ID: &str = "plh-rack-monitor";
pub const SHUTDOWN_HEADER: &str = "x-plh-token";

pub struct AppState {
    pub svc: Arc<Service>,
    pub shutdown_token: String,
    allowed_hosts: Option<Vec<String>>,
}

impl AppState {
    pub fn new(svc: Arc<Service>, shutdown_token: String) -> Arc<Self> {
        let port = svc.settings.port;
        let allowed_hosts = svc.settings.loopback_only().then(|| {
            vec![format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")]
        });
        Arc::new(Self { svc, shutdown_token, allowed_hosts })
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/static/app.js", get(app_js))
        .route("/static/styles.css", get(styles_css))
        .route("/api/health", get(health))
        .route("/api/config", get(config))
        .route("/api/metrics", get(metrics))
        .route("/api/stream", get(stream))
        .route("/api/shutdown", post(shutdown))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// Reject foreign Host headers and add defensive headers to every response.
async fn guard(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    if let Some(allowed) = &state.allowed_hosts {
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !allowed.iter().any(|a| *a == host) {
            return (StatusCode::FORBIDDEN, "Host not allowed").into_response();
        }
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

async fn index() -> Response {
    asset("text/html; charset=utf-8", INDEX_HTML)
}

async fn app_js() -> Response {
    asset("text/javascript; charset=utf-8", APP_JS)
}

async fn styles_css() -> Response {
    asset("text/css; charset=utf-8", STYLES_CSS)
}

async fn health(State(state): State<Arc<AppState>>) -> Response {
    axum::Json(json!({
        "status": "ok",
        "app": APP_ID,
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "service": state.svc.service_status(),
    }))
    .into_response()
}

async fn config(State(state): State<Arc<AppState>>) -> Response {
    let mut body = state.svc.settings.public(&state.svc.host_label());
    if let Some(nodes) = body.get_mut("nodes").and_then(|n| n.as_array_mut()) {
        nodes.extend(crate::service::demo_public(state.svc.demo_nodes()));
    }
    axum::Json(body).into_response()
}

async fn metrics(State(state): State<Arc<AppState>>) -> Response {
    axum::Json(state.svc.snapshot()).into_response()
}

async fn stream(State(state): State<Arc<AppState>>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let frames = WatchStream::new(state.svc.subscribe())
        .map(|json| Ok(Event::default().event("metrics").data(json.as_str())));
    Sse::new(frames).keep_alive(KeepAlive::new().interval(Duration::from_secs(10)).text("keepalive"))
}

/// Graceful stop for `plh-rack-monitor stop`. The token is random per run
/// and stored only in the per-user instance file, so a web page cannot
/// trigger this through the visitor's browser.
async fn shutdown(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let supplied = headers.get(SHUTDOWN_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !constant_time_eq(supplied.as_bytes(), state.shutdown_token.as_bytes()) {
        return (StatusCode::FORBIDDEN, "invalid token").into_response();
    }
    crate::log_info!("shutdown requested through the API");
    let svc = state.svc.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        svc.stop();
    });
    (StatusCode::ACCEPTED, axum::Json(json!({"status": "stopping"}))).into_response()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FileConfig, validate};
    use std::path::Path;

    async fn start(demo: usize) -> (String, Arc<AppState>) {
        let mut file = FileConfig::default();
        file.host.disk_health = false;
        file.host.temperature = false;
        file.nodes.push(crate::config::NodeSection { name: "PVE02".into(), ..Default::default() });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        file.server.port = port;
        let settings = Arc::new(validate(file, Path::new("."), Path::new("c.toml"), false, Vec::new()));
        let svc = Service::new(settings, demo);
        svc.start();
        let state = AppState::new(svc, "secret-token".into());
        let app = router(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("127.0.0.1:{port}"), state)
    }

    /// GET through httpmini, or through a raw socket when a forged Host
    /// header is needed (httpmini always sends the real one).
    fn get(authority: &str, path: &str, host_header: Option<&str>) -> (u16, String) {
        let (host, port) = authority.split_once(':').unwrap();
        let port: u16 = port.parse().unwrap();
        if let Some(forged) = host_header {
            return raw(host, port, path, forged);
        }
        let r = crate::httpmini::request(host, port, "GET", path, &[], Duration::from_secs(5)).unwrap();
        (r.status, r.body)
    }

    fn raw(host: &str, port: u16, path: &str, host_header: &str) -> (u16, String) {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect((host, port)).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n\r\n").unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        let r = crate::httpmini::parse_response(&buf).unwrap();
        (r.status, r.body)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_identifies_the_app() {
        let (addr, _) = start(0).await;
        let (status, body) = tokio::task::spawn_blocking(move || get(&addr, "/api/health", None)).await.unwrap();
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["app"], APP_ID);
        assert_eq!(v["pid"], std::process::id());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_lists_nodes_without_credentials() {
        let (addr, _) = start(2).await;
        let (status, body) = tokio::task::spawn_blocking(move || get(&addr, "/api/config", None)).await.unwrap();
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(v["nodes"][0]["name"], "PVE02");
        assert_eq!(v["nodes"][1]["key"], "demo1");
        assert!(!body.to_lowercase().contains("token_secret"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unconfigured_node_has_no_invented_figures() {
        let (addr, _) = start(0).await;
        let (_, body) = tokio::task::spawn_blocking(move || get(&addr, "/api/metrics", None)).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let node = &v["nodes"][0];
        assert_eq!(node["status"], "UNCONFIGURED");
        assert!(node["primary"]["cpu_percent"].is_null());
        assert!(node["primary"]["memory_percent"].is_null());
        assert!(v["host"]["memory"]["total_bytes"].as_u64().unwrap() > 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn foreign_host_header_is_refused() {
        let (addr, _) = start(0).await;
        let a = addr.clone();
        let (status, _) = tokio::task::spawn_blocking(move || get(&a, "/api/metrics", Some("attacker.example:80")))
            .await
            .unwrap();
        assert_eq!(status, 403);
        let port = addr.split(':').nth(1).unwrap().to_string();
        let (status, _) = tokio::task::spawn_blocking(move || get(&addr, "/", Some(&format!("localhost:{port}"))))
            .await
            .unwrap();
        assert_eq!(status, 200);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn assets_are_served_with_security_headers() {
        let (addr, _) = start(0).await;
        let (host, port) = addr.split_once(':').map(|(h, p)| (h.to_string(), p.parse::<u16>().unwrap())).unwrap();
        let raw_head = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut s = std::net::TcpStream::connect((host.as_str(), port)).unwrap();
            write!(s, "GET /static/app.js HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).unwrap();
            String::from_utf8_lossy(&buf).to_lowercase()
        })
        .await
        .unwrap();
        assert!(raw_head.starts_with("http/1.1 200"));
        assert!(raw_head.contains("content-security-policy"));
        assert!(raw_head.contains("x-content-type-options: nosniff"));
        assert!(raw_head.contains("cache-control: no-store"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_needs_the_token() {
        let (addr, state) = start(0).await;
        let a = addr.clone();
        let refused = tokio::task::spawn_blocking(move || {
            let (h, p) = a.split_once(':').unwrap();
            crate::httpmini::request(h, p.parse().unwrap(), "POST", "/api/shutdown", &[(SHUTDOWN_HEADER, "wrong")], Duration::from_secs(5))
                .unwrap()
                .status
        })
        .await
        .unwrap();
        assert_eq!(refused, 403);
        assert!(!state.svc.shutdown.is_set());

        let accepted = tokio::task::spawn_blocking(move || {
            let (h, p) = addr.split_once(':').unwrap();
            crate::httpmini::request(h, p.parse().unwrap(), "POST", "/api/shutdown", &[(SHUTDOWN_HEADER, "secret-token")], Duration::from_secs(5))
                .unwrap()
                .status
        })
        .await
        .unwrap();
        assert_eq!(accepted, 202);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(state.svc.shutdown.is_set());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stream_sends_a_metrics_event() {
        let (addr, _) = start(0).await;
        let frame = tokio::task::spawn_blocking(move || {
            use std::io::{BufRead, BufReader, Write};
            let (h, p) = addr.split_once(':').unwrap();
            let mut s = std::net::TcpStream::connect((h, p.parse::<u16>().unwrap())).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            write!(s, "GET /api/stream HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\n\r\n").unwrap();
            let reader = BufReader::new(s);
            reader
                .lines()
                .map_while(Result::ok)
                .find(|l| l.starts_with("data:"))
                .map(|l| l.trim_start_matches("data:").trim().to_string())
        })
        .await
        .unwrap()
        .expect("a data line");
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert!(v["host"]["memory"]["total_bytes"].as_u64().unwrap() > 0);
    }

    #[test]
    fn token_comparison() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"ab", b"abc"));
    }
}
