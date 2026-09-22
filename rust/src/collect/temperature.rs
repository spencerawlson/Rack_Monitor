//! CPU temperature, from whichever real sensor source the machine has.
//!
//! Sources, cheapest first:
//! 1. sysinfo components: hwmon on Linux, SMC on macOS, ACPI on Windows
//!    (which usually needs administrator rights and returns nothing).
//! 2. Windows only: LibreHardwareMonitor's local web server.
//! 3. Windows only: the LibreHardwareMonitor or OpenHardwareMonitor WMI
//!    provider, read through PowerShell.
//!
//! When every source fails the reading is None and the dashboard shows N/A.
//! A value is never estimated or carried over from an earlier probe. Failure
//! backs off, so a machine with no sensor is not charged a PowerShell launch
//! on every interval.

use std::time::{Duration, Instant};

use serde_json::Value;
use sysinfo::Components;

use crate::config::HostSection;
use crate::model::Temperature;

/// Sensor labels that plausibly describe a whole-CPU temperature.
fn is_cpu_label(label: &str) -> bool {
    let l = label.to_ascii_lowercase();
    ["cpu", "package", "tdie", "tctl", "core average", "k10temp", "coretemp"]
        .iter()
        .any(|needle| l.contains(needle))
}

pub struct TemperatureSampler {
    host: HostSection,
    backoff: Duration,
    probe_after: Option<Instant>,
    components: Option<Components>,
}

impl TemperatureSampler {
    pub fn new(host: &HostSection, backoff_seconds: f64) -> Self {
        Self {
            host: host.clone(),
            backoff: Duration::from_secs_f64(backoff_seconds),
            probe_after: None,
            components: None,
        }
    }

    pub fn sample(&mut self) -> Temperature {
        if !self.host.temperature {
            return unavailable("Disabled by configuration (host.temperature = false)");
        }
        let now = Instant::now();
        if let Some(after) = self.probe_after {
            if now < after {
                let wait = after.duration_since(now).as_secs();
                return unavailable(&format!("No sensor source; retrying in {wait}s"));
            }
        }

        let reading = self
            .from_components()
            .map(|v| (v, "sysinfo components".to_string()))
            .or_else(|| self.from_lhm_http().map(|v| (v, "LibreHardwareMonitor (HTTP)".to_string())))
            .or_else(|| self.from_wmi());

        match reading {
            Some((celsius, source)) => {
                self.probe_after = None;
                Temperature {
                    cpu_celsius: Some(crate::rates::round1(celsius)),
                    source: Some(source),
                    available: true,
                    detail: None,
                }
            }
            None => {
                self.probe_after = Some(now + self.backoff);
                unavailable(if cfg!(windows) {
                    "No supported sensor source (ACPI, LibreHardwareMonitor HTTP and WMI all unavailable)"
                } else {
                    "No CPU temperature sensor exposed by the operating system"
                })
            }
        }
    }

    fn from_components(&mut self) -> Option<f64> {
        let components = self.components.get_or_insert_with(Components::new_with_refreshed_list);
        components.refresh(true);
        let mut labelled: Vec<f64> = Vec::new();
        for component in components.list() {
            let Some(t) = component.temperature() else { continue };
            let t = f64::from(t);
            // Zero and absurd readings come from sensors that are present
            // but not wired up.
            if !(1.0..=150.0).contains(&t) {
                continue;
            }
            if is_cpu_label(component.label()) {
                labelled.push(t);
            }
        }
        labelled.into_iter().reduce(f64::max)
    }

    fn from_lhm_http(&self) -> Option<f64> {
        if !cfg!(windows) || self.host.lhm_url.trim().is_empty() {
            return None;
        }
        let (host, port, path) = split_http_url(self.host.lhm_url.trim())?;
        let response =
            crate::httpmini::request(&host, port, "GET", &path, &[], Duration::from_millis(1500)).ok()?;
        if response.status != 200 {
            return None;
        }
        let payload: Value = serde_json::from_str(&response.body).ok()?;
        let mut readings = Vec::new();
        walk_lhm(&payload, "", &mut readings);
        readings.into_iter().reduce(f64::max)
    }

    #[cfg(windows)]
    fn from_wmi(&self) -> Option<(f64, String)> {
        if !self.host.lhm_wmi {
            return None;
        }
        const SCRIPT: &str = "$ErrorActionPreference='Stop';$o=@();\
            foreach($ns in 'root/LibreHardwareMonitor','root/OpenHardwareMonitor'){\
            try{$s=Get-CimInstance -Namespace $ns -ClassName Sensor -ErrorAction Stop|\
            Where-Object{$_.SensorType -eq 'Temperature'};\
            foreach($i in $s){$o+=('{0}|{1}' -f $i.Name,$i.Value)}}catch{}};$o -join \"`n\"";
        let output = crate::collect::powershell(SCRIPT, Duration::from_secs(8))?;
        let readings: Vec<f64> = output
            .lines()
            .filter_map(|line| line.split_once('|'))
            .filter(|(name, _)| is_cpu_label(name))
            .filter_map(|(_, value)| value.trim().replace(',', ".").parse::<f64>().ok())
            .filter(|t| (1.0..=150.0).contains(t))
            .collect();
        readings.into_iter().reduce(f64::max).map(|t| (t, "LibreHardwareMonitor (WMI)".to_string()))
    }

    #[cfg(not(windows))]
    fn from_wmi(&self) -> Option<(f64, String)> {
        None
    }
}

fn unavailable(detail: &str) -> Temperature {
    Temperature { cpu_celsius: None, source: None, available: false, detail: Some(detail.to_string()) }
}

/// Split "http://host:port/path" into its parts. Only plain HTTP is
/// supported, which is all LibreHardwareMonitor serves.
pub fn split_http_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') || h.starts_with('[') => (h.trim_matches(['[', ']']), p.parse().ok()?),
        _ => (authority, 80),
    };
    (!host.is_empty()).then(|| (host.to_string(), port, path))
}

/// Depth-first walk of LibreHardwareMonitor's JSON tree, collecting Celsius
/// values from nodes whose label (or an ancestor's) looks CPU-related.
pub fn walk_lhm(node: &Value, context: &str, out: &mut Vec<f64>) {
    match node {
        Value::Object(map) => {
            let text = map.get("Text").and_then(Value::as_str).unwrap_or("");
            if let Some(value) = map.get("Value").and_then(Value::as_str) {
                if value.contains('C') && is_cpu_label(&format!("{context} {text}")) && !value.contains("MHz") {
                    if let Some(number) = leading_number(value) {
                        if (1.0..=150.0).contains(&number) {
                            out.push(number);
                        }
                    }
                }
            }
            if let Some(children) = map.get("Children").and_then(Value::as_array) {
                let next = if text.is_empty() { context } else { text };
                for child in children {
                    walk_lhm(child, next, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| walk_lhm(i, context, out)),
        _ => {}
    }
}

fn leading_number(text: &str) -> Option<f64> {
    let cleaned: String = text
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',' || *c == '-')
        .collect();
    cleaned.replace(',', ".").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn disabled_reports_why() {
        let host = HostSection { temperature: false, ..HostSection::default() };
        let t = TemperatureSampler::new(&host, 60.0).sample();
        assert!(!t.available);
        assert_eq!(t.cpu_celsius, None);
        assert!(t.detail.unwrap().contains("Disabled"));
    }

    #[test]
    fn missing_sensors_back_off_without_a_value() {
        // An unreachable LibreHardwareMonitor URL and no WMI provider make
        // the Windows chain fail; elsewhere components may or may not exist.
        let host = HostSection { lhm_url: "http://127.0.0.1:9/data.json".into(), lhm_wmi: false, ..HostSection::default() };
        let mut sampler = TemperatureSampler::new(&host, 60.0);
        let first = sampler.sample();
        if !first.available {
            assert_eq!(first.cpu_celsius, None);
            let second = sampler.sample();
            assert!(second.detail.unwrap().contains("retrying in"));
        } else {
            assert!(first.cpu_celsius.is_some());
        }
    }

    #[test]
    fn lhm_tree_is_parsed() {
        let payload = json!({"Text": "Sensor", "Children": [
            {"Text": "MiniPC", "Children": [
                {"Text": "Intel Core i5", "Children": [
                    {"Text": "Temperatures", "Children": [
                        {"Text": "CPU Core #1", "Value": "44.0 \u{b0}C"},
                        {"Text": "CPU Package", "Value": "47,5 \u{b0}C"},
                        {"Text": "Core Max", "Value": "48.0 \u{b0}C"}
                    ]},
                    {"Text": "Clocks", "Children": [
                        {"Text": "CPU Core #1", "Value": "3400 MHz"}
                    ]}
                ]}
            ]}
        ]});
        let mut readings = Vec::new();
        walk_lhm(&payload, "", &mut readings);
        // "Core Max" is not a CPU-package label and is left out rather than
        // guessed at; the clock value is not a temperature.
        assert_eq!(readings, vec![44.0, 47.5]);
    }

    #[test]
    fn url_splitting() {
        assert_eq!(split_http_url("http://127.0.0.1:8085/data.json"), Some(("127.0.0.1".into(), 8085, "/data.json".into())));
        assert_eq!(split_http_url("http://localhost"), Some(("localhost".into(), 80, "/".into())));
        assert_eq!(split_http_url("https://x/y"), None);
    }

    #[test]
    fn cpu_label_matching() {
        assert!(is_cpu_label("Package id 0"));
        assert!(is_cpu_label("CPU Package"));
        assert!(is_cpu_label("Tctl"));
        assert!(!is_cpu_label("nvme Composite"));
    }
}
