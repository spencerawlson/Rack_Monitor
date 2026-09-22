//! Percentage and rate arithmetic shared by every collector.

use std::collections::HashMap;
use std::time::Instant;

/// Round to one decimal place, the precision shown anywhere on the dashboard.
pub fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// Percentage of `total`, or None when the denominator is unusable.
///
/// The result is clamped to 0..=100 so a transient counter anomaly can never
/// push a donut chart past full scale.
pub fn percent_of(used: f64, total: f64) -> Option<f64> {
    if !used.is_finite() || !total.is_finite() || total <= 0.0 {
        return None;
    }
    Some(round1((used / total * 100.0).clamp(0.0, 100.0)))
}

/// Converts monotonically increasing counters into per-second rates.
#[derive(Default)]
pub struct RateTracker {
    previous: HashMap<String, (u64, Instant)>,
}

impl RateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Per-second change of `key` since its previous sample.
    pub fn rate(&mut self, key: &str, value: u64) -> Option<f64> {
        self.rate_at(key, value, Instant::now())
    }

    /// As `rate`, with the sample time supplied by the caller.
    ///
    /// The first sample of a key has nothing to compare against and yields
    /// None. A smaller value than last time means the counter was reset (an
    /// adapter restarted, a disk was reattached) and yields 0 rather than a
    /// negative rate.
    pub fn rate_at(&mut self, key: &str, value: u64, now: Instant) -> Option<f64> {
        let previous = self.previous.insert(key.to_owned(), (value, now));
        let (prev_value, prev_time) = previous?;
        let elapsed = now.checked_duration_since(prev_time)?.as_secs_f64();
        if elapsed <= 0.0 {
            return None;
        }
        if value < prev_value {
            return Some(0.0);
        }
        Some((value - prev_value) as f64 / elapsed)
    }

    /// Forget keys that were not sampled this round, such as a removed disk.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.previous.retain(|k, _| keep(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn percent_basic() {
        assert_eq!(percent_of(50.0, 200.0), Some(25.0));
        assert_eq!(percent_of(206262779904.0, 254679183360.0), Some(81.0));
    }

    #[test]
    fn percent_rejects_unusable_denominator() {
        assert_eq!(percent_of(10.0, 0.0), None);
        assert_eq!(percent_of(10.0, -1.0), None);
        assert_eq!(percent_of(f64::NAN, 10.0), None);
    }

    #[test]
    fn percent_clamps_to_full_scale() {
        assert_eq!(percent_of(150.0, 100.0), Some(100.0));
        assert_eq!(percent_of(-5.0, 100.0), Some(0.0));
    }

    #[test]
    fn first_sample_has_no_rate() {
        let mut tracker = RateTracker::new();
        assert_eq!(tracker.rate("k", 1000), None);
    }

    #[test]
    fn rate_is_per_second() {
        let mut tracker = RateTracker::new();
        let t0 = Instant::now();
        tracker.rate_at("k", 1000, t0);
        let rate = tracker.rate_at("k", 3000, t0 + Duration::from_secs(2)).unwrap();
        assert!((rate - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn counter_reset_is_zero_not_negative() {
        let mut tracker = RateTracker::new();
        let t0 = Instant::now();
        tracker.rate_at("k", 5000, t0);
        assert_eq!(tracker.rate_at("k", 10, t0 + Duration::from_secs(1)), Some(0.0));
    }

    #[test]
    fn retain_drops_unsampled_keys() {
        let mut tracker = RateTracker::new();
        tracker.rate("a", 1);
        tracker.rate("b", 1);
        tracker.retain(|k| k == "a");
        // "b" is new again, so it has no previous sample.
        assert_eq!(tracker.rate("b", 5), None);
    }
}
