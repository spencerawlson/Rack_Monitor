//! Samplers for the machine the dashboard runs on.
//!
//! Each sampler owns its sysinfo handle and its rate state, and each is
//! driven by exactly one thread. Nothing is shared between samplers, so a
//! slow disk query cannot delay CPU sampling and counter deltas are never
//! computed from another thread's baseline.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use sysinfo::{DiskKind, Disks, Networks, ProcessesToUpdate, System};

use super::cputimes::{self, CpuTimes, Flavor};
use super::platform;
use crate::config::HostSection;
use crate::model::{
    CoreUsage, CpuMetrics, DiskIo, Filesystem, Interface, LoadAverage, MemoryMetrics, Network, PerDiskIo,
    Processes, SwapMetrics, SystemInfo,
};
use crate::rates::{RateTracker, percent_of, round1};

/// A shorter gap than this between CPU samples amplifies scheduler jitter
/// into a meaningless percentage.
const MIN_CPU_DELTA: Duration = Duration::from_millis(200);

// --------------------------------------------------------------- cpu, memory

pub struct CpuMemSampler {
    sys: System,
    label: Option<String>,
    baseline: Option<(Vec<CpuTimes>, Instant)>,
    last_sysinfo_refresh: Instant,
    rates: RateTracker,
}

pub struct CpuMemReading {
    pub system: SystemInfo,
    pub cpu: CpuMetrics,
    pub memory: MemoryMetrics,
    pub swap: SwapMetrics,
}

impl CpuMemSampler {
    pub fn new(label: &str) -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        sys.refresh_cpu_frequency();
        let baseline = platform::cpu_sample().map(|s| (s.per_core, Instant::now()));
        Self {
            sys,
            label: (!label.trim().is_empty()).then(|| label.trim().to_string()),
            baseline,
            last_sysinfo_refresh: Instant::now(),
            rates: RateTracker::new(),
        }
    }

    pub fn sample(&mut self) -> CpuMemReading {
        CpuMemReading {
            system: self.system_info(),
            cpu: self.cpu(),
            memory: self.memory(),
            swap: self.swap(),
        }
    }

    fn system_info(&self) -> SystemInfo {
        let hostname = System::host_name();
        SystemInfo {
            label: self.label.clone().or_else(|| hostname.clone()),
            hostname,
            os_name: System::name(),
            os_version: System::long_os_version().or_else(System::os_version),
            kernel: System::kernel_version(),
            platform: std::env::consts::OS.to_string(),
            arch: System::cpu_arch(),
            cpu_name: self
                .sys
                .cpus()
                .first()
                .map(|c| c.brand().trim().to_string())
                .filter(|b| !b.is_empty()),
            boot_time: Some(System::boot_time()),
            uptime_seconds: Some(System::uptime()),
        }
    }

    fn cpu(&mut self) -> CpuMetrics {
        self.sys.refresh_cpu_frequency();
        let mut out = CpuMetrics {
            cores_physical: System::physical_core_count(),
            cores_logical: Some(self.sys.cpus().len()).filter(|n| *n > 0),
            freq_current_mhz: self
                .sys
                .cpus()
                .first()
                .map(|c| c.frequency() as f64)
                .filter(|f| *f > 0.0),
            load: load_average(),
            ..CpuMetrics::default()
        };

        let now = Instant::now();
        if let Some(sample) = platform::cpu_sample() {
            if let Some(n) = out.cores_logical.or(Some(sample.per_core.len())) {
                out.cores_logical = Some(n.max(sample.per_core.len()));
            }
            out.ctx_switches_per_sec = sample.ctx_switches.and_then(|v| self.rates.rate("ctx", v)).map(f64::round);
            out.interrupts_per_sec = sample.interrupts.and_then(|v| self.rates.rate("intr", v)).map(f64::round);

            match &self.baseline {
                Some((prev, at)) if now.duration_since(*at) >= MIN_CPU_DELTA && prev.len() == sample.per_core.len() => {
                    fill_from_times(&mut out, prev, &sample.per_core, sample.flavor);
                    self.baseline = Some((sample.per_core, now));
                }
                Some((prev, _)) if prev.len() == sample.per_core.len() => {
                    // Too soon: keep the older baseline so the next sample
                    // measures a full interval.
                }
                _ => self.baseline = Some((sample.per_core, now)),
            }
        } else if now.duration_since(self.last_sysinfo_refresh) >= MIN_CPU_DELTA {
            // No platform counters: sysinfo's own usage figures stand in, and
            // the per-state split is left unknown rather than estimated.
            self.sys.refresh_cpu_usage();
            self.last_sysinfo_refresh = now;
            out.percent = Some(round1(f64::from(self.sys.global_cpu_usage()).clamp(0.0, 100.0)));
            out.per_core = self
                .sys
                .cpus()
                .iter()
                .enumerate()
                .map(|(core, c)| CoreUsage { core, percent: Some(round1(f64::from(c.cpu_usage()).clamp(0.0, 100.0))) })
                .collect();
        }
        out
    }

    fn memory(&mut self) -> MemoryMetrics {
        self.sys.refresh_memory();
        let total = self.sys.total_memory();
        let used = self.sys.used_memory();
        MemoryMetrics {
            total_bytes: Some(total).filter(|t| *t > 0),
            used_bytes: Some(used),
            available_bytes: Some(self.sys.available_memory()),
            free_bytes: Some(self.sys.free_memory()),
            percent: percent_of(used as f64, total as f64),
        }
    }

    fn swap(&self) -> SwapMetrics {
        let total = self.sys.total_swap();
        let used = self.sys.used_swap();
        SwapMetrics {
            total_bytes: Some(total),
            used_bytes: Some(used),
            free_bytes: Some(self.sys.free_swap()),
            percent: percent_of(used as f64, total as f64),
        }
    }
}

fn fill_from_times(out: &mut CpuMetrics, prev: &[CpuTimes], cur: &[CpuTimes], flavor: Flavor) {
    let total = cputimes::breakdown(&CpuTimes::sum(prev), &CpuTimes::sum(cur), flavor);
    if let Some(b) = total {
        out.percent = Some(b.busy);
        out.user = Some(b.user);
        out.system = Some(b.system);
        out.idle = Some(b.idle);
        out.iowait = b.iowait;
        out.irq = b.irq;
        out.softirq = b.softirq;
        out.dpc = b.dpc;
        out.interrupt = b.interrupt;
    }
    out.per_core = prev
        .iter()
        .zip(cur)
        .enumerate()
        .map(|(core, (p, c))| CoreUsage { core, percent: cputimes::breakdown(p, c, flavor).map(|b| b.busy) })
        .collect();
}

fn load_average() -> LoadAverage {
    // Windows has no load average; sysinfo reports zeros there, which would
    // read as an idle machine, so the figure is marked unavailable instead.
    if cfg!(windows) {
        return LoadAverage::default();
    }
    let l = System::load_average();
    LoadAverage {
        min1: Some((l.one * 100.0).round() / 100.0),
        min5: Some((l.five * 100.0).round() / 100.0),
        min15: Some((l.fifteen * 100.0).round() / 100.0),
        available: true,
    }
}

// ------------------------------------------------------------------ network

pub struct NetSampler {
    networks: Networks,
    rates: RateTracker,
}

impl NetSampler {
    pub fn new() -> Self {
        Self { networks: Networks::new_with_refreshed_list(), rates: RateTracker::new() }
    }

    pub fn sample(&mut self) -> Network {
        self.networks.refresh(true);
        let links = platform::link_info();
        let mut names: Vec<&String> = self.networks.list().keys().collect();
        names.sort();

        let mut interfaces = Vec::new();
        let mut best: Option<(usize, (f64, u64))> = None;
        let mut best_ip: Option<(String, u8)> = None;
        for name in names {
            let data = &self.networks.list()[name];
            let recv = data.total_received();
            let sent = data.total_transmitted();
            let down = self.rates.rate(&format!("rx:{name}"), recv);
            let up = self.rates.rate(&format!("tx:{name}"), sent);
            let ipv4 = data
                .ip_networks()
                .iter()
                .find(|n| n.addr.is_ipv4())
                .map(|n| (n.addr.to_string(), n.prefix));
            let link = links.get(name.as_str()).copied().unwrap_or_default();

            let index = interfaces.len();
            interfaces.push(Interface {
                name: name.clone(),
                up: link.up,
                speed_mbps: link.speed_mbps,
                ip_address: ipv4.as_ref().map(|(a, _)| a.clone()),
                download_bytes_per_sec: down,
                upload_bytes_per_sec: up,
                bytes_recv: recv,
                bytes_sent: sent,
            });

            // The active interface: not loopback, not known to be down, has
            // an IPv4 address, and moves the most traffic; ties go to the
            // faster link.
            if is_loopback(name) || link.up == Some(false) || ipv4.is_none() {
                continue;
            }
            let score = (down.unwrap_or(0.0) + up.unwrap_or(0.0), link.speed_mbps.unwrap_or(0));
            if best.as_ref().is_none_or(|(_, s)| score > *s) {
                best = Some((index, score));
                best_ip = ipv4;
            }
        }
        let present: HashSet<String> = interfaces.iter().map(|i| i.name.clone()).collect();
        self.rates.retain(|k| k.split_once(':').is_some_and(|(_, n)| present.contains(n)));

        let mut out = Network { connectivity: "DOWN".into(), ..Network::default() };
        if let Some((index, _)) = best {
            let chosen = &interfaces[index];
            out.interface = Some(chosen.name.clone());
            out.ip_address = best_ip.as_ref().map(|(a, _)| a.clone());
            out.ip_mask_cidr = best_ip.as_ref().map(|(_, p)| *p);
            out.link_up = Some(chosen.up.unwrap_or(true));
            out.speed_mbps = chosen.speed_mbps;
            out.download_bytes_per_sec = chosen.download_bytes_per_sec;
            out.upload_bytes_per_sec = chosen.upload_bytes_per_sec;
            out.total_bytes_recv = Some(chosen.bytes_recv);
            out.total_bytes_sent = Some(chosen.bytes_sent);
            // Local link state only. No external host is contacted, so this
            // is connectivity to the local network, not to the internet.
            out.connectivity = "LINK UP".into();
        }
        out.per_interface = interfaces;
        out
    }
}

fn is_loopback(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "lo" || lower.starts_with("lo0") || lower.contains("loopback")
}

// -------------------------------------------------------------------- disks

pub struct DiskSampler {
    disks: Disks,
    rates: RateTracker,
    host: HostSection,
    primary: String,
}

pub struct DiskReading {
    pub filesystems: Vec<Filesystem>,
    pub io: DiskIo,
    pub primary_percent: Option<f64>,
    pub primary_mount: Option<String>,
}

impl DiskSampler {
    pub fn new(host: &HostSection) -> Self {
        let primary = if host.primary_disk.trim().is_empty() {
            platform::system_drive()
        } else {
            normalize_mount(host.primary_disk.trim())
        };
        Self { disks: Disks::new_with_refreshed_list(), rates: RateTracker::new(), host: host.clone(), primary }
    }

    pub fn sample(&mut self) -> DiskReading {
        self.disks.refresh(true);

        let mut filesystems: Vec<Filesystem> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let found: Vec<&sysinfo::Disk> = self.disks.list().iter().collect();

        // Configured mounts come first and stay listed while absent.
        let mut wanted: Vec<String> = self.host.disks.iter().map(|m| normalize_mount(m)).collect();
        if !wanted.iter().any(|m| same_mount(m, &self.primary)) && self.host.disks.is_empty() {
            wanted.insert(0, self.primary.clone());
        }
        for mount in &wanted {
            if !seen.insert(mount_key(mount)) {
                continue;
            }
            match found.iter().find(|d| same_mount(&d.mount_point().to_string_lossy(), mount)) {
                Some(disk) => filesystems.push(describe(disk)),
                None => filesystems.push(Filesystem { mount: mount.clone(), present: false, ..Filesystem::default() }),
            }
        }
        if self.host.autodiscover_disks {
            for disk in &found {
                let mount = disk.mount_point().to_string_lossy().into_owned();
                if seen.contains(&mount_key(&mount)) || is_pseudo(disk) {
                    continue;
                }
                if disk.is_removable() && !self.host.include_removable {
                    continue;
                }
                seen.insert(mount_key(&mount));
                filesystems.push(describe(disk));
            }
        }

        // Throughput per physical device. A device mounted in two places is
        // counted once.
        let mut per_disk = Vec::new();
        let mut devices = HashSet::new();
        let mut read_total: Option<f64> = None;
        let mut write_total: Option<f64> = None;
        for disk in &found {
            if is_pseudo(disk) {
                continue;
            }
            let device = disk.name().to_string_lossy().into_owned();
            let key = if device.is_empty() { disk.mount_point().to_string_lossy().into_owned() } else { device };
            if !devices.insert(key.clone()) {
                continue;
            }
            let usage = disk.usage();
            let read = self.rates.rate(&format!("r:{key}"), usage.total_read_bytes);
            let write = self.rates.rate(&format!("w:{key}"), usage.total_written_bytes);
            if let Some(r) = read {
                read_total = Some(read_total.unwrap_or(0.0) + r);
            }
            if let Some(w) = write {
                write_total = Some(write_total.unwrap_or(0.0) + w);
            }
            per_disk.push(PerDiskIo {
                name: display_mount(disk),
                read_bytes_per_sec: read,
                write_bytes_per_sec: write,
            });
        }
        self.rates.retain(|k| k.split_once(':').is_some_and(|(_, d)| devices.contains(d)));

        // The donut shows one volume, never a blend of several.
        let chosen = filesystems
            .iter()
            .find(|fs| fs.present && same_mount(&fs.mount, &self.primary))
            .or_else(|| filesystems.iter().find(|fs| fs.present));

        DiskReading {
            primary_percent: chosen.and_then(|fs| fs.percent),
            primary_mount: chosen.map(|fs| fs.mount.clone()),
            io: DiskIo { read_bytes_per_sec: read_total, write_bytes_per_sec: write_total, per_disk },
            filesystems,
        }
    }
}

fn describe(disk: &sysinfo::Disk) -> Filesystem {
    let total = disk.total_space();
    let free = disk.available_space();
    let used = total.saturating_sub(free);
    Filesystem {
        mount: disk.mount_point().to_string_lossy().into_owned(),
        device: Some(disk.name().to_string_lossy().into_owned()).filter(|n| !n.is_empty()),
        fstype: Some(disk.file_system().to_string_lossy().into_owned()).filter(|f| !f.is_empty()),
        kind: match disk.kind() {
            DiskKind::SSD => Some("SSD".into()),
            DiskKind::HDD => Some("HDD".into()),
            _ => None,
        },
        removable: Some(disk.is_removable()),
        total_bytes: Some(total),
        used_bytes: Some(used),
        free_bytes: Some(free),
        percent: percent_of(used as f64, total as f64),
        present: true,
    }
}

fn display_mount(disk: &sysinfo::Disk) -> String {
    let mount = disk.mount_point().to_string_lossy().into_owned();
    let name = disk.name().to_string_lossy().into_owned();
    if name.is_empty() || name == mount { mount } else { format!("{mount} ({name})") }
}

/// Filesystems that are not storage a person would want charted: kernel
/// interfaces, container layers, snap images and system-internal volumes.
fn is_pseudo(disk: &sysinfo::Disk) -> bool {
    let fs = disk.file_system().to_string_lossy().to_ascii_lowercase();
    let mount = disk.mount_point().to_string_lossy().into_owned();
    const FS: [&str; 16] = [
        "squashfs", "overlay", "tmpfs", "devtmpfs", "ramfs", "proc", "sysfs", "cgroup", "cgroup2", "autofs",
        "nsfs", "efivarfs", "fuse.snapfuse", "tracefs", "debugfs", "devfs",
    ];
    FS.contains(&fs.as_str())
        || mount.starts_with("/snap/")
        || mount.starts_with("/var/lib/docker/")
        || mount.starts_with("/run/")
        || mount.starts_with("/System/Volumes/")
        || mount == "/boot/efi"
        || mount.starts_with("/private/var/vm")
}

/// "C:" and "c:\" name the same mount on Windows.
pub fn normalize_mount(mount: &str) -> String {
    let m = mount.trim();
    if cfg!(windows) && m.len() == 2 && m.ends_with(':') {
        return format!("{m}\\");
    }
    m.to_string()
}

fn mount_key(mount: &str) -> String {
    let m = normalize_mount(mount);
    if cfg!(windows) { m.to_ascii_lowercase() } else { m }
}

fn same_mount(a: &str, b: &str) -> bool {
    mount_key(a) == mount_key(b)
}

// ---------------------------------------------------------------- processes

pub struct ProcSampler {
    fallback: Option<System>,
}

impl ProcSampler {
    pub fn new() -> Self {
        Self { fallback: None }
    }

    /// Process and thread counts. Walking every process is the most
    /// expensive sample, so it runs on its own slow loop.
    pub fn sample(&mut self) -> Processes {
        if let Some(counts) = platform::process_counts() {
            return Processes { total: Some(counts.total), running: counts.running, threads: counts.threads };
        }
        let sys = self.fallback.get_or_insert_with(System::new);
        sys.refresh_processes(ProcessesToUpdate::All, true);
        Processes { total: Some(sys.processes().len() as u64), running: None, threads: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_reads_after_an_interval() {
        let mut sampler = CpuMemSampler::new("");
        std::thread::sleep(Duration::from_millis(350));
        let reading = sampler.sample();
        let pct = reading.cpu.percent.expect("a percentage after 350 ms");
        assert!((0.0..=100.0).contains(&pct));
        assert!(!reading.cpu.per_core.is_empty());
        for core in &reading.cpu.per_core {
            if let Some(p) = core.percent {
                assert!((0.0..=100.0).contains(&p));
            }
        }
        assert!(reading.memory.total_bytes.unwrap() > 0);
        assert!(reading.system.hostname.is_some());
        assert_eq!(reading.system.label, reading.system.hostname);
    }

    #[test]
    fn cpu_too_soon_reports_none_rather_than_a_guess() {
        let mut sampler = CpuMemSampler::new("");
        let reading = sampler.sample();
        if platform::cpu_sample().is_some() {
            assert_eq!(reading.cpu.percent, None);
        }
    }

    #[test]
    fn label_overrides_hostname() {
        let mut sampler = CpuMemSampler::new("Rack Host");
        assert_eq!(sampler.sample().system.label.as_deref(), Some("Rack Host"));
    }

    #[test]
    fn configured_but_absent_disk_stays_listed() {
        let host = HostSection {
            disks: vec![platform::system_drive(), if cfg!(windows) { "Q:\\".into() } else { "/definitely/not/mounted".into() }],
            autodiscover_disks: false,
            ..HostSection::default()
        };
        let reading = DiskSampler::new(&host).sample();
        assert_eq!(reading.filesystems.len(), 2);
        assert!(reading.filesystems[0].present);
        let absent = &reading.filesystems[1];
        assert!(!absent.present);
        assert_eq!(absent.percent, None);
        assert_eq!(absent.total_bytes, None);
        // The donut uses the present system volume, not the absent one.
        assert!(reading.primary_percent.is_some());
    }

    #[test]
    fn disk_io_needs_two_samples() {
        let mut sampler = DiskSampler::new(&HostSection::default());
        let first = sampler.sample();
        assert_eq!(first.io.read_bytes_per_sec, None);
        std::thread::sleep(Duration::from_millis(50));
        let second = sampler.sample();
        if let Some(r) = second.io.read_bytes_per_sec {
            assert!(r >= 0.0);
        }
    }

    #[test]
    fn network_lists_interfaces() {
        let mut sampler = NetSampler::new();
        sampler.sample();
        let net = sampler.sample();
        if net.interface.is_some() {
            assert_eq!(net.connectivity, "LINK UP");
        } else {
            assert_eq!(net.connectivity, "DOWN");
        }
    }

    #[test]
    fn processes_are_counted() {
        let p = ProcSampler::new().sample();
        assert!(p.total.unwrap() > 1);
    }

    #[test]
    fn mount_normalisation() {
        if cfg!(windows) {
            assert_eq!(normalize_mount("C:"), "C:\\");
            assert!(same_mount("c:\\", "C:"));
        } else {
            assert!(same_mount("/", "/"));
        }
        assert!(is_loopback("Loopback Pseudo-Interface 1"));
        assert!(is_loopback("lo"));
        assert!(!is_loopback("Ethernet"));
    }
}
