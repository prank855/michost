//! Monitor retry policy: backoff schedule and parking rules.
//!
//! The app owns the monitor I/O and probe; this module owns only when to try
//! again, so the timing rules are testable without a session.

use std::time::{Duration, Instant};

/// Retry bookkeeping for a monitor output that could not be opened. The
/// desired monitor intent (`monitor` / `monitor_enabled`) is never cleared by
/// a failure; this state only schedules the next attempt.
pub(crate) struct MonitorPending {
    pub(crate) next_at: Instant,
    pub(crate) attempts: u32,
    pub(crate) last_status: Option<i32>,
    pub(crate) last_message: Option<String>,
    pub(crate) waiting_for_probe: bool,
}

pub(crate) const MONITOR_RETRY_DELAYS_MS: [u64; 5] = [500, 1_000, 2_000, 5_000, 10_000];
pub(crate) const MONITOR_PROBE_INTERVAL: Duration = Duration::from_secs(30);
pub(crate) const MONITOR_NOTICE_RETRY: &str = "Retrying the monitor output; audio keeps running.";
pub(crate) const MONITOR_NOTICE_WAITING: &str =
    "Monitor output unavailable; waiting for a device change.";

/// Delay before the next monitor attempt after `attempts` consecutive
/// failures. The ladder is capped at the last entry.
pub(crate) fn monitor_retry_delay(attempts: u32) -> Duration {
    let index = (attempts.max(1) as usize - 1).min(MONITOR_RETRY_DELAYS_MS.len() - 1);
    Duration::from_millis(MONITOR_RETRY_DELAYS_MS[index])
}

/// Repeated missing-device failures are parked until a probe or device change
/// reports a new endpoint list, instead of retrying forever.
pub(crate) fn monitor_should_park(attempts: u32, waiting_for_probe: bool) -> bool {
    attempts >= 3 && waiting_for_probe
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delays_follow_the_backoff_ladder_and_cap() {
        let delays: Vec<u64> = (1..=7)
            .map(|attempt| monitor_retry_delay(attempt).as_millis() as u64)
            .collect();
        assert_eq!(
            delays,
            vec![500, 1_000, 2_000, 5_000, 10_000, 10_000, 10_000]
        );
        assert_eq!(monitor_retry_delay(0), monitor_retry_delay(1));
    }

    #[test]
    fn missing_devices_park_only_after_three_attempts() {
        assert!(!monitor_should_park(0, true));
        assert!(!monitor_should_park(2, true));
        assert!(monitor_should_park(3, true));
        assert!(!monitor_should_park(3, false));
    }

    #[test]
    fn probe_interval_is_bounded() {
        assert_eq!(MONITOR_PROBE_INTERVAL, Duration::from_secs(30));
        assert_eq!(
            MONITOR_NOTICE_RETRY,
            "Retrying the monitor output; audio keeps running."
        );
        assert_eq!(
            MONITOR_NOTICE_WAITING,
            "Monitor output unavailable; waiting for a device change."
        );
    }
}
