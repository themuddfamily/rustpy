//! Actual EditorView syntax rendering and real keyboard repair/Undo.
use super::super::{Caret, Language};
use super::*;
struct Frame {
    shapes: Vec<egui::epaint::ClippedShape>,
}
fn frame(
    ctx: &egui::Context,
    state: &mut EditorState,
    view: &mut EditorView,
    time: f64,
    events: Vec<egui::Event>,
) -> Frame {
    let settings = EditorSettings {
        smooth_scrolling: false,
        caret_blinking: false,
        ..Default::default()
    };
    let opts = EditOptions::from_settings(&settings);
    let theme = UiTheme::dark();
    let scheme = ColorScheme::dark();
    let keymap = Keymap::default_keymap();
    let cfg = ViewConfig {
        scheme: &scheme,
        theme: &theme,
        settings: &settings,
        opts: &opts,
        keymap: &keymap,
        font_size: 14.0,
        search_highlights: &[],
        request_focus: true,
        run_lines: &[],
        line_changes: &[],
        breakpoints: &[],
        exec_line: None,
        inline_values: &[],
        bookmarks: &[],
        gutter_markers: &[],
        gutter_quick_fixes: &[],
        code_vision: None,
        annotations: None,
        coverage: &[],
        keyboard: true,
    };
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 400.0))),
            ..Default::default()
        },
        |ui| {
            let _ = show(ui, state, view, &cfg);
        },
    );
    let result = Frame {
        shapes: output.shapes.clone(),
    };
    output.drop_without_applying_deltas();
    result
}
fn label(frame: &Frame, expected: &str) -> bool {
    frame
        .shapes
        .iter()
        .any(|s| matches!(&s.shape, egui::Shape::Text(text) if text.galley.text() == expected))
}
fn error_wave(frame: &Frame) -> bool {
    let color = rgb(ColorScheme::dark().editor.error_underline);
    frame.shapes.iter().any(|s| match &s.shape {
        egui::Shape::Path(path) => {
            matches!(&path.stroke.color, egui::epaint::ColorMode::Solid(c) if *c == color)
                && path.points.len() > 2
                && !path.closed
                && path.points.windows(2).any(|pair| pair[0].y != pair[1].y)
                && s.clip_rect.intersects(path.visual_bounding_rect())
        }
        _ => false,
    })
}
#[test]
fn actual_unicode_json_error_summary_and_underline_follow_keyboard_repair_and_undo() {
    let original = "{\n  \"π\": @\n}\n";
    let valid = "{\n  \"π\": 1\n}\n";
    let mut state = EditorState::from_text(original, Language::Json);
    let identity = state.doc.identity();
    let original_version = state.doc.version();
    let at = TextSize::new(original.find('@').unwrap() as u32);
    state.carets = vec![Caret::new(at, at + TextSize::new(1))];
    let original_carets = state.carets.clone();
    let mut view = EditorView::new(egui::Id::new("data-syntax-view-test"));
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&ctx, &mut state, &mut view, 0.0, vec![]);
    let invalid = frame(&ctx, &mut state, &mut view, 0.016, vec![]);
    assert!(
        ctx.memory(|m| m.has_focus(view.id)),
        "real EditorView response acquired keyboard focus"
    );
    assert!(
        label(&invalid, "1 errors · 0 warnings · 0 other"),
        "actual inspection summary counts the JSON error"
    );
    assert!(
        error_wave(&invalid),
        "actual source error underline is painted within its clip"
    );
    assert_eq!(state.doc.text(), original);
    assert_eq!(state.doc.identity(), identity);
    assert_eq!(state.doc.version(), original_version);
    assert_eq!(state.carets, original_carets);
    assert!(!state.doc.can_undo());
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.032,
        vec![egui::Event::Text("1".into())],
    );
    let repaired_version = state.doc.version();
    let repaired = frame(&ctx, &mut state, &mut view, 0.048, vec![]);
    assert_eq!(
        state.doc.text(),
        valid,
        "actual focused EditorView Text replaced the selected invalid token"
    );
    assert!(state.doc.can_undo());
    assert!(label(&repaired, "No problems found"));
    assert!(!error_wave(&repaired));
    assert_eq!(
        state.doc.version(),
        repaired_version,
        "painting diagnostics creates no extra edit"
    );
    let ctrl = egui::Modifiers {
        ctrl: true,
        ..Default::default()
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.064,
        vec![
            egui::Event::ModifiersChanged(ctrl),
            egui::Event::Key {
                key: egui::Key::Z,
                physical_key: Some(egui::Key::Z),
                pressed: true,
                repeat: false,
                modifiers: ctrl,
            },
            egui::Event::Key {
                key: egui::Key::Z,
                physical_key: Some(egui::Key::Z),
                pressed: false,
                repeat: false,
                modifiers: ctrl,
            },
        ],
    );
    let undo_version = state.doc.version();
    let restored = frame(
        &ctx,
        &mut state,
        &mut view,
        0.080,
        vec![egui::Event::ModifiersChanged(egui::Modifiers::NONE)],
    );
    assert_eq!(
        state.doc.text(),
        original,
        "actual default CtrlZ restored the original Unicode source"
    );
    assert_eq!(state.doc.identity(), identity);
    assert_eq!(state.doc.version(), undo_version);
    assert!(
        !state.doc.can_undo(),
        "syntax rendering has added no history transaction"
    );
    assert!(state.doc.can_redo());
    assert!(label(&restored, "1 errors · 0 warnings · 0 other"));
    assert!(error_wave(&restored));
}
