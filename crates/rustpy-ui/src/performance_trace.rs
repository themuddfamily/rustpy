//! Opt-in bounded local acceptance measurements, not general telemetry.
//! No worker, network, process owner or implicit output directory is created.
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Write,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
struct Sink {
    file: File,
    written: usize,
    typing: Option<(Instant, usize, bool)>,
    completion: Option<Instant>,
    first_frame: bool,
    index: Option<(bool, usize, u64)>,
    mode: Option<(bool, bool)>,
    /// Completed frames, written or not (index rows carry it, so busy
    /// intervals stay measurable when `frame` rows are count-only).
    frames: u64,
    rows: RowLimits,
}

/// Default row cap when `RUSTPY_PERFORMANCE_TRACE_MAX_ROWS` is unset.
const DEFAULT_MAX_ROWS: usize = 100_000;

/// Which rows reach the file. High-volume kinds named in
/// `RUSTPY_PERFORMANCE_TRACE_COUNT_ONLY` (comma-separated) are only counted;
/// their running totals are written as `suppressed_counts` rows at most once
/// per second. Reaching the cap writes one final `trace_truncated` row, so a
/// truncated trace is explicit rather than silently short.
struct RowLimits {
    max_rows: usize,
    count_only: Vec<String>,
    suppressed: BTreeMap<String, u64>,
    last_summary: Option<Instant>,
}

impl RowLimits {
    fn from_env() -> Self {
        let max_rows = std::env::var("RUSTPY_PERFORMANCE_TRACE_MAX_ROWS")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|rows| *rows >= 2)
            .unwrap_or(DEFAULT_MAX_ROWS);
        let count_only = std::env::var("RUSTPY_PERFORMANCE_TRACE_COUNT_ONLY")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .map(String::from)
            .collect();
        Self {
            max_rows,
            count_only,
            suppressed: BTreeMap::new(),
            last_summary: None,
        }
    }

    /// The rows to write for one event of `kind`, given `written` rows so far.
    fn admit(
        &mut self,
        kind: &str,
        data: Value,
        written: usize,
        now: Instant,
    ) -> Vec<(String, Value)> {
        if written >= self.max_rows {
            return Vec::new();
        }
        let mut rows = Vec::new();
        if self.count_only.iter().any(|counted| counted == kind) {
            *self.suppressed.entry(kind.to_string()).or_default() += 1;
            if self
                .last_summary
                .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(1))
            {
                self.last_summary = Some(now);
                rows.push((
                    "suppressed_counts".to_string(),
                    json!({"counts":self.suppressed}),
                ));
            }
        } else {
            rows.push((kind.to_string(), data));
        }
        let room = self.max_rows - written;
        if rows.len() >= room {
            rows.truncate(room - 1);
            rows.push((
                "trace_truncated".to_string(),
                json!({"max_rows":self.max_rows,"suppressed":self.suppressed}),
            ));
        }
        rows
    }
}
static SINK: OnceLock<Option<Mutex<Sink>>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);
fn sink() -> Option<&'static Mutex<Sink>> {
    SINK.get_or_init(|| {
        let path = std::env::var_os("RUSTPY_PERFORMANCE_TRACE")?;
        // Never overwrite an earlier measurement. The final driver owns paths.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .ok()?;
        Some(Mutex::new(Sink {
            file,
            written: 0,
            typing: None,
            completion: None,
            first_frame: false,
            index: None,
            mode: None,
            frames: 0,
            rows: RowLimits::from_env(),
        }))
    })
    .as_ref()
}
pub(crate) fn enabled() -> bool {
    sink().is_some()
}
fn write(sink: &mut Sink, kind: &str, data: Value) {
    for (kind, data) in sink.rows.admit(kind, data, sink.written, Instant::now()) {
        write_row(sink, &kind, data);
    }
}
fn write_row(sink: &mut Sink, kind: &str, data: Value) {
    let epoch_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64;
    let row =
        json!({"schema":1,"pid":std::process::id(),"epoch_ns":epoch_ns,"kind":kind,"data":data});
    if let Ok(bytes) = serde_json::to_vec(&row) {
        if sink
            .file
            .write_all(&bytes)
            .and_then(|_| sink.file.write_all(b"\n"))
            .is_ok()
        {
            sink.written += 1;
        }
    }
}
#[cfg(test)]
thread_local! {
    /// Events emitted on this test thread, so tests can assert on them
    /// without enabling the process-wide trace file.
    static RECORDED: std::cell::RefCell<Vec<(String, Value)>> = const { std::cell::RefCell::new(Vec::new()) };
}
/// Takes the events emitted so far on the current test thread.
#[cfg(test)]
pub(crate) fn take_recorded() -> Vec<(String, Value)> {
    RECORDED.with(|recorded| std::mem::take(&mut *recorded.borrow_mut()))
}
pub(crate) fn event(kind: &str, data: Value) {
    #[cfg(test)]
    RECORDED.with(|recorded| recorded.borrow_mut().push((kind.to_string(), data.clone())));
    if let Some(sink) = sink() {
        if let Ok(mut sink) = sink.lock() {
            write(&mut sink, kind, data);
        }
    }
}
pub(crate) fn next_id() -> u64 {
    if enabled() {
        NEXT.fetch_add(1, Ordering::Relaxed)
    } else {
        0
    }
}
pub(crate) fn input(input: &egui::RawInput, document_lines: usize, large_file: bool) {
    if !input
        .events
        .iter()
        .any(|event| matches!(event, egui::Event::Text(_) | egui::Event::Paste(_)))
    {
        return;
    }
    if let Some(sink) = sink() {
        if let Ok(mut sink) = sink.lock() {
            sink.typing
                .get_or_insert_with(|| (Instant::now(), document_lines, large_file));
        }
    }
}
pub(crate) fn completion_requested() {
    if let Some(sink) = sink() {
        if let Ok(mut sink) = sink.lock() {
            sink.completion = Some(Instant::now());
        }
    }
}
pub(crate) fn completion_drawn(items: usize) {
    if items == 0 {
        return;
    }
    if let Some(sink) = sink() {
        if let Ok(mut sink) = sink.lock() {
            if let Some(start) = sink.completion.take() {
                write(
                    &mut sink,
                    "completion_popup",
                    json!({"ms":start.elapsed().as_secs_f64()*1000.0,"items":items}),
                );
            }
        }
    }
}
pub(crate) fn frame(duration: Duration, had_input: bool) {
    if let Some(sink) = sink() {
        if let Ok(mut sink) = sink.lock() {
            sink.frames += 1;
            if !sink.first_frame {
                sink.first_frame = true;
                write(&mut sink, "first_frame", json!({}));
            }
            write(
                &mut sink,
                "frame",
                json!({"ms":duration.as_secs_f64()*1000.0,"input":had_input}),
            );
            if let Some((start, document_lines, large_file)) = sink.typing.take() {
                write(
                    &mut sink,
                    "typing_frame",
                    json!({"ms":start.elapsed().as_secs_f64()*1000.0,"document_lines":document_lines,"large_file":large_file}),
                );
            }
        }
    }
}
pub(crate) fn index(busy: bool, files: usize, generation: u64) {
    if let Some(sink) = sink() {
        if let Ok(mut sink) = sink.lock() {
            if sink.index != Some((busy, files, generation)) {
                sink.index = Some((busy, files, generation));
                let frames = sink.frames;
                write(
                    &mut sink,
                    "index",
                    json!({"busy":busy,"files":files,"generation":generation,"frames":frames}),
                );
            }
        }
    }
}

/// Captures the actual current GL context, never a requested backend label.
/// Called by the native AppCreator after context initialization.
pub(crate) fn record_creation_context(cc: &eframe::CreationContext<'_>) {
    if !enabled() {
        return;
    }
    if let Some(gl) = &cc.gl {
        use eframe::glow::HasContext;
        // SAFETY: AppCreator is invoked on the native thread with its GL
        // context current, before handing rendering to the application.
        let (vendor, renderer, version) = unsafe {
            (
                gl.get_parameter_string(eframe::glow::VENDOR),
                gl.get_parameter_string(eframe::glow::RENDERER),
                gl.get_parameter_string(eframe::glow::VERSION),
            )
        };
        event(
            "renderer",
            json!({"backend":"glow","vendor":vendor,"renderer":renderer,"version":version}),
        );
    } else {
        event(
            "renderer_unavailable",
            json!({"reason":"No current GL context; native adapter identity must be captured by its backend"}),
        );
    }
}

pub(crate) fn mode(power_save: bool, indexing: bool) {
    if let Some(sink) = sink() {
        if let Ok(mut sink) = sink.lock() {
            if sink.mode != Some((power_save, indexing)) {
                sink.mode = Some((power_save, indexing));
                write(
                    &mut sink,
                    "mode",
                    json!({"power_save":power_save,"indexing":indexing}),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_rows: usize, count_only: &[&str]) -> RowLimits {
        RowLimits {
            max_rows,
            count_only: count_only.iter().map(|kind| kind.to_string()).collect(),
            suppressed: BTreeMap::new(),
            last_summary: None,
        }
    }

    #[test]
    fn count_only_kinds_are_summarised_at_most_once_per_second() {
        let mut rows = limits(100, &["frame"]);
        let start = Instant::now();
        let first = rows.admit("frame", json!({}), 0, start);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, "suppressed_counts");
        assert!(rows.admit("frame", json!({}), 1, start).is_empty());
        let later = rows.admit("frame", json!({}), 1, start + Duration::from_secs(1));
        assert_eq!(later[0].1["counts"]["frame"], 3);
        let other = rows.admit("index", json!({"busy":true}), 2, start);
        assert_eq!(other[0].0, "index");
    }

    #[test]
    fn reaching_the_cap_writes_an_explicit_truncation_row_then_nothing() {
        let mut rows = limits(3, &[]);
        let now = Instant::now();
        assert_eq!(rows.admit("frame", json!({}), 1, now)[0].0, "frame");
        let last = rows.admit("frame", json!({}), 2, now);
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].0, "trace_truncated");
        assert_eq!(last[0].1["max_rows"], 3);
        assert!(rows.admit("frame", json!({}), 3, now).is_empty());
    }
}
