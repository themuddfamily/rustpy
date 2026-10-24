//! Settings form shared by local and SSH tools.
use rustpy_settings::{
    external_tools::{PathMapping, Ssh},
    ExternalTool,
};
pub(crate) fn options(ui: &mut egui::Ui, tool: &mut ExternalTool) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Show in:");
        ui.checkbox(&mut tool.options.main_menu, "Main menu");
        ui.checkbox(&mut tool.options.editor_menu, "Editor menu");
        ui.checkbox(&mut tool.options.project_menu, "Project view menu");
    });
    ui.checkbox(
        &mut tool.options.synchronize,
        "Synchronize local files after execution",
    );
    ui.checkbox(
        &mut tool.options.allow_parallel,
        "Allow concurrent processes",
    );
    ui.checkbox(&mut tool.options.inherit_environment, "Inherit environment");
    ui.label("Output is retained even when Open console is off.");
    ui.collapsing("Environment", |ui| {
        let entries = tool.options.environment.clone();
        for (key, value) in entries {
            let (mut edited_key, mut edited_value) = (key.clone(), value);
            let mut remove = false;
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut edited_key);
                ui.text_edit_singleline(&mut edited_value);
                remove = ui.small_button("Remove").clicked();
            });
            if remove {
                tool.options.environment.remove(&key);
            } else if edited_key != key && tool.options.environment.contains_key(&edited_key) {
                ui.colored_label(egui::Color32::RED, "Environment variable already exists");
            } else {
                tool.options.environment.remove(&key);
                tool.options.environment.insert(edited_key, edited_value);
            }
        }
        if ui.button("Add variable").clicked() {
            let mut key = "VARIABLE".to_owned();
            let mut index = 2;
            while tool.options.environment.contains_key(&key) {
                key = format!("VARIABLE_{index}");
                index += 1;
            }
            tool.options.environment.insert(key, String::new());
        }
    });
    ui.collapsing("Output filters",|ui|{
        ui.label("One regex per line: named path/line captures, or groups 1/2. $FILE_PATH$, $LINE$ and $COLUMN$ are also supported.");
        let mut filters=tool.options.output_filters.join("\n");
        if ui.add(egui::TextEdit::multiline(&mut filters).desired_rows(3).hint_text(r"^$FILE_PATH$:$LINE$:$COLUMN$:")).changed(){tool.options.output_filters=filters.lines().filter(|line|!line.trim().is_empty()).map(str::to_owned).collect();}
        for filter in &tool.options.output_filters{if let Err(error)=crate::external_tool_workspace::compile_filter(filter){ui.colored_label(egui::Color32::RED,error);}}
    });
    let mut remote = tool.options.ssh.is_some();
    if ui.checkbox(&mut remote, "Run on SSH host").changed() {
        tool.options.ssh = remote.then(Ssh::default);
    }
    if let Some(ssh) = &mut tool.options.ssh {
        ui.label("Uses OpenSSH key/agent/config authentication and a POSIX remote shell.");
        egui::Grid::new("external-tool-ssh")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Host / SSH config alias");
                ui.text_edit_singleline(&mut ssh.host);
                ui.end_row();
                ui.label("Port");
                ui.add(egui::DragValue::new(&mut ssh.port).range(1..=65535));
                ui.end_row();
                ui.label("User (empty: SSH config)");
                ui.text_edit_singleline(&mut ssh.user);
                ui.end_row();
                ui.label("Identity file (optional)");
                let mut key = ssh
                    .key_file
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                if ui.text_edit_singleline(&mut key).changed() {
                    ssh.key_file = (!key.trim().is_empty()).then(|| key.trim().into());
                }
                ui.end_row();
                ui.label("Local SSH executable");
                ui.text_edit_singleline(&mut ssh.ssh_program);
                ui.end_row();
                ui.label("Local SSH config file (optional, absolute)");
                let mut config = ssh
                    .ssh_config_file
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                if ui.text_edit_singleline(&mut config).changed() {
                    ssh.ssh_config_file = (!config.is_empty()).then(|| config.into());
                }
                ui.end_row();
                ui.label("Remote project root");
                ui.text_edit_singleline(&mut ssh.remote_project_root);
                ui.end_row();
                ui.label("Remote Python executable (for macros)");
                ui.text_edit_singleline(&mut ssh.remote_interpreter);
                ui.end_row();
            });
        ui.label("File/project macros use remote paths. Output links map back to local source; additional mappings override the project mapping.");
        ui.collapsing("Additional SSH path mappings", |ui| {
            let mut remove = None;
            for (index, mapping) in ssh.mappings.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let mut local = mapping.local.display().to_string();
                    if ui.text_edit_singleline(&mut local).changed() {
                        mapping.local = local.into();
                    }
                    ui.label("→");
                    ui.text_edit_singleline(&mut mapping.remote);
                    if ui.small_button("Remove").clicked() {
                        remove = Some(index);
                    }
                });
            }
            if let Some(index) = remove {
                ssh.mappings.remove(index);
            }
            if ui.button("Add mapping").clicked() {
                ssh.mappings.push(PathMapping::default());
            }
        });
    }
}

pub(crate) fn shortcuts(
    ui: &mut egui::Ui,
    tools: &mut [ExternalTool],
    keymap: &crate::actions::Keymap,
) {
    ui.collapsing("External Tools", |ui| {
        let shortcuts = tools
            .iter()
            .filter_map(|tool| {
                crate::external_tools::tool_chord(&tool.shortcut, keymap)
                    .ok()
                    .flatten()
            })
            .collect::<Vec<_>>();
        if shortcuts
            .iter()
            .any(|chord| shortcuts.iter().filter(|other| *other == chord).count() > 1)
        {
            ui.colored_label(
                egui::Color32::RED,
                "The same shortcut is assigned to multiple tools; ambiguous bindings are inactive",
            );
        }
        for (index, tool) in tools.iter_mut().enumerate() {
            ui.push_id(("tool-keymap", index), |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("{} / {}", tool.group, tool.name));
                    ui.add(
                        egui::TextEdit::singleline(&mut tool.shortcut)
                            .hint_text("Ctrl+Alt+Shift+B")
                            .desired_width(160.),
                    );
                    if ui.small_button("Remove shortcut").clicked() {
                        tool.shortcut.clear();
                    }
                });
                if let Some(error) = crate::external_tools::shortcut_problem(&tool.shortcut, keymap)
                {
                    ui.colored_label(egui::Color32::RED, error);
                }
            });
        }
    });
}
