use super::*;
fn fixture(text: &str, language: Language) -> (tempfile::TempDir, IdeApp, egui::Context) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = IdeApp::new(Settings::default(), None);
    app.push_tab(EditorState::from_text(text, language));
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    (dir, app, ctx)
}
fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> Vec<(String, egui::Rect)> {
    let mut labels = Vec::new();
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000., 720.),
            )),
            events,
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            app.emmet_preview_ui(ui.ctx());
        },
    );
    fn text(shape: &egui::epaint::Shape, labels: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::epaint::Shape::Text(text) => labels.push((
                text.galley.text().into(),
                egui::Rect::from_min_size(text.pos, text.galley.size()),
            )),
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    text(shape, labels)
                }
            }
            _ => {}
        }
    }
    for shape in &output.shapes {
        text(&shape.shape, &mut labels);
    }
    output.drop_without_applying_deltas();
    labels
}
fn key(key: egui::Key) -> Vec<egui::Event> {
    vec![
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}
#[test]
fn actual_surround_preview_paints_unicode_and_apply_is_one_undo_cancel_preserves_selection() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_CONFIGURATION.with(|value| *value.borrow_mut() = None);
        }
    }
    let _reset = Reset;
    let source = "π <b>😀</b>";
    let (dir, mut app, ctx) = fixture(source, Language::Html);
    let index = app.active.unwrap();
    app.tabs[index]
        .state
        .set_carets(vec![crate::editor::Caret::new(
            TextSize::new(0),
            TextSize::new(source.len() as u32),
        )]);
    let before = app.tabs[index].state.carets.clone();
    TEST_CONFIGURATION.with(|value| *value.borrow_mut() = Some(dir.path().join("snippets.json")));
    app.handle_action(Action::SurroundWithEmmet, &ctx);
    app.emmet_ui.preview.as_mut().unwrap().abbreviation = "section>p".into();
    frame(&mut app, &ctx, 0., vec![]);
    let labels = frame(&mut app, &ctx, 0.01, vec![]);
    assert!(labels.iter().any(|(text, _)| text.contains(source)));
    assert!(labels.iter().any(|(text, _)| text == "Apply"));
    frame(&mut app, &ctx, 0.1, key(egui::Key::Escape));
    assert!(!app.emmet_ui.active());
    assert_eq!(app.tabs[index].state.doc.text(), source);
    assert_eq!(app.tabs[index].state.carets, before);
    TEST_CONFIGURATION.with(|value| *value.borrow_mut() = Some(dir.path().join("snippets.json")));
    app.handle_action(Action::SurroundWithEmmet, &ctx);
    app.emmet_ui.preview.as_mut().unwrap().abbreviation = "section>p".into();
    frame(&mut app, &ctx, 0.2, vec![]);
    frame(&mut app, &ctx, 0.3, key(egui::Key::Enter));
    let expected = "<section>\n    <p>π <b>😀</b></p>\n</section>";
    assert_eq!(app.tabs[index].state.doc.text(), expected);
    assert!(!app.emmet_ui.active());
    app.tabs[index].state.undo();
    assert_eq!(app.tabs[index].state.doc.text(), source);
    assert_eq!(app.tabs[index].state.carets, before);
    assert!(!app.tabs[index].state.doc.can_undo());
    app.tabs[index].state.redo();
    assert_eq!(app.tabs[index].state.doc.text(), expected);
}
#[test]
fn stale_and_readonly_confirmation_never_publish_and_snippets_persist_checked() {
    let (dir, mut app, ctx) = fixture("ul>li", Language::Html);
    let index = app.active.unwrap();
    let end = app.tabs[index].state.doc.len();
    app.tabs[index]
        .state
        .set_carets(vec![crate::editor::Caret::at(end)]);
    let path = dir.path().join("snippets.json");
    let snippets = Snippets {
        html: BTreeMap::from([("card".into(), "div.card>p".into())]),
        css: BTreeMap::from([("space".into(), "m10".into())]),
    };
    save(&path, &snippets).unwrap();
    assert_eq!(load(&path).unwrap().html, snippets.html);
    app.open_emmet_at(false, path.clone());
    app.tabs[index].state.apply_text_edits(
        vec![(TextRange::empty(TextSize::new(0)), "changed ".into())],
        "Edit",
    );
    let changed = app.tabs[index].state.doc.text();
    frame(&mut app, &ctx, 0., vec![]);
    frame(&mut app, &ctx, 0.1, key(egui::Key::Enter));
    assert_eq!(app.tabs[index].state.doc.text(), changed);
    assert!(app.emmet_ui.active());
    frame(&mut app, &ctx, 0.2, key(egui::Key::Escape));
    app.open_emmet_at(false, path.clone());
    app.tabs[index].state.doc.set_read_only(true);
    frame(&mut app, &ctx, 0.3, vec![]);
    frame(&mut app, &ctx, 0.4, key(egui::Key::Enter));
    assert_eq!(app.tabs[index].state.doc.text(), changed);
    let bytes = std::fs::read(&path).unwrap();
    let invalid = Snippets {
        html: BTreeMap::from([("".into(), "p".into())]),
        css: BTreeMap::new(),
    };
    assert!(save(&path, &invalid).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn actual_preview_action_and_tab_use_persisted_custom_snippet() {
    let (dir, mut app, ctx) = fixture("card", Language::Html);
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_CONFIGURATION.with(|value| *value.borrow_mut() = None);
        }
    }
    let _reset = Reset;
    let path = dir.path().join("snippets.json");
    TEST_CONFIGURATION.with(|value| *value.borrow_mut() = Some(path.clone()));
    let mut snippets = Snippets::default();
    snippets.html.insert("card".into(), "section>p{π😀}".into());
    save(&path, &snippets).unwrap();
    let index = app.active.unwrap();
    app.tabs[index]
        .state
        .set_carets(vec![crate::editor::Caret::at(TextSize::new(4))]);
    app.handle_action(Action::EmmetPreview, &ctx);
    assert!(!app.editor_keyboard());
    frame(&mut app, &ctx, 0., vec![]);
    let labels = frame(&mut app, &ctx, 0.01, vec![]);
    assert!(labels.iter().any(|(text, _)| text.contains("π😀")));
    frame(&mut app, &ctx, 0.1, key(egui::Key::Enter));
    let expected = "<section>\n    <p>π😀</p>\n</section>";
    assert_eq!(app.tabs[index].state.doc.text(), expected);
    app.tabs[index].state.undo();
    assert_eq!(app.tabs[index].state.doc.text(), "card");
    assert!(app.try_expand_emmet());
    assert_eq!(app.tabs[index].state.doc.text(), expected);
}

#[test]
fn same_version_path_language_column_and_virtual_retarget_refuse_preview() {
    for kind in 0..4 {
        let (dir, mut app, _) = fixture("div", Language::Html);
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(3))]);
        app.open_emmet_at(false, dir.path().join("snippets.json"));
        let preview = app.emmet_ui.preview.take().unwrap();
        let state = &mut app.tabs[index].state;
        match kind {
            0 => state.path = Some(dir.path().join("changed.xml")),
            1 => state.language = Language::Css,
            2 => state.column_mode = true,
            _ => {
                state.set_virtual_space_enabled(true);
                state.set_virtual_carets(vec![crate::editor::state::virtual_space::VirtualCaret {
                    anchor: rustpy_core::Position::new(0, 8),
                    head: rustpy_core::Position::new(0, 8),
                }]);
            }
        }
        assert_eq!(state.doc.version(), preview.version);
        assert!(app.apply_emmet_preview(&preview).is_err());
        assert_eq!(app.tabs[index].state.doc.text(), "div");
        assert!(!app.tabs[index].state.undo());
    }
}
