//! Bound file-crumb intents; symbols remain the existing breadcrumb route.
use super::*;
#[derive(Clone)]
struct Target {
    document: rustpy_text::DocumentId,
    path: PathBuf,
}
impl IdeApp {
    pub(super) fn refresh_editor_window_title(&mut self, ctx: &egui::Context) {
        let project = self
            .project
            .as_ref()
            .and_then(|project| project.root.file_name())
            .map(|name| name.to_string_lossy().into_owned());
        let tab = self.active.and_then(|index| self.tabs.get(index));
        let file = tab.map(|tab| tab.state.title());
        let mut title = editor_title::window_title(
            self.settings
                .chrome
                .title_project
                .then_some(project.as_deref())
                .flatten(),
            if self.settings.chrome.title_file && self.settings.chrome.title_full_path {
                tab.and_then(|tab| tab.state.path.as_deref())
            } else {
                None
            },
            self.settings
                .chrome
                .title_file
                .then_some(file.as_deref())
                .flatten(),
        );
        if let Some(branch) = self
            .project
            .as_ref()
            .filter(|_| self.settings.chrome.title_branch)
            .and_then(|project| project.git_branch.as_deref())
        {
            title.push_str(&format!(" [{branch}]"));
        }
        if self.annotate_enabled && self.settings.vcs_operations.annotate_title {
            title.push_str(" [Git annotations]");
        }
        if self.window_title != title {
            self.window_title = title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }
        // The accessible window name: without it AT-SPI exposes an unnamed
        // frame and Orca announces only "frame" on window activation.
        let name = self.window_title.clone();
        ctx.accesskit_node_builder(egui::accesskit_root_id(), |root| root.set_label(name));
    }

    pub(super) fn editor_file_breadcrumb(&mut self, ui: &mut egui::Ui, index: usize, title: &str) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let captured = tab.state.path.clone().map(|path| Target {
            document: tab.state.doc.identity(),
            path,
        });
        let intent = ui
            .push_id(tab.view.id.with("file-path-popup"), |ui| {
                editor_title::show(
                    ui,
                    captured.as_ref().map(|target| target.path.as_path()),
                    title,
                )
            })
            .inner;
        if let (Some(target), Some(intent)) = (captured, intent) {
            self.consume_editor_title(target, intent);
        }
    }
    fn consume_editor_title(&mut self, captured: Target, intent: editor_title::Intent) {
        if !self.tabs.iter().any(|tab| {
            tab.state.doc.identity() == captured.document
                && tab.state.path.as_ref() == Some(&captured.path)
        }) {
            return;
        }
        let path = match &intent {
            editor_title::Intent::CopyPath(path) | editor_title::Intent::SelectIn(path) => path,
        };
        if !editor_title::hierarchy(&captured.path).contains(path) {
            return;
        }
        match intent {
            editor_title::Intent::CopyPath(path) => {
                // Match path-copy history policy, but invalidate rich text provenance.
                self.clipboard_payload = None;
                self.pending_copy = Some(path.display().to_string());
            }
            editor_title::Intent::SelectIn(path) => {
                let Some(project) = &mut self.project else {
                    self.set_status("No project view is available for this path");
                    return;
                };
                // Match ProjectTree::top_of without probing disk. Ancestors
                // outside every displayed root cannot be selected by that tree.
                if !std::iter::once(&project.tree.root)
                    .chain(project.tree.content_roots.iter())
                    .any(|root| path.starts_with(root))
                {
                    self.set_status("This path is outside the project view's content roots");
                    return;
                }
                project.tree.reveal(&path);
                project.tree.request_focus();
                self.left_tool = LeftTool::Project;
                self.layout.left_visible = true;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> IdeApp {
        let mut app = IdeApp::new(Settings::default(), None);
        let mut state = EditorState::from_text("class π: pass", Language::Python);
        state.path = Some(PathBuf::from("/owned project/π folder/module π.py"));
        app.push_tab(state);
        app
    }
    #[test]
    fn actual_app_copy_is_single_publication_and_uses_captured_file_not_active_tab() {
        let mut app = app();
        let target = Target {
            document: app.tabs[0].state.doc.identity(),
            path: app.tabs[0].state.path.clone().unwrap(),
        };
        app.push_tab(EditorState::from_text("other", Language::PlainText));
        app.active = Some(1);
        app.consume_editor_title(
            target.clone(),
            editor_title::Intent::CopyPath(target.path.clone()),
        );
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.flush_pending_copy(ui.ctx());
            app.flush_pending_copy(ui.ctx());
        });
        let copied: Vec<_> = output
            .platform_output
            .commands
            .iter()
            .filter_map(|command| {
                if let egui::OutputCommand::CopyText(text) = command {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(copied, vec!["/owned project/π folder/module π.py"]);
        assert_eq!(app.active, Some(1));
        assert_eq!(app.tabs[0].state.doc.text(), "class π: pass");
        assert!(!app.tabs[0].state.doc.can_undo());
        output.drop_without_applying_deltas();
    }
    #[test]
    fn stale_document_path_and_foreign_ancestor_refuse_publication() {
        let mut app = app();
        let target = Target {
            document: app.tabs[0].state.doc.identity(),
            path: app.tabs[0].state.path.clone().unwrap(),
        };
        app.consume_editor_title(
            target.clone(),
            editor_title::Intent::CopyPath(PathBuf::from("/foreign")),
        );
        assert!(app.pending_copy.is_none());
        app.tabs[0].state.path = Some(PathBuf::from("/renamed.py"));
        app.consume_editor_title(
            target.clone(),
            editor_title::Intent::CopyPath(target.path.clone()),
        );
        assert!(app.pending_copy.is_none());
        assert!(!app.tabs[0].state.doc.can_undo());
    }
    #[test]
    fn actual_app_pathless_breadcrumb_has_no_filesystem_action() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("unsaved", Language::PlainText));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.editor_file_breadcrumb(ui, 0, "Untitled")
        });
        assert!(app.pending_copy.is_none());
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Untitled")));
        assert!(!app.tabs[0].state.doc.can_undo());
        output.drop_without_applying_deltas();
    }
    #[test]
    fn actual_app_title_emits_full_unicode_viewport_title() {
        let mut app = app();
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.refresh_editor_window_title(ui.ctx())
        });
        assert_eq!(
            app.window_title,
            "/owned project/π folder/module π.py – RustPy"
        );
        assert!(output.viewport_output.values().any(|viewport| viewport.commands.iter()
            .any(|command| matches!(command, egui::ViewportCommand::Title(title) if title == &app.window_title))));
        output.drop_without_applying_deltas();
    }
    #[test]
    fn actual_app_select_in_reveals_exact_ancestor_without_active_retargeting() {
        let mut app = app();
        let root = PathBuf::from("/owned project");
        app.project = Some(Project {
            root: root.clone(),
            tree: ProjectTree::new(root.clone()),
            analysis: Arc::new(Analysis::for_project(&root)),
            run_configs: RunConfigStore::default(),
            interpreters: vec![],
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        let target = Target {
            document: app.tabs[0].state.doc.identity(),
            path: app.tabs[0].state.path.clone().unwrap(),
        };
        let parent = PathBuf::from("/owned project/π folder");
        app.consume_editor_title(target, editor_title::Intent::SelectIn(parent.clone()));
        assert_eq!(
            app.project.as_ref().unwrap().tree.selected.as_ref(),
            Some(&parent)
        );
        assert_eq!(app.left_tool, LeftTool::Project);
        assert!(app.layout.left_visible);
        assert_eq!(app.tabs[0].state.doc.text(), "class π: pass");
        assert!(!app.tabs[0].state.doc.can_undo());
    }
    #[test]
    fn actual_app_breadcrumb_click_copy_routes_rendered_path_to_platform_once() {
        let mut app = app();
        app.push_tab(EditorState::from_text("other", Language::PlainText));
        app.active = Some(1);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
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
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(700.0, 450.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    app.editor_file_breadcrumb(ui, 0, "module π.py");
                    app.flush_pending_copy(ui.ctx());
                },
            )
        }
        fn label(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap()
        }
        fn click(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        }
        let output = frame(&mut app, &ctx, 0.0, vec![]);
        let crumb = label(&output, "module π.py");
        output.drop_without_applying_deltas();
        frame(&mut app, &ctx, 0.02, click(crumb, true)).drop_without_applying_deltas();
        frame(&mut app, &ctx, 0.04, click(crumb, false)).drop_without_applying_deltas();
        let output = frame(&mut app, &ctx, 0.06, vec![]);
        label(&output, "/owned project/π folder/module π.py");
        let copy = label(&output, "Copy Path");
        output.drop_without_applying_deltas();
        frame(&mut app, &ctx, 0.08, click(copy, true)).drop_without_applying_deltas();
        let output = frame(&mut app, &ctx, 0.10, click(copy, false));
        let copied: Vec<_> = output
            .platform_output
            .commands
            .iter()
            .filter_map(|command| {
                if let egui::OutputCommand::CopyText(text) = command {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(copied, vec!["/owned project/π folder/module π.py"]);
        assert_eq!(app.active, Some(1));
        assert!(!app.tabs[0].state.doc.can_undo());
        output.drop_without_applying_deltas();
    }
    #[test]
    fn unavailable_ancestor_preserves_project_selection_and_tool_visibility() {
        let mut app = app();
        let root = PathBuf::from("/owned project");
        let selected = root.join("old.py");
        let mut tree = ProjectTree::new(root.clone());
        tree.selected = Some(selected.clone());
        app.project = Some(Project {
            root: root.clone(),
            tree,
            analysis: Arc::new(Analysis::for_project(&root)),
            run_configs: RunConfigStore::default(),
            interpreters: vec![],
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        app.left_tool = LeftTool::Structure;
        app.layout.left_visible = false;
        let target = Target {
            document: app.tabs[0].state.doc.identity(),
            path: app.tabs[0].state.path.clone().unwrap(),
        };
        app.consume_editor_title(target, editor_title::Intent::SelectIn(PathBuf::from("/")));
        assert_eq!(
            app.project.as_ref().unwrap().tree.selected.as_ref(),
            Some(&selected)
        );
        assert_eq!(app.left_tool, LeftTool::Structure);
        assert!(!app.layout.left_visible);
        assert!(app
            .status
            .as_ref()
            .is_some_and(|(message, _)| message.contains("outside")));
        assert!(!app.tabs[0].state.doc.can_undo());
    }
}
