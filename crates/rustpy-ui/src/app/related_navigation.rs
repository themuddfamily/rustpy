//! Remaining navigation actions share existing checked editor/open routes.
use super::*;

const SELECT_TARGETS: &[&str] = &[
    "Project View",
    "Structure",
    "Changes",
    "File Explorer",
    "Navigation Bar",
    "Favorites / Bookmarks",
    "Terminal",
];

pub(super) fn line_target(input: &str, current_line: usize) -> Option<(usize, Option<usize>)> {
    let mut parts = input.trim().split(':');
    let line = parts.next()?.trim();
    let column = parts.next().map(str::trim);
    if parts.next().is_some() {
        return None;
    }
    let line = if line.is_empty() && column.is_some() {
        current_line.checked_add(1)?
    } else {
        line.parse().ok()?
    };
    let column = match column {
        Some(value) => Some(value.parse::<usize>().ok()?),
        None => None,
    };
    (line > 0 && column != Some(0)).then_some((line, column))
}
impl IdeApp {
    pub(super) fn show_select_in(&mut self) {
        if self
            .active
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| tab.state.path.as_ref())
            .is_none()
        {
            self.set_status("Open a file to select it in another view");
            return;
        }
        self.choice = Some(ChoicePopup::new(
            ChoiceKind::SelectIn,
            "Select In".into(),
            SELECT_TARGETS
                .iter()
                .map(|name| ((*name).into(), String::new()))
                .collect(),
        ));
    }
    pub(super) fn select_in(&mut self, target: usize, ctx: &egui::Context) {
        let project_safe_mode = self.project_safe_mode();
        let Some(path) = self
            .active
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| tab.state.path.clone())
        else {
            return;
        };
        match target {
            0 => self.handle_action(Action::SelectInProjectView, ctx),
            1 => {
                self.left_tool = LeftTool::Structure;
                self.layout.left_visible = true;
                self.focus_editor = false;
            }
            2 => {
                let text = self
                    .active
                    .map(|index| self.tabs[index].state.doc.text())
                    .unwrap_or_default();
                if let Some(repo) = self
                    .project
                    .as_ref()
                    .and_then(|project| project.git.as_ref())
                {
                    self.git_panel.select_working_file(repo, &path, &text);
                    self.bottom = Some(BottomTool::Git);
                    self.focus_editor = false;
                } else {
                    self.set_status("This project has no Git repository");
                }
            }
            3 => {
                let directory = path.parent().unwrap_or(&path);
                let program = if cfg!(target_os = "macos") {
                    "open"
                } else if cfg!(windows) {
                    "explorer"
                } else {
                    "xdg-open"
                };
                match std::process::Command::new(program).arg(directory).spawn() {
                    Ok(_) => self.set_status(format!("Opened {}", directory.display())),
                    Err(error) => self.set_status(format!("Cannot open file manager: {error}")),
                }
            }
            4 => self.focus_navigation_bar(ctx),
            5 => {
                self.reveal_bookmark_path(&path);
                self.left_tool = LeftTool::Bookmarks;
                self.layout.left_visible = true;
                self.focus_editor = false;
            }
            6 => {
                if project_safe_mode {
                    self.set_status("Trust the project before opening its terminal");
                    return;
                }
                let directory = path.parent().unwrap_or(&path);
                let env = crate::terminal_panel::terminal_env(
                    self.settings.terminal.activate_virtualenv,
                    self.project
                        .as_ref()
                        .and_then(|project| project.interpreter.as_ref()),
                );
                match self.terminal_panel.open(directory, &env) {
                    Ok(_) => {
                        self.bottom = Some(BottomTool::Terminal);
                        self.focus_editor = false;
                    }
                    Err(error) => self.set_status(format!("Cannot open terminal: {error}")),
                }
            }
            _ => {}
        }
    }
    pub(super) fn jump_to_source(&mut self, ctx: &egui::Context) {
        if self.jump_to_selected_structure() {
            return;
        }
        let editor_focused = self
            .active
            .and_then(|index| self.tabs.get(index))
            .is_some_and(|tab| tab.view.focused);
        if self.bottom == Some(BottomTool::Find) && self.jump_to_selected_usage() {
            return;
        }
        if crate::project_tree::ProjectTree::has_focus(ctx) && self.left_tool == LeftTool::Project {
            if let Some(path) = self
                .project
                .as_ref()
                .and_then(|project| project.tree.selected.clone())
                .filter(|path| !path.is_dir())
            {
                self.mark_nav_jump();
                self.open_file(&path);
                self.focus_editor = true;
                return;
            }
        }
        if self.bottom == Some(BottomTool::Git) && !editor_focused {
            if let Some(path) = self.git_panel.selected_path().map(Path::to_path_buf) {
                self.mark_nav_jump();
                self.open_file(&path);
                self.focus_editor = true;
                return;
            }
        }
        if let Some(index) = self.active {
            self.tabs[index].state.scroll_to_caret = true;
            self.focus_editor = true;
        }
    }
    pub(super) fn go_to_related_symbol(&mut self) {
        if self.framework_template_related() {
            return;
        }
        self.sync_analysis_now();
        let Some(index) = self.active else {
            return;
        };
        let Some((analysis, file)) = self.analysis_file(index) else {
            return;
        };
        let paths = self.project_files();
        let templates: Vec<_> = paths
            .iter()
            .filter(|path| {
                path.ancestors()
                    .any(|part| part.file_name().is_some_and(|name| name == "templates"))
                    || self.project.as_ref().is_some_and(|project| {
                        project
                            .tree
                            .template_dirs
                            .iter()
                            .any(|directory| path.starts_with(directory))
                    })
            })
            .cloned()
            .collect();
        let locations = rustpy_semantic::template_context::related_locations(
            &analysis,
            &file,
            self.tabs[index].state.primary().head(),
            &paths,
            &templates,
        );
        if !locations.is_empty() {
            self.navigate_locations(locations, "No related symbols found");
            return;
        }
        self.go_to_test();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(source: &str) -> (tempfile::TempDir, IdeApp, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("subject.py");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
        app.open_file(&path);
        (directory, app, path)
    }
    #[test]
    fn line_column_targets_are_positive_unicode_columns_and_reject_ambiguous_input() {
        assert_eq!(line_target("12:3", 8), Some((12, Some(3))));
        assert_eq!(line_target(":3", 8), Some((9, Some(3))));
        assert_eq!(line_target(" 2 ", 8), Some((2, None)));
        for input in ["", "0", "2:0", "2:", "1:2:3", "-1", "word", ":"] {
            assert!(line_target(input, 8).is_none(), "{input}");
        }
        let (_directory, mut app, _) = fixture("first\nπx\n");
        let (line, column) = line_target("2:2", 0).unwrap();
        app.tabs[0].state.go_to_line(line, column);
        assert_eq!(usize::from(app.tabs[0].state.primary().head()), 8);
    }
    #[test]
    fn select_in_chooser_routes_project_structure_bookmarks_and_navigation_bar() {
        let (_directory, mut app, path) = fixture("def subject(): pass\n");
        let context = egui::Context::default();
        app.handle_action(Action::SelectIn, &context);
        assert_eq!(app.choice.as_ref().unwrap().items.len(), 7);
        app.apply_choice(0);
        assert_eq!(app.pending_select_in.take(), Some(0));
        app.select_in(0, &context);
        assert_eq!(
            app.project.as_ref().unwrap().tree.selected.as_deref(),
            Some(path.as_path())
        );
        app.select_in(1, &context);
        assert!(app.left_tool == LeftTool::Structure && app.layout.left_visible);
        app.handle_action(Action::ToggleBookmark, &context);
        let count = app.bookmarks.len();
        app.select_in(5, &context);
        assert!(app.left_tool == LeftTool::Bookmarks);
        assert_eq!(app.bookmarks.len(), count);
        app.select_in(4, &context);
        assert!(!app.focus_editor);
        assert_eq!(app.tabs[0].state.doc.text(), "def subject(): pass\n");
    }
    #[test]
    fn related_test_subject_and_template_view_routes_use_physical_counterparts() {
        // Populate the complete project before its background file snapshot starts.
        // Creating templates later would require delivery of watcher events first.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("subject.py");
        let test = directory.path().join("test_subject.py");
        let template = directory.path().join("templates/page.html");
        std::fs::write(&path, "def subject(): return 1\n").unwrap();
        std::fs::write(&test, "def test_subject(): pass\n").unwrap();
        std::fs::create_dir_all(template.parent().unwrap()).unwrap();
        std::fs::write(&template, "{{ value }}\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
        app.open_file(&path);
        app.go_to_related_symbol();
        assert_eq!(
            app.tabs[app.active.unwrap()].state.path.as_deref(),
            Some(test.as_path())
        );
        app.go_to_related_symbol();
        assert_eq!(
            app.tabs[app.active.unwrap()].state.path.as_deref(),
            Some(path.as_path())
        );
        let source="from django.shortcuts import render\ndef page(request):\n    return render(request,'page.html',{'value':1})\n";
        app.tabs[app.active.unwrap()]
            .state
            .doc
            .set_text(source, &[], &[])
            .unwrap();
        app.go_to_related_symbol();
        assert_eq!(
            app.tabs[app.active.unwrap()].state.path.as_deref(),
            Some(template.as_path())
        );
        app.go_to_related_symbol();
        assert_eq!(
            app.tabs[app.active.unwrap()].state.path.as_deref(),
            Some(path.as_path())
        );
        assert_eq!(
            app.tabs[app.active.unwrap()]
                .state
                .doc
                .slice(app.tabs[app.active.unwrap()].state.primary().range()),
            "page"
        );
    }
    #[test]
    fn new_actions_have_real_bindings_and_right_split_keeps_the_source_buffer() {
        assert_eq!(
            Action::GoToRelatedSymbol.default_shortcuts(),
            ["Ctrl+Alt+Home"]
        );
        assert_eq!(Action::JumpToSource.default_shortcuts(), ["F4"]);
        assert_eq!(Action::SelectIn.default_shortcuts(), ["Alt+F1"]);
        let (_directory, mut app, path) = fixture("def first(): pass\ndef second(): pass\n");
        let context = egui::Context::default();
        app.handle_action(Action::OpenInRightSplit, &context);
        assert!(app
            .tabs
            .iter()
            .any(|tab| tab.group == 1 && tab.state.path.as_deref() == Some(path.as_path())));
        let index = app.active.unwrap();
        app.tabs[index].state.move_to_definition(true);
        assert!(app.tabs[index].state.primary().head() > TextSize::new(0));
        app.handle_action(Action::JumpToSource, &context);
        assert!(app.focus_editor);
    }
    #[test]
    fn existing_error_change_bookmark_and_occurrence_actions_keep_real_destinations() {
        let (_directory, mut app, path) = fixture("one\nold\nmid\nold\n");
        let context = egui::Context::default();
        let changes =
            rustpy_vcs::line_changes("one\nprior\nmid\nprior\n", &app.tabs[0].state.doc.text());
        assert_eq!(changes.len(), 2);
        app.line_changes = Some((path, app.tabs[0].state.doc.version(), changes));
        app.goto_change(true);
        assert_eq!(
            app.tabs[0]
                .state
                .doc
                .line_of(app.tabs[0].state.primary().head()),
            1
        );
        app.goto_change(true);
        assert_eq!(
            app.tabs[0]
                .state
                .doc
                .line_of(app.tabs[0].state.primary().head()),
            3
        );
        app.goto_change(false);
        assert_eq!(
            app.tabs[0]
                .state
                .doc
                .line_of(app.tabs[0].state.primary().head()),
            1
        );
        app.handle_action(Action::ToggleBookmark, &context);
        app.tabs[0].state.go_to_line(4, None);
        app.handle_action(Action::ToggleBookmark, &context);
        app.handle_action(Action::NextBookmark, &context);
        assert_eq!(
            app.tabs[0]
                .state
                .doc
                .line_of(app.tabs[0].state.primary().head()),
            1
        );
        app.handle_action(Action::PreviousBookmark, &context);
        assert_eq!(
            app.tabs[0]
                .state
                .doc
                .line_of(app.tabs[0].state.primary().head()),
            3
        );
        let state = &mut app.tabs[0].state;
        state.extra_diagnostics = vec![
            rustpy_core::Diagnostic::warning(TextRange::new(4.into(), 7.into()), "test", "first"),
            rustpy_core::Diagnostic::error(TextRange::new(12.into(), 15.into()), "test", "second"),
        ];
        state.semantic_version = Some(state.doc.version());
        state.set_carets(vec![crate::editor::Caret::at(0.into())]);
        app.navigate_highlighted_error(true, false);
        assert_eq!(app.tabs[0].state.primary().head(), TextSize::new(4));
        app.navigate_highlighted_error(true, true);
        assert_eq!(app.tabs[0].state.primary().head(), TextSize::new(12));
        app.find.query = "old".into();
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(0.into())]);
        app.handle_action(Action::FindNext, &context);
        assert_eq!(
            app.tabs[0].state.primary().range(),
            TextRange::new(4.into(), 7.into())
        );
        app.handle_action(Action::FindNext, &context);
        assert_eq!(
            app.tabs[0].state.primary().range(),
            TextRange::new(12.into(), 15.into())
        );
        app.handle_action(Action::FindPrevious, &context);
        assert_eq!(
            app.tabs[0].state.primary().range(),
            TextRange::new(4.into(), 7.into())
        );
    }
    #[test]
    fn brace_and_block_navigation_use_syntax_without_changing_text() {
        let source = "def f():\n    value = (1 + 2)\n    return value\n";
        let (_directory, mut app, _) = fixture(source);
        let open = source.find("(1").unwrap();
        let close = source[open..].find(')').unwrap() + open;
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(open as u32))]);
        app.tabs[0].state.move_to_matching_brace();
        assert_eq!(
            app.tabs[0].state.primary().head(),
            TextSize::new(close as u32)
        );
        app.tabs[0].state.move_to_matching_brace();
        assert_eq!(
            app.tabs[0].state.primary().head(),
            TextSize::new(open as u32)
        );
        app.tabs[0].state.move_to_code_block(true);
        assert!(app.tabs[0].state.primary().head() > TextSize::new(open as u32));
        assert_eq!(app.tabs[0].state.doc.text(), source);
    }
}
