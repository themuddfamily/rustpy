//! Keyboard equivalent of the editor's painted line controls.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_text::DocumentId;

pub(super) struct LineOwner {
    document: DocumentId,
    version: u64,
    path: Option<PathBuf>,
    caret: crate::editor::Caret,
    line: usize,
    selected: usize,
    annotation: String,
    language: crate::editor::Language,
    color_previews: bool,
    color_page: usize,
}
#[derive(Default)]
pub(super) struct EditorAccessibility {
    pub(super) owner: Option<LineOwner>,
    pending_tool_focus: Option<(BottomTool, Option<PathBuf>, Option<DocumentId>)>,
}
const LABELS: &[&str] = &[
    "Collapse or expand region",
    "Toggle line breakpoint",
    "Edit line breakpoint",
    "Run this line",
    "Debug this line",
    "Run this line with coverage",
    "Profile this line",
    "Show context actions",
    "Go to super method",
    "Go to implementations",
];
impl IdeApp {
    pub(super) fn queue_tool_controls_focus(&mut self, tool: BottomTool) {
        self.editor_accessibility.pending_tool_focus = Some((
            tool,
            self.project.as_ref().map(|p| p.root.clone()),
            self.active
                .and_then(|i| self.tabs.get(i))
                .map(|t| t.state.doc.identity()),
        ));
        self.focus_editor = false;
    }
    pub(super) fn request_tool_controls_focus(&mut self, ctx: &egui::Context) {
        if !self.editor_keyboard()
            || !self.line_actions_owner_available(ctx)
            || self.ide_shortcuts_blocked()
        {
            return;
        }
        if let Some(tool) = self.bottom {
            self.queue_tool_controls_focus(tool);
        } else {
            self.set_status("Open a tool window before focusing its controls");
        }
    }
    pub(super) fn tool_controls_focus_entry(&mut self, ui: &mut egui::Ui, title: &str) {
        let available = self.editor_keyboard()
            && self.line_actions_owner_available(ui.ctx())
            && !self.ide_shortcuts_blocked();
        let pending = self.editor_accessibility.pending_tool_focus.take();
        let id = ui.id().with("tool-controls-focus");
        let response = ui.interact(
            ui.available_rect_before_wrap(),
            id,
            if available {
                egui::Sense::focusable_noninteractive()
            } else {
                egui::Sense::hover()
            },
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Other,
                available,
                format!("{title} tool window controls. Tab moves to the next control"),
            )
        });
        response.ctx.accesskit_node_builder(response.id, |node| {
            node.set_role(egui::accesskit::Role::Group)
        });
        if let Some((tool, project, document)) = pending {
            if available
                && self.bottom == Some(tool)
                && self.project.as_ref().map(|p| p.root.clone()) == project
                && self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .map(|t| t.state.doc.identity())
                    == document
            {
                response.request_focus();
            }
        }
        if available && response.has_focus() {
            crate::a11y::announce(
                ui.ctx(),
                response.id,
                &format!("{title} tool window controls. Press Tab to choose a control"),
            );
        }
    }
    pub(super) fn cancel_pending_tool_focus(&mut self) {
        self.editor_accessibility.pending_tool_focus = None;
    }
    pub(super) fn open_editor_line_actions(&mut self, ctx: &egui::Context) {
        if !self.editor_keyboard()
            || !self.line_actions_owner_available(ctx)
            || !self
                .active
                .and_then(|i| self.tabs.get(i))
                .is_some_and(|tab| ctx.memory(|m| m.has_focus(tab.view.id)))
        {
            return;
        }
        let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) else {
            return;
        };
        let caret = tab.state.primary();
        let line = tab.state.doc.line_of(caret.head());
        let annotation = self
            .blame
            .as_ref()
            .filter(|(path, version, _)| {
                Some(path) == tab.state.path.as_ref() && *version == tab.state.doc.version()
            })
            .and_then(|(_, _, lines)| lines.get(line))
            .map(|(text, _, detail)| format!("{text}\n{detail}"))
            .unwrap_or_else(|| {
                "No current Git blame annotation. Enable Annotate to load annotations.".into()
            });
        self.editor_accessibility.owner = Some(LineOwner {
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            path: tab.state.path.clone(),
            caret,
            line,
            selected: 0,
            annotation,
            language: tab.state.language,
            color_previews: self.settings.editor.show_css_color_preview,
            color_page: 0,
        });
        self.completion.visible = false;
        self.quick_doc = None;
        self.param_info = None;
    }
    pub(super) fn line_actions_owner_available(&self, ctx: &egui::Context) -> bool {
        self.component_menu_owner_available(false) && !egui::Popup::is_any_open(ctx)
    }
    /// A component's own context menu does not retire its modal authority.
    /// The Branches dialog is permitted only for its explicit branch owner.
    pub(super) fn component_menu_owner_available(&self, branches: bool) -> bool {
        !self.coordinated_quit_active()
            && (matches!(self.dialog, Dialog::None)
                || (branches && matches!(self.dialog, Dialog::Branches)))
            && self.choice.is_none()
            && !self.everywhere.visible
            && !self.find.visible
            && !self.find_in_files.visible
            && !self.nav.popup_visible()
            && self.intentions.is_none()
            && self.inplace_rename.is_none()
            && !self.completion.visible
            && !self.intention_menu.dialog_active()
            && !self.emmet_ui.active()
            && !self.injection_ui.active()
            && !self.generate_methods.active()
            && (branches || !self.git_panel.branch_popup)
            && self.layout.exit_prompt.is_none()
    }
    fn line_actions_current(&self, ctx: &egui::Context) -> bool {
        if !self.line_actions_owner_available(ctx) {
            return false;
        }
        if let Some(focus) = ctx.memory(|m| m.focused()) {
            let editor = self
                .active
                .and_then(|i| self.tabs.get(i))
                .is_some_and(|tab| focus == tab.view.id);
            let chooser = ctx
                .read_response(focus)
                .is_some_and(|r| r.layer_id.id == egui::Id::new("editor-line-actions"));
            if !editor && !chooser {
                return false;
            }
        }
        self.editor_accessibility
            .owner
            .as_ref()
            .is_some_and(|owner| {
                self.active
                    .and_then(|i| self.tabs.get(i))
                    .is_some_and(|tab| {
                        tab.state.doc.identity() == owner.document
                            && tab.state.doc.version() == owner.version
                            && tab.state.path == owner.path
                            && tab.state.primary() == owner.caret
                            && tab.state.language == owner.language
                            && self.settings.editor.show_css_color_preview == owner.color_previews
                    })
            })
    }
    fn apply_editor_line_action(&mut self, selected: usize, ctx: &egui::Context) {
        if !self.line_actions_current(ctx) {
            self.editor_accessibility.owner = None;
            self.status = Some((
                "Line actions unavailable: editor changed".into(),
                ctx.input(|i| i.time),
            ));
            return;
        }
        let owner = self.editor_accessibility.owner.take().unwrap();
        let i = self.active.unwrap();
        match selected {
            0 => {
                self.tabs[i].state.toggle_fold_at_line(owner.line);
            }
            1 => self.handle_action(Action::ToggleLineBreakpoint, ctx),
            2 => self.open_breakpoint_popup(i, owner.line, None),
            3..=6 => self.gutter_run(
                i,
                owner.line,
                [
                    crate::run_context::RunMode::Run,
                    crate::run_context::RunMode::Debug,
                    crate::run_context::RunMode::Coverage,
                    crate::run_context::RunMode::Profile,
                ][selected - 3],
            ),
            7 => self.handle_action(Action::ShowIntentionActions, ctx),
            8 => self.handle_action(Action::GotoSuperMethod, ctx),
            9 => self.handle_action(Action::GotoImplementation, ctx),
            _ => {}
        }
        self.focus_editor = true;
    }
    pub(super) fn editor_accessibility_keyboard(&mut self, ui: &mut egui::Ui) -> bool {
        if self.editor_accessibility.owner.is_none() {
            return false;
        }
        let ctx = ui.ctx();
        if !self.line_actions_current(ctx) {
            self.editor_accessibility.owner = None;
            return false;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.editor_accessibility.owner = None;
            self.focus_editor = true;
        } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            let selected = self.editor_accessibility.owner.as_ref().unwrap().selected;
            self.apply_editor_line_action(selected, ctx);
        } else {
            let next_colors =
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown));
            let previous_colors =
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp));
            let owner = self.editor_accessibility.owner.as_ref().unwrap();
            let pages = self
                .active
                .and_then(|i| self.tabs.get(i))
                .and_then(|tab| {
                    tab.view
                        .cached_line_color_information(&tab.state, owner.line, owner.color_page)
                })
                .map_or(1, |(_, pages)| pages);
            let owner = self.editor_accessibility.owner.as_mut().unwrap();
            if next_colors {
                owner.color_page = (owner.color_page + 1).min(pages - 1);
            }
            if previous_colors {
                owner.color_page = owner.color_page.saturating_sub(1);
            }
            let down =
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
            let up = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
            let owner = self.editor_accessibility.owner.as_mut().unwrap();
            if down {
                owner.selected = (owner.selected + 1) % LABELS.len();
            }
            if up {
                owner.selected = (owner.selected + LABELS.len() - 1) % LABELS.len();
            }
        }
        true
    }
    pub(super) fn editor_accessibility_ui(&mut self, ctx: &egui::Context) {
        if self.editor_accessibility.owner.is_none() {
            return;
        }
        if !self.line_actions_current(ctx) {
            self.editor_accessibility.owner = None;
            return;
        }
        let owner = self.editor_accessibility.owner.as_ref().unwrap();
        let line = owner.line;
        let selected = owner.selected;
        let annotation = owner.annotation.clone();
        let colors = if owner.color_previews {
            self.active
                .and_then(|i| self.tabs.get(i))
                .and_then(|tab| {
                    tab.view
                        .cached_line_color_information(&tab.state, line, owner.color_page)
                })
                .map(|(text, _)| text)
                .unwrap_or_else(|| {
                    "No current cached source colors. Reopen after the editor is painted.".into()
                })
        } else {
            "Source color previews are disabled in Editor settings.".into()
        };
        let mut chosen = None;
        let mut close = false;
        egui::Window::new(format!("Editor line {} actions", line + 1))
            .id(egui::Id::new("editor-line-actions"))
            .collapsible(false)
            .resizable(false)
            .show_modal(ctx, |ui| {
                ui.label("Up/Down selects an action; Enter applies; Escape cancels.");
                for (index, label) in LABELS.iter().enumerate() {
                    if ui.selectable_label(index == selected, *label).clicked() {
                        chosen = Some(index);
                    }
                }
                ui.separator();
                ui.label("Git blame for this line");
                ui.label(annotation);
                ui.separator();
                ui.label(&colors);
                close = ui.button("Close").clicked();
                crate::a11y::announce(
                    ui.ctx(),
                    ui.id(),
                    &format!(
                        "Line {}. Selected action: {}. {colors}",
                        line + 1,
                        LABELS[selected]
                    ),
                );
            });
        if close {
            self.editor_accessibility.owner = None;
            self.focus_editor = true;
        }
        if let Some(selected) = chosen {
            self.apply_editor_line_action(selected, ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, IdeApp, egui::Context) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("π.py");
        std::fs::write(&path, "def π():\n    return 1\n").unwrap();
        let mut settings = Settings::default();
        settings.completion.show_as_you_type = false;
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        assert!(matches!(app.dialog, Dialog::None));
        assert!(app.editor_keyboard());
        app.open_file(&path);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        frame(&mut app, &ctx, 0.0, vec![]);
        frame(&mut app, &ctx, 0.1, vec![]);
        (root, app, ctx)
    }
    fn key(key: egui::Key) -> Vec<egui::Event> {
        vec![
            egui::Event::ModifiersChanged(egui::Modifiers::NONE),
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }
    fn frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> egui::accesskit::TreeUpdate {
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                focused: true,
                events,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 650.0))),
                ..Default::default()
            },
            |ui| {
                app.global_shortcuts(ui);
                egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
                app.editor_accessibility_ui(ui.ctx());
            },
        );
        let update = output.platform_output.accesskit_update.clone().unwrap();
        output.drop_without_applying_deltas();
        update
    }
    #[test]
    fn actual_keyboard_line_information_reads_and_pages_the_painted_css_cache() {
        let (root, mut app, ctx) = fixture();
        let path = root.path().join("π colors.css");
        let source = ".π { color: #ff1234; background: rgba(18, 171, 239, 0.5); }\n";
        std::fs::write(&path, source).unwrap();
        app.open_file(&path);
        app.settings.editor.show_css_color_preview = true;
        frame(&mut app, &ctx, 0.2, vec![]);
        frame(&mut app, &ctx, 0.3, vec![]);
        app.handle_action(Action::EditorLineActions, &ctx);
        assert!(app.editor_accessibility.owner.is_some());
        frame(&mut app, &ctx, 0.4, vec![]);
        let update = frame(&mut app, &ctx, 0.5, vec![]);
        let status = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Status
                    && node
                        .label()
                        .is_some_and(|label| label.starts_with("Line 1. Selected action"))
            })
            .unwrap();
        assert!(status
            .1
            .label()
            .unwrap()
            .contains("#ff1234: RGBA(255, 18, 52, 255)"));
        assert!(status
            .1
            .label()
            .unwrap()
            .contains("rgba(18, 171, 239, 0.5): RGBA(18, 171, 239, 128)"));
        frame(&mut app, &ctx, 0.6, key(egui::Key::Escape));
        let i = app.active.unwrap();
        assert_eq!(app.tabs[i].state.doc.text(), source);
        assert!(!app.tabs[i].state.doc.can_undo());
        // A changed language or disabled setting cannot keep captured cache authority.
        app.handle_action(Action::EditorLineActions, &ctx);
        app.tabs[i].state.language = crate::editor::Language::Python;
        frame(&mut app, &ctx, 0.7, vec![]);
        assert!(app.editor_accessibility.owner.is_none());
        app.tabs[i].state.language = crate::editor::Language::Css;
        frame(&mut app, &ctx, 0.8, vec![]);
        app.handle_action(Action::EditorLineActions, &ctx);
        app.settings.editor.show_css_color_preview = false;
        frame(&mut app, &ctx, 0.9, vec![]);
        assert!(app.editor_accessibility.owner.is_none());
    }
    #[test]
    fn actual_page_keys_reach_colors_beyond_the_pointer_swatch_overflow() {
        let (root, mut app, ctx) = fixture();
        let path = root.path().join("overflow.css");
        let source = format!(
            ".π {{ {} }}\n",
            (0..18)
                .map(|n| format!("--color{n}: #{n:06x};"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        std::fs::write(&path, &source).unwrap();
        app.open_file(&path);
        app.settings.editor.show_css_color_preview = true;
        frame(&mut app, &ctx, 0.2, vec![]);
        frame(&mut app, &ctx, 0.3, vec![]);
        app.handle_action(Action::EditorLineActions, &ctx);
        frame(&mut app, &ctx, 0.4, vec![]);
        frame(&mut app, &ctx, 0.5, key(egui::Key::PageDown));
        let last = frame(&mut app, &ctx, 0.6, key(egui::Key::PageDown));
        assert!(last.nodes.iter().any(|(_, node)| node
            .label()
            .is_some_and(|text| text.contains("page 3 of 3")
                && text.contains("#000011: RGBA(0, 0, 17, 255)"))));
        let middle = frame(&mut app, &ctx, 0.7, key(egui::Key::PageUp));
        assert!(middle
            .nodes
            .iter()
            .any(|(_, node)| node
                .label()
                .is_some_and(|text| text.contains("page 2 of 3")
                    && text.contains("#000008: RGBA(0, 0, 8, 255)"))));
        frame(&mut app, &ctx, 0.8, key(egui::Key::Escape));
        let i = app.active.unwrap();
        assert_eq!(app.tabs[i].state.doc.text(), source);
        assert!(!app.tabs[i].state.doc.can_undo());
    }

    #[test]
    fn actual_line_action_keyboard_toggles_breakpoint_on_captured_unicode_owner() {
        let (_root, mut app, ctx) = fixture();
        let path = app.tabs[0].state.path.clone().unwrap();
        let before = app.tabs[0].state.doc.text();
        app.handle_action(Action::EditorLineActions, &ctx);
        frame(&mut app, &ctx, 0.2, vec![]);
        let update = frame(&mut app, &ctx, 0.3, key(egui::Key::ArrowDown));
        assert!(update.nodes.iter().any(|(_, node)| node
            .label()
            .is_some_and(|label| label.contains("Selected action: Toggle line breakpoint"))));
        frame(&mut app, &ctx, 0.4, key(egui::Key::Enter));
        assert!(app.breakpoints.get(&path, 1).is_some());
        assert!(app.editor_accessibility.owner.is_none());
        assert_eq!(app.tabs[0].state.doc.text(), before);
        assert!(!app.tabs[0].state.doc.can_undo());
    }
    #[test]
    fn actual_line_menu_reads_cached_blame_cancels_and_refuses_changed_document() {
        let (_root, mut app, ctx) = fixture();
        let path = app.tabs[0].state.path.clone().unwrap();
        app.blame = Some((
            path.clone(),
            app.tabs[0].state.doc.version(),
            vec![("2026 Ada".into(), 0, "abc123 Ada\nUnicode π commit".into())],
        ));
        app.handle_action(Action::EditorLineActions, &ctx);
        assert!(app
            .editor_accessibility
            .owner
            .as_ref()
            .unwrap()
            .annotation
            .contains("Unicode π commit"));
        frame(&mut app, &ctx, 0.2, key(egui::Key::Escape));
        assert!(app.editor_accessibility.owner.is_none());
        app.handle_action(Action::EditorLineActions, &ctx);
        app.tabs[0].state.move_doc_end(false);
        app.apply_editor_line_action(1, &ctx);
        assert!(app.breakpoints.get(&path, 1).is_none());
        assert!(app.status.as_ref().unwrap().0.contains("editor changed"));
    }
    #[test]
    fn split_resize_actions_are_bounded_keyboard_discoverable_and_do_not_edit() {
        let (_root, mut app, ctx) = fixture();
        let before = app.tabs[0].state.doc.text();
        let second = app.tabs[0]
            .state
            .path
            .as_ref()
            .unwrap()
            .parent()
            .unwrap()
            .join("second.py");
        std::fs::write(&second, "pass\n").unwrap();
        app.open_file(&second);
        app.handle_action(Action::SplitVertically, &ctx);
        assert!(app.split.is_some());
        for _ in 0..30 {
            app.handle_action(Action::EnlargeFirstSplit, &ctx);
        }
        assert_eq!(app.split_fraction, 0.9);
        for _ in 0..30 {
            app.handle_action(Action::ShrinkFirstSplit, &ctx);
        }
        assert_eq!(app.split_fraction, 0.1);
        app.handle_action(Action::EqualizeSplits, &ctx);
        assert_eq!(app.split_fraction, 0.5);
        assert!(Action::ALL.contains(&Action::EnlargeFirstSplit));
        assert_eq!(app.tabs[0].state.doc.text(), before);
        assert!(!app.tabs[0].state.doc.can_undo());
    }
    #[test]
    fn line_chooser_yields_escape_to_actual_find_and_cannot_open_over_its_field() {
        let (_root, mut app, ctx) = fixture();
        let before = app.tabs[0].state.doc.text();
        let version = app.tabs[0].state.doc.version();
        app.handle_action(Action::EditorLineActions, &ctx);
        assert!(app.editor_accessibility.owner.is_some());
        frame(&mut app, &ctx, 0.2, vec![]);
        app.handle_action(Action::Find, &ctx);
        assert!(app.find.visible);
        frame(&mut app, &ctx, 0.3, vec![]);
        assert!(app.editor_accessibility.owner.is_none());
        app.handle_action(Action::EditorLineActions, &ctx);
        assert!(
            app.editor_accessibility.owner.is_none(),
            "focused Find owns input"
        );
        frame(&mut app, &ctx, 0.4, key(egui::Key::Escape));
        assert!(
            !app.find.visible,
            "actual Find bar receives and handles Escape"
        );
        assert_eq!(app.tabs[0].state.doc.text(), before);
        assert_eq!(app.tabs[0].state.doc.version(), version);
    }
    #[test]
    fn actual_diagram_activation_moves_keyboard_focus_and_enter_navigates() {
        let (root, mut app, ctx) = fixture();
        let target = root.path().join("target.py");
        std::fs::write(&target, "class Unicodeπ:\n    pass\n").unwrap();
        let before = app.tabs[0].state.doc.text();
        // Production diagram builders install project ownership before their
        // captured model. First paint correctly clears a foreign/unowned model.
        app.diagram_panel.project_context(root.path());
        app.diagram_panel.set(
            "Target".into(),
            vec![crate::diagram_panel::DiagramClass {
                id: "accessible-class".into(),
                name: "Unicodeπ".into(),
                path: target.clone(),
                range: rustpy_core::TextRange::new(TextSize::new(0), TextSize::new(15)),
                bases: vec![],
                fields: vec![],
                methods: vec![],
            }],
        );
        app.diagram_panel
            .capture_sources(&app.project.as_ref().unwrap().analysis);
        app.handle_action(Action::ToggleDiagramView, &ctx);
        assert_eq!(app.bottom, Some(BottomTool::Diagram));
        for (time, events) in [(0.2, vec![]), (0.3, vec![]), (0.4, key(egui::Key::Enter))] {
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    focused: true,
                    events,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 720.0))),
                    ..Default::default()
                },
                |ui| {
                    app.global_shortcuts(ui);
                    app.bottom_panel(ui);
                    egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
                },
            );
            if time == 0.3 {
                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                assert!(
                    tree.nodes.iter().any(|(id, node)| {
                        *id == tree.focus && node.label() == Some("Open selected class")
                    }),
                    "diagram activation must focus the actual navigation button before Enter"
                );
                assert_eq!(app.diagram_panel.classes.len(), 1);
            }
            output.drop_without_applying_deltas();
        }
        assert_eq!(
            app.tabs[app.active.unwrap()].state.path.as_deref(),
            Some(target.as_path())
        );
        assert_eq!(app.tabs[0].state.doc.text(), before);
        assert!(!app.tabs[0].state.doc.can_undo());
    }
    #[test]
    fn actual_tool_focus_shortcut_enters_named_pane_then_tab_reaches_its_control() {
        let (_root, mut app, ctx) = fixture();
        app.handle_action(Action::ToggleProblemsView, &ctx);
        let original = app.tabs[0].state.doc.text();
        let mods = egui::Modifiers {
            ctrl: true,
            alt: true,
            ..Default::default()
        };
        let mut updates = Vec::new();
        for (time, events) in [
            (
                0.2,
                vec![
                    egui::Event::ModifiersChanged(mods),
                    egui::Event::Key {
                        key: egui::Key::F6,
                        physical_key: Some(egui::Key::F6),
                        pressed: true,
                        repeat: false,
                        modifiers: mods,
                    },
                    egui::Event::Key {
                        key: egui::Key::F6,
                        physical_key: Some(egui::Key::F6),
                        pressed: false,
                        repeat: false,
                        modifiers: mods,
                    },
                ],
            ),
            (0.3, vec![]),
            (0.4, key(egui::Key::Tab)),
        ] {
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    focused: true,
                    events,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 720.0))),
                    ..Default::default()
                },
                |ui| {
                    app.global_shortcuts(ui);
                    app.bottom_panel(ui);
                    egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
                },
            );
            updates.push(output.platform_output.accesskit_update.clone().unwrap());
            output.drop_without_applying_deltas();
        }
        assert!(updates[1]
            .nodes
            .iter()
            .any(|(id, node)| *id == updates[1].focus
                && node
                    .label()
                    .is_some_and(|s| s.starts_with("Problems tool window controls"))));
        assert_ne!(
            updates[2].focus, updates[1].focus,
            "actual Tab leaves pane entry"
        );
        assert!(
            updates[2]
                .nodes
                .iter()
                .any(|(id, node)| *id == updates[2].focus && node.label() == Some("Terminal")),
            "actual first toolbar control receives focus"
        );
        assert_eq!(app.tabs[0].state.doc.text(), original);
        assert!(!app.tabs[0].state.doc.can_undo());
        assert_eq!(
            Keymap::default_keymap().lookup(crate::actions::KeyChord::parse("F6").unwrap()),
            Some(Action::MoveRefactoring)
        );
    }
    #[test]
    fn pending_tool_focus_cannot_steal_settings_or_retarget_a_reopened_pane() {
        let (_root, mut app, ctx) = fixture();
        app.handle_action(Action::ToggleProblemsView, &ctx);
        app.handle_action(Action::FocusToolWindowControls, &ctx);
        assert!(app.editor_accessibility.pending_tool_focus.is_some());
        app.handle_action(Action::Settings, &ctx);
        for time in [0.2, 0.3] {
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    focused: true,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 720.0))),
                    ..Default::default()
                },
                |ui| {
                    app.global_shortcuts(ui);
                    app.bottom_panel(ui);
                    egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
                    app.dialogs(ui.ctx());
                },
            );
            assert!(
                !output
                    .platform_output
                    .accesskit_update
                    .as_ref()
                    .unwrap()
                    .nodes
                    .iter()
                    .any(|(_, node)| node.role() == egui::accesskit::Role::Status
                        && node
                            .label()
                            .is_some_and(|s| s.starts_with("Problems tool window controls"))),
                "no background focus announcement under Settings"
            );
            output.drop_without_applying_deltas();
        }
        assert!(app.editor_accessibility.pending_tool_focus.is_none());
        let focus = ctx
            .memory(|m| m.focused())
            .expect("actual Settings search focus");
        assert_eq!(
            ctx.read_response(focus).unwrap().layer_id.id,
            egui::Id::new("Settings")
        );
        app.dialog = Dialog::None;
        app.handle_action(Action::FocusToolWindowControls, &ctx);
        assert!(app.editor_accessibility.pending_tool_focus.is_some());
        app.handle_action(Action::ToggleProblemsView, &ctx);
        assert!(
            app.editor_accessibility.pending_tool_focus.is_none(),
            "closing cancels the request"
        );
        app.handle_action(Action::ToggleTodoView, &ctx);
        assert!(
            app.editor_accessibility.pending_tool_focus.is_none(),
            "other pane inherits no request"
        );
    }
    #[test]
    fn hide_all_then_jump_last_does_not_revive_unpainted_tool_focus() {
        let (_root, mut app, ctx) = fixture();
        app.handle_action(Action::ToggleProblemsView, &ctx);
        app.handle_action(Action::FocusToolWindowControls, &ctx);
        assert!(app.editor_accessibility.pending_tool_focus.is_some());
        app.handle_action(Action::HideAllToolWindows, &ctx);
        assert!(app.bottom.is_none());
        assert!(app.editor_accessibility.pending_tool_focus.is_none());
        app.handle_action(Action::JumpToLastToolWindow, &ctx);
        assert!(app.editor_accessibility.pending_tool_focus.is_none());
    }
}
