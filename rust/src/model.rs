//! The snapshot served to the browser.
//!
//! Every measured field is optional. A missing sensor, an unmounted drive or
//! a truncated Proxmox reply produces null, which the dashboard renders as
//! N/A; nothing defaults to zero, because zero is a reading and null is the
//! absence of one.

use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub generated_at: String,
    pub service: ServiceState,
    pub host: HostMetrics,
    pub nodes: Vec<NodeMetrics>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ServiceState {
    pub status: String,
    pub started_at: String,
    pub uptime_seconds: f64,
    pub version: String,
    pub collectors: BTreeMap<String, CollectorState>,
    pub config_warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CollectorState {
    pub last_attempt: Option<String>,
    pub last_success: Option<String>,
    pub last_error: Option<String>,
    pub stale: bool,
    pub runs: u64,
    pub failures: u64,
}

// ------------------------------------------------------------------- host

#[derive(Debug, Clone, Default, Serialize)]
pub struct HostMetrics {
    pub system: SystemInfo,
    pub cpu: CpuMetrics,
    pub memory: MemoryMetrics,
    pub swap: SwapMetrics,
    pub filesystems: Vec<Filesystem>,
    pub disk_io: DiskIo,
    pub network: Network,
    pub processes: Processes,
    pub temperature: Temperature,
    pub disk_health: DiskHealth,
    pub primary: Primary,
    pub history: History,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SystemInfo {
    pub hostname: Option<String>,
    pub label: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub kernel: Option<String>,
    pub platform: String,
    pub arch: String,
    pub cpu_name: Option<String>,
    pub boot_time: Option<u64>,
    pub uptime_seconds: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LoadAverage {
    pub min1: Option<f64>,
    pub min5: Option<f64>,
    pub min15: Option<f64>,
    /// False where the operating system has no load average (Windows).
    pub available: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CoreUsage {
    pub core: usize,
    pub percent: Option<f64>,
}

/// CPU utilisation. The state split differs by platform: DPC and interrupt
/// time exist on Windows, iowait and softirq on Linux. Fields a platform does
/// not have are null rather than zero.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuMetrics {
    pub percent: Option<f64>,
    pub user: Option<f64>,
    pub system: Option<f64>,
    pub idle: Option<f64>,
    pub iowait: Option<f64>,
    pub irq: Option<f64>,
    pub softirq: Option<f64>,
    pub dpc: Option<f64>,
    pub interrupt: Option<f64>,
    pub cores_physical: Option<usize>,
    pub cores_logical: Option<usize>,
    pub freq_current_mhz: Option<f64>,
    pub ctx_switches_per_sec: Option<f64>,
    pub interrupts_per_sec: Option<f64>,
    pub per_core: Vec<CoreUsage>,
    pub load: LoadAverage,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryMetrics {
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub percent: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SwapMetrics {
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub percent: Option<f64>,
}

/// One mount point. `present = false` keeps a configured but detached drive
/// on the dashboard instead of letting it silently disappear.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Filesystem {
    pub mount: String,
    pub device: Option<String>,
    pub fstype: Option<String>,
    pub kind: Option<String>,
    pub removable: Option<bool>,
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub percent: Option<f64>,
    pub present: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PerDiskIo {
    pub name: String,
    pub read_bytes_per_sec: Option<f64>,
    pub write_bytes_per_sec: Option<f64>,
}

/// Disk activity, kept distinct from capacity utilisation.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DiskIo {
    pub read_bytes_per_sec: Option<f64>,
    pub write_bytes_per_sec: Option<f64>,
    pub per_disk: Vec<PerDiskIo>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Interface {
    pub name: String,
    pub up: Option<bool>,
    pub speed_mbps: Option<u64>,
    pub ip_address: Option<String>,
    pub download_bytes_per_sec: Option<f64>,
    pub upload_bytes_per_sec: Option<f64>,
    pub bytes_recv: u64,
    pub bytes_sent: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Network {
    pub interface: Option<String>,
    pub ip_address: Option<String>,
    pub ip_mask_cidr: Option<u8>,
    pub link_up: Option<bool>,
    pub speed_mbps: Option<u64>,
    pub download_bytes_per_sec: Option<f64>,
    pub upload_bytes_per_sec: Option<f64>,
    pub total_bytes_recv: Option<u64>,
    pub total_bytes_sent: Option<u64>,
    /// Local link state only; no external host is ever contacted.
    pub connectivity: String,
    pub per_interface: Vec<Interface>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Processes {
    pub total: Option<u64>,
    pub running: Option<u64>,
    pub threads: Option<u64>,
}

/// A reading is reported only when a real sensor supplied it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Temperature {
    pub cpu_celsius: Option<f64>,
    pub source: Option<String>,
    pub available: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PhysicalDisk {
    pub name: Option<String>,
    pub media_type: Option<String>,
    pub health: Option<String>,
    pub operational: Option<String>,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DiskHealth {
    pub available: bool,
    pub detail: Option<String>,
    pub disks: Vec<PhysicalDisk>,
}

/// The three values behind a section's donut charts.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Primary {
    pub cpu_percent: Option<f64>,
    pub memory_percent: Option<f64>,
    pub disk_percent: Option<f64>,
    pub disk_mount: Option<String>,
}

/// A rolling window of the same three percentages, oldest first, behind the
/// sparklines under the donuts.
///
/// Points are evenly spaced, so the point at index `i` of a series of length
/// `n` was taken `(n - 1 - i) * step_seconds` before `generated_at` and needs
/// no timestamp of its own. A null is a gap in collection, never a zero.
///
/// An empty series means history is switched off or nothing has been
/// collected yet; the dashboard then shows no chart rather than a flat line.
#[derive(Debug, Clone, Default, Serialize)]
pub struct History {
    pub step_seconds: f64,
    pub capacity: usize,
    pub cpu: Vec<Option<f64>>,
    pub memory: Vec<Option<f64>>,
    pub disk: Vec<Option<f64>>,
}

// ----------------------------------------------------------------- proxmox

#[derive(Debug, Clone, Default, Serialize)]
pub struct Guests {
    pub running: Option<u64>,
    pub total: Option<u64>,
    /// False when the token may not list guests; never shown as 0/0.
    pub permitted: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StorageEntry {
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub percent: Option<f64>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NodeMetrics {
    pub key: String,
    pub name: String,
    pub status: String,
    pub api_node: Option<String>,
    pub message: Option<String>,
    pub last_attempt: Option<String>,
    pub last_success: Option<String>,
    pub stale: bool,
    pub consecutive_failures: u32,
    pub primary: Primary,
    pub cpu_percent: Option<f64>,
    pub cpu_count: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub memory_used_bytes: Option<u64>,
    pub memory_percent: Option<f64>,
    pub rootfs_total_bytes: Option<u64>,
    pub rootfs_used_bytes: Option<u64>,
    pub rootfs_percent: Option<f64>,
    pub swap_percent: Option<f64>,
    pub uptime_seconds: Option<f64>,
    pub load: LoadAverage,
    pub pve_version: Option<String>,
    pub kernel: Option<String>,
    pub temperature: Temperature,
    pub storage: Vec<StorageEntry>,
    pub storage_total_bytes: Option<u64>,
    pub storage_available_bytes: Option<u64>,
    pub vms: Option<Guests>,
    pub containers: Option<Guests>,
    /// Filled in when the snapshot is assembled; the collector that builds a
    /// node leaves it empty, because the window outlives any single poll.
    pub history: History,
}

pub fn now_iso() -> String {
    humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string()
}
