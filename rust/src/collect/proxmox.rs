//! Proxmox VE node collection over the official REST API.
//!
//! One collector serves one node and owns one persistent HTTPS client, so TLS
//! is negotiated once rather than on every poll. Only per-node endpoints are
//! read for figures (/nodes/{node}/...), never cluster-wide ones, so two
//! configured members of one cluster cannot double-count anything.
//!
//! Failure never propagates: an unreachable node returns its last good
//! figures marked stale with the time of that success. Consecutive failures
//! back off geometrically so a node that is switched off is not polled at
//! full rate.
//!
//! The API token travels in the Authorization header, marked sensitive, and
//! its text is scrubbed from any message that could reach a log or the UI.

use std::sync::Once;
use std::time::{Duration, Instant};

use reqwest::Client;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;

use crate::config::{
    NodeSettings, STATUS_AUTH_ERROR, STATUS_CONFIG_ERROR, STATUS_OFFLINE, STATUS_ONLINE,
};
use crate::model::{Guests, LoadAverage, NodeMetrics, Primary, StorageEntry, Temperature, now_iso};
use crate::rates::{percent_of, round1};

/// Install ring as the process-wide TLS crypto provider. reqwest is built
/// without a provider of its own and refuses to create a client otherwise.
pub fn install_crypto() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[derive(Debug)]
enum FetchError {
    Status(u16),
    Forbidden,
    Connect(String),
    Timeout,
    Invalid(String),
    Other(String),
}

pub struct ProxmoxCollector {
    node: NodeSettings,
    interval: f64,
    base_url: String,
    client: Option<Client>,
    resolved: Option<String>,
    failures: u32,
    retry_after: Option<Instant>,
}

impl ProxmoxCollector {
    pub fn new(node: NodeSettings, interval: f64) -> Self {
        let base_url = node.base_url();
        Self::with_base_url(node, interval, base_url)
    }

    /// As `new`, against an explicit API root. The tests point this at a
    /// local plain-HTTP server that replays recorded Proxmox responses.
    pub fn with_base_url(node: NodeSettings, interval: f64, base_url: String) -> Self {
        install_crypto();
        Self { node, interval, base_url, client: None, resolved: None, failures: 0, retry_after: None }
    }

    fn redact(&self, text: &str) -> String {
        let mut cleaned = text.to_string();
        for secret in [&self.node.token_secret, &self.node.token_id] {
            if !secret.is_empty() {
                cleaned = cleaned.replace(secret.as_str(), "[redacted]");
            }
        }
        cleaned
    }

    fn client(&mut self) -> Result<Client, String> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        let client = build_client(&self.node)?;
        self.client = Some(client.clone());
        Ok(client)
    }

    // ---------------------------------------------------------------- fetch

    /// GET one endpoint and return its `data` member.
    async fn fetch(&self, client: &Client, path: &str) -> Result<Value, FetchError> {
        let url = format!("{}{}", self.base_url, path);
        let response = client.get(&url).send().await.map_err(|e| classify(&e))?;
        let status = response.status().as_u16();
        if status == 403 {
            return Err(FetchError::Forbidden);
        }
        if !(200..300).contains(&status) {
            return Err(FetchError::Status(status));
        }
        let payload: Value = response
            .json()
            .await
            .map_err(|_| FetchError::Invalid(format!("Response from {path} was not JSON")))?;
        match payload {
            Value::Object(mut map) => map
                .remove("data")
                .ok_or_else(|| FetchError::Invalid(format!("Response from {path} lacked a data member"))),
            _ => Err(FetchError::Invalid(format!("Response from {path} lacked a data member"))),
        }
    }

    /// Confirm the API node name, discovering it when the configured one
    /// does not exist, so a node can be renamed in Proxmox without a config
    /// change. Only a positive identification is cached; a fallback to the
    /// configured name is retried on the next poll, so a node that was down
    /// at startup is still identified later.
    async fn resolve_node(&mut self, client: &Client) -> String {
        if let Some(name) = &self.resolved {
            return name.clone();
        }
        let configured =
            if self.node.api_node.is_empty() { self.node.name.clone() } else { self.node.api_node.clone() };
        let Ok(nodes) = self.fetch(client, "/nodes").await else {
            return configured;
        };
        let names: Vec<String> = nodes
            .as_array()
            .map(|items| {
                items.iter().filter_map(|i| i.get("node").and_then(Value::as_str).map(str::to_string)).collect()
            })
            .unwrap_or_default();

        let resolved = if names.contains(&configured) {
            Some(configured.clone())
        } else if let Some(found) = names.iter().find(|n| n.eq_ignore_ascii_case(&configured)) {
            Some(found.clone())
        } else if names.len() == 1 {
            Some(names[0].clone())
        } else if names.len() > 1 {
            // In a cluster /nodes lists every member whichever host answers,
            // so the member this host *is* comes from /cluster/status.
            self.local_cluster_node(client).await
        } else {
            None
        };
        match resolved {
            Some(name) => {
                self.resolved = Some(name.clone());
                name
            }
            None => configured,
        }
    }

    /// Name of the cluster member that answered, from its `local` flag.
    /// Used for identification only; no figures come from cluster endpoints.
    async fn local_cluster_node(&self, client: &Client) -> Option<String> {
        let members = self.fetch(client, "/cluster/status").await.ok()?;
        members.as_array()?.iter().find_map(|item| {
            let is_node = item.get("type").and_then(Value::as_str) == Some("node");
            let local = match item.get("local") {
                Some(Value::Number(n)) => n.as_i64() == Some(1),
                Some(Value::Bool(b)) => *b,
                Some(Value::String(s)) => s == "1",
                _ => false,
            };
            (is_node && local).then(|| item.get("name").and_then(Value::as_str).map(str::to_string)).flatten()
        })
    }

    // -------------------------------------------------------------- results

    /// A card with no figures, for a node that is not being polled.
    fn placeholder(&self, status: &str, message: Option<String>) -> NodeMetrics {
        NodeMetrics {
            key: self.node.key.clone(),
            name: self.node.name.clone(),
            status: status.to_string(),
            message,
            last_attempt: Some(now_iso()),
            ..NodeMetrics::default()
        }
    }

    /// Report a failure while keeping the last successful figures, which
    /// keep their original success time so the dashboard can show their age.
    fn carry_forward(&self, previous: Option<&NodeMetrics>, status: &str, message: String) -> NodeMetrics {
        if let Some(prev) = previous.filter(|p| p.last_success.is_some()) {
            let mut carried = prev.clone();
            carried.key = self.node.key.clone();
            carried.name = self.node.name.clone();
            carried.status = status.to_string();
            carried.message = Some(message);
            carried.last_attempt = Some(now_iso());
            carried.stale = true;
            carried.consecutive_failures = self.failures;
            return carried;
        }
        NodeMetrics {
            key: self.node.key.clone(),
            name: self.node.name.clone(),
            status: status.to_string(),
            api_node: self.resolved.clone(),
            message: Some(message),
            last_attempt: Some(now_iso()),
            consecutive_failures: self.failures,
            ..NodeMetrics::default()
        }
    }

    fn register_failure(&mut self) {
        self.failures += 1;
        let exponent = self.failures.saturating_sub(1).min(5);
        let delay = (self.interval * f64::from(1u32 << exponent)).min(60.0);
        self.retry_after = Some(Instant::now() + Duration::from_secs_f64(delay));
    }

    /// Poll the node once. Never panics on a bad reply and never fails.
    pub async fn collect(&mut self, previous: Option<&NodeMetrics>) -> NodeMetrics {
        if let Some(reason) = self.node.unconfigured_reason() {
            return self.placeholder(self.node.status_when_unpolled(), Some(reason));
        }
        if let Some(after) = self.retry_after {
            let now = Instant::now();
            if now < after {
                let wait = after.duration_since(now).as_secs();
                return self.carry_forward(previous, STATUS_OFFLINE, format!("Unreachable; next retry in {wait}s"));
            }
        }
        let client = match self.client() {
            Ok(client) => client,
            Err(reason) => {
                self.register_failure();
                return self.carry_forward(previous, STATUS_CONFIG_ERROR, reason);
            }
        };

        let outcome = async {
            let node_name = self.resolve_node(&client).await;
            let status = self.fetch(&client, &format!("/nodes/{node_name}/status")).await?;
            if !status.is_object() {
                return Err(FetchError::Invalid("Node status was not an object".into()));
            }
            let mut metrics = build(&self.node, &status, &node_name);
            self.optional(&client, &node_name, previous, &mut metrics).await;
            Ok(metrics)
        }
        .await;

        match outcome {
            Ok(metrics) => {
                self.failures = 0;
                self.retry_after = None;
                metrics
            }
            Err(error) => {
                self.register_failure();
                let (status, message) = match error {
                    FetchError::Status(401) => (
                        STATUS_AUTH_ERROR,
                        "Authentication failed (401) - check token id, secret and permissions".to_string(),
                    ),
                    FetchError::Forbidden => (
                        STATUS_AUTH_ERROR,
                        "Token lacks permission for node status (403) - grant PVEAuditor".to_string(),
                    ),
                    FetchError::Status(code) => (STATUS_OFFLINE, format!("HTTP {code} from node")),
                    FetchError::Connect(detail) => {
                        // A fresh client on the next attempt: the old one may
                        // hold a connection the node has forgotten.
                        self.client = None;
                        (STATUS_OFFLINE, format!("Connection failed: {}", self.redact(&detail)))
                    }
                    FetchError::Timeout => {
                        (STATUS_OFFLINE, format!("Timed out after {}s", self.node.timeout))
                    }
                    FetchError::Invalid(detail) => {
                        (STATUS_OFFLINE, format!("Invalid API response: {}", self.redact(&detail)))
                    }
                    FetchError::Other(detail) => {
                        self.client = None;
                        (STATUS_OFFLINE, self.redact(&detail))
                    }
                };
                self.carry_forward(previous, status, message)
            }
        }
    }

    /// Storage and guest inventory, each optional on token permissions.
    ///
    /// A section that fails transiently (a node busy enough that its storage
    /// query times out) keeps its last value so the card does not flicker
    /// between figures and blanks. A permission refusal is reported as such
    /// and never masked by an old value.
    async fn optional(
        &self,
        client: &Client,
        node_name: &str,
        previous: Option<&NodeMetrics>,
        out: &mut NodeMetrics,
    ) {
        let before = previous.filter(|p| p.last_success.is_some());

        match self.fetch(client, &format!("/nodes/{node_name}/storage")).await {
            Ok(storage) => {
                let mut seen = std::collections::HashSet::new();
                let mut total_sum = 0u64;
                let mut avail_sum = 0u64;
                for item in storage.as_array().into_iter().flatten().filter(|i| i.is_object()) {
                    let name = item.get("storage").and_then(Value::as_str).map(str::to_string);
                    let total = int(item.get("total"));
                    let used = int(item.get("used"));
                    let avail = int(item.get("avail"));
                    out.storage.push(StorageEntry {
                        name: name.clone(),
                        kind: item.get("type").and_then(Value::as_str).map(str::to_string),
                        total_bytes: total,
                        used_bytes: used,
                        available_bytes: avail,
                        percent: pct(used, total),
                        enabled: item.get("enabled").map(truthy).unwrap_or(true),
                    });
                    // A storage listed twice on one node is counted once.
                    if seen.insert(name.unwrap_or_default()) {
                        total_sum += total.unwrap_or(0);
                        avail_sum += avail.unwrap_or(0);
                    }
                }
                out.storage_total_bytes = (total_sum > 0).then_some(total_sum);
                out.storage_available_bytes = (avail_sum > 0).then_some(avail_sum);
            }
            Err(FetchError::Forbidden) => out.storage.clear(),
            Err(_) => {
                if let Some(b) = before.filter(|b| b.storage_total_bytes.is_some() && b.storage_available_bytes.is_some()) {
                    out.storage = b.storage.clone();
                    out.storage_total_bytes = b.storage_total_bytes;
                    out.storage_available_bytes = b.storage_available_bytes;
                }
            }
        }

        for (path, is_vm) in [("qemu", true), ("lxc", false)] {
            let guests = match self.fetch(client, &format!("/nodes/{node_name}/{path}")).await {
                Ok(list) => {
                    let items: Vec<&Value> = list.as_array().into_iter().flatten().filter(|g| g.is_object()).collect();
                    Guests {
                        total: Some(items.len() as u64),
                        running: Some(
                            items.iter().filter(|g| g.get("status").and_then(Value::as_str) == Some("running")).count()
                                as u64,
                        ),
                        permitted: true,
                    }
                }
                Err(FetchError::Forbidden) => Guests { total: None, running: None, permitted: false },
                Err(_) => {
                    let old = before.and_then(|b| if is_vm { b.vms.clone() } else { b.containers.clone() });
                    old.unwrap_or(Guests { total: None, running: None, permitted: true })
                }
            };
            if is_vm {
                out.vms = Some(guests);
            } else {
                out.containers = Some(guests);
            }
        }
    }
}

fn build_client(node: &NodeSettings) -> Result<Client, String> {
    let mut auth = HeaderValue::from_str(&format!("PVEAPIToken={}={}", node.token_id, node.token_secret))
        .map_err(|_| "API token contains characters that cannot be sent in a header".to_string())?;
    auth.set_sensitive(true);
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, auth);

    let timeout = Duration::from_secs_f64(node.timeout);
    // The connect phase gets the smaller share. With equal limits the
    // whole-request timer wins the race and a switched-off node would be
    // reported as a slow reply instead of an unreachable host.
    let connect_timeout = Duration::from_secs_f64((node.timeout * 0.75).max(0.4));
    let mut builder = Client::builder()
        .default_headers(headers)
        .timeout(timeout)
        .connect_timeout(connect_timeout)
        .redirect(reqwest::redirect::Policy::none())
        // Nodes are on the local network; a proxy configured for web
        // browsing must not see the token or break the connection.
        .no_proxy();

    if !node.verify_tls {
        builder = builder.tls_danger_accept_invalid_certs(true);
    } else if let Some(path) = &node.ca_cert {
        // With a CA named, only that CA is trusted, as the Python edition
        // did. Without one, the operating system's trust store is used.
        let pem = std::fs::read(path).map_err(|e| format!("Cannot read CA certificate {}: {e}", path.display()))?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|e| format!("CA certificate {} is not valid PEM: {e}", path.display()))?;
        if certs.is_empty() {
            return Err(format!("CA certificate {} contains no certificates", path.display()));
        }
        builder = builder.tls_certs_only(certs);
    }
    builder.build().map_err(|e| format!("Could not create HTTPS client: {}", chain(&e)))
}

fn classify(error: &reqwest::Error) -> FetchError {
    // Connection failures come first, including a connect that timed out:
    // a node that is switched off never answers, and Windows retries a
    // refused loopback connect for about two seconds before giving up. Both
    // are connection-level and warrant a fresh client, unlike a node that
    // accepted the connection and then answered too slowly.
    if error.is_connect() {
        FetchError::Connect(chain(error))
    } else if error.is_timeout() {
        FetchError::Timeout
    } else if error.is_decode() || error.is_body() {
        FetchError::Invalid(chain(error))
    } else {
        FetchError::Other(chain(error))
    }
}

/// An error and its causes on one line, e.g. the TLS reason behind a failed
/// connection ("invalid peer certificate: UnknownIssuer").
fn chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = cause.source();
    }
    message
}

// -------------------------------------------------------------------- parse

/// A JSON number, or a numeric string as Proxmox uses for load averages.
fn num(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|f| f.is_finite())
}

fn int(value: Option<&Value>) -> Option<u64> {
    num(value).filter(|f| *f >= 0.0).map(|f| f as u64)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_i64() != Some(0),
        Value::String(s) => !matches!(s.as_str(), "" | "0" | "false"),
        _ => false,
    }
}

fn pct(used: Option<u64>, total: Option<u64>) -> Option<f64> {
    percent_of(used? as f64, total? as f64)
}

/// A nested object, or an empty one when absent. Proxmox omits fields a
/// token cannot see and adds fields between releases, so none is assumed.
fn sub<'a>(value: &'a Value, key: &str) -> Option<&'a serde_json::Map<String, Value>> {
    value.get(key).and_then(Value::as_object)
}

/// Map a /nodes/{node}/status payload onto the dashboard shape.
fn build(node: &NodeSettings, status: &Value, node_name: &str) -> NodeMetrics {
    let cpu_percent = num(status.get("cpu")).map(|f| round1((f * 100.0).clamp(0.0, 100.0)));

    let memory = sub(status, "memory");
    let memory_total = memory.and_then(|m| int(m.get("total")));
    let memory_used = memory.and_then(|m| int(m.get("used")));
    let memory_percent = pct(memory_used, memory_total);

    let rootfs = sub(status, "rootfs");
    let rootfs_total = rootfs.and_then(|m| int(m.get("total")));
    let rootfs_used = rootfs.and_then(|m| int(m.get("used")));
    let rootfs_percent = pct(rootfs_used, rootfs_total);

    let swap = sub(status, "swap");
    let swap_percent = pct(swap.and_then(|m| int(m.get("used"))), swap.and_then(|m| int(m.get("total"))));

    let mut load = LoadAverage::default();
    if let Some(values) = status.get("loadavg").and_then(Value::as_array) {
        let two_dp = |v: Option<&Value>| num(v).map(|f| (f * 100.0).round() / 100.0);
        load.min1 = two_dp(values.first());
        load.min5 = two_dp(values.get(1));
        load.min15 = two_dp(values.get(2));
        load.available = load.min1.is_some();
    }

    let now = now_iso();
    NodeMetrics {
        key: node.key.clone(),
        name: node.name.clone(),
        status: STATUS_ONLINE.to_string(),
        api_node: Some(node_name.to_string()),
        message: None,
        last_attempt: Some(now.clone()),
        last_success: Some(now),
        stale: false,
        consecutive_failures: 0,
        primary: Primary {
            cpu_percent,
            memory_percent,
            disk_percent: rootfs_percent,
            disk_mount: Some("rootfs".into()),
        },
        cpu_percent,
        cpu_count: sub(status, "cpuinfo").and_then(|c| int(c.get("cpus"))),
        memory_total_bytes: memory_total,
        memory_used_bytes: memory_used,
        memory_percent,
        rootfs_total_bytes: rootfs_total,
        rootfs_used_bytes: rootfs_used,
        rootfs_percent,
        swap_percent,
        uptime_seconds: num(status.get("uptime")),
        load,
        pve_version: status.get("pveversion").and_then(Value::as_str).map(str::to_string),
        kernel: status.get("kversion").and_then(Value::as_str).map(str::to_string),
        // Proxmox documents no temperature endpoint, so none is reported
        // rather than one inferred from an unrelated field.
        temperature: Temperature {
            cpu_celsius: None,
            source: None,
            available: false,
            detail: Some("Not exposed by the Proxmox VE API".into()),
        },
        ..NodeMetrics::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::State;
    use axum::http::{HeaderMap as AxHeaders, StatusCode, Uri};
    use axum::response::{IntoResponse, Response};
    use serde_json::json;

    const SECRET: &str = "11111111-2222-3333-4444-555555555555";

    #[derive(Clone)]
    enum Reply {
        Json(u16, Value),
        Raw(u16, &'static str),
        Slow,
    }

    #[derive(Clone, Default)]
    struct Mock {
        routes: Arc<Mutex<HashMap<String, Reply>>>,
        hits: Arc<AtomicUsize>,
        auth: Arc<Mutex<Option<String>>>,
    }

    impl Mock {
        fn set(&self, suffix: &str, reply: Reply) {
            self.routes.lock().unwrap().insert(suffix.to_string(), reply);
        }
    }

    async fn handle(State(mock): State<Mock>, uri: Uri, headers: AxHeaders) -> Response {
        mock.hits.fetch_add(1, Ordering::SeqCst);
        *mock.auth.lock().unwrap() =
            headers.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
        let path = uri.path().to_string();
        let reply = {
            let routes = mock.routes.lock().unwrap();
            // The longest matching suffix wins, so "/nodes/pve/status" is not
            // answered by the "/nodes" route.
            routes
                .iter()
                .filter(|(suffix, _)| path.ends_with(suffix.as_str()))
                .max_by_key(|(suffix, _)| suffix.len())
                .map(|(_, r)| r.clone())
        };
        match reply {
            Some(Reply::Json(code, body)) => {
                (StatusCode::from_u16(code).unwrap(), axum::Json(body)).into_response()
            }
            Some(Reply::Raw(code, body)) => (StatusCode::from_u16(code).unwrap(), body).into_response(),
            Some(Reply::Slow) => {
                tokio::time::sleep(Duration::from_secs(3)).await;
                (StatusCode::OK, axum::Json(json!({"data": {}}))).into_response()
            }
            None => (StatusCode::NOT_FOUND, axum::Json(json!({"data": null}))).into_response(),
        }
    }

    async fn serve(mock: Mock) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new().fallback(handle).with_state(mock);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://127.0.0.1:{port}/api2/json")
    }

    async fn dead_url() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        format!("http://127.0.0.1:{port}/api2/json")
    }

    fn node(api_node: &str) -> NodeSettings {
        NodeSettings {
            key: "node1".into(),
            name: "PVE01".into(),
            host: "127.0.0.1".into(),
            port: 8006,
            api_node: api_node.into(),
            token_id: "monitor@pve!plh".into(),
            token_secret: SECRET.into(),
            verify_tls: true,
            ca_cert: None,
            timeout: 1.0,
            errors: Vec::new(),
        }
    }

    fn status_payload() -> Value {
        json!({"data": {
            "uptime": 1036800,
            "cpu": 0.1834,
            "loadavg": ["0.42", "0.35", "0.30"],
            "cpuinfo": {"cpus": 4, "model": "Intel(R) Core(TM) i5-6500T"},
            "memory": {"total": 33645936640u64, "used": 12163481600u64, "free": 21482455040u64},
            "rootfs": {"total": 100861792256u64, "used": 22194724864u64, "avail": 73513623552u64},
            "swap": {"total": 8589934592u64, "used": 268435456u64},
            "pveversion": "pve-manager/9.2.20/abc",
            "kversion": "Linux 6.14.8-2-pve"
        }})
    }

    fn healthy(mock: &Mock) {
        mock.set("/nodes", Reply::Json(200, json!({"data": [{"node": "pve"}]})));
        mock.set("/nodes/pve/status", Reply::Json(200, status_payload()));
        mock.set(
            "/nodes/pve/storage",
            Reply::Json(200, json!({"data": [
                {"storage": "local", "type": "dir", "total": 100, "used": 40, "avail": 60, "enabled": 1},
                {"storage": "local-lvm", "type": "lvmthin", "total": 200, "used": 50, "avail": 150}
            ]})),
        );
        mock.set(
            "/nodes/pve/qemu",
            Reply::Json(200, json!({"data": [{"vmid": 100, "status": "running"}, {"vmid": 101, "status": "stopped"}]})),
        );
        mock.set("/nodes/pve/lxc", Reply::Json(200, json!({"data": [{"vmid": 200, "status": "running"}]})));
    }

    #[test]
    fn percentages_from_a_status_payload() {
        let data = status_payload()["data"].clone();
        let m = build(&node("pve"), &data, "pve");
        assert_eq!(m.cpu_percent, Some(18.3));
        assert_eq!(m.memory_percent, Some(36.2));
        assert_eq!(m.rootfs_percent, Some(22.0));
        assert_eq!(m.swap_percent, Some(3.1));
        assert_eq!(m.cpu_count, Some(4));
        assert_eq!(m.load.min1, Some(0.42));
        assert_eq!(m.primary.disk_percent, Some(22.0));
        assert!(!m.temperature.available);
    }

    #[tokio::test]
    async fn successful_collection() {
        let mock = Mock::default();
        healthy(&mock);
        let mut c = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock.clone()).await);
        let m = c.collect(None).await;
        assert_eq!(m.status, STATUS_ONLINE, "{:?}", m.message);
        assert_eq!(m.api_node.as_deref(), Some("pve"));
        assert_eq!(m.vms.as_ref().unwrap().total, Some(2));
        assert_eq!(m.vms.as_ref().unwrap().running, Some(1));
        assert_eq!(m.containers.as_ref().unwrap().running, Some(1));
        assert_eq!(m.storage_total_bytes, Some(300));
        assert_eq!(m.storage_available_bytes, Some(210));
        assert_eq!(mock.auth.lock().unwrap().as_deref(), Some(format!("PVEAPIToken=monitor@pve!plh={SECRET}").as_str()));
    }

    #[tokio::test]
    async fn unconfigured_node_has_no_figures() {
        let mut n = node("pve");
        n.host.clear();
        let m = ProxmoxCollector::with_base_url(n, 4.0, dead_url().await).collect(None).await;
        assert_eq!(m.status, "UNCONFIGURED");
        assert_eq!(m.message.as_deref(), Some("Host not set"));
        assert_eq!(m.primary.cpu_percent, None);
    }

    #[tokio::test]
    async fn authentication_failure() {
        let mock = Mock::default();
        mock.set("/nodes", Reply::Json(401, json!({"data": null})));
        mock.set("/status", Reply::Json(401, json!({"data": null})));
        let mut c = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock).await);
        let m = c.collect(None).await;
        assert_eq!(m.status, STATUS_AUTH_ERROR);
        let message = m.message.unwrap();
        assert!(message.contains("401"));
        assert!(!message.contains(SECRET));
    }

    #[tokio::test]
    async fn forbidden_status_is_auth_error() {
        let mock = Mock::default();
        healthy(&mock);
        mock.set("/nodes/pve/status", Reply::Json(403, json!({"data": null})));
        let m = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock).await).collect(None).await;
        assert_eq!(m.status, STATUS_AUTH_ERROR);
        assert!(m.message.unwrap().contains("PVEAuditor"));
    }

    #[tokio::test]
    async fn forbidden_subresources_are_not_fatal() {
        let mock = Mock::default();
        healthy(&mock);
        for p in ["/nodes/pve/storage", "/nodes/pve/qemu", "/nodes/pve/lxc"] {
            mock.set(p, Reply::Json(403, json!({"data": null})));
        }
        let m = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock).await).collect(None).await;
        assert_eq!(m.status, STATUS_ONLINE);
        assert_eq!(m.cpu_percent, Some(18.3));
        assert!(!m.vms.as_ref().unwrap().permitted);
        assert_eq!(m.vms.as_ref().unwrap().total, None);
        assert!(m.storage.is_empty());
    }

    #[tokio::test]
    async fn disconnection_keeps_last_good_figures_marked_stale() {
        let mock = Mock::default();
        healthy(&mock);
        let mut c = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock).await);
        let good = c.collect(None).await;
        assert_eq!(good.status, STATUS_ONLINE);

        c.base_url = dead_url().await;
        let offline = c.collect(Some(&good)).await;
        assert_eq!(offline.status, STATUS_OFFLINE);
        assert!(offline.stale);
        assert_eq!(offline.cpu_percent, good.cpu_percent);
        assert_eq!(offline.memory_percent, good.memory_percent);
        assert_eq!(offline.last_success, good.last_success);
        let message = offline.message.unwrap();
        assert!(message.starts_with("Connection failed"), "{message}");
    }

    #[tokio::test]
    async fn never_reachable_node_shows_no_figures() {
        let m = ProxmoxCollector::with_base_url(node("pve"), 4.0, dead_url().await).collect(None).await;
        assert_eq!(m.status, STATUS_OFFLINE);
        assert!(!m.stale);
        assert_eq!(m.primary.cpu_percent, None);
    }

    #[tokio::test]
    async fn timeout_is_reported() {
        let mock = Mock::default();
        mock.set("/nodes", Reply::Json(200, json!({"data": [{"node": "pve"}]})));
        mock.set("/nodes/pve/status", Reply::Slow);
        let m = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock).await).collect(None).await;
        assert_eq!(m.status, STATUS_OFFLINE);
        assert!(m.message.unwrap().contains("Timed out"), "timeout expected");
    }

    #[tokio::test]
    async fn repeated_failures_back_off_without_requests() {
        let mock = Mock::default();
        mock.set("/nodes", Reply::Json(500, json!({})));
        mock.set("/status", Reply::Json(500, json!({})));
        let mut c = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock.clone()).await);
        c.collect(None).await;
        let hits = mock.hits.load(Ordering::SeqCst);
        let second = c.collect(None).await;
        assert_eq!(mock.hits.load(Ordering::SeqCst), hits);
        assert!(second.message.unwrap().contains("next retry in"));
    }

    #[tokio::test]
    async fn missing_rootfs_does_not_mark_a_reachable_node_offline() {
        let mock = Mock::default();
        healthy(&mock);
        mock.set(
            "/nodes/pve/status",
            Reply::Json(200, json!({"data": {"cpu": 0.5, "memory": {"total": 100, "used": 50}, "uptime": 60}})),
        );
        let m = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock).await).collect(None).await;
        assert_eq!(m.status, STATUS_ONLINE);
        assert_eq!(m.cpu_percent, Some(50.0));
        assert_eq!(m.rootfs_percent, None);
        assert_eq!(m.primary.disk_percent, None);
    }

    #[tokio::test]
    async fn payload_without_data_is_invalid() {
        let mock = Mock::default();
        healthy(&mock);
        mock.set("/nodes/pve/status", Reply::Json(200, json!({"unexpected": true})));
        let m = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock).await).collect(None).await;
        assert_eq!(m.status, STATUS_OFFLINE);
        assert!(m.message.unwrap().contains("Invalid API response"));
    }

    #[tokio::test]
    async fn non_json_body_is_invalid() {
        let mock = Mock::default();
        healthy(&mock);
        mock.set("/nodes/pve/status", Reply::Raw(200, "<html>proxy error</html>"));
        let m = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock).await).collect(None).await;
        assert!(m.message.unwrap().contains("not JSON"));
    }

    #[tokio::test]
    async fn non_numeric_values_become_unknown() {
        let mock = Mock::default();
        healthy(&mock);
        mock.set(
            "/nodes/pve/status",
            Reply::Json(200, json!({"data": {"cpu": "abc", "memory": {"total": "x"}, "rootfs": [], "loadavg": "no"}})),
        );
        let m = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock).await).collect(None).await;
        assert_eq!(m.status, STATUS_ONLINE);
        assert_eq!(m.cpu_percent, None);
        assert_eq!(m.memory_percent, None);
        assert!(!m.load.available);
    }

    #[tokio::test]
    async fn single_node_name_is_discovered() {
        let mock = Mock::default();
        mock.set("/nodes", Reply::Json(200, json!({"data": [{"node": "pve-real"}]})));
        mock.set("/nodes/pve-real/status", Reply::Json(200, status_payload()));
        let m = ProxmoxCollector::with_base_url(node("wrong"), 4.0, serve(mock).await).collect(None).await;
        assert_eq!(m.status, STATUS_ONLINE);
        assert_eq!(m.api_node.as_deref(), Some("pve-real"));
    }

    #[tokio::test]
    async fn cluster_member_is_found_by_local_flag() {
        let mock = Mock::default();
        mock.set("/nodes", Reply::Json(200, json!({"data": [{"node": "pve"}, {"node": "pve02"}]})));
        mock.set(
            "/cluster/status",
            Reply::Json(200, json!({"data": [
                {"type": "cluster", "name": "homelab"},
                {"type": "node", "name": "pve", "local": 0},
                {"type": "node", "name": "pve02", "local": 1}
            ]})),
        );
        mock.set("/nodes/pve02/status", Reply::Json(200, status_payload()));
        let m = ProxmoxCollector::with_base_url(node(""), 4.0, serve(mock).await).collect(None).await;
        assert_eq!(m.api_node.as_deref(), Some("pve02"));
        assert_eq!(m.status, STATUS_ONLINE);
    }

    #[tokio::test]
    async fn failed_discovery_is_retried_not_cached() {
        let mock = Mock::default();
        mock.set("/nodes", Reply::Json(500, json!({})));
        let mut c = ProxmoxCollector::with_base_url(node("wrong"), 4.0, serve(mock.clone()).await);
        c.collect(None).await;
        assert!(c.resolved.is_none());
        mock.set("/nodes", Reply::Json(200, json!({"data": [{"node": "pve"}]})));
        mock.set("/nodes/pve/status", Reply::Json(200, status_payload()));
        c.retry_after = None;
        let m = c.collect(None).await;
        assert_eq!(m.api_node.as_deref(), Some("pve"));
    }

    #[tokio::test]
    async fn busy_node_keeps_storage_and_guest_figures() {
        let mock = Mock::default();
        healthy(&mock);
        let mut c = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock.clone()).await);
        let good = c.collect(None).await;
        mock.set("/nodes/pve/storage", Reply::Json(500, json!({})));
        mock.set("/nodes/pve/qemu", Reply::Json(500, json!({})));
        let busy = c.collect(Some(&good)).await;
        assert_eq!(busy.status, STATUS_ONLINE);
        assert_eq!(busy.storage_total_bytes, Some(300));
        assert_eq!(busy.vms.as_ref().unwrap().total, Some(2));
    }

    #[tokio::test]
    async fn unreadable_guest_count_is_unknown_not_zero() {
        let mock = Mock::default();
        healthy(&mock);
        mock.set("/nodes/pve/qemu", Reply::Json(500, json!({})));
        let m = ProxmoxCollector::with_base_url(node("pve"), 4.0, serve(mock).await).collect(None).await;
        let vms = m.vms.unwrap();
        assert_eq!(vms.total, None);
        assert!(vms.permitted);
    }

    #[tokio::test]
    async fn unreadable_ca_is_a_config_error() {
        let dir = std::env::temp_dir().join(format!("plh-ca-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.pem");
        std::fs::write(&bad, "not a certificate").unwrap();
        let mut n = node("pve");
        n.ca_cert = Some(bad);
        let m = ProxmoxCollector::with_base_url(n, 4.0, dead_url().await).collect(None).await;
        assert_eq!(m.status, STATUS_CONFIG_ERROR);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn redaction_removes_token_material() {
        let c = ProxmoxCollector::with_base_url(node("pve"), 4.0, String::new());
        let cleaned = c.redact(&format!("failed talking with monitor@pve!plh={SECRET}"));
        assert!(!cleaned.contains(SECRET));
        assert!(!cleaned.contains("monitor@pve!plh"));
    }

    #[test]
    fn only_per_node_endpoints_supply_figures() {
        let source = include_str!("proxmox.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        assert!(!code.contains("/cluster/resources"));
    }
}
