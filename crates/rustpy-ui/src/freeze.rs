//! UI freeze detector (roadmap §8 "Freeze detector reporting UI stalls").
//!
//! The UI thread marks the start and end of every frame. A watchdog thread
//! notices a frame running longer than the threshold *while it is still
//! running* and logs a warning with what the IDE was doing: the last action
//! and input, the running background tasks and (on Linux) the state of every
//! thread of the process — the closest thing to a full thread dump that
//! needs no debugger. When the frame finally ends, its duration is logged and
//! kept for the UI's non-modal notice. Frame times of frames with input are
//! also summarized in the debug log (typing latency). Frames during a grace
//! period after start-up (fonts, first layout) are not reported.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// A frame that took longer than the threshold.
#[derive(Clone, Debug, PartialEq)]
pub struct Freeze {
    pub duration: Duration,
    /// The last action or input before the freeze.
    pub action: String,
}

/// The notice shown for `f`.
pub fn notice_text(f: &Freeze) -> String {
    let secs = f.duration.as_secs_f64();
    let action = if f.action == "none" {
        String::new()
    } else {
        format!(" (last action: {})", f.action)
    };
    format!("The IDE was unresponsive for {secs:.1} s{action}.")
}

struct Shared {
    threshold: Duration,
    /// Frames starting this soon after `epoch` are not reported.
    grace: Duration,
    epoch: Instant,
    /// Start of the running frame (ms since `epoch`, plus one); 0 between frames.
    frame_start: AtomicU64,
    /// Number of the running frame; the watchdog reports each frame once.
    frame_seq: AtomicU64,
    reported_seq: AtomicU64,
    last_action: Mutex<String>,
    finished: Mutex<Option<Freeze>>,
    stats: Mutex<FrameStats>,
    watchdog: AtomicBool,
}

/// Frame times of frames that handled input, summarized every few seconds.
#[derive(Default)]
struct FrameStats {
    samples: Vec<u32>,
    since: Option<Instant>,
}

impl FrameStats {
    const WINDOW: Duration = Duration::from_secs(10);

    /// Records `ms`; returns a summary line once per window.
    fn record(&mut self, ms: u32, now: Instant) -> Option<String> {
        let since = *self.since.get_or_insert(now);
        self.samples.push(ms);
        if now.duration_since(since) < Self::WINDOW || self.samples.len() < 5 {
            return None;
        }
        let mut s = std::mem::take(&mut self.samples);
        self.since = Some(now);
        s.sort_unstable();
        let pct = |p: usize| s[(s.len() - 1) * p / 100];
        Some(format!(
            "frames with input: n={} p50={} ms p95={} ms max={} ms",
            s.len(),
            pct(50),
            pct(95),
            s[s.len() - 1]
        ))
    }
}

/// Watches the UI thread; cheap to clone.
#[derive(Clone)]
pub struct FreezeDetector {
    shared: Arc<Shared>,
}

impl FreezeDetector {
    /// Frames longer than this are reported.
    pub const DEFAULT_THRESHOLD: Duration = Duration::from_secs(1);
    /// Start-up time during which slow frames are expected.
    pub const STARTUP_GRACE: Duration = Duration::from_secs(5);

    /// A detector reporting frames longer than `threshold` that start at
    /// least `grace` after its creation.
    pub fn new(threshold: Duration, grace: Duration) -> Self {
        Self {
            shared: Arc::new(Shared {
                threshold,
                grace,
                epoch: Instant::now(),
                frame_start: AtomicU64::new(0),
                frame_seq: AtomicU64::new(0),
                reported_seq: AtomicU64::new(0),
                last_action: Mutex::new(String::new()),
                finished: Mutex::new(None),
                stats: Mutex::new(FrameStats::default()),
                watchdog: AtomicBool::new(false),
            }),
        }
    }

    pub fn threshold(&self) -> Duration {
        self.shared.threshold
    }

    /// Starts the watchdog thread (once); it ends when every detector clone is dropped.
    pub fn start_watchdog(&self) {
        if self.shared.watchdog.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(&self.shared);
        let poll = (self.shared.threshold / 4)
            .clamp(Duration::from_millis(20), Duration::from_millis(250));
        let spawned = std::thread::Builder::new().name("freeze-watchdog".into()).spawn(move || loop {
            std::thread::sleep(poll);
            let Some(shared) = weak.upgrade() else { return };
            if let Some(running) = shared.frame_running_for().filter(|_| shared.armed()) {
                let seq = shared.frame_seq.load(Ordering::Acquire);
                if running >= shared.threshold && shared.reported_seq.swap(seq, Ordering::AcqRel) != seq {
                    log::warn!("UI freeze: the UI thread has been busy for {} ms (last action: {}). {}", running.as_millis(), shared.action(), process_dump());
                }
            }
        });
        if let Err(e) = spawned {
            log::warn!("freeze detector unavailable: {e}");
        }
    }

    /// Marks the start of a frame on the UI thread; the frame ends when the
    /// guard is dropped.
    pub fn frame(&self) -> FrameGuard {
        let s = &self.shared;
        s.frame_seq.fetch_add(1, Ordering::AcqRel);
        s.frame_start
            .store(s.epoch.elapsed().as_millis() as u64 + 1, Ordering::Release);
        FrameGuard {
            detector: self.clone(),
            started: Instant::now(),
            had_input: false,
        }
    }

    /// Records what the user did last ("Find Usages", "typing", …).
    pub fn note_action(&self, action: impl Into<String>) {
        *self.shared.last_action.lock() = action.into();
    }

    pub fn last_action(&self) -> String {
        self.shared.action()
    }

    /// The last freeze since the previous call, for the UI notice.
    pub fn take_freeze(&self) -> Option<Freeze> {
        self.shared.finished.lock().take()
    }

    fn end_frame(&self, started: Instant, had_input: bool) -> Option<Freeze> {
        let s = &self.shared;
        s.frame_start.store(0, Ordering::Release);
        let duration = started.elapsed();
        crate::performance_trace::frame(duration, had_input);
        if had_input {
            if let Some(summary) = s.stats.lock().record(
                duration.as_millis().min(u32::MAX as u128) as u32,
                Instant::now(),
            ) {
                log::debug!("{summary}");
            }
        }
        if duration < s.threshold || started.duration_since(s.epoch) < s.grace {
            return None;
        }
        let freeze = Freeze {
            duration,
            action: s.action(),
        };
        log::warn!(
            "UI freeze: a frame took {} ms (last action: {})",
            duration.as_millis(),
            freeze.action
        );
        *s.finished.lock() = Some(freeze.clone());
        Some(freeze)
    }
}

impl Shared {
    /// Past the start-up grace period.
    fn armed(&self) -> bool {
        self.epoch.elapsed() >= self.grace
    }

    fn frame_running_for(&self) -> Option<Duration> {
        let start = self.frame_start.load(Ordering::Acquire);
        (start != 0).then(|| {
            Duration::from_millis(
                (self.epoch.elapsed().as_millis() as u64 + 1).saturating_sub(start),
            )
        })
    }

    fn action(&self) -> String {
        let a = self.last_action.lock();
        if a.is_empty() {
            "none".into()
        } else {
            a.clone()
        }
    }
}

/// Ends its frame when dropped (so early returns still end the frame).
pub struct FrameGuard {
    detector: FreezeDetector,
    started: Instant,
    had_input: bool,
}

impl FrameGuard {
    /// The frame handled user input (its time counts towards the latency summary).
    pub fn set_had_input(&mut self, had_input: bool) {
        self.had_input = had_input;
    }
}

impl Drop for FrameGuard {
    fn drop(&mut self) {
        self.detector.end_frame(self.started, self.had_input);
    }
}

/// Running background tasks and, on Linux, the process's threads with their
/// scheduler state (`R` running, `S` sleeping, `D` waiting on I/O, …).
fn process_dump() -> String {
    let tasks: Vec<String> = rustpy_core::TaskManager::global()
        .running_all()
        .iter()
        .map(|t| format!("{} ({} ms)", t.title, t.elapsed.as_millis()))
        .collect();
    let mut out = format!("Background tasks: [{}].", tasks.join(", "));
    if let Some(threads) = linux_threads() {
        out.push_str(&format!(" Threads: {threads}."));
    }
    out
}

#[cfg(target_os = "linux")]
fn linux_threads() -> Option<String> {
    let mut threads = Vec::new();
    for entry in std::fs::read_dir("/proc/self/task").ok()?.flatten() {
        let stat = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
        // `pid (comm) state ...`: the name may contain spaces or parentheses.
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let name = stat[open + 1..close].to_string();
        let state = stat[close + 1..]
            .split_whitespace()
            .next()
            .unwrap_or("?")
            .to_string();
        threads.push((name, state));
    }
    Some(summarize_threads(threads))
}

/// `name[state]` per thread; threads of a pool (same name up to a numeric
/// suffix, same state) are counted: `llvmpipe-*[S]×32 rustpy[R] task-7[D]`.
fn summarize_threads(threads: Vec<(String, String)>) -> String {
    let mut groups: std::collections::BTreeMap<(String, String), Vec<String>> =
        std::collections::BTreeMap::new();
    for (name, state) in threads {
        let stem = name
            .trim_end_matches(|c: char| c.is_ascii_digit())
            .to_string();
        groups.entry((stem, state)).or_default().push(name);
    }
    let parts: Vec<String> = groups
        .into_iter()
        .map(|((stem, state), names)| match names.as_slice() {
            [one] => format!("{one}[{state}]"),
            many if many.iter().all(|n| *n == stem) => format!("{stem}[{state}]×{}", many.len()),
            many => format!("{stem}*[{state}]×{}", many.len()),
        })
        .collect();
    parts.join(" ")
}

#[cfg(not(target_os = "linux"))]
fn linux_threads() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_frames_are_not_freezes() {
        let d = FreezeDetector::new(Duration::from_millis(200), Duration::ZERO);
        drop(d.frame());
        assert_eq!(d.take_freeze(), None);
    }

    #[test]
    fn long_frames_are_reported_with_the_last_action() {
        let d = FreezeDetector::new(Duration::from_millis(30), Duration::ZERO);
        d.note_action("Find Usages");
        {
            let _frame = d.frame();
            std::thread::sleep(Duration::from_millis(60));
        }
        let f = d.take_freeze().expect("freeze");
        assert!(f.duration >= Duration::from_millis(30));
        assert_eq!(f.action, "Find Usages");
        assert_eq!(d.take_freeze(), None, "reported once");
    }

    #[test]
    fn the_watchdog_sees_a_frame_while_it_is_still_running() {
        let d = FreezeDetector::new(Duration::from_millis(40), Duration::ZERO);
        d.start_watchdog();
        d.start_watchdog();
        let frame = d.frame();
        let deadline = Instant::now() + Duration::from_secs(10);
        while d.shared.reported_seq.load(Ordering::Acquire)
            != d.shared.frame_seq.load(Ordering::Acquire)
        {
            assert!(Instant::now() < deadline, "watchdog did not report");
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(frame);
        assert!(d.take_freeze().is_some());
        // The next frame starts clean.
        drop(d.frame());
        assert_eq!(d.take_freeze(), None);
    }

    #[test]
    fn slow_start_up_frames_are_not_freezes() {
        let d = FreezeDetector::new(Duration::from_millis(20), Duration::from_secs(3600));
        {
            let _frame = d.frame();
            std::thread::sleep(Duration::from_millis(40));
        }
        assert_eq!(d.take_freeze(), None);
        assert!(!d.shared.armed());
    }

    #[test]
    fn notices_read_naturally() {
        let f = Freeze {
            duration: Duration::from_millis(2340),
            action: "Find Usages".into(),
        };
        assert_eq!(
            notice_text(&f),
            "The IDE was unresponsive for 2.3 s (last action: Find Usages)."
        );
        assert_eq!(
            notice_text(&Freeze {
                duration: Duration::from_secs(3),
                action: "none".into()
            }),
            "The IDE was unresponsive for 3.0 s."
        );
    }

    #[test]
    fn frame_stats_summarize_once_per_window() {
        let mut stats = FrameStats::default();
        let t0 = Instant::now();
        for ms in [5, 7, 9, 11] {
            assert_eq!(stats.record(ms, t0), None);
        }
        let line = stats.record(40, t0 + FrameStats::WINDOW).unwrap();
        assert_eq!(line, "frames with input: n=5 p50=9 ms p95=11 ms max=40 ms");
        assert_eq!(stats.record(3, t0 + FrameStats::WINDOW), None);
    }

    #[test]
    fn thread_summaries_count_pools() {
        let t = |n: &str, s: &str| (n.to_string(), s.to_string());
        let threads = vec![
            t("rustpy", "R"),
            t("llvmpipe-0", "S"),
            t("llvmpipe-1", "S"),
            t("llvmpipe-12", "S"),
            t("rustpy", "S"),
            t("rustpy", "S"),
            t("task-7", "D"),
            t("task-8", "S"),
        ];
        assert_eq!(
            summarize_threads(threads),
            "llvmpipe-*[S]×3 rustpy[R] rustpy[S]×2 task-7[D] task-8[S]"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn thread_dump_names_threads() {
        let dump = process_dump();
        assert!(dump.contains("Background tasks: ["), "{dump}");
        assert!(dump.contains("Threads:"), "{dump}");
    }
}
