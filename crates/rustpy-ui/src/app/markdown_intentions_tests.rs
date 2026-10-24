//! Real prose analysis, Alt+Enter, rendered split/ordinary editor and pointer fix.
use super::*;

fn fixture(extension: &str) -> (tempfile::TempDir, IdeApp, egui::Context, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(format!("prose.{extension}"));
    std::fs::write(&path, "the the answer").unwrap();
    let mut settings = Settings::default();
    settings.inspections.spelling = false;
    settings.completion.show_as_you_type = false;
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_file(&path);
    let i = app.active.unwrap();
    app.last_edit_time = 0.0;
    app.sync_analysis(1.0);
    assert!(app.tabs[i]
        .state
        .extra_diagnostics
        .iter()
        .any(|d| d.code == "Grammar"));
    let offset = app.tabs[i].state.quick_fixes[0].0.start();
    app.tabs[i]
        .state
        .set_carets(vec![crate::editor::Caret::at(offset)]);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]).drop_without_applying_deltas();
    frame(&mut app, &ctx, 0.02, vec![]).drop_without_applying_deltas();
    (root, app, ctx, path)
}
fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
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
}
fn open(app: &mut IdeApp, ctx: &egui::Context, time: f64) -> (String, egui::FullOutput) {
    frame(
        app,
        ctx,
        time,
        vec![
            egui::Event::ModifiersChanged(egui::Modifiers::ALT),
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: Some(egui::Key::Enter),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::ALT,
            },
        ],
    )
    .drop_without_applying_deltas();
    let title = app
        .intentions
        .as_ref()
        .expect("actual Alt+Enter offers grammar fixes")
        .0[0]
        .title
        .clone();
    frame(
        app,
        ctx,
        time + 0.02,
        vec![
            egui::Event::ModifiersChanged(egui::Modifiers::NONE),
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: Some(egui::Key::Enter),
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    )
    .drop_without_applying_deltas();
    (title, frame(app, ctx, time + 0.04, vec![]))
}
fn painted(output: &egui::FullOutput, title: &str) -> Option<Rect> {
    output.shapes.iter().find_map(|s| match &s.shape {
        egui::Shape::Text(t) if t.galley.text() == title => {
            Some(Rect::from_min_size(t.pos, t.galley.size()))
        }
        _ => None,
    })
}
fn click(app: &mut IdeApp, ctx: &egui::Context, rect: Rect, time: f64) {
    for (pressed, at) in [(true, time), (false, time + 0.02)] {
        frame(
            app,
            ctx,
            at,
            vec![
                egui::Event::PointerMoved(rect.center()),
                egui::Event::PointerButton {
                    pos: rect.center(),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .drop_without_applying_deltas();
    }
}
#[test]
fn actual_markdown_split_and_plain_text_paint_and_apply_grammar_quickfix() {
    for extension in ["md", "txt"] {
        let (_root, mut app, ctx, path) = fixture(extension);
        let i = app.active.unwrap();
        if extension == "md" {
            assert_eq!(app.markdown_mode, MarkdownMode::Split);
        }
        let (title, out) = open(&mut app, &ctx, 0.04);
        let rect =
            painted(&out, &title).expect("actual prose editor must paint its Alt+Enter quickfix");
        if extension == "md" {
            assert!(
                out.shapes
                    .iter()
                    .any(|s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.text()=="Split")),
                "real split preview stays rendered"
            );
        }
        out.drop_without_applying_deltas();
        click(&mut app, &ctx, rect, 0.10);
        assert_eq!(app.tabs[i].state.doc.text(), "the answer");
        assert!(app.tabs[i].state.undo());
        assert_eq!(app.tabs[i].state.doc.text(), "the the answer");
        assert!(!app.tabs[i].state.doc.can_undo());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "the the answer");
    }
}
#[test]
fn actual_prose_popup_drops_stale_caret_and_readonly_fix_preserves_source() {
    let (_root, mut app, ctx, path) = fixture("md");
    let i = app.active.unwrap();
    let (title, out) = open(&mut app, &ctx, 0.04);
    assert!(painted(&out, &title).is_some());
    out.drop_without_applying_deltas();
    app.tabs[i].state.go_to_line(1, Some(14));
    let out = frame(&mut app, &ctx, 0.10, vec![]);
    assert!(app.intentions.is_none());
    assert!(painted(&out, &title).is_none());
    out.drop_without_applying_deltas();
    let offset = app.tabs[i].state.quick_fixes[0].0.start();
    app.tabs[i]
        .state
        .set_carets(vec![crate::editor::Caret::at(offset)]);
    app.tabs[i].state.doc.set_read_only(true);
    let (title, out) = open(&mut app, &ctx, 0.12);
    let rect = painted(&out, &title).expect("read-only prose still offers grammar fix");
    out.drop_without_applying_deltas();
    click(&mut app, &ctx, rect, 0.18);
    assert_eq!(app.tabs[i].state.doc.text(), "the the answer");
    assert!(!app.tabs[i].state.doc.can_undo());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "the the answer");
}
