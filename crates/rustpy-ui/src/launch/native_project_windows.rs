//! macOS process host: each real project viewport retains an independent
//! IdeShell, authenticated endpoint, workspace token, document graph and process
//! owners. The hidden root is transport/render infrastructure, not a participant.
#[cfg(target_os = "macos")]
use rustpy_cli::{
    args::{OpenArgs, OpenTarget},
    ipc::Incoming,
};
use std::path::Path;
#[cfg(target_os = "macos")]
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
};
#[cfg(target_os = "macos")]
mod platform;
#[cfg(target_os = "macos")]
const MAX_WINDOWS: usize = 64;
#[cfg(target_os = "macos")]
const QUEUE: usize = 64;
#[cfg(target_os = "macos")]
#[derive(Debug)]
// A bounded mailbox of 64 entries; boxing `OpenArgs` would buy nothing.
#[allow(clippy::large_enum_variant)]
enum Command {
    Open {
        args: OpenArgs,
        reply: Option<Incoming>,
    },
    Close(egui::ViewportId),
    Merge(egui::ViewportId),
}
#[cfg(target_os = "macos")]
struct Mailbox {
    config: PathBuf,
    sender: SyncSender<Command>,
    ctx: egui::Context,
}
#[cfg(target_os = "macos")]
static MAILBOX: Mutex<Option<Mailbox>> = Mutex::new(None);
#[derive(Clone)]
pub(crate) struct Binding {
    #[cfg(target_os = "macos")]
    viewport: egui::ViewportId,
    #[cfg(target_os = "macos")]
    sender: SyncSender<Command>,
    #[cfg(target_os = "macos")]
    closing: Arc<AtomicBool>,
}
#[cfg(target_os = "macos")]
impl Binding {
    pub(crate) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }
    pub(crate) fn close(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if !self.closing.swap(true, Ordering::AcqRel)
            && self.sender.try_send(Command::Close(self.viewport)).is_err()
        {
            self.closing.store(false, Ordering::Release);
            ctx.request_repaint();
        }
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
    pub(crate) fn merge(&self) -> Result<(), String> {
        self.sender
            .try_send(Command::Merge(self.viewport))
            .map_err(|_| "The native project host is busy".to_string())
    }
}
#[cfg(target_os = "macos")]
pub(crate) fn available() -> bool {
    cfg!(target_os = "macos") && MAILBOX.lock().is_ok_and(|mailbox| mailbox.is_some())
}
/// Return None only when this process is not hosting native project windows.
/// Queue errors never fall through to spawning an unrelated process.
#[cfg(target_os = "macos")]
pub(crate) fn open_project(root: &Path) -> Option<Result<(), String>> {
    let mailbox = match MAILBOX.lock() {
        Ok(mailbox) => mailbox,
        Err(_) => return Some(Err("The native project host is unavailable".into())),
    };
    let mailbox = mailbox.as_ref()?;
    if dunce::canonicalize(&rustpy_settings::Settings::config_dir())
        .ok()
        .zip(dunce::canonicalize(&mailbox.config).ok())
        .is_none_or(|(current, expected)| current != expected)
    {
        return Some(Err("Native host configuration changed".into()));
    }
    let args = OpenArgs {
        new_window: true,
        targets: vec![OpenTarget::new(root)],
        ..Default::default()
    };
    let result = mailbox
        .sender
        .try_send(Command::Open { args, reply: None })
        .map_err(|_| "The native project host is busy".to_string());
    mailbox.ctx.request_repaint_of(egui::ViewportId::ROOT);
    Some(result)
}
#[cfg(target_os = "macos")]
pub(crate) fn merge(binding: Option<&Binding>) -> Result<(), String> {
    binding
        .ok_or("This window does not belong to a native project host")?
        .merge()
}
#[cfg(target_os = "macos")]
struct Installation;
#[cfg(target_os = "macos")]
impl Drop for Installation {
    fn drop(&mut self) {
        if let Ok(mut mailbox) = MAILBOX.lock() {
            *mailbox = None;
        }
    }
}
#[cfg(target_os = "macos")]
pub(super) struct Host {
    _installed: Installation,
    config: PathBuf,
    global: rustpy_cli::args::GlobalOptions,
    router: rustpy_cli::ipc::Server,
    sender: SyncSender<Command>,
    receiver: Receiver<Command>,
    windows: std::collections::BTreeMap<egui::ViewportId, super::IdeShell>,
    closing: std::collections::BTreeSet<egui::ViewportId>,
    configured: std::collections::BTreeSet<egui::ViewportId>,
    restart: Vec<super::IdeShell>,
    focused: Option<egui::ViewportId>,
    merge: Option<(egui::ViewportId, u8)>,
}
#[cfg(target_os = "macos")]
impl Host {
    pub(super) fn enabled(args: &OpenArgs) -> bool {
        !args.is_smoke_test()
    }
    /// Construct before startup::initialize so additional CLI folders and
    /// restored children queue into the same process from their first frame.
    pub(super) fn new(
        ctx: &egui::Context,
        router: rustpy_cli::ipc::Server,
        config: PathBuf,
        global: rustpy_cli::args::GlobalOptions,
    ) -> Result<Self, String> {
        router
            .become_native_project_router()
            .map_err(|e| e.to_string())?;
        let (sender, receiver) = mpsc::sync_channel(QUEUE);
        let mut mailbox = MAILBOX
            .lock()
            .map_err(|_| "Native host mailbox is unavailable")?;
        if mailbox.is_some() {
            return Err("This process already owns a native project host".into());
        }
        *mailbox = Some(Mailbox {
            config: config.clone(),
            sender: sender.clone(),
            ctx: ctx.clone(),
        });
        let repaint = ctx.clone();
        router.set_waker(move || repaint.request_repaint_of(egui::ViewportId::ROOT));
        eframe::native_window::set_root_ui_when_hidden(ctx, true);
        Ok(Self {
            _installed: Installation,
            config,
            global,
            router,
            sender,
            receiver,
            windows: Default::default(),
            closing: Default::default(),
            configured: Default::default(),
            restart: Vec::new(),
            focused: None,
            merge: None,
        })
    }
    pub(super) fn insert_initial(&mut self, mut shell: super::IdeShell, ctx: &egui::Context) {
        let id =
            egui::ViewportId::from_hash_of(("project", shell.server.as_ref().unwrap().window_id()));
        shell
            .app
            .bind_project_window_identity(shell.server.as_ref().unwrap().window_id());
        shell.app.bind_native_project_window(Binding {
            viewport: id,
            sender: self.sender.clone(),
            closing: Arc::new(AtomicBool::new(false)),
        });
        let repaint = ctx.clone();
        shell.server.as_ref().unwrap().set_waker(move || {
            repaint.request_repaint_of(id);
            repaint.request_repaint_of(egui::ViewportId::ROOT);
        });
        self.focused = Some(id);
        self.windows.insert(id, shell);
    }
    fn admitting(&self, args: &OpenArgs) -> bool {
        !self.windows.values().any(|shell| {
            shell.quit.active()
                || shell.app.coordinated_quit_active()
                || (args.restore_child.is_none() && shell.startup.awaiting_confirmation())
        })
    }
    fn create(&mut self, args: OpenArgs, reply: Option<Incoming>, ctx: &egui::Context) {
        if self.windows.len() >= MAX_WINDOWS || !self.admitting(&args) {
            if let Some(reply) = reply {
                reply.reject("Native project host is at capacity or preparing Quit/startup");
            }
            return;
        }
        let result = (|| -> Result<super::IdeShell, String> {
            let admission = rustpy_cli::window_admission::WindowAdmission::new(&self.config)
                .map_err(|e| e.to_string())?;
            let lease = admission
                .acquire_shared(std::time::Instant::now() + std::time::Duration::from_millis(5))
                .map_err(|e| e.to_string())?;
            let server = rustpy_cli::ipc::Server::start_secondary(&self.config)
                .map_err(|e| e.to_string())?;
            let super::startup::StartupWindow {
                mut app,
                owner,
                restore,
            } = super::startup::initialize(
                rustpy_settings::Settings::load(),
                &args,
                super::startup_project(&args.targets),
                &self.config,
                &server,
                &lease,
                &self.global,
            )?;
            super::configure_startup_app(&mut app, &args);
            let mut shell = super::IdeShell {
                app,
                server: Some(server),
                waits: Vec::new(),
                registered_project: None,
                registry_failure: None,
                config: self.config.clone(),
                admission,
                owner: Some(owner),
                quit: super::GlobalQuit::with_configuration(&self.config),
                quit_failure: None,
                startup: restore,
                startup_warning: None,
            };
            if !shell.sync_project_registration_with_lease(&lease) {
                // Retain accepted owner even when publication failed; never
                // synthesize a second owner or acknowledge an unregistered NEW.
                shell
                    .app
                    .registry_error("Native window publication will be retried".into());
            }
            drop(lease);
            Ok(shell)
        })();
        match result {
            Ok(mut shell) => {
                if let Some(reply) = reply {
                    let opened = args
                        .targets
                        .iter()
                        .filter(|target| shell.app.cli_target_is_open(&target.path))
                        .map(|target| target.path.clone())
                        .collect::<Vec<_>>();
                    if shell.registered_project.is_some() {
                        if let Some(wait) = reply.accept(opened.clone()) {
                            shell.waits.push((opened, wait));
                        }
                    } else {
                        reply.reject(
                            "Native project window accepted but endpoint publication is pending",
                        );
                    }
                }
                self.insert_initial(shell, ctx);
                if let Some(id) = self.focused {
                    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
                }
            }
            Err(error) => {
                if let Some(reply) = reply {
                    reply.reject(error);
                } else if let Some(shell) = self.focused.and_then(|id| self.windows.get_mut(&id)) {
                    shell.app.native_project_report(error);
                }
            }
        }
    }
    fn route(&mut self, incoming: Incoming, ctx: &egui::Context) {
        if let Some(args) = incoming.native_window.clone() {
            self.create(args, Some(incoming), ctx);
            return;
        }
        let selected = self
            .windows
            .iter()
            .filter_map(|(id, shell)| {
                let root = shell.app.actual_current_project_root()?;
                incoming
                    .targets
                    .iter()
                    .any(|target| target.path.starts_with(root))
                    .then_some((*id, root.components().count()))
            })
            .max_by_key(|(id, depth)| (*depth, Some(*id) == self.focused))
            .map(|(id, _)| id)
            .or(self.focused)
            .or_else(|| self.windows.keys().next().copied());
        if let Some(id) = selected {
            if self.windows[&id].app.native_project_is_closing() {
                incoming.reject("Busy: this native project window is retiring");
                return;
            }
            if self.windows.get_mut(&id).unwrap().accept_request(incoming) {
                ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
            }
        } else {
            self.create(
                OpenArgs {
                    new_window: true,
                    targets: incoming.targets.clone(),
                    wait: incoming.wait,
                    ..Default::default()
                },
                Some(incoming),
                ctx,
            );
        }
    }
    fn drain(&mut self, ctx: &egui::Context) {
        // Logical child endpoints keep servicing guarded Quit controls even
        // when the hidden root or an inactive native tab is occluded.
        for _ in 0..QUEUE {
            let Some(control) = self.router.try_recv_control() else {
                break;
            };
            let _ = control.respond(rustpy_cli::ipc::ControlStatus::Rejected {
                message: "The process router is not a logical project window".into(),
            });
        }
        // Admit at most one new external window per frame. Each logical
        // constructor retains the existing bounded startup/admission checks.
        if let Some(incoming) = self.router.try_recv() {
            self.route(incoming, ctx);
        }
        for _ in 0..QUEUE {
            let Ok(command) = self.receiver.try_recv() else {
                break;
            };
            match command {
                Command::Open { args, reply } => {
                    self.create(args, reply, ctx);
                    break;
                }
                Command::Close(id) => {
                    if self.windows.contains_key(&id) {
                        self.closing.insert(id);
                    }
                }
                Command::Merge(id) => {
                    if self.windows.contains_key(&id) {
                        self.merge = Some((id, 0));
                    }
                }
            }
        }
        let mut retired = Vec::new();
        for id in &self.closing {
            if let Some(shell) = self.windows.get_mut(id) {
                match shell.retire_native_window() {
                    Ok(true) => retired.push(*id),
                    Ok(false) => {}
                    Err(error) => {
                        shell.app.native_project_report(error);
                    }
                }
            }
        }
        for id in retired {
            self.closing.remove(&id);
            self.configured.remove(&id);
            let shell = self.windows.remove(&id).unwrap();
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Close);
            if shell.app.native_project_restart_pending() {
                self.restart.push(shell);
            }
            if self.focused == Some(id) {
                self.focused = self.windows.keys().next().copied();
            }
        }
    }
}
#[cfg(target_os = "macos")]
impl eframe::App for Host {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.windows.is_empty() && ctx.input(|input| input.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if let Some(shell) = self.focused.and_then(|id| self.windows.get_mut(&id)) {
                shell.app.request_native_host_quit();
            }
        }
        for (id, shell) in &mut self.windows {
            if shell.app.native_project_is_closing() {
                // Close was approved for the captured documents/process owners.
                // Durable retirement may retry; never accept a new document in
                // that interval. Guarded Quit controls and wait handles remain owned.
                shell.drive_quit();
                for _ in 0..QUEUE {
                    let Some(request) = shell
                        .server
                        .as_ref()
                        .and_then(rustpy_cli::ipc::Server::try_recv)
                    else {
                        break;
                    };
                    request.reject("Busy: this native project window is retiring");
                }
            } else {
                shell.serve_on(ctx, *id);
            }
            shell.request_global_quit();
            if shell.app.coordinated_quit_active() {
                ctx.request_repaint_of(*id);
            }
        }
        self.drain(ctx);
        if !self
            .windows
            .values()
            .any(|shell| shell.frozen() || shell.app.native_project_is_closing())
        {
            if let Some(shell) = self.focused.and_then(|id| self.windows.get_mut(&id)) {
                shell
                    .app
                    .native_project_open_paths(crate::macos_open::take_opened_paths(ctx));
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        let mut focused = None;
        for (id, shell) in &mut self.windows {
            let title = shell
                .app
                .actual_current_project_root()
                .and_then(Path::file_name)
                .map(|name| format!("{} — RustPy", name.to_string_lossy()))
                .unwrap_or_else(|| "RustPy — Welcome".into());
            ctx.show_viewport_immediate(
                *id,
                egui::ViewportBuilder::default()
                    .with_title(title)
                    .with_visible(false)
                    .with_inner_size([1400., 900.])
                    .with_min_inner_size([640., 400.])
                    .with_app_id("rustpy"),
                |ui, class| {
                    if class == egui::ViewportClass::EmbeddedWindow {
                        ui.label(
                            "Native project windows require the desktop multi-viewport backend",
                        );
                        return;
                    }
                    // Immediate viewports bypass eframe's root App input
                    // hook. Route this viewport's actual events to its own
                    // app before any editor/shortcut consumer sees them.
                    if !shell.frozen() && !shell.app.native_project_is_closing() {
                        let mut input = ui.ctx().input(|state| state.raw.clone());
                        shell.raw_input_hook(ui.ctx(), &mut input);
                        ui.ctx().input_mut(|state| {
                            state.events = input.events.clone();
                            state.raw = input;
                        });
                        shell.app.native_project_viewport_begin(ui.ctx());
                    }
                    if ui.input(|input| input.viewport().focused == Some(true)) {
                        focused = Some(*id);
                    }
                    if shell.app.native_project_is_closing() && !shell.app.coordinated_quit_active()
                    {
                        shell.app.native_project_retirement_ui(ui);
                    } else {
                        shell.ui(ui, frame);
                    }
                },
            );
            if !self.configured.contains(id) {
                match platform::prepare(&ctx, *id) {
                    Ok(()) => {
                        self.configured.insert(*id);
                        ctx.send_viewport_cmd_to(*id, egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd_to(*id, egui::ViewportCommand::Focus);
                    }
                    Err(error) => {
                        shell.app.native_project_report(error);
                        ctx.request_repaint();
                    }
                }
            }
        }
        if focused.is_some() {
            self.focused = focused;
        }
        if let Some((source, attempt)) = self.merge.take() {
            let projects = self
                .windows
                .iter()
                .filter(|(_, shell)| shell.app.actual_current_project_root().is_some())
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            match platform::merge(&ctx, &projects) {
                Ok(()) => {
                    if let Some(shell) = self.windows.get_mut(&source) {
                        shell.app.native_project_report(
                            "Merged all project windows into native macOS tabs".into(),
                        );
                    }
                }
                Err(_)
                    if attempt < 30
                        && projects
                            .iter()
                            .any(|id| eframe::native_window::get(&ctx, *id).is_none()) =>
                {
                    self.merge = Some((source, attempt + 1));
                    ctx.request_repaint();
                }
                Err(error) => {
                    if let Some(shell) = self.windows.get_mut(&source) {
                        shell.app.native_project_report(format!(
                            "Cannot merge project windows: {error}"
                        ));
                    }
                }
            }
        }
        if !self.windows.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.windows.is_empty() {
            for shell in &mut self.restart {
                shell.app.native_project_finish_restart();
            }
            self.restart.clear();
            crate::netsec::shutdown_prompts();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        if let Some(shell) = self.windows.get_mut(&input.viewport_id) {
            if !shell.app.native_project_is_closing() {
                shell.raw_input_hook(ctx, input);
            }
        }
    }
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        for shell in self.windows.values_mut() {
            shell.save(storage);
        }
    }
    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        // Forced OS/process shutdown retains the existing shell's last-resort
        // save/owner handling. Normal closes retire each exact app beforehand.
        for shell in self.windows.values_mut() {
            shell.on_exit(gl);
        }
        crate::netsec::shutdown_prompts();
    }
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

#[cfg(all(test, target_os = "macos"))]
mod fixtures {
    use super::*;
    #[test]
    fn close_is_scoped_to_one_viewport_and_retains_queue_ownership() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let id = egui::ViewportId::from_hash_of("one-project");
        let binding = Binding {
            viewport: id,
            sender,
            closing: Arc::new(AtomicBool::new(false)),
        };
        let ctx = egui::Context::default();
        assert!(!binding.is_closing());
        binding.close(&ctx);
        assert!(
            binding.is_closing(),
            "Admission freezes before the host drains its queue"
        );
        binding.close(&ctx);
        assert!(matches!(receiver.try_recv().unwrap(),Command::Close(actual) if actual==id));
        assert!(
            receiver.try_recv().is_err(),
            "One retirement request owns the exact viewport"
        );
    }
    #[test]
    fn full_queue_retries_close_instead_of_dropping_the_live_app() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let id = egui::ViewportId::from_hash_of("project");
        sender.send(Command::Merge(id)).unwrap();
        let binding = Binding {
            viewport: id,
            sender,
            closing: Arc::new(AtomicBool::new(false)),
        };
        let ctx = egui::Context::default();
        binding.close(&ctx);
        assert!(!binding.closing.load(Ordering::Acquire));
        receiver.try_recv().unwrap();
        binding.close(&ctx);
        assert!(matches!(receiver.try_recv().unwrap(),Command::Close(actual) if actual==id));
    }
}

#[cfg(not(target_os = "macos"))]
impl Binding {
    pub(crate) fn is_closing(&self) -> bool {
        false
    }
    pub(crate) fn close(&self, _ctx: &egui::Context) {}
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn available() -> bool {
    false
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn open_project(_root: &Path) -> Option<Result<(), String>> {
    None
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn merge(_binding: Option<&Binding>) -> Result<(), String> {
    Err("Native project window tab groups require macOS".into())
}
