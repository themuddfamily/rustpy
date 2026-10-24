//! Settings › Appearance & Behavior › Path Variables: IDE-level `$NAME$` folders that project files under `.rustpy/`,
//! run configurations and external tools use. `$USER_HOME$` is built in.
//! Opening a project that uses a name the IDE does not define posts an
//! "Undefined path variables" notification leading here.

use super::*;
use crate::notifications::{NotificationAction, NotificationKind};
use rustpy_settings::path_variables::{self as pv, PathVariable, PathVariables};

/// Page state that is not a setting.
#[derive(Default)]
pub(super) struct PathVariablesPage {
    selected: Option<usize>,
    /// The folder picker: the row it chooses for and the folder it shows.
    browsing: Option<(usize, PathBuf)>,
    new_ignored: String,
    /// Names the open project uses without a definition.
    pub undefined: Vec<String>,
}

/// A name for a new variable that no other variable has.
fn fresh_name(vars: &PathVariables) -> String {
    (1..)
        .map(|i| {
            if i == 1 {
                "NEW_VARIABLE".to_string()
            } else {
                format!("NEW_VARIABLE_{i}")
            }
        })
        .find(|name| !vars.variables.iter().any(|v| &v.name == name))
        .expect("an unused name")
}

/// Why row `index` cannot be saved (shown next to the row).
fn row_problem(vars: &PathVariables, index: usize) -> Option<String> {
    let var = &vars.variables[index];
    if !pv::is_valid_name(&var.name) {
        return Some("Use letters, digits and _, not starting with a digit".into());
    }
    if pv::RESERVED.contains(&var.name.as_str()) {
        return Some("Defined by the IDE".into());
    }
    if vars.variables[..index].iter().any(|v| v.name == var.name) {
        return Some("Name used twice".into());
    }
    if var.path.as_os_str().is_empty() {
        return Some("Choose a folder".into());
    }
    if !var.path.is_absolute() {
        return Some("Must be an absolute path".into());
    }
    if !var.path.is_dir() {
        return Some("Folder does not exist (allowed)".into());
    }
    None
}

/// Sub-folders of `dir`, sorted by name (hidden ones last), for the picker.
fn subfolders(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .take(500)
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|p| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        (name.starts_with('.'), name)
    });
    out
}

impl IdeApp {
    pub(super) fn page_path_variables(&mut self, ui: &mut egui::Ui) {
        let dim = self.theme.text_dim;
        let error = self.theme.error;
        ui.heading(crate::i18n::tr_static("Path Variables"));
        ui.label(egui::RichText::new(crate::i18n::tr_static("Project files in .rustpy (project.json, shared run configurations) store paths inside these folders as $NAME$, so the project works where the folders are elsewhere. Run configurations and external tools expand $NAME$ too.")).color(dim));
        ui.add_space(6.0);

        let page = &mut self.settings_ui.path_variables;
        let vars = &mut self.settings.path_variables;
        page.undefined.retain(|name| !vars.is_known(name));
        if !page.undefined.is_empty() {
            ui.group(|ui| {
                ui.label(
                    egui::RichText::new(crate::i18n::tr_static(
                        "Undefined path variables used by this project",
                    ))
                    .strong(),
                );
                let mut define = None;
                let mut ignore = None;
                for name in &page.undefined {
                    ui.horizontal(|ui| {
                        ui.monospace(format!("${name}$"));
                        if ui.button(crate::i18n::tr_static("Define…")).clicked() {
                            define = Some(name.clone());
                        }
                        if ui.button(crate::i18n::tr_static("Ignore")).clicked() {
                            ignore = Some(name.clone());
                        }
                    });
                }
                if let Some(name) = define {
                    vars.variables.push(PathVariable {
                        name,
                        path: PathBuf::new(),
                    });
                    let row = vars.variables.len() - 1;
                    page.selected = Some(row);
                    page.browsing =
                        Some((row, pv::user_home().unwrap_or_else(|| PathBuf::from("/"))));
                }
                if let Some(name) = ignore {
                    vars.ignored.push(name);
                }
            });
            ui.add_space(6.0);
        }

        let mut remove = None;
        egui::Grid::new("path-variables-grid")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                ui.label(egui::RichText::new(crate::i18n::tr_static("Name")).strong());
                ui.label(egui::RichText::new(crate::i18n::tr_static("Value")).strong());
                ui.label("");
                ui.label("");
                ui.end_row();
                // Built in, read-only.
                ui.label(egui::RichText::new(pv::USER_HOME).color(dim));
                ui.label(
                    egui::RichText::new(
                        pv::user_home()
                            .map(|h| h.display().to_string())
                            .unwrap_or_default(),
                    )
                    .color(dim),
                )
                .on_hover_text(crate::i18n::tr_static(
                    "Built in: the user's home directory (read-only)",
                ));
                ui.label("");
                ui.label("");
                ui.end_row();
                let count = vars.variables.len();
                for index in 0..count {
                    let problem = row_problem(vars, index);
                    let var = &mut vars.variables[index];
                    let name = ui.add(
                        egui::TextEdit::singleline(&mut var.name)
                            .desired_width(160.0)
                            .hint_text("NAME"),
                    );
                    if name.changed() {
                        var.name.retain(|c| c.is_ascii_alphanumeric() || c == '_');
                    }
                    let mut text = var.path.display().to_string();
                    let value = ui.add(
                        egui::TextEdit::singleline(&mut text)
                            .desired_width(320.0)
                            .hint_text(crate::i18n::tr_static("Absolute folder path")),
                    );
                    if value.changed() {
                        var.path = PathBuf::from(text.trim());
                    }
                    if name.gained_focus() || value.gained_focus() {
                        page.selected = Some(index);
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .button("…")
                            .on_hover_text(crate::i18n::tr_static("Choose folder"))
                            .clicked()
                        {
                            let start = Some(var.path.clone())
                                .filter(|p| p.is_dir())
                                .or_else(pv::user_home)
                                .unwrap_or_else(|| PathBuf::from("/"));
                            page.browsing = Some((index, start));
                            page.selected = Some(index);
                        }
                        if ui.button(crate::i18n::tr_static("Remove")).clicked() {
                            remove = Some(index);
                        }
                    });
                    match problem {
                        Some(p) if p.ends_with("(allowed)") => {
                            ui.label(egui::RichText::new(p).color(dim))
                        }
                        Some(p) => ui.label(egui::RichText::new(p).color(error)),
                        None => ui.label(""),
                    };
                    ui.end_row();
                }
            });
        if let Some(index) = remove {
            vars.variables.remove(index);
            page.selected = None;
            page.browsing = None;
        }
        ui.horizontal(|ui| {
            if ui.button(crate::i18n::tr_static("Add")).clicked() {
                let name = fresh_name(vars);
                vars.variables.push(PathVariable {
                    name,
                    path: PathBuf::new(),
                });
                page.selected = Some(vars.variables.len() - 1);
            }
        });

        // Folder picker for one row.
        let mut close = false;
        if let Some((row, dir)) = &mut page.browsing {
            if *row >= vars.variables.len() {
                close = true;
            } else {
                ui.add_space(6.0);
                ui.group(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "Choose the folder of ${}$",
                            vars.variables[*row].name
                        ))
                        .strong(),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                dir.parent().is_some(),
                                egui::Button::new(crate::i18n::tr_static("Up")),
                            )
                            .clicked()
                        {
                            if let Some(parent) = dir.parent() {
                                *dir = parent.to_path_buf();
                            }
                        }
                        ui.monospace(dir.display().to_string());
                    });
                    let mut open = None;
                    egui::ScrollArea::vertical()
                        .id_salt("path-variables-picker")
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for sub in subfolders(dir) {
                                let label = sub
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                if ui.selectable_label(false, format!("{label}/")).clicked() {
                                    open = Some(sub);
                                }
                            }
                        });
                    if let Some(sub) = open {
                        *dir = sub;
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .button(crate::i18n::tr_static("Select This Folder"))
                            .clicked()
                        {
                            vars.variables[*row].path = dir.clone();
                            close = true;
                        }
                        if ui.button(crate::i18n::tr_static("Cancel")).clicked() {
                            close = true;
                        }
                    });
                });
            }
        }
        if close {
            page.browsing = None;
        }

        ui.add_space(10.0);
        ui.label(egui::RichText::new(crate::i18n::tr_static("Ignored variables")).strong());
        ui.label(egui::RichText::new(crate::i18n::tr_static("$NAME$ tokens that are not folders (a tool's own placeholders); they are never reported as undefined.")).color(dim));
        let mut unignore = None;
        for (index, name) in vars.ignored.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.monospace(name);
                if ui.small_button(crate::i18n::tr_static("Remove")).clicked() {
                    unignore = Some(index);
                }
            });
        }
        if let Some(index) = unignore {
            vars.ignored.remove(index);
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut page.new_ignored)
                    .desired_width(160.0)
                    .hint_text("NAME"),
            );
            let name = page.new_ignored.trim().trim_matches('$').to_string();
            let valid = pv::is_valid_name(&name) && !vars.ignored.contains(&name);
            if ui
                .add_enabled(valid, egui::Button::new(crate::i18n::tr_static("Add")))
                .clicked()
            {
                vars.ignored.push(name);
                page.new_ignored.clear();
            }
        });

        if let Err(problem) = vars.validate() {
            ui.add_space(6.0);
            ui.colored_label(error, problem);
        }
    }

    /// The project at `root` uses path variables the IDE does not define:
    /// offer to define them (Settings › Path Variables). Run-configuration
    /// macros like `$FilePath$` are not path variables.
    pub(super) fn notify_undefined_path_variables(&mut self, root: &Path) {
        let names = rustpy_project::project::undefined_path_variables(root, &|name| {
            crate::external_tools::MACROS
                .iter()
                .any(|(m, _)| *m == name)
        });
        if names.is_empty() {
            return;
        }
        let list = names
            .iter()
            .map(|n| format!("${n}$"))
            .collect::<Vec<_>>()
            .join(", ");
        self.settings_ui.path_variables.undefined = names;
        self.notify(
            NotificationKind::Warning,
            "Path Variables",
            "Undefined path variables",
            format!("The project's .rustpy files use {list}, which this IDE does not define. Paths that use them cannot be found until they are defined."),
            vec![(
                "Define Path Variables…".into(),
                NotificationAction::OpenSettings("path-variables"),
            )],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_variables_get_unused_names_and_rows_report_problems() {
        let mut vars = PathVariables::default();
        assert_eq!(fresh_name(&vars), "NEW_VARIABLE");
        vars.variables.push(PathVariable {
            name: "NEW_VARIABLE".into(),
            path: PathBuf::new(),
        });
        assert_eq!(fresh_name(&vars), "NEW_VARIABLE_2");
        assert_eq!(row_problem(&vars, 0).as_deref(), Some("Choose a folder"));
        vars.variables.push(PathVariable {
            name: "USER_HOME".into(),
            path: std::env::temp_dir(),
        });
        assert_eq!(row_problem(&vars, 1).as_deref(), Some("Defined by the IDE"));
        vars.variables[1].name = "NEW_VARIABLE".into();
        assert_eq!(row_problem(&vars, 1).as_deref(), Some("Name used twice"));
        vars.variables[1].name = "TMP".into();
        assert_eq!(row_problem(&vars, 1), None);
        vars.variables[1].path = PathBuf::from("relative");
        assert_eq!(
            row_problem(&vars, 1).as_deref(),
            Some("Must be an absolute path")
        );
    }

    #[test]
    fn undefined_detection_ignores_run_configuration_macros() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let store = rustpy_project::RunConfigStore::path_for(root);
        std::fs::create_dir_all(store.parent().unwrap()).unwrap();
        std::fs::write(
            &store,
            r#"{"configs":[{"name":"x","parameters":"$FilePath$","working_dir":"$PV_UI_MISSING$/w"}]}"#,
        )
        .unwrap();
        let names = rustpy_project::project::undefined_path_variables(root, &|name| {
            crate::external_tools::MACROS
                .iter()
                .any(|(m, _)| *m == name)
        });
        assert_eq!(names, vec!["PV_UI_MISSING".to_string()]);
    }
}
