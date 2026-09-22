//! Physical drive health.
//!
//! Windows asks the Storage provider (Get-PhysicalDisk); Linux asks smartctl
//! when it is installed and permitted. Neither is a full SMART attribute
//! dump. When no source answers the result is unavailable, and the dashboard
//! shows N/A rather than an assumed "healthy".


use serde_json::Value;

use crate::model::{DiskHealth, PhysicalDisk};

pub struct HealthSampler {
    enabled: bool,
}

impl HealthSampler {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    pub fn sample(&mut self) -> DiskHealth {
        if !self.enabled {
            return unavailable("Disabled by configuration (host.disk_health = false)");
        }
        imp::sample()
    }
}

pub fn unavailable(detail: &str) -> DiskHealth {
    DiskHealth { available: false, detail: Some(detail.to_string()), disks: Vec::new() }
}

fn text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(|v| text(Some(v))).collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Parse Get-PhysicalDisk output. A single disk serialises as an object
/// rather than an array; older providers report MediaType as a number.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_windows(json_text: &str) -> Result<Vec<PhysicalDisk>, &'static str> {
    let payload: Value = serde_json::from_str(json_text).map_err(|_| "Storage provider returned unreadable data")?;
    let items = match payload {
        Value::Array(items) => items,
        object @ Value::Object(_) => vec![object],
        _ => return Err("Storage provider returned an unexpected shape"),
    };
    let disks: Vec<PhysicalDisk> = items
        .iter()
        .filter(|i| i.is_object())
        .map(|item| PhysicalDisk {
            name: text(item.get("FriendlyName")),
            media_type: match item.get("MediaType") {
                Some(Value::Number(n)) => Some(
                    match n.as_i64() {
                        Some(3) => "HDD",
                        Some(4) => "SSD",
                        Some(5) => "SCM",
                        _ => "Unspecified",
                    }
                    .to_string(),
                ),
                other => text(other),
            },
            health: text(item.get("HealthStatus")),
            operational: text(item.get("OperationalStatus")),
            size_bytes: item.get("Size").and_then(Value::as_u64),
        })
        .collect();
    if disks.is_empty() { Err("No physical disks reported") } else { Ok(disks) }
}

/// Parse one `smartctl -H -j <device>` document.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_smartctl(json_text: &str) -> Option<PhysicalDisk> {
    let doc: Value = serde_json::from_str(json_text).ok()?;
    let passed = doc.get("smart_status")?.get("passed")?.as_bool()?;
    let rotation = doc.get("rotation_rate").and_then(Value::as_i64);
    Some(PhysicalDisk {
        name: text(doc.get("model_name")).or_else(|| text(doc.get("device").and_then(|d| d.get("name")))),
        media_type: match (rotation, doc.get("device").and_then(|d| d.get("type")).and_then(Value::as_str)) {
            (_, Some("nvme")) => Some("NVMe".into()),
            (Some(0), _) => Some("SSD".into()),
            (Some(_), _) => Some("HDD".into()),
            _ => None,
        },
        health: Some(if passed { "Healthy" } else { "Failing" }.into()),
        operational: Some(if passed { "SMART passed" } else { "SMART failed" }.into()),
        size_bytes: doc.get("user_capacity").and_then(|c| c.get("bytes")).and_then(Value::as_u64),
    })
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::time::Duration;

    pub fn sample() -> DiskHealth {
        const SCRIPT: &str = "$ErrorActionPreference='Stop';Get-PhysicalDisk|\
            Select-Object FriendlyName,MediaType,HealthStatus,OperationalStatus,Size|\
            ConvertTo-Json -Compress -Depth 3";
        let Some(output) = crate::collect::powershell(SCRIPT, Duration::from_secs(25)) else {
            return unavailable("Storage provider query failed or timed out");
        };
        if output.trim().is_empty() {
            return unavailable("Storage provider returned no data");
        }
        match parse_windows(&output) {
            Ok(disks) => DiskHealth { available: true, detail: None, disks },
            Err(reason) => unavailable(reason),
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::time::Duration;
    use std::process::Command;

    pub fn sample() -> DiskHealth {
        let scan = {
            let mut c = Command::new("smartctl");
            c.args(["--scan", "-j"]);
            crate::collect::run_with_timeout(c, Duration::from_secs(10))
        };
        let Some((_, scan_text)) = scan else {
            return unavailable("smartctl is not installed (package smartmontools)");
        };
        let devices: Vec<String> = serde_json::from_str::<Value>(&scan_text)
            .ok()
            .and_then(|v| v.get("devices").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str).map(str::to_string))
            .collect();
        if devices.is_empty() {
            return unavailable("smartctl found no drives (it usually needs root)");
        }
        let mut disks = Vec::new();
        let mut denied = false;
        for device in devices {
            let mut c = Command::new("smartctl");
            c.args(["-H", "-i", "-j", &device]);
            match crate::collect::run_with_timeout(c, Duration::from_secs(15)) {
                Some((_, out)) => match parse_smartctl(&out) {
                    Some(disk) => disks.push(disk),
                    None => denied = true,
                },
                None => denied = true,
            }
        }
        if disks.is_empty() {
            return unavailable(if denied {
                "smartctl could not open the drives; it needs root"
            } else {
                "smartctl returned no health status"
            });
        }
        DiskHealth { available: true, detail: None, disks }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use super::*;

    pub fn sample() -> DiskHealth {
        unavailable("Drive health is not implemented on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_windows_disk_object() {
        let disks = parse_windows(
            r#"{"FriendlyName":"G521N 256G","MediaType":4,"HealthStatus":"Healthy","OperationalStatus":"OK","Size":256060514304}"#,
        )
        .unwrap();
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].media_type.as_deref(), Some("SSD"));
        assert_eq!(disks[0].size_bytes, Some(256060514304));
    }

    #[test]
    fn list_valued_status_is_flattened() {
        let disks = parse_windows(
            r#"[{"FriendlyName":"NXT","MediaType":"SSD","HealthStatus":"Warning","OperationalStatus":["OK","Degraded"]}]"#,
        )
        .unwrap();
        assert_eq!(disks[0].operational.as_deref(), Some("OK, Degraded"));
    }

    #[test]
    fn unreadable_output_is_an_error() {
        assert!(parse_windows("not json").is_err());
        assert!(parse_windows("[]").is_err());
    }

    #[test]
    fn smartctl_document() {
        let disk = parse_smartctl(
            r#"{"device":{"name":"/dev/sda","type":"sat"},"model_name":"SSD240GB","rotation_rate":0,
                "smart_status":{"passed":true},"user_capacity":{"bytes":240057409536}}"#,
        )
        .unwrap();
        assert_eq!(disk.name.as_deref(), Some("SSD240GB"));
        assert_eq!(disk.media_type.as_deref(), Some("SSD"));
        assert_eq!(disk.health.as_deref(), Some("Healthy"));
    }

    #[test]
    fn smartctl_without_status_is_none() {
        assert!(parse_smartctl(r#"{"smartctl":{"exit_status":2}}"#).is_none());
    }

    #[test]
    fn disabled_is_unavailable() {
        let h = HealthSampler::new(false).sample();
        assert!(!h.available);
        assert!(h.disks.is_empty());
    }
}
