//! Terminal host: settings, project layout persistence, detached viewport and IDE actions.
use super::*;
use crate::terminal_panel::TerminalEvent;
#[derive(Default)]
pub(super) struct TerminalWorkspace {
    root: Option<PathBuf>,
    last_save: f64,
    builtin_disabled: bool,
}
impl IdeApp {
    pub(super) fn terminal_settings(&mut self, ui: &mut egui::Ui) {
        let p = &mut self.settings.terminal.preferences;
        ui.horizontal(|ui| {
            ui.label("Shell arguments");
            let mut args = crate::run_anything::quote_args(&p.arguments);
            if ui.text_edit_singleline(&mut args).changed() {
                p.arguments = RunConfig::split_args(&args);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Start directory (empty = project root)");
            ui.text_edit_singleline(&mut p.start_directory);
        });
        ui.horizontal(|ui| {
            ui.label("Tab name");
            ui.text_edit_singleline(&mut p.tab_name);
            ui.label("Scrollback lines");
            ui.add(egui::DragValue::new(&mut p.scrollback).range(100..=1_000_000));
        });
        egui::ComboBox::from_label("Terminal engine")
            .selected_text(&p.engine)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut p.engine, "classic".into(), "Classic");
                ui.selectable_value(&mut p.engine, "new".into(), "Command blocks");
            });
        for (label, value) in [
            ("Shell integration", &mut p.shell_integration),
            ("Show separators between blocks", &mut p.block_separators),
            ("Run commands with IDE features", &mut p.smart_commands),
            (
                "Restore tabs and working directories",
                &mut p.restore_sessions,
            ),
            ("Close session on exit", &mut p.close_on_exit),
            ("Copy on selection", &mut p.copy_on_selection),
            ("Paste on middle mouse", &mut p.paste_on_middle),
            ("Mouse reporting (Shift overrides)", &mut p.mouse_reporting),
            ("Use Option as Meta (macOS)", &mut p.option_as_meta),
            ("Audible bell", &mut p.audible_bell),
            ("Visual bell", &mut p.visual_bell),
            ("Cursor blink", &mut p.cursor_blink),
            ("Highlight hovered hyperlinks", &mut p.highlight_links),
            ("Allow OSC 52 clipboard writes", &mut p.osc52_clipboard),
            (
                "Move focus to the editor with Escape",
                &mut p.escape_focuses_editor,
            ),
        ] {
            ui.checkbox(value, crate::i18n::tr_static(label));
        }
        egui::ComboBox::from_label("Cursor shape")
            .selected_text(&p.cursor)
            .show_ui(ui, |ui| {
                for value in ["block", "underline", "bar"] {
                    ui.selectable_value(&mut p.cursor, value.into(), crate::i18n::tr_static(value));
                }
            });
        ui.horizontal(|ui| {
            ui.label("Prompt prefix");
            ui.text_edit_singleline(&mut p.prompt_prefix);
        });
        ui.label("Override IDE shortcuts (one per line; Ctrl+C without a selection interrupts)");
        let mut shortcuts = p.override_shortcuts.join("\n");
        if ui.text_edit_multiline(&mut shortcuts).changed() {
            p.override_shortcuts = shortcuts
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
        ui.collapsing("Environment variables", |ui| {
            let mut remove = None;
            for (old_key, old_value) in p.environment.clone() {
                let mut key = old_key.clone();
                let mut value = old_value;
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut key);
                    ui.text_edit_singleline(&mut value);
                    if ui.small_button("×").clicked() {
                        remove = Some(key.clone());
                    }
                });
                if key != old_key {
                    p.environment.remove(&old_key);
                }
                p.environment.insert(key, value);
            }
            if let Some(key) = remove {
                p.environment.remove(&key);
            }
            if ui.button("Add variable").clicked() {
                let key = format!("VARIABLE_{}", p.environment.len() + 1);
                p.environment.insert(key, String::new());
            }
        });
        ui.separator();
        ui.heading("Shell and remote profiles");
        let mut remove = None;
        for (index, profile) in p.profiles.iter_mut().enumerate() {
            ui.push_id(index,|ui|{ui.collapsing(profile.name.clone(),|ui|{
            ui.horizontal(|ui|{ui.label("Name");ui.text_edit_singleline(&mut profile.name);if ui.button("Use for new tabs").clicked(){p.selected_profile=profile.name.clone();}
if ui.button("Remove").clicked(){remove=Some(index);}});
            egui::ComboBox::from_label("Target").selected_text(&profile.target_kind).show_ui(ui,|ui|{for kind in ["local","ssh","wsl","docker","compose","vagrant"]{ui.selectable_value(&mut profile.target_kind,kind.into(),crate::i18n::tr_static(kind));}});
            for (label,value) in [("Host / distribution / container / service",&mut profile.target),("Shell executable",&mut profile.shell),("Target working directory",&mut profile.directory)]{ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));ui.text_edit_singleline(value);});}
            ui.horizontal(|ui|{ui.label("Arguments");let mut args=crate::run_anything::quote_args(&profile.arguments);if ui.text_edit_singleline(&mut args).changed(){profile.arguments=RunConfig::split_args(&args);}});
            if profile.target_kind=="ssh"{ui.horizontal(|ui|{ui.label("SSH user");ui.text_edit_singleline(&mut profile.ssh_user);ui.label("Port");ui.add(egui::DragValue::new(&mut profile.ssh_port).range(1..=65535));});ui.horizontal(|ui|{ui.label("Private key (empty = SSH agent/config)");ui.text_edit_singleline(&mut profile.ssh_key);});ui.label("Passwords, passphrases and host-key questions are handled by SSH inside the owned terminal.");}
        });});
        }
        if let Some(index) = remove {
            p.profiles.remove(index);
        }
        if ui.button("Add profile").clicked() {
            p.profiles.push(rustpy_settings::terminal::Profile {
                name: format!("Terminal {}", p.profiles.len() + 1),
                ..Default::default()
            });
        }
        self.terminal_panel.preferences = p.clone();
    }
    pub(super) fn poll_terminal_workspace(&mut self, ctx: &egui::Context) {
        if !self.builtin_plugin_enabled("terminal") {
            self.terminal_workspace.builtin_disabled = true;
            self.terminal_panel.set_auto_shell(false);
            for index in (0..self.terminal_panel.tab_count()).rev() {
                self.terminal_panel.close(index);
            }
            return;
        }
        let project_safe_mode = self.project_safe_mode();
        if std::mem::take(&mut self.terminal_workspace.builtin_disabled) {
            self.terminal_panel.set_auto_shell(!project_safe_mode);
        }
        let root = self.project.as_ref().map(|p| p.root.clone());
        if self.terminal_workspace.root != root {
            // Capture the previous owner's layout before disposing its live processes.
            self.persist_terminal_layout();
            for tab in &mut self.terminal_panel.tabs {
                tab.terminal.kill();
            }
            self.terminal_panel.tabs.clear();
            self.terminal_workspace.root = root.clone();
            self.terminal_panel.preferences = self.settings.terminal.preferences.clone();
            self.terminal_panel.shell =
                Some(self.settings.terminal.shell.clone()).filter(|s| !s.is_empty());
            if !project_safe_mode
                && self.settings.terminal.preferences.restore_sessions
                && self.terminal_on_start.is_none()
            {
                if let Some(root) = &root {
                    let saved = self
                        .settings
                        .terminal
                        .preferences
                        .sessions
                        .iter()
                        .find(|s| &s.root == root)
                        .map(|s| s.tabs.clone())
                        .unwrap_or_default();
                    let env = crate::terminal_panel::terminal_env(
                        self.settings.terminal.activate_virtualenv,
                        self.project.as_ref().and_then(|p| p.interpreter.as_ref()),
                    );
                    self.terminal_panel.restore(&saved, &env);
                }
            }
        }
        self.settings.terminal.preferences.selected_profile =
            self.terminal_panel.preferences.selected_profile.clone();
        self.terminal_panel.preferences = self.settings.terminal.preferences.clone();
        let now = ctx.input(|i| i.time);
        if now - self.terminal_workspace.last_save >= 2.0 {
            self.terminal_workspace.last_save = now;
            self.persist_terminal_layout();
        }
        self.consume_terminal_events();
    }
    pub(super) fn persist_terminal_layout(&mut self) {
        if !self.settings.terminal.preferences.restore_sessions {
            return;
        }
        let Some(root) = self.terminal_workspace.root.clone() else {
            return;
        };
        let tabs = self.terminal_panel.saved_tabs();
        let sessions = &mut self.settings.terminal.preferences.sessions;
        if let Some(entry) = sessions.iter_mut().find(|entry| entry.root == root) {
            if entry.tabs == tabs {
                return;
            }
            entry.tabs = tabs;
        } else {
            sessions.push(rustpy_settings::terminal::ProjectSessions { root, tabs });
        }
        if sessions.len() > 32 {
            sessions.remove(0);
        }
        let _ = self.settings.save();
    }
    pub(super) fn terminal_tool_window(
        &mut self,
        ui: &mut egui::Ui,
        theme: &crate::theme::UiTheme,
        mono: egui::FontId,
        cwd: &Path,
        env: &[(String, String)],
    ) {
        if !self.builtin_plugin_enabled("terminal") {
            ui.label("Terminal bundled capability is disabled in Plugins");
            return;
        }

        if self.project_safe_mode() {
            ui.label("Trust the project before opening or using a terminal.");
            return;
        }

        self.terminal_panel.preferences = self.settings.terminal.preferences.clone();
        let directory = if self
            .settings
            .terminal
            .preferences
            .start_directory
            .is_empty()
        {
            cwd.to_path_buf()
        } else {
            PathBuf::from(&self.settings.terminal.preferences.start_directory)
        };
        if self.terminal_panel.detached {
            ui.label("Terminal is in a separate window");
            if ui.button("Reattach terminal").clicked() {
                self.terminal_panel.detached = false;
            }
        } else {
            self.terminal_panel.ui(ui, theme, mono, &directory, env);
        }
        self.consume_terminal_events();
    }
    pub(super) fn detached_terminal_window(&mut self, ctx: &egui::Context) {
        if !self.builtin_plugin_enabled("terminal") {
            return;
        }
        if self.project_safe_mode() {
            return;
        }
        if !self.terminal_panel.detached {
            return;
        }
        let cwd = self.project_root();
        let env = crate::terminal_panel::terminal_env(
            self.settings.terminal.activate_virtualenv,
            self.project.as_ref().and_then(|p| p.interpreter.as_ref()),
        );
        let theme = self.theme.clone();
        let mono = egui::FontId::new(
            self.settings.run_console.font_size.max(10.0),
            crate::theme::mono_family(false, false),
        );
        let mut reattach = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("rustpy-terminal"),
            egui::ViewportBuilder::default()
                .with_title("Terminal — RustPy")
                .with_inner_size([900.0, 500.0]),
            |ui, class| {
                if ui.input(|i| i.viewport().close_requested()) {
                    reattach = true;
                }
                if class == egui::ViewportClass::EmbeddedWindow {
                    // egui already supplies the fallback window's content Ui.
                    self.terminal_panel.ui(ui, &theme, mono.clone(), &cwd, &env);
                } else {
                    egui::CentralPanel::default().show(ui, |ui| {
                        self.terminal_panel.ui(ui, &theme, mono.clone(), &cwd, &env)
                    });
                }
            },
        );
        if reattach {
            self.terminal_panel.detached = false;
        }
        self.consume_terminal_events();
    }
    fn consume_terminal_events(&mut self) {
        for event in std::mem::take(&mut self.terminal_panel.events) {
            match event {
                TerminalEvent::OpenFile(path, line) => {
                    self.goto_file_line(&path, line.saturating_sub(1))
                }
                TerminalEvent::OpenUrl(url) => self.open_url(&url),
                TerminalEvent::PythonConsole => {
                    self.bottom = Some(BottomTool::PythonConsole);
                    self.py_console.request_focus();
                }
                TerminalEvent::SmartCommand(command, cwd) => {
                    self.terminal_smart_command(&command, &cwd)
                }
            }
        }
    }
    fn terminal_smart_command(&mut self, line: &str, cwd: &Path) {
        if !self.builtin_plugin_enabled("terminal") {
            return;
        }
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            return;
        }
        let Some(plan) = crate::run_anything::plan_command(line, cwd) else {
            return;
        };
        let args_string = crate::run_anything::quote_args;
        match plan {
            crate::run_anything::CommandPlan::Pytest { targets, args } => {
                let config = RunConfig {
                    name: line.into(),
                    kind: RunConfigKind::Pytest {
                        target: String::new(),
                        keywords: String::new(),
                    },
                    working_dir: Some(cwd.into()),
                    parameters: args_string(&targets.into_iter().chain(args).collect::<Vec<_>>()),
                    temporary: true,
                    ..Default::default()
                };
                self.launch_config(config, crate::run_context::RunMode::Run);
            }
            crate::run_anything::CommandPlan::Script {
                interpreter_options,
                script,
                args,
            } => {
                let path = PathBuf::from(script);
                let mut config = RunConfig::for_script(&if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                });
                config.name = line.into();
                config.working_dir = Some(cwd.into());
                config.parameters = args_string(&args);
                config.interpreter_options = args_string(&interpreter_options);
                self.launch_config(config, crate::run_context::RunMode::Run);
            }
            crate::run_anything::CommandPlan::Process { program, args } => {
                if program.as_deref() == Some("git") {
                    if let Some(root) = self.vcs_workspace.mappings.root_for(cwd).cloned() {
                        self.start_git_op(crate::git_jobs::GitOp::BranchNetwork {
                            root,
                            label: line.into(),
                            args,
                        });
                        self.bottom = Some(BottomTool::Git);
                    } else {
                        self.set_status("The terminal directory has no mapped Git root");
                    }
                    return;
                }
                let interpreter = self.project.as_ref().and_then(|p| p.interpreter.as_ref());
                let program = program
                    .map(PathBuf::from)
                    .or_else(|| interpreter.map(|i| i.path.clone()))
                    .unwrap_or_else(|| "python".into());
                let env = crate::terminal_panel::terminal_env(
                    self.settings.terminal.activate_virtualenv,
                    interpreter,
                );
                self.spawn_command_session(
                    line,
                    crate::run_anything::CommandSpec {
                        program,
                        args,
                        cwd: cwd.into(),
                        env,
                    },
                );
            }
        }
    }
    pub(super) fn open_editor_terminal(&mut self, send_selection: bool) {
        if !self.builtin_plugin_enabled("terminal") {
            return;
        }
        let path = self
            .active
            .and_then(|i| self.tabs.get(i))
            .and_then(|tab| tab.state.path.clone());
        let directory = path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.project_root());
        self.terminal_panel.preferences = self.settings.terminal.preferences.clone();
        let env = crate::terminal_panel::terminal_env(
            self.settings.terminal.activate_virtualenv,
            self.project.as_ref().and_then(|p| p.interpreter.as_ref()),
        );
        if self.terminal_panel.tabs.is_empty() || !send_selection {
            if let Err(error) = self.terminal_panel.open(&directory, &env) {
                self.set_status(error.to_string());
                return;
            }
        }
        if send_selection {
            if let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) {
                let caret = tab.state.primary();
                if caret.has_selection() {
                    let text = tab.state.doc.slice(caret.range());
                    self.terminal_panel.send_selection(&text);
                }
            }
        }
        self.bottom = Some(BottomTool::Terminal);
        self.terminal_panel.request_focus();
    }
}
