//! Active-tab navigation routes must use the view's measured page size.
use super::*;
use crate::editor::Caret;

fn app_with_two_tabs() -> IdeApp {
    let mut app = IdeApp::new(Settings::default(), None);
    for _ in 0..2 {
        let mut state = EditorState::from_text(&"αβ界 suffix\n".repeat(100), Language::Python);
        let offset = state.doc.line_range(20).start() + TextSize::new("αβ界".len() as u32);
        state.set_carets(vec![Caret::at(offset)]);
        app.push_tab(state);
    }
    app.active = Some(0);
    app.tabs[0].view.page_lines = 4;
    app.tabs[1].view.page_lines = 9;
    app
}
fn assert_primary(app: &IdeApp, line: usize) {
    let tab = &app.tabs[0];
    let expected = tab.state.doc.line_range(line).start() + TextSize::new("αβ界".len() as u32);
    assert_eq!(
        tab.state.primary().head(),
        expected,
        "page={}, caret={:?}",
        tab.view.page_lines,
        tab.state.primary()
    );
    assert_eq!(tab.state.primary().goal_column, Some(3));
    assert!(!tab.state.doc.can_undo());
}

#[test]
fn app_center_requests_only_active_view_and_preserves_editor_state() {
    let mut app = app_with_two_tabs();
    let before: Vec<_> = app.tabs.iter().map(|t| t.state.selections()).collect();
    app.handle_action(Action::ScrollToCenter, &egui::Context::default());
    assert!(app.tabs[0]
        .view
        .center_request
        .take_offset(100.0, 20.0, 100.0, 1000.0)
        .is_some());
    assert_eq!(
        app.tabs[1]
            .view
            .center_request
            .take_offset(100.0, 20.0, 100.0, 1000.0),
        None
    );
    for (tab, selection) in app.tabs.iter().zip(before) {
        assert_eq!(tab.state.selections(), selection);
        assert_eq!(tab.state.doc.text(), "αβ界 suffix\n".repeat(100));
        assert!(!tab.state.doc.can_undo());
    }
}

#[test]
fn direct_finder_and_macro_page_routes_share_active_view_metric() {
    let ctx = egui::Context::default();
    let mut app = app_with_two_tabs();
    app.tabs[0].state.doc.set_read_only(true);
    let other = app.tabs[1].state.primary();
    assert!(app.dispatch_editor_action(Action::CaretPageDown));
    assert_primary(&app, 24);
    assert!(app.dispatch_editor_action(Action::CaretPageUp));
    assert_primary(&app, 20);
    app.handle_nav_event(NavEvent::RunAction(Action::CaretPageDown), &ctx);
    assert_primary(&app, 24);
    app.play_macro(vec!["action:EditorPageUp".into()], &ctx);
    assert_primary(&app, 20);
    app.play_macro(vec!["action:EditorPageDown".into()], &ctx);
    assert_primary(&app, 24);
    assert_eq!(app.tabs[1].state.primary(), other);
    assert_eq!(app.tabs[0].state.doc.text(), "αβ界 suffix\n".repeat(100));
}

#[test]
fn navigation_routes_with_no_active_editor_are_safe() {
    let ctx = egui::Context::default();
    let mut app = IdeApp::new(Settings::default(), None);
    app.handle_action(Action::ScrollToCenter, &ctx);
    assert!(!app.dispatch_editor_action(Action::CaretPageDown));
    app.handle_nav_event(NavEvent::RunAction(Action::CaretPageUp), &ctx);
    app.play_macro(vec!["action:EditorPageDown".into()], &ctx);
    assert!(app.tabs.is_empty());
    assert!(app.active.is_none());
}
