use std::time::Duration;

/// Total number of bytes that should have been written `elapsed` after start
/// at the given rate. Pacing against this cumulative target keeps the
/// long-run average on the configured rate without accumulating drift.
pub fn bytes_due(rate_bytes_per_sec: f64, elapsed: Duration) -> u64 {
    (rate_bytes_per_sec * elapsed.as_secs_f64()) as u64
}

/// Decides how many bytes may have been written by a given moment. It follows
/// `bytes_due`, except that a backlog of more than one second of output is
/// forgiven rather than caught up, so a stall (slow disk, paused process) is
/// not followed by a burst far above the configured rate.
pub struct Pacer {
    rate_bytes_per_sec: f64,
    max_backlog: u64,
    forgiven: u64,
}

impl Pacer {
    pub fn new(rate_bytes_per_sec: f64) -> Pacer {
        Pacer {
            rate_bytes_per_sec,
            max_backlog: bytes_due(rate_bytes_per_sec, Duration::from_secs(1)),
            forgiven: 0,
        }
    }

    /// Total bytes that may have been written `elapsed` after start, given
    /// that `written` bytes have been written so far.
    pub fn target(&mut self, elapsed: Duration, written: u64) -> u64 {
        let due = bytes_due(self.rate_bytes_per_sec, elapsed).saturating_sub(self.forgiven);
        let backlog = due.saturating_sub(written);
        if backlog <= self.max_backlog {
            return due;
        }
        self.forgiven = self.forgiven.saturating_add(backlog - self.max_backlog);
        written.saturating_add(self.max_backlog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: f64 = crate::config::MIB as f64;

    #[test]
    fn target_follows_the_rate_when_on_pace() {
        let mut pacer = Pacer::new(MIB);
        assert_eq!(pacer.target(Duration::from_millis(500), 0), 512 * 1024);
    }

    #[test]
    fn a_short_lag_is_caught_up_in_full() {
        let mut pacer = Pacer::new(MIB);
        assert_eq!(pacer.target(Duration::from_millis(100), 0), 104_857);
        assert_eq!(pacer.target(Duration::from_millis(300), 0), 314_572);
    }

    #[test]
    fn a_backlog_is_capped_at_one_second_of_output() {
        let mut pacer = Pacer::new(MIB);
        assert_eq!(pacer.target(Duration::from_secs(10), 0), 1024 * 1024);
    }

    #[test]
    fn a_forgiven_backlog_is_never_made_up_later() {
        let mut pacer = Pacer::new(MIB);
        let written = pacer.target(Duration::from_secs(10), 0);
        assert_eq!(
            pacer.target(Duration::from_millis(10_500), written),
            written + 512 * 1024
        );
    }

    #[test]
    fn target_does_not_overflow_at_an_extreme_rate() {
        let mut pacer = Pacer::new(f64::MAX);
        let written = u64::MAX - 10;
        assert!(pacer.target(Duration::from_secs(1), written) >= written);
    }

    #[test]
    fn nothing_is_due_at_start() {
        assert_eq!(bytes_due(MIB, Duration::ZERO), 0);
    }

    #[test]
    fn one_second_at_one_mib_per_second_is_one_mib() {
        assert_eq!(bytes_due(MIB, Duration::from_secs(1)), 1024 * 1024);
    }

    #[test]
    fn partial_seconds_are_proportional() {
        assert_eq!(bytes_due(10.0 * MIB, Duration::from_millis(100)), 1024 * 1024);
    }

    #[test]
    fn fractional_bytes_round_down() {
        assert_eq!(bytes_due(MIB, Duration::from_millis(100)), 104_857);
    }
}
