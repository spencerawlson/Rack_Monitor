//! The rolling window behind the dashboard sparklines.
//!
//! Points are taken at a fixed cadence, so a point's age is its distance from
//! the end of the series and no timestamp has to be sent for each one.
//!
//! A sample that could not be read is stored as None. The line then breaks
//! where collection stopped, which is the truth; a zero would be a reading
//! nobody took.

use std::collections::VecDeque;

use crate::model::{History, Primary};

/// One fixed-cadence series, oldest first, capped at `capacity`.
///
/// A capacity of zero is a disabled series: it accepts pushes and stays
/// empty, so turning history off needs no branch at the call site.
pub struct Ring {
    points: VecDeque<Option<f64>>,
    capacity: usize,
}

impl Ring {
    pub fn new(capacity: usize) -> Self {
        Self { points: VecDeque::with_capacity(capacity), capacity }
    }

    /// Append a point, evicting the oldest once the window is full.
    pub fn push(&mut self, value: Option<f64>) {
        if self.capacity == 0 {
            return;
        }
        while self.points.len() >= self.capacity {
            self.points.pop_front();
        }
        self.points.push_back(value);
    }

    pub fn points(&self) -> Vec<Option<f64>> {
        self.points.iter().copied().collect()
    }
}

/// The three headline percentages tracked together, so every chart on a panel
/// covers exactly the same span of time.
pub struct Series {
    cpu: Ring,
    memory: Ring,
    disk: Ring,
    step_seconds: f64,
    capacity: usize,
}

impl Series {
    pub fn new(capacity: usize, step_seconds: f64) -> Self {
        Self {
            cpu: Ring::new(capacity),
            memory: Ring::new(capacity),
            disk: Ring::new(capacity),
            step_seconds,
            capacity,
        }
    }

    /// Record one tick. Every series advances even when a figure is missing,
    /// so the three stay aligned on the same time axis.
    pub fn push(&mut self, primary: &Primary) {
        self.cpu.push(primary.cpu_percent);
        self.memory.push(primary.memory_percent);
        self.disk.push(primary.disk_percent);
    }

    pub fn history(&self) -> History {
        History {
            step_seconds: self.step_seconds,
            capacity: self.capacity,
            cpu: self.cpu.points(),
            memory: self.memory.points(),
            disk: self.disk.points(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oldest_point_is_evicted_when_full() {
        let mut ring = Ring::new(3);
        for v in [1.0, 2.0, 3.0, 4.0] {
            ring.push(Some(v));
        }
        assert_eq!(ring.points(), vec![Some(2.0), Some(3.0), Some(4.0)]);
    }

    #[test]
    fn a_missing_sample_is_a_gap_not_a_zero() {
        let mut ring = Ring::new(3);
        ring.push(Some(50.0));
        ring.push(None);
        ring.push(Some(60.0));
        assert_eq!(ring.points(), vec![Some(50.0), None, Some(60.0)]);
    }

    #[test]
    fn zero_capacity_stays_empty() {
        let mut ring = Ring::new(0);
        ring.push(Some(1.0));
        assert!(ring.points().is_empty());
    }

    #[test]
    fn a_series_advances_every_track_together() {
        let mut series = Series::new(4, 1.0);
        series.push(&Primary {
            cpu_percent: Some(10.0),
            memory_percent: None,
            disk_percent: Some(30.0),
            disk_mount: Some("C:\\".into()),
        });
        series.push(&Primary::default());

        let history = series.history();
        assert_eq!(history.step_seconds, 1.0);
        assert_eq!(history.capacity, 4);
        assert_eq!(history.cpu, vec![Some(10.0), None]);
        assert_eq!(history.memory, vec![None, None]);
        assert_eq!(history.disk, vec![Some(30.0), None]);
    }
}
