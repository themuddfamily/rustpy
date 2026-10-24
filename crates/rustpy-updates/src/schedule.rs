//! When the automatic update check runs: on startup and, while the IDE stays open, again
//! whenever a day has passed since the last check. A check that could not reach the server
//! (offline) is retried an hour later in the same session; the persisted time of the last
//! check only moves when the server answered, so the next start tries again too.

/// The automatic check runs at most once per this many seconds (a day).
pub const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

/// After an attempt that could not reach the server, the next automatic one waits this long.
pub const RETRY_AFTER_FAILURE_SECS: i64 = 60 * 60;

/// Whether an automatic check is due at `now` (Unix seconds): never checked (`last_check`
/// is 0), at least a day since the last check, or the clock was set back past it.
pub fn check_due(last_check: i64, now: i64) -> bool {
    last_check <= 0 || now < last_check || now - last_check >= CHECK_INTERVAL_SECS
}

/// Seconds until the next automatic check is due (0 when it is due now).
pub fn seconds_until_due(last_check: i64, now: i64) -> i64 {
    if check_due(last_check, now) {
        0
    } else {
        CHECK_INTERVAL_SECS - (now - last_check)
    }
}

/// Automatic-check state of one IDE session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scheduler {
    /// No automatic attempt before this time (Unix seconds); set after a failed attempt.
    retry_at: i64,
    /// A check (automatic or manual) is running.
    running: bool,
}

impl Scheduler {
    /// Whether to start an automatic check at `now`: enabled, nothing running, not waiting
    /// after a failure, and [`check_due`].
    pub fn should_start(&self, enabled: bool, last_check: i64, now: i64) -> bool {
        enabled && !self.running && now >= self.retry_at && check_due(last_check, now)
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// A check started (automatic or manual).
    pub fn started(&mut self) {
        self.running = true;
    }

    /// The check ended. `reached_server`: the server answered (the caller records `now` as
    /// the last check); otherwise automatic attempts pause for [`RETRY_AFTER_FAILURE_SECS`].
    pub fn finished(&mut self, reached_server: bool, now: i64) {
        self.running = false;
        if !reached_server {
            self.retry_at = now + RETRY_AFTER_FAILURE_SECS;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = CHECK_INTERVAL_SECS;
    const T: i64 = 1_790_000_000;

    #[test]
    fn due_once_a_day() {
        assert!(check_due(0, T), "never checked");
        assert!(!check_due(T, T));
        assert!(!check_due(T, T + DAY - 1));
        assert!(check_due(T, T + DAY));
        assert!(check_due(T, T + 10 * DAY));
        assert!(check_due(T, T - 60), "clock set back");
        assert_eq!(seconds_until_due(T, T + 3600), DAY - 3600);
        assert_eq!(seconds_until_due(T, T + DAY), 0);
        assert_eq!(seconds_until_due(0, T), 0);
    }

    #[test]
    fn scheduler_runs_one_check_and_waits_after_failures() {
        let mut s = Scheduler::default();
        assert!(
            !s.should_start(false, 0, T),
            "automatic checks switched off"
        );
        assert!(s.should_start(true, 0, T));
        s.started();
        assert!(s.is_running());
        assert!(!s.should_start(true, 0, T + 1), "one check at a time");
        // Offline: no new attempt for an hour although the last check is still a day old.
        s.finished(false, T + 5);
        assert!(!s.should_start(true, 0, T + 5 + RETRY_AFTER_FAILURE_SECS - 1));
        assert!(s.should_start(true, 0, T + 5 + RETRY_AFTER_FAILURE_SECS));
        // Answered: the caller records the time, and the next check is a day later.
        s.started();
        let checked = T + 7200;
        s.finished(true, checked);
        assert!(!s.should_start(true, checked, checked + 1));
        assert!(!s.should_start(true, checked, checked + DAY - 1));
        assert!(s.should_start(true, checked, checked + DAY));
    }
}
