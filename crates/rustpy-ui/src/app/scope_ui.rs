//! Shared captured scope inputs and the Scopes settings page.
use super::*;
use crate::a11y::AccessibleCombo;
use rustpy_settings::scopes::{
    apply_tree_action, NamedScope, PredefinedScope, ScopeContext, ScopeSelection, ScopeSettings,
    TreeAction,
};
use std::collections::BTreeSet;
use std::ops::Range;
/// Project files shown in the scope editor tree.
const TREE_FILE_LIMIT: usize = 20_000;
#[derive(Default)]
pub(super) struct ScopeUi {
    editing: Option<usize>,
    name: String,
    pattern: String,
    color: bool,
    rgb: [u8; 3],
    shared: bool,
    message: String,
    /// Selected tree node: project-relative path and whether it is a directory.
    selected: Option<(String, bool)>,
    expanded: BTreeSet<String>,
    only_matching: bool,
    preview: Option<ScopePreview>,
}
/// Tree and match state for one pattern over one captured file catalogue,
/// recomputed only when either changes.
struct ScopePreview {
    pattern: String,
    catalogue: Arc<Vec<PathBuf>>,
    rows: Vec<TreeRow>,
    files: usize,
    matched: usize,
    error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct TreeRow {
    depth: usize,
    name: String,
    /// Project-relative, `/`-separated; empty for the project root.
    relative: String,
    directory: bool,
    /// Range of the row's files in the root-relative file list.
    files: Range<usize>,
    matched: usize,
}
/// Directory rows (preceded by the project root) and file rows in tree order
/// for the sorted catalogue; files outside `root` are left out. Sorted paths
/// keep each directory's files contiguous.
fn tree_rows(root: &Path, catalogue: &[PathBuf]) -> (Vec<TreeRow>, Vec<PathBuf>) {
    let mut rows = vec![TreeRow {
        depth: 0,
        name: root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Project".into()),
        relative: String::new(),
        directory: true,
        files: 0..0,
        matched: 0,
    }];
    let mut files = Vec::new();
    let mut open: Vec<(String, usize)> = Vec::new();
    for path in catalogue {
        if files.len() >= TREE_FILE_LIMIT {
            break;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let parts: Vec<String> = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();
        let Some((name, directories)) = parts.split_last() else {
            continue;
        };
        let index = files.len();
        let common = open
            .iter()
            .zip(directories)
            .take_while(|((open, _), directory)| open == *directory)
            .count();
        for (_, row) in open.drain(common..) {
            rows[row].files.end = index;
        }
        for directory in &directories[common..] {
            rows.push(TreeRow {
                depth: open.len() + 1,
                name: directory.clone(),
                relative: open
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .chain([directory.as_str()])
                    .collect::<Vec<_>>()
                    .join("/"),
                directory: true,
                files: index..index,
                matched: 0,
            });
            open.push((directory.clone(), rows.len() - 1));
        }
        rows.push(TreeRow {
            depth: open.len() + 1,
            name: name.clone(),
            relative: parts.join("/"),
            directory: false,
            files: index..index + 1,
            matched: 0,
        });
        files.push(path.clone());
    }
    for (_, row) in open {
        rows[row].files.end = files.len();
    }
    rows[0].files = 0..files.len();
    (rows, files)
}
/// One selector, shared by consumers rather than separate scope parsers.
pub(super) fn scope_selector(
    ui: &mut egui::Ui,
    selection: &mut ScopeSelection,
    names: &[String],
    id: &str,
) -> bool {
    let before = selection.clone();
    egui::ComboBox::from_id_salt(id)
        .selected_text(match selection {
            ScopeSelection::Predefined(scope) => scope.label().into(),
            ScopeSelection::Named(name) => name.clone(),
            ScopeSelection::Custom(_) => "Custom pattern".into(),
        })
        .show_ui(ui, |ui| {
            for scope in PredefinedScope::ALL {
                ui.selectable_value(selection, ScopeSelection::Predefined(*scope), scope.label());
            }
            for name in names {
                ui.selectable_value(selection, ScopeSelection::Named(name.clone()), name);
            }
        })
        .combo_named(crate::i18n::tr_static("Scope"));
    before != *selection
}
impl IdeApp {
    pub(super) fn capture_scope_context(&self) -> ScopeContext {
        let project_scope = self.project_scope();
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        let project_files = project_scope
            .as_ref()
            .and_then(|scope| self.project_index.files.files_for(scope))
            .unwrap_or_default();
        let open_files = self
            .tabs
            .iter()
            .filter_map(|tab| tab.state.path.clone())
            .collect();
        let mut changed_files: BTreeSet<_> = self
            .git_panel
            .entries
            .iter()
            .filter(|entry| {
                !matches!(
                    entry.status,
                    rustpy_vcs::FileStatus::Ignored | rustpy_vcs::FileStatus::Unmodified
                )
            })
            .map(|entry| entry.path.clone())
            .collect();
        changed_files.extend(
            self.tabs
                .iter()
                .filter(|tab| tab.has_unsaved_changes())
                .filter_map(|tab| tab.state.path.clone()),
        );
        let recently_changed = self
            .nav
            .history
            .changed_places()
            .into_iter()
            .map(|point| point.path)
            .collect();
        let mut modules = std::collections::BTreeMap::new();
        let mut ambiguous_modules = BTreeSet::new();
        if let Some(scope) = &project_scope {
            for path in scope.walk_roots() {
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    if ambiguous_modules.contains(name) {
                        continue;
                    }
                    if modules.get(name).is_some_and(|previous| previous != &path) {
                        modules.remove(name);
                        ambiguous_modules.insert(name.to_string());
                    } else {
                        modules.insert(name.to_string(), path);
                    }
                }
            }
        }
        ScopeContext {
            root,
            project_files,
            test_roots: self
                .project
                .as_ref()
                .map(|project| project.tree.test_roots.iter().cloned().collect())
                .unwrap_or_default(),
            open_files,
            changed_files,
            current_file: self
                .active
                .and_then(|index| self.tabs.get(index))
                .and_then(|tab| tab.state.path.clone()),
            recently_viewed: self.recent_files.iter().cloned().collect(),
            recently_changed,
            scratch_roots: vec![crate::scratches::scratch_dir()],
            modules,
        }
    }
    pub(super) fn scope_names(&self) -> Vec<String> {
        self.settings
            .scopes
            .named
            .iter()
            .map(|scope| scope.name.clone())
            .collect()
    }
    pub(super) fn scope_settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Scopes and File Colors");
        ui.label(
            "Patterns: file:src/*//*.py&&!file:*/test* · file:/setup.py (root only) · && / || / ! / parentheses · $NamedScope",
        );
        let names = self.scope_names();
        ui.horizontal(|ui| {
            ui.label("TODO:");
            scope_selector(
                ui,
                &mut self.settings.scopes.todo,
                &names,
                "todo-default-scope",
            );
        });
        ui.horizontal(|ui| {
            ui.label("Inspections:");
            scope_selector(
                ui,
                &mut self.settings.scopes.inspections,
                &names,
                "inspection-default-scope",
            );
        });
        ui.horizontal(|ui| {
            ui.label("Project View:");
            scope_selector(
                ui,
                &mut self.settings.scopes.project_view,
                &names,
                "project-default-scope",
            );
        });
        ui.checkbox(
            &mut self.settings.scopes.file_colors_enabled,
            "Use named-scope file colors in tabs and Project View (last matching color wins)",
        );
        let mut choose = None;
        let mut remove = None;
        let mut swap = None;
        for (index, scope) in self.settings.scopes.named.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.scope_ui.editing == Some(index), &scope.name)
                    .clicked()
                {
                    choose = Some(index);
                }
                ui.label(
                    egui::RichText::new(if scope.shared { "Shared" } else { "Local" })
                        .small()
                        .color(self.theme.text_dim),
                );
                ui.label(&scope.pattern);
                if ui.small_button("↑").clicked() && index > 0 {
                    swap = Some((index, index - 1));
                }
                if ui.small_button("↓").clicked() && index + 1 < self.settings.scopes.named.len()
                {
                    swap = Some((index, index + 1));
                }
                if ui.small_button("Delete").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some((a, b)) = swap {
            self.settings.scopes.named.swap(a, b);
            self.scope_ui.editing = None;
        }
        if let Some(index) = choose {
            let scope = &self.settings.scopes.named[index];
            self.scope_ui.editing = Some(index);
            self.scope_ui.name = scope.name.clone();
            self.scope_ui.pattern = scope.pattern.clone();
            self.scope_ui.color = scope.color.is_some();
            self.scope_ui.rgb = scope.color.unwrap_or([80, 100, 140]);
            self.scope_ui.shared = scope.shared;
        }
        if let Some(index) = remove {
            let mut candidate = self.settings.scopes.clone();
            candidate.named.remove(index);
            match validate(&candidate) {
                Ok(()) => {
                    self.settings.scopes = candidate;
                    self.scope_ui.editing = None;
                }
                Err(error) => self.scope_ui.message = error,
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.text_edit_singleline(&mut self.scope_ui.name);
        });
        ui.add(
            egui::TextEdit::multiline(&mut self.scope_ui.pattern)
                .desired_rows(3)
                .hint_text("file:src//*&&!file:*test*"),
        );
        self.scope_preview_tree(ui);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.scope_ui.color, "File color");
            if self.scope_ui.color {
                ui.color_edit_button_srgb(&mut self.scope_ui.rgb);
            }
            ui.add_enabled(
                self.project.is_some(),
                egui::Checkbox::new(&mut self.scope_ui.shared, "Share through VCS"),
            )
            .on_hover_text(
                "Shared scopes are stored in the project's .rustpy/scopes.json; \
                 local ones in the IDE settings",
            );
        });
        ui.horizontal(|ui| {
            if ui.button("New scope").clicked() {
                self.scope_ui = ScopeUi {
                    expanded: std::mem::take(&mut self.scope_ui.expanded),
                    ..ScopeUi::default()
                };
            }
            if ui.button("Save scope").clicked() {
                let name = self.scope_ui.name.trim().to_string();
                let mut candidate = self.settings.scopes.clone();
                let scope = NamedScope {
                    name: name.clone(),
                    pattern: self.scope_ui.pattern.clone(),
                    color: self.scope_ui.color.then_some(self.scope_ui.rgb),
                    shared: self.scope_ui.shared && self.project.is_some(),
                };
                if let Some(index) = self.scope_ui.editing {
                    if index < candidate.named.len() {
                        candidate.named[index] = scope;
                    }
                } else {
                    candidate.named.push(scope);
                    self.scope_ui.editing = Some(candidate.named.len() - 1);
                }
                match validate(&candidate) {
                    Ok(()) => {
                        self.settings.scopes = candidate;
                        self.scope_ui.message = format!("Saved {name}");
                    }
                    Err(error) => self.scope_ui.message = error,
                }
            }
        });
        if !self.scope_ui.message.is_empty() {
            ui.label(&self.scope_ui.message);
        }
    }
    /// Shared scopes go to the open project's `.rustpy/scopes.json` on Apply.
    pub(super) fn save_project_scopes(&self) -> Result<(), String> {
        match &self.project {
            Some(project) => self
                .settings
                .scopes
                .save_project(&project.root)
                .map_err(|error| format!("Cannot save shared scopes: {error}")),
            None => Ok(()),
        }
    }
    fn refresh_scope_preview(&mut self) {
        let context = self.capture_scope_context();
        let pattern = self.scope_ui.pattern.clone();
        if self.scope_ui.preview.as_ref().is_some_and(|preview| {
            preview.pattern == pattern && Arc::ptr_eq(&preview.catalogue, &context.project_files)
        }) {
            return;
        }
        let (mut rows, files) = tree_rows(&context.root, &context.project_files);
        let (matched, error) = if pattern.trim().is_empty() {
            (vec![false; files.len()], None)
        } else {
            match self
                .settings
                .scopes
                .compile(&ScopeSelection::Custom(pattern.clone()))
            {
                Ok(scope) => (
                    files
                        .iter()
                        .map(|path| scope.matches(path, &context))
                        .collect(),
                    None,
                ),
                Err(error) => (vec![false; files.len()], Some(error)),
            }
        };
        for row in &mut rows {
            row.matched = matched[row.files.clone()]
                .iter()
                .filter(|hit| **hit)
                .count();
        }
        self.scope_ui.preview = Some(ScopePreview {
            pattern,
            catalogue: context.project_files.clone(),
            matched: rows[0].matched,
            files: files.len(),
            rows,
            error,
        });
    }
    /// The scope editor tree: files colored by whether the pattern being
    /// edited matches them, with buttons that edit the pattern for the
    /// selected node, and inline pattern errors.
    fn scope_preview_tree(&mut self, ui: &mut egui::Ui) {
        self.refresh_scope_preview();
        let Some(preview) = self.scope_ui.preview.take() else {
            return;
        };
        if let Some(error) = &preview.error {
            ui.colored_label(self.theme.error, format!("Pattern error: {error}"));
        } else if !preview.pattern.trim().is_empty() {
            ui.label(format!(
                "Preview: {} of {} project files match",
                preview.matched, preview.files
            ));
        }
        let mut action = None;
        ui.horizontal(|ui| {
            for tree_action in TreeAction::ALL {
                if ui
                    .add_enabled(
                        self.scope_ui.selected.is_some(),
                        egui::Button::new(tree_action.label()),
                    )
                    .clicked()
                {
                    action = Some(tree_action);
                }
            }
            ui.checkbox(&mut self.scope_ui.only_matching, "Show only matching");
        });
        let mut toggle = None;
        let mut select = None;
        egui::ScrollArea::vertical()
            .id_salt("scope-editor-tree")
            .max_height(260.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let mut hidden_below: Option<usize> = None;
                for row in &preview.rows {
                    if let Some(depth) = hidden_below {
                        if row.depth > depth {
                            continue;
                        }
                        hidden_below = None;
                    }
                    let total = row.files.len();
                    if self.scope_ui.only_matching && row.matched == 0 {
                        if row.directory {
                            hidden_below = Some(row.depth);
                        }
                        continue;
                    }
                    let expanded = row.directory
                        && (row.relative.is_empty()
                            || self.scope_ui.expanded.contains(&row.relative));
                    if row.directory && !expanded {
                        hidden_below = Some(row.depth);
                    }
                    let color = if total > 0 && row.matched == total {
                        self.theme.success
                    } else if row.matched > 0 {
                        self.theme.warning
                    } else {
                        self.theme.text_dim
                    };
                    let label = if row.directory {
                        format!("{}  ({}/{total})", row.name, row.matched)
                    } else {
                        row.name.clone()
                    };
                    let node = (row.relative.clone(), row.directory);
                    ui.horizontal(|ui| {
                        ui.add_space(row.depth as f32 * 14.0);
                        if row.directory && !row.relative.is_empty() {
                            if ui.small_button(if expanded { "▾" } else { "▸" }).clicked() {
                                toggle = Some(row.relative.clone());
                            }
                        } else {
                            ui.add_space(18.0);
                        }
                        if ui
                            .selectable_label(
                                self.scope_ui.selected.as_ref() == Some(&node),
                                egui::RichText::new(label).color(color),
                            )
                            .clicked()
                        {
                            select = Some(node.clone());
                        }
                    });
                }
            });
        self.scope_ui.preview = Some(preview);
        if let Some(relative) = toggle {
            if !self.scope_ui.expanded.remove(&relative) {
                self.scope_ui.expanded.insert(relative);
            }
        }
        if select.is_some() {
            self.scope_ui.selected = select;
        }
        if let (Some(action), Some((relative, directory))) = (action, &self.scope_ui.selected) {
            self.scope_ui.pattern =
                apply_tree_action(&self.scope_ui.pattern, action, relative, *directory);
        }
    }
}
fn validate(settings: &ScopeSettings) -> Result<(), String> {
    let mut names = BTreeSet::new();
    for scope in &settings.named {
        if scope.name.trim().is_empty()
            || scope.name.len() > 128
            || !names.insert(scope.name.clone())
            || PredefinedScope::ALL
                .iter()
                .any(|predefined| predefined.label() == scope.name)
        {
            return Err(
                "Scope names must be unique, nonempty and distinct from predefined scopes".into(),
            );
        }
        settings.compile(&ScopeSelection::Named(scope.name.clone()))?;
    }
    for selection in [
        &settings.todo,
        &settings.inspections,
        &settings.project_view,
    ] {
        settings.compile(selection)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_scope_settings_validate_saving_and_own_dirty_page_without_overriding_other_settings() {
        let mut app = IdeApp::new_welcome(Settings::default());
        let before = app.settings.clone();
        app.settings.scopes.named.push(NamedScope {
            name: "Production Python".into(),
            pattern: "file:src//*.py&&!file:*test*".into(),
            color: Some([20, 40, 60]),
            shared: false,
        });
        app.settings.scopes.todo = ScopeSelection::Named("Production Python".into());
        assert!(validate(&app.settings.scopes).is_ok());
        assert!(settings_ui::page_modified(
            settings_ui::SettingsPage::Scopes,
            &before,
            &app.settings
        ));
        assert!(!settings_ui::page_modified(
            settings_ui::SettingsPage::EditorGeneral,
            &before,
            &app.settings
        ));
        let mut broken = app.settings.scopes.clone();
        broken.named.clear();
        assert!(validate(&broken).is_err());
        let encoded = serde_json::to_string(&app.settings).unwrap();
        let loaded: Settings = serde_json::from_str(&encoded).unwrap();
        assert_eq!(loaded.scopes, app.settings.scopes);
    }
    #[test]
    fn captured_current_open_changed_and_named_tab_color_use_actual_editor_paths() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("item.py");
        std::fs::write(&path, "value=1\n").unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        app.open_file(&path);
        let index = app.active.unwrap();
        app.tabs[index].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "# edited\n".into())],
            "Edit scope fixture",
        );
        app.observe_navigation();
        let captured = app.capture_scope_context();
        for scope in [
            PredefinedScope::Current,
            PredefinedScope::Open,
            PredefinedScope::Changed,
            PredefinedScope::RecentlyChanged,
        ] {
            assert!(captured.predefined(scope, &path), "{scope:?}");
        }
        app.settings.scopes.file_colors_enabled = true;
        app.settings.scopes.named.push(NamedScope {
            name: "Current".into(),
            pattern: "$Current File".into(),
            color: Some([12, 34, 56]),
            shared: false,
        });
        assert_eq!(
            app.tab_scope_color(index),
            Some(egui::Color32::from_rgb(12, 34, 56))
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "value=1\n");
    }
    #[test]
    fn actual_settings_ui_lists_shared_predefined_custom_patterns_and_color_controls() {
        let mut app = IdeApp::new_welcome(Settings::default());
        let context = egui::Context::default();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 900.0),
                )),
                ..Default::default()
            },
            |ui| app.scope_settings_page(ui),
        );
        for label in ["Scopes and File Colors", "Name:", "Save scope", "New scope"] {
            assert!(
                output.shapes.iter().any(|shape| match &shape.shape {
                    egui::Shape::Text(text) => text.galley.text().contains(label),
                    _ => false,
                }),
                "{label}"
            );
        }
        output.drop_without_applying_deltas();
    }
    #[test]
    fn scope_editor_tree_groups_sorted_files_and_counts_pattern_matches() {
        let mut files = vec![
            PathBuf::from("/p/src/pkg/a.py"),
            PathBuf::from("/p/src/pkg/sub/b.py"),
            PathBuf::from("/p/src/top.py"),
            PathBuf::from("/p/readme.md"),
            PathBuf::from("/elsewhere/lib.py"),
        ];
        files.sort();
        let (rows, kept) = tree_rows(Path::new("/p"), &files);
        assert_eq!(kept.len(), 4, "files outside the project root are left out");
        let shape: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.depth,
                    row.relative.as_str(),
                    row.directory,
                    row.files.len(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (0, "", true, 4),
                (1, "readme.md", false, 1),
                (1, "src", true, 3),
                (2, "src/pkg", true, 2),
                (3, "src/pkg/a.py", false, 1),
                (3, "src/pkg/sub", true, 1),
                (4, "src/pkg/sub/b.py", false, 1),
                (2, "src/top.py", false, 1),
            ]
        );
        let mut app = IdeApp::new_welcome(Settings::default());
        app.scope_ui.pattern = apply_tree_action("", TreeAction::IncludeRecursively, "src", true);
        app.scope_ui.pattern =
            apply_tree_action(&app.scope_ui.pattern, TreeAction::Exclude, "src/pkg", true);
        assert_eq!(app.scope_ui.pattern, "file:src//*&&!file:src/pkg/*");
        let scope = app
            .settings
            .scopes
            .compile(&ScopeSelection::Custom(app.scope_ui.pattern.clone()))
            .unwrap();
        let context = ScopeContext {
            root: "/p".into(),
            ..Default::default()
        };
        let hits: Vec<_> = kept
            .iter()
            .map(|path| scope.matches(path, &context))
            .collect();
        assert_eq!(hits, vec![false, false, true, true]);
    }
    #[test]
    fn shared_scopes_are_listed_by_selectors_and_saved_with_the_project_on_apply() {
        let root = tempfile::tempdir().unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        app.settings.scopes.named.push(NamedScope {
            name: "Team".into(),
            pattern: "file:src//*".into(),
            color: None,
            shared: true,
        });
        assert!(app.scope_names().contains(&"Team".to_string()));
        app.settings.scopes.save_project(root.path()).unwrap();
        let encoded = serde_json::to_string(&app.settings).unwrap();
        let mut loaded: Settings = serde_json::from_str(&encoded).unwrap();
        assert!(
            loaded.scopes.named.is_empty(),
            "shared scopes stay out of IDE settings"
        );
        loaded.scopes.load_project(root.path()).unwrap();
        assert_eq!(loaded.scopes.named, app.settings.scopes.named);
        // Without an open project there is nowhere to share to.
        assert!(app.save_project_scopes().is_ok());
    }
}
