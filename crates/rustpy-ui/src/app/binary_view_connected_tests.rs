//! Real bounded hex rendering and immutable binary-viewer input/save attempts.
use super::*;

fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> Vec<String> {
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            focused: true,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 720.0),
            )),
            events,
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| app.group_area(ui, 0));
        },
    );
    let painted = output
        .shapes
        .iter()
        .filter_map(|shape| {
            if let egui::Shape::Text(text) = &shape.shape {
                Some(text.galley.text().to_owned())
            } else {
                None
            }
        })
        .collect();
    output.drop_without_applying_deltas();
    painted
}

fn command(key: egui::Key, shift: bool) -> Vec<egui::Event> {
    let modifiers = egui::Modifiers {
        ctrl: true,
        command: true,
        shift,
        ..Default::default()
    };
    vec![
        egui::Event::ModifiersChanged(modifiers),
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        },
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: false,
            repeat: false,
            modifiers,
        },
    ]
}

#[test]
fn actual_binary_hex_ascii_paint_and_text_paste_save_refusals_preserve_authority() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bytes π.bin");
    let original: &[u8] = b"A\x00\x7f\xff z\nbcdefghijK";
    std::fs::write(&path, original).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(directory.path());
    settings.completion.show_as_you_type = false;
    let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
    app.open_file(&path);
    let index = app.active.unwrap();
    let first = "00000000  41 00 7f ff 20 7a 0a 62 63 64 65 66 67 68 69 6a  |A... z.bcdefghij|";
    let second = "00000010  4b                                               |K|";
    let expected = format!("{first}\n{second}\n");
    assert_eq!(app.tabs[index].state.doc.text(), expected);
    assert_eq!(app.tabs[index].state.language, Language::PlainText);
    assert!(app.tabs[index].state.doc.is_read_only());
    assert!(app.tabs[index].image.is_none());
    assert!(app.tabs[index].notebook.is_none());
    assert!(app
        .permission_viewers
        .contains(&app.tabs[index].state.doc.identity()));
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    let painted = frame(&mut app, &ctx, 0.1, vec![]);
    assert!(app.tabs[index].view.focused);
    assert!(
        painted.iter().any(|text| text.contains(first)),
        "actual EditorView paints first offset, hex and ASCII: {painted:?}"
    );
    assert!(
        painted.iter().any(|text| text.contains(second)),
        "actual EditorView paints second-row offset and byte: {painted:?}"
    );
    let identity = app.tabs[index].state.doc.identity();
    let version = app.tabs[index].state.doc.version();
    let carets = app.tabs[index].state.carets.clone();
    assert!(!app.tabs[index].state.doc.can_undo());
    assert!(!app.tabs[index].state.doc.can_redo());
    assert!(!app.tabs[index].state.doc.is_modified());

    for (time, events) in [
        (0.2, vec![egui::Event::Text("MUST NOT TYPE π🙂".into())]),
        (0.3, vec![egui::Event::Paste("MUST NOT PASTE β\n".into())]),
        (0.4, command(egui::Key::S, false)),
    ] {
        frame(&mut app, &ctx, time, events);
        let state = &app.tabs[index].state;
        assert_eq!(state.doc.text(), expected);
        assert_eq!(state.doc.identity(), identity);
        assert_eq!(state.doc.version(), version);
        assert_eq!(state.carets, carets);
        assert_eq!(state.path.as_deref(), Some(path.as_path()));
        assert!(state.doc.is_read_only());
        assert!(!state.doc.is_modified());
        assert!(!state.doc.can_undo());
        assert!(!state.doc.can_redo());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(
            matches!(app.dialog, Dialog::None),
            "a permanent viewer cannot offer Make Writable"
        );
    }
    assert!(
        app.status.as_ref().is_some_and(
            |(text, _)| text.contains("is read-only") && text.contains("nothing to save")
        ),
        "actual Save action reaches the read-only refusal"
    );
}
