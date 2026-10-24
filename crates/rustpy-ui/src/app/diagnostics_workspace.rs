//! Explicit local captures and activity information for the running IDE.
use super::*;
use crate::a11y::DialogWindow;
use crate::diagnostic_tools::{self, Artifact, Kind};
use rustpy_core::{TaskHandle, TaskManager};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct State {
    monitor: bool,
    tools: bool,
    report: bool,
    title: String,
    description: String,
    include_system: bool,
    job: Option<(Kind, TaskHandle<Artifact>)>,
    artifact: Option<Artifact>,
    message: String,
    cpu_sample: Option<(Instant, Duration)>,
    process_cpu_percent: Option<f64>,
}

fn issue_url(title: &str, description: &str, system: Option<&str>) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() || description.trim().is_empty() {
        return Err("Enter a title and steps to reproduce".into());
    }
    if title.len() > 256 || description.len() > 12_000 {
        return Err("Keep the title below 256 bytes and the description below 12 KB".into());
    }
    let mut body = description.to_string();
    if let Some(system) = system {
        body.push_str("\n\nSystem information:\n");
        body.push_str(system);
    }
    let mut url = url::Url::parse("https://github.com/themuddfamily/rustpy/issues/new")
        .map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("title", title)
        .append_pair("body", &body);
    Ok(url.into())
}

impl IdeApp {
    pub(super) fn diagnostics_action(&mut self, action: Action) {
        match action {
            Action::IdeActivityMonitor => self.diagnostics_workspace.monitor = true,
            Action::ReportIdeIssue => self.diagnostics_workspace.report = true,
            Action::DumpIdeThreads => self.start_ide_capture(Kind::Threads),
            Action::ProfileIdeCpu => self.start_ide_capture(Kind::Profile),
            _ => {}
        }
    }

    fn start_ide_capture(&mut self, kind: Kind) {
        self.diagnostics_workspace.tools = true;
        if self.diagnostics_workspace.job.is_some() {
            self.set_status("An IDE diagnostic capture is already running");
            return;
        }
        // Scripted GUI checks may show these controls but cannot attach a debugger
        // or sampler to the host process as a side effect of queued actions.
        if cfg!(test) || self.screenshot_path.is_some() || self.input_player.is_some() {
            self.diagnostics_workspace.message =
                "Captures require an explicit action in an ordinary IDE session".into();
            return;
        }
        let directory = Settings::log_dir().join("diagnostics");
        if let Err(error) = std::fs::create_dir_all(&directory) {
            self.diagnostics_workspace.message = error.to_string();
            return;
        }
        if !std::fs::symlink_metadata(&directory)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        {
            self.diagnostics_workspace.message =
                "The diagnostic destination must be a regular directory".into();
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(error) =
                std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            {
                self.diagnostics_workspace.message = error.to_string();
                return;
            }
        }
        let suffix = if kind == Kind::Profile {
            "json.gz"
        } else {
            "txt"
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let output = directory.join(format!(
            "ide-{}-{}-{}.{}",
            std::process::id(),
            stamp,
            window_lifecycle::next_process_owner(),
            suffix
        ));
        let plan = match diagnostic_tools::plan(
            kind,
            &self.settings.diagnostics,
            std::process::id(),
            output,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.diagnostics_workspace.message = error;
                return;
            }
        };
        let title = if kind == Kind::Profile {
            "IDE CPU profile"
        } else {
            "IDE thread dump"
        };
        self.diagnostics_workspace.message = format!(
            "{title} is running; OS attach permissions and the configured tool are required"
        );
        self.diagnostics_workspace.job = Some((
            kind,
            TaskManager::global()
                .task(title)
                .spawn(move |progress| diagnostic_tools::capture(plan, progress)),
        ));
    }

    pub(super) fn ide_capture_owner(&self) -> Option<(u64, String)> {
        self.diagnostics_workspace
            .job
            .as_ref()
            .map(|(_, job)| (job.id(), job.title().into()))
    }

    pub(super) fn stop_ide_capture(&mut self, id: Option<u64>) {
        if let Some((_, job)) = &self.diagnostics_workspace.job {
            if id.is_none_or(|id| id == job.id()) {
                job.cancel();
            }
        }
    }

    pub(super) fn poll_ide_capture(&mut self, ctx: &egui::Context, retiring: bool) -> bool {
        if retiring {
            self.stop_ide_capture(None);
        }
        let result = self
            .diagnostics_workspace
            .job
            .as_mut()
            .and_then(|(_, job)| job.try_result());
        if let Some(result) = result {
            self.diagnostics_workspace.job = None;
            match result {
                Ok(artifact) => {
                    self.diagnostics_workspace.message =
                        format!("Saved locally: {}", artifact.path.display());
                    if !retiring {
                        self.diagnostics_workspace.tools = true;
                    }
                    self.diagnostics_workspace.artifact = Some(artifact);
                }
                Err(error) => self.diagnostics_workspace.message = error.to_string(),
            }
        }
        let pending = self.diagnostics_workspace.job.is_some();
        if pending {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        pending
    }

    pub(super) fn diagnostics_settings_page(&mut self, ui: &mut egui::Ui) {
        let settings = &mut self.settings.diagnostics;
        ui.heading(crate::i18n::tr_static("IDE diagnostic tools"));
        ui.label(crate::i18n::tr_static("Thread dumps use an installed debugger; CPU recording uses samply. Tools attach only to this RustPy process after an explicit action."));
        ui.label(crate::i18n::tr_static(
            "Debugger executable (gdb, lldb or cdb)",
        ));
        ui.text_edit_singleline(&mut settings.debugger_program);
        ui.label(crate::i18n::tr_static("Samply executable"));
        ui.text_edit_singleline(&mut settings.profiler_program);
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr_static("Profile duration (seconds)"));
            ui.add(egui::DragValue::new(&mut settings.profile_seconds).range(1..=60));
        });
        ui.label(crate::i18n::tr_static("The executable is one path, with no shell expansion. Captures stay in the log directory. OS debugger/profiler attachment permissions may need configuration."));
    }

    pub(super) fn data_sharing_settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading(crate::i18n::tr_static("Data Sharing"));
        ui.checkbox(
            &mut self.settings.diagnostics.collect_usage_statistics,
            crate::i18n::tr_static("Collect local feature usage statistics for Productivity Guide"),
        );
        ui.label(crate::i18n::tr_static("Disabled by default. Stores bounded action IDs and counts locally, without code, filenames, typed text or credentials. Disabling stops collection; Productivity Guide can reset saved counts."));
        ui.label(crate::i18n::tr_static("Usage statistics and diagnostic captures are never uploaded automatically. Review locally collected logs and crash backtraces before sharing them."));
        if ui
            .button(crate::i18n::tr_static("Open Productivity Guide"))
            .clicked()
        {
            self.help_action(Action::ProductivityGuide, ui.ctx());
        }
    }

    pub(super) fn diagnostics_workspace_ui(&mut self, ctx: &egui::Context) {
        if self.diagnostics_workspace.monitor {
            let state = &mut self.diagnostics_workspace;
            let now = Instant::now();
            if state
                .cpu_sample
                .as_ref()
                .is_none_or(|(at, _)| at.elapsed() >= Duration::from_millis(750))
            {
                if let Some(cpu) = rustpy_core::tasks::process_cpu_time() {
                    state.process_cpu_percent = state.cpu_sample.and_then(|(at, before)| {
                        cpu.checked_sub(before).map(|elapsed| {
                            elapsed.as_secs_f64() / now.duration_since(at).as_secs_f64().max(0.001)
                                * 100.0
                        })
                    });
                    state.cpu_sample = Some((now, cpu));
                }
            }
            egui::Window::new(crate::i18n::tr_static("IDE Activity Monitor")).open(&mut state.monitor).default_width(700.0).show_modeless(ctx, |ui| {
                ui.label(state.process_cpu_percent.map(|percent|crate::i18n::tr_fmt("Process CPU: {percent}% (100% = one CPU core)",&[("percent",&format!("{percent:.1}"))])).unwrap_or_else(||crate::i18n::tr("Process CPU: waiting for an OS sample").into_owned()));
                if let Some(bytes) = layout::resident_memory_bytes() { ui.label(crate::i18n::tr_fmt("Resident memory: {memory}",&[("memory",&layout::format_memory(bytes))])); }
                ui.label(crate::i18n::tr_static("Worker CPU measures each task's own thread at progress checkpoints. Parallel helper threads and other IDE work are included in process CPU; they are not assigned to a task here."));
                let manager = TaskManager::global();
                let mut tasks = manager.running_all();
                tasks.extend(manager.finished());
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    egui::Grid::new("ide_activity_tasks").striped(true).show(ui, |ui| {
                        for title in ["Task / subsystem", "Elapsed", "Worker CPU", "State"] { ui.strong(crate::i18n::tr(title)); } ui.end_row();
                        for task in tasks {
                            ui.label(&task.title).on_hover_text(format!("{}\n{}", task.text, task.details));
                            ui.label(format!("{:.2} s", task.elapsed.as_secs_f64()));
                            ui.label(task.cpu_time.map(|time|format!("{:.1} ms",time.as_secs_f64()*1000.0)).unwrap_or_else(||crate::i18n::tr("Unavailable").into_owned()));
                            if let Some(outcome) = task.outcome { ui.label(format!("{outcome:?}")); }
                            else if ui.add_enabled(task.cancellable && !task.cancel_requested, egui::Button::new(if task.cancel_requested {crate::i18n::tr_static("Stopping")} else {crate::i18n::tr_static("Cancel")})).clicked() { manager.cancel(task.id); }
                            ui.end_row();
                        }
                    });
                });
                ctx.request_repaint_after(Duration::from_millis(750));
            });
        }
        if self.diagnostics_workspace.tools {
            let mut open = true;
            let mut analyze = false;
            let mut load = false;
            let mut configure = false;
            let mut cancel = None;
            egui::Window::new(crate::i18n::tr_static("IDE Diagnostic Capture"))
                .open(&mut open)
                .default_width(580.0)
                .show_modeless(ctx, |ui| {
                    ui.label(&self.diagnostics_workspace.message);
                    if let Some((id, title)) = self.ide_capture_owner() {
                        crate::a11y::spinner(ui);
                        ui.label(title);
                        if ui
                            .button(crate::i18n::tr_static("Cancel capture"))
                            .clicked()
                        {
                            cancel = Some(id);
                        }
                    }
                    if let Some(artifact) = &self.diagnostics_workspace.artifact {
                        ui.monospace(artifact.path.display().to_string());
                        if artifact.text.is_some() {
                            analyze = ui
                                .button(crate::i18n::tr_static("Analyze captured thread stacks"))
                                .clicked();
                        } else {
                            load = ui
                                .button(crate::i18n::tr_static(
                                    "View profile in local samply server",
                                ))
                                .clicked();
                        }
                    }
                    configure = ui
                        .button(crate::i18n::tr_static("Configure diagnostic tools…"))
                        .clicked();
                });
            self.diagnostics_workspace.tools = open;
            if let Some(id) = cancel {
                self.stop_ide_capture(Some(id));
            }
            if configure {
                self.settings_ui.page = settings_ui::SettingsPage::DiagnosticTools;
                self.dialog = Dialog::Settings;
            }
            if analyze {
                if let Some(artifact) = &self.diagnostics_workspace.artifact {
                    self.open_stack_trace_analysis(
                        artifact.text.clone(),
                        Some(artifact.path.clone()),
                        false,
                    );
                }
            }
            if load {
                if let Some(artifact) = &self.diagnostics_workspace.artifact {
                    let spec = crate::run_anything::CommandSpec {
                        program: self.settings.diagnostics.profiler_program.trim().into(),
                        args: vec!["load".into(), artifact.path.to_string_lossy().into_owned()],
                        cwd: artifact.path.parent().unwrap_or(Path::new(".")).into(),
                        env: Vec::new(),
                    };
                    self.spawn_command_session("IDE CPU profile viewer", spec);
                }
            }
        }
        if self.diagnostics_workspace.report {
            let state = &mut self.diagnostics_workspace;
            let mut open = true;
            let mut draft = None;
            egui::Window::new(crate::i18n::tr_static("Report a RustPy issue")).open(&mut open).default_width(620.0).show_modeless(ctx, |ui| {
                ui.label(crate::i18n::tr_static("Title")); ui.text_edit_singleline(&mut state.title);
                ui.label(crate::i18n::tr_static("Steps to reproduce, expected behavior, and actual behavior"));
                ui.add(egui::TextEdit::multiline(&mut state.description).desired_rows(12).desired_width(f32::INFINITY));
                ui.checkbox(&mut state.include_system, crate::i18n::tr_static("Include build / runtime / OS information"));
                let system = help_workspace::about_info();
                if state.include_system { ui.collapsing(crate::i18n::tr_static("Preview system information"), |ui| {ui.monospace(&system);}); }
                ui.label(crate::i18n::tr_static("Opens a browser draft for your review. Logs, diagnostic captures and project files are not attached."));
                if ui.button(crate::i18n::tr_static("Open issue draft in browser")).clicked() {
                    match issue_url(&state.title, &state.description, state.include_system.then_some(system.as_str())) {
                        Ok(url) => draft = Some(url),
                        Err(error) => state.message = error,
                    }
                }
                ui.label(&state.message);
            });
            self.diagnostics_workspace.report = open;
            if let Some(url) = draft {
                if cfg!(test) || self.screenshot_path.is_some() || self.input_player.is_some() {
                    self.set_status(
                        "Issue draft prepared; scripted sessions do not open a browser",
                    );
                } else {
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn issue_draft_encodes_user_text_and_only_includes_system_with_consent() {
        let raw = "expected & actual? unicode π\nsecond line";
        let url = url::Url::parse(&issue_url("A # bug", raw, None).unwrap()).unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(pairs["title"], "A # bug");
        assert_eq!(pairs["body"], raw);
        let url = url::Url::parse(&issue_url("Title", raw, Some("explicit system data")).unwrap())
            .unwrap();
        assert!(url
            .query_pairs()
            .any(|(key, value)| key == "body" && value.contains("explicit system data")));
        assert!(issue_url(" ", raw, None).is_err());
    }
}
