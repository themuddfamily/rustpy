//! Project structure (roadmap §4 "Project structure"): Mark Directory as, the
//! Settings › Project › Project Structure page, and where the roots take
//! effect (module resolution, PYTHONPATH, Run All Tests, the file index). The
//! structure is saved in the shareable `.rustpy/project.json`.

use super::*;
use crate::a11y::AccessibleCombo;
use rustpy_project::project::{ProjectModel, RootKind};

/// Widget state of the Project Structure page (kept in egui memory).
#[derive(Clone, Default)]
struct PageState {
    new_root: String,
    name: Option<String>,
    message: Option<String>,
    /// Add Content Root › Browse…: picks a folder.
    browser: Option<FileBrowser>,
}

/// Language of a file in a folder marked Template Folder: HTML and text files
/// there are Jinja2 / Django templates (like those in a `templates` folder).
fn template_folder_language(
    path: &Path,
    base: Language,
    template_dirs: &std::collections::HashSet<PathBuf>,
) -> Language {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let candidate = matches!(base, Language::Html | Language::PlainText)
        && matches!(ext.as_str(), "html" | "htm" | "xml" | "txt");
    if candidate && template_dirs.iter().any(|d| path.starts_with(d)) {
        Language::Template
    } else {
        base
    }
}

/// Run All Tests targets: the existing Test Sources roots, relative to the project.
fn test_targets(root: &Path, test_roots: &[PathBuf]) -> Vec<String> {
    let mut dirs: Vec<&PathBuf> = test_roots.iter().filter(|d| d.is_dir()).collect();
    dirs.sort();
    dirs.into_iter()
        .map(|d| {
            d.strip_prefix(root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| d.display().to_string())
        })
        .collect()
}

/// A content root typed on the page: `~` expands, relative paths are taken
/// from the project directory. It must be an existing folder outside the project.
fn parse_content_root(
    input: &str,
    project: &Path,
    model: &ProjectModel,
) -> Result<PathBuf, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("Enter a folder path".into());
    }
    let path = match raw
        .strip_prefix("~/")
        .or(if raw == "~" { Some("") } else { None })
    {
        Some(rest) => dirs::home_dir()
            .map(|h| h.join(rest))
            .ok_or("No home directory")?,
        None if Path::new(raw).is_absolute() => PathBuf::from(raw),
        None => project.join(raw),
    };
    let path = path
        .canonicalize()
        .map_err(|_| format!("{} does not exist", path.display()))?;
    if !path.is_dir() {
        return Err(format!("{} is not a folder", path.display()));
    }
    if path.starts_with(project) {
        return Err("That folder is already inside the project".into());
    }
    if project.starts_with(&path) {
        return Err("A content root cannot contain the project directory".into());
    }
    if model.content_roots.contains(&path) {
        return Err("That folder is already a content root".into());
    }
    if let Some(other) = model
        .content_roots
        .iter()
        .find(|r| path.starts_with(r) || r.starts_with(&path))
    {
        return Err(format!(
            "That folder overlaps the content root {}",
            other.display()
        ));
    }
    Ok(path)
}

/// Sub-folders shown in the page's tree (no VCS, cache or hidden folders).
fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                        !n.starts_with('.') && !matches!(n, "__pycache__" | "node_modules")
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

impl IdeApp {
    /// Project view › Mark Directory as (`None` unmarks the folder).
    pub(super) fn mark_directory(&mut self, dir: &Path, kind: Option<RootKind>) {
        // After a load error the structure in memory is not the file's: when the
        // file was fixed meanwhile (unnoticed), load it and mark on top of it;
        // while it is still broken `apply_project_model` refuses the change.
        if self.project_index.structure_error.is_some()
            && self
                .project
                .as_ref()
                .is_some_and(|p| ProjectModel::try_load(&p.root).is_ok())
        {
            self.reload_project_structure();
        }
        let Some(p) = &self.project else { return };
        let mut model = p.tree.model();
        let before = model.kind_of(dir);
        model.mark(dir, kind);
        if !self.apply_project_model(model) {
            return;
        }
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        match (kind, before) {
            (Some(k), _) => self.set_status(format!("Marked {name} as {}", k.label())),
            (None, Some(k)) => self.set_status(format!("Unmarked {name} as {}", k.label())),
            (None, None) => {}
        }
    }

    /// Applies an edited structure: the Project view, `project.json`, import
    /// resolution and the file index (search, navigation and refactoring scope).
    /// Refused (false) while `project.json` cannot be read: saving would replace
    /// what the user has to fix, such as both sides of a merge conflict.
    pub(super) fn apply_project_model(&mut self, model: ProjectModel) -> bool {
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            return false;
        };
        let readable = match self.project_index.structure_error.clone() {
            Some(e) => Err(e),
            None => ProjectModel::check_replaceable(&root),
        };
        if let Err(e) = readable {
            self.project_index.structure_error = Some(e.clone());
            let actions = vec![(
                "Open project.json".to_string(),
                crate::notifications::NotificationAction::OpenFile(
                    ProjectModel::path_for(&root),
                    None,
                ),
            )];
            self.notify(crate::notifications::NotificationKind::Error, "Project Structure", "The project structure was not changed", format!("{e}\nFix .rustpy/project.json (or delete it) first; RustPy does not overwrite it."), actions);
            return false;
        }
        if let Err(e) = model.save(&root) {
            self.notify_error(
                "Project Structure",
                "Cannot save the project structure",
                format!("{}: {e}", ProjectModel::path_for(&root).display()),
            );
            return false;
        }
        self.use_project_model(model);
        true
    }

    /// The project opened: its structure from `project.json` (or the roots older
    /// versions kept in `workspace.json`; nothing is written until the structure
    /// changes). A file that cannot be read is reported; the structure stays
    /// empty and cannot be changed until the file is fixed (it is reloaded when
    /// it changes).
    pub(super) fn install_prepared_project_structure(
        &mut self,
        root: &Path,
        model: ProjectModel,
        ignored: &[String],
    ) {
        self.project_index.structure_error = None;
        self.notify_ignored_content_roots(root, ignored);
        self.notify_undefined_path_variables(root);
        if let Some(project) = &mut self.project {
            project.tree.set_model(&model);
        }
    }

    pub(super) fn load_project_structure(&mut self, root: &Path) {
        let model = match ProjectModel::try_load_reporting(root) {
            Ok((model, ignored)) => {
                self.project_index.structure_error = None;
                self.notify_ignored_content_roots(root, &ignored);
                self.notify_undefined_path_variables(root);
                model
            }
            Err(e) => {
                self.project_index.structure_error = Some(e.clone());
                self.notify_project_file_error(root, e);
                ProjectModel::default()
            }
        };
        if let Some(p) = &mut self.project {
            p.tree.set_model(&model);
        }
    }

    /// `project.json` changed on disk (a pull, an edit in the editor): the new
    /// structure takes effect; our own saves change nothing.
    pub(super) fn reload_project_structure(&mut self) {
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            return;
        };
        match ProjectModel::try_load_reporting(&root) {
            Ok((model, ignored)) => {
                let was_unreadable = self.project_index.structure_error.take().is_some();
                if self
                    .project
                    .as_ref()
                    .is_some_and(|p| p.tree.model() != model)
                {
                    self.notify_ignored_content_roots(&root, &ignored);
                    self.use_project_model(model);
                    self.set_status("Project structure reloaded from .rustpy/project.json");
                } else if was_unreadable {
                    self.set_status("Project structure: .rustpy/project.json can be read again");
                }
            }
            Err(e) => {
                self.project_index.structure_error = Some(e.clone());
                self.notify_project_file_error(&root, e);
            }
        }
    }

    fn notify_project_file_error(&mut self, root: &Path, error: String) {
        let actions = vec![(
            "Open project.json".to_string(),
            crate::notifications::NotificationAction::OpenFile(ProjectModel::path_for(root), None),
        )];
        self.notify(crate::notifications::NotificationKind::Error, "Project Structure", "Cannot read the project structure", format!("{error}\nContent roots and folder marks apply again once the file is fixed; until then RustPy does not change it."), actions);
    }

    /// Content roots of `project.json` that are not used (they contain the
    /// project, lie inside it or overlap another content root).
    fn notify_ignored_content_roots(&mut self, root: &Path, ignored: &[String]) {
        if ignored.is_empty() {
            return;
        }
        let actions = vec![(
            "Open project.json".to_string(),
            crate::notifications::NotificationAction::OpenFile(ProjectModel::path_for(root), None),
        )];
        self.notify(
            crate::notifications::NotificationKind::Warning,
            "Project Structure",
            "Content roots ignored",
            format!(
                "These content roots of .rustpy/project.json are not used:\n{}",
                ignored.join("\n")
            ),
            actions,
        );
    }

    /// Puts a structure into effect without saving it: the Project view, import
    /// resolution, the file index and the language of open template files.
    fn use_project_model(&mut self, model: ProjectModel) {
        let Some(p) = &mut self.project else { return };
        p.tree.set_model(&model);
        if let Some(sdk) = model.sdk.as_deref() {
            p.interpreter = p
                .interpreters
                .iter()
                .find(|interpreter| interpreter.path == sdk)
                .cloned();
        }
        p.tree.invalidate_all();
        self.apply_interpreter_paths();
        self.rebuild_file_index();
        // Open HTML files follow Template Folder marks.
        let Some(p) = &self.project else { return };
        let template_dirs = p.tree.template_dirs.clone();
        for tab in &mut self.tabs {
            let Some(path) = tab.state.path.clone() else {
                continue;
            };
            let base = Language::from_path(&path);
            if tab.state.language == base
                || (tab.state.language == Language::Template && base != Language::Template)
            {
                tab.state
                    .set_language(template_folder_language(&path, base, &template_dirs));
            }
        }
    }

    /// A new editor tab: HTML files in a Template Folder open as templates.
    pub(super) fn apply_template_folders(&self, state: &mut EditorState) {
        let (Some(p), Some(path)) = (&self.project, state.path.as_deref()) else {
            return;
        };
        if !p.tree.template_dirs.is_empty() {
            state.language = template_folder_language(path, state.language, &p.tree.template_dirs);
        }
    }

    /// Run All Tests: the Test Sources roots when the project has some, else the whole project.
    pub(super) fn default_test_targets(&self) -> Vec<String> {
        let Some(p) = &self.project else {
            return Vec::new();
        };
        let roots: Vec<PathBuf> = p.tree.test_roots.iter().cloned().collect();
        test_targets(&p.root, &roots)
    }

    /// Settings › Project › Project Structure: content roots and folder marks.
    pub(super) fn page_project_structure(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        ui.heading("Project Structure");
        let Some((root, mut model)) = self
            .project
            .as_ref()
            .map(|p| (p.root.clone(), p.tree.model()))
        else {
            ui.label("Open a project first.");
            return;
        };
        if let Some(e) = self.project_index.structure_error.clone() {
            // Editing would replace a file the user has to fix (a merge conflict).
            ui.label(
                egui::RichText::new(format!("The project structure cannot be read: {e}"))
                    .color(theme.error),
            );
            ui.label(egui::RichText::new("Fix .rustpy/project.json (or delete it) to edit the structure here; RustPy does not overwrite it.").color(theme.text_dim));
            if ui
                .button("Reload")
                .on_hover_text("Read .rustpy/project.json again")
                .clicked()
            {
                self.reload_project_structure();
            }
            return;
        }
        ui.label(egui::RichText::new("Content roots and folder marks are saved in .rustpy/project.json, which you can commit so that everyone who clones the project gets the same layout. Changes apply at once.").color(theme.text_dim));
        let state_id = ui.id().with("project_structure_page");
        let mut st: PageState =
            ui.data_mut(|d| d.get_temp_mut_or_default::<PageState>(state_id).clone());
        let mut changed = false;
        ui.label(egui::RichText::new("Project structure is shared with this project. IDE appearance, trusted locations and interpreter discovery preferences are IDE settings. Workspace layout, open files and the display name stay local.").color(theme.text_dim));
        ui.horizontal(|ui| {
            ui.label("Project name:");
            let name = st
                .name
                .get_or_insert_with(|| ProjectModel::display_name(&root));
            ui.text_edit_singleline(name);
            if ui.button("Save Name").clicked() {
                match ProjectModel::save_display_name(&root, name) {
                    Ok(()) => {
                        if let Some(project) = &mut self.project {
                            project.tree.refresh_display_name();
                        }
                        st.message = Some("Project name saved in .rustpy/.name".into());
                    }
                    Err(error) => st.message = Some(format!("Cannot save project name: {error}")),
                }
            }
        });

        let interpreters = self
            .project
            .as_ref()
            .map(|project| {
                project
                    .interpreters
                    .iter()
                    .map(|interpreter| (interpreter.label(), interpreter.path.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Project SDK and language level").strong());
        changed |= sdk_language_controls(
            ui,
            "project-sdk",
            &interpreters,
            &mut model.sdk,
            &mut model.language_level,
        );
        ui.label(egui::RichText::new("Attached modules inherit these values unless overridden. SDKs must be configured in Python Interpreter settings; safe mode does not execute them.").color(theme.text_dim));
        for content_root in model.content_roots.clone() {
            let mut module = model
                .modules
                .iter()
                .find(|module| module.content_root == content_root)
                .cloned()
                .unwrap_or_else(|| rustpy_project::project::ProjectModule {
                    content_root: content_root.clone(),
                    ..Default::default()
                });
            ui.push_id(&content_root, |ui| {
                ui.label(format!(
                    "Module: {}",
                    ProjectModel::display_name(&content_root)
                ));
                if sdk_language_controls(
                    ui,
                    "module-sdk",
                    &interpreters,
                    &mut module.sdk,
                    &mut module.language_level,
                ) {
                    model
                        .modules
                        .retain(|entry| entry.content_root != content_root);
                    model.modules.push(module);
                    changed = true;
                }
            });
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Content Roots").strong());
        ui.horizontal(|ui| {
            ui.label(root.display().to_string());
            ui.label(egui::RichText::new("(project directory)").color(theme.text_dim));
        });
        for r in model.content_roots.clone() {
            ui.horizontal(|ui| {
                let missing = !r.is_dir();
                ui.label(
                    egui::RichText::new(r.display().to_string()).color(if missing {
                        theme.text_disabled
                    } else {
                        theme.text
                    }),
                );
                if missing {
                    ui.label(egui::RichText::new("(missing)").color(theme.warning));
                }
                if ui
                    .small_button("Remove")
                    .on_hover_text("Detach this folder (and the marks inside it) from the project")
                    .clicked()
                {
                    model.remove_content_root(&r);
                    changed = true;
                }
            });
        }
        let mut add: Option<String> = None;
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut st.new_root)
                    .hint_text("/path/to/folder (~ and paths relative to the project allowed)")
                    .desired_width(380.0),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Add Content Root").clicked() || enter {
                add = Some(st.new_root.clone());
            }
            if ui
                .button("Browse…")
                .on_hover_text("Pick the folder to attach")
                .clicked()
            {
                st.browser = match st.browser {
                    Some(_) => None,
                    None => Some(FileBrowser::new(
                        root.parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_else(|| root.clone()),
                    )),
                };
            }
        });
        let mut close_browser = false;
        if let Some(browser) = &mut st.browser {
            egui::Frame::new()
                .stroke(egui::Stroke::new(1.0, theme.border))
                .inner_margin(egui::Margin::same(6))
                .corner_radius(4)
                .show(ui, |ui| {
                    browser.ui(ui, &theme, false, 160.0);
                    // The folder clicked in the list, else the one shown.
                    let pick = browser
                        .selected
                        .clone()
                        .filter(|p| p.is_dir())
                        .unwrap_or_else(|| browser.dir.clone());
                    ui.horizontal(|ui| {
                        if ui.button(format!("Add {}", pick.display())).clicked() {
                            add = Some(pick.display().to_string());
                        }
                        if ui.button("Cancel").clicked() {
                            close_browser = true;
                        }
                    });
                });
        }
        if close_browser {
            st.browser = None;
            st.message = None;
        }
        if let Some(input) = add {
            match parse_content_root(&input, &root, &model) {
                Ok(path) => {
                    model.add_content_root(&path);
                    st.new_root.clear();
                    st.message = None;
                    st.browser = None;
                    changed = true;
                }
                Err(e) => st.message = Some(e),
            }
        }
        if let Some(m) = &st.message {
            ui.label(egui::RichText::new(m).color(theme.warning));
        }

        ui.add_space(8.0);
        ui.label(egui::RichText::new("Folders").strong());
        ui.label(
            egui::RichText::new("Expand a folder and choose what it is marked as.")
                .color(theme.text_dim)
                .small(),
        );
        let tops: Vec<PathBuf> = std::iter::once(root.clone())
            .chain(
                model
                    .content_roots
                    .iter()
                    .filter(|r| !r.starts_with(&root))
                    .cloned(),
            )
            .collect();
        egui::Frame::new()
            .fill(theme.panel_bg_darker)
            .inner_margin(egui::Margin::same(6))
            .corner_radius(4)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("project_structure_tree")
                    .max_height(260.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for top in &tops {
                            changed |= structure_tree_row(ui, &theme, top, &mut model, true);
                        }
                    });
            });

        for kind in RootKind::ALL {
            let dirs = model.dirs(kind).to_vec();
            if dirs.is_empty() {
                continue;
            }
            ui.add_space(6.0);
            let color =
                crate::project_tree::root_kind_color(kind, &theme).unwrap_or(theme.text_dim);
            ui.label(egui::RichText::new(kind.plural()).strong().color(color));
            for d in dirs {
                ui.horizontal(|ui| {
                    let shown = d
                        .strip_prefix(&root)
                        .map(|r| r.display().to_string())
                        .unwrap_or_else(|_| d.display().to_string());
                    ui.label(if d.is_dir() {
                        egui::RichText::new(shown)
                    } else {
                        egui::RichText::new(format!("{shown} (missing)")).color(theme.text_disabled)
                    });
                    if ui
                        .small_button("×")
                        .on_hover_text(format!("Unmark as {}", kind.label()))
                        .clicked()
                    {
                        model.mark(&d, None);
                        changed = true;
                    }
                });
            }
        }
        ui.data_mut(|d| d.insert_temp(state_id, st));
        if changed {
            self.apply_project_model(model);
        }
    }
}

fn sdk_language_controls(
    ui: &mut egui::Ui,
    id: &str,
    interpreters: &[(String, PathBuf)],
    sdk: &mut Option<PathBuf>,
    level: &mut Option<(u32, u32)>,
) -> bool {
    let before = (sdk.clone(), *level);
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt((id, "sdk"))
            .selected_text(
                sdk.as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "Inherit SDK".into()),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(sdk, None, "Inherit SDK");
                for (label, path) in interpreters {
                    ui.selectable_value(
                        sdk,
                        Some(path.clone()),
                        format!("{label} — {}", path.display()),
                    );
                }
            })
            .combo_named(crate::i18n::tr_static("SDK"));
        egui::ComboBox::from_id_salt((id, "language"))
            .selected_text(
                level
                    .map(|(major, minor)| format!("Python {major}.{minor}"))
                    .unwrap_or_else(|| "SDK language level".into()),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(level, None, "SDK language level");
                for minor in 7..=15 {
                    ui.selectable_value(level, Some((3, minor)), format!("Python 3.{minor}"));
                }
            })
            .combo_named(crate::i18n::tr_static("Language level"));
    });
    before != (sdk.clone(), *level)
}

/// One folder of the page's tree with its mark menu; children load when it is
/// expanded. Returns true when a mark changed.
fn structure_tree_row(
    ui: &mut egui::Ui,
    theme: &UiTheme,
    dir: &Path,
    model: &mut ProjectModel,
    top: bool,
) -> bool {
    let mut changed = false;
    let id = ui.make_persistent_id(("project_structure_dir", dir));
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    let kind = model.kind_of(dir);
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, top)
        .show_header(ui, |ui| {
            let color = kind
                .and_then(|k| crate::project_tree::root_kind_color(k, theme))
                .unwrap_or(if kind.is_some() {
                    theme.text_disabled
                } else {
                    theme.text
                });
            ui.label(egui::RichText::new(&name).color(color));
            {
                let label = kind.map(|k| k.label()).unwrap_or("Mark as…");
                ui.menu_button(
                    egui::RichText::new(label).small().color(theme.text_dim),
                    |ui| {
                        for k in RootKind::ALL {
                            if ui
                                .selectable_label(kind == Some(k), k.label())
                                .on_hover_text(crate::project_tree::root_kind_hint(k))
                                .clicked()
                            {
                                model.mark(dir, if kind == Some(k) { None } else { Some(k) });
                                changed = true;
                                ui.close();
                            }
                        }
                        if kind.is_some() {
                            ui.separator();
                            if ui.button("Unmark").clicked() {
                                model.mark(dir, None);
                                changed = true;
                                ui.close();
                            }
                        }
                    },
                );
            }
        })
        .body(|ui| {
            // Nothing inside an excluded folder is part of the project.
            if kind == Some(RootKind::Excluded) {
                return;
            }
            for child in child_dirs(dir) {
                changed |= structure_tree_row(ui, theme, &child, model, false);
            }
        });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_dictionaries_are_scoped_and_reload_on_disk_changes() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("one");
        let second = directory.path().join("two");
        let path = first.join(".rustpy/dictionaries/project.txt");
        rustpy_spell::add_user_word(&path, "Rustpyscopedfixtureword").unwrap();
        let original = spell_dictionary_for(Some(&first));
        assert!(original.contains("Rustpyscopedfixtureword"));
        assert!(!spell_dictionary_for(Some(&second)).contains("Rustpyscopedfixtureword"));
        assert!(Arc::ptr_eq(&original, &spell_dictionary_for(Some(&first))));
        rustpy_spell::add_user_word(&path, "Rustpysecondfixtureword").unwrap();
        let changed = spell_dictionary_for(Some(&first));
        assert!(!Arc::ptr_eq(&original, &changed));
        assert!(changed.contains("Rustpysecondfixtureword"));
    }

    #[test]
    fn content_roots_and_test_targets() {
        let base = std::env::temp_dir().join(format!("rustpy-structure-{}", std::process::id()));
        let root = base.join("app");
        for d in ["app/tests/unit", "app/src", "shared/lib", "app/.git"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        let root = root.canonicalize().unwrap();
        let shared = base.join("shared").canonicalize().unwrap();
        let mut model = ProjectModel::default();
        assert_eq!(
            parse_content_root("../shared", &root, &model),
            Ok(shared.clone())
        );
        assert_eq!(
            parse_content_root(&shared.display().to_string(), &root, &model),
            Ok(shared.clone())
        );
        assert!(parse_content_root("src", &root, &model)
            .unwrap_err()
            .contains("inside the project"));
        assert!(parse_content_root("../nope", &root, &model)
            .unwrap_err()
            .contains("does not exist"));
        assert!(parse_content_root("  ", &root, &model).is_err());
        model.add_content_root(&shared);
        assert!(parse_content_root("../shared", &root, &model)
            .unwrap_err()
            .contains("already a content root"));
        assert!(
            parse_content_root("../shared/lib", &root, &model)
                .unwrap_err()
                .contains("overlaps"),
            "nested content roots would be listed twice"
        );
        assert!(
            parse_content_root("..", &root, &model)
                .unwrap_err()
                .contains("contain the project"),
            "the parent would pull in every sibling"
        );
        assert_eq!(
            test_targets(
                &root,
                &[
                    root.join("tests/unit"),
                    root.join("gone"),
                    shared.join("lib")
                ]
            ),
            vec![
                "tests/unit".to_string(),
                shared.join("lib").display().to_string()
            ]
        );
        assert_eq!(
            child_dirs(&root),
            vec![root.join("src"), root.join("tests")],
            "hidden folders are left out"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn template_folders_make_html_templates() {
        let dirs: std::collections::HashSet<PathBuf> =
            [PathBuf::from("/p/jinja")].into_iter().collect();
        let lang = |p: &str| {
            template_folder_language(Path::new(p), Language::from_path(Path::new(p)), &dirs)
        };
        assert_eq!(lang("/p/jinja/base.html"), Language::Template);
        assert_eq!(lang("/p/jinja/mail/welcome.txt"), Language::Template);
        assert_eq!(
            lang("/p/site/base.html"),
            Language::Html,
            "outside the marked folder"
        );
        assert_eq!(
            lang("/p/jinja/helpers.py"),
            Language::Python,
            "only HTML and text files"
        );
        assert_eq!(lang("/p/jinja/style.css"), Language::Css);
    }
}
