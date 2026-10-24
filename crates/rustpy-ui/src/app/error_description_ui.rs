//! Ctrl+F1 description and severity-filtered F2 navigation of current diagnostics.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_core::{Diagnostic, Severity};

#[derive(Default)]
pub(super) struct ErrorDescriptionUi {
    popup: Option<Description>,
}
struct Description {
    view: egui::Id,
    version: u64,
    caret: TextSize,
    diagnostics: Vec<Diagnostic>,
}

fn navigation_diagnostics(state: &mut EditorState, errors_only: bool) -> Vec<Diagnostic> {
    let mut diagnostics = state.diagnostics();
    diagnostics.retain(|d| {
        d.range.end() <= state.doc.len()
            && if errors_only {
                d.severity == Severity::Error
            } else {
                matches!(
                    d.severity,
                    Severity::Error | Severity::Warning | Severity::WeakWarning
                )
            }
    });
    diagnostics.sort_by(|a, b| {
        a.range
            .start()
            .cmp(&b.range.start())
            .then_with(|| b.severity.cmp(&a.severity))
            .then_with(|| a.range.end().cmp(&b.range.end()))
            .then_with(|| a.code.cmp(&b.code))
            .then_with(|| a.message.cmp(&b.message))
    });
    diagnostics
}

fn description_diagnostics(state: &mut EditorState) -> Vec<Diagnostic> {
    let caret = state.primary().head();
    let diagnostics = state.diagnostics();
    let valid: Vec<_> = diagnostics
        .into_iter()
        .filter(|d| d.range.end() <= state.doc.len())
        .collect();
    let mut at_caret: Vec<_> = valid
        .iter()
        .filter(|d| d.range.contains(caret) || d.range.start() == caret)
        .cloned()
        .collect();
    if at_caret.is_empty() {
        let line = state.doc.line_of(caret);
        at_caret = valid
            .into_iter()
            .filter(|d| state.doc.line_of(d.range.start()) == line)
            .collect();
    }
    at_caret.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.range.start().cmp(&b.range.start()))
            .then_with(|| a.range.end().cmp(&b.range.end()))
            .then_with(|| a.code.cmp(&b.code))
            .then_with(|| a.message.cmp(&b.message))
    });
    at_caret
}

impl IdeApp {
    pub(super) fn navigate_highlighted_error(&mut self, next: bool, errors_only: bool) {
        self.error_description.popup = None;
        let message = self
            .active
            .and_then(|i| self.tabs.get_mut(i))
            .and_then(|tab| {
                let head = tab.state.primary().head();
                let diagnostics = navigation_diagnostics(&mut tab.state, errors_only);
                let found = if next {
                    diagnostics
                        .iter()
                        .find(|d| d.range.start() > head)
                        .or_else(|| diagnostics.first())
                } else {
                    diagnostics
                        .iter()
                        .rev()
                        .find(|d| d.range.start() < head)
                        .or_else(|| diagnostics.last())
                }?;
                let start = found.range.start();
                tab.state.reveal_offset(start);
                tab.state
                    .move_primary_to(tab.state.doc.offset_to_position(start));
                Some(found.message.clone())
            });
        self.set_status(message.unwrap_or_else(|| {
            if errors_only {
                "No errors found"
            } else {
                "No errors or warnings found"
            }
            .into()
        }));
        self.focus_editor = true;
    }

    pub(super) fn show_error_description(&mut self) {
        self.error_description.popup = None;
        let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) else {
            return;
        };
        let caret = tab.state.primary().head();
        let at_caret = description_diagnostics(&mut tab.state);
        if at_caret.is_empty() {
            self.set_status("No highlighted problem at the caret");
            return;
        }
        self.error_description.popup = Some(Description {
            view: tab.view.id,
            version: tab.state.doc.version(),
            caret,
            diagnostics: at_caret,
        });
    }

    // Validate before consuming input, not merely before painting. Analysis may
    // publish new findings without changing the document version or caret.
    fn error_description_current(&mut self, ctx: &egui::Context) -> bool {
        let Some(description) = self.error_description.popup.as_ref() else {
            return false;
        };
        let valid = self.editor_keyboard()
            && !self.find.visible
            && !self.find_in_files.visible
            && !self.nav.popup_visible()
            && self.intentions.is_none()
            && self.inplace_rename.is_none()
            && !self.completion.visible
            && self.quick_doc.is_none()
            && !self.coordinated_quit_active()
            && !egui::Popup::is_any_open(ctx)
            && ctx.memory(|memory| memory.focused().is_none_or(|id| id == description.view))
            && self
                .active
                .and_then(|i| self.tabs.get_mut(i))
                .is_some_and(|tab| {
                    tab.view.id == description.view
                        && tab.state.doc.version() == description.version
                        && tab.state.primary().head() == description.caret
                        && description_diagnostics(&mut tab.state) == description.diagnostics
                });
        if !valid {
            self.error_description.popup = None;
        }
        valid
    }

    pub(super) fn error_description_keys(&mut self, ctx: &egui::Context) -> bool {
        if self.error_description_current(ctx)
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.error_description.popup = None;
            self.focus_editor = true;
            return true;
        }
        false
    }

    pub(super) fn error_description_ui(&mut self, ctx: &egui::Context) {
        if self.error_description_keys(ctx) {
            return;
        }
        let Some(description) = self.error_description.popup.as_ref() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Error Description")
            .id(egui::Id::new("error_description"))
            .collapsible(false)
            .resizable(true)
            .default_width(460.0)
            .open(&mut open)
            .show_modeless(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .show(ui, |ui| {
                        for diagnostic in &description.diagnostics {
                            ui.label(format!(
                                "{} · {}",
                                diagnostic.severity.label(),
                                diagnostic.code
                            ));
                            ui.label(&diagnostic.message);
                        }
                    });
            });
        if !open {
            self.error_description.popup = None;
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Caret;
    fn app() -> (IdeApp, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("problems.py");
        std::fs::write(&path, "π = 1\nvalue = 2\nother = 3\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.open_file(&path);
        let state = &mut app.tabs[0].state;
        state.semantic_version = Some(state.doc.version());
        let range = |start, end| TextRange::new(TextSize::new(start), TextSize::new(end));
        state.extra_diagnostics = vec![
            Diagnostic::warning(range(17, 22), "warning", "later warning"),
            Diagnostic::new(range(0, 2), Severity::Info, "info", "information"),
            Diagnostic::error(range(7, 12), "error", "actual error"),
            Diagnostic::new(range(4, 5), Severity::WeakWarning, "weak", "weak warning"),
        ];
        state.set_carets(vec![Caret::at(TextSize::new(0))]);
        (app, root)
    }
    #[test]
    fn actual_app_navigation_sorts_wraps_filters_and_preserves_readonly_text() {
        let (mut app, _root) = app();
        app.tabs[0].state.doc.set_read_only(true);
        let before = app.tabs[0].state.doc.text();
        for expected in [4, 7, 17, 4] {
            app.navigate_highlighted_error(true, false);
            assert_eq!(app.tabs[0].state.primary().head(), TextSize::new(expected));
        }
        app.navigate_highlighted_error(false, false);
        assert_eq!(app.tabs[0].state.primary().head(), TextSize::new(17));
        for next in [true, true, false] {
            app.navigate_highlighted_error(next, true);
            assert_eq!(app.tabs[0].state.primary().head(), TextSize::new(7));
        }
        assert_eq!(app.tabs[0].state.doc.text(), before);
        assert!(!app.tabs[0].state.doc.can_undo());
        app.tabs[0]
            .state
            .extra_diagnostics
            .retain(|d| d.severity != Severity::Error);
        app.navigate_highlighted_error(true, true);
        assert_eq!(app.tabs[0].state.primary().head(), TextSize::new(7));
    }
    fn frame(app: &mut IdeApp, ctx: &egui::Context, events: Vec<egui::Event>) -> Vec<String> {
        let output = ctx.run_ui(
            egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 600.0),
                )),
                ..Default::default()
            },
            |_ui| app.error_description_ui(ctx),
        );
        let labels = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_string()),
                _ => None,
            })
            .collect();
        output.drop_without_applying_deltas();
        labels
    }
    #[test]
    fn actual_description_popup_shows_problem_and_escape_and_document_changes_close_it() {
        let (mut app, _root) = app();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        app.tabs[0]
            .state
            .set_carets(vec![Caret::at(TextSize::new(8))]);
        app.show_error_description();
        frame(&mut app, &ctx, vec![]);
        let labels = frame(&mut app, &ctx, vec![]);
        assert!(labels.iter().any(|s| s == "Error Description"));
        assert!(labels.iter().any(|s| s == "actual error"));
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(app.error_description.popup.is_none());
        app.show_error_description();
        app.tabs[0]
            .state
            .insert_text("changed", &EditOptions::default());
        frame(&mut app, &ctx, vec![]);
        assert!(app.error_description.popup.is_none());
    }
    #[test]
    fn stale_popup_does_not_take_escape_from_modal_or_new_editor() {
        for modal in [true, false] {
            let (mut app, root) = app();
            app.tabs[0]
                .state
                .set_carets(vec![Caret::at(TextSize::new(8))]);
            app.show_error_description();
            assert!(app.error_description.popup.is_some());
            if modal {
                app.dialog = Dialog::Settings;
            } else {
                let path = root.path().join("other.py");
                std::fs::write(&path, "other = 1\n").unwrap();
                app.open_file(&path);
                assert_eq!(app.tabs.len(), 2);
            }
            let ctx = egui::Context::default();
            let mut retained_escape = false;
            ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |_ui| {
                    assert!(!app.error_description_keys(&ctx));
                    retained_escape = ctx.input_mut(|input| {
                        input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                    });
                },
            )
            .drop_without_applying_deltas();
            assert!(retained_escape, "stale popup stole owner Escape");
            assert!(app.error_description.popup.is_none());
        }
    }

    #[test]
    fn same_version_analysis_refresh_invalidates_old_description() {
        let (mut app, _root) = app();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        app.tabs[0]
            .state
            .set_carets(vec![Caret::at(TextSize::new(8))]);
        app.show_error_description();
        let version = app.tabs[0].state.doc.version();
        let caret = app.tabs[0].state.primary().head();
        app.tabs[0]
            .state
            .extra_diagnostics
            .iter_mut()
            .find(|d| d.code == "error")
            .unwrap()
            .message = "updated analysis".into();
        let labels = frame(&mut app, &ctx, vec![]);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        assert_eq!(app.tabs[0].state.primary().head(), caret);
        assert!(app.error_description.popup.is_none());
        assert!(!labels.iter().any(|s| s == "actual error"));
        app.show_error_description();
        frame(&mut app, &ctx, vec![]);
        let labels = frame(&mut app, &ctx, vec![]);
        assert!(labels.iter().any(|s| s == "updated analysis"));
        app.tabs[0].state.extra_diagnostics.clear();
        frame(&mut app, &ctx, vec![]);
        assert!(app.error_description.popup.is_none());
    }
    #[test]
    fn other_popup_owners_keep_escape_without_moving_editor_caret() {
        for owner in 0..4 {
            let (mut app, root) = app();
            if owner == 3 {
                let diagnostics = app.tabs[0].state.extra_diagnostics.clone();
                app.open_project(root.path());
                app.open_file(&root.path().join("problems.py"));
                let state = &mut app.tabs[0].state;
                state.semantic_version = Some(state.doc.version());
                state.extra_diagnostics = diagnostics;
            }
            app.tabs[0]
                .state
                .set_carets(vec![Caret::at(TextSize::new(8))]);
            app.show_error_description();
            assert!(
                app.error_description.popup.is_some(),
                "owner {owner} needs a live description"
            );
            let ctx = egui::Context::default();
            match owner {
                0 => app.everywhere.visible = true,
                1 => app.find.visible = true,
                2 => app.intentions = Some((Vec::new(), 0)),
                _ => app.focus_navigation_bar(&ctx),
            }
            let mut retained = false;
            ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |_ui| {
                    assert!(!app.error_description_keys(&ctx));
                    retained = ctx.input_mut(|input| {
                        input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                    });
                },
            )
            .drop_without_applying_deltas();
            assert!(retained, "popup owner {owner} lost Escape");
            assert!(app.error_description.popup.is_none());
            assert_eq!(app.tabs[0].state.primary().head(), TextSize::new(8));
        }
    }
}

#[cfg(test)]
#[path = "error_navigation_visibility_tests.rs"]
mod visibility_tests;
