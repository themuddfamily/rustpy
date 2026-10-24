//! Run/Debug Configurations dialog (roadmap §3.1): add (Python script or
//! module, pytest, unittest, doctest, shell script), copy and remove
//! configurations; edit the target, parameters, per-configuration
//! interpreter, interpreter options, working directory, environment
//! variables (table) and `.env` files, PYTHONPATH options, "Emulate terminal
//! in output console", "Run with Python Console" and "Redirect input from";
//! Run or Debug the selected configuration.

use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use std::path::{Path, PathBuf};

use egui::Vec2;
use rustpy_project::{Interpreter, RunConfig, RunConfigKind, RunConfigStore};

use crate::theme::UiTheme;
mod workflow_form;
use rustpy_project::configuration_types::{ConfigurationType, TargetKind};

/// What the dialog asks the application to do.
#[derive(Clone, Debug, PartialEq)]
pub enum RunCfgEvent {
    Run(RunConfig),
    Debug(RunConfig),
    /// The store changed and should be saved.
    Changed,
}

pub struct DialogInput<'a> {
    pub root: &'a Path,
    pub interpreters: &'a [Interpreter],
    pub project_interpreter: Option<&'a Interpreter>,
}

/// Configuration kinds offered by the "+" menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewKind {
    Python,
    Pytest,
    Unittest,
    Doctest,
    Shell,
    Typed(ConfigurationType),
}

impl NewKind {
    pub const ALL: [NewKind; 5] = [
        NewKind::Python,
        NewKind::Pytest,
        NewKind::Unittest,
        NewKind::Doctest,
        NewKind::Shell,
    ];

    pub fn label(self) -> &'static str {
        match self {
            NewKind::Python => "Python",
            NewKind::Pytest => "pytest",
            NewKind::Unittest => "unittest",
            NewKind::Doctest => "Doctests",
            NewKind::Shell => "Shell Script",
            NewKind::Typed(kind) => kind.label(),
        }
    }
}

fn names(store: &RunConfigStore) -> Vec<&str> {
    store.configs.iter().map(|c| c.name.as_str()).collect()
}

/// A new permanent configuration of `kind` with a unique name.
pub fn new_config(kind: NewKind, store: &RunConfigStore, root: &Path) -> RunConfig {
    if let NewKind::Typed(kind) = kind {
        let cfg = rustpy_project::configuration_types::new_config(kind, root, &names(store));
        return rustpy_project::run_workflow::Templates::load(root).apply(&cfg);
    }
    let (base, kind_value, parameters) = match kind {
        NewKind::Python => (
            "Unnamed",
            RunConfigKind::Python {
                script: PathBuf::new(),
            },
            String::new(),
        ),
        NewKind::Pytest => (
            "pytest",
            RunConfigKind::Pytest {
                target: String::new(),
                keywords: String::new(),
            },
            String::new(),
        ),
        NewKind::Unittest => (
            "Unittests",
            RunConfigKind::Unittest {
                target: String::new(),
            },
            String::new(),
        ),
        NewKind::Doctest => (
            "Doctests",
            RunConfigKind::PythonModule {
                module: "doctest".into(),
            },
            "-v".to_string(),
        ),
        NewKind::Typed(_) => unreachable!(),
        NewKind::Shell => (
            "Shell Script",
            RunConfigKind::ShellScript {
                script: PathBuf::new(),
            },
            String::new(),
        ),
    };
    rustpy_project::run_workflow::Templates::load(root).apply(&RunConfig {
        name: RunConfig::unique_name(base, &names(store)),
        kind: kind_value,
        parameters,
        working_dir: Some(root.to_path_buf()),
        temporary: false,
        ..Default::default()
    })
}

/// Adds a copy of the configuration `name` ("name (1)", permanent) and selects it.
pub fn copy_config(store: &mut RunConfigStore, name: &str) -> Option<String> {
    let mut copy = store.configs.iter().find(|c| c.name == name)?.clone();
    copy.name = RunConfig::unique_name(name, &names(store));
    copy.temporary = false;
    let new_name = copy.name.clone();
    let pos = store
        .configs
        .iter()
        .position(|c| c.name == name)
        .map(|i| i + 1)
        .unwrap_or(store.configs.len());
    store.configs.insert(pos, copy);
    store.selected = Some(new_name.clone());
    Some(new_name)
}

/// Why a configuration cannot run as it is (shown under the form).
pub fn validate(cfg: &RunConfig, store: &RunConfigStore, root: &Path) -> Option<String> {
    if cfg.name.trim().is_empty() {
        return Some("The name is empty".into());
    }
    if store.configs.iter().filter(|c| c.name == cfg.name).count() > 1 {
        return Some(format!("Another configuration is named '{}'", cfg.name));
    }
    if let Some(spec) = &cfg.type_options {
        return spec.validate().err().or_else(|| cfg.input_problem(root));
    }
    match &cfg.kind {
        RunConfigKind::Python { script } if script.as_os_str().is_empty() => {
            return Some("Specify the script path".into())
        }
        RunConfigKind::PythonModule { module } if module.trim().is_empty() => {
            return Some("Specify the module name".into())
        }
        RunConfigKind::ShellScript { script } if script.as_os_str().is_empty() => {
            return Some("Specify the shell script".into())
        }
        RunConfigKind::Pytest { .. } | RunConfigKind::Unittest { .. } => return None,
        _ => {}
    }
    if cfg
        .redirect_input
        .as_ref()
        .is_some_and(|p| p.as_os_str().is_empty())
    {
        return Some("Specify the file to redirect input from".into());
    }
    cfg.input_problem(root)
}

/// Dialog-local editing state kept in egui memory: the name being typed and
/// the environment table rows of the selected configuration.
#[derive(Clone, Default)]
struct EditState {
    config: String,
    name: String,
    env_rows: Vec<(String, String)>,
}

fn env_rows(cfg: &RunConfig) -> Vec<(String, String)> {
    cfg.env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

const FIELD_WIDTH: f32 = 420.0;

/// Draws the dialog; `open` becomes false when it is closed.
pub fn show(
    ctx: &egui::Context,
    theme: &UiTheme,
    store: &mut RunConfigStore,
    input: &DialogInput,
    open: &mut bool,
) -> Vec<RunCfgEvent> {
    let mut events = Vec::new();
    let mut close = false;
    let state_id = egui::Id::new("run_config_dialog_state");
    egui::Window::new("Run/Debug Configurations")
        .collapsible(false)
        .resizable(true)
        .default_size([820.0, 560.0])
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .open(open)
        .show_modal(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("+ Add", |ui| {
                    for kind in ConfigurationType::ALL
                        .iter()
                        .map(|kind| NewKind::Typed(*kind))
                    {
                        if ui.button(kind.label()).clicked() {
                            let cfg = new_config(kind, store, input.root);
                            store.selected = Some(cfg.name.clone());
                            store.configs.push(cfg);
                            events.push(RunCfgEvent::Changed);
                            ui.close();
                        }
                    }
                });
                let selected = store
                    .selected
                    .clone()
                    .filter(|s| store.configs.iter().any(|c| &c.name == s));
                if ui
                    .add_enabled(selected.is_some(), egui::Button::new("− Remove"))
                    .clicked()
                {
                    if let Some(sel) = &selected {
                        let idx = store
                            .configs
                            .iter()
                            .position(|c| &c.name == sel)
                            .unwrap_or(0);
                        store.configs.retain(|c| &c.name != sel);
                        store.selected = store
                            .configs
                            .get(idx.min(store.configs.len().saturating_sub(1)))
                            .map(|c| c.name.clone());
                        events.push(RunCfgEvent::Changed);
                    }
                }
                if ui
                    .add_enabled(selected.is_some(), egui::Button::new("Copy"))
                    .on_hover_text("Copy the configuration")
                    .clicked()
                {
                    if let Some(sel) = &selected {
                        copy_config(store, sel);
                        events.push(RunCfgEvent::Changed);
                    }
                }
                workflow_form::template_toolbar(ui, store, input.root, &mut events);
            });
            ui.separator();
            ui.horizontal_top(|ui| {
                // Configuration list.
                ui.vertical(|ui| {
                    ui.set_width(200.0);
                    workflow_form::configuration_list(ui, theme, store);
                });
                ui.separator();
                ui.vertical(|ui| {
                    let Some(sel) = store.selected.clone() else {
                        ui.label(
                            egui::RichText::new("Select or add a configuration")
                                .color(theme.text_dim),
                        );
                        return;
                    };
                    let Some(idx) = store.configs.iter().position(|c| c.name == sel) else {
                        ui.label(
                            egui::RichText::new("Select or add a configuration")
                                .color(theme.text_dim),
                        );
                        return;
                    };
                    let mut st: EditState = ctx
                        .data_mut(|d| d.get_temp::<EditState>(state_id))
                        .unwrap_or_default();
                    let staged: std::collections::BTreeMap<String, String> = st
                        .env_rows
                        .iter()
                        .filter(|(k, _)| !k.trim().is_empty())
                        .map(|(k, v)| (k.trim().to_string(), v.clone()))
                        .collect();
                    if st.config != sel || staged != store.configs[idx].env {
                        // Another configuration, or its variables changed elsewhere.
                        st = EditState {
                            config: sel.clone(),
                            name: sel.clone(),
                            env_rows: env_rows(&store.configs[idx]),
                        };
                    }
                    let other_names: Vec<String> = store
                        .configs
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != idx)
                        .map(|(_, c)| c.name.clone())
                        .collect();
                    let mut changed = false;
                    egui::ScrollArea::vertical()
                        .id_salt("run_cfg_form")
                        .max_height(440.0)
                        .show(ui, |ui| {
                            let cfg = &mut store.configs[idx];
                            changed |= form(ui, theme, cfg, &mut st, &other_names, input);
                        });
                    if st.config != store.configs[idx].name {
                        // Renamed: keep the selection on it.
                        store.selected = Some(store.configs[idx].name.clone());
                        st.config = store.configs[idx].name.clone();
                    }
                    if let Some(problem) = validate(&store.configs[idx], store, input.root) {
                        ui.label(egui::RichText::new(format!("⚠ {problem}")).color(theme.warning));
                    }
                    ctx.data_mut(|d| d.insert_temp(state_id, st));
                    if changed {
                        events.push(RunCfgEvent::Changed);
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        let cfg = &store.configs[idx];
                        let runnable = validate(cfg, store, input.root).is_none();
                        if ui
                            .add_enabled(
                                runnable,
                                egui::Button::new(
                                    egui::RichText::new("▶ Run").color(theme.success),
                                ),
                            )
                            .clicked()
                        {
                            events.push(RunCfgEvent::Run(cfg.clone()));
                            close = true;
                        }
                        let debuggable =
                            runnable && !matches!(cfg.kind, RunConfigKind::ShellScript { .. });
                        if ui
                            .add_enabled(debuggable, egui::Button::new("🐞 Debug"))
                            .clicked()
                        {
                            events.push(RunCfgEvent::Debug(cfg.clone()));
                            close = true;
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                });
            });
        });
    if close {
        *open = false;
    }
    workflow_form::templates_window(ctx, theme, input, &mut events);
    events
}

fn path_field(ui: &mut egui::Ui, path: &mut PathBuf, hint: &str) -> bool {
    let mut text = path.to_string_lossy().into_owned();
    let changed = ui
        .add(
            egui::TextEdit::singleline(&mut text)
                .desired_width(FIELD_WIDTH)
                .hint_text(hint),
        )
        .changed();
    if changed {
        *path = PathBuf::from(text);
    }
    changed
}

/// The editable form of one configuration; returns true when it changed.
fn form(
    ui: &mut egui::Ui,
    theme: &UiTheme,
    cfg: &mut RunConfig,
    st: &mut EditState,
    other_names: &[String],
    input: &DialogInput,
) -> bool {
    let mut changed = typed_form(ui, cfg);
    let typed = cfg.type_options.is_some();
    let is_test = matches!(
        cfg.kind,
        RunConfigKind::Pytest { .. } | RunConfigKind::Unittest { .. }
    );
    let is_shell = matches!(cfg.kind, RunConfigKind::ShellScript { .. });
    egui::Grid::new("run_cfg_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Name:");
        ui.vertical(|ui| {
            if ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(FIELD_WIDTH)).changed() {
                let name = st.name.trim();
                if !name.is_empty() && !other_names.iter().any(|n| n == name) {
                    cfg.name = name.to_string();
                    changed = true;
                }
            }
            let name = st.name.trim();
            if name.is_empty() {
                ui.label(egui::RichText::new("Enter a name").color(theme.warning).small());
            } else if other_names.iter().any(|n| n == name) {
                ui.label(egui::RichText::new(format!("'{name}' is already used")).color(theme.warning).small());
            }
        });
        ui.end_row();
        if !typed { match &mut cfg.kind {
            RunConfigKind::Python { .. } | RunConfigKind::PythonModule { .. } => {
                let module = matches!(cfg.kind, RunConfigKind::PythonModule { .. });
                let mut choice = module;
                egui::ComboBox::from_id_salt("run_cfg_target_kind").selected_text(if module { "Module name:" } else { "Script path:" }).width(110.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut choice, false, "Script path");
                    ui.selectable_value(&mut choice, true, "Module name");
                }).combo_named(crate::i18n::tr_static("Target type"));
                if choice != module {
                    cfg.kind = match &cfg.kind {
                        RunConfigKind::Python { script } => RunConfigKind::PythonModule { module: script.to_string_lossy().trim_end_matches(".py").replace(['/', '\\'], ".") },
                        RunConfigKind::PythonModule { module } => RunConfigKind::Python { script: PathBuf::from(if module.is_empty() { String::new() } else { format!("{}.py", module.replace('.', "/")) }) },
                        other => other.clone(),
                    };
                    changed = true;
                }
                match &mut cfg.kind {
                    RunConfigKind::Python { script } => changed |= path_field(ui, script, "main.py or an absolute path"),
                    RunConfigKind::PythonModule { module } => changed |= ui.add(egui::TextEdit::singleline(module).desired_width(FIELD_WIDTH).hint_text("package.module (python -m)")).changed(),
                    _ => {}
                }
                ui.end_row();
            }
            RunConfigKind::Pytest { target, keywords } => {
                ui.label("Target:");
                changed |= ui.add(egui::TextEdit::singleline(target).desired_width(FIELD_WIDTH).hint_text("file, directory or node id (tests/test_x.py::TestA::test_b); empty: all")).changed();
                ui.end_row();
                ui.label("Keywords (-k):");
                changed |= ui.add(egui::TextEdit::singleline(keywords).desired_width(FIELD_WIDTH)).changed();
                ui.end_row();
            }
            RunConfigKind::Unittest { target } => {
                ui.label("Target:");
                changed |= ui.add(egui::TextEdit::singleline(target).desired_width(FIELD_WIDTH).hint_text("file, directory, node id or dotted name; empty: all")).changed();
                ui.end_row();
            }
            RunConfigKind::ShellScript { script } => {
                ui.label("Script:");
                changed |= path_field(ui, script, "script.sh");
                ui.end_row();
            }
            // An External Tool's command line (run once from Tools › External Tools).
            RunConfigKind::Program { program, args } => {
                ui.label("Program:");
                ui.label(program.display().to_string());
                ui.end_row();
                ui.label("Arguments:");
                ui.label(args.iter().map(|a| if a.contains(char::is_whitespace) || a.is_empty() { format!("\"{a}\"") } else { a.clone() }).collect::<Vec<_>>().join(" "));
                ui.end_row();
            }
        }
        } // type-specific targets are edited above.
        ui.label(if is_test { "Additional arguments:" } else { "Parameters:" });
        changed |= ui.add(egui::TextEdit::singleline(&mut cfg.parameters).desired_width(FIELD_WIDTH).hint_text("$FilePath$, $FileDir$, $ProjectFileDir$ and $Prompt$ are expanded")).changed();
        ui.end_row();
        if !is_shell {
            let interpreter_label = ui.label("Python interpreter:");
            let project_label = input.project_interpreter.map(|i| format!("Project Default ({})", i.label())).unwrap_or_else(|| "Project Default".to_string());
            let current = match &cfg.interpreter {
                None => project_label.clone(),
                Some(p) => input.interpreters.iter().find(|i| &i.path == p).map(|i| i.label()).unwrap_or_else(|| p.display().to_string()),
            };
            egui::ComboBox::from_id_salt("run_cfg_interpreter").selected_text(current).width(FIELD_WIDTH).show_ui(ui, |ui| {
                if ui.selectable_label(cfg.interpreter.is_none(), &project_label).clicked() {
                    cfg.interpreter = None;
                    changed = true;
                }
                for i in input.interpreters {
                    let sel = cfg.interpreter.as_ref() == Some(&i.path);
                    if ui.selectable_label(sel, i.label()).on_hover_text(i.path.display().to_string()).clicked() {
                        cfg.interpreter = Some(i.path.clone());
                        changed = true;
                    }
                }
            }).combo_labelled(&interpreter_label);
            ui.end_row();
            ui.label("Interpreter options:");
            changed |= ui.add(egui::TextEdit::singleline(&mut cfg.interpreter_options).desired_width(FIELD_WIDTH).hint_text("-X dev -W error")).changed();
            ui.end_row();
        }
        ui.label("Working directory:");
        let mut wd = cfg.working_dir.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        if ui.add(egui::TextEdit::singleline(&mut wd).desired_width(FIELD_WIDTH).hint_text("project root")).changed() {
            cfg.working_dir = if wd.trim().is_empty() { None } else { Some(PathBuf::from(wd)) };
            changed = true;
        }
        ui.end_row();
        ui.label("Environment variables:");
        ui.vertical(|ui| {
            let mut remove = None;
            let mut rows_changed = false;
            for (i, (k, v)) in st.env_rows.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    rows_changed |= ui.add(egui::TextEdit::singleline(k).desired_width(150.0).hint_text("NAME")).changed();
                    ui.label("=");
                    rows_changed |= ui.add(egui::TextEdit::singleline(v).desired_width(FIELD_WIDTH - 200.0).hint_text("value")).changed();
                    if ui.small_button("×").on_hover_text("Remove the variable").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                st.env_rows.remove(i);
                rows_changed = true;
            }
            if ui.small_button("+ Add variable").clicked() {
                st.env_rows.push((String::new(), String::new()));
            }
            if rows_changed {
                cfg.env = st.env_rows.iter().filter(|(k, _)| !k.trim().is_empty()).map(|(k, v)| (k.trim().to_string(), v.clone())).collect();
                changed = true;
            }
        });
        ui.end_row();
        ui.label("Paths to .env files:");
        ui.vertical(|ui| {
            let mut remove = None;
            for (i, f) in cfg.env_files.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let mut text = f.to_string_lossy().into_owned();
                    if ui.add(egui::TextEdit::singleline(&mut text).desired_width(FIELD_WIDTH - 40.0).hint_text(".env")).changed() {
                        *f = PathBuf::from(text);
                        changed = true;
                    }
                    if ui.small_button("×").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                cfg.env_files.remove(i);
                changed = true;
            }
            if ui.small_button("+ Add .env file").clicked() {
                let default = if input.root.join(".env").is_file() && !cfg.env_files.iter().any(|f| f == Path::new(".env")) { PathBuf::from(".env") } else { PathBuf::new() };
                cfg.env_files.push(default);
                changed = true;
            }
        });
        ui.end_row();
        ui.label("Storage:");
        changed |= ui.checkbox(&mut cfg.shared, "Store as project file (share through version control)").changed();
        ui.end_row();
        ui.label("PYTHONPATH:");
        ui.vertical(|ui| {
            changed |= ui.checkbox(&mut cfg.add_content_roots, "Add content roots to PYTHONPATH").changed();
            changed |= ui.checkbox(&mut cfg.add_source_roots, "Add source roots to PYTHONPATH").changed();
        });
        ui.end_row();
        if !is_test {
            ui.label("Execution:");
            ui.vertical(|ui| {
                changed |= ui.checkbox(&mut cfg.show_console_on_stdout, "Show console on stdout").changed();
                changed |= ui.checkbox(&mut cfg.show_console_on_stderr, "Show console on stderr").changed();
                ui.horizontal(|ui| {
                    let encoding_label = ui.label("Output encoding:");
                    egui::ComboBox::from_id_salt("console_encoding").selected_text(&cfg.console_encoding).show_ui(ui,|ui|{ for encoding in ["utf-8","ascii","latin-1","windows-1252","utf-16le","utf-16be"] {changed |= ui.selectable_value(&mut cfg.console_encoding,encoding.into(),crate::i18n::tr_static(encoding)).changed();} }).combo_labelled(&encoding_label);
                    let mut override_buffer=cfg.console_buffer_kib.is_some();
                    if ui.checkbox(&mut override_buffer,"Override buffer (KiB)").changed(){cfg.console_buffer_kib=override_buffer.then_some(1024);changed=true;}
                    if let Some(buffer)=&mut cfg.console_buffer_kib {changed |= ui.add(egui::DragValue::new(buffer).range(1..=1_048_576)).changed();}
                });
                changed |= ui.checkbox(&mut cfg.emulate_terminal, "Emulate terminal in output console").on_hover_text("Run in a terminal tab (colors, cursor control, interactive input)").changed();
                if matches!(cfg.kind, RunConfigKind::Python { .. }) {
                    changed |= ui.checkbox(&mut cfg.run_with_console, "Run with Python Console").on_hover_text("Run the script in the Python Console and keep its variables").changed();
                }
                ui.horizontal(|ui| {
                    let mut redirect = cfg.redirect_input.is_some();
                    if ui.checkbox(&mut redirect, "Redirect input from:").changed() {
                        cfg.redirect_input = redirect.then(PathBuf::new);
                        changed = true;
                    }
                    if let Some(path) = &mut cfg.redirect_input {
                        let mut text = path.to_string_lossy().into_owned();
                        if ui.add(egui::TextEdit::singleline(&mut text).desired_width(FIELD_WIDTH - 150.0).hint_text("input.txt")).changed() {
                            *path = PathBuf::from(text);
                            changed = true;
                        }
                    }
                });
            });
            ui.end_row();
        }
        ui.label("");
        let mut keep = !cfg.temporary;
        if ui.checkbox(&mut keep, "Save configuration (keep after temporary limit)").changed() {
            cfg.temporary = !keep;
            changed = true;
        }
        ui.end_row();
    });
    changed |= workflow_form::fields(ui, cfg);
    changed
}

/// The type-specific editor is intentionally separate from shared launch options.
fn typed_form(ui: &mut egui::Ui, cfg: &mut RunConfig) -> bool {
    let Some(spec) = &mut cfg.type_options else {
        return false;
    };
    let mut changed = false;
    ui.heading(spec.label());
    egui::Grid::new("configuration_type_options")
        .num_columns(2)
        .show(ui, |ui| {
            let mut field = |ui: &mut egui::Ui, label: &str, text: &mut String| {
                ui.label(label);
                changed |= ui.text_edit_singleline(text).changed();
                ui.end_row();
            };
            field(ui, "Target / script / module", &mut spec.target);
            field(ui, "Member / task / builder", &mut spec.member);
            if spec.kind.is_test() {
                field(ui, "Keywords / name filter", &mut spec.keywords);
            }
            if matches!(
                spec.kind,
                ConfigurationType::Docker | ConfigurationType::DockerImage
            ) {
                field(ui, "Image name / tag", &mut spec.image);
            }
            if spec.kind == ConfigurationType::DockerCompose {
                field(ui, "Compose service (blank: all)", &mut spec.service);
            }
            if spec.kind == ConfigurationType::Conda {
                field(ui, "Conda environment name", &mut spec.environment);
            }
            if spec.kind == ConfigurationType::DockerCompose {
                ui.label("Session ownership");
                changed |= ui.checkbox(&mut spec.compose_owned_project, "Isolated owned project (Stop removes resources created by this run)").changed();
                ui.end_row();
            }
            if matches!(
                spec.kind,
                ConfigurationType::AppEngine
                    | ConfigurationType::Docker
                    | ConfigurationType::DockerCompose
                    | ConfigurationType::DockerImage
                    | ConfigurationType::Npm
                    | ConfigurationType::Node
                    | ConfigurationType::Conda
            ) {
                let mut path = spec.executable.display().to_string();
                ui.label("Executable override (blank: PATH)");
                if ui.text_edit_singleline(&mut path).changed() {
                    spec.executable = path.into();
                    changed = true;
                }
                ui.end_row();
            }
            let mut args = rustpy_project::configuration_types::quote_arguments(&spec.arguments);
            ui.label("Type arguments");
            if ui.text_edit_singleline(&mut args).changed() {
                spec.arguments = RunConfig::split_args(&args);
                changed = true;
            }
            ui.end_row();
            if spec.kind.is_test() {
                let target_kind_label = ui.label("Target kind");
                egui::ComboBox::from_id_salt("test_target_kind")
                    .selected_text(format!("{:?}", spec.target_kind))
                    .show_ui(ui, |ui| {
                        for kind in [
                            TargetKind::Custom,
                            TargetKind::Script,
                            TargetKind::Module,
                            TargetKind::Class,
                            TargetKind::Method,
                            TargetKind::Function,
                        ] {
                            changed |= ui
                                .selectable_value(&mut spec.target_kind, kind, format!("{kind:?}"))
                                .changed();
                        }
                    }).combo_labelled(&target_kind_label);
                ui.end_row();
            }
            if spec.kind == ConfigurationType::Compound {
                let mut children = spec.configurations.join("\n");
                ui.label("Configuration names (one per line)");
                if ui.text_edit_multiline(&mut children).changed() {
                    spec.configurations = children
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(|line| line.trim().to_string())
                        .collect();
                    changed = true;
                }
                ui.end_row();
            }
            if matches!(
                spec.kind,
                ConfigurationType::PythonDebugServer
                    | ConfigurationType::JavaScriptDebug
                    | ConfigurationType::AttachProcess
            ) {
                if spec.kind==ConfigurationType::PythonDebugServer {
                ui.label("Host");
                changed |= ui.text_edit_singleline(&mut spec.host).changed();
                ui.end_row();
                ui.label("Port");
                changed |= ui
                    .add(egui::DragValue::new(&mut spec.port).range(1..=65535))
                    .changed();
                ui.end_row();
                if spec.kind==ConfigurationType::PythonDebugServer {
                ui.label("Legacy pydevd protocol");changed |= ui.checkbox(&mut spec.pydevd_legacy,"Accept pydevd_pycharm.settrace()").changed();ui.end_row();
                ui.label("pydevd DAP protocol");changed |= ui.checkbox(&mut spec.pydevd,"Accept pydevd.settrace(protocol='dap')").changed();ui.end_row();
                if ui.button("Copy remote setup snippet").clicked() {
                    let host=serde_json::to_string(&spec.host).unwrap_or_default();
                    let snippet=if spec.pydevd_legacy{format!("import pydevd_pycharm\npydevd_pycharm.settrace({host}, port={}, stdout_to_server=True, stderr_to_server=True, suspend=True)\n",spec.port)}else if spec.pydevd {format!("import debugpy._vendored.force_pydevd\nimport pydevd\npydevd.settrace({host}, port={}, protocol='dap', suspend=False)\n",spec.port)}else if spec.listen{format!("import debugpy\ndebugpy.connect(({host}, {}))\ndebugpy.wait_for_client()\n",spec.port)}else{format!("import debugpy\ndebugpy.listen(({host}, {}))\ndebugpy.wait_for_client()\n",spec.port)};
                    ui.ctx().copy_text(snippet);
                }
                ui.end_row();
            }
            ui.label("Listen for remote connection");
                changed |= ui.checkbox(&mut spec.listen, "").changed();
                ui.end_row();
                }
                ui.label("Path mappings (local = remote)");
                let mut mappings = spec
                    .path_mappings
                    .iter()
                    .map(|(local, remote)| format!("{local}={remote}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                if ui.text_edit_multiline(&mut mappings).changed() {
                    spec.path_mappings = mappings
                        .lines()
                        .filter_map(|line| line.split_once('='))
                        .map(|(a, b)| (a.trim().into(), b.trim().into()))
                        .collect();
                    changed = true;
                }
                ui.end_row();
            }
            if spec.kind == ConfigurationType::AttachProcess {
                ui.label("Process ID");
                changed |= ui
                    .add(egui::DragValue::new(&mut spec.process_id).range(1..=u32::MAX))
                    .changed();
                ui.end_row();
            }
            if spec.kind == ConfigurationType::JavaScriptDebug {
                let mut executable = spec.executable.display().to_string();
                ui.label("DAP adapter executable");
                if ui.text_edit_singleline(&mut executable).changed() {
                    spec.executable = executable.into();
                    changed = true;
                }
                ui.end_row();
                let mut arguments =
                    rustpy_project::configuration_types::quote_arguments(&spec.adapter_arguments);
                ui.label("Adapter arguments ($Port$ replaced)");
                if ui.text_edit_singleline(&mut arguments).changed() {
                    spec.adapter_arguments = RunConfig::split_args(&arguments);
                    changed = true;
                }
                ui.end_row();
                let dap_request_label = ui.label("DAP request");
                egui::ComboBox::from_id_salt("dap_request")
                    .selected_text(&spec.debug_request)
                    .show_ui(ui, |ui| {
                        changed |= ui
                            .selectable_value(&mut spec.debug_request, "launch".into(), "launch")
                            .changed();
                        changed |= ui
                            .selectable_value(&mut spec.debug_request, "attach".into(), "attach")
                            .changed();
                    }).combo_labelled(&dap_request_label);
                ui.end_row();
                let mut json =
                    serde_json::to_string_pretty(&spec.debug_options).unwrap_or_default();
                ui.label("DAP launch / attach options");
                if ui.text_edit_multiline(&mut json).changed() {
                    if let Ok(value) = serde_json::from_str(&json) {
                        spec.debug_options = value;
                        changed = true;
                    }
                }
                ui.end_row();
            }
            if spec.kind == ConfigurationType::HttpRequest {
                ui.label("Request line (zero based)");
                changed |= ui
                    .add(egui::DragValue::new(&mut spec.request_line))
                    .changed();
                ui.end_row();
            }
        });
    let options = spec.clone();
    if changed {
        if let Ok(lowered) = options.materialize(cfg) {
            cfg.kind = lowered.kind;
            cfg.run_with_console = lowered.run_with_console;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_configurations_get_unique_names_and_kinds() {
        let root = Path::new("/proj");
        let mut store = RunConfigStore::default();
        for kind in NewKind::ALL {
            let cfg = new_config(kind, &store, root);
            assert!(!cfg.temporary);
            assert_eq!(cfg.working_dir.as_deref(), Some(root));
            store.configs.push(cfg);
        }
        let labels: Vec<(&str, &str)> = store
            .configs
            .iter()
            .map(|c| (c.name.as_str(), c.kind_label()))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("Unnamed", "Python"),
                ("pytest", "pytest"),
                ("Unittests", "unittest"),
                ("Doctests", "Python"),
                ("Shell Script", "Shell")
            ]
        );
        assert_eq!(
            store.configs[3].kind,
            RunConfigKind::PythonModule {
                module: "doctest".into()
            }
        );
        assert_eq!(new_config(NewKind::Pytest, &store, root).name, "pytest (1)");
    }

    #[test]
    fn copy_inserts_after_the_original_and_selects_it() {
        let mut store = RunConfigStore::default();
        let mut cfg = RunConfig::for_pytest("tests", Path::new("/p"));
        cfg.env.insert("A".into(), "1".into());
        store.configs.push(cfg);
        store.configs.push(RunConfig {
            name: "other".into(),
            ..Default::default()
        });
        assert_eq!(
            copy_config(&mut store, "pytest in tests").as_deref(),
            Some("pytest in tests (1)")
        );
        assert_eq!(store.configs[1].name, "pytest in tests (1)");
        assert_eq!(store.configs[1].env.get("A").map(String::as_str), Some("1"));
        assert!(!store.configs[1].temporary, "copies are permanent");
        assert_eq!(store.selected.as_deref(), Some("pytest in tests (1)"));
        assert_eq!(copy_config(&mut store, "missing"), None);
    }

    #[test]
    fn validation() {
        let mut store = RunConfigStore::default();
        store.configs.push(RunConfig {
            name: "a".into(),
            kind: RunConfigKind::Python {
                script: PathBuf::new(),
            },
            ..Default::default()
        });
        let dir = crate::run_context::tests::ScratchDir::new("validate");
        let root = dir.0.as_path();
        std::fs::write(root.join("input.txt"), "1\n").unwrap();
        assert_eq!(
            validate(&store.configs[0], &store, root).as_deref(),
            Some("Specify the script path")
        );
        store.configs[0].kind = RunConfigKind::Python {
            script: "main.py".into(),
        };
        assert_eq!(validate(&store.configs[0], &store, root), None);
        // "Redirect input from" needs an existing file.
        store.configs[0].redirect_input = Some(PathBuf::new());
        assert_eq!(
            validate(&store.configs[0], &store, root).as_deref(),
            Some("Specify the file to redirect input from")
        );
        store.configs[0].redirect_input = Some(PathBuf::from("missing.txt"));
        assert!(
            validate(&store.configs[0], &store, root).is_some_and(|p| p.contains("missing.txt"))
        );
        store.configs[0].redirect_input = Some(PathBuf::from("input.txt"));
        assert_eq!(validate(&store.configs[0], &store, root), None);
        store.configs.push(store.configs[0].clone());
        assert!(validate(&store.configs[0], &store, root)
            .unwrap()
            .contains("Another configuration"));
        let module = RunConfig {
            name: "m".into(),
            kind: RunConfigKind::PythonModule { module: " ".into() },
            ..Default::default()
        };
        assert_eq!(
            validate(&module, &store, root).as_deref(),
            Some("Specify the module name")
        );
        assert_eq!(
            validate(&RunConfig::for_pytest("", Path::new("/p")), &store, root),
            None
        );
    }
}
