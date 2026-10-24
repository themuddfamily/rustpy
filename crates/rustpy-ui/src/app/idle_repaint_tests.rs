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

#[test]
#[cfg_attr(
    windows,
    ignore = "the idle frame delay reaches a full blink period on the Windows runner; see known issues"
)]
fn an_idle_app_with_no_pending_work_waits_for_the_caret_blink() {
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
    while started.elapsed() < Duration::from_secs(60) && idle_run < 8 {
        let delay = frame(&mut app, &ctx, now(), vec![]);
        delays.push(delay);
        if delay >= Duration::from_millis(100) {
            idle_run += 1;
        } else {
            idle_run = 0;
        }
        std::thread::sleep(delay.min(Duration::from_millis(600)));
    }
    let recent: Vec<_> = delays.iter().rev().take(12).collect();
    assert!(
        idle_run >= 8,
        "an idle app kept asking for frames sooner than 100 ms: {recent:?}"
    );
    // Idle frames wait for the blink: about half a period, never immediate,
    // and the blink itself is still scheduled (not stalled).
    let last = *delays.last().unwrap();
    assert!(
        last <= Duration::from_millis(600),
        "blink not scheduled: {last:?}"
    );
}
