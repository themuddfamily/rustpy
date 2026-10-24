//! Authenticated project-window discovery and exact, captured window activation.
//! Registry verification and IPC run on retained TaskManager workers, never in paint.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_cli::window_registry::WindowInfo;
use rustpy_core::{TaskError, TaskHandle, TaskManager};
#[derive(Clone, Debug, PartialEq, Eq)]
struct Owner {
    session: u64,
    project: Option<PathBuf>,
    identity: Option<String>,
    config: PathBuf,
}
#[derive(Clone)]
enum Intent {
    List,
    Cycle(i8),
    Focus(WindowInfo),
}
enum Output {
    Listed(Vec<WindowInfo>),
    Focused,
}
struct Job {
    owner: Owner,
    generation: u64,
    intent: Intent,
    task: TaskHandle<Output>,
}
#[derive(Default)]
pub(super) struct State {
    identity: Option<String>,
    owner: Option<Owner>,
    generation: u64,
    jobs: Vec<Job>,
    windows: Vec<WindowInfo>,
    listed: bool,
    open: bool,
    error: Option<String>,
}
impl Drop for State {
    fn drop(&mut self) {
        for job in &self.jobs {
            job.task.cancel();
        }
    }
}
fn order(windows: &mut [WindowInfo]) {
    windows.sort_by(|a, b| a.project.cmp(&b.project).then(a.id.cmp(&b.id)));
}
fn cycle(windows: &[WindowInfo], current: &str, direction: i8) -> Option<WindowInfo> {
    let index = windows.iter().position(|window| window.id == current)?;
    if windows.len() < 2 {
        return None;
    }
    let target = if direction < 0 {
        (index + windows.len() - 1) % windows.len()
    } else {
        (index + 1) % windows.len()
    };
    Some(windows[target].clone())
}
fn label(window: &WindowInfo) -> String {
    let project = window
        .project
        .as_deref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "Welcome".into());
    format!(
        "{project} · {}",
        window.id.chars().take(8).collect::<String>()
    )
}
impl IdeApp {
    /// Bound by the shell after its exact authenticated endpoint is published.
    pub(crate) fn bind_project_window_identity(&mut self, identity: String) {
        if self.project_windows.identity.as_ref() != Some(&identity) {
            self.cancel_project_windows();
            self.project_windows.identity = Some(identity);
        }
    }
    fn project_windows_owner(&self) -> Owner {
        Owner {
            session: self.file_session,
            project: self.project.as_ref().map(|p| p.root.clone()),
            identity: self.project_windows.identity.clone(),
            config: Settings::config_dir(),
        }
    }
    fn project_window_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.set_status(message.clone());
        self.project_windows.error = Some(message);
    }
    fn start_project_window_job(&mut self, intent: Intent) {
        if self.exit_requested || self.coordinated_quit_active() || self.native_project_is_closing()
        {
            return;
        }
        // An already-delivered focus request cannot be withdrawn. Keep it owned
        // and serialize further activations until its exact acknowledgement ends.
        if self
            .project_windows
            .jobs
            .iter()
            .any(|job| matches!(job.intent, Intent::Focus(_)))
        {
            self.project_window_error("Waiting for the previous window activation to finish");
            return;
        }
        if self.project_windows.jobs.len() >= 4 {
            self.project_window_error("Waiting for cancelled window discovery to finish");
            return;
        }
        if matches!(&intent, Intent::Focus(window) if window.project.is_none()) {
            self.project_window_error("Only project windows can be activated from this list");
            return;
        }
        self.cancel_project_windows();
        let owner = self.project_windows_owner();
        if matches!(intent, Intent::Cycle(_)) && owner.project.is_none() {
            self.project_window_error("Open a project before cycling windows; use Project Windows to select another project");
            return;
        }
        if matches!(intent, Intent::Cycle(_)) && owner.identity.is_none() {
            self.project_window_error("This window's authenticated identity is not yet published");
            return;
        }
        self.project_windows.owner = Some(owner.clone());
        self.project_windows.error = None;
        let generation = self.project_windows.generation;
        let worker_owner = owner.clone();
        let worker_intent = intent.clone();
        let title = if matches!(intent, Intent::Focus(_)) {
            "Activate project window"
        } else {
            "Discover project windows"
        };
        let task = TaskManager::global().task(title).spawn(move |progress| {
            progress.check()?;
            match worker_intent {
                Intent::List | Intent::Cycle(_) => {
                    progress.set_text("Verifying authenticated project-window endpoints");
                    let mut windows =
                        rustpy_cli::window_registry::live_windows(&worker_owner.config)?;
                    progress.check()?;
                    // The shared IPC UI precondition retains Some(project) but
                    // legacy welcome-window None is unconstrained. Never select
                    // that weaker form from this project-window consumer.
                    windows.retain(|window| window.project.is_some());
                    order(&mut windows);
                    Ok(Output::Listed(windows))
                }
                Intent::Focus(window) => {
                    progress.set_text("Activating the selected project window");
                    progress.set_details(label(&window));
                    progress.check()?;
                    // Empty targets focus the exact existing endpoint. Never
                    // reopen its project, fall back to another owner or spawn.
                    let _ack = rustpy_cli::ipc::forward_window(
                        &window,
                        &[],
                        false,
                        worker_owner.project.as_deref(),
                    )
                    .map_err(|error| TaskError::Failed(error.to_string()))?;
                    progress.check()?;
                    Ok(Output::Focused)
                }
            }
        });
        self.project_windows.jobs.push(Job {
            owner,
            generation,
            intent,
            task,
        });
    }
    pub(super) fn cancel_project_windows(&mut self) {
        self.project_windows.generation = self.project_windows.generation.wrapping_add(1);
        for job in &self.project_windows.jobs {
            job.task.cancel();
        }
        self.project_windows.windows.clear();
        self.project_windows.listed = false;
        self.project_windows.error = None;
    }
    pub(super) fn project_window_jobs(&self) -> Vec<(u64, String)> {
        self.project_windows
            .jobs
            .iter()
            .map(|job| {
                (
                    job.task.id(),
                    if matches!(job.intent, Intent::Focus(_)) {
                        "Activate project window"
                    } else {
                        "Discover project windows"
                    }
                    .into(),
                )
            })
            .collect()
    }
    pub(super) fn stop_project_window_job(&mut self, id: u64) {
        if let Some(job) = self
            .project_windows
            .jobs
            .iter()
            .find(|job| job.task.id() == id)
        {
            job.task.cancel();
            self.project_windows.listed = true;
        }
    }
    pub(super) fn retire_project_windows(&mut self, ctx: &egui::Context) -> bool {
        self.cancel_project_windows();
        self.project_windows.open = false;
        self.project_windows
            .jobs
            .retain_mut(|job| job.task.try_result().is_none());
        let pending = !self.project_windows.jobs.is_empty();
        if pending {
            ctx.request_repaint_after(std::time::Duration::from_millis(25));
        }
        pending
    }
    pub(super) fn project_windows_tick(&mut self, ctx: &egui::Context) {
        let owner = self.project_windows_owner();
        if self
            .project_windows
            .owner
            .as_ref()
            .is_some_and(|old| old != &owner)
        {
            self.cancel_project_windows();
            self.project_windows.owner = Some(owner.clone());
        }
        let generation = self.project_windows.generation;
        let mut completed = Vec::new();
        self.project_windows.jobs.retain_mut(|job| {
            let cancelled = job.task.is_cancelled();
            if let Some(result) = job.task.try_result() {
                if !cancelled && job.generation == generation && job.owner == owner {
                    completed.push((job.intent.clone(), result));
                }
                false
            } else {
                true
            }
        });
        for (intent, result) in completed {
            match result {
                Ok(Output::Listed(windows)) => {
                    self.project_windows.windows = windows;
                    self.project_windows.listed = true;
                    if let Intent::Cycle(direction) = intent {
                        match owner.identity.as_deref().and_then(|id|cycle(&self.project_windows.windows,id,direction)) {
                            Some(window) => self.start_project_window_job(Intent::Focus(window)),
                            None => self.project_window_error("No other authenticated project window, or this window is no longer in the registry"),
                        }
                    }
                }
                Ok(Output::Focused) => self.set_status("Activated the selected project window"),
                Err(error) => {
                    self.project_windows.listed = true;
                    self.project_window_error(error.to_string());
                }
            }
        }
        if !self.project_windows.jobs.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(25));
        }
    }
    pub(super) fn show_project_windows(&mut self) {
        self.project_windows.open = true;
        self.start_project_window_job(Intent::List);
    }
    pub(super) fn cycle_project_windows(&mut self, previous: bool) {
        self.start_project_window_job(Intent::Cycle(if previous { -1 } else { 1 }));
    }
    pub(super) fn project_windows_menu(&mut self, ui: &mut egui::Ui) {
        if !self.project_windows.listed && self.project_windows.jobs.is_empty() {
            self.start_project_window_job(Intent::List);
        }
        let busy = !self.project_windows.jobs.is_empty();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("Refresh windows"))
                .clicked()
            {
                self.start_project_window_job(Intent::List);
            }
            if busy {
                crate::a11y::spinner(ui);
                if ui.button("Cancel discovery / activation").clicked() {
                    self.cancel_project_windows();
                    self.project_windows.listed = true;
                }
            }
        });
        if let Some(error) = &self.project_windows.error {
            ui.colored_label(self.theme.error, error);
        }
        if self.project_windows.listed && self.project_windows.windows.is_empty() {
            ui.label("No authenticated project windows");
        }
        let mut selected = None;
        for window in &self.project_windows.windows {
            let current = self.project_windows.identity.as_ref() == Some(&window.id);
            if ui
                .add_enabled(!busy, egui::Button::new(label(window)).selected(current))
                .clicked()
            {
                selected = Some(window.clone());
            }
        }
        if let Some(window) = selected {
            self.start_project_window_job(Intent::Focus(window));
            ui.close();
        }
        if !self.project_windows.open && ui.button("Open project-window list…").clicked() {
            self.project_windows.open = true;
            ui.close();
        }
    }
    pub(super) fn project_windows_ui(&mut self, ctx: &egui::Context) {
        if !self.project_windows.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Project Windows")
            .id(egui::Id::new("authenticated-project-windows"))
            .open(&mut open)
            .default_width(500.0)
            .show_modeless(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(480.0)
                    .show(ui, |ui| self.project_windows_menu(ui));
            });
        self.project_windows.open = open;
    }
}

#[cfg(test)]
mod fixtures {
    use super::*;
    fn window(id: &str, project: &str) -> WindowInfo {
        WindowInfo {
            id: id.into(),
            project: Some(project.into()),
            endpoint: rustpy_cli::ipc::InstanceInfo {
                native_project_host: false,
                pid: 42,
                port: 1,
                token: "fixture-only-not-a-live-endpoint".into(),
                version: String::new(),
            },
        }
    }
    fn app() -> IdeApp {
        IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Default::default()
            },
            None,
        )
    }
    #[test]
    fn cycling_uses_exact_logical_identity_even_for_same_process_and_project() {
        let mut windows = vec![
            window("b", "/same"),
            window("a", "/same"),
            window("c", "/other"),
        ];
        order(&mut windows);
        let next = cycle(&windows, "a", 1).unwrap();
        assert_eq!(next.id, "b");
        assert_eq!(cycle(&windows, "b", -1).unwrap().id, "a");
        assert!(
            cycle(&windows, "unpublished", 1).is_none(),
            "Never guess a source window by PID/project or active editor"
        );
        let captured = next;
        windows.clear();
        assert_eq!(captured.id, "b");
        assert_eq!(captured.project, Some(PathBuf::from("/same")));
    }
    #[test]
    fn welcome_window_selection_never_uses_an_unconstrained_project_precondition() {
        let mut app = app();
        let mut welcome = window("welcome", "/unused");
        welcome.project = None;
        app.start_project_window_job(Intent::Focus(welcome));
        assert!(app.project_windows.jobs.is_empty());
        assert!(app
            .project_windows
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Only project windows")));
    }
    #[test]
    fn project_switch_invalidates_a_cached_menu_without_changing_documents() {
        let mut app = app();
        app.bind_project_window_identity("logical-source".into());
        app.push_tab(EditorState::from_text("value = 'λ'\n", Language::Python));
        let before = app.tabs[0].state.doc.text();
        app.project_windows.owner = Some(app.project_windows_owner());
        app.project_windows.windows = vec![window("destination", "/other")];
        app.project_windows.listed = true;
        app.file_session = app.file_session.wrapping_add(1);
        app.project_windows_tick(&egui::Context::default());
        assert!(app.project_windows.windows.is_empty());
        assert!(!app.project_windows.listed);
        assert_eq!(app.tabs[0].state.doc.text(), before);
    }
    #[test]
    fn cancelled_workers_remain_owned_until_the_result_channel_drains() {
        let mut app = app();
        let (started, wait_started) = std::sync::mpsc::sync_channel(1);
        let (release, wait_release) = std::sync::mpsc::sync_channel(1);
        let task = TaskManager::global()
            .task("Fixture window discovery")
            .spawn(move |_| {
                let _ = started.send(());
                wait_release
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .map_err(|e| TaskError::Failed(e.to_string()))?;
                Ok(Output::Listed(Vec::new()))
            });
        wait_started
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let id = task.id();
        let owner = app.project_windows_owner();
        app.project_windows.jobs.push(Job {
            owner,
            generation: 0,
            intent: Intent::List,
            task,
        });
        let ctx = egui::Context::default();
        assert!(app.retire_project_windows(&ctx));
        assert!(app.project_window_jobs().iter().any(|job| job.0 == id));
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app.retire_project_windows(&ctx) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(app.project_window_jobs().is_empty());
    }
}
