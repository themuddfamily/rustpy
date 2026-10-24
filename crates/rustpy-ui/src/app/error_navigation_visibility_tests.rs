//! Connected diagnostic navigation must reveal the caret beyond the fixed gutter.
use super::*;
use crate::editor::view::{self, ViewConfig};
use crate::editor::Caret;
fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64) -> egui::FullOutput {
    let scheme = ColorScheme::dark();
    let theme = UiTheme::dark();
    let settings = rustpy_settings::EditorSettings {
        smooth_scrolling: false,
        soft_wrap: false,
        ..Default::default()
    };
    let opts = EditOptions::default();
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
    ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(340.0, 300.0))),
            ..Default::default()
        },
        |ui| {
            let i = app.active.unwrap();
            let tab = &mut app.tabs[i];
            view::show(ui, &mut tab.state, &mut tab.view, &cfg);
        },
    )
}
#[test]
fn actual_app_f2_long_line_then_shift_f2_reveals_short_line_caret_past_fixed_gutter() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warnings.py");
    let source = format!("import os\nvalue = '{}'\n", "x".repeat(500));
    std::fs::write(&path, &source).unwrap();
    let mut app = IdeApp::new_welcome(Settings::default());
    app.open_file(&path);
    let i = app.active.unwrap();
    let state = &mut app.tabs[i].state;
    state.semantic_version = Some(state.doc.version());
    let long = state.doc.line_range(1).start() + TextSize::new(79);
    state.extra_diagnostics = vec![
        rustpy_core::Diagnostic::warning(
            TextRange::new(TextSize::new(7), TextSize::new(9)),
            "F401",
            "Unused import statement 'import os'",
        ),
        rustpy_core::Diagnostic::warning(
            TextRange::new(long, long + TextSize::new(1)),
            "E501",
            "Line too long",
        ),
    ];
    state.set_carets(vec![Caret::at(TextSize::new(0))]);
    let version = state.doc.version();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    for n in 0..3 {
        frame(&mut app, &ctx, n as f64 * 0.02).drop_without_applying_deltas();
    }
    app.navigate_highlighted_error(true, false);
    assert_eq!(app.tabs[i].state.primary().head(), TextSize::new(7));
    app.navigate_highlighted_error(true, false);
    assert_eq!(app.tabs[i].state.primary().head(), long);
    for n in 3..7 {
        frame(&mut app, &ctx, n as f64 * 0.02).drop_without_applying_deltas();
    }
    app.navigate_highlighted_error(false, false);
    assert_eq!(app.tabs[i].state.primary().head(), TextSize::new(7));
    for n in 7..10 {
        frame(&mut app, &ctx, n as f64 * 0.02).drop_without_applying_deltas();
    }
    let output = frame(&mut app, &ctx, 0.20);
    let gutter_color = crate::theme::rgb(ColorScheme::dark().editor.gutter_background);
    let gutter = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect)
                if rect.fill == gutter_color
                    && rect.rect.height() > 200.0
                    && rect.rect.width() < 180.0 =>
            {
                Some(rect.rect)
            }
            _ => None,
        })
        .expect("actual fixed editor gutter painted");
    let caret = app.tabs[i]
        .view
        .caret_screen_rect
        .expect("actual primary caret geometry");
    assert!(
        caret.left() >= gutter.right(),
        "F2 return caret {caret:?} must be beyond fixed gutter {gutter:?}"
    );
    assert!(
        caret.right()
            <= output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if rect.rect == gutter => Some(shape.clip_rect.right()),
                    _ => None,
                })
                .unwrap(),
        "caret must remain inside source viewport"
    );
    assert_eq!(app.tabs[i].state.doc.text(), source);
    assert_eq!(app.tabs[i].state.doc.version(), version);
    assert!(!app.tabs[i].state.doc.can_undo());
    output.drop_without_applying_deltas();
}
