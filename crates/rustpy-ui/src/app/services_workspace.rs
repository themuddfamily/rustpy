//! Services owns selection and presentation; each existing tool retains its
//! process, connection, cancellation and console ownership.
use super::*;
use crate::services_panel::{self, AddService, ResourceRow, ServiceId};

impl IdeApp {
    pub(super) fn open_process_service(&mut self, id: u64) {
        if self
            .run_panel
            .sessions
            .iter()
            .any(|session| session.id == id)
        {
            self.services_panel.selected = Some(ServiceId::Process(id));
            self.bottom = Some(BottomTool::Services);
        }
    }
    fn service_resources(&mut self) -> Vec<ResourceRow> {
        let mut resources = Vec::new();
        if let Some((id, name, stamp)) = self.docker_services_pending() {
            resources.push(ResourceRow {
                id: ServiceId::DockerSnapshot(id, stamp),
                name,
                group: "Docker".into(),
                status: self
                    .services_panel
                    .docker
                    .drain_status()
                    .unwrap_or_default(),
                running: true,
                restartable: false,
            });
        }
        for (id, name) in self.window_operation_jobs() {
            resources.push(ResourceRow {
                id: ServiceId::WindowOperation(id),
                name,
                group: "IDE background operations".into(),
                status: "running or stopping".into(),
                running: true,
                restartable: false,
            });
        }
        for (id, name) in self.plugin_jobs() {
            resources.push(ResourceRow {
                id: ServiceId::PluginJob(id),
                name,
                group: "Plugins and IDE scripts".into(),
                status: "running or stopping".into(),
                running: true,
                restartable: false,
            });
        }
        for (id, name, stamp) in self.remote_development_pending() {
            resources.push(ResourceRow {
                id: ServiceId::RemoteDevelopment(id, stamp),
                name,
                group: "Remote Development".into(),
                status: "connected or stopping".into(),
                running: true,
                restartable: false,
            });
        }
        let ai = self.ai_worker_ids();
        if !ai.is_empty() {
            resources.push(ResourceRow {
                id: ServiceId::AiWorkers(ai),
                name: "AI requests".into(),
                group: "AI Assistant".into(),
                status: "running or retiring".into(),
                running: true,
                restartable: false,
            });
        }
        if self.web_languages.is_running() {
            resources.push(ResourceRow {
                id: ServiceId::WebLanguages(self.web_languages.stamps()),
                name: "Web language servers".into(),
                group: "Web development".into(),
                status: "running".into(),
                running: true,
                restartable: false,
            });
        }
        if self.collaboration_running() {
            resources.push(ResourceRow {
                id: ServiceId::Collaboration(self.collaboration_stamp()),
                name: "Collaboration".into(),
                group: "Collaboration".into(),
                status: "connected or stopping".into(),
                running: true,
                restartable: false,
            });
        }
        for (id, name, stamp) in self.settings_sync_pending() {
            resources.push(ResourceRow {
                id: ServiceId::SettingsSync(id, stamp),
                name,
                group: "Settings".into(),
                status: "running".into(),
                running: true,
                restartable: false,
            });
        }
        self.db_panel.set_database_execution_allowed(
            self.builtin_plugin_enabled("database") && !self.project_safe_mode(),
        );
        for (id, name, busy, stopping) in self.db_panel.database_owners() {
            resources.push(ResourceRow {
                id: ServiceId::DatabaseSession(id),
                name,
                group: "Database sessions".into(),
                status: if stopping {
                    "stopping"
                } else if busy {
                    "running"
                } else {
                    "connected"
                }
                .into(),
                running: true,
                restartable: false,
            });
        }
        if let Some(project) = &self.project {
            self.db_panel.load(&project.root);
        }
        for (id, source, connected) in self.db_panel.service_rows() {
            resources.push(ResourceRow {
                id: ServiceId::Database(id),
                name: source.name,
                group: "Databases".into(),
                status: if connected {
                    "connected"
                } else {
                    "disconnected"
                }
                .into(),
                running: connected,
                restartable: true,
            });
        }
        let mut kernels = std::collections::HashSet::new();
        for tab in &self.tabs {
            if let Some(notebook) = &tab.notebook {
                if let Some((id, state)) = notebook.service_snapshot() {
                    if kernels.insert(id) {
                        resources.push(ResourceRow {
                            id: ServiceId::Notebook(id),
                            name: tab
                                .state
                                .path
                                .as_ref()
                                .and_then(|path| path.file_name())
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "Notebook".into()),
                            group: "Jupyter kernels".into(),
                            status: format!("{state:?}"),
                            running: state != rustpy_notebook::KernelState::Dead,
                            restartable: true,
                        });
                    }
                }
            }
        }
        for (id, name, running) in self.py_console.service_rows() {
            resources.push(ResourceRow {
                id: ServiceId::Console(id),
                name,
                group: "Python consoles".into(),
                status: if running { "running" } else { "stopped" }.into(),
                running,
                restartable: true,
            });
        }
        for debug in self
            .debug
            .iter()
            .chain(self.debug_sessions.iter().map(|(session, _)| session))
        {
            resources.push(ResourceRow {
                id: ServiceId::Debug(debug.id),
                name: debug.name.clone(),
                group: "Debug sessions".into(),
                status: format!("{:?}", debug.state),
                running: debug.is_active(),
                restartable: true,
            });
        }
        if let Some(id) = self.test_panel.service_id() {
            resources.push(ResourceRow {
                id: ServiceId::Tests(id),
                name: "Test run".into(),
                group: "Python tests".into(),
                status: if self.test_panel.is_running() {
                    "running"
                } else {
                    "finished"
                }
                .into(),
                running: self.test_panel.is_running(),
                restartable: true,
            });
        }
        resources.extend(self.measured_service_rows());
        resources.extend(
            self.deploy
                .pending_transfers()
                .into_iter()
                .map(|(id, label, stamp)| ResourceRow {
                    id: ServiceId::Deployment(id, stamp),
                    name: label,
                    group: "Deployment".into(),
                    status: if id == 0 { "queued" } else { "running" }.into(),
                    running: true,
                    restartable: false,
                }),
        );
        if self.web_preview_pending() {
            resources.push(ResourceRow {
                id: ServiceId::WebPreview(self.web_preview_stamps()),
                name: "Web preview".into(),
                group: "Web previews".into(),
                status: self.web_preview_labels().join("; "),
                running: true,
                restartable: false,
            });
        }
        for execution in &self.http_panel.executions {
            resources.push(ResourceRow {
                id: ServiceId::Http(execution.id),
                name: execution.configuration.clone(),
                group: "HTTP requests".into(),
                status: execution
                    .job
                    .as_ref()
                    .map(|job| job.control.phase().to_string())
                    .unwrap_or_else(|| {
                        execution
                            .response
                            .as_ref()
                            .map(|response| {
                                if let Some(error) = &response.error {
                                    error.clone()
                                } else {
                                    format!("HTTP {}", response.status)
                                }
                            })
                            .unwrap_or_else(|| "finished".into())
                    }),
                running: execution.job.is_some(),
                restartable: false,
            });
        }
        for terminal in &mut self.terminal_panel.tabs {
            let running = terminal.terminal.is_alive();
            resources.push(ResourceRow {
                id: ServiceId::Terminal(terminal.id),
                name: terminal.title.clone(),
                group: "Terminals".into(),
                status: if running { "running" } else { "exited" }.into(),
                running,
                restartable: false,
            });
        }
        resources
    }
    fn service_processes(&self) -> Vec<ProcessRow> {
        self.run_panel
            .sessions
            .iter()
            .map(|session| ProcessRow {
                id: session.id,
                name: session.name.clone(),
                status: session.status.clone(),
                owns_process: session.owns_process(),
                elapsed: session.elapsed(),
                group: session
                    .config
                    .as_ref()
                    .map(services_panel::configuration_group)
                    .unwrap_or_else(|| "Processes".into()),
            })
            .collect()
    }
    pub(super) fn services_workspace_ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme) {
        let configs = self
            .project
            .as_ref()
            .map(|project| project.run_configs.configs.clone())
            .unwrap_or_default();
        let processes = self.service_processes();
        let resources = self.service_resources();
        let input = ServicesInput {
            configs: &configs,
            processes,
            resources,
        };
        self.poll_docker_services(ui.ctx());
        let events = egui::Panel::left("service_tree")
            .resizable(true)
            .default_size(440.0)
            .min_size(250.0)
            .show(ui, |ui| self.services_panel.ui(ui, theme, input))
            .inner;
        for event in events {
            self.handle_service_event(event);
        }
        ui.push_id("service_console", |ui| self.service_console_ui(ui, theme));
    }
    fn handle_service_event(&mut self, event: ServicesEvent) {
        match event {
            ServicesEvent::Select(id) => self.services_panel.selected = Some(id),
            ServicesEvent::ShowService(id) => self.show_service(id),
            ServicesEvent::StopService(id) => self.stop_service(id),
            ServicesEvent::RestartService(id) => self.restart_service(id),
            ServicesEvent::RerunSession(id) => self.rerun_session(id),
            ServicesEvent::Add(kind) => self.add_service(kind),
            ServicesEvent::Run(config) => self.run_config(config),
            ServicesEvent::Debug(config) => self.debug_config(config),
            ServicesEvent::Edit(config) => {
                if let Some(project) = &mut self.project {
                    project.run_configs.selected = Some(config.name);
                }
                self.dialog = Dialog::RunConfigurations;
            }
            ServicesEvent::Delete(name) => {
                if let Some(project) = &mut self.project {
                    project
                        .run_configs
                        .configs
                        .retain(|config| config.name != name);
                    if project.run_configs.selected.as_deref() == Some(name.as_str()) {
                        project.run_configs.selected = None;
                    }
                    if let Err(error) = project.run_configs.save(&project.root) {
                        self.set_status(error.to_string());
                    }
                }
            }
            ServicesEvent::StopSession(id) => self.stop_service(ServiceId::Process(id)),
            ServicesEvent::ShowSession(id) => self.show_service(ServiceId::Process(id)),
            ServicesEvent::StopDebug => {
                if let Some(debug) = &mut self.debug {
                    debug.stop();
                }
            }
            ServicesEvent::ShowDebug => self.bottom = Some(BottomTool::Debug),
            ServicesEvent::StopTests => self.test_panel.stop(),
            ServicesEvent::ShowTests => self.bottom = Some(BottomTool::Tests),
            ServicesEvent::ShowTerminal => self.bottom = Some(BottomTool::Terminal),
            ServicesEvent::Docker(args) => self.service_docker_command(args),
            ServicesEvent::DockerTerminal(title, args) => {
                if self.project_safe_mode() || !self.builtin_plugin_enabled("docker") {
                    self.set_status("Trust the project before starting Docker commands");
                    return;
                }
                let program = if self.settings.docker_path.is_empty() {
                    "docker"
                } else {
                    &self.settings.docker_path
                };
                let root = self
                    .project
                    .as_ref()
                    .map(|project| project.root.clone())
                    .unwrap_or_default();
                if let Err(error) =
                    self.terminal_panel
                        .open_command(&title, program, &args, &root, &[])
                {
                    self.set_status(error.to_string());
                } else if let Some(tab) = self.terminal_panel.tabs.get(self.terminal_panel.active) {
                    self.services_panel.selected = Some(ServiceId::Terminal(tab.id));
                }
                self.services_panel.docker.force_refresh = true;
            }
        }
    }
    fn service_docker_command(&mut self, args: Vec<String>) {
        if self.project_safe_mode() || !self.builtin_plugin_enabled("docker") {
            self.set_status("Trust the project before starting Docker commands");
            return;
        }
        let program = if self.settings.docker_path.is_empty() {
            PathBuf::from("docker")
        } else {
            PathBuf::from(&self.settings.docker_path)
        };
        let config = RunConfig {
            name: format!("Docker {}", args.join(" ")),
            kind: RunConfigKind::Program { program, args },
            temporary: true,
            ..Default::default()
        };
        self.start_program_session(config, false);
        if let Some(id) = self.run_panel.active {
            self.services_panel.selected = Some(ServiceId::Process(id));
        }
        self.services_panel.docker.force_refresh = true;
    }
    fn add_service(&mut self, kind: AddService) {
        match kind {
            AddService::Configuration => self.dialog = Dialog::RunConfigurations,
            AddService::Database => {
                self.bottom = Some(BottomTool::Database);
                self.db_panel.show_console = true;
            }
            AddService::Docker => self.services_panel.docker.force_refresh = true,
            AddService::PythonConsole => {
                if self.project_safe_mode() {
                    self.set_status("Trust the project before starting a console");
                    return;
                }
                self.py_console.add("Service");
                if self.ensure_py_console() {
                    if let Some(id) = self.py_console.service_id() {
                        self.services_panel.selected = Some(ServiceId::Console(id));
                    }
                }
            }
            AddService::Jupyter | AddService::Django => {
                let Some(project) = &mut self.project else {
                    self.set_status("Open a project before adding a server");
                    return;
                };
                let (name, kind, parameters) = if kind == AddService::Jupyter {
                    (
                        "Jupyter server",
                        RunConfigKind::PythonModule {
                            module: "jupyter".into(),
                        },
                        "lab --no-browser",
                    )
                } else {
                    (
                        "Django server",
                        RunConfigKind::Python {
                            script: project.root.join("manage.py"),
                        },
                        "runserver",
                    )
                };
                let name = project.run_configs.add_new(RunConfig {
                    name: name.into(),
                    kind,
                    parameters: parameters.into(),
                    working_dir: Some(project.root.clone()),
                    ..Default::default()
                });
                let save = project.run_configs.save(&project.root);
                self.services_panel.selected = Some(ServiceId::Configuration(name));
                if let Err(error) = save {
                    self.set_status(error.to_string());
                }
                self.dialog = Dialog::RunConfigurations;
            }
        }
    }
    fn stop_service(&mut self, id: ServiceId) {
        match id {
            ServiceId::Deployment(id, stamp) => self.stop_deployment(id, &stamp),
            ServiceId::WebPreview(stamps) => self.stop_web_preview(&stamps),
            ServiceId::Http(id) => {
                self.stop_http_request(id);
            }
            ServiceId::Measured(id) => self.stop_measurement(id),
            ServiceId::Process(id) => {
                if let Some(session) = self
                    .run_panel
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == id)
                {
                    session.stop();
                }
            }
            ServiceId::Debug(id) => {
                if let Some(debug) = self.debug_session_mut(id) {
                    debug.stop();
                }
            }
            ServiceId::Tests(id) => {
                if self.test_panel.service_id() == Some(id) {
                    self.test_panel.stop();
                }
            }
            ServiceId::Database(id) => self.db_panel.disconnect_service(&id),
            ServiceId::SettingsSync(id, stamp) => {
                if self
                    .settings_sync_pending()
                    .iter()
                    .any(|owner| owner.0 == id && owner.2 == stamp)
                {
                    self.stop_settings_sync(id);
                }
            }
            ServiceId::WebLanguages(stamps) => {
                if stamps == self.web_languages.stamps() {
                    self.stop_web_languages();
                }
            }
            ServiceId::Collaboration(stamp) => {
                if stamp == self.collaboration_stamp() {
                    self.stop_collaboration();
                }
            }
            ServiceId::AiWorkers(ids) => rustpy_ai::cancel_workers(&ids),
            ServiceId::PluginJob(id) => {
                self.stop_plugin_job(id);
            }
            ServiceId::WindowOperation(id) => self.stop_window_operation(id),
            ServiceId::DockerSnapshot(id, stamp) => self.stop_docker_services(id, &stamp),
            ServiceId::RemoteDevelopment(id, stamp) => {
                if self
                    .remote_development_pending()
                    .iter()
                    .any(|owner| owner.0 == id && owner.2 == stamp)
                {
                    self.stop_remote_development(id);
                }
            }
            ServiceId::DatabaseSession(id) => self.db_panel.stop_database(id),
            ServiceId::Notebook(id) => {
                if let Some(notebook) = self
                    .tabs
                    .iter_mut()
                    .filter_map(|tab| tab.notebook.as_mut())
                    .find(|notebook| {
                        notebook
                            .service_snapshot()
                            .is_some_and(|(key, _)| key == id)
                    })
                {
                    notebook.shutdown();
                }
            }
            ServiceId::Console(id) => {
                if self.py_console.select_service(id) {
                    self.py_console.stop();
                }
            }
            ServiceId::Terminal(id) => {
                if let Some(index) = self.terminal_panel.tabs.iter().position(|tab| tab.id == id) {
                    self.terminal_panel.close(index);
                }
            }
            ServiceId::Configuration(_) => {}
        }
    }
    fn restart_service(&mut self, id: ServiceId) {
        if self.project_safe_mode() {
            self.set_status("Trust the project before restarting a service");
            return;
        }
        match id {
            ServiceId::Process(id) => self.rerun_session(id),
            ServiceId::Debug(id) => {
                if self.select_debug_session(id) {
                    self.debugger_action(Action::RestartDebug);
                }
            }
            ServiceId::Tests(id) => {
                if self.test_panel.service_id() == Some(id) {
                    if let Err(error) = self.test_panel.rerun() {
                        self.set_status(error);
                    }
                    self.services_panel.selected =
                        self.test_panel.service_id().map(ServiceId::Tests);
                }
            }
            ServiceId::SettingsSync(_, _) => {
                self.set_status("Open Settings Sync to start a new operation")
            }
            ServiceId::PluginJob(_) => self.plugin_runtime.open = true,
            ServiceId::WindowOperation(_) => self.bg.tasks_popup = true,
            ServiceId::DockerSnapshot(_, _) => {
                self.set_status("Use Refresh in Docker Services to start another snapshot")
            }
            ServiceId::WebLanguages(_) => {
                self.dialog = Dialog::WebLanguages;
            }
            ServiceId::Collaboration(_) => {
                self.collaboration.visible = true;
            }
            ServiceId::AiWorkers(_) => self.ai_panel.visible = true,
            ServiceId::RemoteDevelopment(_, _) => self.remote_development.open = true,
            ServiceId::DatabaseSession(_) => {
                self.set_status("Open a new database console to reconnect a stopped session")
            }
            ServiceId::Database(id) => {
                self.db_panel.set_database_execution_allowed(
                    self.builtin_plugin_enabled("database") && !self.project_safe_mode(),
                );
                self.db_panel.disconnect_service(&id);
                if let Err(error) = self.db_panel.connect_service(&id) {
                    self.set_status(error);
                }
            }
            ServiceId::Notebook(id) => {
                if let Some(notebook) = self
                    .tabs
                    .iter_mut()
                    .filter_map(|tab| tab.notebook.as_mut())
                    .find(|notebook| {
                        notebook
                            .service_snapshot()
                            .is_some_and(|(key, _)| key == id)
                    })
                {
                    notebook.restart_service();
                }
            }
            ServiceId::Console(id) if self.py_console.select_service(id) => {
                self.py_console.stop();
                if self.ensure_py_console() {
                    self.services_panel.selected =
                        self.py_console.service_id().map(ServiceId::Console);
                }
            }
            _ => {}
        }
    }
    fn show_service(&mut self, id: ServiceId) {
        match id {
            ServiceId::Deployment(id, stamp) => {
                if self
                    .deploy
                    .pending_transfers()
                    .iter()
                    .any(|(key, _, state)| *key == id && state == &stamp)
                {
                    if let Some(project) = &self.project {
                        self.deploy.open_targets(&project.root);
                    }
                }
            }
            ServiceId::WebPreview(stamps) => {
                if stamps == self.web_preview_stamps() {
                    self.bottom = Some(BottomTool::WebPreview);
                }
            }
            ServiceId::Http(id) => {
                self.show_http_request(id);
            }
            ServiceId::Measured(id) => {
                if let Some(tool) = self.show_measured_service(id) {
                    self.bottom = Some(tool);
                }
            }
            ServiceId::Process(id) => {
                if self
                    .run_panel
                    .sessions
                    .iter()
                    .any(|session| session.id == id)
                {
                    self.run_panel.active = Some(id);
                    self.bottom = Some(BottomTool::Run);
                }
            }
            ServiceId::Debug(id) => {
                if self.select_debug_session(id) {
                    self.bottom = Some(BottomTool::Debug);
                }
            }
            ServiceId::Tests(id) => {
                if self.test_panel.service_id() == Some(id) {
                    self.bottom = Some(BottomTool::Tests);
                }
            }
            ServiceId::SettingsSync(_, _) => self.open_settings_sync(false),
            ServiceId::PluginJob(_) => self.plugin_runtime.open = true,
            ServiceId::WindowOperation(_) => self.bg.tasks_popup = true,
            ServiceId::DockerSnapshot(_, _) => self.bottom = Some(BottomTool::Services),
            ServiceId::WebLanguages(_) => {
                self.dialog = Dialog::WebLanguages;
            }
            ServiceId::Collaboration(_) => {
                self.collaboration.visible = true;
            }
            ServiceId::AiWorkers(_) => self.ai_panel.visible = true,
            ServiceId::RemoteDevelopment(_, _) => self.remote_development.open = true,
            ServiceId::DatabaseSession(id) => {
                if self.db_panel.select_database_owner(id) {
                    self.bottom = Some(BottomTool::Database);
                }
            }
            ServiceId::Database(id) => {
                if self.db_panel.select_service(&id) {
                    self.bottom = Some(BottomTool::Database);
                }
            }
            ServiceId::Notebook(id) => {
                if let Some(index) = self.tabs.iter().position(|tab| {
                    tab.notebook.as_ref().is_some_and(|notebook| {
                        notebook
                            .service_snapshot()
                            .is_some_and(|(key, _)| key == id)
                    })
                }) {
                    self.active = Some(index);
                    self.bottom = None;
                }
            }
            ServiceId::Console(id) => {
                if self.py_console.select_service(id) {
                    self.bottom = Some(BottomTool::PythonConsole);
                }
            }
            ServiceId::Terminal(id) => {
                if let Some(index) = self.terminal_panel.tabs.iter().position(|tab| tab.id == id) {
                    self.terminal_panel.active = index;
                    self.bottom = Some(BottomTool::Terminal);
                }
            }
            ServiceId::Configuration(name) => {
                if let Some(project) = &mut self.project {
                    if project
                        .run_configs
                        .configs
                        .iter()
                        .any(|config| config.name == name)
                    {
                        project.run_configs.selected = Some(name);
                        self.dialog = Dialog::RunConfigurations;
                    }
                }
            }
        }
    }
    fn service_console_ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme) {
        let mono = egui::FontId::new(
            self.settings.editor.font_size.max(10.0),
            crate::theme::mono_family(false, false),
        );
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        let Some(selected) = self.services_panel.selected.clone() else {
            ui.label("Select a service to attach its console.");
            return;
        };
        match selected {
            ServiceId::Deployment(id, stamp) => {
                if let Some((_, label, _)) = self
                    .deploy
                    .pending_transfers()
                    .into_iter()
                    .find(|(key, _, state)| *key == id && state == &stamp)
                {
                    ui.heading(label);
                    if ui.button("Stop operation").clicked() {
                        self.stop_deployment(id, &stamp);
                    }
                    if ui.button("Open Deployment targets").clicked() {
                        self.show_service(ServiceId::Deployment(id, stamp));
                    }
                } else {
                    ui.label("This deployment operation has finished or changed.");
                }
            }
            ServiceId::WebPreview(stamps) => {
                if stamps == self.web_preview_stamps() {
                    self.web_preview_tool(ui);
                } else {
                    ui.label("This web preview has closed or changed.");
                }
            }
            ServiceId::Http(id) => {
                if self.http_panel.show_execution(id) {
                    self.http_panel.trusted = !self.project_safe_mode();
                    let events = self.http_panel.ui(ui, theme, mono, &root);
                    self.handle_http_events(events, ui.ctx());
                    self.services_panel.selected = self.http_panel.selected.map(ServiceId::Http);
                } else {
                    ui.label("This HTTP request has expired from retained history.");
                }
            }
            ServiceId::Process(id) => {
                if !self
                    .run_panel
                    .sessions
                    .iter()
                    .any(|session| session.id == id)
                {
                    ui.label("This process has closed.");
                    return;
                }
                self.run_panel.active = Some(id);
                for event in self.run_panel.ui(ui, theme, mono) {
                    self.service_run_event(event);
                }
                if let Some(active) = self.run_panel.active {
                    self.services_panel.selected = Some(ServiceId::Process(active));
                }
            }
            ServiceId::WindowOperation(id) => {
                if let Some((_, name)) = self
                    .window_operation_jobs()
                    .into_iter()
                    .find(|job| job.0 == id)
                {
                    ui.label(name);
                    if ui.button("Show Background Tasks").clicked() {
                        self.bg.tasks_popup = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.stop_window_operation(id);
                    }
                } else {
                    ui.label("This operation has retired.");
                }
            }
            ServiceId::DockerSnapshot(id, stamp) => {
                if self
                    .docker_services_pending()
                    .is_some_and(|owner| owner.0 == id && owner.2 == stamp)
                {
                    ui.label(
                        self.services_panel
                            .docker
                            .drain_status()
                            .unwrap_or_default(),
                    );
                    if ui.button("Stop snapshot").clicked() {
                        self.stop_docker_services(id, &stamp);
                    }
                } else {
                    ui.label("This Docker snapshot has retired.");
                }
            }
            ServiceId::PluginJob(id) => {
                if let Some((_, name)) = self.plugin_jobs().into_iter().find(|owner| owner.0 == id)
                {
                    ui.label(name);
                    if ui
                        .button("Open Executable Plugins / IDE Scripting")
                        .clicked()
                    {
                        self.plugin_runtime.open = true;
                    }
                    if ui.button("Stop").clicked() {
                        self.stop_plugin_job(id);
                    }
                } else {
                    ui.label("This plugin job has retired.");
                }
            }
            ServiceId::SettingsSync(id, stamp) => {
                if self
                    .settings_sync_pending()
                    .iter()
                    .any(|owner| owner.0 == id && owner.2 == stamp)
                {
                    ui.label("Settings synchronization is running");
                    if ui.button("Open Settings Sync").clicked() {
                        self.open_settings_sync(false);
                    }
                    if ui.button("Stop").clicked() {
                        self.stop_settings_sync(id);
                    }
                } else {
                    ui.label("This settings operation has finished.");
                }
            }
            ServiceId::WebLanguages(stamps) => {
                if stamps == self.web_languages.stamps() {
                    if ui.button("Open Web Development").clicked() {
                        self.dialog = Dialog::WebLanguages;
                    }
                    if ui.button("Stop servers").clicked() {
                        self.stop_web_languages();
                    }
                } else {
                    ui.label("These language-server owners have retired.");
                }
            }
            ServiceId::Collaboration(stamp) => {
                if stamp == self.collaboration_stamp() {
                    if ui.button("Open Collaboration").clicked() {
                        self.collaboration.visible = true;
                    }
                    if ui.button("Disconnect").clicked() {
                        self.stop_collaboration();
                    }
                } else {
                    ui.label("This collaboration session has retired.");
                }
            }
            ServiceId::AiWorkers(ids) => {
                ui.label(if rustpy_ai::workers_running(&ids) {
                    "AI request readers are still running or finishing cancellation"
                } else {
                    "These AI requests have retired"
                });
                if ui.button("Open AI Assistant").clicked() {
                    self.ai_panel.visible = true;
                }
                if ui.button("Cancel requests").clicked() {
                    rustpy_ai::cancel_workers(&ids);
                }
            }
            ServiceId::RemoteDevelopment(id, stamp) => {
                if self
                    .remote_development_pending()
                    .iter()
                    .any(|owner| owner.0 == id && owner.2 == stamp)
                {
                    ui.label("Remote workspace transport or port forward is active");
                    if ui.button("Open Remote Development").clicked() {
                        self.remote_development.open = true;
                    }
                    if ui.button("Disconnect / Stop forward").clicked() {
                        self.stop_remote_development(id);
                    }
                } else {
                    ui.label("This remote owner has retired.");
                }
            }
            ServiceId::DatabaseSession(id) => {
                if let Some((_, name, busy, stopping)) = self
                    .db_panel
                    .database_owners()
                    .into_iter()
                    .find(|owner| owner.0 == id)
                {
                    ui.heading(name);
                    ui.label(if stopping {
                        "Waiting for the driver to finish stopping"
                    } else if busy {
                        "Executing database operation"
                    } else {
                        "Connected database session"
                    });
                    if ui.button("Open Database Console").clicked()
                        && self.db_panel.select_database_owner(id)
                    {
                        self.bottom = Some(BottomTool::Database);
                    }
                    if ui
                        .add_enabled(!stopping, egui::Button::new("Stop"))
                        .clicked()
                    {
                        self.db_panel.stop_database(id);
                    }
                } else {
                    ui.label("This database session has closed.");
                }
            }
            ServiceId::Database(id) => {
                if !self.db_panel.select_service(&id) {
                    ui.label("This data source was removed.");
                    return;
                }
                for event in self.db_panel.ui(ui, theme, mono, &root) {
                    match event {
                        DbEvent::Status(message) => self.set_status(message),
                        DbEvent::Navigate(path, line) => {
                            self.open_file(&path);
                            if let Some(index) = self.active {
                                self.tabs[index].state.go_to_line(line, None);
                            }
                        }
                    }
                }
                if let Some((id, _, _)) =
                    self.db_panel
                        .service_rows()
                        .into_iter()
                        .find(|(_, source, _)| {
                            self.db_panel.selected_source.as_deref() == Some(source.name.as_str())
                        })
                {
                    self.services_panel.selected = Some(ServiceId::Database(id));
                }
            }
            ServiceId::Notebook(id) => {
                if let Some(index) = self.tabs.iter().position(|tab| {
                    tab.notebook.as_ref().is_some_and(|notebook| {
                        notebook
                            .service_snapshot()
                            .is_some_and(|(key, _)| key == id)
                    })
                }) {
                    self.notebook_area(ui, index);
                } else {
                    ui.label("This kernel has stopped.");
                }
            }
            ServiceId::Console(id) => {
                if self.py_console.select_service(id) {
                    self.py_console.ui(ui, theme, mono);
                    self.consume_python_console_requests();
                } else {
                    ui.label("This console has closed.");
                }
            }
            ServiceId::Terminal(id) => {
                if let Some(index) = self.terminal_panel.tabs.iter().position(|tab| tab.id == id) {
                    self.terminal_panel.active = index;
                    let env = crate::terminal_panel::terminal_env(
                        self.settings.terminal.activate_virtualenv,
                        self.project
                            .as_ref()
                            .and_then(|project| project.interpreter.as_ref()),
                    );
                    self.terminal_panel.ui(ui, theme, mono, &root, &env);
                    if let Some(tab) = self.terminal_panel.tabs.get(self.terminal_panel.active) {
                        self.services_panel.selected = Some(ServiceId::Terminal(tab.id));
                    }
                } else {
                    ui.label("This terminal has closed.");
                }
            }
            ServiceId::Measured(id) => {
                if let Some(tool) = self.show_measured_service(id) {
                    ui.heading(format!("{} run", tool.title()));
                    if ui.button("Open console / report").clicked() {
                        self.bottom = Some(tool);
                    }
                    if ui.button("Stop this run").clicked() {
                        self.stop_measurement(id);
                    }
                } else {
                    ui.label("This run has closed.");
                }
            }
            ServiceId::Tests(id) => {
                if self.test_panel.service_id() != Some(id) {
                    ui.label("This test session has closed.");
                    return;
                }
                for event in self.test_panel.ui(ui, theme, mono) {
                    match event {
                        TestPanelEvent::Navigate(path, line) => self.service_open_file(path, line),
                        TestPanelEvent::Status(message) => self.set_status(message),
                        TestPanelEvent::Debug(id) => self.debug_test_node(id),
                        TestPanelEvent::Measure { profile, target } => {
                            self.measure_test_run(profile, target)
                        }
                        TestPanelEvent::FixtureUsages(path, line) => {
                            self.test_fixture_usages(path, line)
                        }
                    }
                }
            }
            ServiceId::Debug(id) => {
                if let Some(debug) =
                    self.debug
                        .as_mut()
                        .filter(|debug| debug.id == id)
                        .or_else(|| {
                            self.debug_sessions
                                .iter_mut()
                                .find(|(debug, _)| debug.id == id)
                                .map(|(debug, _)| debug)
                        })
                {
                    let debug_input = self.services_panel.debug_inputs.entry(id).or_default();
                    ui.heading(format!("{} — {:?}", debug.name, debug.state));
                    egui::ScrollArea::vertical()
                        .id_salt(("service_debug_output", id))
                        .max_height((ui.available_height() - 70.0).max(40.0))
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for chunk in &debug.output {
                                ui.label(egui::RichText::new(&chunk.text).monospace().color(
                                    if chunk.kind == rustpy_run::OutputKind::Stderr {
                                        theme.error
                                    } else {
                                        theme.text
                                    },
                                ));
                            }
                            for item in &debug.repl {
                                let (text, color) = match &item.entry {
                                    rustpy_debug::ReplEntry::Input(source) => {
                                        (format!(">>> {source}"), theme.text_dim)
                                    }
                                    rustpy_debug::ReplEntry::Value(value) => {
                                        (value.value.clone(), theme.text)
                                    }
                                    rustpy_debug::ReplEntry::Error(error) => {
                                        (error.clone(), theme.error)
                                    }
                                };
                                ui.label(egui::RichText::new(text).monospace().color(color));
                            }
                        });
                    ui.horizontal(|ui| {
                        let response = ui.add_enabled(
                            debug.is_stopped(),
                            egui::TextEdit::singleline(&mut *debug_input)
                                .hint_text("Evaluate in the selected suspended frame…")
                                .desired_width(f32::INFINITY),
                        );
                        let submit = (response.has_focus() || response.lost_focus())
                            && ui.input(|input| input.key_pressed(egui::Key::Enter));
                        if debug.is_stopped()
                            && (submit || ui.button("Evaluate").clicked())
                            && !debug_input.trim().is_empty()
                        {
                            let source = std::mem::take(&mut *debug_input);
                            rustpy_console::debugger::evaluate_in_console(debug, &source);
                        }
                    });
                    if ui.button("Show Debugger").clicked() {
                        self.select_debug_session(id);
                        self.bottom = Some(BottomTool::Debug);
                    }
                } else {
                    ui.label("This debug session has closed.");
                }
            }
            ServiceId::Configuration(name) => {
                if let Some(config) = self
                    .project
                    .as_ref()
                    .and_then(|project| {
                        project
                            .run_configs
                            .configs
                            .iter()
                            .find(|config| config.name == name)
                    })
                    .cloned()
                {
                    ui.heading(&config.name);
                    ui.label(config.kind_label());
                    ui.label(format!(
                        "Working directory: {}",
                        config.working_dir.as_deref().unwrap_or(&root).display()
                    ));
                    if ui.button("Run").clicked() {
                        self.run_config(config.clone());
                    }
                    if ui.button("Edit configuration…").clicked() {
                        self.handle_service_event(ServicesEvent::Edit(config));
                    }
                }
            }
        }
    }
    fn service_run_event(&mut self, event: RunPanelEvent) {
        self.handle_run_panel_event(event);
    }
    fn service_open_file(&mut self, path: PathBuf, line: usize) {
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        let path = crate::run_panel::resolve_output_path(&root, &path);
        if path.is_file() {
            self.mark_nav_jump();
            self.open_file(&path);
            if let Some(index) = self.opened_editor_index(&path) {
                self.tabs[index].view.cancel_session_restore();
                self.tabs[index].state.go_to_line(line, None);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    struct Fixture {
        app: IdeApp,
        ctx: egui::Context,
        _root: tempfile::TempDir,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            for session in &mut self.app.run_panel.sessions {
                session.stop();
            }
            // Keep the project alive through actual child/pipe retirement even
            // when a UI assertion unwinds. Never equate Stop with completion.
            while self.app.run_panel.sessions.iter().any(|s| s.owns_process()) {
                for session in &mut self.app.run_panel.sessions {
                    session.poll();
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        app.run_panel.settings.confirm_rerun = false;
        let cfg = RunConfig {
            name: "owned service π".into(),
            kind: RunConfigKind::Program {
                program: "sleep".into(),
                args: vec!["30".into()],
            },
            working_dir: Some(root.path().into()),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        let mut fixture = Fixture {
            app,
            ctx,
            _root: root,
        };
        fixture
            .app
            .run_panel
            .add_session(rustpy_run::RunSession::spawn(
                900,
                &cfg,
                None,
                fixture._root.path(),
            ));
        assert!(
            fixture.app.run_panel.sessions[0].owns_process(),
            "real generic process must start"
        );
        fixture
    }
    fn frame(
        f: &mut Fixture,
        failed_snapshot: bool,
        events: Vec<egui::Event>,
    ) -> egui::accesskit::TreeUpdate {
        let theme = f.app.theme.clone();
        let time = f.ctx.input(|input| input.time) + 0.1;
        let output = f.ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                focused: true,
                events,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300.0, 800.0))),
                ..Default::default()
            },
            |ui| {
                let mut rows = f.app.service_processes();
                if failed_snapshot {
                    // Model the legitimate late-error presentation emitted by a
                    // daemon owner, keeping real captured physical ownership. No
                    // fake daemon or forged RunSession ownership is installed.
                    rows[0].status =
                        rustpy_run::SessionStatus::Failed("retirement unresolved π".into());
                }
                let events = f.app.services_panel.ui(
                    ui,
                    &theme,
                    services_panel::ServicesInput {
                        configs: &[],
                        processes: rows,
                        resources: Vec::new(),
                    },
                );
                for event in events {
                    f.app.handle_service_event(event);
                }
            },
        );
        let update = output.platform_output.accesskit_update.clone().unwrap();
        output.drop_without_applying_deltas();
        update
    }
    fn retire(f: &mut Fixture, id: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let session = f
                .app
                .run_panel
                .sessions
                .iter_mut()
                .find(|s| s.id == id)
                .unwrap();
            session.poll();
            if !session.owns_process() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "service did not physically retire"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn failed_owned_process_stays_visible_and_actual_services_stop_retires_it() {
        let mut f = fixture();
        let before_pid = f.app.run_panel.sessions[0]
            .process_id()
            .expect("real child PID");
        frame(&mut f, true, vec![]);
        let update = frame(&mut f, true, vec![]);
        // egui exposes static Label text through AccessKit's value property;
        // its button/selectable labels use the separate label property.
        assert!(
            update.nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::Label
                    && node
                        .value()
                        .is_some_and(|text| text.contains("failed: retirement unresolved π"))
            }),
            "actual Services failure text missing from Label/value: {:?}",
            update
                .nodes
                .iter()
                .map(|(_, node)| (node.role(), node.label(), node.value()))
                .collect::<Vec<_>>()
        );
        let bounds = update
            .nodes
            .iter()
            .find_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Button && node.label() == Some("■"))
                    .then(|| node.bounds())
                    .flatten()
            })
            .expect("failed physically owned service retains actual Stop button");
        let point = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        frame(
            &mut f,
            true,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        retire(&mut f, 900);
        assert!(!f.app.service_processes()[0].owns_process);
        let update = frame(&mut f, true, vec![]);
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("owned service π")),
            "retired process leaves the default live listing"
        );
        #[cfg(target_os = "linux")]
        assert!(
            !PathBuf::from(format!("/proc/{before_pid}")).exists(),
            "child was reaped before retirement publication"
        );
        #[cfg(not(target_os = "linux"))]
        let _ = before_pid;
    }
    #[test]
    fn services_add_select_attach_show_and_rerun_dispatch_to_existing_owners() {
        let mut f = fixture();
        f.app
            .handle_service_event(ServicesEvent::Add(AddService::Configuration));
        assert!(matches!(f.app.dialog, Dialog::RunConfigurations));
        f.app
            .handle_service_event(ServicesEvent::Add(AddService::Django));
        let django = f
            .app
            .project
            .as_ref()
            .unwrap()
            .run_configs
            .configs
            .last()
            .unwrap();
        assert_eq!(
            services_panel::configuration_group(django),
            "Django servers"
        );
        assert_eq!(django.parameters, "runserver");
        f.app
            .handle_service_event(ServicesEvent::Add(AddService::Jupyter));
        let jupyter = f
            .app
            .project
            .as_ref()
            .unwrap()
            .run_configs
            .configs
            .last()
            .unwrap();
        assert_eq!(
            services_panel::configuration_group(jupyter),
            "Jupyter servers"
        );
        assert_eq!(jupyter.parameters, "lab --no-browser");
        f.app
            .handle_service_event(ServicesEvent::Add(AddService::Database));
        assert_eq!(f.app.bottom, Some(BottomTool::Database));
        assert!(f.app.db_panel.show_console);
        f.app.dialog = Dialog::None;
        f.app
            .handle_service_event(ServicesEvent::Select(ServiceId::Process(900)));
        let theme = f.app.theme.clone();
        let output = f.ctx.run_ui(Default::default(), |ui| {
            f.app.service_console_ui(ui, &theme)
        });
        output.drop_without_applying_deltas();
        assert_eq!(f.app.run_panel.active, Some(900));
        assert_eq!(
            f.app.run_panel.sessions.len(),
            1,
            "attach must reuse the captured owner"
        );
        f.app
            .handle_service_event(ServicesEvent::ShowService(ServiceId::Process(900)));
        assert_eq!(f.app.bottom, Some(BottomTool::Run));
        f.app
            .handle_service_event(ServicesEvent::StopService(ServiceId::Process(900)));
        retire(&mut f, 900);
        f.app.handle_service_event(ServicesEvent::RerunSession(900));
        assert!(
            f.app
                .run_panel
                .sessions
                .iter()
                .any(|s| s.id != 900 && s.name == "owned service π" && s.owns_process()),
            "rerun must launch the captured real Program configuration"
        );
    }
}
