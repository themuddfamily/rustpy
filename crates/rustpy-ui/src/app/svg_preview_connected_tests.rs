//! Actual App/editor routes; Root registers this module after installing SVG bridges.
use super::*;
use crate::editor::svg_preview::{SvgMode, SvgStamp};

fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
) -> Vec<(String, egui::Rect)> {
    let output = ctx.run_ui(
        egui::RawInput {
            focused: true,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
            )),
            events,
            ..Default::default()
        },
        |ui| {
            app.poll_svg_previews(ui.ctx());
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| {
                app.editor_area(ui);
            });
        },
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| {
            if let egui::epaint::Shape::Text(text) = &shape.shape {
                Some((
                    text.galley.text().to_owned(),
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                ))
            } else {
                None
            }
        })
        .collect();
    output.drop_without_applying_deltas();
    labels
}
fn click(app: &mut IdeApp, ctx: &egui::Context, label: &str) {
    let labels = frame(app, ctx, vec![]);
    let point = labels
        .iter()
        .find(|(text, _)| text == label)
        .expect("actual rendered SVG selector")
        .1
        .center();
    for pressed in [true, false] {
        frame(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}
fn wait_preview(app: &mut IdeApp, ctx: &egui::Context, index: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !app.tabs[index].svg.as_ref().unwrap().has_texture()
        && std::time::Instant::now() < deadline
    {
        frame(app, ctx, vec![]);
        std::thread::yield_now();
    }
    assert!(
        app.tabs[index].svg.as_ref().unwrap().has_texture(),
        "actual managed SVG worker published to current App view"
    );
}
fn fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    IdeApp,
    egui::Context,
    String,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drawing.svg");
    let source = "<svg xmlns='http://www.w3.org/2000/svg' width='32' height='16'><rect width='32' height='16' fill='red'/><!--π--></svg>".to_owned();
    std::fs::write(&path, &source).unwrap();
    let mut settings = Settings::default();
    settings.editor.ensure_newline_at_eof = false;
    settings.editor.strip_trailing_spaces_on_save = rustpy_settings::StripTrailing::None;
    let mut app = IdeApp::new(settings, None);
    app.open_file(&path);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, vec![]);
    (dir, path, app, ctx, source)
}
#[test]
fn actual_svg_selectors_preserve_document_and_source_edit_undo_save() {
    let (_dir, path, mut app, ctx, source) = fixture();
    let identity = app.tabs[0].state.doc.identity();
    let selection = app.tabs[0].state.carets.clone();
    click(&mut app, &ctx, "Preview");
    assert_eq!(app.tabs[0].svg.as_ref().unwrap().mode, SvgMode::Preview);
    wait_preview(&mut app, &ctx, 0);
    assert!(!app.editor_keyboard());
    frame(
        &mut app,
        &ctx,
        vec![egui::Event::Paste("MUST NOT EDIT".into())],
    );
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert_eq!(app.tabs[0].state.carets, selection);
    click(&mut app, &ctx, "Source");
    frame(&mut app, &ctx, vec![egui::Event::Paste("<!--β-->".into())]);
    assert_eq!(app.tabs[0].state.doc.text(), format!("<!--β-->{source}"));
    assert_eq!(app.tabs[0].state.doc.identity(), identity);
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert!(app.tabs[0].state.redo());
    app.handle_action(Action::Save, &ctx);
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        format!("<!--β-->{source}")
    );
}
#[test]
fn actual_svg_split_has_independent_modes_shared_source_and_readonly_preview() {
    let (_dir, path, mut app, ctx, source) = fixture();
    app.split_shared_editor(SplitDir::Horizontal);
    assert!(app.tabs[0]
        .state
        .doc
        .shares_authority(&app.tabs[1].state.doc));
    click(&mut app, &ctx, "Preview");
    assert_eq!(app.tabs[0].svg.as_ref().unwrap().mode, SvgMode::Preview);
    assert_eq!(app.tabs[1].svg.as_ref().unwrap().mode, SvgMode::Source);
    app.tabs[1].state.doc.set_read_only(true);
    app.active = Some(1);
    app.focus_editor = true;
    frame(&mut app, &ctx, vec![egui::Event::Paste("rejected".into())]);
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert!(!app.tabs[1].state.doc.can_undo());
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}
#[test]
fn actual_svg_pane_refuses_old_worker_result_after_real_editor_edit() {
    let (_dir, _path, mut app, ctx, source) = fixture();
    let tab = &mut app.tabs[0];
    let stamp = SvgStamp {
        view: tab.view.id,
        document: tab.state.doc.identity(),
        version: tab.state.doc.version(),
    };
    let pane = tab.svg.as_mut().unwrap();
    let request = pane.request(source.clone());
    tab.state
        .insert_text("<!--changed-->", &EditOptions::default());
    let current = SvgStamp {
        version: tab.state.doc.version(),
        ..stamp
    };
    let pane = tab.svg.as_mut().unwrap();
    pane.refresh(current);
    assert!(!pane.accept(
        &ctx,
        current,
        request,
        Err(crate::editor::svg_preview::SvgError::Allocation)
    ));
    assert!(tab.state.undo());
    assert_eq!(tab.state.doc.text(), source);
}

// Added regressions use existing fixture/frame/click; no previous SVG oracle changes.
struct PreviewFocusFixture {
    dir: tempfile::TempDir,
    svg_path: std::path::PathBuf,
    python_path: std::path::PathBuf,
    app: IdeApp,
    ctx: egui::Context,
    svg_source: String,
    python_source: String,
    preview: usize,
    python: usize,
}
fn cloned_preview_and_passive_python() -> PreviewFocusFixture {
    let (dir, svg_path, mut app, ctx, svg_source) = fixture();
    click(&mut app, &ctx, "Preview");
    app.split_shared_editor(SplitDir::Horizontal);
    let preview = app.active.unwrap();
    assert_eq!(
        app.tabs[preview].svg.as_ref().unwrap().mode,
        SvgMode::Preview
    );
    let python_path = dir.path().join("plain.py");
    let python_source = "plain_value = 'π🙂'\n".to_owned();
    std::fs::write(&python_path, &python_source).unwrap();
    app.open_file(&python_path);
    let python = app.open_document_in_group(app.active.unwrap(), 0).unwrap();
    // Normal existing-instance activation queues focus for the cloned Preview.
    let active = app.open_document_in_group(preview, 1).unwrap();
    assert_eq!(active, preview);
    assert!(app.focus_editor);
    PreviewFocusFixture {
        dir,
        svg_path,
        python_path,
        app,
        ctx,
        svg_source,
        python_source,
        preview,
        python,
    }
}

#[test]
fn cloned_svg_preview_retires_owned_focus_and_passive_python_click_activates_exact_view() {
    let PreviewFocusFixture {
        dir: _dir,
        svg_path,
        python_path,
        mut app,
        ctx,
        svg_source,
        python_source,
        preview,
        python,
    } = cloned_preview_and_passive_python();
    let documents: Vec<_> = app
        .tabs
        .iter()
        .map(|tab| {
            (
                tab.state.doc.identity(),
                tab.state.doc.text(),
                tab.state.doc.can_undo(),
                tab.state.doc.can_redo(),
            )
        })
        .collect();
    let labels = frame(&mut app, &ctx, vec![]);
    assert_eq!(app.active, Some(preview));
    assert!(
        !app.focus_editor,
        "owned Preview paint must retire its queued text focus"
    );
    let point = labels
        .iter()
        .find(|(label, _)| label.contains("plain_value"))
        .expect("actual Python code galley in passive group")
        .1
        .center();
    let python_id = app.tabs[python].view.id;
    for pressed in [true, false] {
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert_eq!(app.active, Some(python));
    assert_eq!(app.tabs[app.active.unwrap()].view.id, python_id);
    assert!(app.tabs[python].view.focused);
    assert_eq!(ctx.memory(|memory| memory.focused()), Some(python_id));
    assert_eq!(
        app.tabs
            .iter()
            .map(|tab| (
                tab.state.doc.identity(),
                tab.state.doc.text(),
                tab.state.doc.can_undo(),
                tab.state.doc.can_redo()
            ))
            .collect::<Vec<_>>(),
        documents
    );
    assert_eq!(std::fs::read_to_string(svg_path).unwrap(), svg_source);
    assert_eq!(std::fs::read_to_string(python_path).unwrap(), python_source);
}

#[test]
fn cloned_svg_preview_does_not_retire_focus_intent_while_actual_settings_owns_input() {
    let PreviewFocusFixture {
        dir: _dir,
        svg_path,
        python_path,
        mut app,
        ctx,
        svg_source,
        python_source,
        preview,
        python: _python,
    } = cloned_preview_and_passive_python();
    app.handle_action(Action::Settings, &ctx);
    assert!(matches!(app.dialog, Dialog::Settings));
    assert!(app.focus_editor);
    let output = ctx.run_ui(
        egui::RawInput {
            focused: true,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
            )),
            ..Default::default()
        },
        |ui| {
            app.poll_svg_previews(ui.ctx());
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| {
                app.editor_area(ui);
            });
            app.dialogs(ui.ctx());
        },
    );
    output.drop_without_applying_deltas();
    assert!(matches!(app.dialog, Dialog::Settings));
    assert_eq!(app.active, Some(preview));
    assert!(
        app.focus_editor,
        "foreign Settings owner must preserve deferred editor focus intent"
    );
    assert!(app.tabs.iter().all(|tab| !tab.state.doc.can_undo()));
    assert_eq!(std::fs::read_to_string(svg_path).unwrap(), svg_source);
    assert_eq!(std::fs::read_to_string(python_path).unwrap(), python_source);
}
