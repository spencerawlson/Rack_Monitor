//! CPU time accounting, independent of where the counters came from.
//!
//! Percentages are derived from the change in cumulative CPU time between two
//! samples. This is the method psutil (and therefore Glances) uses, so the
//! figures agree with them rather than with a differently defined counter.

/// Cumulative time per CPU state. Units are whatever the source reports
/// (100 ns on Windows, clock ticks on Linux); only ratios are used.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuTimes {
    pub user: f64,
    pub nice: f64,
    pub system: f64,
    pub idle: f64,
    pub iowait: f64,
    pub irq: f64,
    pub softirq: f64,
    pub steal: f64,
    pub dpc: f64,
    pub interrupt: f64,
}

impl CpuTimes {
    fn fields(&self) -> [f64; 10] {
        [
            self.user, self.nice, self.system, self.idle, self.iowait, self.irq, self.softirq, self.steal,
            self.dpc, self.interrupt,
        ]
    }

    pub fn sum(items: &[CpuTimes]) -> CpuTimes {
        items.iter().fold(CpuTimes::default(), |acc, t| CpuTimes {
            user: acc.user + t.user,
            nice: acc.nice + t.nice,
            system: acc.system + t.system,
            idle: acc.idle + t.idle,
            iowait: acc.iowait + t.iowait,
            irq: acc.irq + t.irq,
            softirq: acc.softirq + t.softirq,
            steal: acc.steal + t.steal,
            dpc: acc.dpc + t.dpc,
            interrupt: acc.interrupt + t.interrupt,
        })
    }
}

/// Which CPU states a platform actually reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Windows,
    Linux,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Breakdown {
    pub busy: f64,
    pub user: f64,
    pub system: f64,
    pub idle: f64,
    pub iowait: Option<f64>,
    pub irq: Option<f64>,
    pub softirq: Option<f64>,
    pub dpc: Option<f64>,
    pub interrupt: Option<f64>,
}

fn pct(part: f64, total: f64) -> f64 {
    crate::rates::round1((part / total * 100.0).clamp(0.0, 100.0))
}

/// Percentage of elapsed CPU time spent in each state between two samples.
///
/// Busy time excludes idle and iowait, matching psutil. Nice time is folded
/// into user time. None when no time elapsed, which happens when two samples
/// land in the same scheduler tick.
pub fn breakdown(prev: &CpuTimes, cur: &CpuTimes, flavor: Flavor) -> Option<Breakdown> {
    let before = prev.fields();
    let after = cur.fields();
    let mut delta = [0.0; 10];
    for i in 0..10 {
        delta[i] = (after[i] - before[i]).max(0.0);
    }
    let total: f64 = delta.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let [user, nice, system, idle, iowait, irq, softirq, _steal, dpc, interrupt] = delta;
    let linux = flavor == Flavor::Linux;
    let windows = flavor == Flavor::Windows;
    Some(Breakdown {
        busy: pct(total - idle - iowait, total),
        user: pct(user + nice, total),
        system: pct(system, total),
        idle: pct(idle, total),
        iowait: linux.then(|| pct(iowait, total)),
        irq: linux.then(|| pct(irq, total)),
        softirq: linux.then(|| pct(softirq, total)),
        dpc: windows.then(|| pct(dpc, total)),
        interrupt: windows.then(|| pct(interrupt, total)),
    })
}

/// Parsed contents of Linux's /proc/stat. Compiled on every platform so the
/// parser is tested everywhere; only Linux reads the real file.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Default, PartialEq)]
pub struct ProcStat {
    pub per_core: Vec<CpuTimes>,
    pub ctxt: Option<u64>,
    pub intr: Option<u64>,
}

/// Parse /proc/stat text. Kept free of any file access so it can be tested
/// on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_proc_stat(text: &str) -> ProcStat {
    let mut out = ProcStat::default();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(tag) = parts.next() else { continue };
        if let Some(index) = tag.strip_prefix("cpu") {
            // "cpu" alone is the aggregate line; per-core lines carry a number.
            if index.is_empty() || index.parse::<usize>().is_err() {
                continue;
            }
            let v: Vec<f64> = parts.filter_map(|p| p.parse::<f64>().ok()).collect();
            let at = |i: usize| v.get(i).copied().unwrap_or(0.0);
            out.per_core.push(CpuTimes {
                user: at(0),
                nice: at(1),
                system: at(2),
                idle: at(3),
                iowait: at(4),
                irq: at(5),
                softirq: at(6),
                steal: at(7),
                ..CpuTimes::default()
            });
        } else if tag == "ctxt" {
            out.ctxt = parts.next().and_then(|p| p.parse().ok());
        } else if tag == "intr" {
            out.intr = parts.next().and_then(|p| p.parse().ok());
        }
    }
    out
}

/// Parse /proc/loadavg into (running tasks, total tasks). The total counts
/// kernel scheduling entities, which is the thread count.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_loadavg_tasks(text: &str) -> Option<(u64, u64)> {
    let field = text.split_whitespace().nth(3)?;
    let (running, total) = field.split_once('/')?;
    Some((running.parse().ok()?, total.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROC_STAT: &str = "cpu  4705 356 584 3699176 23060 0 277 0 0 0\n\
cpu0 1393280 32966 572056 13343292 6130 0 17875 0 23933 0\n\
cpu1 1000 10 300 9000 50 5 7 0 0 0\n\
intr 114930548 113199788 3 0 5 263\n\
ctxt 1990473\n\
btime 1062191376\n\
procs_running 2\n";

    #[test]
    fn proc_stat_reads_per_core_lines_only() {
        let parsed = parse_proc_stat(PROC_STAT);
        assert_eq!(parsed.per_core.len(), 2);
        assert_eq!(parsed.per_core[1].user, 1000.0);
        assert_eq!(parsed.per_core[1].irq, 5.0);
        assert_eq!(parsed.ctxt, Some(1990473));
        assert_eq!(parsed.intr, Some(114930548));
    }

    #[test]
    fn loadavg_tasks() {
        assert_eq!(parse_loadavg_tasks("0.20 0.18 0.12 1/80 11206\n"), Some((1, 80)));
        assert_eq!(parse_loadavg_tasks("garbage"), None);
    }

    #[test]
    fn linux_breakdown_excludes_iowait_from_busy() {
        let prev = CpuTimes::default();
        let cur = CpuTimes { user: 30.0, nice: 10.0, system: 20.0, idle: 30.0, iowait: 10.0, ..CpuTimes::default() };
        let b = breakdown(&prev, &cur, Flavor::Linux).unwrap();
        assert_eq!(b.busy, 60.0);
        assert_eq!(b.user, 40.0);
        assert_eq!(b.system, 20.0);
        assert_eq!(b.iowait, Some(10.0));
        assert_eq!(b.dpc, None);
    }

    #[test]
    fn windows_breakdown_reports_dpc_and_interrupt() {
        let prev = CpuTimes::default();
        let cur = CpuTimes { user: 20.0, system: 10.0, idle: 60.0, dpc: 5.0, interrupt: 5.0, ..CpuTimes::default() };
        let b = breakdown(&prev, &cur, Flavor::Windows).unwrap();
        assert_eq!(b.busy, 40.0);
        assert_eq!(b.dpc, Some(5.0));
        assert_eq!(b.interrupt, Some(5.0));
        assert_eq!(b.iowait, None);
    }

    #[test]
    fn no_elapsed_time_is_none() {
        let t = CpuTimes { user: 5.0, ..CpuTimes::default() };
        assert_eq!(breakdown(&t, &t, Flavor::Linux), None);
    }

    #[test]
    fn counters_going_backwards_do_not_produce_negatives() {
        let prev = CpuTimes { user: 100.0, idle: 100.0, ..CpuTimes::default() };
        let cur = CpuTimes { user: 50.0, idle: 200.0, ..CpuTimes::default() };
        let b = breakdown(&prev, &cur, Flavor::Linux).unwrap();
        assert_eq!(b.busy, 0.0);
        assert_eq!(b.idle, 100.0);
    }
}
