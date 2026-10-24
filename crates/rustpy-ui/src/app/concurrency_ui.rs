//! Concurrency capture uses the shared launch workflow before this dispatch.
use super::*;
impl IdeApp {
    pub(super) fn start_concurrency_config(&mut self, cfg: RunConfig) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            self.set_status("Trust the project before running a concurrency capture");
            return;
        }
        if !self.save_all() {
            return;
        }
        let Some(cfg) = self.process_test_cfg(cfg) else {
            return;
        };
        let launch = self.launch_cfg(&cfg);
        self.report_env_errors(&launch);
        let Some(project) = &self.project else {
            return;
        };
        let root = project.root.clone();
        let interpreter = project.interpreter.clone();
        let settings = &self.settings.concurrency;
        let options = rustpy_run::concurrency::CaptureOptions {
            threads: settings.threads,
            asyncio: settings.asyncio,
            locks: settings.locks,
            project_only: settings.project_only,
            max_events: settings.max_events,
            max_lanes: settings.max_lanes,
            roots: std::iter::once(root.clone())
                .chain(project.tree.model().content_roots.iter().cloned())
                .collect(),
        };
        let id = self.next_session_id;
        self.next_session_id += 1;
        match rustpy_run::concurrency::run_with_concurrency(
            id,
            &launch,
            interpreter.as_ref(),
            &root,
            &options,
        ) {
            Ok(run) => {
                self.begin_measurement(
                    super::run_configuration_sessions::Kind::Concurrency,
                    &cfg.name,
                );
                self.concurrency_panel.minimum_duration_ms =
                    self.settings.concurrency.minimum_duration_ms;
                self.concurrency_panel.follow_live = self.settings.concurrency.follow_live;
                self.concurrency_panel.start(run, cfg, root);
                self.bottom = Some(BottomTool::Concurrency);
            }
            Err(error) => self.set_status(format!("Cannot capture concurrency: {error}")),
        }
    }
    pub(super) fn concurrency_panel_ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme) {
        for event in self.concurrency_panel.ui(ui, theme) {
            match event {
                ConcurrencyEvent::Rerun => {
                    if let Some(config) = self.concurrency_panel.last_config.clone() {
                        self.launch_config(config, crate::run_context::RunMode::Concurrency);
                    }
                }
                ConcurrencyEvent::Settings => {
                    self.settings_ui.page = settings_ui::SettingsPage::Concurrency;
                    self.dialog = Dialog::Settings;
                }
                ConcurrencyEvent::Navigate(path, line) => {
                    if path.is_file() {
                        self.mark_nav_jump();
                        self.open_file(&path);
                        if let Some(tab) = self
                            .opened_editor_index(&path)
                            .and_then(|index| self.tabs.get_mut(index))
                        {
                            tab.view.cancel_session_restore();
                            tab.state.go_to_line(line, None);
                        }
                        self.focus_editor = true;
                    }
                }
            }
        }
    }
    pub(super) fn page_concurrency(&mut self, ui: &mut egui::Ui) {
        ui.heading("Thread Concurrency Visualization");
        let settings = &mut self.settings.concurrency;
        ui.checkbox(&mut settings.threads, "Capture Python threads");
        ui.checkbox(
            &mut settings.asyncio,
            "Capture asyncio tasks and suspension",
        );
        ui.checkbox(
            &mut settings.locks,
            "Capture lock and semaphore acquisition waits",
        );
        ui.checkbox(
            &mut settings.project_only,
            "Restrict source spans to project content roots",
        );
        ui.horizontal(|ui| {
            ui.label("Maximum events");
            ui.add(egui::DragValue::new(&mut settings.max_events).range(1000..=1_000_000));
        });
        ui.horizontal(|ui| {
            ui.label("Maximum thread/task lanes");
            ui.add(egui::DragValue::new(&mut settings.max_lanes).range(1..=1024));
        });
        ui.horizontal(|ui| {
            ui.label("Hide spans shorter than");
            ui.add(
                egui::DragValue::new(&mut settings.minimum_duration_ms)
                    .range(0.0..=10000.0)
                    .suffix(" ms"),
            );
        });
        ui.checkbox(&mut settings.follow_live, "Follow the live timeline");
        ui.label("Execution bars show Python call lifetimes. Lock bars measure acquisition attempts, including time waiting. Await bars show coroutine suspension; native code and other processes are outside this capture.");
    }
}
