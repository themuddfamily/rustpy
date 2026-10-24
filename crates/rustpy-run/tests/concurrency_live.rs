//! Explicit real collector acceptance; no replayed/seeded events or timing thresholds.
use rustpy_project::RunConfig;
use rustpy_run::concurrency::{run_with_concurrency, CaptureOptions, ConcurrencyRun, Phase};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
const TIMEOUT: Duration = Duration::from_secs(45);
const SOURCE: &str = r#"import asyncio, os, pathlib, threading, time
lock = threading.Lock()
ready = threading.Event()
def worker():
    ready.set()
    with lock:
        print('WORKER_ACQUIRED', flush=True)
async def consumer(gate):
    async with gate:
        await asyncio.sleep(0.02)
async def cancelled_worker():
    await asyncio.sleep(120)
async def tasks():
    gate = asyncio.Lock()
    first = asyncio.create_task(consumer(gate), name='producer-native')
    second = asyncio.create_task(consumer(gate), name='consumer-native')
    cancelled = asyncio.create_task(cancelled_worker(), name='cancelled-native')
    await asyncio.gather(first, second)
    cancelled.cancel()
    try:
        await cancelled
    except asyncio.CancelledError:
        print('TASK_CANCELLED', flush=True)
with lock:
    thread = threading.Thread(target=worker, name='blocked-worker-native')
    thread.start()
    assert ready.wait(5)
    time.sleep(0.02)
thread.join()
asyncio.run(tasks())
print('COLLECTOR_PID ' + str(os.getpid()), flush=True)
print('COLLECTOR_READY', flush=True)
while not pathlib.Path('release').exists():
    time.sleep(0.01)
print('COLLECTOR_RELEASED', flush=True)
"#;
struct Fixture {
    run: ConcurrencyRun,
    directory: tempfile::TempDir,
    source: PathBuf,
    output: Vec<(String, bool)>,
    owner: Option<(u32, String)>,
}
impl Fixture {
    fn start(id: u64) -> Self {
        assert_eq!(
            std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").as_deref(),
            Ok("1"),
            "explicitly admit real Python collector execution"
        );
        assert!(
            std::fs::read_to_string("/proc/self/stat").is_ok(),
            "this physical process identity acceptance requires readable Linux /proc"
        );
        let python = PathBuf::from(
            std::env::var_os("RUSTPY_PROVIDER_PYTHON")
                .expect("set absolute admitted Python interpreter"),
        );
        assert!(python.is_absolute() && python.is_file());
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("live_concurrency.py");
        std::fs::write(&source, SOURCE).unwrap();
        let mut config = RunConfig::for_script(&source);
        config.interpreter = Some(python);
        let run = run_with_concurrency(
            id,
            &config,
            None,
            directory.path(),
            &CaptureOptions::default(),
        )
        .expect("public owned concurrency collector starts");
        Self {
            run,
            directory,
            source,
            output: Vec::new(),
            owner: None,
        }
    }
    fn poll(&mut self) {
        self.output.extend(self.run.poll());
        if self.owner.is_none() {
            if let Some(pid) = self.output.iter().find_map(|(line, error)| {
                (!error)
                    .then(|| line.strip_prefix("COLLECTOR_PID "))
                    .flatten()
                    .and_then(|text| text.parse::<u32>().ok())
            }) {
                self.owner = Some((
                    pid,
                    start_ticks(pid).expect("live captured collector identity"),
                ));
            }
        }
    }
    fn wait(&mut self, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !done(self) {
            self.poll();
            assert!(
                Instant::now() < deadline,
                "collector timeout, owner={:?}, error={:?}, output={:?}",
                self.owner,
                self.run.error(),
                self.output
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn ready(&mut self) {
        self.wait(|this| {
            this.output
                .iter()
                .any(|(line, error)| !error && line == "COLLECTOR_READY")
        });
        assert!(
            self.run.is_running(),
            "live publication must precede physical process completion"
        );
        assert!(self
            .owner
            .as_ref()
            .is_some_and(|(pid, start)| start_ticks(*pid).as_ref() == Some(start)));
    }
    fn assert_retired(&self) {
        assert!(!self.run.is_running());
        let (pid, start) = self.owner.as_ref().expect("captured real process");
        assert_ne!(
            start_ticks(*pid).as_ref(),
            Some(start),
            "same collector process identity still exists"
        );
        assert_eq!(std::fs::read_to_string(&self.source).unwrap(), SOURCE);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.run.is_running() {
            self.run.stop();
            let deadline = Instant::now() + TIMEOUT;
            while self.run.is_running() && Instant::now() < deadline {
                self.run.poll();
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
fn start_ticks(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(
        stat.rsplit_once(") ")?
            .1
            .split_whitespace()
            .nth(19)?
            .to_owned(),
    )
}
fn assert_live_timeline(fixture: &Fixture) {
    let report = fixture.run.report();
    assert!(report.event_count > 0 && !report.finished);
    assert_eq!(report.dropped, 0);
    let worker = report
        .lanes
        .iter()
        .find(|lane| lane.name == "blocked-worker-native")
        .expect("real named worker lane");
    assert_eq!(worker.kind, "thread");
    assert!(worker
        .spans
        .iter()
        .any(|span| span.phase == Phase::LockWait && span.resource.is_some()));
    for name in ["producer-native", "consumer-native", "cancelled-native"] {
        let task = report
            .lanes
            .iter()
            .find(|lane| lane.name == name)
            .expect("real named asyncio lane");
        assert_eq!(task.kind, "task");
        assert!(task.parent.as_ref().is_some_and(|parent| report
            .lanes
            .iter()
            .any(|lane| &lane.id == parent && lane.kind == "thread")));
        assert!(task.spans.iter().any(|span| span.phase == Phase::Await));
    }
    assert!(report
        .lanes
        .iter()
        .flat_map(|lane| &lane.spans)
        .any(|span| span.phase == Phase::LockWait && span.name == "Lock.acquire"));
    for span in report.lanes.iter().flat_map(|lane| &lane.spans) {
        assert!(span.start.is_finite() && span.end.is_finite() && span.end >= span.start);
        assert!(span.line > 0 && span.line <= SOURCE.lines().count());
        assert_eq!(span.file.as_ref(), Some(&fixture.source));
    }
    assert!(report
        .lanes
        .iter()
        .flat_map(|lane| &lane.spans)
        .any(|span| span.phase == Phase::Execution));
    assert!(fixture
        .output
        .iter()
        .any(|(line, error)| !error && line == "TASK_CANCELLED"));
}
#[test]
#[ignore = "requires explicit admitted Python and Linux process identity acceptance"]
fn configured_python_collector_publishes_real_thread_task_lock_and_await_timeline() {
    let mut fixture = Fixture::start(72001);
    fixture.ready();
    assert_live_timeline(&fixture);
    std::fs::write(
        fixture.directory.path().join("release"),
        "release owned collector",
    )
    .unwrap();
    fixture.wait(|this| !this.run.is_running());
    assert_eq!(fixture.run.exit_code(), Some(0), "{:?}", fixture.output);
    assert!(fixture.run.error().is_none(), "{:?}", fixture.run.error());
    assert!(fixture.run.report().finished && !fixture.run.report().terminated);
    assert_eq!(fixture.run.report().pending_spans().count(), 0);
    let traces: Vec<_> = std::fs::read_dir(fixture.directory.path().join(".rustpy/concurrency"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    assert_eq!(traces.len(), 1);
    let raw = std::fs::read_to_string(&traces[0]).unwrap();
    let replay = rustpy_run::concurrency::load_trace(&raw)
        .expect("actual collector artifact remains importable");
    assert_eq!(replay.event_count, fixture.run.report().event_count);
    assert_eq!(replay.lanes.len(), fixture.run.report().lanes.len());
    fixture.assert_retired();
}
#[test]
#[ignore = "requires explicit admitted Python and Linux process identity acceptance"]
fn configured_collector_stop_retires_real_owner_and_closes_live_spans() {
    let mut fixture = Fixture::start(72002);
    fixture.ready();
    assert_live_timeline(&fixture);
    assert!(fixture.run.report().pending_spans().next().is_some());
    fixture.run.stop();
    assert!(
        fixture.run.is_running(),
        "a Stop request alone cannot publish physical completion"
    );
    fixture.wait(|this| !this.run.is_running());
    assert!(fixture.run.report().finished && fixture.run.report().terminated);
    assert_eq!(fixture.run.report().pending_spans().count(), 0);
    assert!(fixture
        .run
        .report()
        .lanes
        .iter()
        .flat_map(|lane| &lane.spans)
        .any(|span| span.incomplete));
    assert!(!fixture
        .output
        .iter()
        .any(|(line, _)| line == "COLLECTOR_RELEASED"));
    fixture.assert_retired();
}
