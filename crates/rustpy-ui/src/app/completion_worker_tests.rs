//! Off-UI-thread completion: requests return at once, results arrive later,
//! typing meanwhile refilters them and moving away cancels them.
use super::*;
use std::time::Instant;

fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
    ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        },
    )
    .drop_without_applying_deltas();
}

/// A Python editor whose completion provider takes `delay` and whose
/// requests do not wait for it.
fn slow_python(source: &str, delay: Duration) -> (tempfile::TempDir, IdeApp, egui::Context) {
    slow_document(&[("views.py", source)], delay)
}

/// Project files (the last one opened, caret at its end) with a slow provider.
fn slow_document(
    files: &[(&str, &str)],
    delay: Duration,
) -> (tempfile::TempDir, IdeApp, egui::Context) {
    let root = tempfile::tempdir().unwrap();
    for (name, source) in files {
        let file = root.path().join(name);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, source).unwrap();
    }
    let path = root.path().join(files.last().unwrap().0);
    let mut settings = Settings::default();
    settings.web_development.enabled = false;
    settings.completion.auto_popup_delay_ms = 0;
    settings.editor.auto_close_brackets = false;
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
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.02, vec![]);
    app.completion_worker.grace = Duration::ZERO;
    app.completion_worker.delay = delay;
    (root, app, ctx)
}

fn settle(app: &mut IdeApp) {
    let until = Instant::now() + Duration::from_secs(60);
    while app.poll_completion_worker() {
        assert!(
            Instant::now() < until,
            "background completion never finished"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn labels(app: &IdeApp) -> Vec<String> {
    app.completion
        .items
        .iter()
        .map(|item| item.label.to_string())
        .collect()
}

const SOURCE: &str = "def handler():\n    pass\n\n\ndef hammer():\n    pass\n\n\nha";

#[test]
fn a_slow_provider_returns_control_at_once_and_its_result_arrives_later() {
    let (_root, mut app, _ctx) = slow_python(SOURCE, Duration::from_millis(600));
    let started = Instant::now();
    app.open_completion(true);
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "the request blocked the UI for {:?}",
        started.elapsed()
    );
    assert!(app.completion_worker.pending());
    assert!(
        app.completion.visible && app.completion.items.is_empty() && app.completion_loading(),
        "the lookup shows only its loading row"
    );
    settle(&mut app);
    assert!(app.completion.visible && app.completion.explicit);
    let found = labels(&app);
    assert!(found.contains(&"handler".to_string()), "{found:?}");
    assert!(found.contains(&"hammer".to_string()), "{found:?}");
    // The result serves the same caret again without another request.
    app.open_completion(true);
    assert!(!app.completion_worker.pending());
}

#[test]
fn characters_typed_during_computation_filter_its_result_and_enter_commits_it() {
    let (_root, mut app, ctx) = slow_python(SOURCE, Duration::from_millis(400));
    app.open_completion(false);
    assert!(app.completion_worker.pending());
    // Typing extends the identifier: the request keeps running for it.
    frame(&mut app, &ctx, 0.04, vec![egui::Event::Text("n".into())]);
    frame(&mut app, &ctx, 0.06, vec![]);
    assert!(app.completion_worker.pending());
    settle(&mut app);
    let index = app.active.unwrap();
    assert_eq!(app.completion.caret, app.tabs[index].state.primary().head());
    let found = labels(&app);
    assert!(found.contains(&"handler".to_string()), "{found:?}");
    assert!(
        !found.contains(&"hammer".to_string()),
        "'han' must filter the late result: {found:?}"
    );
    app.completion.selected = found.iter().position(|label| label == "handler").unwrap();
    frame(
        &mut app,
        &ctx,
        0.1,
        vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert!(
        app.tabs[index].state.doc.text().ends_with("\nhandler()"),
        "{}",
        app.tabs[index].state.doc.text()
    );
}

#[test]
fn leaving_the_identifier_cancels_the_request() {
    let (_root, mut app, ctx) = slow_python(SOURCE, Duration::from_millis(400));
    app.open_completion(false);
    assert!(app.completion_worker.pending());
    frame(&mut app, &ctx, 0.04, vec![egui::Event::Text("(".into())]);
    assert!(!app.poll_completion_worker(), "a stale request must stop");
    assert!(!app.completion.visible);
    // Escape cancels a request still running for the caret.
    let (_root, mut app, ctx) = slow_python(SOURCE, Duration::from_millis(400));
    app.open_completion(true);
    assert!(app.completion_worker.pending());
    frame(
        &mut app,
        &ctx,
        0.04,
        vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert!(!app.completion.visible);
    assert!(
        !app.poll_completion_worker(),
        "closing the lookup cancels its request"
    );
}

#[test]
fn identifier_extension_rules() {
    use super::extends;
    assert!(extends("x = ha", 4, 6, "x = han", 7));
    assert!(extends("x = ha", 4, 6, "x = ha", 6));
    assert!(
        !extends("x = ha", 4, 6, "x = h", 5),
        "deleting leaves the candidates incomplete"
    );
    assert!(
        !extends("x = ha", 4, 6, "y = ha", 6),
        "text before the identifier changed"
    );
    assert!(!extends("x = ha", 4, 6, "x = ha(", 7));
}

fn key(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

#[test]
fn commit_keys_do_not_commit_from_the_loading_row() {
    for commit in [egui::Key::Enter, egui::Key::Tab] {
        let (_root, mut app, ctx) = slow_python(SOURCE, Duration::from_millis(400));
        app.open_completion(true);
        assert!(app.completion_loading());
        frame(&mut app, &ctx, 0.04, vec![key(commit)]);
        let text = app.tabs[app.active.unwrap()].state.doc.text();
        assert!(
            !text.contains("\nhandler") && !text.contains("\nhammer"),
            "{commit:?}: {text}"
        );
        app.completion_worker.cancel();
    }
}

#[test]
fn smart_completion_runs_in_the_background() {
    let source = "def number() -> int:\n    return 1\n\n\ndef label() -> str:\n    return ''\n\n\ncount: int = num";
    let (_root, mut app, _ctx) = slow_python(source, Duration::from_millis(500));
    let started = Instant::now();
    app.open_smart_completion();
    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(app.completion_loading());
    settle(&mut app);
    assert!(app.completion.visible && app.completion.explicit);
    assert!(
        labels(&app).contains(&"number".to_string()),
        "{:?}",
        labels(&app)
    );
}

#[test]
fn template_members_arrive_from_the_background() {
    let (_root, mut app, _ctx) = slow_document(
        &[
            (
                "views.py",
                "from django.shortcuts import render\n\n\nclass Customer:\n    def __init__(self):\n        self.email = ''\n\n\ndef view(request):\n    return render(request, 'shop/detail.html', {'customer': Customer()})\n",
            ),
            ("templates/shop/detail.html", "<p>{{ customer.em"),
        ],
        Duration::from_millis(400),
    );
    let started = Instant::now();
    app.open_completion(true);
    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(app.completion_loading(), "{:?}", labels(&app));
    settle(&mut app);
    assert!(
        labels(&app).contains(&"email".to_string()),
        "{:?}",
        labels(&app)
    );
}

#[test]
fn chameleon_render_context_names_arrive_from_the_background() {
    let (_root, mut app, _ctx) = slow_document(
        &[
            (
                "views.py",
                "from flask import render_template\n\n\ndef view():\n    return render_template('page.pt', headline='x')\n",
            ),
            ("page.pt", "<p>${head"),
        ],
        Duration::from_millis(400),
    );
    let started = Instant::now();
    app.open_completion(true);
    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(app.completion_loading(), "{:?}", labels(&app));
    settle(&mut app);
    assert!(
        labels(&app).contains(&"headline".to_string()),
        "{:?}",
        labels(&app)
    );
}

#[test]
fn doctest_prompt_completion_runs_in_the_background() {
    let source = "def helper():\n    return 1\n\n\ndef documented():\n    \"\"\"Examples.\n    >>> hel\n    \"\"\"\n";
    let (_root, mut app, _ctx) = slow_python(source, Duration::from_millis(400));
    let index = app.active.unwrap();
    let caret = TextSize::new((source.find(">>> hel").unwrap() + 7) as u32);
    app.tabs[index]
        .state
        .set_carets(vec![crate::editor::Caret::at(caret)]);
    let started = Instant::now();
    app.open_completion(true);
    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(app.completion_loading(), "{:?}", labels(&app));
    settle(&mut app);
    assert!(
        labels(&app).contains(&"helper".to_string()),
        "{:?}",
        labels(&app)
    );
}
