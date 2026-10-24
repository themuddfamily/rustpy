//! Configuration organization, reusable defaults and ordered prerequisites.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_project::run_workflow::{BeforeLaunch, Task, Templates};

pub(super) fn configuration_list(ui: &mut egui::Ui, theme: &UiTheme, store: &mut RunConfigStore) {
    let id = ui.make_persistent_id("configuration-search");
    let mut query = ui
        .ctx()
        .data_mut(|data| data.get_temp::<String>(id))
        .unwrap_or_default();
    ui.add(egui::TextEdit::singleline(&mut query).hint_text("Search configurations"));
    ui.ctx()
        .data_mut(|data| data.insert_temp(id, query.clone()));
    let query = query.to_lowercase();
    let mut groups = std::collections::BTreeMap::<String, Vec<_>>::new();
    for cfg in &store.configs {
        if !query.is_empty()
            && !format!("{} {} {}", cfg.name, cfg.kind_label(), cfg.workflow.folder)
                .to_lowercase()
                .contains(&query)
        {
            continue;
        }
        groups
            .entry(cfg.workflow.folder.clone())
            .or_default()
            .push(cfg);
    }
    egui::ScrollArea::vertical()
        .id_salt("run_cfg_list")
        .max_height(410.0)
        .show(ui, |ui| {
            for (folder, mut items) in groups {
                items.sort_by_key(|cfg| !cfg.workflow.pinned);
                let render = |ui: &mut egui::Ui| {
                    for cfg in items {
                        let text = format!(
                            "{}{}  ({})",
                            if cfg.workflow.pinned { "◆ " } else { "" },
                            cfg.name,
                            cfg.kind_label()
                        );
                        if ui
                            .selectable_label(
                                store.selected.as_deref() == Some(&cfg.name),
                                egui::RichText::new(text).color(if cfg.temporary {
                                    theme.text_dim
                                } else {
                                    theme.text
                                }),
                            )
                            .clicked()
                        {
                            store.selected = Some(cfg.name.clone());
                        }
                    }
                };
                if folder.trim().is_empty() {
                    render(ui);
                } else {
                    egui::CollapsingHeader::new(folder)
                        .default_open(true)
                        .show(ui, render);
                }
            }
            if store.configs.is_empty() {
                ui.label("No configurations. Use + Add.");
            }
        });
}

fn template_id(root: &Path) -> egui::Id {
    egui::Id::new(("run-configuration-templates", root))
}
pub(super) fn template_toolbar(
    ui: &mut egui::Ui,
    store: &RunConfigStore,
    root: &Path,
    events: &mut Vec<RunCfgEvent>,
) {
    if ui.button("Edit configuration templates…").clicked() {
        ui.ctx()
            .data_mut(|data| data.insert_temp(template_id(root), true));
    }
    if let Some(cfg) = store.selected_config() {
        if ui.button("Save as type defaults").clicked() {
            let mut templates = Templates::load(root);
            templates.remember(cfg);
            match templates.save(root) {
                Ok(()) => events.push(RunCfgEvent::Changed),
                Err(error) => {
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(template_id(root).with("error"), error));
                }
            }
        }
    }
}
pub(super) fn templates_window(
    ctx: &egui::Context,
    theme: &UiTheme,
    input: &DialogInput,
    events: &mut Vec<RunCfgEvent>,
) {
    let id = template_id(input.root);
    let mut open = ctx
        .data_mut(|data| data.get_temp::<bool>(id))
        .unwrap_or(false);
    if !open {
        return;
    }
    let mut templates = Templates::load(input.root);
    let mut changed = false;
    egui::Window::new("Run Configuration Templates").id(id).open(&mut open).default_width(740.0).show_modal(ctx,|ui|{
        ui.horizontal(|ui|{ui.label("Temporary configurations limit");changed|=ui.add(egui::DragValue::new(&mut templates.temporary_limit).range(1..=100)).changed();});
        let mut key=ctx.data_mut(|data|data.get_temp::<String>(id.with("kind"))).unwrap_or_else(||ConfigurationType::Python.label().into());
        let mut labels=ConfigurationType::ALL.iter().map(|kind|kind.label().to_string()).collect::<Vec<_>>();
        labels.extend(templates.defaults.keys().cloned());labels.sort();labels.dedup();
        egui::ComboBox::from_id_salt("template-kind").selected_text(&key).show_ui(ui,|ui|{for label in labels{ui.selectable_value(&mut key,label.clone(),label);}}).combo_named(crate::i18n::tr_static("Configuration type"));
        ctx.data_mut(|data|data.insert_temp(id.with("kind"),key.clone()));
        let cfg=templates.defaults.entry(key.clone()).or_insert_with(||RunConfig{name:key.clone(),..rustpy_project::configuration_types::new_config(ConfigurationType::ALL.iter().copied().find(|kind|kind.label()==key).unwrap_or(ConfigurationType::Python),input.root,&[])});
        let form_id=id.with(("template-edit-state",&key));
        let mut st=ctx.data_mut(|data|data.get_temp::<EditState>(form_id)).unwrap_or_else(||EditState {config:cfg.name.clone(),name:cfg.name.clone(),env_rows:env_rows(cfg)});
        egui::ScrollArea::vertical().id_salt("template-fields").max_height(460.0).show(ui,|ui|{changed|=form(ui,theme,cfg,&mut st,&[],input);});
        st.config=cfg.name.clone();ctx.data_mut(|data|data.insert_temp(form_id,st));
        if let Some(error)=ctx.data_mut(|data|data.get_temp::<String>(id.with("error"))){ui.colored_label(theme.warning,error);}
        ui.label("Defaults apply to new configurations; existing configurations retain their settings.");
    });
    if changed {
        match templates.save(input.root) {
            Ok(()) => events.push(RunCfgEvent::Changed),
            Err(error) => {
                ctx.data_mut(|data| data.insert_temp(id.with("error"), error));
            }
        }
    }
    ctx.data_mut(|data| data.insert_temp(id, open));
}

fn task_label(task: &Task) -> &'static str {
    match task {
        Task::Configuration { .. } => "Run configuration",
        Task::ExternalTool { .. } => "External tool",
        Task::FileWatcher { .. } => "File watcher",
        Task::Upload { .. } => "Upload to deployment",
        Task::Npm { .. } => "npm script",
        Task::Grunt { .. } => "Grunt task",
        Task::Command { .. } => "Program",
    }
}
fn new_task(index: usize) -> Task {
    match index {
        0 => Task::Configuration {
            name: String::new(),
        },
        1 => Task::ExternalTool {
            name: String::new(),
        },
        2 => Task::FileWatcher {
            name: String::new(),
        },
        3 => Task::Upload {
            server: String::new(),
            path: PathBuf::new(),
        },
        4 => Task::Npm {
            directory: PathBuf::new(),
            script: "build".into(),
        },
        5 => Task::Grunt {
            directory: PathBuf::new(),
            task: String::new(),
        },
        _ => Task::Command {
            program: PathBuf::new(),
            arguments: String::new(),
            directory: PathBuf::new(),
        },
    }
}
pub(super) fn fields(ui: &mut egui::Ui, cfg: &mut RunConfig) -> bool {
    let mut changed = false;
    ui.separator();
    ui.horizontal(|ui| {
        ui.label("Configuration folder");
        changed |= ui.text_edit_singleline(&mut cfg.workflow.folder).changed();
        changed |= ui
            .checkbox(&mut cfg.workflow.pinned, "Pin in Run widget")
            .changed();
    });
    ui.horizontal_wrapped(|ui| {
        changed |= ui
            .checkbox(&mut cfg.workflow.allow_parallel, "Allow parallel run")
            .changed();
        changed |= ui
            .checkbox(
                &mut cfg.workflow.show_settings,
                "Show this page before launch",
            )
            .changed();
        changed |= ui
            .checkbox(&mut cfg.workflow.activate_window, "Activate tool window")
            .changed();
        changed |= ui
            .checkbox(&mut cfg.workflow.focus_window, "Focus tool window")
            .changed();
        changed |= ui
            .checkbox(
                &mut cfg.workflow.inherit_environment,
                "Inherit system environment",
            )
            .changed();
    });
    ui.collapsing("Before launch (executed in order)", |ui| {
        let mut remove = None;
        let mut movement = None;
        let length = cfg.workflow.before_launch.len();
        for (index, entry) in cfg.workflow.before_launch.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut entry.enabled, "").changed();
                    egui::ComboBox::from_id_salt("task-kind")
                        .selected_text(task_label(&entry.task))
                        .show_ui(ui, |ui| {
                            for kind in 0..7 {
                                let task = new_task(kind);
                                if ui
                                    .selectable_label(
                                        task_label(&entry.task) == task_label(&task),
                                        task_label(&task),
                                    )
                                    .clicked()
                                {
                                    entry.task = task;
                                    changed = true;
                                }
                            }
                        })
                        .combo_named(crate::i18n::tr_static("Before launch task"));
                    if ui.add_enabled(index > 0, egui::Button::new("↑")).clicked() {
                        movement = Some((index, index - 1));
                    }
                    if ui
                        .add_enabled(index + 1 < length, egui::Button::new("↓"))
                        .clicked()
                    {
                        movement = Some((index, index + 1));
                    }
                    if ui.small_button("×").clicked() {
                        remove = Some(index);
                    }
                });
                match &mut entry.task {
                    Task::Configuration { name }
                    | Task::ExternalTool { name }
                    | Task::FileWatcher { name } => {
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(name).hint_text("Exact configured name"),
                            )
                            .changed();
                    }
                    Task::Upload { server, path } => {
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(server)
                                    .hint_text("Deployment server (empty: default)"),
                            )
                            .changed();
                        changed |= path_field(ui, path, "Local file or directory (empty: project)");
                    }
                    Task::Npm { directory, script } => {
                        changed |= path_field(ui, directory, "Working directory (empty: project)");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(script).hint_text("package.json script"),
                            )
                            .changed();
                    }
                    Task::Grunt { directory, task } => {
                        changed |= path_field(ui, directory, "Working directory (empty: project)");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(task)
                                    .hint_text("Grunt task (empty: default)"),
                            )
                            .changed();
                    }
                    Task::Command {
                        program,
                        arguments,
                        directory,
                    } => {
                        changed |= path_field(ui, program, "Executable");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(arguments).hint_text("Quoted arguments"),
                            )
                            .changed();
                        changed |= path_field(ui, directory, "Working directory (empty: project)");
                    }
                }
            });
        }
        if let Some(index) = remove {
            cfg.workflow.before_launch.remove(index);
            changed = true;
        } else if let Some((a, b)) = movement {
            cfg.workflow.before_launch.swap(a, b);
            changed = true;
        }
        if ui
            .add_enabled(length < 128, egui::Button::new("+ Add before-launch task"))
            .clicked()
        {
            cfg.workflow.before_launch.push(BeforeLaunch::default());
            changed = true;
        }
    });
    changed
}
