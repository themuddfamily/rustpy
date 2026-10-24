//! Project/editor synchronization uses real selection changes, never steals focus.
use super::*;

#[derive(Default)]
pub(super) struct ProjectNavigationUi {
    root: Option<PathBuf>,
    source: Option<(egui::Id, PathBuf)>,
    selected: Option<PathBuf>,
}
impl IdeApp {
    /// One preview per destination group. A failed open must not discard its predecessor.
    pub(super) fn open_preview(&mut self, path: &Path) {
        let path = rustpy_core::vfs::normalize_path(path);
        if self
            .tabs
            .iter()
            .any(|t| t.state.path.as_ref() == Some(&path))
        {
            self.open_file(&path);
            return;
        }
        let group = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map_or(0, |t| t.group);
        let previous = self
            .tabs
            .iter()
            .find(|t| t.group == group && t.preview && !t.pinned && !t.has_unsaved_changes())
            .map(|t| t.view.id);
        self.tab_policy.explicit_preview = true;
        self.open_file(&path);
        self.tab_policy.explicit_preview = false;
        let Some(opened) = self
            .active
            .and_then(|i| self.tabs.get(i))
            .filter(|t| t.state.path.as_ref() == Some(&path))
            .map(|t| t.view.id)
        else {
            return;
        };
        if let Some(previous) = previous {
            if let Some(i) = self.tabs.iter().position(|t| t.view.id == previous) {
                self.close_tab(i);
            }
        }
        if let Some(i) = self.tabs.iter().position(|t| t.view.id == opened) {
            self.active = Some(i);
            self.tabs[i].preview = true;
        }
    }
    fn project_navigation_owner(&mut self) {
        let root = self.project.as_ref().map(|p| p.root.clone());
        if root != self.project_navigation.root {
            self.project_navigation = ProjectNavigationUi {
                root,
                ..Default::default()
            };
        }
    }
    /// Called after the normal active-editor/history observer, before painting the tree.
    pub(super) fn autoscroll_from_source(&mut self) {
        self.project_navigation_owner();
        for tab in &mut self.tabs {
            if tab.preview && (tab.pinned || tab.has_unsaved_changes()) {
                tab.preview = false;
            }
        }
        if !self.settings.editor.autoscroll_from_source
            && !self.settings.editor.project_view.always_select_opened
        {
            self.project_navigation.source = None;
            return;
        }
        let source = self
            .active
            .and_then(|i| self.tabs.get(i))
            .and_then(|t| t.state.path.clone().map(|path| (t.view.id, path)));
        if source == self.project_navigation.source {
            return;
        }
        self.project_navigation.source = source.clone();
        if let (Some((_, path)), Some(project)) = (source, self.project.as_mut()) {
            project.tree.reveal(&path);
            // A reveal from the editor is not a user request to open the tree's selection.
            self.project_navigation.selected = project.tree.selected.clone();
        }
    }
    /// Called after Tree::ui; arrow keys and speed search change real tree.selected.
    pub(super) fn autoscroll_to_source(&mut self, ctx: &egui::Context) {
        self.project_navigation_owner();
        let selected = self.project.as_ref().and_then(|p| p.tree.selected.clone());
        let changed = selected != self.project_navigation.selected;
        self.project_navigation.selected = selected.clone();
        if !changed
            || !self.settings.editor.autoscroll_to_source
            || !ProjectTree::has_focus(ctx)
            || !self.editor_keyboard()
            || self.nav.popup_visible()
            || self.intentions.is_some()
            || self.find_in_files.visible
            || self.coordinated_quit_active()
            || egui::Popup::is_any_open(ctx)
            || self.completion.visible
            || self.quick_doc.is_some()
            || self.inplace_rename.is_some()
        {
            return;
        }
        let Some(path) = selected.filter(|p| p.is_file()) else {
            return;
        };
        if self.settings.editor.open_in_preview_tab {
            self.open_preview(&path);
        } else {
            self.open_file(&path);
        }
        self.focus_editor = false;
    }
    pub(super) fn project_navigation_options(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Options", |ui| {
            let changed = ui
                .checkbox(
                    &mut self.settings.editor.open_in_preview_tab,
                    "Open in Preview Tab",
                )
                .changed()
                | ui.checkbox(
                    &mut self.settings.editor.autoscroll_from_source,
                    "Autoscroll from Source",
                )
                .changed()
                | ui.checkbox(
                    &mut self.settings.editor.autoscroll_to_source,
                    "Autoscroll to Source",
                )
                .changed();
            if changed {
                self.project_navigation.source = None;
                let _ = self.settings.save();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, IdeApp, egui::Context, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a_π.py");
        let b = dir.path().join("b_β.py");
        std::fs::write(&a, "π = 1\n").unwrap();
        std::fs::write(&b, "β = 2\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(dir.path());
        let mut app = IdeApp::new(settings, None);
        app.open_project(dir.path());
        assert!(matches!(app.dialog, Dialog::None));
        assert!(
            app.editor_keyboard(),
            "settled project must allow the real tree owner"
        );
        app.open_file(&a);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        (dir, app, ctx, a, b)
    }
    fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, key: Option<egui::Key>) {
        let mut events = vec![egui::Event::ModifiersChanged(egui::Modifiers::NONE)];
        if let Some(key) = key {
            for pressed in [true, false] {
                events.push(egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
        ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                focused: true,
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                ..Default::default()
            },
            |ui| {
                app.global_shortcuts(ui);
                app.observe_navigation();
                app.autoscroll_from_source();
                let keyboard = app.editor_keyboard();
                let project = app.project.as_mut().unwrap();
                project.tree.ui(ui, &app.theme, &app.keymap, keyboard);
                app.autoscroll_to_source(ui.ctx());
            },
        )
        .drop_without_applying_deltas();
    }
    #[test]
    fn actual_tree_keyboard_selection_autoscrolls_to_preview_without_taking_editor_focus() {
        let (_dir, mut app, ctx, a, b) = fixture();
        app.settings.editor.open_in_preview_tab = true;
        app.settings.editor.autoscroll_to_source = true;
        app.project.as_mut().unwrap().tree.reveal(&a);
        app.project.as_mut().unwrap().tree.request_focus();
        frame(&mut app, &ctx, 0.0, None);
        frame(&mut app, &ctx, 0.1, None);
        let version = app.tabs[app.active.unwrap()].state.doc.version();
        frame(&mut app, &ctx, 0.2, Some(egui::Key::ArrowDown));
        assert_eq!(
            app.project.as_ref().unwrap().tree.selected.as_ref(),
            Some(&b)
        );
        let tab = &app.tabs[app.active.unwrap()];
        assert_eq!(tab.state.path.as_ref(), Some(&b));
        assert!(tab.preview);
        assert!(!app.focus_editor);
        assert!(ProjectTree::has_focus(&ctx));
        assert_eq!(
            app.tabs
                .iter()
                .find(|t| t.state.path.as_ref() == Some(&a))
                .unwrap()
                .state
                .doc
                .version(),
            version
        );
        assert_eq!(std::fs::read_to_string(a).unwrap(), "π = 1\n");
        assert_eq!(std::fs::read_to_string(b).unwrap(), "β = 2\n");
    }
    #[test]
    fn actual_editor_switch_reveals_from_source_but_idle_respects_manual_tree_selection() {
        let (_dir, mut app, ctx, a, b) = fixture();
        app.settings.editor.autoscroll_from_source = true;
        frame(&mut app, &ctx, 0.0, None);
        assert_eq!(
            app.project.as_ref().unwrap().tree.selected.as_ref(),
            Some(&a)
        );
        app.open_file(&b);
        frame(&mut app, &ctx, 0.1, None);
        assert_eq!(
            app.project.as_ref().unwrap().tree.selected.as_ref(),
            Some(&b)
        );
        assert!(!ProjectTree::has_focus(&ctx));
        app.project.as_mut().unwrap().tree.selected = Some(a.clone());
        frame(&mut app, &ctx, 0.2, None);
        assert_eq!(
            app.project.as_ref().unwrap().tree.selected.as_ref(),
            Some(&a),
            "idle must not undo manual selection"
        );
        assert_eq!(app.tabs[app.active.unwrap()].state.path.as_ref(), Some(&b));
        app.settings.editor.autoscroll_from_source = false;
        app.open_file(&a);
        frame(&mut app, &ctx, 0.3, None);
        app.project.as_mut().unwrap().tree.selected = Some(b.clone());
        frame(&mut app, &ctx, 0.4, None);
        assert_eq!(
            app.project.as_ref().unwrap().tree.selected.as_ref(),
            Some(&b)
        );
    }
    #[test]
    fn preview_promotes_unsaved_and_pinned_tabs_across_groups_without_editing_history() {
        let (_dir, mut app, _ctx, a, b) = fixture();
        app.open_preview(&b);
        let i = app.active.unwrap();
        app.tabs[i].preview = true;
        app.tabs[i]
            .state
            .insert_text("🙂", &crate::editor::EditOptions::default());
        let version = app.tabs[i].state.doc.version();
        app.active = app
            .tabs
            .iter()
            .position(|t| t.state.path.as_ref() == Some(&a));
        app.autoscroll_from_source();
        let dirty = app
            .tabs
            .iter()
            .find(|t| t.state.path.as_ref() == Some(&b))
            .unwrap();
        assert!(!dirty.preview);
        assert_eq!(dirty.state.doc.version(), version);
        assert!(dirty.state.doc.can_undo());
        let i = app.active.unwrap();
        app.tabs[i].preview = true;
        app.tabs[i].pinned = true;
        app.autoscroll_from_source();
        assert!(!app.tabs[i].preview);
    }
    #[test]
    fn autoscroll_defaults_and_serde_roundtrip_preserve_opt_in_policy() {
        let mut opts: rustpy_settings::EditorSettings = serde_json::from_str("{}").unwrap();
        assert!(!opts.autoscroll_from_source);
        assert!(!opts.autoscroll_to_source);
        opts.autoscroll_from_source = true;
        opts.autoscroll_to_source = true;
        let loaded: rustpy_settings::EditorSettings =
            serde_json::from_str(&serde_json::to_string(&opts).unwrap()).unwrap();
        assert!(loaded.autoscroll_from_source && loaded.autoscroll_to_source);
    }
    #[test]
    fn actual_tree_context_menu_selection_does_not_autoscroll_behind_menu() {
        let (_dir, mut app, ctx, a, b) = fixture();
        app.settings.editor.open_in_preview_tab = true;
        app.settings.editor.autoscroll_to_source = true;
        app.project.as_mut().unwrap().tree.reveal(&a);
        let host = app.tabs[app.active.unwrap()].view.id;
        let version = app.tabs[app.active.unwrap()].state.doc.version();
        let carets = app.tabs[app.active.unwrap()].state.carets.clone();
        let mut render = |time, events| {
            ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    focused: true,
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1400.0, 900.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    app.global_shortcuts(ui);
                    app.observe_navigation();
                    app.autoscroll_from_source();
                    let keyboard = app.editor_keyboard();
                    app.project
                        .as_mut()
                        .unwrap()
                        .tree
                        .ui(ui, &app.theme, &app.keymap, keyboard);
                    app.autoscroll_to_source(ui.ctx());
                },
            )
        };
        render(0.0, vec![]).drop_without_applying_deltas();
        let output = render(0.1, vec![]);
        let point = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "b_β.py" => {
                    Some(text.pos + egui::vec2(5.0, 5.0))
                }
                _ => None,
            })
            .expect("derive pointer from the actual painted filename");
        output.drop_without_applying_deltas();
        for (time, pressed) in [(0.2, true), (0.3, false)] {
            render(
                time,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Secondary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .drop_without_applying_deltas();
        }
        render(0.4, vec![]).drop_without_applying_deltas();
        assert!(
            egui::Popup::is_any_open(&ctx),
            "real secondary click opens the tree context menu"
        );
        assert!(ProjectTree::has_focus(&ctx));
        assert_eq!(
            app.project.as_ref().unwrap().tree.selected.as_ref(),
            Some(&b),
            "actual menu selects its target"
        );
        assert_eq!(app.tabs.len(), 1, "must not open a preview behind the menu");
        let tab = &app.tabs[app.active.unwrap()];
        assert_eq!(tab.view.id, host);
        assert_eq!(tab.state.path.as_ref(), Some(&a));
        assert_eq!(tab.state.doc.text(), "π = 1\n");
        assert_eq!(tab.state.doc.version(), version);
        assert_eq!(tab.state.carets, carets);
        assert!(!tab.preview);
        assert!(!tab.state.doc.can_undo());
        assert_eq!(std::fs::read_to_string(a).unwrap(), "π = 1\n");
        assert_eq!(std::fs::read_to_string(b).unwrap(), "β = 2\n");
    }
}
