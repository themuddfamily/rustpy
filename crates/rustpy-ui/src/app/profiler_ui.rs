//! Profiler preferences and measured launch options.
use super::*;
use crate::a11y::AccessibleCombo;
impl IdeApp {
    pub(super) fn profiler_options(&mut self) -> rustpy_run::profile::ProfileOptions {
        let settings = &self.settings.profiler;
        let options = rustpy_run::profile::ProfileOptions {
            engine: settings.engine.clone(),
            clock: settings.clock.clone(),
            threads: settings.threads,
            builtins: settings.builtins,
        };
        self.profiler_panel.options = options.clone();
        self.profiler_panel.python = self
            .project
            .as_ref()
            .and_then(|project| project.interpreter.as_ref())
            .map(|interpreter| interpreter.path.clone());
        options
    }
    pub(super) fn profiler_settings_page(&mut self, ui: &mut egui::Ui) {
        let settings = &mut self.settings.profiler;
        ui.heading("Python Profiler");
        egui::ComboBox::from_id_salt("profiler_engine")
            .selected_text(&settings.engine)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.engine,
                    "cprofile".into(),
                    "cProfile (Python standard library)",
                );
                ui.selectable_value(
                    &mut settings.engine,
                    "yappi".into(),
                    "yappi (install in selected interpreter)",
                );
                ui.selectable_value(
                    &mut settings.engine,
                    "vmprof".into(),
                    "vmprof (optional legacy sampling profiler)",
                );
            })
            .combo_named(crate::i18n::tr_static("Profiler"));
        ui.add_enabled_ui(settings.engine == "yappi", |ui| {
            egui::ComboBox::from_id_salt("profiler_clock")
                .selected_text(&settings.clock)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut settings.clock, "wall".into(), "Wall time");
                    ui.selectable_value(&mut settings.clock, "cpu".into(), "CPU time");
                })
                .combo_named(crate::i18n::tr_static("Clock type"));
            ui.checkbox(&mut settings.threads, "Profile all threads");
        });
        ui.checkbox(&mut settings.builtins, "Include built-in functions");
        ui.checkbox(
            &mut settings.activate_view,
            "Activate Profiler tool window after launch",
        );
        ui.checkbox(
            &mut settings.disable_server_reload,
            "Disable Django/Flask development-server reload while profiling",
        );
        ui.label("Profile tests from the Test Runner or a test configuration. For a server, use Finish and collect to stop it and write its pstat results.");
    }
}
