//! Clipboard values belong to the clicked target, never an unrelated active tab.
use super::*;

#[derive(Clone, Debug)]
pub(super) struct CopyTarget {
    path: PathBuf,
    line: Option<usize>,
    definitions: Vec<String>,
}
impl CopyTarget {
    pub(super) fn path(path: PathBuf) -> Self {
        let line = path.is_file().then_some(1);
        Self {
            path,
            line,
            definitions: Vec::new(),
        }
    }
}

/// Component-aware matching: `/project/src2` is not inside `/project/src`.
fn deepest_root<'a>(path: &Path, roots: impl Iterator<Item = &'a PathBuf>) -> Option<&'a PathBuf> {
    roots
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
}
fn path_items(
    target: &CopyTarget,
    project: Option<&Path>,
    content_roots: &[PathBuf],
    source_roots: &[PathBuf],
    excluded: &[PathBuf],
) -> Vec<(String, String)> {
    let path = &target.path;
    let mut items = vec![("Absolute Path".into(), path.display().to_string())];
    if let Some(project) = project {
        if let Ok(relative) = path.strip_prefix(project) {
            items.push(("Relative Path".into(), relative.display().to_string()));
        }
        // Excluded files still have ordinary paths, but are not source/content context.
        if !excluded.iter().any(|root| path.starts_with(root)) {
            let roots: Vec<_> = std::iter::once(project.to_path_buf())
                .chain(content_roots.iter().cloned())
                .collect();
            if let Some(root) = deepest_root(path, roots.iter()) {
                items.push((
                    "Path From Content Root".into(),
                    path.strip_prefix(root).unwrap().display().to_string(),
                ));
            }
            if let Some(root) = deepest_root(path, source_roots.iter()) {
                items.push((
                    "Path From Source Root".into(),
                    path.strip_prefix(root).unwrap().display().to_string(),
                ));
            }
        }
    }
    if let Some(line) = target.line {
        items.push((
            "Path With Line Number".into(),
            format!("{}:{line}", path.display()),
        ));
    }
    items.push((
        "File Name".into(),
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
    ));
    items
}

impl IdeApp {
    /// Inactive splits and detached views keep their existing clipboard handling.
    /// Only target-bound copy actions are added; all unrelated actions stay untouched.
    pub(super) fn record_auxiliary_editor_events(&mut self, index: usize, events: &[ViewEvent]) {
        self.record_clipboard_events(events);
        for event in events {
            if let ViewEvent::Action(action @ (Action::CopyPath | Action::CopyReference)) = event {
                self.copy_action_for_tab(*action, index);
            }
        }
    }
    pub(super) fn copy_action_for_tab(&mut self, action: Action, index: usize) -> bool {
        if !matches!(action, Action::CopyPath | Action::CopyReference) {
            return false;
        }
        if let Some(target) = self.copy_target_for_tab(index) {
            match action {
                Action::CopyPath => self.open_copy_path_target(target),
                Action::CopyReference => self.copy_reference_target(target),
                _ => unreachable!(),
            }
        }
        true
    }
    pub(super) fn copy_target_for_tab(&mut self, index: usize) -> Option<CopyTarget> {
        let tab = self.tabs.get_mut(index)?;
        let path = tab.state.path.clone()?;
        let line = tab.state.doc.line_of(tab.state.primary().head()) + 1;
        let definitions = if tab.state.language == Language::Python {
            tab.state
                .breadcrumbs()
                .into_iter()
                .map(|(name, _)| name.trim_end_matches("()").to_owned())
                .collect()
        } else {
            Vec::new()
        };
        Some(CopyTarget {
            path,
            line: Some(line),
            definitions,
        })
    }
    fn qualified_copy_reference(&self, target: &CopyTarget) -> Option<String> {
        if target.line.is_none()
            || !matches!(
                target
                    .path
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("py" | "pyi")
            )
        {
            return None;
        }
        let project = self.project.as_ref()?;
        if project
            .tree
            .excluded
            .iter()
            .any(|root| target.path.starts_with(root))
        {
            return None;
        }
        let mut reference = code_insight::reference_module(
            &project.analysis.resolver,
            &project.root,
            &target.path,
        )?;
        if reference.is_empty() {
            return None;
        }
        for name in &target.definitions {
            reference.push('.');
            reference.push_str(name);
        }
        Some(reference)
    }
    fn copy_path_items(&self, target: &CopyTarget) -> Vec<(String, String)> {
        let (project, contents, sources, excluded) = match &self.project {
            Some(project) => (
                Some(project.root.as_path()),
                project.tree.content_roots.clone(),
                project.tree.source_roots.iter().cloned().collect(),
                project.tree.excluded.iter().cloned().collect(),
            ),
            None => (None, Vec::new(), Vec::new(), Vec::new()),
        };
        let mut items = path_items(target, project, &contents, &sources, &excluded);
        if let Some(reference) = self.qualified_copy_reference(target) {
            items.push(("Reference".into(), reference));
        }
        items
    }
    pub(super) fn flush_pending_copy(&mut self, ctx: &egui::Context) {
        if let Some(text) = self.pending_copy.take() {
            ctx.copy_text(text);
        }
    }
    fn copy_target_in_context(&mut self, ctx: &egui::Context) -> Option<CopyTarget> {
        if ProjectTree::has_focus(ctx) {
            return self
                .project
                .as_ref()?
                .tree
                .selected
                .clone()
                .map(CopyTarget::path);
        }
        self.active
            .and_then(|index| self.copy_target_for_tab(index))
    }
    pub(super) fn open_copy_path_in_context(&mut self, ctx: &egui::Context) {
        if let Some(target) = self.copy_target_in_context(ctx) {
            self.open_copy_path_target(target);
        }
    }
    pub(super) fn open_copy_path_target(&mut self, target: CopyTarget) {
        self.choice = Some(ChoicePopup::new(
            ChoiceKind::CopyPath,
            "Copy Path / Reference".into(),
            self.copy_path_items(&target),
        ));
    }
    pub(super) fn copy_reference(&mut self, ctx: &egui::Context) {
        if let Some(target) = self.copy_target_in_context(ctx) {
            self.copy_reference_target(target);
        }
    }
    pub(super) fn copy_reference_target(&mut self, target: CopyTarget) {
        if let Some(reference) = self.qualified_copy_reference(&target) {
            self.set_status(format!("Copied: {reference}"));
            self.pending_copy = Some(reference);
        } else {
            // Never replace the clipboard with an invented module or empty string.
            self.set_status("No Python reference is available for this file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn value(items: &[(String, String)], label: &str) -> Option<String> {
        items
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, value)| value.clone())
    }
    #[test]
    fn unicode_nested_multiple_roots_and_exclusions_have_distinct_values() {
        let target = CopyTarget {
            path: "/p/extra/src/深/é.py".into(),
            line: Some(12),
            definitions: Vec::new(),
        };
        let items = path_items(
            &target,
            Some(Path::new("/p")),
            &["/p/extra".into()],
            &["/p/extra/src".into(), "/p/extra/src/深".into()],
            &[],
        );
        assert_eq!(
            value(&items, "Relative Path").as_deref(),
            Some("extra/src/深/é.py")
        );
        assert_eq!(
            value(&items, "Path From Content Root").as_deref(),
            Some("src/深/é.py")
        );
        assert_eq!(
            value(&items, "Path From Source Root").as_deref(),
            Some("é.py")
        );
        assert_eq!(
            value(&items, "Path With Line Number").as_deref(),
            Some("/p/extra/src/深/é.py:12")
        );
        let excluded = path_items(
            &target,
            Some(Path::new("/p")),
            &[],
            &["/p/extra/src".into()],
            &["/p/extra/src/深".into()],
        );
        assert!(value(&excluded, "Path From Source Root").is_none());
        assert!(value(&excluded, "Path From Content Root").is_none());
        assert!(value(&excluded, "Relative Path").is_some());
    }
    #[test]
    fn external_directory_and_unconfigured_sources_do_not_invent_context() {
        let outside = path_items(
            &CopyTarget::path("/elsewhere/space dir".into()),
            Some(Path::new("/p")),
            &[],
            &["/p/src".into()],
            &[],
        );
        assert_eq!(outside.len(), 2);
        assert_eq!(value(&outside, "File Name").as_deref(), Some("space dir"));
        let under = path_items(
            &CopyTarget::path("/p/src2/a.py".into()),
            Some(Path::new("/p")),
            &[],
            &["/p/src".into()],
            &[],
        );
        assert!(value(&under, "Path From Source Root").is_none());
        assert!(value(&under, "Path With Line Number").is_none());
    }
    /// `opened_editor_index` intentionally checks only the active navigation target.
    /// These tests also need an already installed background editor without activating it.
    fn installed_index(app: &IdeApp, path: &Path) -> usize {
        let normalized = rustpy_core::vfs::normalize_path(path);
        app.tabs
            .iter()
            .position(|tab| tab.state.path.as_deref() == Some(normalized.as_path()))
            .expect("real file open must install the requested editor")
    }
    fn app() -> (tempfile::TempDir, IdeApp, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("module.py");
        let second = root.path().join("other.py");
        std::fs::write(
            &first,
            "class Sample:\n    def method(self):\n        return 'é'\n",
        )
        .unwrap();
        std::fs::write(&second, "def elsewhere():\n    pass\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&first);
        app.open_file(&second);
        // Small text files use the real synchronous VFS/Document open path. Verify
        // both installed editors; retain the second as active for background-target tests.
        for path in [&first, &second] {
            let index = installed_index(&app, path);
            assert_eq!(
                app.tabs[index].state.doc.text(),
                std::fs::read_to_string(path).unwrap()
            );
            assert!(!app.tabs[index].state.doc.is_modified());
        }
        assert_eq!(app.active, Some(installed_index(&app, &second)));
        (root, app, first, second)
    }
    #[test]
    fn qualified_reference_uses_deepest_configured_root_and_package_init() {
        let (root, mut app, _, _) = app();
        let outer = root.path().join("extra");
        let source = outer.join("src");
        let package = source.join("包");
        std::fs::create_dir_all(&package).unwrap();
        let file = package.join("é.py");
        let init = package.join("__init__.py");
        std::fs::write(&file, "class Café:\n    def méthode(self):\n        pass\n").unwrap();
        std::fs::write(&init, "").unwrap();
        let project = app.project.as_mut().unwrap();
        project.tree.content_roots.push(outer.clone());
        project.tree.source_roots.insert(source.clone());
        project.analysis = Arc::new(Analysis::new(code_insight::resolver_for(
            vec![project.root.clone(), outer, source],
            None,
        )));
        app.open_file(&file);
        let index = installed_index(&app, &file);
        app.tabs[index].state.go_to_line(3, Some(9));
        let target = app.copy_target_for_tab(index).unwrap();
        assert_eq!(
            app.qualified_copy_reference(&target).as_deref(),
            Some("包.é.Café.méthode")
        );
        assert_eq!(
            app.qualified_copy_reference(&CopyTarget::path(init))
                .as_deref(),
            Some("包")
        );
        app.project.as_mut().unwrap().tree.excluded.insert(package);
        assert!(app.qualified_copy_reference(&target).is_none());
        assert!(value(&app.copy_path_items(&target), "Relative Path").is_some());
    }

    #[test]
    fn direct_action_copies_module_class_method_without_popup_or_document_change() {
        let (_root, mut app, first, _) = app();
        let index = installed_index(&app, &first);
        app.active = Some(index);
        app.tabs[index].state.go_to_line(3, Some(9));
        let before = app.tabs[index].state.doc.text();
        app.handle_action(Action::CopyReference, &egui::Context::default());
        assert_eq!(app.pending_copy.as_deref(), Some("module.Sample.method"));
        assert!(app.choice.is_none());
        assert_eq!(app.tabs[index].state.doc.text(), before);
        assert!(!app.tabs[index].state.doc.is_modified());
    }
    #[test]
    fn auxiliary_consumer_preserves_clipboard_events_and_ignores_unrelated_actions() {
        let (_root, mut app, first, _) = app();
        let index = installed_index(&app, &first);
        app.tabs[index].state.go_to_line(3, Some(9));
        let active = app.active;
        let original = app.tabs[index].state.doc.text();
        app.record_auxiliary_editor_events(
            index,
            &[
                ViewEvent::Copied("ordinary copied text".into()),
                ViewEvent::Action(Action::SelectAll),
                ViewEvent::Action(Action::CopyReference),
            ],
        );
        assert_eq!(app.clipboard_history, vec!["ordinary copied text"]);
        assert_eq!(app.pending_copy.as_deref(), Some("module.Sample.method"));
        assert_eq!(app.active, active);
        assert!(!app.tabs[index].state.primary().has_selection());
        assert_eq!(app.tabs[index].state.doc.text(), original);
        app.record_auxiliary_editor_events(index, &[ViewEvent::Action(Action::CopyPath)]);
        assert_eq!(
            value(&app.choice.as_ref().unwrap().items, "Reference").as_deref(),
            Some("module.Sample.method")
        );
    }

    #[test]
    fn inactive_split_context_menu_frame_opens_clicked_editor_path_choices() {
        let (_root, mut app, first, second) = app();
        let index = installed_index(&app, &first);
        app.tabs[index].group = 1;
        app.tabs[index].state.go_to_line(3, Some(9));
        let active = app.active;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let frame = |app: &mut IdeApp, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0))),
                    events,
                    ..Default::default()
                },
                |ui| app.group_area(ui, 1),
            )
        };
        // Derive the editor location from its actual rendered caret, not fixed menu geometry.
        let initial = frame(&mut app, vec![]);
        initial.drop_without_applying_deltas();
        let pointer = app.tabs[index].view.caret_screen_rect.unwrap().center();
        let event = |pos, button, pressed| egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            &mut app,
            vec![
                egui::Event::PointerMoved(pointer),
                event(pointer, egui::PointerButton::Secondary, true),
            ],
        )
        .drop_without_applying_deltas();
        frame(
            &mut app,
            vec![event(pointer, egui::PointerButton::Secondary, false)],
        )
        .drop_without_applying_deltas();
        let menu = frame(&mut app, vec![]);
        let copy_position = menu
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == Action::CopyPath.text() => {
                    Some(text.pos + text.galley.rect.center().to_vec2())
                }
                _ => None,
            })
            .expect("actual split context menu must paint Copy Path");
        menu.drop_without_applying_deltas();
        frame(
            &mut app,
            vec![
                egui::Event::PointerMoved(copy_position),
                event(copy_position, egui::PointerButton::Primary, true),
            ],
        )
        .drop_without_applying_deltas();
        frame(
            &mut app,
            vec![event(copy_position, egui::PointerButton::Primary, false)],
        )
        .drop_without_applying_deltas();
        let items = &app
            .choice
            .as_ref()
            .expect("actual view event reaches the App consumer")
            .items;
        assert_eq!(
            value(items, "Absolute Path"),
            Some(first.display().to_string())
        );
        assert!(value(items, "Reference")
            .is_some_and(|reference| reference.starts_with("module.Sample")));
        assert_eq!(app.active, active);
        assert_eq!(
            app.tabs[active.unwrap()].state.path.as_deref(),
            Some(second.as_path())
        );
    }

    #[test]
    fn clicked_editor_action_uses_its_explicit_index_even_with_other_focus() {
        let (_root, mut app, first, second) = app();
        let index = installed_index(&app, &first);
        app.tabs[index].state.go_to_line(3, Some(9));
        let active = app.active;
        assert!(app.copy_action_for_tab(Action::CopyReference, index));
        assert_eq!(app.pending_copy.as_deref(), Some("module.Sample.method"));
        assert!(app.copy_action_for_tab(Action::CopyPath, index));
        assert_eq!(
            value(&app.choice.as_ref().unwrap().items, "Reference").as_deref(),
            Some("module.Sample.method")
        );
        assert!(!app.copy_action_for_tab(Action::Save, index));
        assert_eq!(app.active, active);
        assert_eq!(
            app.tabs[active.unwrap()].state.path.as_deref(),
            Some(second.as_path())
        );
    }

    #[test]
    fn inactive_tab_and_project_target_do_not_borrow_active_reference_or_line() {
        let (_root, mut app, first, second) = app();
        let index = installed_index(&app, &first);
        app.tabs[index].state.go_to_line(3, Some(9));
        let active = app.active;
        let tab = app.copy_target_for_tab(index).unwrap();
        let items = app.copy_path_items(&tab);
        assert_eq!(
            value(&items, "Reference").as_deref(),
            Some("module.Sample.method")
        );
        assert_eq!(
            value(&items, "Path With Line Number"),
            Some(format!("{}:3", first.display()))
        );
        let tree = app.copy_path_items(&CopyTarget::path(first.clone()));
        assert_eq!(value(&tree, "Reference").as_deref(), Some("module"));
        assert_eq!(
            value(&tree, "Path With Line Number"),
            Some(format!("{}:1", first.display()))
        );
        assert_eq!(
            app.tabs[app.active.unwrap()].state.path.as_deref(),
            Some(second.as_path())
        );
        assert_eq!(app.active, active);
    }
    #[test]
    fn chooser_uses_clipboard_handoff_and_no_editor_required_for_project_target() {
        let (_root, mut app, first, _) = app();
        app.active = None;
        app.open_copy_path_target(CopyTarget::path(first.clone()));
        let choice = app.choice.as_ref().unwrap();
        assert_eq!(choice.items[0].1, first.display().to_string());
        let selected = choice
            .items
            .iter()
            .position(|(name, _)| name == "File Name")
            .unwrap();
        app.apply_choice(selected);
        assert_eq!(app.pending_copy.as_deref(), Some("module.py"));
    }

    fn clipboard_frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> Vec<String> {
        let mut raw = egui::RawInput {
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0))),
            ..Default::default()
        };
        <IdeApp as eframe::App>::raw_input_hook(app, ctx, &mut raw);
        let output = ctx.run_ui(raw, |ui| {
            app.global_shortcuts(ui);
            app.flush_pending_copy(ui.ctx());
        });
        let copied = output
            .platform_output
            .commands
            .iter()
            .filter_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        output.drop_without_applying_deltas();
        copied
    }
    #[test]
    fn desktop_copy_transport_preserves_modified_reference_shortcut() {
        let (_root, mut app, first, _) = app();
        let index = installed_index(&app, &first);
        app.active = Some(index);
        app.tabs[index].state.go_to_line(3, Some(9));
        let original = app.tabs[index].state.doc.text().to_string();
        let ctx = egui::Context::default();
        let modifiers = egui::Modifiers::CTRL
            | egui::Modifiers::ALT
            | egui::Modifiers::SHIFT
            | egui::Modifiers::COMMAND;
        // egui-winit emits Copy for every command+C chord. A release can be
        // coalesced into this frame, leaving RawInput.modifiers empty.
        let copied = clipboard_frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::Copy,
                egui::Event::ModifiersChanged(egui::Modifiers::NONE),
            ],
        );
        assert_eq!(copied, vec!["module.Sample.method"]);
        assert!(app.choice.is_none());
        assert_eq!(app.active, Some(index));
        assert_eq!(app.tabs[index].state.doc.text().to_string(), original);
    }

    #[test]
    fn actual_shortcut_frame_writes_qualified_reference_to_platform_clipboard() {
        let (_root, mut app, first, _) = app();
        let index = installed_index(&app, &first);
        app.active = Some(index);
        app.tabs[index].state.go_to_line(3, Some(9));
        let history = app.clipboard_history.clone();
        let ctx = egui::Context::default();
        let modifiers = egui::Modifiers::CTRL | egui::Modifiers::ALT | egui::Modifiers::SHIFT;
        let copied = clipboard_frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::C,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
        );
        assert_eq!(copied, vec!["module.Sample.method"]);
        assert!(app.choice.is_none());
        assert_eq!(
            app.clipboard_history, history,
            "path/reference copies preserve existing history policy"
        );
    }
    #[test]
    fn every_popup_variant_is_delivered_to_actual_clipboard_without_retargeting() {
        let (_root, mut app, first, _) = app();
        let index = installed_index(&app, &first);
        app.tabs[index].state.go_to_line(3, Some(9));
        let project = app.project.as_mut().unwrap();
        project.tree.source_roots.insert(project.root.clone());
        let target = app.copy_target_for_tab(index).unwrap();
        let items = app.copy_path_items(&target);
        assert_eq!(
            items
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Absolute Path",
                "Relative Path",
                "Path From Content Root",
                "Path From Source Root",
                "Path With Line Number",
                "File Name",
                "Reference"
            ]
        );
        let ctx = egui::Context::default();
        for (index, (_, expected)) in items.iter().enumerate() {
            app.open_copy_path_target(target.clone());
            app.apply_choice(index);
            assert_eq!(
                clipboard_frame(&mut app, &ctx, vec![]),
                vec![expected.clone()]
            );
        }
    }
    #[test]
    fn unicode_definition_reference_and_unavailable_context_preserve_clipboard() {
        let (_root, mut app, first, _) = app();
        let index = installed_index(&app, &first);
        app.tabs[index]
            .state
            .doc
            .reload_from(rustpy_text::Document::new(
                "class Café:\n    def méthode(self):\n        pass\n",
            ));
        app.tabs[index].state.go_to_line(3, Some(9));
        let target = app.copy_target_for_tab(index).unwrap();
        app.copy_reference_target(target);
        assert_eq!(app.pending_copy.as_deref(), Some("module.Café.méthode"));
        app.copy_reference_target(CopyTarget::path("/external/a.txt".into()));
        assert_eq!(app.pending_copy.as_deref(), Some("module.Café.méthode"));
        assert!(app.choice.is_none());
    }
    #[test]
    fn focused_project_keyboard_targets_selected_file_without_an_editor() {
        let (_root, mut app, first, _) = app();
        app.active = None;
        app.project.as_mut().unwrap().tree.selected = Some(first);
        let ctx = egui::Context::default();
        ctx.memory_mut(|memory| memory.request_focus(ProjectTree::focus_id()));
        app.copy_reference(&ctx);
        assert_eq!(clipboard_frame(&mut app, &ctx, vec![]), vec!["module"]);
    }
}
