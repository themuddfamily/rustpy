//! Typing latency benchmark (roadmap §2.2 "Performance"): characters typed
//! into a 5,000-line Python file through complete IDE frames, timing each
//! frame on the UI thread, and the cancellation latency of the background
//! analysis that an edit makes obsolete.
//!
//! The bounds are generous: this crate's tests build without optimisation.
//! The release targets are a p95 under 16 ms per keystroke frame (with
//! highlighting) and a cancellation p95 under 100 ms.
use super::*;
use std::time::{Duration, Instant};

/// A 5,000-line module of classes, methods, loops and comprehensions.
pub(super) fn five_thousand_lines() -> String {
    let mut out = String::from("import os\nimport sys\nfrom collections import defaultdict\n\n\n");
    let mut n = 0;
    while out.lines().count() < 5000 {
        out.push_str(&format!(
            "class Model{n}:\n    \"\"\"Model number {n}.\"\"\"\n\n    def __init__(self, value: int, name: str = \"m{n}\") -> None:\n        self.value = value\n        self.name = name\n        self.items = defaultdict(list)\n\n    def compute(self, factor: int) -> int:\n        total = 0\n        for index in range(factor):\n            if index % 3 == 0:\n                total += index * self.value\n            else:\n                total -= len(self.name)\n        squares = [x * x for x in range(total % 7)]\n        return total + sum(squares)\n\n    def describe(self) -> str:\n        path = os.path.join(\"root\", self.name)\n        return f\"{{self.name}}: {{path}} {{sys.version}}\"\n\n\ndef helper_{n}(model: Model{n}) -> int:\n    result = model.compute({n} % 11 + 1)\n    while result > 100:\n        result //= 2\n    return result\n\n\n"
        ));
        n += 1;
    }
    out
}

fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
    let mut native = eframe::Frame::_new_kittest();
    ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300.0, 850.0))),
            ..Default::default()
        },
        |ui| eframe::App::ui(app, ui, &mut native),
    )
    .drop_without_applying_deltas();
}

fn percentile(samples: &mut [Duration], p: f64) -> Duration {
    samples.sort_unstable();
    samples[((samples.len() - 1) as f64 * p).round() as usize]
}

#[test]
fn typing_into_a_five_thousand_line_file_keeps_ui_frames_short() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("big.py");
    let source = five_thousand_lines();
    std::fs::write(&path, &source).unwrap();
    let mut settings = Settings::default();
    settings.web_development.enabled = false;
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&path);
    let index = app.active.unwrap();
    // Type in the middle of the file, at the end of a method body line.
    let line = 2500;
    let at = app.tabs[index].state.doc.line_range(line).end();
    app.tabs[index]
        .state
        .set_carets(vec![crate::editor::Caret::at(at)]);
    app.focus_editor = true;
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut time = 0.0;
    // Warm-up: first layout, the initial analysis of the whole file.
    for _ in 0..20 {
        frame(&mut app, &ctx, time, vec![]);
        time += 0.5;
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    while app.bg.analysis.has_job() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        frame(&mut app, &ctx, time, vec![]);
        time += 0.5;
    }
    let enter = || egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: Some(egui::Key::Enter),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    // Types `keys` (one per frame, 40 ms apart, so the analysis debounce
    // never elapses), pausing after every fifth long enough for the
    // background analysis to start, so the next keystroke has to cancel it.
    let typing = |app: &mut IdeApp, time: &mut f64, keys: Vec<egui::Event>| {
        let mut samples = Vec::new();
        for (i, key) in keys.into_iter().enumerate() {
            let started = Instant::now();
            frame(app, &ctx, *time, vec![key]);
            samples.push(started.elapsed());
            *time += 0.04;
            if i % 5 == 4 {
                *time += 0.5;
                frame(app, &ctx, *time, vec![]);
                *time += 0.01;
            }
        }
        samples
    };
    let text = |s: &str| {
        s.chars()
            .map(|c| egui::Event::Text(c.to_string()))
            .collect::<Vec<_>>()
    };
    // Production's wait for a fast lookup (tests otherwise wait for every result).
    app.completion_worker.grace = Duration::from_millis(30);
    let reparses_before = app.tabs[index].state.blocking_reparses;
    // Comment lines: the editor's own work (edit, highlighting, analysis
    // bookkeeping), no completion lookup.
    let mut keys = vec![enter()];
    keys.extend(text("# typed while measuring the"));
    keys.push(enter());
    keys.extend(text("# frame time of every key"));
    keys.push(enter());
    let samples = typing(&mut app, &mut time, keys);
    // A statement in code: the completion lookup opens and follows the typing.
    let mut code_keys = text("total = len(self.name) + index");
    code_keys.push(enter());
    let code = typing(&mut app, &mut time, code_keys);
    let blocking_reparses = app.tabs[index].state.blocking_reparses - reparses_before;
    // For comparison: frames without input (the rest of the IDE's frame cost).
    let mut idle = Vec::new();
    for _ in 0..10 {
        time += 0.04;
        let started = Instant::now();
        frame(&mut app, &ctx, time, vec![]);
        idle.push(started.elapsed());
    }
    let idle_p50 = percentile(&mut idle, 0.5);
    let text = app.tabs[index].state.doc.text();
    assert!(
        text.contains("# typed while measuring the") && text.contains("total = len(self.name)"),
        "the keystrokes reached the editor"
    );
    // The analysis database catches up once the background pushes land.
    let version = app.tabs[index].state.doc.version();
    let deadline = Instant::now() + Duration::from_secs(60);
    while app.tabs[index].state.synced_version != Some(version) {
        assert!(
            Instant::now() < deadline,
            "the editor text never reached the analysis"
        );
        std::thread::sleep(Duration::from_millis(5));
        time += 0.04;
        frame(&mut app, &ctx, time, vec![]);
    }
    let analysis = app.project.as_ref().unwrap().analysis.clone();
    assert_eq!(&*analysis.file_for_path(&path).unwrap().text, text.as_str());
    let mut sorted = samples.clone();
    let p50 = percentile(&mut sorted, 0.5);
    let p95 = percentile(&mut sorted, 0.95);
    let mut code_sorted = code.clone();
    let code_p50 = percentile(&mut code_sorted, 0.5);
    let code_p95 = percentile(&mut code_sorted, 0.95);
    eprintln!(
        "typing latency in a {}-line file: comments ({} keys) p50 {p50:?}, p95 {p95:?}; \
         code with the completion lookup ({} keys) p50 {code_p50:?}, p95 {code_p95:?}; \
         idle frame p50 {idle_p50:?}",
        app.tabs[index].state.doc.line_count(),
        samples.len(),
        code.len(),
    );
    // Release target: p95 < 16 ms. This crate's tests build unoptimised (an
    // idle IDE frame alone takes about 6 ms; keystrokes measured p50 13 ms,
    // p95 16 ms in comments and 26 ms in code there), so the bounds leave
    // headroom for load.
    assert!(
        p95 < Duration::from_millis(60),
        "keystroke frame p95 {p95:?} (p50 {p50:?})"
    );
    assert!(
        code_p95 < Duration::from_millis(60),
        "keystroke frame in code p95 {code_p95:?} (p50 {code_p50:?})"
    );
    // The per-frame consumers (view, breadcrumbs, completion checks, run
    // markers) wait for no reparse of the large file.
    assert_eq!(
        blocking_reparses, 0,
        "the UI thread reparsed the file while typing"
    );
}

#[test]
fn an_edit_cancels_the_obsolete_analysis_of_a_five_thousand_line_file_promptly() {
    let db = Arc::new(Analysis::for_project(Path::new("/typing-latency")));
    let path = PathBuf::from("/typing-latency/big.py");
    db.set_file_text(&path, &five_thousand_lines());
    let file = db.file_for_path(&path).unwrap();
    let inputs = AnalysisInputs {
        inspections: InspectionOptions::default(),
        disabled_inspections: Vec::new(),
        grammar: false,
        grammar_policy: None,
        dictionary: None,
        parameter_hints: true,
        type_hints: true,
        index: Default::default(),
    };
    let mut latencies = Vec::new();
    for delay_ms in [5u64, 20, 40, 80, 120, 200, 300, 450, 600, 800] {
        let token = CancellationToken::new();
        let (db, file, inputs, worker_token) =
            (db.clone(), file.clone(), inputs.clone(), token.clone());
        let worker = std::thread::spawn(move || {
            let result = analyze_file(&db, &file, &inputs, &worker_token);
            (result.is_err(), Instant::now())
        });
        std::thread::sleep(Duration::from_millis(delay_ms));
        let cancelled_at = Instant::now();
        token.cancel();
        let (stopped, ended) = worker.join().unwrap();
        if stopped {
            latencies.push(ended.saturating_duration_since(cancelled_at));
        }
    }
    assert!(
        !latencies.is_empty(),
        "every run finished before it was cancelled"
    );
    let p95 = percentile(&mut latencies, 0.95);
    eprintln!(
        "analysis cancellation over {} runs: p95 {p95:?}, max {:?}",
        latencies.len(),
        latencies.last().unwrap()
    );
    // Release target: p95 < 100 ms.
    assert!(p95 < Duration::from_millis(250), "cancellation p95 {p95:?}");
}

#[test]
fn background_text_pushes_are_coalesced_and_never_overwrite_newer_text() {
    let db = Arc::new(Analysis::for_project(Path::new("/text-sync")));
    let path = PathBuf::from("/text-sync/big.py");
    let document = rustpy_text::Document::new("").identity();
    let text = |db: &Analysis| db.file_for_path(&path).unwrap().text.to_string();
    let mut sync = TextSync::default();
    assert!(sync.push_in_background(&db, &path, document, 1, || "a = 1\n".into()));
    // One push at a time: the next frame pushes the newest text.
    assert!(!sync.push_in_background(&db, &path, document, 2, || "a = 2\n".into()));
    let deadline = Instant::now() + Duration::from_secs(30);
    let landed = loop {
        if let Some(landed) = sync.poll() {
            break landed;
        }
        assert!(Instant::now() < deadline, "the push never landed");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(landed, (path.clone(), document, 1));
    assert_eq!(text(&db), "a = 1\n");
    // A synchronous push waits for the one in flight: the newest text wins.
    assert!(sync.push_in_background(&db, &path, document, 2, || "a = 2\n".into()));
    sync.push_now(&db, &path, "a = 3\n");
    assert_eq!(text(&db), "a = 3\n");
    assert!(
        sync.poll().is_none(),
        "the finished push was collected by the synchronous one"
    );
}

#[test]
fn typing_in_a_large_file_gets_completions_from_the_worker() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("big.py");
    std::fs::write(&path, five_thousand_lines()).unwrap();
    let mut settings = Settings::default();
    settings.web_development.enabled = false;
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&path);
    let index = app.active.unwrap();
    let end = app.tabs[index].state.doc.len();
    app.tabs[index]
        .state
        .set_carets(vec![crate::editor::Caret::at(end)]);
    app.focus_editor = true;
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut time = 0.0;
    for _ in 0..3 {
        frame(&mut app, &ctx, time, vec![]);
        time += 0.5;
    }
    app.completion_worker.grace = Duration::ZERO;
    let reparses = app.tabs[index].state.blocking_reparses;
    for c in "helper_12".chars() {
        frame(&mut app, &ctx, time, vec![egui::Event::Text(c.to_string())]);
        time += 0.04;
    }
    assert!(
        app.completion.visible,
        "the lookup opens (with its loading row while the worker computes)"
    );
    let deadline = Instant::now() + Duration::from_secs(120);
    while app.completion_worker.pending() {
        assert!(Instant::now() < deadline, "the worker never finished");
        std::thread::sleep(Duration::from_millis(10));
        frame(&mut app, &ctx, time, vec![]);
        time += 0.04;
    }
    let labels: Vec<String> = app
        .completion
        .items
        .iter()
        .map(|item| item.label.to_string())
        .collect();
    assert!(
        labels.iter().any(|label| label == "helper_12"),
        "{labels:?}"
    );
    assert_eq!(
        app.tabs[index].state.blocking_reparses, reparses,
        "typing did not reparse the file on the UI thread"
    );
}
