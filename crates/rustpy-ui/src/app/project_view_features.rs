//! Project View presentation and target-bound operations. Editor commands never
//! borrow an unrelated active document when invoked from a tree row.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_core::{TaskHandle, TaskManager};
use rustpy_settings::scopes::{PredefinedScope, ScopeSelection};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// Captured source locations and finding text for the Project View report.
type ProjectAnalysisRows = Vec<(PathBuf, TextRange, String)>;

#[derive(Default)]
pub(super) struct ProjectViewUi {
    clipboard: Option<(Vec<PathBuf>, bool)>,
    clipboard_text: Option<String>,
    transfer_name: Option<String>,
    transfer: Option<(Vec<PathBuf>, String, bool)>,
    delete: Option<Vec<PathBuf>>,
    report: Option<(String, ProjectAnalysisRows)>,
    analysis_job: Option<TaskHandle<(ProjectAnalysisRows, u64)>>,
    analysis_title: String,
    project_owner: Option<std::sync::Weak<Analysis>>,
    queued_format: std::collections::VecDeque<(PathBuf, Action)>,
    queued_delete: std::collections::VecDeque<PathBuf>,
    analysis_owner: Option<(std::sync::Weak<Analysis>, u64)>,
    filter_stamp: Option<u64>,
    /// The library roots shown, for the (project root, interpreter sys.path) they came from.
    library_roots: Option<(LibraryRootsKey, Vec<PathBuf>)>,
    error: Option<String>,
}

type LibraryRootsKey = Option<(PathBuf, Vec<PathBuf>)>;
impl ProjectViewUi {
    pub(super) fn modal_active(&self) -> bool {
        self.transfer.is_some() || self.delete.is_some() || self.error.is_some()
    }
}
impl Drop for ProjectViewUi {
    fn drop(&mut self) {
        if let Some(job) = &self.analysis_job {
            job.cancel();
        }
    }
}

fn ancestors(paths: impl IntoIterator<Item = PathBuf>, root: &Path) -> HashSet<PathBuf> {
    let mut allowed = HashSet::new();
    for path in paths {
        for parent in path.ancestors() {
            allowed.insert(parent.to_path_buf());
            if parent == root {
                break;
            }
        }
    }
    allowed
}
fn minimal_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    let mut result = Vec::new();
    for path in paths {
        if !result
            .iter()
            .any(|parent: &PathBuf| path.starts_with(parent))
        {
            result.push(path);
        }
    }
    result
}

impl IdeApp {
    pub(super) fn project_view_shortcut(&mut self, action: Action, ctx: &egui::Context) -> bool {
        if !ProjectTree::has_focus(ctx) || !self.editor_controls_available() {
            return false;
        }
        if matches!(action, Action::Cut | Action::Copy | Action::Paste) {
            let Some(project) = &self.project else {
                return false;
            };
            let paths = project.tree.selection();
            let selected = project
                .tree
                .selected
                .clone()
                .unwrap_or_else(|| project.root.clone());
            if action == Action::Paste {
                self.project_paste(
                    if selected.is_dir() {
                        selected
                    } else {
                        selected.parent().unwrap_or(Path::new(".")).into()
                    },
                    ctx,
                );
            } else {
                self.project_clipboard(paths, action == Action::Cut, ctx);
            }
            return true;
        }
        false
    }

    pub(super) fn project_view_toolbar(&mut self, ui: &mut egui::Ui) {
        let Some(analysis) = self
            .project
            .as_ref()
            .map(|project| project.analysis.clone())
        else {
            return;
        };
        if self
            .project_view
            .project_owner
            .as_ref()
            .is_some_and(|owner| {
                owner
                    .upgrade()
                    .is_none_or(|owner| !Arc::ptr_eq(&owner, &analysis))
            })
        {
            self.project_view = ProjectViewUi::default();
        }
        self.project_view.project_owner = Some(Arc::downgrade(&analysis));
        let mut changed = false;
        ui.horizontal(|ui| {
            let selected = self.settings.editor.project_view.view.clone();
            egui::ComboBox::from_id_salt("project_view_kind")
                .selected_text(&selected)
                .show_ui(ui, |ui| {
                    for name in [
                        "Project",
                        "Project Files",
                        "Problems",
                        "Open Files",
                        "Scratches and Consoles",
                        "Changed Files",
                        "Tests",
                        "Production",
                        "Custom Scope",
                    ] {
                        changed |= ui
                            .selectable_value(
                                &mut self.settings.editor.project_view.view,
                                name.into(),
                                crate::i18n::tr_static(name),
                            )
                            .changed();
                    }
                    for scope in &self.settings.scopes.named {
                        let name = format!("Scope: {}", scope.name);
                        changed |= ui
                            .selectable_value(
                                &mut self.settings.editor.project_view.view,
                                name.clone(),
                                name,
                            )
                            .changed();
                    }
                })
                .combo_named(crate::i18n::tr_static("Project view"));
            ui.menu_button("View Options", |ui| {
                let options = &mut self.settings.editor.project_view;
                for (label, value) in [
                    ("Flatten Packages", &mut options.flatten_packages),
                    ("Compact Middle Packages", &mut options.compact_packages),
                    ("Show Excluded Files", &mut options.show_excluded),
                    ("Show Visibility Icons", &mut options.visibility_icons),
                    ("Sort by Type", &mut options.sort_by_type),
                    ("Folders Always on Top", &mut options.folders_first),
                    ("Show Library Contents", &mut options.show_libraries),
                    ("Open Files with Single Click", &mut options.single_click),
                    (
                        "Always Select Opened File",
                        &mut options.always_select_opened,
                    ),
                    ("Focus Editor on Esc", &mut options.focus_editor_on_escape),
                ] {
                    changed |= ui.checkbox(value, crate::i18n::tr_static(label)).changed();
                }
                changed |= ui
                    .checkbox(
                        &mut self.settings.editor.structure.show_members_in_project,
                        "Show Members",
                    )
                    .changed();
                changed |= ui
                    .checkbox(
                        &mut self.settings.editor.autoscroll_from_source,
                        "Autoscroll from Source",
                    )
                    .changed();
                changed |= ui
                    .checkbox(
                        &mut self.settings.editor.autoscroll_to_source,
                        "Autoscroll to Source",
                    )
                    .changed();
                ui.label("File nesting: parent suffix=child suffixes; separate rules with ;");
                changed |= ui
                    .text_edit_singleline(&mut self.settings.editor.project_view.nesting_rules)
                    .changed();
            });
        });
        if self.settings.editor.project_view.view == "Custom Scope" {
            let names = self.scope_names();
            changed |= super::scope_ui::scope_selector(
                ui,
                &mut self.settings.scopes.project_view,
                &names,
                "project_view_scope",
            );
        }
        if changed {
            let _ = self.settings.save();
        }
        let published_native = self.native_skeleton_ui.published_module_names();
        let context = self.capture_scope_context();
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        (Arc::as_ptr(&context.project_files) as usize).hash(&mut hash);
        context.open_files.hash(&mut hash);
        context.changed_files.hash(&mut hash);
        context.current_file.hash(&mut hash);
        context.recently_viewed.hash(&mut hash);
        context.recently_changed.hash(&mut hash);
        serde_json::to_string(&self.settings.scopes)
            .unwrap_or_default()
            .hash(&mut hash);
        serde_json::to_string(&self.settings.editor.project_view)
            .unwrap_or_default()
            .hash(&mut hash);
        // Problems can change without an edit (a background project scan completes).
        let mut counts = HashMap::<PathBuf, usize>::new();
        for (path, errors) in &self.bg.problems.results {
            counts.insert(path.clone(), errors.len());
        }
        for tab in &self.tabs {
            if let Some(path) = &tab.state.path {
                counts.insert(path.clone(), tab.state.extra_diagnostics.len());
            }
        }
        let file_counts = counts.clone();
        if let Some(project) = &self.project {
            for (path, count) in file_counts {
                for parent in path.ancestors().skip(1) {
                    if !parent.starts_with(&project.root) {
                        break;
                    }
                    *counts.entry(parent.into()).or_default() += count;
                }
            }
        }
        let mut count_stamp: Vec<_> = counts.iter().collect();
        count_stamp.sort_by_key(|(path, _)| *path);
        count_stamp.hash(&mut hash);
        published_native.hash(&mut hash);
        let stamp = hash.finish();
        let project = self.project.as_mut().unwrap();
        project.tree.configure(&self.settings.editor.project_view);
        project.tree.show_details = self.settings.appearance.show_tree_details;
        project.tree.show_python_stubs = self.settings.editor.project_view.show_libraries;
        // Library roots are directories of the interpreter's sys.path: stat
        // them when the interpreter (or the option) changes, not every frame.
        let library_key = self
            .settings
            .editor
            .project_view
            .show_libraries
            .then(|| {
                project
                    .interpreter
                    .as_ref()
                    .map(|interpreter| (project.root.clone(), interpreter.sys_path.clone()))
            })
            .flatten();
        if self.project_view.library_roots.as_ref().map(|(key, _)| key) != Some(&library_key) {
            let roots = library_key
                .as_ref()
                .map(|(root, _)| {
                    project
                        .interpreter
                        .as_ref()
                        .map(|interpreter| {
                            interpreter
                                .search_paths()
                                .into_iter()
                                .filter(|path| !path.starts_with(root))
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            self.project_view.library_roots = Some((library_key, roots));
        }
        if let Some((_, roots)) = &self.project_view.library_roots {
            if project.tree.library_roots != *roots {
                project.tree.library_roots = roots.clone();
            }
        }
        project.tree.problem_counts = counts;
        if self.settings.editor.project_view.view != "Project"
            && self.project_index.files.is_building()
        {
            ui.label("Indexing project files…");
        }
        if self.project_view.filter_stamp == Some(stamp) {
            return;
        }
        self.project_view.filter_stamp = Some(stamp);
        for name in &published_native {
            let _ = project.analysis.module(name);
        }
        project.tree.extended_definitions = project
            .analysis
            .files_with_limit(4096)
            .unwrap_or_default()
            .into_iter()
            .filter(|file| file.path.starts_with("<typeshed>/@compiled"))
            .map(|file| file.path.clone())
            .collect();

        let name = &self.settings.editor.project_view.view;
        let selection = match name.as_str() {
            "Project Files" => Some(ScopeSelection::Predefined(PredefinedScope::Project)),
            "Open Files" => Some(ScopeSelection::Predefined(PredefinedScope::Open)),
            "Changed Files" => Some(ScopeSelection::Predefined(PredefinedScope::Changed)),
            "Tests" => Some(ScopeSelection::Predefined(PredefinedScope::Tests)),
            "Production" => Some(ScopeSelection::Predefined(PredefinedScope::Production)),
            "Scratches and Consoles" => Some(ScopeSelection::Predefined(
                PredefinedScope::ScratchesConsoles,
            )),
            "Custom Scope" => Some(self.settings.scopes.project_view.clone()),
            name if name.starts_with("Scope: ") => Some(ScopeSelection::Named(name[7..].into())),
            _ => None,
        };
        let candidates = context
            .project_files
            .iter()
            .cloned()
            .chain(context.open_files.iter().cloned());
        let allowed = if name == "Problems" {
            Some(ancestors(
                project
                    .tree
                    .problem_counts
                    .iter()
                    .filter(|(_, count)| **count > 0)
                    .map(|(path, _)| path.clone()),
                &project.root,
            ))
        } else if let Some(selection) = selection {
            match self.settings.scopes.compile(&selection) {
                Ok(scope) => Some(ancestors(
                    candidates.filter(|path| scope.matches(path, &context)),
                    &project.root,
                )),
                Err(error) => {
                    self.project_view.error = Some(error);
                    Some(HashSet::new())
                }
            }
        } else {
            None
        };
        project.tree.set_allowed_paths(allowed);
        project.tree.scope_colors = context
            .project_files
            .iter()
            .filter_map(|path| {
                self.settings
                    .scopes
                    .color(path, &context)
                    .map(|[r, g, b]| (path.clone(), Color32::from_rgb(r, g, b)))
            })
            .collect();
    }

    pub(super) fn project_clipboard(
        &mut self,
        paths: Vec<PathBuf>,
        cut: bool,
        ctx: &egui::Context,
    ) {
        let paths = minimal_paths(paths);
        if paths.is_empty() {
            return;
        }
        let text = paths
            .iter()
            .map(|path| {
                format!(
                    "file://{}",
                    path.to_string_lossy()
                        .replace('%', "%25")
                        .replace(' ', "%20")
                        .replace('#', "%23")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        ctx.copy_text(text.clone());
        self.remember_copied(text.clone());
        self.clipboard_payload = None;
        self.project_view.clipboard_text = Some(text);
        self.project_view.clipboard = Some((paths, cut));
    }
    pub(super) fn project_paste(&mut self, target: PathBuf, _ctx: &egui::Context) {
        let text = self.clipboard_text();
        let selected = self
            .project_view
            .clipboard
            .clone()
            .filter(|_| text.as_ref() == self.project_view.clipboard_text.as_ref())
            .or_else(|| {
                text.map(|text| {
                    (
                        text.lines()
                            .filter_map(|line| line.strip_prefix("file://"))
                            .map(|path| {
                                PathBuf::from(
                                    path.replace("%23", "#")
                                        .replace("%20", " ")
                                        .replace("%25", "%"),
                                )
                            })
                            .collect(),
                        false,
                    )
                })
            });
        if let Some((paths, cut)) = selected {
            self.project_transfer_dialog(paths, target, !cut);
        }
    }
    pub(super) fn project_transfer_dialog(
        &mut self,
        paths: Vec<PathBuf>,
        target: PathBuf,
        copy: bool,
    ) {
        let paths = minimal_paths(paths);
        self.project_view.transfer_name = if paths.len() == 1 {
            paths[0]
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        } else {
            None
        };
        self.project_view.transfer = Some((paths, target.display().to_string(), copy));
        self.focus_editor = false;
    }
    pub(super) fn project_delete_many(&mut self, paths: Vec<PathBuf>) {
        let Some(project) = &self.project else { return };
        let paths: Vec<_> = paths
            .into_iter()
            .filter(|path| {
                path != &project.root
                    && !project.tree.content_roots.contains(path)
                    && (path.starts_with(&project.root)
                        || project
                            .tree
                            .content_roots
                            .iter()
                            .any(|root| path.starts_with(root)))
            })
            .collect();
        if paths.is_empty() {
            self.set_status("Project/content roots and libraries cannot be deleted from this tree");
            return;
        }
        self.project_view.delete = Some(minimal_paths(paths));
        self.focus_editor = false;
    }

    pub(super) fn project_path_action(
        &mut self,
        path: PathBuf,
        action: Action,
        ctx: &egui::Context,
    ) {
        if !self.dumb_mode_admits_with(action, |_| crate::dumb_mode::Target::Path(path.clone())) {
            return;
        }
        if action == Action::ShowFileHistory {
            self.show_operation_history(path);
            return;
        }
        if action == Action::VcsPopup {
            self.open_git_operations();
            if let Some(root) = self.vcs_workspace.mappings.root_for(&path).cloned() {
                self.git_panel.open_operations(
                    root,
                    Some(path),
                    None,
                    self.settings.vcs_operations.clone(),
                );
            }
            return;
        }
        if let Some(project) = &mut self.project {
            project.tree.selected = Some(path.clone());
        }
        if matches!(
            action,
            Action::Rename
                | Action::MoveRefactoring
                | Action::SafeDelete
                | Action::ConvertToPackage
                | Action::CopyFile
        ) && self.project.as_ref().is_some_and(|project| {
            path == project.root || project.tree.content_roots.contains(&path)
        }) {
            self.set_status("Project/content roots cannot be refactored as child paths");
            return;
        }

        if action == Action::ToggleBookmark {
            self.add_path_bookmark(&path);
            return;
        }
        if action == Action::FindUsages {
            let Some(project) = &self.project else { return };
            let analysis = project.analysis.clone();
            let root = project.root.clone();
            self.sync_open_tabs_now();
            let usages = rustpy_refactor::refactor2::module_import_sites(
                &analysis,
                &root,
                &path,
                &self.project_files(),
            );
            let title = format!("Imports of {}", path.display());
            self.usage_ui.adopt(
                title.clone(),
                usages
                    .iter()
                    .cloned()
                    .map(rustpy_search::usages::results::Hit::from_usage)
                    .collect(),
            );
            self.usages = usages;
            self.usages_title = title;
            self.select_usage_results();
            return;
        }
        if matches!(action, Action::InspectCode | Action::RunInspectionByName) {
            self.show_inspection_runner_for_path(&path);
            return;
        }
        if matches!(
            action,
            Action::DeploymentConfiguration
                | Action::DeploymentUpload
                | Action::DeploymentDownload
                | Action::DeploymentSync
                | Action::DeploymentBrowse
        ) {
            let root = self.project_root_for_view();
            let error = match action {
                Action::DeploymentConfiguration => {
                    self.deploy.open_config(&root);
                    None
                }
                Action::DeploymentUpload => {
                    if !self.save_all() {
                        return;
                    }
                    self.deploy.upload(&root, vec![path])
                }
                Action::DeploymentDownload => self.deploy.download(&root, vec![path]),
                Action::DeploymentSync => {
                    if !self.save_all() {
                        return;
                    }
                    self.deploy.open_sync(&root)
                }
                _ => self.deploy.open_browser(&root),
            };
            if let Some(error) = error {
                self.set_status(error);
            }
            return;
        }
        if matches!(action, Action::OpenInBrowser | Action::OpenWebPreview) {
            self.open_web_file(&path, None, action == Action::OpenWebPreview);
            return;
        }
        if action == Action::MoveRefactoring {
            self.project_transfer_dialog(
                vec![path.clone()],
                path.parent().unwrap_or(&path).into(),
                false,
            );
            return;
        }
        if action == Action::Rename {
            self.open_tree_rename(path);
            return;
        }
        if action == Action::SafeDelete {
            self.open_tree_delete(path);
            return;
        }
        if action == Action::CopyFile {
            self.project_transfer_dialog(
                vec![path.clone()],
                path.parent().unwrap_or(&path).into(),
                true,
            );
            return;
        }
        if action == Action::RunWithConcurrencyDiagram {
            self.launch_path(&path, crate::run_context::RunMode::Concurrency);
            return;
        }
        if matches!(action, Action::RunWithCoverage | Action::ProfileRun) {
            self.launch_path(
                &path,
                if action == Action::RunWithCoverage {
                    crate::run_context::RunMode::Coverage
                } else {
                    crate::run_context::RunMode::Profile
                },
            );
            return;
        }
        if matches!(
            action,
            Action::EditRunConfigurations
                | Action::ModifyContextConfiguration
                | Action::CreateContextConfiguration
        ) {
            let configuration =
                crate::run_context::config_for_path(&self.project_root_for_view(), &path);
            let context_action = if action == Action::ModifyContextConfiguration {
                crate::run_context::ContextAction::Modify
            } else {
                crate::run_context::ContextAction::Create
            };
            self.dispatch_context_configuration(configuration, context_action);
            return;
        }
        if path.is_dir() {
            if action == Action::ReloadFromDisk {
                let indices: Vec<_> = self
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|(_, tab)| {
                        tab.state
                            .path
                            .as_ref()
                            .is_some_and(|file| file.starts_with(&path))
                    })
                    .map(|(index, _)| index)
                    .collect();
                for index in indices {
                    self.reload_tab_from_disk(index);
                }
                if let Some(project) = &mut self.project {
                    project.tree.invalidate(&path);
                }
                return;
            }
            if action == Action::ConvertToPackage {
                self.set_status("Choose a Python module to convert to a package");
                return;
            }
            if matches!(action, Action::InspectCode | Action::RunInspectionByName) {
                self.show_inspection_runner();
                return;
            }
            if action == Action::ShowUmlDiagram {
                self.set_status("Choose a Python file to show its class diagram");
                return;
            }
            if matches!(
                action,
                Action::ReformatCode | Action::OptimizeImports | Action::CodeCleanup
            ) {
                self.project_batch_format(&path, action);
                return;
            }
        } else {
            self.open_file(&path);
            if self
                .active
                .and_then(|index| self.tabs[index].state.path.as_deref())
                != Some(path.as_path())
            {
                return;
            }
        }
        self.handle_action(action, ctx);
    }
    fn project_root_for_view(&self) -> PathBuf {
        self.project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default()
    }

    pub(super) fn project_encoding(&mut self, path: PathBuf, label: &str) {
        self.open_file(&path);
        let Some(index) = self
            .active
            .filter(|index| self.tabs[*index].state.path.as_deref() == Some(&path))
        else {
            return;
        };
        let encoding = match label {
            "UTF-8 BOM" => rustpy_text::Encoding::Utf8Bom,
            "UTF-16 LE" => rustpy_text::Encoding::Utf16Le,
            "UTF-16 BE" => rustpy_text::Encoding::Utf16Be,
            "ISO-8859-1" => rustpy_text::Encoding::Latin1,
            _ => rustpy_text::Encoding::Utf8,
        };
        self.convert_encoding(index, encoding);
    }
    pub(super) fn project_line_endings(&mut self, path: PathBuf, label: &str) {
        self.open_file(&path);
        let Some(index) = self
            .active
            .filter(|index| self.tabs[*index].state.path.as_deref() == Some(&path))
        else {
            return;
        };
        if self.tabs[index].state.doc.is_read_only() {
            self.set_status("File is read-only");
            return;
        }
        self.tabs[index].state.doc.set_line_ending(match label {
            "CRLF" => rustpy_text::LineEnding::CrLf,
            "CR" => rustpy_text::LineEnding::Cr,
            _ => rustpy_text::LineEnding::Lf,
        });
    }
    pub(super) fn project_open_associated(&mut self, path: PathBuf) {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        if let Err(error) = std::process::Command::new(program).arg(path).spawn() {
            self.set_status(error.to_string());
        }
    }
    pub(super) fn project_create_gist(&mut self, path: PathBuf, _ctx: &egui::Context) {
        self.github_gist(Some(path));
    }

    /// Reuse the authenticated Gist preview for retained console output.
    pub(super) fn console_create_gist(&mut self, text: String) {
        if text.len() > 1024 * 1024 {
            self.set_status("Gist input exceeds 1 MiB; export the console instead");
            return;
        }
        let mut composer =
            crate::github_panel::Composer::new(crate::github_panel::ComposeKind::Gist);
        composer.title = "Run console output".into();
        composer.files.insert("console-output.log".into(), text);
        composer.paths.push(PathBuf::from("console-output.log"));
        composer.open_browser = true;
        self.github.panel.composer = Some(composer);
        self.bind_github_composer();
        self.dialog = Dialog::GitHubModal;
        self.focus_editor = false;
    }

    pub(super) fn project_many_actions(&mut self, paths: Vec<PathBuf>, action: Action) {
        let files = self.project_files();
        let mut selected = std::collections::BTreeSet::new();
        for path in minimal_paths(paths) {
            if path.is_dir() {
                selected.extend(
                    files
                        .iter()
                        .filter(|file| {
                            file.starts_with(&path)
                                && file.extension().is_some_and(|extension| extension == "py")
                        })
                        .cloned(),
                );
            } else if path.extension().is_some_and(|extension| extension == "py") {
                selected.insert(path);
            }
        }
        if selected.len() > 256 {
            self.set_status("Select at most 256 Python files per formatting operation");
            return;
        }
        self.project_view.queued_format = selected.into_iter().map(|path| (path, action)).collect();
    }

    fn project_batch_format(&mut self, path: &Path, action: Action) {
        if matches!(action, Action::ReformatCode | Action::CodeCleanup) {
            self.reformat_directory_dialog(path.to_owned(), action);
            return;
        }
        let files: Vec<_> = self
            .project_files()
            .into_iter()
            .filter(|file| {
                file.starts_with(path)
                    && file.extension().is_some_and(|extension| extension == "py")
            })
            .collect();
        if files.len() > 256 {
            self.set_status(
                "Select a smaller directory: at most 256 Python files per formatting operation",
            );
            return;
        }
        self.project_view.queued_format = files.into_iter().map(|path| (path, action)).collect();
    }

    pub(super) fn project_analyze(&mut self, path: PathBuf, kind: &'static str) {
        let Some(analysis) = self
            .project
            .as_ref()
            .map(|project| project.analysis.clone())
        else {
            return;
        };
        self.sync_open_tabs_now();
        let files = self.project_files();
        let revision = analysis.revision_excluding(Path::new(""));
        if let Some(job) = self.project_view.analysis_job.take() {
            job.cancel();
        }
        self.project_view.analysis_owner = Some((Arc::downgrade(&analysis), revision));
        self.project_view.analysis_title = kind.into();
        let duplicate_policy = self.settings.editor.preferences.clone();
        self.project_view.analysis_job =
            Some(TaskManager::global().task(kind).spawn(move |progress| {
                let mut loaded = Vec::new();
                let mut bytes = 0usize;
                if files.len() > 4096 {
                    return Err(rustpy_core::TaskError::Failed(
                        "Analysis selection exceeds 4096 files; choose a smaller project scope"
                            .into(),
                    ));
                }
                for file in files {
                    if progress.token().is_cancelled() {
                        return Err(rustpy_core::TaskError::Cancelled);
                    }
                    let Some(file) = analysis.file_for_path(&file) else {
                        continue;
                    };
                    bytes = bytes.saturating_add(file.text.len());
                    if bytes > 16 * 1024 * 1024 {
                        return Err(rustpy_core::TaskError::Failed(
                            "Analysis source budget exceeded (16 MiB)".into(),
                        ));
                    }
                    loaded.push(file);
                }
                let mut rows = Vec::new();
                let mut work = 100_000usize;
                let mut key_bytes = 0usize;
                if kind == "Data Flow" {
                    for file in &loaded {
                        if rows.len() >= 4096 {
                            break;
                        }
                        if file.path == path || file.path.starts_with(&path) {
                            let inferer = rustpy_semantic::infer::Inferer::new(&analysis);
                            for reference in &file.scopes.references {
                                let expression = file
                                    .parse
                                    .syntax_node()
                                    .token_at_offset(reference.range.start())
                                    .right_biased()
                                    .and_then(|token| token.parent())
                                    .and_then(rustpy_syntax::ast::NameExpr::cast);
                                let reaching = expression
                                    .and_then(|name| inferer.reaching_bindings(file, &name));
                                let definitions = reaching
                                    .map(|bindings| {
                                        bindings
                                            .into_iter()
                                            .filter_map(|binding| {
                                                file.scopes.bindings.get(binding.0 as usize)
                                            })
                                            .map(|binding| {
                                                let line = file.text
                                                    [..usize::from(binding.range.start())]
                                                    .bytes()
                                                    .filter(|byte| *byte == b'\n')
                                                    .count()
                                                    + 1;
                                                format!("{} at line {line}", binding.name)
                                            })
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    })
                                    .unwrap_or_else(|| "possibly unbound / unknown source".into());
                                rows.push((
                                    file.path.clone(),
                                    reference.range,
                                    format!(
                                        "{} {} ← {definitions}",
                                        if reference.is_write { "Write" } else { "Read" },
                                        reference.name
                                    ),
                                ));
                                if rows.len() >= 4096 {
                                    break;
                                }
                            }
                        }
                    }
                } else if kind == "Locate Duplicates" {
                    let mut seen = HashMap::<String, (PathBuf, TextRange)>::new();
                    for file in &loaded {
                        if rows.len() >= 4096 {
                            break;
                        }
                        for statement in file
                            .parse
                            .syntax_node()
                            .descendants()
                            .filter_map(rustpy_syntax::ast::Stmt::cast)
                        {
                            if progress.token().is_cancelled() {
                                return Err(rustpy_core::TaskError::Cancelled);
                            }
                            work = work.checked_sub(1).ok_or_else(|| {
                                rustpy_core::TaskError::Failed(
                                    "Analysis work budget exceeded".into(),
                                )
                            })?;
                            let length = usize::from(statement.syntax().text_range().len());
                            if !(duplicate_policy.duplicate_min_size.clamp(8, 8192)..=8192)
                                .contains(&length)
                            {
                                continue;
                            }
                            let text = statement.syntax().text().to_string();
                            if !(duplicate_policy.duplicate_min_size.clamp(8, 8192)..=8192)
                                .contains(&text.len())
                            {
                                continue;
                            }
                            let key = duplicate_policy.duplicate_key(&text);
                            key_bytes += key.len();
                            if key_bytes > 4 * 1024 * 1024 {
                                return Err(rustpy_core::TaskError::Failed(
                                    "Duplicate-source key budget exceeded".into(),
                                ));
                            }
                            if let Some((previous, range)) = seen.get(&key) {
                                if file.path.starts_with(&path) || previous.starts_with(&path) {
                                    rows.push((
                                        previous.clone(),
                                        *range,
                                        "Original duplicated statement".into(),
                                    ));
                                    rows.push((
                                        file.path.clone(),
                                        statement.syntax().text_range(),
                                        "Duplicate statement".into(),
                                    ));
                                }
                            } else {
                                seen.insert(
                                    key,
                                    (file.path.clone(), statement.syntax().text_range()),
                                );
                            }
                            if rows.len() >= 4096 {
                                break;
                            }
                        }
                    }
                } else {
                    let mut edges = Vec::<(PathBuf, PathBuf, TextRange)>::new();
                    for file in &loaded {
                        for binding in &file.scopes.bindings {
                            let rustpy_semantic::scope::BindingKind::Import(import) = &binding.kind
                            else {
                                continue;
                            };
                            let child = import.name.as_ref().map(|name| {
                                if import.module.is_empty() {
                                    name.to_string()
                                } else {
                                    format!("{}.{}", import.module, name)
                                }
                            });
                            let target = child
                                .and_then(|child| {
                                    analysis.module_relative(file, import.level, &child)
                                })
                                .or_else(|| {
                                    analysis.module_relative(file, import.level, &import.module)
                                });
                            let Some(target) = target else { continue };
                            edges.push((file.path.clone(), target.path.clone(), binding.range));
                            if edges.len() > 16384 {
                                return Err(rustpy_core::TaskError::Failed(
                                    "Import graph edge budget exceeded".into(),
                                ));
                            }
                        }
                    }
                    for (source, target, range) in &edges {
                        if progress.token().is_cancelled() {
                            return Err(rustpy_core::TaskError::Cancelled);
                        }
                        let include = if kind == "Backward Dependencies" {
                            target == &path || target.starts_with(&path)
                        } else {
                            source == &path || source.starts_with(&path)
                        };
                        if !include {
                            continue;
                        }
                        if kind == "Cyclic Dependencies" {
                            let mut pending = vec![target.clone()];
                            let mut visited = HashSet::new();
                            let mut found = false;
                            while let Some(current) = pending.pop() {
                                if current == *source {
                                    found = true;
                                    break;
                                }
                                if !visited.insert(current.clone()) {
                                    continue;
                                }
                                if visited.len() > 4096 {
                                    break;
                                }
                                for (from, to, _) in &edges {
                                    work = work.checked_sub(1).ok_or_else(|| {
                                        rustpy_core::TaskError::Failed(
                                            "Cycle search work budget exceeded".into(),
                                        )
                                    })?;
                                    if progress.token().is_cancelled() {
                                        return Err(rustpy_core::TaskError::Cancelled);
                                    }
                                    if from == &current {
                                        pending.push(to.clone());
                                    }
                                }
                            }
                            if !found {
                                continue;
                            }
                        }
                        rows.push((
                            source.clone(),
                            *range,
                            format!("{} → {}", source.display(), target.display()),
                        ));
                        if rows.len() >= 4096 {
                            break;
                        }
                    }
                }
                if loaded.iter().any(|file| {
                    analysis
                        .file(file.id)
                        .is_none_or(|current| !Arc::ptr_eq(&current, file))
                }) {
                    return Err(rustpy_core::TaskError::Failed(
                        "Sources changed during analysis".into(),
                    ));
                }
                Ok((rows, analysis.revision_excluding(Path::new(""))))
            }));
    }

    pub(super) fn project_view_dialog(&mut self, ctx: &egui::Context) {
        if self.coordinated_quit_active() {
            return;
        }
        if self.project_view.modal_active()
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.project_view.transfer = None;
            self.project_view.delete = None;
            self.project_view.error = None;
            self.project_view.queued_delete.clear();
            if let Some(project) = &mut self.project {
                project.tree.request_focus();
            }
        }
        let owner_valid = self
            .project_view
            .project_owner
            .as_ref()
            .is_none_or(|owner| {
                self.project.as_ref().is_some_and(|project| {
                    owner
                        .upgrade()
                        .is_some_and(|owner| Arc::ptr_eq(&owner, &project.analysis))
                })
            });
        if !owner_valid {
            if let Some(job) = self.project_view.analysis_job.take() {
                job.cancel();
            }
            self.project_view.analysis_owner = None;
            self.project_view.report = None;
            self.project_view.transfer = None;
            self.project_view.queued_format.clear();
            self.project_view.queued_delete.clear();
            self.project_view.project_owner = None;
        }
        if let Some(result) = self
            .project_view
            .analysis_job
            .as_mut()
            .and_then(|job| job.try_result())
        {
            self.project_view.analysis_job = None;
            match result {
                Ok((rows, epoch)) => {
                    let fresh =
                        self.project_view
                            .analysis_owner
                            .as_ref()
                            .is_some_and(|(owner, _)| {
                                owner.upgrade().is_some_and(|owner| {
                                    owner.revision_excluding(Path::new("")) == epoch
                                })
                            });
                    if fresh {
                        self.project_view.report =
                            Some((self.project_view.analysis_title.clone(), rows));
                    } else {
                        self.project_view.error =
                            Some("Sources changed during analysis; run it again".into());
                    }
                }
                Err(error) => self.project_view.error = Some(error.to_string()),
            }
        }
        if self.project_view.analysis_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if self.bg.external_formatter.is_none()
            && self.bg.reformat.is_none()
            && matches!(self.dialog, Dialog::None)
        {
            if let Some(path) = self.project_view.queued_delete.pop_front() {
                self.open_tree_delete(path);
            } else if let Some((path, action)) = self.project_view.queued_format.pop_front() {
                self.project_path_action(path, action, ctx);
            }
        }
        if let Some((paths, mut target, mut copy)) = self.project_view.transfer.take() {
            let mut open = true;
            let mut apply = false;
            let mut name = self.project_view.transfer_name.take();
            egui::Window::new(if copy{"Copy Selected Paths"}else{"Move Selected Paths"}).open(&mut open).show_modal(ctx,|ui| {
                for path in &paths {ui.label(path.display().to_string());}
                ui.label("Destination directory");ui.text_edit_singleline(&mut target);
                if let Some(name)=&mut name{ui.label("Destination name");ui.text_edit_singleline(name);}
                ui.checkbox(&mut copy,"Copy instead of move");
                if !copy {ui.label("Python module/package moves update imports and preserve relative import targets.");}
                apply=ui.button(if copy{"Copy"}else{"Refactor and Move"}).clicked();
            });
            if apply {
                if let Err(error) = self.project_transfer_named(
                    &paths,
                    Path::new(target.trim()),
                    copy,
                    name.as_deref(),
                ) {
                    self.project_view.error = Some(error);
                }
            } else if open {
                self.project_view.transfer = Some((paths, target, copy));
                self.project_view.transfer_name = name;
            }
        }
        if let Some(paths) = self.project_view.delete.take() {
            let mut open = true;
            let mut apply = false;
            egui::Window::new("Delete Selected Paths")
                .open(&mut open)
                .show_modal(ctx, |ui| {
                    ui.label("Each Python module/package retains its import-usage confirmation.");
                    for path in &paths {
                        ui.label(path.display().to_string());
                    }
                    apply = ui.button("Continue with selected paths").clicked();
                });
            if apply {
                self.project_view.queued_delete = paths.into();
            } else if open {
                self.project_view.delete = Some(paths);
            }
        }
        if let Some((title, rows)) = self.project_view.report.take() {
            let mut open = true;
            let mut navigate = None;
            egui::Window::new(title.clone())
                .open(&mut open)
                .resizable(true)
                .show_modeless(ctx, |ui| {
                    ui.label(format!("{} results (bounded source analysis)", rows.len()));
                    egui::ScrollArea::both().max_height(500.0).show(ui, |ui| {
                        for (path, range, label) in &rows {
                            if ui.selectable_label(false, label).double_clicked() {
                                navigate = Some((path.clone(), *range));
                            }
                        }
                    });
                });
            if open {
                self.project_view.report = Some((title, rows));
            }
            if let Some((path, range)) = navigate {
                self.navigate_to(&path, range);
            }
        }
        if let Some(error) = self.project_view.error.clone() {
            let mut open = true;
            egui::Window::new("Project View")
                .open(&mut open)
                .show_modal(ctx, |ui| {
                    ui.label(error);
                });
            if !open {
                self.project_view.error = None;
            }
        }
    }

    #[cfg(test)]
    fn project_transfer(
        &mut self,
        paths: &[PathBuf],
        target: &Path,
        copy: bool,
    ) -> Result<(), String> {
        self.project_transfer_named(paths, target, copy, None)
    }

    fn project_transfer_named(
        &mut self,
        paths: &[PathBuf],
        target: &Path,
        copy: bool,
        name: Option<&str>,
    ) -> Result<(), String> {
        if let Some(name) = name {
            if paths.len() != 1
                || name.is_empty()
                || Path::new(name).components().count() != 1
                || name == "."
                || name == ".."
                || name.contains(['/', '\\', '\0'])
            {
                return Err("Destination name must be one file/directory name".into());
            }
        }
        let project = self.project.as_ref().ok_or("No project")?;
        let root = project.root.clone();
        let analysis = project.analysis.clone();
        let target = dunce::canonicalize(target).map_err(|error| error.to_string())?;
        if !target.is_dir() {
            return Err("Destination is not a directory".into());
        }
        let roots = std::iter::once(&root)
            .chain(project.tree.content_roots.iter())
            .collect::<Vec<_>>();
        if !roots.iter().any(|root| target.starts_with(root)) {
            return Err("Destination must be a project/content root directory".into());
        }
        let sources = minimal_paths(paths.to_vec());
        let mut destinations = HashSet::new();
        for path in &sources {
            if !roots.iter().any(|root| path.starts_with(root)) || roots.contains(&path) {
                return Err("Only project children can be moved/copied".into());
            }
            let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err("Move/copy the symlink through your file manager".into());
            }
            let destination = target.join(
                name.map(std::ffi::OsStr::new)
                    .unwrap_or(path.file_name().ok_or("Invalid source name")?),
            );
            if destination.exists()
                || !destinations.insert(destination.clone())
                || destination.starts_with(path)
            {
                return Err(format!(
                    "Conflicting/recursive destination: {}",
                    destination.display()
                ));
            }
        }
        self.sync_open_tabs_now();
        let files = self.project_files();
        let mut operations = Vec::new();
        if copy {
            if self.tabs.iter().any(|tab| {
                tab.has_unsaved_changes()
                    && tab
                        .state
                        .path
                        .as_ref()
                        .is_some_and(|path| sources.iter().any(|source| path.starts_with(source)))
            }) {
                return Err("Save modified source files before copying their disk contents".into());
            }
            let mut directories = Vec::new();
            let mut entries = Vec::<(PathBuf, PathBuf, Vec<u8>)>::new();
            let mut bytes = 0usize;
            for source in &sources {
                let destination = target.join(
                    name.map(std::ffi::OsStr::new)
                        .unwrap_or(source.file_name().unwrap()),
                );
                collect_copy(
                    source,
                    &destination,
                    &mut entries,
                    &mut directories,
                    &mut bytes,
                    0,
                )?;
            }
            // Preflight every immutable source before the first destination is created.
            for (source, _, bytes) in &entries {
                if read_copy_source(source)? != *bytes {
                    return Err("Source changed while planning copy".into());
                }
            }
            let mut created = Vec::new();
            let mut created_directories = Vec::new();
            let result = (|| {
                for directory in &directories {
                    std::fs::create_dir(directory).map_err(|error| error.to_string())?;
                    created_directories.push(directory.clone());
                }
                for (_, destination, bytes) in &entries {
                    if let Some(parent) = destination.parent() {
                        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    }
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(destination)
                        .map_err(|error| error.to_string())?;
                    created.push(destination.clone());
                    file.write_all(bytes).map_err(|error| error.to_string())?;
                }
                Ok::<_, String>(())
            })();
            if let Err(error) = result {
                for path in created.into_iter().rev() {
                    let _ = std::fs::remove_file(path);
                }
                for path in created_directories.into_iter().rev() {
                    let _ = std::fs::remove_dir(path);
                }
                return Err(error);
            }
            for source in &sources {
                if source.is_dir() {
                    std::fs::create_dir_all(
                        target.join(
                            name.map(std::ffi::OsStr::new)
                                .unwrap_or(source.file_name().unwrap()),
                        ),
                    )
                    .map_err(|error| error.to_string())?;
                }
            }
        } else {
            for source in &sources {
                let destination = target.join(
                    name.map(std::ffi::OsStr::new)
                        .unwrap_or(source.file_name().unwrap()),
                );
                let module = source
                    .extension()
                    .is_some_and(|extension| extension == "py")
                    || source.join("__init__.py").is_file();
                if module || (source.is_dir() && source.starts_with(&root)) {
                    operations.extend(rustpy_refactor::refactor2::move_module(
                        &analysis,
                        &root,
                        source,
                        &destination,
                        &files,
                    )?);
                } else {
                    operations.push(rustpy_refactor::refactor2::FileOp::Move(
                        source.clone(),
                        destination,
                    ));
                }
            }
            let mut edits = std::collections::BTreeMap::<PathBuf, Vec<(TextRange, String)>>::new();
            let mut moves = Vec::new();
            for operation in operations {
                match operation {
                    rustpy_refactor::refactor2::FileOp::Edit(path, changes) => {
                        edits.entry(path).or_default().extend(changes)
                    }
                    other => moves.push(other),
                }
            }
            let mut operations = Vec::new();
            for (path, mut changes) in edits {
                changes.sort_by_key(|(range, _)| range.start());
                changes.dedup();
                if changes
                    .windows(2)
                    .any(|pair| pair[0].0.end() > pair[1].0.start())
                {
                    return Err("Selected moves affect the same import statement; move those modules separately".into());
                }
                operations.push(rustpy_refactor::refactor2::FileOp::Edit(path, changes));
            }
            operations.extend(moves);
            if self.settings.refactoring.preview_refactoring {
                self.preview_file_ops(operations, "Move Project Paths");
            } else {
                let failures = self.apply_file_ops_failures(operations, "Move Project Paths");
                if !failures.is_empty() {
                    return Err(failures.join("\n"));
                }
            }
        }
        if let Some(project) = &mut self.project {
            project.tree.invalidate_all();
        }
        self.refresh_project_files();
        self.project_view.filter_stamp = None;
        self.project_view.clipboard = None;
        self.project_view.clipboard_text = None;
        Ok(())
    }
}

fn collect_copy(
    source: &Path,
    target: &Path,
    entries: &mut Vec<(PathBuf, PathBuf, Vec<u8>)>,
    directories: &mut Vec<PathBuf>,
    total: &mut usize,
    depth: usize,
) -> Result<(), String> {
    if depth > 64 || entries.len() + directories.len() >= 4096 {
        return Err("Copy exceeds 4096 files; select a smaller subtree".into());
    }
    let metadata = std::fs::symlink_metadata(source).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("Directory copy does not follow symlinks".into());
    }
    if metadata.is_dir() {
        directories.push(target.to_path_buf());
        for child in std::fs::read_dir(source).map_err(|error| error.to_string())? {
            let child = child.map_err(|error| error.to_string())?;
            collect_copy(
                &child.path(),
                &target.join(child.file_name()),
                entries,
                directories,
                total,
                depth + 1,
            )?;
        }
    } else if metadata.is_file() {
        if metadata.len() > 16 * 1024 * 1024
            || total.saturating_add(metadata.len() as usize) > 64 * 1024 * 1024
        {
            return Err("Copy exceeds 64 MiB source budget".into());
        }
        let data = read_copy_source(source)?;
        *total += data.len();
        entries.push((source.into(), target.into(), data));
    } else {
        return Err("Only ordinary files/directories can be copied".into());
    }
    Ok(())
}

fn read_copy_source(path: &Path) -> Result<Vec<u8>, String> {
    let snapshot = rustpy_core::vfs::Vfs::new()
        .read(
            path,
            rustpy_core::vfs::ReadLimits {
                max_file_bytes: 16 * 1024 * 1024,
                max_cached_files: 1,
                max_cached_bytes: 16 * 1024 * 1024,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(snapshot.bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(root: &Path) -> IdeApp {
        let mut settings = Settings::default();
        settings.trust_project(root);
        IdeApp::new(settings, Some(root.into()))
    }

    #[test]
    fn tree_bookmark_and_inspection_context_do_not_mutate_an_unrelated_editor() {
        let root = tempfile::tempdir().unwrap();
        let other = root.path().join("other.py");
        let selected = root.path().join("selected.py");
        std::fs::write(&other, "untouched = 1\n").unwrap();
        std::fs::write(&selected, "selected = 2\n").unwrap();
        let mut app = app(root.path());
        app.open_file(&other);
        let ctx = egui::Context::default();
        app.project_path_action(selected.clone(), Action::ToggleBookmark, &ctx);
        assert_eq!(
            app.tabs[app.active.unwrap()].state.path.as_ref(),
            Some(&other)
        );
        assert!(app
            .bookmarks
            .iter()
            .any(|bookmark| bookmark.path == selected
                && bookmark.kind == rustpy_settings::BookmarkKind::File));
        app.project_path_action(selected, Action::InspectCode, &ctx);
        assert!(app.bg.problems.infrastructure.dialog);
        assert_eq!(
            app.tabs[app.active.unwrap()].state.doc.text().as_str(),
            "untouched = 1\n"
        );
    }

    #[test]
    fn copy_preserves_binary_and_empty_directories_and_preflights_all_collisions() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        std::fs::create_dir_all(source.join("empty/nested")).unwrap();
        std::fs::create_dir(&target).unwrap();
        std::fs::write(source.join("image.bin"), [0, 255, 1, 2]).unwrap();
        let mut app = app(root.path());
        app.project_transfer(std::slice::from_ref(&source), &target, true)
            .unwrap();
        assert_eq!(
            std::fs::read(target.join("source/image.bin")).unwrap(),
            [0, 255, 1, 2]
        );
        assert!(target.join("source/empty/nested").is_dir());
        assert!(app
            .project_transfer(std::slice::from_ref(&source), &target, true)
            .is_err());
        assert_eq!(
            std::fs::read(target.join("source/image.bin")).unwrap(),
            [0, 255, 1, 2]
        );
    }

    #[test]
    fn module_drag_move_updates_import_aliases_and_preserves_source_relative_imports() {
        let root = tempfile::tempdir().unwrap();
        let left = root.path().join("left");
        let right = root.path().join("right");
        std::fs::create_dir(&left).unwrap();
        std::fs::create_dir(&right).unwrap();
        for package in [&left, &right] {
            std::fs::write(package.join("__init__.py"), "").unwrap();
        }
        let module = left.join("module.py");
        let consumer = root.path().join("consumer.py");
        std::fs::write(left.join("helper.py"), "value = 1\n").unwrap();
        std::fs::write(&module, "from .helper import value\n").unwrap();
        std::fs::write(&consumer,"from left import module as chosen\nimport left.module\nchosen.value\nleft.module.value\n").unwrap();
        let mut app = app(root.path());
        app.open_file(&consumer);
        app.project_transfer(std::slice::from_ref(&module), &right, false)
            .unwrap();
        assert!(!module.exists());
        assert_eq!(
            std::fs::read_to_string(right.join("module.py")).unwrap(),
            "from left.helper import value\n"
        );
        let text = app
            .tabs
            .iter()
            .find(|tab| tab.state.path.as_ref() == Some(&consumer))
            .unwrap()
            .state
            .doc
            .text();
        assert!(text.contains("from right import module as chosen"));
        assert!(text.contains("import right.module"));
        assert!(text.contains("right.module.value"));
        assert!(text.contains("chosen.value"));
    }

    #[test]
    fn gist_confirmation_captures_immutable_contents_and_defaults_to_secret() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("π.py");
        std::fs::write(&path, "π = 1\n").unwrap();
        let mut app = app(root.path());
        // Opening a project does not implicitly open its files. Keep a real
        // editor buffer to mutate after the preview captures its snapshot.
        app.open_file(&path);
        let ctx = egui::Context::default();
        app.project_create_gist(path.clone(), &ctx);
        let composer = app.github.panel.composer.as_ref().unwrap();
        assert_eq!(composer.paths.first(), Some(&path));
        assert_eq!(composer.files.get("π.py").unwrap(), "π = 1\n");
        assert!(composer.private);
        assert!(!app.github.busy());
        app.tabs[app.active.unwrap()].state.apply_text_edits(
            vec![(TextRange::empty(0.into()), "# later\n".into())],
            "Edit",
        );
        assert_eq!(
            app.github
                .panel
                .composer
                .as_ref()
                .unwrap()
                .files
                .get("π.py")
                .unwrap(),
            "π = 1\n"
        );
    }

    #[test]
    fn retained_console_gist_uses_bound_secret_composer_without_publishing() {
        let root = tempfile::tempdir().unwrap();
        let mut app = app(root.path());
        let unrelated = root.path().join("unrelated.py");
        std::fs::write(&unrelated, "value = 1\n").unwrap();
        app.open_file(&unrelated);
        app.console_create_gist("retained output π\n".into());
        let composer = app.github.panel.composer.as_ref().unwrap();
        assert_eq!(composer.kind, crate::github_panel::ComposeKind::Gist);
        assert!(composer.bound && composer.private);
        assert_eq!(
            composer.files.get("console-output.log").unwrap(),
            "retained output π\n"
        );
        assert!(matches!(app.dialog, Dialog::GitHubModal));
        assert!(
            !app.github.busy(),
            "preview must not publish or start a request"
        );
        app.tabs[app.active.unwrap()].state.apply_text_edits(
            vec![(
                TextRange::empty(0.into()),
                "# unrelated editor edit\n".into(),
            )],
            "Edit",
        );
        assert_eq!(
            app.github
                .panel
                .composer
                .as_ref()
                .unwrap()
                .files
                .get("console-output.log")
                .unwrap(),
            "retained output π\n"
        );
    }

    #[test]
    fn copy_refuses_dirty_buffers_and_recursive_move_without_writing_destinations() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.py");
        let target = root.path().join("target");
        std::fs::write(&source, "value = 1\n").unwrap();
        std::fs::create_dir(&target).unwrap();
        let mut app = app(root.path());
        app.open_file(&source);
        app.tabs[app.active.unwrap()].state.apply_text_edits(
            vec![(TextRange::empty(0.into()), "# dirty\n".into())],
            "Edit",
        );
        assert!(app
            .project_transfer(std::slice::from_ref(&source), &target, true)
            .is_err());
        assert!(!target.join("source.py").exists());
        assert!(app
            .project_transfer(std::slice::from_ref(&target), &target, false)
            .is_err());
        assert!(source.exists());
    }
    #[test]
    fn copying_to_a_new_name_in_the_same_directory_uses_the_captured_target() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.bin");
        std::fs::write(&path, [0, 255, 4]).unwrap();
        let mut app = app(root.path());
        app.project_transfer_named(
            std::slice::from_ref(&path),
            root.path(),
            true,
            Some("copy.bin"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.path().join("copy.bin")).unwrap(),
            [0, 255, 4]
        );
        assert!(app
            .project_transfer_named(
                std::slice::from_ref(&path),
                root.path(),
                true,
                Some("../escape.bin")
            )
            .is_err());
        assert_eq!(std::fs::read(path).unwrap(), [0, 255, 4]);
    }
}
