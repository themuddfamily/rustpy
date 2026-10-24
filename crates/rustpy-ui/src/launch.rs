//! Starting the IDE from the command line (roadmap §8 "Command-line
//! launcher"): `rustpy [--line N] [--column M] [--wait] [path …]` and the
//! headless smoke-test flags. When an IDE already runs for the configuration
//! directory the request is forwarded to it (`rustpy_cli::instance`);
//! otherwise this process runs the IDE and answers later requests: it opens
//! their files, comes to the front and tells `--wait` launchers when the
//! files' tabs are closed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustpy_cli::args::{GlobalOptions, OpenArgs, OpenTarget};
use rustpy_cli::instance::{self, Launch};
use rustpy_cli::ipc::{Server, WaitHandle};
use rustpy_cli::window_admission::{AdmissionLease, WindowAdmission};

use crate::app::IdeApp;

mod global_quit;
pub(crate) mod native_project_windows;
mod render_fallback;
pub(crate) use render_fallback::SoftwareReason;
mod startup;
mod window_owner;
use global_quit::GlobalQuit;
use window_owner::WindowOwnerLifetime;

/// Folders whose presence marks an IDE project root.
const IDE_PROJECT_MARKERS: &[&str] = &[".rustpy", ".idea"];

/// The project to open at startup for the command-line targets: the first
/// folder; for files, the closest folder above the first one that has a
/// `.rustpy` or `.idea` folder, otherwise the file itself (its folder becomes
/// the project). Files inside Git's own folder (`git commit` messages) open
/// without choosing a project for them.
pub fn startup_project(targets: &[OpenTarget]) -> Option<PathBuf> {
    if let Some(dir) = targets.iter().find(|t| t.path.is_dir()) {
        return Some(dir.path.clone());
    }
    let file = targets.iter().find(|t| t.path.is_file())?;
    let path = file
        .path
        .canonicalize()
        .unwrap_or_else(|_| file.path.clone());
    if path.components().any(|c| c.as_os_str() == ".git") {
        return None;
    }
    let mut dir = path.parent();
    while let Some(d) = dir {
        if IDE_PROJECT_MARKERS.iter().any(|m| d.join(m).is_dir()) {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    Some(path)
}

/// The IDE plus the single-instance server: forwarded requests are handled
/// before every frame (also while the window is minimized).
struct IdeShell {
    app: IdeApp,
    server: Option<Server>,
    /// `--wait` launchers and the paths they wait for.
    waits: Vec<(Vec<PathBuf>, WaitHandle)>,
    registered_project: Option<Option<PathBuf>>,
    registry_failure: Option<String>,
    config: PathBuf,
    admission: WindowAdmission,
    owner: Option<WindowOwnerLifetime>,
    quit: GlobalQuit,
    quit_failure: Option<String>,
    startup: startup::StartupState,
    startup_warning: Option<String>,
}

impl IdeShell {
    fn frozen(&self) -> bool {
        self.quit.active()
            || self.app.coordinated_quit_active()
            || self.startup.awaiting_confirmation()
    }
    fn drive_quit(&mut self) {
        if let (Some(server), Some(owner)) = (&self.server, &self.owner) {
            self.quit.drive(&mut self.app, server, owner.token());
        }
        self.app
            .defer_coordinated_close(!self.quit.ready_to_close());
        if let Some(error) = self.quit.error().map(str::to_owned) {
            if self.quit_failure.as_ref() != Some(&error) {
                self.app
                    .lifecycle_error("Cannot quit all windows", error.clone());
                self.quit_failure = Some(error);
            }
        }
    }
    fn request_global_quit(&mut self) {
        if !self.app.take_global_quit_request() {
            return;
        }
        if self.startup.unresolved() {
            self.app.cancel_requested_restart();
            self.app.lifecycle_error("Previous windows are still starting", "The previous-window restore has not confirmed its complete outcome. Close this window locally or wait for confirmation before quitting all windows.".into());
            return;
        }
        if let Some(server) = &self.server {
            if let Err(error) = self.quit.request(&self.config, &server.window_id()) {
                self.app.cancel_requested_restart();
                self.app.lifecycle_error("Cannot quit all windows", error);
            } else {
                self.quit_failure = None;
            }
        }
    }
    fn sync_project_registration(&mut self) {
        if self.quit.active() || self.app.coordinated_quit_active() {
            return;
        }
        let Ok(lease) = self
            .admission
            .acquire_shared(Instant::now() + Duration::from_millis(5))
        else {
            return;
        };
        self.sync_project_registration_with_lease(&lease);
    }
    fn sync_project_registration_with_lease(&mut self, lease: &AdmissionLease) -> bool {
        let Some(server) = &self.server else {
            return false;
        };
        let project = self
            .app
            .actual_current_project_root()
            .map(Path::to_path_buf);
        let result = (|| -> std::io::Result<()> {
            lease
                .validate_identity()
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            if self.owner.is_none() {
                self.owner = Some(WindowOwnerLifetime::accept(
                    &self.config,
                    server,
                    project.as_deref(),
                    lease,
                )?);
            }
            if self.registered_project.as_ref() != Some(&project) {
                self.owner
                    .as_mut()
                    .unwrap()
                    .sync(server, project.as_deref(), lease)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.app.bind_project_window_identity(server.window_id());
                self.registered_project = Some(project);
                self.registry_failure = None;
                if let (Some(server), Some(owner)) = (&self.server, &self.owner) {
                    if let Err(error) = self.startup.confirm(server, owner, lease) {
                        self.startup.report_warning(error);
                    }
                }
                true
            }
            Err(error) => {
                self.registered_project = None;
                let error = format!("Cannot register this window's project: {error}");
                if self.registry_failure.as_ref() != Some(&error) {
                    self.app.registry_error(error.clone());
                }
                self.registry_failure = Some(error);
                false
            }
        }
    }
    fn accept_request(&mut self, request: rustpy_cli::ipc::Incoming) -> bool {
        if request.native_window.is_some() {
            request.reject("Native NEW must target the process host");
            return false;
        }
        if self.frozen() {
            request.reject("Busy: this window is preparing a coordinated Quit or startup");
            return false;
        }
        let Ok(lease) = self
            .admission
            .acquire_shared(Instant::now() + Duration::from_millis(5))
        else {
            request.reject("Busy: another window holds application admission");
            return false;
        };
        if request
            .expected_project
            .as_deref()
            .is_some_and(|expected| self.app.actual_current_project_root() != Some(expected))
        {
            request.reject("This window no longer owns the requested project");
            return false;
        }
        let opened = self.app.open_cli_targets(&request.targets, false);
        if !self.sync_project_registration_with_lease(&lease) {
            request.reject("This window could not publish its accepted project");
            return false;
        }
        if let Some(handle) = request.accept(opened.clone()) {
            self.waits.push((opened, handle));
        }
        true
    }
    fn serve(&mut self, ctx: &egui::Context) {
        self.serve_on(ctx, egui::ViewportId::ROOT);
    }
    fn serve_on(&mut self, ctx: &egui::Context, viewport: egui::ViewportId) {
        // Controls continue outside shared admission, including our own worker's
        // Prepare/Commit while it holds the exclusive configuration-wide lease.
        self.drive_quit();
        self.sync_project_registration();
        let mut raise = false;
        while let Some(request) = self.server.as_ref().and_then(Server::try_recv) {
            raise |= self.accept_request(request);
        }
        if raise {
            ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }
        // Waiting launchers survive Prepare and Abort. Only actual tab closure
        // or actual native exit releases them.
        let mut i = 0;
        while i < self.waits.len() {
            if self.waits[i]
                .0
                .iter()
                .any(|path| self.app.cli_target_is_open(path))
            {
                i += 1;
            } else {
                let (_, handle) = self.waits.remove(i);
                handle.finish();
            }
        }
        if !self.quit.active() && !self.app.coordinated_quit_active() {
            self.startup.drive(&self.config, &mut self.server);
            if let Some(warning) = self.startup.warning().map(str::to_owned) {
                if self.startup_warning.as_ref() != Some(&warning) {
                    self.app
                        .lifecycle_error("Previous windows", warning.clone());
                    self.startup_warning = Some(warning);
                }
            }
        }
        if self.quit.active() || self.startup.unresolved() {
            ctx.request_repaint_after(Duration::from_millis(20));
        }
    }
    /// Actual local native viewport retirement. Never drop an accepted owner
    /// while admission or its durable close is uncertain; the host retries.
    #[cfg(target_os = "macos")]
    fn retire_native_window(&mut self) -> Result<bool, String> {
        if self.quit.active() && !self.quit.ready_to_close() {
            return Ok(false);
        }
        if !self.quit.committed() {
            let Ok(lease) = self
                .admission
                .acquire_shared(Instant::now() + Duration::from_millis(5))
            else {
                return Ok(false);
            };
            if let (Some(server), Some(owner)) = (&self.server, &mut self.owner) {
                owner.retire(server, &lease).map_err(|e| e.to_string())?;
            }
        }
        eframe::App::on_exit(&mut self.app, None);
        for (_, handle) in self.waits.drain(..) {
            handle.finish();
        }
        self.owner = None;
        self.server.take();
        Ok(true)
    }
    fn waiting_ui(&mut self, ui: &mut egui::Ui) {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.quit.active() {
            self.app.paint_quit_backdrop(ui.ctx());
        }
        ui.vertical_centered(|ui| {
            crate::a11y::spinner(ui);
            if self.quit.active() {
                ui.label("Preparing to quit all RustPy windows…");
                if let Some(progress) = self.quit.progress() {
                    if let Some(detail) = &progress.detail {
                        ui.label(detail);
                    }
                }
                if ui.button("Cancel Quit").clicked() {
                    self.quit.cancel();
                }
            } else {
                ui.label("Confirming previous windows…");
                if let Some(warning) = self.startup.warning() {
                    ui.label(warning);
                }
                if self.server.is_some()
                    && self.startup.awaiting_confirmation()
                    && ui.button("Close this window").clicked()
                {
                    self.app.request_startup_window_close();
                }
            }
        });
        if !self.quit.active() && self.server.is_some() && self.startup.awaiting_confirmation() {
            if let Ok(_lease) = self
                .admission
                .acquire_shared(Instant::now() + Duration::from_millis(5))
            {
                self.app.startup_close_ui(ui.ctx());
            }
        }
        ui.ctx().request_repaint_after(Duration::from_millis(20));
    }
}

impl eframe::App for IdeShell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // `RUSTPY_DEBUG_REPAINT=1`: log why each frame was painted (the
        // `request_repaint*` call sites), to find what keeps an idle IDE busy.
        static DEBUG_REPAINT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *DEBUG_REPAINT.get_or_init(|| std::env::var_os("RUSTPY_DEBUG_REPAINT").is_some()) {
            log::info!("repaint causes: {:?}", ctx.repaint_causes());
        }
        self.serve(ctx);
        self.request_global_quit();
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.drive_quit();
        if self.app.coordinated_quit_active() {
            self.app.ui(ui, frame);
        } else if self.quit.active()
            || self.startup.awaiting_confirmation()
            || self.server.is_none()
        {
            self.waiting_ui(ui);
        } else {
            // A frame drawn without the IDE (waiting_ui) drops egui's keyboard
            // focus and replaces the whole accessibility tree, so a screen reader
            // loses its place. The lease also loads the session store: allow a
            // loaded machine more than 5 ms, and restore the focus afterwards.
            let parked = egui::Id::new("rustpy-admission-parked-focus");
            match self
                .admission
                .acquire_shared(Instant::now() + Duration::from_millis(25))
            {
                Ok(lease) => {
                    let ctx = ui.ctx().clone();
                    if let Some(id) = ctx.data_mut(|data| {
                        let id = data.get_temp::<egui::Id>(parked);
                        data.remove::<egui::Id>(parked);
                        id
                    }) {
                        if ctx.memory(|memory| memory.focused()).is_none() {
                            ctx.memory_mut(|memory| memory.request_focus(id));
                        }
                    }
                    self.app.ui(ui, frame);
                    self.sync_project_registration_with_lease(&lease);
                }
                Err(error) => {
                    let ctx = ui.ctx().clone();
                    if let Some(id) = ctx.memory(|memory| memory.focused()) {
                        ctx.data_mut(|data| data.insert_temp(parked, id));
                    }
                    log::debug!("frame without shared window admission: {error}");
                    self.waiting_ui(ui)
                }
            }
        }
        self.request_global_quit();
    }
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.frozen() || self.server.is_none() {
            return;
        }
        if let Ok(_lease) = self
            .admission
            .acquire_shared(Instant::now() + Duration::from_millis(5))
        {
            self.app.save(storage);
        }
    }
    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        if self.quit.committed() {
            // Guarded publication already retired the exact cohort atomically.
            self.app.on_exit(gl);
        } else {
            match self
                .admission
                .acquire_shared(Instant::now() + Duration::from_secs(5))
            {
                Ok(lease) => {
                    self.app.on_exit(gl);
                    if let (Some(server), Some(owner)) = (&self.server, &mut self.owner) {
                        if let Err(error) = owner.retire(server, &lease) {
                            log::error!("window owner retirement is uncertain: {error}");
                        }
                    }
                }
                Err(error) => log::error!("window exit admission is uncertain: {error}"),
            }
        }
        for (_, handle) in self.waits.drain(..) {
            handle.finish();
        }
    }

    fn auto_save_interval(&self) -> std::time::Duration {
        self.app.auto_save_interval()
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        self.app.clear_color(visuals)
    }

    fn persist_egui_memory(&self) -> bool {
        self.app.persist_egui_memory()
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, raw_input);
    }
}

/// Runs the IDE for `args` (forwarding to a running one when possible) and
/// returns the exit status.
pub fn run_ide(args: OpenArgs, global: &GlobalOptions) -> anyhow::Result<i32> {
    // Before the IDE's threads allocate (see `rustpy_core::memory`).
    rustpy_core::memory::tune_allocator();
    for w in &args.warnings {
        eprintln!("rustpy: {w}");
    }
    let config_dir = rustpy_settings::Settings::config_dir();
    let admission = WindowAdmission::new(&config_dir)?;
    let startup_lease = admission.acquire_shared(Instant::now() + Duration::from_secs(15))?;
    let (launch, startup_lease) =
        instance::launch_admitted(&args, global, &config_dir, startup_lease);
    let server = match launch {
        Launch::Done(code) => return Ok(code),
        Launch::Start(server) => match server {
            Some(server) => server,
            None => Server::start_secondary(&config_dir)?,
        },
    };
    // Owned run/terminal process groups must not survive a killed IDE. The
    // watchdog is forked now, while this process is small.
    rustpy_core::orphan_guard::install_exit_signal_handlers();
    rustpy_core::orphan_guard::start();
    let startup_lease = startup_lease.expect("new native window retains startup admission");
    let global = global.clone();
    let mut settings = rustpy_settings::Settings::load();
    // Proxy, trusted certificates and the password safe, before anything connects.
    crate::netsec::configure(&mut settings, &config_dir);
    #[cfg(target_os = "macos")]
    let native_host_enabled = native_project_windows::Host::enabled(&args);
    let render_mode = render_fallback::RenderMode::select(
        args.software_rendering,
        settings.platform.software_rendering,
        |name| std::env::var_os(name),
    );
    let software_reason = render_mode.software_reason().cloned();
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RustPy")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([640.0, 400.0])
            .with_app_id("rustpy"),
        // Window/panel layout lives next to the settings (per RUSTPY_CONFIG_DIR),
        // so headless runs with their own config start from a known layout.
        persistence_path: Some(config_dir.join("ui_state")),
        ..Default::default()
    };
    render_mode.apply(&mut options);
    #[cfg(target_os = "macos")]
    let options = if native_host_enabled {
        eframe::NativeOptions {
            viewport: options
                .viewport
                .with_visible(false)
                .with_decorations(false)
                .with_inner_size([1., 1.])
                .with_min_inner_size([1., 1.]),
            ..options
        }
    } else {
        options
    };
    let project_dir = startup_project(&args.targets);
    // Finder's "open documents" events (Open With RustPy); does nothing outside macOS.
    crate::macos_open::install();
    // Set once the IDE window exists: a later failure must not start a second IDE.
    let app_started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let started = app_started.clone();
    let result = eframe::run_native(
        "RustPy",
        options,
        Box::new(move |cc| {
            started.store(true, std::sync::atomic::Ordering::SeqCst);
            crate::performance_trace::record_creation_context(cc);
            // Bundled fonts at once, then the installed ones (editor and UI families,
            // fallbacks); the i18n plugin adds CJK fonts for the UI language on top.
            crate::fonts::start(
                &cc.egui_ctx,
                &crate::fonts::FontChoice::from_settings(&settings),
            );
            crate::i18n::init(&cc.egui_ctx, &settings.appearance.language);
            // Ctrl+Minus / Ctrl+Equals are folding shortcuts; the IDE zooms
            // through its own Increase/Decrease Font Size actions instead.
            cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
            crate::netsec::install_certificate_prompts(&cc.egui_ctx);
            #[cfg(target_os = "macos")]
            let mut initial_endpoint = Some(server);
            #[cfg(target_os = "macos")]
            let mut native_host = if native_host_enabled {
                Some(
                    native_project_windows::Host::new(
                        &cc.egui_ctx,
                        initial_endpoint.take().unwrap(),
                        config_dir.clone(),
                        global.clone(),
                    )
                    .map_err(std::io::Error::other)?,
                )
            } else {
                None
            };
            #[cfg(target_os = "macos")]
            let server = match initial_endpoint {
                Some(server) => server,
                None => Server::start_secondary(&config_dir)?,
            };
            let startup::StartupWindow {
                mut app,
                owner,
                restore,
            } = startup::initialize(
                settings,
                &args,
                project_dir.clone(),
                &config_dir,
                &server,
                &startup_lease,
                &global,
            )
            .map_err(std::io::Error::other)?;
            configure_startup_app(&mut app, &args);
            if let Some(reason) = software_reason {
                app.note_software_rendering(reason);
            }
            let ctx = cc.egui_ctx.clone();
            server.set_waker(move || ctx.request_repaint());
            let mut shell = IdeShell {
                app,
                server: Some(server),
                waits: Vec::new(),
                registered_project: None,
                registry_failure: None,
                config: config_dir.clone(),
                admission,
                owner: Some(owner),
                quit: GlobalQuit::with_configuration(&config_dir),
                quit_failure: None,
                startup: restore,
                startup_warning: None,
            };
            if shell.sync_project_registration_with_lease(&startup_lease) {
                // Owners left by killed/crashed windows would otherwise block
                // every later Quit; probing endpoints stays off the UI thread.
                let config = config_dir.clone();
                std::thread::spawn(move || {
                    let _ = global_quit::prune_dead_owners(
                        &config,
                        Instant::now() + Duration::from_secs(3),
                    );
                });
            }
            drop(startup_lease);
            #[cfg(target_os = "macos")]
            if let Some(mut host) = native_host.take() {
                host.insert_initial(shell, &cc.egui_ctx);
                return Ok(Box::new(host) as Box<dyn eframe::App>);
            }
            Ok(Box::new(shell))
        }),
    )
    .map_err(|e| render_fallback::StartFailure::classify(&e));
    render_fallback::run_with_fallback(
        &render_mode,
        || app_started.load(std::sync::atomic::Ordering::SeqCst),
        || result,
        render_fallback::restart_with_software_rendering,
    )
}

/// Explicit CLI/startup actions belong to each independent app, including
/// a same-process native project window. No UI/process owner is shared.
fn configure_startup_app(app: &mut IdeApp, args: &OpenArgs) {
    app.manage_shell_lifecycle();
    app.screenshot_path = args.screenshot.clone();
    app.run_on_start = args.run;
    app.terminal_on_start = args.terminal.clone();
    app.tool_on_start = args.tool.clone();
    app.goto_on_start = args.goto;
    app.actions_on_start = args.actions.clone();
    if let Some(path) = &args.input_script {
        match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|src| crate::input_script::parse(&src))
        {
            Ok(steps) => app.input_player = Some(crate::input_script::InputPlayer::new(steps)),
            Err(e) => log::error!("input script {}: {e}", path.display()),
        }
    }
}

/// Whether `path` is a project root for the IDE.
pub fn is_ide_project(path: &Path) -> bool {
    IDE_PROJECT_MARKERS.iter().any(|m| path.join(m).is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered_shell(config: &Path, project: &Path) -> IdeShell {
        let mut settings = rustpy_settings::Settings {
            reopen_last_project: false,
            ..Default::default()
        };
        settings.set_open_project_in(rustpy_settings::OpenProjectIn::Ask);
        let mut shell = IdeShell {
            app: IdeApp::new(settings, Some(project.to_path_buf())),
            server: Some(Server::start_secondary(config).unwrap()),
            waits: Vec::new(),
            registered_project: None,
            registry_failure: None,
            config: config.to_path_buf(),
            admission: WindowAdmission::new(config).unwrap(),
            owner: None,
            quit: GlobalQuit::with_configuration(config),
            quit_failure: None,
            startup: startup::StartupState::empty(&GlobalOptions::default()),
            startup_warning: None,
        };
        shell.app.manage_shell_lifecycle();
        // Setup owns the startup lease; a single 5 ms GUI tick may defer
        // registration before this fixture's publication scenario begins.
        let startup_lease = shell
            .admission
            .acquire_shared(Instant::now() + Duration::from_secs(15))
            .expect("fixture startup admission must complete");
        assert!(
            shell.sync_project_registration_with_lease(&startup_lease),
            "fixture startup registration must complete: {:?}",
            shell.registry_failure
        );
        assert!(shell.owner.is_some());
        assert_eq!(
            shell.registered_project,
            Some(Some(project.canonicalize().unwrap()))
        );
        drop(startup_lease);
        shell
    }

    /// Frames of the real shell until `done` holds (drive_quit is what
    /// `ui()` runs first in every frame).
    #[cfg(unix)]
    fn drive_until(
        shell: &mut IdeShell,
        what: &str,
        frame: Duration,
        mut done: impl FnMut(&mut IdeShell) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done(shell) {
            assert!(
                Instant::now() < deadline,
                "{what}: quit error {:?}, progress {:?}",
                shell.quit.error(),
                shell.quit.progress()
            );
            shell.drive_quit();
            std::thread::sleep(frame);
        }
    }

    #[cfg(unix)]
    const FRAME: Duration = Duration::from_millis(5);
    #[cfg(unix)]
    #[test]
    fn file_exit_with_running_process_prompts_and_cancel_or_terminate_work() {
        use crate::actions::Action;
        use crate::app::window_lifecycle::{QuitAnswer, QuitProgress};
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join(".rustpy")).unwrap();
        let mut shell = registered_shell(&config, &project);
        shell.app.testing_dismiss_dialog();
        let session = rustpy_run::RunSession::spawn_command(
            1,
            "long",
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 300 & wait".into()],
            &project,
            &Default::default(),
        );
        shell.app.testing_run_sessions().push(session);
        let pending = |shell: &mut IdeShell| {
            shell.app.coordinated_quit_active() && {
                let tx = shell.app.testing_coordinated_quit_transaction().unwrap();
                matches!(
                    shell.app.poll_coordinated_quit(&tx),
                    QuitProgress::PendingProcesses { ref processes }
                        if processes == &["Run 'long'".to_string()]
                )
            }
        };
        // File > Exit through the real action dispatch and shell frame.
        shell.app.testing_handle_action(Action::Exit);
        shell.request_global_quit();
        assert!(shell.quit.active(), "{:?}", shell.quit_failure);
        // Busy UI: frames slower than a second must not expire the Prepare.
        drive_until(
            &mut shell,
            "running-process prompt",
            Duration::from_millis(1200),
            pending,
        );
        // Cancel: the Quit is aborted and the process keeps running.
        let tx = shell.app.testing_coordinated_quit_transaction().unwrap();
        shell.app.answer_coordinated_quit(&tx, QuitAnswer::Cancel);
        drive_until(&mut shell, "cancelled quit", FRAME, |shell| {
            !shell.quit.active() && !shell.app.coordinated_quit_active()
        });
        assert!(!shell.quit.committed());
        assert!(shell.app.testing_run_sessions()[0].is_running());
        // Exit again, then Terminate: the Quit commits and stops the process.
        shell.app.testing_handle_action(Action::Exit);
        shell.request_global_quit();
        drive_until(&mut shell, "second running-process prompt", FRAME, pending);
        let tx = shell.app.testing_coordinated_quit_transaction().unwrap();
        shell.app.answer_coordinated_quit(&tx, QuitAnswer::Continue);
        drive_until(&mut shell, "committed quit", FRAME, |shell| {
            shell.quit.ready_to_close()
        });
        let session = &mut shell.app.testing_run_sessions()[0];
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.is_running() && Instant::now() < deadline {
            session.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!session.is_running(), "Terminate must stop the run");
    }

    #[test]
    fn registry_tracks_accepted_project_changes_and_ignores_pending_questions() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(first.join(".rustpy")).unwrap();
        std::fs::create_dir_all(second.join(".rustpy")).unwrap();
        let mut shell = registered_shell(&config, &first);
        let original = rustpy_cli::window_registry::find_project(&config, &first)
            .unwrap()
            .unwrap();
        shell
            .app
            .open_cli_targets(&[OpenTarget::new(second.clone())], false);
        shell.sync_project_registration();
        assert_eq!(
            shell.app.actual_current_project_root(),
            Some(first.canonicalize().unwrap().as_path())
        );
        assert_eq!(
            rustpy_cli::window_registry::find_project(&config, &first)
                .unwrap()
                .unwrap()
                .id,
            original.id
        );
        assert!(rustpy_cli::window_registry::find_project(&config, &second)
            .unwrap()
            .is_none());
        shell.app.open_project(&second);
        shell.sync_project_registration();
        assert!(rustpy_cli::window_registry::find_project(&config, &first)
            .unwrap()
            .is_none());
        assert_eq!(
            rustpy_cli::window_registry::find_project(&config, &second)
                .unwrap()
                .unwrap()
                .id,
            original.id
        );
    }

    #[test]
    fn registration_publication_failure_retains_truth_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(first.join(".rustpy")).unwrap();
        std::fs::create_dir_all(second.join(".rustpy")).unwrap();
        let mut shell = registered_shell(&config, &first);
        let original = rustpy_cli::window_registry::find_project(&config, &first)
            .unwrap()
            .unwrap();
        let record = config
            .join("windows")
            .join(&original.id)
            .join("window.json");
        std::fs::remove_file(&record).unwrap();
        std::fs::create_dir(&record).unwrap();
        shell.app.open_project(&second);
        shell.sync_project_registration();
        assert!(
            shell.registry_failure.is_some(),
            "publication errors remain visible and pending"
        );
        assert_eq!(
            shell.app.actual_current_project_root(),
            Some(second.canonicalize().unwrap().as_path())
        );
        assert!(matches!(rustpy_cli::ipc::forward_window(&original, &[], false, None), Err(rustpy_cli::ipc::ForwardError::Rejected(_))), "endpoint proof must retain the accepted new project even when metadata publication fails");
        std::fs::remove_dir(&record).unwrap();
        shell.sync_project_registration();
        assert!(shell.registry_failure.is_none());
        assert_eq!(
            shell.registered_project,
            Some(Some(second.canonicalize().unwrap()))
        );
        assert_eq!(
            rustpy_cli::window_registry::find_project(&config, &second)
                .unwrap()
                .unwrap()
                .id,
            original.id
        );
    }

    #[test]
    fn returning_to_previous_project_after_failed_publication_retries_registration() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(first.join(".rustpy")).unwrap();
        std::fs::create_dir_all(second.join(".rustpy")).unwrap();
        let mut shell = registered_shell(&config, &first);
        let original = rustpy_cli::window_registry::find_project(&config, &first)
            .unwrap()
            .unwrap();
        let record = config
            .join("windows")
            .join(&original.id)
            .join("window.json");
        std::fs::remove_file(&record).unwrap();
        std::fs::create_dir(&record).unwrap();
        shell.app.open_project(&second);
        shell.sync_project_registration();
        assert!(shell.registry_failure.is_some());
        std::fs::remove_dir(&record).unwrap();
        shell.app.open_project(&first);
        shell.sync_project_registration();
        assert!(
            shell.registry_failure.is_none(),
            "returning to the earlier project must retry the failed publication"
        );
        let restored = rustpy_cli::window_registry::find_project(&config, &first)
            .unwrap()
            .expect("the restored project must have a published, authenticated endpoint");
        assert_eq!(restored.id, original.id);
        assert!(rustpy_cli::window_registry::find_project(&config, &second)
            .unwrap()
            .is_none());
    }

    #[test]
    fn routed_request_acknowledges_and_releases_wait_on_project_close() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(first.join(".rustpy")).unwrap();
        std::fs::create_dir_all(second.join(".rustpy")).unwrap();
        let file = first.join("wait.py");
        std::fs::write(&file, "answer = 42\n").unwrap();
        let mut shell = registered_shell(&config, &first);
        let destination = rustpy_cli::window_registry::find_project(&config, &file)
            .unwrap()
            .unwrap();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let forward = std::thread::spawn(move || {
            let forwarded =
                rustpy_cli::ipc::forward_window(&destination, &[OpenTarget::new(file)], true, None)
                    .unwrap();
            accepted_tx.send(forwarded.opened.clone()).unwrap();
            forwarded.wait_closed()
        });
        let request = shell
            .server
            .as_ref()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(shell.accept_request(request));
        assert_eq!(
            accepted_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            vec![first.join("wait.py").canonicalize().unwrap()]
        );
        assert_eq!(shell.waits.len(), 1);
        shell.app.open_project(&second);
        shell.serve(&egui::Context::default());
        assert!(shell.waits.is_empty());
        forward.join().unwrap().unwrap();
    }

    #[test]
    fn queued_project_request_is_refused_after_an_accepted_switch() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(first.join(".rustpy")).unwrap();
        std::fs::create_dir_all(second.join(".rustpy")).unwrap();
        let file = first.join("queued.py");
        std::fs::write(&file, "answer = 42\n").unwrap();
        let file = file.canonicalize().unwrap();
        let server = Server::start_secondary(&dir.path().join("config")).unwrap();
        server.set_project(Some(&first)).unwrap();
        let destination =
            rustpy_cli::window_registry::find_project(&dir.path().join("config"), &file)
                .unwrap()
                .unwrap();
        let forward = std::thread::spawn(move || {
            rustpy_cli::ipc::forward_window(&destination, &[OpenTarget::new(file)], true, None)
        });
        let request = server
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let settings = rustpy_settings::Settings {
            reopen_last_project: false,
            ..Default::default()
        };
        let mut shell = IdeShell {
            app: IdeApp::new(settings, Some(first.clone())),
            server: Some(server),
            waits: Vec::new(),
            registered_project: None,
            registry_failure: None,
            config: dir.path().join("config"),
            admission: WindowAdmission::new(&dir.path().join("config")).unwrap(),
            owner: None,
            quit: GlobalQuit::with_configuration(&dir.path().join("config")),
            quit_failure: None,
            startup: startup::StartupState::empty(&GlobalOptions::default()),
            startup_warning: None,
        };
        // The network thread accepted this request before the user switched.
        shell.app.open_project(&second);
        assert!(shell
            .app
            .cli_target_is_open(&second.canonicalize().unwrap()));
        shell.accept_request(request);
        let opened = shell
            .app
            .cli_target_is_open(&first.join("queued.py").canonicalize().unwrap());
        let result = forward.join().unwrap();
        assert!(
            !opened,
            "a queued request must not open files in the replacement project"
        );
        assert!(
            result.is_err(),
            "a stale destination must be refused rather than ACKed"
        );
        assert!(
            shell.waits.is_empty(),
            "refused requests must not install wait handles"
        );
    }

    #[test]
    fn startup_project_for_targets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("proj/.rustpy")).unwrap();
        std::fs::create_dir_all(root.join("proj/src/pkg")).unwrap();
        std::fs::create_dir_all(root.join("loose")).unwrap();
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        for f in [
            "proj/src/pkg/m.py",
            "loose/a.py",
            "repo/.git/COMMIT_EDITMSG",
        ] {
            std::fs::write(root.join(f), "").unwrap();
        }
        assert_eq!(startup_project(&[]), None);
        assert_eq!(
            startup_project(&[
                OpenTarget::new(root.join("loose/a.py")),
                OpenTarget::new(root.join("proj"))
            ]),
            Some(root.join("proj")),
            "a folder wins"
        );
        assert_eq!(
            startup_project(&[OpenTarget::new(root.join("proj/src/pkg/m.py"))]),
            Some(root.join("proj")),
            "the file's IDE project"
        );
        if !is_ide_project(&root) && root.ancestors().all(|a| !is_ide_project(a)) {
            assert_eq!(
                startup_project(&[OpenTarget::new(root.join("loose/a.py"))]),
                Some(root.join("loose/a.py")),
                "its folder becomes the project"
            );
        }
        assert_eq!(
            startup_project(&[OpenTarget::new(root.join("repo/.git/COMMIT_EDITMSG"))]),
            None,
            "no project for Git's files"
        );
        assert_eq!(
            startup_project(&[OpenTarget::new(root.join("missing.py"))]),
            None
        );
    }
}
