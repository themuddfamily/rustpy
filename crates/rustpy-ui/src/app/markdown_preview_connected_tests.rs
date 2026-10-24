//! Live Markdown rendering and actual painted mode controls through the App.
use super::*;

fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
    action: Option<Action>,
) -> Vec<(String, egui::Rect)> {
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            focused: true,
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            if let Some(action) = action {
                let handled = app.dispatch_editor_action(action);
                if !handled {
                    app.handle_action(action, ui.ctx());
                }
            }
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        },
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| {
            if let egui::Shape::Text(text) = &shape.shape {
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

fn has(labels: &[(String, egui::Rect)], text: &str) -> bool {
    labels.iter().any(|(label, _)| label == text)
}

fn click_selector(app: &mut IdeApp, ctx: &egui::Context, time: &mut f64, label: &str) {
    *time += 0.1;
    let labels = frame(app, ctx, *time, vec![], None);
    // Preview appears twice: the actual selector is rightmost, not the heading.
    let point = labels
        .iter()
        .filter(|(text, _)| text == label)
        .max_by(|(_, left), (_, right)| left.center().x.total_cmp(&right.center().x))
        .expect("actual painted Markdown selector")
        .1
        .center();
    for pressed in [true, false] {
        *time += 0.1;
        frame(
            app,
            ctx,
            *time,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            None,
        );
    }
}

#[test]
fn actual_markdown_mode_clicks_live_text_and_history_refresh_rendered_preview() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("notes π.md");
    let original = "# π original\n\nbody\n";
    let prefix = "# λ🙂 fresh\n\n";
    let edited = format!("{prefix}{original}");
    std::fs::write(&path, original).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    settings.completion.show_as_you_type = false;
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_file(&path);
    let index = app.active.unwrap();
    assert_eq!(app.tabs[index].state.language, Language::Markdown);
    assert_eq!(app.markdown_mode, MarkdownMode::Split);
    let identity = app.tabs[index].state.doc.identity();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![], None);
    let labels = frame(&mut app, &ctx, 0.1, vec![], None);
    assert!(app.tabs[index].view.focused);
    assert!(
        has(&labels, "π original"),
        "rendered heading omits Markdown syntax"
    );
    assert!(
        labels.iter().any(|(text, _)| text.contains("# π original")),
        "actual source editor also paints in Split"
    );
    assert!(!app.tabs[index].state.doc.can_undo());

    frame(
        &mut app,
        &ctx,
        0.2,
        vec![egui::Event::Text(prefix.into())],
        None,
    );
    assert_eq!(app.tabs[index].state.doc.text(), edited);
    let labels = frame(&mut app, &ctx, 0.3, vec![], None);
    assert!(
        has(&labels, "λ🙂 fresh"),
        "preview refreshes from unsaved EditorView text"
    );
    assert!(has(&labels, "π original"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let version = app.tabs[index].state.doc.version();
    let carets = app.tabs[index].state.carets.clone();
    let mut time = 0.3;
    click_selector(&mut app, &ctx, &mut time, "Preview");
    time += 0.1;
    let labels = frame(&mut app, &ctx, time, vec![], None);
    assert_eq!(app.markdown_mode, MarkdownMode::PreviewOnly);
    assert!(has(&labels, "λ🙂 fresh"));
    assert!(
        !labels.iter().any(|(text, _)| text.contains("# λ🙂 fresh")),
        "Preview-only excludes raw source paint"
    );
    time += 0.1;
    frame(
        &mut app,
        &ctx,
        time,
        vec![egui::Event::Text("MUST NOT EDIT".into())],
        None,
    );
    assert_eq!(app.tabs[index].state.doc.text(), edited);
    assert_eq!(app.tabs[index].state.doc.version(), version);
    assert_eq!(app.tabs[index].state.carets, carets);

    click_selector(&mut app, &ctx, &mut time, "Editor");
    time += 0.1;
    let labels = frame(&mut app, &ctx, time, vec![], None);
    assert_eq!(app.markdown_mode, MarkdownMode::EditorOnly);
    assert!(labels.iter().any(|(text, _)| text.contains("# λ🙂 fresh")));
    assert!(
        !has(&labels, "λ🙂 fresh"),
        "Editor-only excludes parsed heading paint"
    );
    assert_eq!(app.tabs[index].state.doc.version(), version);
    assert_eq!(app.tabs[index].state.carets, carets);
    time += 0.1;
    let labels = frame(
        &mut app,
        &ctx,
        time,
        vec![],
        Some(Action::ToggleMarkdownPreview),
    );
    assert_eq!(app.markdown_mode, MarkdownMode::Split);
    assert!(
        has(&labels, "λ🙂 fresh"),
        "actual layout action restores live Split preview"
    );

    time += 0.1;
    frame(&mut app, &ctx, time, vec![], Some(Action::Undo));
    time += 0.1;
    let labels = frame(&mut app, &ctx, time, vec![], None);
    assert_eq!(app.tabs[index].state.doc.text(), original);
    assert!(has(&labels, "π original"));
    assert!(
        !has(&labels, "λ🙂 fresh"),
        "Undo removes stale preview content"
    );
    assert!(
        !app.tabs[index].state.doc.can_undo(),
        "mode changes add no edit transactions"
    );
    assert!(app.tabs[index].state.doc.can_redo());
    time += 0.1;
    frame(&mut app, &ctx, time, vec![], Some(Action::Redo));
    time += 0.1;
    let labels = frame(&mut app, &ctx, time, vec![], None);
    assert_eq!(app.tabs[index].state.doc.text(), edited);
    assert!(has(&labels, "λ🙂 fresh"));
    assert!(app.tabs[index].state.doc.can_undo());
    assert!(!app.tabs[index].state.doc.can_redo());
    assert_eq!(app.tabs[index].state.doc.identity(), identity);
    assert_eq!(app.tabs[index].state.path.as_deref(), Some(path.as_path()));
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}
