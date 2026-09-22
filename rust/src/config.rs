//! Configuration: a TOML file, validated into runtime settings.
//!
//! Loading never fails outright. An unreadable file, an unknown key or an
//! out-of-range number is recorded as a warning and a safe default is used,
//! so a typo can never stop the host section from being monitored; the
//! warnings are shown on the dashboard instead.
//!
//! Token secrets are held in memory only and never appear in any payload
//! served to the browser.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg_attr(not(feature = "proxmox"), allow(dead_code))]
pub const STATUS_ONLINE: &str = "ONLINE";
#[cfg_attr(not(feature = "proxmox"), allow(dead_code))]
pub const STATUS_OFFLINE: &str = "OFFLINE";
#[cfg_attr(not(feature = "proxmox"), allow(dead_code))]
pub const STATUS_AUTH_ERROR: &str = "AUTH_ERROR";
pub const STATUS_UNCONFIGURED: &str = "UNCONFIGURED";
pub const STATUS_CONFIG_ERROR: &str = "CONFIG_ERROR";

/// Annotated template written by `config init` and by the installer.
pub const TEMPLATE: &str = include_str!("../config.example.toml");

// ------------------------------------------------------------------ file shape

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub server: ServerSection,
    pub browser: BrowserSection,
    pub display: DisplaySection,
    pub thresholds: ThresholdSection,
    pub intervals: IntervalSection,
    pub history: HistorySection,
    pub host: HostSection,
    #[serde(rename = "proxmox", skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<NodeSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSection {
    pub host: String,
    pub port: u16,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self { host: "127.0.0.1".into(), port: 8765 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserSection {
    pub open_on_run: bool,
    pub kind: String,
    pub app_window: bool,
    pub fullscreen: bool,
    pub window_width: u32,
    pub window_height: u32,
}

impl Default for BrowserSection {
    fn default() -> Self {
        Self {
            open_on_run: true,
            kind: "auto".into(),
            app_window: true,
            fullscreen: false,
            window_width: 0,
            window_height: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DisplaySection {
    pub title: String,
    pub page_seconds: f64,
}

impl Default for DisplaySection {
    fn default() -> Self {
        Self { title: "PLH RACK MONITOR".into(), page_seconds: 10.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThresholdSection {
    pub usage_warning: f64,
    pub usage_critical: f64,
    pub temp_warning_c: f64,
    pub temp_critical_c: f64,
}

impl Default for ThresholdSection {
    fn default() -> Self {
        Self { usage_warning: 70.0, usage_critical: 90.0, temp_warning_c: 75.0, temp_critical_c: 90.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IntervalSection {
    pub cpu_mem: f64,
    pub net: f64,
    pub disk: f64,
    pub processes: f64,
    pub temperature: f64,
    pub disk_health: f64,
    pub proxmox: f64,
    pub stream_push: f64,
    pub stale_after: f64,
    pub sensor_backoff: f64,
    pub history_step: f64,
}

impl Default for IntervalSection {
    fn default() -> Self {
        Self {
            cpu_mem: 1.5,
            net: 1.5,
            disk: 5.0,
            processes: 10.0,
            temperature: 5.0,
            disk_health: 60.0,
            proxmox: 4.0,
            stream_push: 1.0,
            stale_after: 15.0,
            sensor_backoff: 60.0,
            history_step: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistorySection {
    pub enabled: bool,
    pub seconds: f64,
}

impl Default for HistorySection {
    fn default() -> Self {
        Self { enabled: true, seconds: 60.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostSection {
    pub label: String,
    pub disks: Vec<String>,
    pub autodiscover_disks: bool,
    pub include_removable: bool,
    pub primary_disk: String,
    pub temperature: bool,
    pub lhm_url: String,
    pub lhm_wmi: bool,
    pub disk_health: bool,
}

impl Default for HostSection {
    fn default() -> Self {
        Self {
            label: String::new(),
            disks: Vec::new(),
            autodiscover_disks: true,
            include_removable: true,
            primary_disk: String::new(),
            temperature: true,
            lhm_url: "http://127.0.0.1:8085/data.json".into(),
            lhm_wmi: true,
            disk_health: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeSection {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub node: String,
    pub token_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub token_secret: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub token_secret_env: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub token_secret_file: String,
    pub verify_tls: bool,
    pub ca_cert: String,
    pub timeout: f64,
}

impl Default for NodeSection {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: 8006,
            node: String::new(),
            token_id: String::new(),
            token_secret: String::new(),
            token_secret_env: String::new(),
            token_secret_file: String::new(),
            verify_tls: true,
            ca_cert: String::new(),
            timeout: 4.0,
        }
    }
}

// ------------------------------------------------------------ runtime settings

#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub warning: f64,
    pub critical: f64,
}

impl Thresholds {
    /// Display state for a value. Colour only; not a health verdict. The page
    /// applies the same rule; this copy pins the rule down in tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn state(&self, value: Option<f64>) -> &'static str {
        match value {
            None => "unavailable",
            Some(v) if v >= self.critical => "critical",
            Some(v) if v >= self.warning => "warning",
            Some(_) => "normal",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Intervals {
    pub cpu_mem: f64,
    pub net: f64,
    pub disk: f64,
    pub processes: f64,
    pub temperature: f64,
    pub disk_health: f64,
    pub proxmox: f64,
    pub stream_push: f64,
    pub stale_after: f64,
    pub sensor_backoff: f64,
    pub history_step: f64,
}

/// The sparkline window, resolved to the values the ring buffers need.
/// A capacity of zero means history is switched off.
#[derive(Debug, Clone, Copy)]
pub struct HistorySettings {
    pub capacity: usize,
    pub step_seconds: f64,
}

#[derive(Clone)]
pub struct NodeSettings {
    pub key: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub api_node: String,
    pub token_id: String,
    pub token_secret: String,
    pub verify_tls: bool,
    pub ca_cert: Option<PathBuf>,
    #[cfg_attr(not(feature = "proxmox"), allow(dead_code))]
    pub timeout: f64,
    pub errors: Vec<String>,
}

// The secret must not reach a log line through a debug print.
impl std::fmt::Debug for NodeSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeSettings")
            .field("key", &self.key)
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("api_node", &self.api_node)
            .field("token_id", &"[redacted]")
            .field("token_secret", &"[redacted]")
            .field("verify_tls", &self.verify_tls)
            .field("ca_cert", &self.ca_cert)
            .field("errors", &self.errors)
            .finish()
    }
}

impl NodeSettings {
    /// True only when enough detail exists to attempt an authenticated call.
    pub fn configured(&self) -> bool {
        !self.host.is_empty()
            && !self.token_id.is_empty()
            && !self.token_secret.is_empty()
            && self.errors.is_empty()
    }

    /// Reason the node is not being polled, or None when it is.
    pub fn unconfigured_reason(&self) -> Option<String> {
        if !self.errors.is_empty() {
            return Some(self.errors.join("; "));
        }
        if self.host.is_empty() {
            return Some("Host not set".into());
        }
        if self.token_id.is_empty() || self.token_secret.is_empty() {
            return Some("API token not set".into());
        }
        None
    }

    pub fn status_when_unpolled(&self) -> &'static str {
        if !self.errors.is_empty() { STATUS_CONFIG_ERROR } else { STATUS_UNCONFIGURED }
    }

    #[cfg_attr(not(feature = "proxmox"), allow(dead_code))]
    pub fn base_url(&self) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("https://{host}:{}/api2/json", self.port)
    }

    /// Frontend-safe description. Token fields are deliberately absent.
    pub fn public(&self) -> Value {
        let set = !self.host.is_empty();
        json!({
            "key": self.key,
            "name": self.name,
            "host": if set { Value::from(self.host.clone()) } else { Value::Null },
            "port": if set { Value::from(self.port) } else { Value::Null },
            "api_node": if set { Value::from(self.api_node.clone()) } else { Value::Null },
            "configured": self.configured(),
            "verify_tls": self.verify_tls,
            "reason": self.unconfigured_reason(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub bind_host: String,
    pub port: u16,
    pub browser: BrowserSection,
    pub title: String,
    pub page_seconds: f64,
    pub usage: Thresholds,
    pub temperature: Thresholds,
    pub intervals: Intervals,
    pub history: HistorySettings,
    pub host: HostSection,
    pub nodes: Vec<NodeSettings>,
    pub warnings: Vec<String>,
    pub config_path: PathBuf,
    pub config_found: bool,
}

impl Settings {
    /// Whether the server is reachable only from this machine.
    pub fn loopback_only(&self) -> bool {
        matches!(self.bind_host.as_str(), "127.0.0.1" | "localhost" | "::1")
    }

    pub fn url(&self) -> String {
        let host = match self.bind_host.as_str() {
            "0.0.0.0" | "::" => "127.0.0.1".to_string(),
            "::1" => "[::1]".to_string(),
            other => other.to_string(),
        };
        format!("http://{host}:{}/", self.port)
    }

    /// Address a local client should dial to reach this server.
    pub fn dial_host(&self) -> String {
        match self.bind_host.as_str() {
            "0.0.0.0" => "127.0.0.1".into(),
            "::" => "::1".into(),
            other => other.to_string(),
        }
    }

    /// Configuration exposed to the browser. Contains no credentials.
    pub fn public(&self, host_label: &str) -> Value {
        json!({
            "app": {"name": crate::paths::APP_NAME, "version": env!("CARGO_PKG_VERSION")},
            "display": {"title": self.title, "page_seconds": self.page_seconds},
            "thresholds": {
                "usage": {"warning": self.usage.warning, "critical": self.usage.critical},
                "temperature": {"warning": self.temperature.warning, "critical": self.temperature.critical},
            },
            "intervals_seconds": {
                "cpu_mem": self.intervals.cpu_mem,
                "net": self.intervals.net,
                "disk": self.intervals.disk,
                "processes": self.intervals.processes,
                "temperature": self.intervals.temperature,
                "disk_health": self.intervals.disk_health,
                "proxmox": self.intervals.proxmox,
                "stream_push": self.intervals.stream_push,
            },
            "host": {
                "key": "host",
                "label": host_label,
                "platform": std::env::consts::OS,
            },
            "nodes": self.nodes.iter().map(NodeSettings::public).collect::<Vec<_>>(),
            "config_path": self.config_path.display().to_string(),
            "config_warnings": self.warnings,
        })
    }
}

/// Overrides supplied on the command line or by environment variables.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub bind_host: Option<String>,
    pub port: Option<u16>,
    pub env_file: Option<PathBuf>,
}

// --------------------------------------------------------------------- loading

/// Load, overlay and validate. Never fails: problems become warnings.
pub fn load(path: &Path, overrides: &Overrides) -> Settings {
    let mut warnings = Vec::new();
    let (mut file, found) = read_file(path, &mut warnings);

    if let Some(env_path) = &overrides.env_file {
        apply_legacy_env(&mut file, env_path, &mut warnings);
    }
    if let Ok(host) = std::env::var("PLH_HOST") {
        if !host.trim().is_empty() {
            file.server.host = host.trim().to_owned();
        }
    }
    if let Ok(port) = std::env::var("PLH_PORT") {
        match port.trim().parse::<u16>() {
            Ok(p) if p > 0 => file.server.port = p,
            _ => warnings.push(format!("PLH_PORT={port:?} is not a valid port; ignored")),
        }
    }
    if let Some(host) = &overrides.bind_host {
        file.server.host = host.clone();
    }
    if let Some(port) = overrides.port {
        file.server.port = port;
    }

    let base_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    validate(file, &base_dir, path, found, warnings)
}

fn read_file(path: &Path, warnings: &mut Vec<String>) -> (FileConfig, bool) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (FileConfig::default(), false),
        Err(e) => {
            warnings.push(format!("Could not read {}: {e}; using defaults", path.display()));
            return (FileConfig::default(), false);
        }
    };
    match parse(&text) {
        Ok(cfg) => (cfg, true),
        Err(e) => {
            warnings.push(format!(
                "{} is not valid ({}); using defaults until it is fixed",
                path.display(),
                e.trim()
            ));
            (FileConfig::default(), true)
        }
    }
}

pub fn parse(text: &str) -> Result<FileConfig, String> {
    toml::from_str::<FileConfig>(text).map_err(|e| e.to_string())
}

fn bounded(name: &str, value: f64, default: f64, low: f64, high: f64, warnings: &mut Vec<String>) -> f64 {
    if value.is_finite() && (low..=high).contains(&value) {
        value
    } else {
        warnings.push(format!("{name}={value} outside {low}..{high}; using {default}"));
        default
    }
}

fn resolve_path(base: &Path, raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() { p } else { base.join(p) }
}

/// Turn the file shape into runtime settings, recording every correction.
pub fn validate(
    file: FileConfig,
    base_dir: &Path,
    config_path: &Path,
    found: bool,
    mut warnings: Vec<String>,
) -> Settings {
    let d = IntervalSection::default();
    let i = &file.intervals;
    let intervals = Intervals {
        cpu_mem: bounded("intervals.cpu_mem", i.cpu_mem, d.cpu_mem, 0.5, 60.0, &mut warnings),
        net: bounded("intervals.net", i.net, d.net, 0.5, 60.0, &mut warnings),
        disk: bounded("intervals.disk", i.disk, d.disk, 1.0, 300.0, &mut warnings),
        processes: bounded("intervals.processes", i.processes, d.processes, 2.0, 600.0, &mut warnings),
        temperature: bounded("intervals.temperature", i.temperature, d.temperature, 1.0, 300.0, &mut warnings),
        disk_health: bounded("intervals.disk_health", i.disk_health, d.disk_health, 10.0, 3600.0, &mut warnings),
        proxmox: bounded("intervals.proxmox", i.proxmox, d.proxmox, 1.0, 300.0, &mut warnings),
        stream_push: bounded("intervals.stream_push", i.stream_push, d.stream_push, 0.25, 30.0, &mut warnings),
        stale_after: bounded("intervals.stale_after", i.stale_after, d.stale_after, 2.0, 600.0, &mut warnings),
        sensor_backoff: bounded("intervals.sensor_backoff", i.sensor_backoff, d.sensor_backoff, 5.0, 3600.0, &mut warnings),
        history_step: bounded("intervals.history_step", i.history_step, d.history_step, 0.5, 10.0, &mut warnings),
    };

    // Capacity is derived rather than configured, so the window is always the
    // span the user asked for whatever the step is. Both ends are bounded
    // first, which keeps the point count off the pathological end: a 0.5 s
    // step over 600 s is 1200 points per series, and nothing larger is
    // reachable.
    let hs = &file.history;
    let history_seconds =
        bounded("history.seconds", hs.seconds, HistorySection::default().seconds, 10.0, 600.0, &mut warnings);
    let history = HistorySettings {
        capacity: if hs.enabled { (history_seconds / intervals.history_step).ceil() as usize } else { 0 },
        step_seconds: intervals.history_step,
    };

    let t = &file.thresholds;
    let mut warn = bounded("thresholds.usage_warning", t.usage_warning, 70.0, 0.0, 100.0, &mut warnings);
    let mut crit = bounded("thresholds.usage_critical", t.usage_critical, 90.0, 0.0, 100.0, &mut warnings);
    if warn > crit {
        warnings.push(format!("usage_warning ({warn}) above usage_critical ({crit}); values swapped"));
        std::mem::swap(&mut warn, &mut crit);
    }
    // Degrees and percent are different scales, so temperature limits are
    // configured and validated independently of utilisation limits.
    let mut twarn = bounded("thresholds.temp_warning_c", t.temp_warning_c, 75.0, 0.0, 150.0, &mut warnings);
    let mut tcrit = bounded("thresholds.temp_critical_c", t.temp_critical_c, 90.0, 0.0, 150.0, &mut warnings);
    if twarn > tcrit {
        warnings.push(format!("temp_warning_c ({twarn}) above temp_critical_c ({tcrit}); values swapped"));
        std::mem::swap(&mut twarn, &mut tcrit);
    }

    let mut browser = file.browser.clone();
    let kinds = ["auto", "chrome", "edge", "chromium", "firefox", "default", "none"];
    if !kinds.contains(&browser.kind.as_str()) {
        warnings.push(format!("browser.kind={:?} is not one of {kinds:?}; using auto", browser.kind));
        browser.kind = "auto".into();
    }
    if browser.window_width > 16384 || browser.window_height > 16384 {
        warnings.push("browser window size above 16384; using browser default".into());
        browser.window_width = 0;
        browser.window_height = 0;
    }

    let port = if file.server.port == 0 {
        warnings.push("server.port=0 is not usable; using 8765".into());
        8765
    } else {
        file.server.port
    };
    let bind_host = if file.server.host.trim().is_empty() {
        "127.0.0.1".to_string()
    } else {
        file.server.host.trim().to_string()
    };
    if !matches!(bind_host.as_str(), "127.0.0.1" | "localhost" | "::1") {
        warnings.push(format!(
            "server.host={bind_host} exposes the dashboard beyond this machine without authentication"
        ));
    }

    let page_seconds =
        bounded("display.page_seconds", file.display.page_seconds, 10.0, 2.0, 600.0, &mut warnings);

    let mut nodes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (idx, section) in file.nodes.iter().enumerate() {
        let node = validate_node(idx + 1, section, base_dir, &mut warnings);
        if !node.host.is_empty() && !seen.insert((node.host.to_lowercase(), node.port)) {
            warnings.push(format!(
                "{}:{} is listed twice; its figures would be shown twice",
                node.host, node.port
            ));
        }
        nodes.push(node);
    }

    Settings {
        bind_host,
        port,
        browser,
        title: if file.display.title.trim().is_empty() {
            "PLH RACK MONITOR".into()
        } else {
            file.display.title.clone()
        },
        page_seconds,
        usage: Thresholds { warning: warn, critical: crit },
        temperature: Thresholds { warning: twarn, critical: tcrit },
        intervals,
        history,
        host: file.host.clone(),
        nodes,
        warnings,
        config_path: config_path.to_path_buf(),
        config_found: found,
    }
}

fn validate_node(index: usize, s: &NodeSection, base_dir: &Path, warnings: &mut Vec<String>) -> NodeSettings {
    let mut errors = Vec::new();
    let name = if s.name.trim().is_empty() { format!("PVE{index:02}") } else { s.name.trim().to_string() };
    let label = format!("proxmox[{index}] ({name})");

    let timeout = bounded(&format!("{label}.timeout"), s.timeout, 4.0, 0.5, 30.0, warnings);
    let port = if s.port == 0 {
        errors.push("port 0 is not usable".to_string());
        8006
    } else {
        s.port
    };

    let token_id = s.token_id.trim().to_string();
    // user@realm!tokenname: a wrong shape is a configuration error, not an
    // authentication failure to be discovered later.
    if !token_id.is_empty() && (!token_id.contains('!') || !token_id.contains('@')) {
        errors.push("token_id must look like user@realm!tokenname".to_string());
    }

    let sources = [!s.token_secret.is_empty(), !s.token_secret_env.is_empty(), !s.token_secret_file.is_empty()]
        .iter()
        .filter(|set| **set)
        .count();
    if sources > 1 {
        warnings.push(format!(
            "{label}: more than one of token_secret, token_secret_env, token_secret_file is set; using the first of env, file, inline"
        ));
    }
    let token_secret = if !s.token_secret_env.is_empty() {
        match std::env::var(&s.token_secret_env) {
            Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => {
                errors.push(format!("environment variable {} is not set", s.token_secret_env));
                String::new()
            }
        }
    } else if !s.token_secret_file.is_empty() {
        let path = resolve_path(base_dir, &s.token_secret_file);
        match std::fs::read_to_string(&path) {
            Ok(v) => v.trim().to_string(),
            Err(e) => {
                errors.push(format!("cannot read token_secret_file {}: {e}", path.display()));
                String::new()
            }
        }
    } else {
        s.token_secret.trim().to_string()
    };

    // A named CA that cannot be read is reported rather than silently
    // replaced by the system trust store, which would change what is trusted.
    let ca_cert = if s.ca_cert.trim().is_empty() {
        None
    } else {
        let path = resolve_path(base_dir, s.ca_cert.trim());
        if path.is_file() {
            Some(path)
        } else {
            errors.push(format!("CA certificate not found: {}", path.display()));
            None
        }
    };

    if !s.host.trim().is_empty() && !s.verify_tls {
        warnings.push(format!("{label}: verify_tls=false - TLS verification is disabled by configuration"));
    }
    warnings.extend(errors.iter().map(|e| format!("{label}: {e}")));

    NodeSettings {
        key: format!("node{index}"),
        name,
        host: s.host.trim().to_string(),
        port,
        api_node: s.node.trim().to_string(),
        token_id,
        token_secret,
        verify_tls: s.verify_tls,
        ca_cert,
        timeout,
        errors,
    }
}

// ------------------------------------------------------------- legacy .env

/// Parse .env text the way python-dotenv does, since that is what wrote and
/// read these files. Unquoted values keep backslashes literally, so a Windows
/// path such as C:\Users\x\ca.pem survives intact; escapes are processed only
/// inside double quotes; single quotes are fully literal; " #" starts an
/// inline comment in an unquoted value.
pub fn parse_dotenv(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map(str::trim_start).unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
            continue;
        }
        out.push((key.to_string(), dotenv_value(value.trim())));
    }
    out
}

fn dotenv_value(value: &str) -> String {
    if let Some(rest) = value.strip_prefix('\'') {
        return rest.split('\'').next().unwrap_or("").to_string();
    }
    if let Some(rest) = value.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some(q @ ('"' | '\\')) => out.push(q),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                },
                other => out.push(other),
            }
        }
        return out;
    }
    let unquoted = match value.find(" #") {
        Some(i) => &value[..i],
        None => value,
    };
    unquoted.trim().to_string()
}

/// Overlay settings from the Python edition's .env file.
///
/// Relative paths in the file are resolved against the file's own directory,
/// which is where the Python edition ran from.
pub fn apply_legacy_env(file: &mut FileConfig, path: &Path, warnings: &mut Vec<String>) {
    let entries = match std::fs::read_to_string(path) {
        Ok(text) => parse_dotenv(&text),
        Err(e) => {
            warnings.push(format!("Could not read {}: {e}", path.display()));
            return;
        }
    };
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let get = |key: &str| {
        entries
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.trim().to_string())
    };

    fn num<T: std::str::FromStr>(key: &str, raw: Option<String>, slot: &mut T, warnings: &mut Vec<String>) {
        if let Some(raw) = raw.filter(|r| !r.is_empty()) {
            match raw.parse::<T>() {
                Ok(v) => *slot = v,
                Err(_) => warnings.push(format!("{key}={raw:?} in .env is not a valid number; ignored")),
            }
        }
    }
    fn flag(key: &str, raw: Option<String>, slot: &mut bool, warnings: &mut Vec<String>) {
        if let Some(raw) = raw.filter(|r| !r.is_empty()) {
            match raw.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => *slot = true,
                "0" | "false" | "no" | "off" => *slot = false,
                _ => warnings.push(format!("{key}={raw:?} in .env is not a boolean; ignored")),
            }
        }
    }

    if let Some(v) = get("APP_HOST").filter(|v| !v.is_empty()) {
        file.server.host = v;
    }
    num("APP_PORT", get("APP_PORT"), &mut file.server.port, warnings);
    num("THRESHOLD_WARNING", get("THRESHOLD_WARNING"), &mut file.thresholds.usage_warning, warnings);
    num("THRESHOLD_CRITICAL", get("THRESHOLD_CRITICAL"), &mut file.thresholds.usage_critical, warnings);
    num("TEMP_WARNING_C", get("TEMP_WARNING_C"), &mut file.thresholds.temp_warning_c, warnings);
    num("TEMP_CRITICAL_C", get("TEMP_CRITICAL_C"), &mut file.thresholds.temp_critical_c, warnings);

    let iv = &mut file.intervals;
    num("INTERVAL_CPU_MEM", get("INTERVAL_CPU_MEM"), &mut iv.cpu_mem, warnings);
    num("INTERVAL_NET", get("INTERVAL_NET"), &mut iv.net, warnings);
    num("INTERVAL_DISK", get("INTERVAL_DISK"), &mut iv.disk, warnings);
    num("INTERVAL_PROCESSES", get("INTERVAL_PROCESSES"), &mut iv.processes, warnings);
    num("INTERVAL_TEMP", get("INTERVAL_TEMP"), &mut iv.temperature, warnings);
    num("INTERVAL_DISK_HEALTH", get("INTERVAL_DISK_HEALTH"), &mut iv.disk_health, warnings);
    num("INTERVAL_PROXMOX", get("INTERVAL_PROXMOX"), &mut iv.proxmox, warnings);
    num("STREAM_PUSH_INTERVAL", get("STREAM_PUSH_INTERVAL"), &mut iv.stream_push, warnings);
    num("STALE_AFTER_SECONDS", get("STALE_AFTER_SECONDS"), &mut iv.stale_after, warnings);
    num("SENSOR_BACKOFF_SECONDS", get("SENSOR_BACKOFF_SECONDS"), &mut iv.sensor_backoff, warnings);

    let h = &mut file.host;
    if let Some(v) = get("DISK_MOUNTS") {
        let mounts: Vec<String> = v.split(',').map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect();
        if !mounts.is_empty() {
            h.disks = mounts;
        }
    }
    flag("DISK_AUTODISCOVER", get("DISK_AUTODISCOVER"), &mut h.autodiscover_disks, warnings);
    if let Some(v) = get("PRIMARY_DISK_MOUNT").filter(|v| !v.is_empty()) {
        h.primary_disk = v;
    }
    flag("DISK_HEALTH_ENABLED", get("DISK_HEALTH_ENABLED"), &mut h.disk_health, warnings);
    flag("TEMP_ENABLED", get("TEMP_ENABLED"), &mut h.temperature, warnings);
    if let Some(v) = get("LHM_HTTP_URL").filter(|v| !v.is_empty()) {
        h.lhm_url = v;
    }
    flag("LHM_WMI_ENABLED", get("LHM_WMI_ENABLED"), &mut h.lhm_wmi, warnings);

    // Nodes are numbered from 1 with no fixed upper bound; the highest index
    // that appears decides how many there are.
    let shared_ca = get("PROXMOX_CA_CERT_PATH").filter(|v| !v.is_empty());
    let highest = entries
        .iter()
        .filter_map(|(k, _)| k.strip_prefix("PROXMOX_NODE_"))
        .filter_map(|rest| rest.split('_').next()?.parse::<usize>().ok())
        .max()
        .unwrap_or(0);
    if highest > 0 {
        file.nodes.clear();
    }
    for n in 1..=highest {
        let key = |field: &str| format!("PROXMOX_NODE_{n}_{field}");
        let mut node = NodeSection { name: format!("PVE{n:02}"), ..NodeSection::default() };
        if let Some(v) = get(&key("NAME")).filter(|v| !v.is_empty()) {
            node.name = v;
        }
        node.host = get(&key("HOST")).unwrap_or_default();
        num(&key("PORT"), get(&key("PORT")), &mut node.port, warnings);
        node.node = get(&key("API_NODE")).unwrap_or_default();
        node.token_id = get(&key("TOKEN_ID")).unwrap_or_default();
        node.token_secret = get(&key("TOKEN_SECRET")).unwrap_or_default();
        num(&key("TIMEOUT_SECONDS"), get(&key("TIMEOUT_SECONDS")), &mut node.timeout, warnings);
        flag(&key("VERIFY_TLS"), get(&key("VERIFY_TLS")), &mut node.verify_tls, warnings);
        if let Some(ca) = get(&key("CA_CERT_PATH")).filter(|v| !v.is_empty()).or_else(|| shared_ca.clone()) {
            node.ca_cert = resolve_path(&base, &ca).display().to_string();
        }
        file.nodes.push(node);
    }
}

/// Serialise a configuration back to TOML, for `import-env`.
pub fn to_toml(file: &FileConfig) -> String {
    let body = toml::to_string_pretty(file).unwrap_or_default();
    format!(
        "# PLH Rack Monitor configuration.\n# Generated by `plh-rack-monitor import-env`. See config.example.toml for every option.\n\n{body}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_from(text: &str) -> Settings {
        let file = parse(text).expect("valid toml");
        validate(file, Path::new("."), Path::new("config.toml"), true, Vec::new())
    }

    #[test]
    fn defaults_bind_to_loopback() {
        let s = validate(FileConfig::default(), Path::new("."), Path::new("c.toml"), false, Vec::new());
        assert_eq!(s.bind_host, "127.0.0.1");
        assert_eq!(s.port, 8765);
        assert!(s.loopback_only());
        assert!(s.nodes.is_empty());
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    }

    #[test]
    fn template_parses_cleanly() {
        let file = parse(TEMPLATE).expect("the shipped template must parse");
        let s = validate(file, Path::new("."), Path::new("c.toml"), true, Vec::new());
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    }

    #[test]
    fn history_capacity_is_derived_from_the_window_and_step() {
        let s = settings_from("[intervals]\nhistory_step = 2.0\n\n[history]\nseconds = 30.0\n");
        assert_eq!(s.history.step_seconds, 2.0);
        assert_eq!(s.history.capacity, 15);
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    }

    #[test]
    fn history_switched_off_leaves_no_window() {
        let s = settings_from("[history]\nenabled = false\n");
        assert_eq!(s.history.capacity, 0);
    }

    #[test]
    fn an_impossible_history_window_warns_and_uses_the_default() {
        let s = settings_from("[history]\nseconds = 99999.0\n");
        assert_eq!(s.history.capacity, 60);
        assert!(s.warnings.iter().any(|w| w.contains("history.seconds")), "{:?}", s.warnings);
    }

    #[test]
    fn unknown_key_is_reported_not_fatal() {
        let mut warnings = Vec::new();
        let dir = std::env::temp_dir().join(format!("plh-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("typo.toml");
        std::fs::write(&path, "[server]\nprot = 9000\n").unwrap();
        let (file, found) = read_file(&path, &mut warnings);
        assert!(found);
        assert_eq!(file.server.port, 8765);
        assert!(warnings[0].contains("prot"), "{warnings:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn out_of_range_interval_falls_back() {
        let s = settings_from("[intervals]\ncpu_mem = 0.01\n");
        assert_eq!(s.intervals.cpu_mem, 1.5);
        assert!(s.warnings.iter().any(|w| w.contains("intervals.cpu_mem")));
    }

    #[test]
    fn inverted_thresholds_are_swapped() {
        let s = settings_from("[thresholds]\nusage_warning = 95\nusage_critical = 70\n");
        assert_eq!(s.usage.warning, 70.0);
        assert_eq!(s.usage.critical, 95.0);
    }

    #[test]
    fn temperature_scale_is_independent() {
        let s = settings_from("[thresholds]\nusage_warning = 50\ntemp_warning_c = 80\n");
        assert_eq!(s.usage.state(Some(60.0)), "warning");
        assert_eq!(s.temperature.state(Some(60.0)), "normal");
    }

    #[test]
    fn threshold_states() {
        let t = Thresholds { warning: 70.0, critical: 90.0 };
        assert_eq!(t.state(None), "unavailable");
        assert_eq!(t.state(Some(10.0)), "normal");
        assert_eq!(t.state(Some(70.0)), "warning");
        assert_eq!(t.state(Some(90.0)), "critical");
    }

    #[test]
    fn malformed_token_id_is_config_error() {
        let s = settings_from(
            "[[proxmox]]\nhost = \"10.0.0.5\"\ntoken_id = \"no-realm\"\ntoken_secret = \"x\"\n",
        );
        let node = &s.nodes[0];
        assert!(!node.configured());
        assert_eq!(node.status_when_unpolled(), STATUS_CONFIG_ERROR);
        assert!(node.unconfigured_reason().unwrap().contains("user@realm"));
    }

    #[test]
    fn missing_ca_is_reported_and_node_not_polled() {
        let s = settings_from(
            "[[proxmox]]\nhost = \"10.0.0.5\"\ntoken_id = \"m@pve!p\"\ntoken_secret = \"x\"\nca_cert = \"nope/ca.pem\"\n",
        );
        let node = &s.nodes[0];
        assert!(!node.configured());
        assert!(node.unconfigured_reason().unwrap().contains("CA certificate not found"));
        assert!(node.verify_tls);
    }

    #[test]
    fn node_without_host_is_unconfigured_not_error() {
        let s = settings_from("[[proxmox]]\nname = \"PVE02\"\n");
        assert_eq!(s.nodes[0].status_when_unpolled(), STATUS_UNCONFIGURED);
        assert_eq!(s.nodes[0].unconfigured_reason().unwrap(), "Host not set");
    }

    #[test]
    fn disabling_tls_warns() {
        let s = settings_from(
            "[[proxmox]]\nhost = \"10.0.0.5\"\ntoken_id = \"m@pve!p\"\ntoken_secret = \"x\"\nverify_tls = false\n",
        );
        assert!(s.nodes[0].configured());
        assert!(s.warnings.iter().any(|w| w.contains("TLS verification is disabled")));
    }

    #[test]
    fn secret_from_environment_variable() {
        // SAFETY: the variable name is unique to this test.
        unsafe { std::env::set_var("PLH_TEST_SECRET_A", "from-env") };
        let s = settings_from(
            "[[proxmox]]\nhost = \"h\"\ntoken_id = \"m@pve!p\"\ntoken_secret_env = \"PLH_TEST_SECRET_A\"\n",
        );
        assert_eq!(s.nodes[0].token_secret, "from-env");
        assert!(s.nodes[0].configured());
    }

    #[test]
    fn missing_secret_variable_is_config_error() {
        let s = settings_from(
            "[[proxmox]]\nhost = \"h\"\ntoken_id = \"m@pve!p\"\ntoken_secret_env = \"PLH_TEST_UNSET_VARIABLE_XYZ\"\n",
        );
        assert_eq!(s.nodes[0].status_when_unpolled(), STATUS_CONFIG_ERROR);
    }

    #[test]
    fn public_config_never_contains_credentials() {
        let secret = "3f8b1c2d-0000-4444-8888-aaaaaaaaaaaa";
        let s = settings_from(&format!(
            "[[proxmox]]\nhost = \"10.0.0.5\"\ntoken_id = \"monitor@pve!plh\"\ntoken_secret = \"{secret}\"\n"
        ));
        let body = s.public("host").to_string();
        assert!(!body.contains(secret));
        assert!(!body.contains("monitor@pve!plh"));
        assert!(!format!("{:?}", s.nodes[0]).contains(secret));
    }

    #[test]
    fn duplicate_node_is_warned() {
        let s = settings_from(
            "[[proxmox]]\nhost = \"a\"\n[[proxmox]]\nhost = \"A\"\n",
        );
        assert!(s.warnings.iter().any(|w| w.contains("listed twice")));
    }

    #[test]
    fn exposing_beyond_loopback_warns() {
        let s = settings_from("[server]\nhost = \"0.0.0.0\"\n");
        assert!(!s.loopback_only());
        assert!(s.warnings.iter().any(|w| w.contains("without authentication")));
        assert_eq!(s.url(), "http://127.0.0.1:8765/");
    }

    #[test]
    fn legacy_env_maps_nodes_and_paths() {
        let dir = std::env::temp_dir().join(format!("plh-env-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("certs")).unwrap();
        std::fs::write(dir.join("certs").join("ca.pem"), "x").unwrap();
        let env = dir.join(".env");
        std::fs::write(
            &env,
            "APP_PORT=8766\nTHRESHOLD_WARNING=65\nINTERVAL_PROXMOX=5\n\
             PROXMOX_NODE_1_NAME=PVE\nPROXMOX_NODE_1_HOST=192.168.1.11\nPROXMOX_NODE_1_API_NODE=pve\n\
             PROXMOX_NODE_1_TOKEN_ID=monitor@pve!plh\nPROXMOX_NODE_1_TOKEN_SECRET=s3cret\n\
             PROXMOX_NODE_2_NAME=PVE02\nPROXMOX_NODE_2_HOST=\n\
             PROXMOX_CA_CERT_PATH=certs\\ca.pem\n",
        )
        .unwrap();
        let mut file = FileConfig::default();
        let mut warnings = Vec::new();
        apply_legacy_env(&mut file, &env, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(file.server.port, 8766);
        assert_eq!(file.thresholds.usage_warning, 65.0);
        assert_eq!(file.intervals.proxmox, 5.0);
        assert_eq!(file.nodes.len(), 2);
        assert_eq!(file.nodes[0].node, "pve");
        assert!(PathBuf::from(&file.nodes[0].ca_cert).is_absolute());
        assert_eq!(file.nodes[1].host, "");

        let s = validate(file, &dir, &dir.join("config.toml"), true, warnings);
        assert!(s.nodes[0].configured(), "{:?}", s.nodes[0].unconfigured_reason());
        assert_eq!(s.nodes[1].unconfigured_reason().unwrap(), "Host not set");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dotenv_keeps_windows_backslashes_literally() {
        // The exact shape of the real .env: an unquoted absolute Windows path.
        let parsed = parse_dotenv(
            "# comment\n\
             PROXMOX_CA_CERT_PATH=C:\\Users\\spenc\\Desktop\\PLH_Rack_Monitor\\certs\\pve-root-ca.pem\n\
             export APP_PORT=8766\n\
             QUOTED=\"a\\tb \\\"c\\\"\"\n\
             SINGLE='C:\\raw\\n # kept'\n\
             INLINE=value # trailing comment\n\
             EMPTY=\n\
             not a pair\n",
        );
        let get = |k: &str| parsed.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("PROXMOX_CA_CERT_PATH"), Some("C:\\Users\\spenc\\Desktop\\PLH_Rack_Monitor\\certs\\pve-root-ca.pem"));
        assert_eq!(get("APP_PORT"), Some("8766"));
        assert_eq!(get("QUOTED"), Some("a\tb \"c\""));
        assert_eq!(get("SINGLE"), Some("C:\\raw\\n # kept"));
        assert_eq!(get("INLINE"), Some("value"));
        assert_eq!(get("EMPTY"), Some(""));
        assert_eq!(parsed.len(), 6);
    }

    #[test]
    fn toml_round_trip_keeps_nodes() {
        let mut file = FileConfig::default();
        file.nodes.push(NodeSection { name: "PVE".into(), host: "h".into(), ..NodeSection::default() });
        let text = to_toml(&file);
        let back = parse(&text).expect("generated toml parses");
        assert_eq!(back.nodes.len(), 1);
        assert_eq!(back.nodes[0].name, "PVE");
    }
}
