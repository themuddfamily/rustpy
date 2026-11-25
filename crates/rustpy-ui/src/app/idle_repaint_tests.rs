//! An idle IDE must not repaint every frame: once its background work is
//! done, only the caret blink (one frame per half period) wakes it.
use super::*;
use std::time::{Duration, Instant};

/// One full IDE frame at `time`; the delay egui asks the host to wait before
/// the next one (`Duration::MAX` when nothing is scheduled).
fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) -> Duration {
    let mut native = eframe::Frame::_new_kittest();
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300.0, 850.0))),
            ..Default::default()
        },
        |ui| eframe::App::ui(app, ui, &mut native),
    );
    let delay = output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map_or(Duration::MAX, |v| v.repaint_delay);
    output.drop_without_applying_deltas();
    delay
}

const IDLE_CHILD: &str = "RUSTPY_IDLE_REPAINT_CHILD";

/// Background tasks are process-wide (`TaskManager::global`), and the status
/// bar keeps polling while any of them runs, so a task of a test running
/// concurrently kept this app awake for as long as that test ran. The check
/// therefore runs alone in a child process of this test binary, with its own
/// configuration directory: the per-process test directory is inherited
/// through `RUSTPY_CONFIG_DIR`, and the parent's tests kept saving settings
/// there, which this app's watcher (it covers the template store's parent,
/// the configuration directory) reported as a stream of file changes.
#[test]
fn an_idle_app_with_no_pending_work_waits_for_the_caret_blink() {
    if std::env::var_os(IDLE_CHILD).is_some() {
        return idle_app_waits_for_the_caret_blink();
    }
    let config = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::idle_repaint_tests::an_idle_app_with_no_pending_work_waits_for_the_caret_blink",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(IDLE_CHILD, "1")
        .env("RUSTPY_CONFIG_DIR", config.path())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "isolated idle check failed ({}):\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn idle_app_waits_for_the_caret_blink() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("main.py");
    std::fs::write(
        &path,
        "import os\n\n\ndef main():\n    print(os.getcwd())\n",
    )
    .unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_file(&path);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let started = Instant::now();
    let now = || started.elapsed().as_secs_f64();
    // Focus the editor (the caret blinks only there).
    frame(&mut app, &ctx, now(), vec![]);
    let click = |pressed| egui::Event::PointerButton {
        pos: Pos2::new(700.0, 300.0),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(
        &mut app,
        &ctx,
        now(),
        vec![
            egui::Event::PointerMoved(Pos2::new(700.0, 300.0)),
            click(true),
        ],
    );
    frame(&mut app, &ctx, now(), vec![click(false)]);
    // Run frames as a host would, sleeping for the requested delay, until the
    // startup work (analysis, indexes, tool windows) has settled.
    let mut idle_run = 0;
    let mut delays = Vec::new();
    // What asked for each frame (file:line of the repaint request), so a
    // failure names the component that keeps the app awake.
    let mut causes = Vec::new();
    // File-system events the app saw (a watched project that keeps changing
    // keeps the app polling); the latest few are reported on failure.
    let mut watcher_events = std::collections::VecDeque::new();
    while started.elapsed() < Duration::from_secs(60) && idle_run < 8 {
        let delay = frame(&mut app, &ctx, now(), vec![]);
        delays.push(delay);
        causes.push(
            ctx.repaint_causes()
                .iter()
                .map(|cause| format!("{}:{}", cause.file, cause.line))
                .collect::<Vec<_>>()
                .join(", "),
        );
        for (kind, data) in crate::performance_trace::take_recorded() {
            if kind == "watcher_event" {
                watcher_events.push_back(format!("{:.2}s {data}", now()));
                if watcher_events.len() > 12 {
                    watcher_events.pop_front();
                }
            }
        }
        if delay >= Duration::from_millis(100) {
            idle_run += 1;
        } else {
            idle_run = 0;
        }
        std::thread::sleep(delay.min(Duration::from_millis(600)));
    }
    let recent: Vec<_> = delays.iter().rev().take(12).collect();
    let recent_causes: Vec<_> = causes.iter().rev().take(12).collect();
    assert!(
        idle_run >= 8,
        "an idle app kept asking for frames sooner than 100 ms: {recent:?}\n\
         requested by (latest first): {recent_causes:#?}\n\
         latest watcher events: {watcher_events:#?}\n\
         pending deployment work: {:?}",
        app.deploy.pending_transfers()
    );
    // Idle frames wait for the blink: about half a period, never immediate,
    // and the blink itself is still scheduled (not stalled).
    let last = *delays.last().unwrap();
    assert!(
        last <= Duration::from_millis(600),
        "blink not scheduled: {last:?}"
    );
    // An idle app does not rewrite its settings.
    let settings = Settings::settings_path();
    let stamp = || {
        std::fs::metadata(&settings)
            .ok()
            .map(|m| (m.len(), m.modified().ok()))
    };
    let before = stamp();
    let _ = crate::performance_trace::take_recorded();
    for _ in 0..6 {
        let delay = frame(&mut app, &ctx, now(), vec![]);
        std::thread::sleep(delay.min(Duration::from_millis(600)));
    }
    let config = Settings::config_dir();
    let config_events: Vec<_> = crate::performance_trace::take_recorded()
        .into_iter()
        .filter(|(kind, data)| {
            kind == "watcher_event"
                && data["paths"].as_array().is_some_and(|paths| {
                    paths.iter().any(|path| {
                        path.as_str()
                            .is_some_and(|path| Path::new(path).starts_with(&config))
                    })
                })
        })
        .collect();
    assert_eq!(
        stamp(),
        before,
        "an idle app rewrote {}",
        settings.display()
    );
    assert!(
        config_events.is_empty(),
        "an idle app changed its configuration directory: {config_events:?}"
    );
}

/// The freeze notice reports a stall after the fact; it must not trap the
/// keyboard like a modal dialog. A frame on a busy machine that crosses the
/// watchdog threshold used to take focus from the editor (no caret blink,
/// Tab confined to the notice) for the notice's lifetime.
#[test]
fn a_freeze_notice_leaves_the_keyboard_with_the_editor() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("main.py");
    std::fs::write(&path, "print(1)\n").unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_file(&path);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let click = |pressed| egui::Event::PointerButton {
        pos: Pos2::new(700.0, 300.0),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(
        &mut app,
        &ctx,
        0.1,
        vec![
            egui::Event::PointerMoved(Pos2::new(700.0, 300.0)),
            click(true),
        ],
    );
    frame(&mut app, &ctx, 0.2, vec![click(false)]);
    let editor = app.tabs[app.active.unwrap()].view.id;
    assert!(
        ctx.memory(|m| m.has_focus(editor)),
        "the click focuses the editor"
    );
    // Every frame now counts as a freeze: the next frame shows the notice.
    app.bg.freeze = crate::freeze::FreezeDetector::new(Duration::ZERO, Duration::ZERO);
    frame(&mut app, &ctx, 0.3, vec![]);
    frame(&mut app, &ctx, 0.4, vec![]);
    let notice = egui::LayerId::new(egui::Order::Middle, egui::Id::new("freeze_notice"));
    assert!(
        ctx.memory(|m| m.areas().is_visible(&notice)),
        "the freeze notice is shown"
    );
    let delay = frame(&mut app, &ctx, 0.5, vec![]);
    assert_eq!(ctx.memory(|m| m.top_modal_layer()), None);
    assert!(
        ctx.memory(|m| m.has_focus(editor)),
        "the notice leaves the keyboard with the editor"
    );
    assert!(
        delay <= Duration::from_millis(600),
        "the caret keeps blinking under the notice: {delay:?}"
    );
}
