//! Collection loops, the current snapshot, and its broadcast.
//!
//! Each blocking metric family (CPU and memory, network, disks, processes,
//! temperature, drive health) runs on its own thread at its own interval.
//! Each Proxmox node runs as its own async task, so one slow node delays
//! nothing but itself. A loop is strictly sequential, which is what prevents
//! overlapping requests and keeps counter deltas correct.
//!
//! A step that panics is recorded and retried on the next tick; a loop never
//! ends except at shutdown, because one failing source must not stop others.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::collect::health::HealthSampler;
use crate::collect::host::{CpuMemSampler, DiskSampler, NetSampler, ProcSampler};
use crate::collect::temperature::TemperatureSampler;
use crate::config::Settings;
use crate::history::Series;
use crate::model::{CollectorState, HostMetrics, NodeMetrics, Primary, ServiceState, Snapshot, now_iso};

/// Shutdown signal usable from both threads and async tasks.
pub struct Shutdown {
    flag: Mutex<bool>,
    cv: Condvar,
    tx: watch::Sender<bool>,
}

impl Shutdown {
    fn new() -> Self {
        Self { flag: Mutex::new(false), cv: Condvar::new(), tx: watch::channel(false).0 }
    }

    pub fn trigger(&self) {
        *self.flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cv.notify_all();
        self.tx.send_replace(true);
    }

    pub fn is_set(&self) -> bool {
        *self.flag.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sleep up to `duration`; true when shutdown arrived meanwhile.
    fn wait(&self, duration: Duration) -> bool {
        let guard = self.flag.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = self
            .cv
            .wait_timeout_while(guard, duration, |stop| !*stop)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }

    /// Async sleep up to `duration`; true when shutdown arrived meanwhile.
    async fn wait_async(&self, duration: Duration) -> bool {
        let mut rx = self.tx.subscribe();
        tokio::select! {
            _ = rx.wait_for(|stop| *stop) => true,
            _ = tokio::time::sleep(duration) => self.is_set(),
        }
    }

    /// Resolve once shutdown has been triggered.
    pub async fn wait_forever(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|stop| *stop).await;
    }
}

struct Track {
    created: Instant,
    interval: f64,
    last_attempt: Option<String>,
    last_success: Option<String>,
    last_success_at: Option<Instant>,
    last_error: Option<String>,
    runs: u64,
    failures: u64,
}

impl Track {
    fn new(interval: f64) -> Self {
        Self {
            created: Instant::now(),
            interval,
            last_attempt: None,
            last_success: None,
            last_success_at: None,
            last_error: None,
            runs: 0,
            failures: 0,
        }
    }

    fn public(&self, stale_after: f64) -> CollectorState {
        // A loop is late only relative to its own cadence, and a loop whose
        // first run is still in flight is starting, not late.
        let threshold = stale_after.max(self.interval * 3.0);
        let reference = self.last_success_at.unwrap_or(self.created);
        CollectorState {
            last_attempt: self.last_attempt.clone(),
            last_success: self.last_success.clone(),
            last_error: self.last_error.clone(),
            stale: reference.elapsed().as_secs_f64() > threshold,
            runs: self.runs,
            failures: self.failures,
        }
    }
}

struct State {
    host: HostMetrics,
    nodes: Vec<NodeMetrics>,
    tracks: BTreeMap<String, Track>,
    /// Sparkline windows. `node_history` is indexed exactly like `nodes`.
    host_history: Series,
    node_history: Vec<Series>,
}

pub struct Service {
    pub settings: Arc<Settings>,
    pub shutdown: Arc<Shutdown>,
    state: Mutex<State>,
    tx: Mutex<Option<watch::Sender<Arc<String>>>>,
    rx: watch::Receiver<Arc<String>>,
    started: Instant,
    started_at: String,
    demo_nodes: usize,
}

impl Service {
    /// Build the service and take one synchronous sample of the cheap
    /// families, so the first request served already carries readings.
    pub fn new(settings: Arc<Settings>, demo_nodes: usize) -> Arc<Self> {
        let (tx, rx) = watch::channel(Arc::new(String::from("{}")));
        let mut tracks = BTreeMap::new();
        let iv = &settings.intervals;
        for (name, interval) in [
            ("cpu_mem", iv.cpu_mem),
            ("net", iv.net),
            ("disk", iv.disk),
            ("processes", iv.processes),
            ("temperature", iv.temperature),
            ("disk_health", iv.disk_health),
        ] {
            tracks.insert(name.to_string(), Track::new(interval));
        }
        for node in &settings.nodes {
            tracks.insert(format!("proxmox:{}", node.key), Track::new(iv.proxmox));
        }
        if settings.history.capacity > 0 {
            tracks.insert("history".to_string(), Track::new(iv.history_step));
        }

        let mut nodes: Vec<NodeMetrics> = settings
            .nodes
            .iter()
            .map(|n| NodeMetrics {
                key: n.key.clone(),
                name: n.name.clone(),
                status: if n.configured() { "CONNECTING".into() } else { n.status_when_unpolled().into() },
                message: n.unconfigured_reason(),
                ..NodeMetrics::default()
            })
            .collect();
        nodes.extend((1..=demo_nodes).map(demo_node_placeholder));

        let h = settings.history;
        let node_history = (0..nodes.len()).map(|_| Series::new(h.capacity, h.step_seconds)).collect();

        let svc = Arc::new(Self {
            settings,
            shutdown: Arc::new(Shutdown::new()),
            state: Mutex::new(State {
                host: HostMetrics::default(),
                nodes,
                tracks,
                host_history: Series::new(h.capacity, h.step_seconds),
                node_history,
            }),
            tx: Mutex::new(Some(tx)),
            rx,
            started: Instant::now(),
            started_at: now_iso(),
            demo_nodes,
        });
        svc
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A panic while the lock was held leaves consistent-enough data for a
        // dashboard; recovering beats taking every other loop down with it.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn track(&self, name: &str, f: impl FnOnce(&mut Track)) {
        if let Some(t) = self.state().tracks.get_mut(name) {
            f(t);
        }
    }

    /// The label shown for this machine: configured, else the host name.
    pub fn host_label(&self) -> String {
        let configured = self.settings.host.label.trim();
        if !configured.is_empty() {
            return configured.to_string();
        }
        self.state().host.system.hostname.clone().unwrap_or_else(|| "HOST".into())
    }

    pub fn demo_nodes(&self) -> usize {
        self.demo_nodes
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<String>> {
        self.rx.clone()
    }

    pub fn snapshot(&self) -> Snapshot {
        let st = self.state();
        // The windows live here rather than on the metrics, because a metric
        // is replaced wholesale on every poll and the window has to outlast
        // that.
        let mut host = st.host.clone();
        host.history = st.host_history.history();
        let nodes = st
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let mut node = node.clone();
                if let Some(series) = st.node_history.get(index) {
                    node.history = series.history();
                }
                node
            })
            .collect();
        Snapshot { generated_at: now_iso(), service: self.service_state(&st), host, nodes }
    }

    pub fn service_status(&self) -> ServiceState {
        let st = self.state();
        self.service_state(&st)
    }

    fn service_state(&self, st: &State) -> ServiceState {
        let collectors: BTreeMap<String, CollectorState> = st
            .tracks
            .iter()
            .map(|(name, t)| (name.clone(), t.public(self.settings.intervals.stale_after)))
            .collect();
        let degraded = collectors.values().any(|c| c.stale);
        ServiceState {
            status: if degraded { "DEGRADED" } else { "RUNNING" }.into(),
            started_at: self.started_at.clone(),
            uptime_seconds: (self.started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            version: env!("CARGO_PKG_VERSION").into(),
            collectors,
            config_warnings: self.settings.warnings.clone(),
        }
    }

    fn publish(&self) {
        let json = serde_json::to_string(&self.snapshot()).unwrap_or_else(|_| "{}".into());
        if let Some(tx) = self.tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            tx.send_replace(Arc::new(json));
        }
    }

    /// Stop every loop and end the snapshot stream so open browser
    /// connections close and the server can exit.
    pub fn stop(&self) {
        self.shutdown.trigger();
        self.tx.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    // ----------------------------------------------------------------- loops

    /// Start every loop. Must be called from within a Tokio runtime.
    pub fn start(self: &Arc<Self>) {
        let host = self.settings.host.clone();
        let iv = self.settings.intervals.clone();

        let mut cpu = CpuMemSampler::new(&host.label);
        let mut net = NetSampler::new();
        let mut disk = DiskSampler::new(&host);
        self.apply_cpu(&mut cpu);
        self.apply_net(&mut net);
        self.apply_disk(&mut disk);
        for name in ["cpu_mem", "net", "disk"] {
            self.track(name, |t| {
                t.runs += 1;
                t.last_attempt = Some(now_iso());
                t.last_success = t.last_attempt.clone();
                t.last_success_at = Some(Instant::now());
            });
        }
        self.publish();

        self.spawn_loop("cpu_mem", iv.cpu_mem, true, move |svc| svc.apply_cpu(&mut cpu));
        self.spawn_loop("net", iv.net, true, move |svc| svc.apply_net(&mut net));
        self.spawn_loop("disk", iv.disk, true, move |svc| svc.apply_disk(&mut disk));

        let mut procs = ProcSampler::new();
        self.spawn_loop("processes", iv.processes, false, move |svc| {
            let reading = procs.sample();
            svc.state().host.processes = reading;
        });

        let mut temp = TemperatureSampler::new(&host, iv.sensor_backoff);
        self.spawn_loop("temperature", iv.temperature, false, move |svc| {
            let reading = temp.sample();
            svc.state().host.temperature = reading;
        });

        let mut health = HealthSampler::new(host.disk_health);
        self.spawn_loop("disk_health", iv.disk_health, false, move |svc| {
            let reading = health.sample();
            svc.state().host.disk_health = reading;
        });

        // One sampler for every panel, on its own steady cadence. Reading the
        // figures the donuts already show, rather than each collector's own
        // tick, is what keeps the points evenly spaced and the three charts
        // on a panel aligned with each other.
        if self.settings.history.capacity > 0 {
            self.spawn_loop("history", iv.history_step, false, |svc| {
                let mut guard = svc.state();
                let st = &mut *guard;
                st.host_history.push(&st.host.primary);
                for (index, node) in st.nodes.iter().enumerate() {
                    if let Some(series) = st.node_history.get_mut(index) {
                        series.push(&node.primary);
                    }
                }
            });
        }

        #[cfg(feature = "proxmox")]
        for (index, node) in self.settings.nodes.iter().enumerate() {
            self.spawn_node(index, node.clone(), iv.proxmox);
        }
        if self.demo_nodes > 0 {
            self.spawn_demo();
        }

        let svc = Arc::clone(self);
        let push = Duration::from_secs_f64(iv.stream_push);
        tokio::spawn(async move {
            while !svc.shutdown.wait_async(push).await {
                svc.publish();
            }
        });
    }

    fn apply_cpu(&self, sampler: &mut CpuMemSampler) {
        let r = sampler.sample();
        let mut st = self.state();
        st.host.primary.cpu_percent = r.cpu.percent;
        st.host.primary.memory_percent = r.memory.percent;
        st.host.system = r.system;
        st.host.cpu = r.cpu;
        st.host.memory = r.memory;
        st.host.swap = r.swap;
    }

    fn apply_net(&self, sampler: &mut NetSampler) {
        let r = sampler.sample();
        self.state().host.network = r;
    }

    fn apply_disk(&self, sampler: &mut DiskSampler) {
        let r = sampler.sample();
        let mut st = self.state();
        st.host.filesystems = r.filesystems;
        st.host.disk_io = r.io;
        st.host.primary.disk_percent = r.primary_percent;
        st.host.primary.disk_mount = r.primary_mount;
    }

    fn spawn_loop<F>(self: &Arc<Self>, name: &'static str, interval: f64, primed: bool, mut step: F)
    where
        F: FnMut(&Service) + Send + 'static,
    {
        let svc = Arc::clone(self);
        let period = Duration::from_secs_f64(interval);
        let spawned = std::thread::Builder::new().name(format!("plh-{name}")).spawn(move || {
            if primed && svc.shutdown.wait(period) {
                return;
            }
            loop {
                let started = Instant::now();
                svc.track(name, |t| {
                    t.runs += 1;
                    t.last_attempt = Some(now_iso());
                });
                match std::panic::catch_unwind(AssertUnwindSafe(|| step(&svc))) {
                    Ok(()) => svc.track(name, |t| {
                        t.last_success = Some(now_iso());
                        t.last_success_at = Some(Instant::now());
                        t.last_error = None;
                    }),
                    Err(panic) => {
                        let reason = panic
                            .downcast_ref::<&str>()
                            .map(|s| s.to_string())
                            .or_else(|| panic.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "panic".into());
                        crate::log_error!("{name} collector failed: {reason}");
                        svc.track(name, |t| {
                            t.failures += 1;
                            t.last_error = Some(reason);
                        });
                    }
                }
                // Sleeping the remainder keeps the cadence steady and always
                // leaves a gap, even after a sample longer than its interval.
                let rest = period.saturating_sub(started.elapsed()).max(Duration::from_millis(250));
                if svc.shutdown.wait(rest) {
                    return;
                }
            }
        });
        if let Err(e) = spawned {
            crate::log_error!("could not start the {name} collector thread: {e}");
        }
    }

    #[cfg(feature = "proxmox")]
    fn spawn_node(self: &Arc<Self>, index: usize, node: crate::config::NodeSettings, interval: f64) {
        let svc = Arc::clone(self);
        let track = format!("proxmox:{}", node.key);
        let period = Duration::from_secs_f64(interval);
        tokio::spawn(async move {
            let mut collector = crate::collect::proxmox::ProxmoxCollector::new(node, interval);
            loop {
                let started = Instant::now();
                svc.track(&track, |t| {
                    t.runs += 1;
                    t.last_attempt = Some(now_iso());
                });
                let previous = svc.state().nodes.get(index).cloned();
                let metrics = collector.collect(previous.as_ref()).await;
                if let Some(slot) = svc.state().nodes.get_mut(index) {
                    *slot = metrics;
                }
                svc.track(&track, |t| {
                    t.last_success = Some(now_iso());
                    t.last_success_at = Some(Instant::now());
                });
                let rest = period.saturating_sub(started.elapsed()).max(Duration::from_millis(250));
                if svc.shutdown.wait_async(rest).await {
                    return;
                }
            }
        });
    }

    /// Development aid: synthetic nodes, labelled DEV_MOCK everywhere they
    /// appear, for checking layouts with more machines than exist.
    fn spawn_demo(self: &Arc<Self>) {
        let svc = Arc::clone(self);
        let first = self.settings.nodes.len();
        tokio::spawn(async move {
            loop {
                let t = svc.started.elapsed().as_secs_f64();
                {
                    let mut st = svc.state();
                    for i in 0..svc.demo_nodes {
                        if let Some(node) = st.nodes.get_mut(first + i) {
                            let wave = |offset: f64, base: f64, span: f64| {
                                crate::rates::round1(base + span * ((t / 20.0) + offset + i as f64).sin())
                            };
                            let cpu = wave(0.0, 45.0, 40.0);
                            let mem = wave(1.3, 55.0, 30.0);
                            let disk = wave(2.1, 60.0, 5.0);
                            node.status = "DEV_MOCK".into();
                            node.last_success = Some(now_iso());
                            node.last_attempt = node.last_success.clone();
                            node.primary = Primary {
                                cpu_percent: Some(cpu),
                                memory_percent: Some(mem),
                                disk_percent: Some(disk),
                                disk_mount: Some("rootfs".into()),
                            };
                            node.cpu_percent = Some(cpu);
                            node.memory_percent = Some(mem);
                            node.rootfs_percent = Some(disk);
                            node.uptime_seconds = Some(86400.0 * (i as f64 + 2.0) + t);
                        }
                    }
                }
                if svc.shutdown.wait_async(Duration::from_secs(2)).await {
                    return;
                }
            }
        });
    }
}

fn demo_node_placeholder(i: usize) -> NodeMetrics {
    NodeMetrics {
        key: format!("demo{i}"),
        name: format!("DEMO {i}"),
        status: "DEV_MOCK".into(),
        message: Some("Synthetic development data - not a real node".into()),
        ..NodeMetrics::default()
    }
}

/// Public description of demo nodes for /api/config.
pub fn demo_public(count: usize) -> Vec<serde_json::Value> {
    (1..=count)
        .map(|i| {
            serde_json::json!({
                "key": format!("demo{i}"),
                "name": format!("DEMO {i}"),
                "host": null, "port": null, "api_node": null,
                "configured": false, "verify_tls": true,
                "reason": "Synthetic development data - not a real node",
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FileConfig, validate};
    use std::path::Path;

    fn settings() -> Arc<Settings> {
        let mut file = FileConfig::default();
        file.host.disk_health = false;
        file.host.temperature = false;
        Arc::new(validate(file, Path::new("."), Path::new("c.toml"), false, Vec::new()))
    }

    #[test]
    fn staleness_is_relative_to_each_interval() {
        let mut slow = Track::new(60.0);
        slow.last_success_at = Some(Instant::now() - Duration::from_secs(30));
        assert!(!slow.public(15.0).stale, "a 60 s loop is not late after 30 s");
        let mut fast = Track::new(1.5);
        fast.last_success_at = Some(Instant::now() - Duration::from_secs(30));
        assert!(fast.public(15.0).stale);
    }

    #[test]
    fn a_loop_still_on_its_first_run_is_not_stale() {
        let starting = Track::new(60.0);
        assert!(!starting.public(15.0).stale);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn first_snapshot_already_has_readings() {
        let svc = Service::new(settings(), 0);
        svc.start();
        let snap = svc.snapshot();
        assert!(snap.host.memory.total_bytes.unwrap() > 0);
        assert!(snap.host.system.hostname.is_some());
        assert!(!snap.host.filesystems.is_empty());
        assert_eq!(snap.service.status, "RUNNING");
        svc.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broadcast_carries_a_full_snapshot() {
        let svc = Service::new(settings(), 1);
        svc.start();
        let rx = svc.subscribe();
        let json = rx.borrow().clone();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value["host"]["memory"]["total_bytes"].as_u64().unwrap() > 0);
        assert_eq!(value["nodes"][0]["status"], "DEV_MOCK");
        svc.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn history_accumulates_a_window_for_every_panel() {
        let mut file = FileConfig::default();
        file.host.disk_health = false;
        file.host.temperature = false;
        file.intervals.history_step = 0.5;
        file.history.seconds = 10.0;
        let settings = Arc::new(validate(file, Path::new("."), Path::new("c.toml"), false, Vec::new()));
        assert_eq!(settings.history.capacity, 20);

        let svc = Service::new(settings, 1);
        svc.start();

        let mut snap = svc.snapshot();
        for _ in 0..40 {
            if !snap.host.history.cpu.is_empty() && !snap.nodes[0].history.cpu.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            snap = svc.snapshot();
        }
        assert_eq!(snap.host.history.step_seconds, 0.5);
        assert_eq!(snap.host.history.capacity, 20);
        assert!(!snap.host.history.cpu.is_empty(), "the host window is still empty");
        assert!(!snap.nodes[0].history.cpu.is_empty(), "the node window is still empty");
        // The three series of a panel advance together, so they stay aligned.
        assert_eq!(snap.host.history.cpu.len(), snap.host.history.memory.len());
        assert_eq!(snap.host.history.cpu.len(), snap.host.history.disk.len());
        svc.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn history_switched_off_runs_no_loop() {
        let mut file = FileConfig::default();
        file.host.disk_health = false;
        file.host.temperature = false;
        file.history.enabled = false;
        let settings = Arc::new(validate(file, Path::new("."), Path::new("c.toml"), false, Vec::new()));

        let svc = Service::new(settings, 0);
        svc.start();
        tokio::time::sleep(Duration::from_millis(200)).await;

        let snap = svc.snapshot();
        assert!(snap.host.history.cpu.is_empty());
        assert!(!snap.service.collectors.contains_key("history"));
        svc.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_ends_the_stream() {
        let svc = Service::new(settings(), 0);
        svc.start();
        let mut rx = svc.subscribe();
        svc.stop();
        // Unseen values are delivered first; once drained, the closed
        // channel releases the subscriber instead of leaving it waiting.
        let drained = tokio::time::timeout(Duration::from_secs(3), async {
            while rx.changed().await.is_ok() {}
        })
        .await;
        assert!(drained.is_ok(), "subscriber still waiting after stop");
    }
}
