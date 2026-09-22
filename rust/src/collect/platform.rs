//! Operating-system specifics that sysinfo does not expose.
//!
//! Each function has a Windows, a Linux and a fallback implementation. The
//! fallback returns None or an empty map, which the dashboard shows as N/A;
//! it never substitutes a guessed value.

use std::collections::HashMap;

use super::cputimes::{CpuTimes, Flavor};

/// One reading of per-core CPU time plus system-wide event counters.
pub struct CpuSample {
    pub per_core: Vec<CpuTimes>,
    pub flavor: Flavor,
    pub ctx_switches: Option<u64>,
    pub interrupts: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LinkInfo {
    pub up: Option<bool>,
    pub speed_mbps: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessCounts {
    pub total: u64,
    pub running: Option<u64>,
    pub threads: Option<u64>,
}

pub use imp::{cpu_sample, link_info, process_counts, system_drive};

// ------------------------------------------------------------------ Windows

#[cfg(windows)]
mod imp {
    use super::*;
    use std::mem::size_of;

    use windows_sys::Wdk::System::SystemInformation::{
        NtQuerySystemInformation, SystemProcessorPerformanceInformation,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };

    /// SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION with its reserved fields
    /// named: the two Reserved1 values are DPC and interrupt time, and
    /// Reserved2 is the interrupt count. psutil reads the same layout.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct ProcessorPerformance {
        idle: i64,
        kernel: i64,
        user: i64,
        dpc: i64,
        interrupt: i64,
        interrupt_count: u32,
    }

    const _: () = assert!(size_of::<ProcessorPerformance>() == 48);

    pub fn cpu_sample() -> Option<CpuSample> {
        // Class 8 reports the processors of the calling thread's group,
        // which is every processor on machines with up to 64 of them.
        let mut buffer = vec![ProcessorPerformance::default(); 256];
        let mut returned: u32 = 0;
        let status = unsafe {
            NtQuerySystemInformation(
                SystemProcessorPerformanceInformation,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * size_of::<ProcessorPerformance>()) as u32,
                &mut returned,
            )
        };
        if status < 0 {
            return None;
        }
        buffer.truncate(returned as usize / size_of::<ProcessorPerformance>());
        if buffer.is_empty() {
            return None;
        }
        let interrupts = buffer.iter().map(|p| u64::from(p.interrupt_count)).sum();
        let per_core = buffer
            .iter()
            .map(|p| CpuTimes {
                user: p.user as f64,
                // Kernel time includes idle, DPC and interrupt time; system
                // time is the remainder.
                system: (p.kernel - p.idle - p.dpc - p.interrupt).max(0) as f64,
                idle: p.idle as f64,
                dpc: p.dpc as f64,
                interrupt: p.interrupt as f64,
                ..CpuTimes::default()
            })
            .collect();
        Some(CpuSample { per_core, flavor: Flavor::Windows, ctx_switches: None, interrupts: Some(interrupts) })
    }

    pub fn link_info() -> HashMap<String, LinkInfo> {
        let mut out = HashMap::new();
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if unsafe { GetIfTable2(&mut table) } != 0 || table.is_null() {
            return out;
        }
        unsafe {
            let count = (*table).NumEntries as usize;
            let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), count);
            for row in rows {
                let end = row.Alias.iter().position(|c| *c == 0).unwrap_or(row.Alias.len());
                let name = String::from_utf16_lossy(&row.Alias[..end]);
                if name.is_empty() {
                    continue;
                }
                let up = row.OperStatus == IfOperStatusUp;
                let speed = row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed);
                let info = LinkInfo {
                    up: Some(up),
                    speed_mbps: (speed != 0 && speed != u64::MAX).then(|| speed / 1_000_000),
                };
                // Several rows can share an alias (filter drivers); an "up"
                // row wins over a "down" one.
                out.entry(name)
                    .and_modify(|existing: &mut LinkInfo| {
                        if up && existing.up != Some(true) {
                            *existing = info;
                        }
                    })
                    .or_insert(info);
            }
            FreeMibTable(table as *const _);
        }
        out
    }

    pub fn process_counts() -> Option<ProcessCounts> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
                return None;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            let mut total = 0u64;
            let mut threads = 0u64;
            if Process32FirstW(snapshot, &mut entry) != 0 {
                loop {
                    total += 1;
                    threads += u64::from(entry.cntThreads);
                    if Process32NextW(snapshot, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snapshot);
            // Windows has no meaningful running/sleeping split per process.
            (total > 0).then_some(ProcessCounts { total, running: None, threads: Some(threads) })
        }
    }

    pub fn system_drive() -> String {
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        format!("{}\\", drive.trim_end_matches('\\'))
    }
}

// -------------------------------------------------------------------- Linux

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use crate::collect::cputimes::{parse_loadavg_tasks, parse_proc_stat};

    pub fn cpu_sample() -> Option<CpuSample> {
        let text = std::fs::read_to_string("/proc/stat").ok()?;
        let parsed = parse_proc_stat(&text);
        if parsed.per_core.is_empty() {
            return None;
        }
        Some(CpuSample {
            per_core: parsed.per_core,
            flavor: Flavor::Linux,
            ctx_switches: parsed.ctxt,
            interrupts: parsed.intr,
        })
    }

    pub fn link_info() -> HashMap<String, LinkInfo> {
        let mut out = HashMap::new();
        let Ok(entries) = std::fs::read_dir("/sys/class/net") else { return out };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let dir = entry.path();
            // Tunnels such as WireGuard report operstate "unknown" while
            // carrying traffic, so that case falls back to the IFF_UP flag.
            let up = std::fs::read_to_string(dir.join("operstate")).ok().map(|state| {
                match state.trim() {
                    "up" => true,
                    "down" | "lowerlayerdown" | "notpresent" => false,
                    _ => std::fs::read_to_string(dir.join("flags"))
                        .ok()
                        .and_then(|f| u32::from_str_radix(f.trim().trim_start_matches("0x"), 16).ok())
                        .is_some_and(|flags| flags & 0x1 != 0),
                }
            });
            // speed is unreadable (EINVAL) while a link is down, and -1 for
            // virtual devices; both mean "not known".
            let speed_mbps = std::fs::read_to_string(dir.join("speed"))
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok())
                .filter(|s| *s > 0)
                .map(|s| s as u64);
            out.insert(name, LinkInfo { up, speed_mbps });
        }
        out
    }

    pub fn process_counts() -> Option<ProcessCounts> {
        let total = std::fs::read_dir("/proc")
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()))
            .count() as u64;
        let tasks = std::fs::read_to_string("/proc/loadavg").ok().and_then(|t| parse_loadavg_tasks(&t));
        Some(ProcessCounts {
            total,
            running: tasks.map(|(running, _)| running),
            threads: tasks.map(|(_, all)| all),
        })
    }

    pub fn system_drive() -> String {
        "/".into()
    }
}

// ------------------------------------------------------------ other systems

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use super::*;

    /// No portable CPU-time source; sysinfo's own usage figure is used.
    pub fn cpu_sample() -> Option<CpuSample> {
        None
    }

    pub fn link_info() -> HashMap<String, LinkInfo> {
        HashMap::new()
    }

    /// None makes the caller count processes through sysinfo instead.
    pub fn process_counts() -> Option<ProcessCounts> {
        None
    }

    pub fn system_drive() -> String {
        "/".into()
    }
}
