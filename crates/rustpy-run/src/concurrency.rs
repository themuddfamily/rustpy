//! Live thread/task timelines captured by a dependency-free Python instrumenter.
//! Process ownership, environment and termination use the shared streamed runner.
use crate::streamed::{PythonCommand, Streamed};
use rustpy_project::{Interpreter, RunConfig};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};
pub const RUNNER_SOURCE: &str = include_str!("concurrency_runner.py");
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureOptions {
    pub threads: bool,
    pub asyncio: bool,
    pub locks: bool,
    pub project_only: bool,
    pub max_events: usize,
    pub max_lanes: usize,
    pub roots: Vec<PathBuf>,
}
impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            threads: true,
            asyncio: true,
            locks: true,
            project_only: true,
            max_events: 100_000,
            max_lanes: 256,
            roots: Vec::new(),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Execution,
    LockWait,
    Await,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TraceEvent {
    pub kind: String,
    pub time: f64,
    pub lane: String,
    pub name: String,
    pub lane_kind: String,
    pub parent: Option<String>,
    pub span: u64,
    pub phase: Phase,
    pub file: Option<PathBuf>,
    pub line: usize,
    pub resource: Option<String>,
    pub depth: usize,
    pub dropped: usize,
    pub terminated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Span {
    pub start: f64,
    pub end: f64,
    pub name: String,
    pub phase: Phase,
    pub file: Option<PathBuf>,
    pub line: usize,
    pub resource: Option<String>,
    pub depth: usize,
    pub incomplete: bool,
}
impl Span {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Lane {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub parent: Option<String>,
    pub spans: Vec<Span>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConcurrencyReport {
    pub lanes: Vec<Lane>,
    pub duration: f64,
    pub event_count: usize,
    pub dropped: usize,
    pub finished: bool,
    pub terminated: bool,
    #[serde(skip)]
    pending: HashMap<u64, TraceEvent>,
}
impl ConcurrencyReport {
    pub fn apply(&mut self, event: TraceEvent) -> Result<(), String> {
        if !event.time.is_finite() || event.time < 0.0 {
            return Err("Invalid concurrency timestamp".into());
        }
        self.duration = self.duration.max(event.time);
        self.event_count += 1;
        match event.kind.as_str() {
            "lane" => {
                if let Some(lane) = self.lanes.iter_mut().find(|lane| lane.id == event.lane) {
                    lane.name = event.name;
                } else if !event.lane.is_empty() && self.lanes.len() < 1024 {
                    self.lanes.push(Lane {
                        id: event.lane,
                        name: event.name,
                        kind: event.lane_kind,
                        parent: event.parent,
                        spans: Vec::new(),
                    });
                }
            }
            "begin" if event.span != 0 && self.lanes.iter().any(|lane| lane.id == event.lane) => {
                self.pending.insert(event.span, event);
            }
            "end" => {
                if let Some(begin) = self.pending.remove(&event.span) {
                    if let Some(lane) = self.lanes.iter_mut().find(|lane| lane.id == begin.lane) {
                        lane.spans.push(Span {
                            start: begin.time,
                            end: event.time.max(begin.time),
                            name: begin.name,
                            phase: begin.phase,
                            file: begin.file,
                            line: begin.line,
                            resource: begin.resource,
                            depth: begin.depth,
                            incomplete: false,
                        });
                    }
                }
            }
            "truncated" => {
                self.dropped = self.dropped.max(event.dropped.max(1));
            }
            "finish" => {
                self.finished = true;
                self.dropped = self.dropped.max(event.dropped);
                self.terminated |= event.terminated;
            }
            _ => {}
        }
        Ok(())
    }
    pub fn close_open_spans(&mut self, time: f64, terminated: bool) {
        self.duration = self.duration.max(time);
        for (_, begin) in self.pending.drain() {
            if let Some(lane) = self.lanes.iter_mut().find(|lane| lane.id == begin.lane) {
                lane.spans.push(Span {
                    start: begin.time,
                    end: self.duration.max(begin.time),
                    name: begin.name,
                    phase: begin.phase,
                    file: begin.file,
                    line: begin.line,
                    resource: begin.resource,
                    depth: begin.depth,
                    incomplete: true,
                });
            }
        }
        self.terminated |= terminated;
        self.finished = true;
    }
    pub fn pending_spans(&self) -> impl Iterator<Item = &TraceEvent> {
        self.pending.values()
    }
    pub fn from_json(text: &str) -> Result<Self, String> {
        if text.len() > 64 * 1024 * 1024 {
            return Err("Concurrency report exceeds 64 MiB".into());
        }
        let mut report: Self = serde_json::from_str(text).map_err(|error| error.to_string())?;
        if !report.duration.is_finite()
            || report.duration < 0.0
            || report.lanes.len() > 1024
            || report.lanes.iter().any(|lane| {
                lane.spans.iter().any(|span| {
                    !span.start.is_finite()
                        || !span.end.is_finite()
                        || span.start < 0.0
                        || span.end < span.start
                })
            })
        {
            return Err("Invalid concurrency report intervals".into());
        }
        let mut identities = std::collections::HashSet::new();
        let mut spans = 0usize;
        for lane in &report.lanes {
            if lane.id.is_empty() || !identities.insert(&lane.id) {
                return Err("Duplicate or empty concurrency lane".into());
            }
            spans = spans.saturating_add(lane.spans.len());
            for span in &lane.spans {
                report.duration = report.duration.max(span.end);
            }
        }
        if spans > 1_000_000 {
            return Err("Too many concurrency spans".into());
        }
        Ok(report)
    }
}
/// Decode recorded protocol fixtures without starting an interpreter.
pub fn load_trace(text: &str) -> Result<ConcurrencyReport, String> {
    let mut report = ConcurrencyReport::default();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        report.apply(
            serde_json::from_str(line)
                .map_err(|error| format!("Invalid concurrency event: {error}"))?,
        )?;
    }
    if report.finished {
        report.close_open_spans(report.duration, report.terminated);
    }
    Ok(report)
}
pub struct ConcurrencyRun {
    pub id: u64,
    pub name: String,
    pub command_line: String,
    process: Streamed,
    runner: PathBuf,
    trace: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    report: ConcurrencyReport,
    finished: bool,
    error: Option<String>,
    exit_code: Option<i32>,
}
pub fn run_with_concurrency(
    id: u64,
    config: &RunConfig,
    interpreter: Option<&Interpreter>,
    root: &Path,
    options: &CaptureOptions,
) -> anyhow::Result<ConcurrencyRun> {
    let command = PythonCommand::for_config(config, interpreter, root, "Concurrency Diagram")?;
    let directory = root.join(".rustpy/concurrency");
    std::fs::create_dir_all(&directory)?;
    // Captures are private transient artifacts, including when a run is killed
    // before normal Rust ownership can remove them.
    if let Ok(mut ignore) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(".gitignore"))
    {
        use std::io::Write;
        let _ = ignore.write_all(b"*\n");
    }
    static NEXT_CAPTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let capture = NEXT_CAPTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stem = format!("{}-{capture}-{id}", std::process::id());
    let runner = directory.join(format!("runner-{stem}.py"));
    let trace = directory.join(format!("trace-{stem}.jsonl"));
    std::fs::write(&runner, RUNNER_SOURCE)?;
    let mut options = options.clone();
    options.max_events = options.max_events.clamp(1000, 1_000_000);
    options.max_lanes = options.max_lanes.clamp(1, 1024);
    if options.roots.is_empty() {
        options.roots.push(root.to_path_buf());
    }
    let inserted = vec![
        runner.display().to_string(),
        "--trace".into(),
        trace.display().to_string(),
        "--options".into(),
        serde_json::to_string(&options)?,
        "--".into(),
    ];
    let mut args = command.interpreter_options.clone();
    args.extend(inserted.iter().cloned());
    args.extend(command.target.iter().cloned());
    let process = match Streamed::spawn_with_environment(
        &command.python,
        &args,
        &command.cwd,
        &command.env,
        command.stdin(),
        command.inherit_environment,
    ) {
        Ok(process) => process,
        Err(error) => {
            let _ = std::fs::remove_file(&runner);
            return Err(error);
        }
    };
    Ok(ConcurrencyRun {
        id,
        name: config.name.clone(),
        command_line: command.command_line(&inserted),
        process,
        runner,
        trace,
        offset: 0,
        partial: Vec::new(),
        report: ConcurrencyReport::default(),
        finished: false,
        error: None,
        exit_code: None,
    })
}
impl ConcurrencyRun {
    pub fn poll(&mut self) -> Vec<(String, bool)> {
        let mut output = self.process.poll_lines();
        if let Err(error) = self.read_trace() {
            if self.error.is_none() {
                output.push((error.clone(), true));
                self.error = Some(error);
            }
        }
        if !self.finished && self.process.exit().is_some() {
            let drained = std::fs::metadata(&self.trace)
                .map(|metadata| metadata.len() <= self.offset)
                .unwrap_or(true);
            if drained {
                self.finished = true;
                self.exit_code = self.process.exit().and_then(|exit| exit.code());
                let terminated =
                    self.report.terminated || !self.report.finished || self.exit_code.is_none();
                self.report
                    .close_open_spans(self.process.elapsed().as_secs_f64(), terminated);
                if self.report.lanes.is_empty() && self.error.is_none() {
                    self.error = Some("The program ended without concurrency events".into());
                }
                let _ = std::fs::remove_file(&self.runner);
            }
        }
        output
    }
    fn read_trace(&mut self) -> Result<(), String> {
        let mut file = match std::fs::File::open(&self.trace) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        file.seek(SeekFrom::Start(self.offset))
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        file.take(2 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        self.offset += bytes.len() as u64;
        self.partial.extend(bytes);
        let consumed = self
            .partial
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let records: Vec<u8> = self.partial.drain(..consumed).collect();
        for line in records
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            if self.report.event_count >= 1_000_100 {
                self.report.dropped += 1;
                continue;
            }
            let event = serde_json::from_slice(line)
                .map_err(|error| format!("Invalid concurrency event: {error}"))?;
            self.report.apply(event)?;
        }
        if self.partial.len() > 1024 * 1024 {
            self.partial.clear();
            return Err("Concurrency event exceeds 1 MiB".into());
        }
        Ok(())
    }
    pub fn lifecycle_stamp(&self) -> String {
        format!("{}:{}", self.id, self.process.lifecycle_stamp())
    }
    pub fn report(&self) -> &ConcurrencyReport {
        &self.report
    }
    pub fn is_running(&self) -> bool {
        !self.finished
    }
    pub fn stop(&mut self) {
        self.process.stop();
    }
    pub fn interrupt(&mut self) {
        self.process.interrupt();
    }
    pub fn elapsed(&self) -> Duration {
        self.process.elapsed()
    }
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}
impl Drop for ConcurrencyRun {
    fn drop(&mut self) {
        if !self.finished {
            self.process.stop();
        }
        let _ = std::fs::remove_file(&self.runner);
        let _ = std::fs::remove_file(&self.trace);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_preserves_thread_task_source_lock_and_cancelled_wait() {
        let report =
            load_trace(include_str!("../tests/fixtures/concurrency/timeline.jsonl")).unwrap();
        assert_eq!(report.lanes.len(), 3);
        assert!(report.finished && report.terminated);
        assert_eq!(report.dropped, 12);
        let worker = &report.lanes[1];
        let lock = worker
            .spans
            .iter()
            .find(|span| span.phase == Phase::LockWait)
            .unwrap();
        assert_eq!(lock.resource.as_deref(), Some("lock:42"));
        assert_eq!(lock.file.as_deref(), Some(Path::new("/project/main.py")));
        assert_eq!(lock.line, 13);
        assert!((lock.duration() - 0.027).abs() < 1e-9);
        let task = &report.lanes[2];
        assert_eq!(task.parent.as_deref(), Some("thread:1"));
        let cancelled = task
            .spans
            .iter()
            .find(|span| span.name == "cancelled")
            .unwrap();
        assert!(cancelled.incomplete);
        assert_eq!(cancelled.end, 0.05);
        assert_eq!(report.pending_spans().count(), 0);
        let imported =
            ConcurrencyReport::from_json(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(imported.lanes[2].spans.len(), task.spans.len());
        assert!(imported.terminated);
    }
    #[test]
    fn forced_shutdown_closes_pending_spans_without_reversing_time() {
        let mut report=load_trace("{\"kind\":\"lane\",\"lane\":\"t\"}\n{\"kind\":\"begin\",\"lane\":\"t\",\"span\":1,\"time\":2.0}").unwrap();
        assert_eq!(report.pending_spans().count(), 1);
        report.close_open_spans(1.0, true);
        assert_eq!(report.lanes[0].spans[0].start, 2.0);
        assert_eq!(report.lanes[0].spans[0].end, 2.0);
        assert!(report.terminated);
    }
    #[test]
    fn imports_reject_ambiguous_lanes_and_invalid_intervals() {
        assert!(ConcurrencyReport::from_json(r#"{"lanes":[{"id":"t","name":"a","kind":"thread","parent":null,"spans":[]},{"id":"t","name":"b","kind":"thread","parent":null,"spans":[]}],"duration":0,"event_count":0,"dropped":0,"finished":true,"terminated":false}"#).is_err());
        assert!(load_trace("{\"kind\":\"lane\",\"time\":-1,\"lane\":\"t\"}").is_err());
        let mut report =
            load_trace(include_str!("../tests/fixtures/concurrency/timeline.jsonl")).unwrap();
        report.lanes[1].spans[0].end = -1.0;
        assert!(ConcurrencyReport::from_json(&serde_json::to_string(&report).unwrap()).is_err());
    }
}
