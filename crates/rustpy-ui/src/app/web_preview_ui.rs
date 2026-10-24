//! Browser profiles, editor browser bar and genuine embedded preview ownership.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::{
    web_browser_workspace::{self as browser, Launch},
    web_preview_host::{Event, Host},
    web_preview_server::Server,
};
use rustpy_settings::web_browsers::{DefaultBrowser, Kind, Profile};
#[cfg(all(test, target_os = "linux"))]
#[path = "web_preview_browser_acceptance.rs"]
mod browser_acceptance;
#[derive(Default)]
struct UrlForm {
    value: String,
    profile: String,
    embedded: bool,
}
pub(super) struct Workspace {
    server: Option<Server>,
    retiring: Vec<Server>,
    host: Host,
    requested: Option<String>,
    rect: Option<(egui::ViewportId, egui::Rect)>,
    frame_popup_open: bool,
    parent: Option<egui::ViewportId>,
    owner: Option<PathBuf>,
    file: Option<PathBuf>,
    address: String,
    title: String,
    message: String,
    url: UrlForm,
    events: (
        crossbeam_channel::Sender<Event>,
        crossbeam_channel::Receiver<Event>,
    ),
    attempted: bool,
    context: Option<egui::Context>,
    system_urls: Vec<String>,
    javascript: Option<bool>,
    revision: Option<u64>,
}
impl Default for Workspace {
    fn default() -> Self {
        Self {
            server: None,
            retiring: Vec::new(),
            host: Host::default(),
            requested: None,
            rect: None,
            frame_popup_open: false,
            parent: None,
            owner: None,
            file: None,
            address: String::new(),
            title: String::new(),
            message: String::new(),
            url: UrlForm::default(),
            events: crossbeam_channel::bounded(64),
            attempted: false,
            context: None,
            system_urls: Vec::new(),
            javascript: None,
            revision: None,
        }
    }
}
impl IdeApp {
    fn preview_server(&mut self, root: &Path) -> Result<(), String> {
        let root = dunce::canonicalize(root).map_err(|e| e.to_string())?;
        if self
            .web_preview
            .server
            .as_ref()
            .is_some_and(|server| server.root == root && !server.cancelling())
        {
            return Ok(());
        }
        if let Some(server) = self.web_preview.server.take() {
            server.cancel();
            self.web_preview.retiring.push(server);
        }
        self.web_preview.server = Some(Server::start(
            &root,
            self.settings.web_browsers.reload_on_save,
            self.settings.markdown.clone(),
        )?);
        Ok(())
    }
    /// Common current-file entry for HTML/Markdown toolbar and action consumers.
    pub(super) fn open_current_web_preview(&mut self, embedded: bool) {
        let path = self
            .active
            .and_then(|i| self.tabs.get(i))
            .and_then(|tab| tab.state.path.clone());
        if let Some(path) = path {
            self.open_web_file(&path, None, embedded);
        } else {
            self.set_status("Save the document before opening browser preview");
        }
    }
    pub(super) fn open_web_file(&mut self, path: &Path, profile: Option<String>, embedded: bool) {
        let project_safe_mode = self.project_safe_mode();
        let path = rustpy_core::vfs::normalize_path(path);
        if !path.is_file() {
            self.set_status("Save the file before opening browser preview");
            return;
        }
        let markdown_file = matches!(
            path.extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("md" | "markdown")
        );
        if std::fs::metadata(&path).is_ok_and(|metadata| {
            metadata.len()
                > if markdown_file {
                    1024 * 1024
                } else {
                    32 * 1024 * 1024
                }
        }) {
            self.set_status(
                "Browser preview source exceeds its 1 MiB Markdown / 32 MiB asset budget",
            );
            return;
        }
        let root = self
            .project
            .as_ref()
            .filter(|p| path.starts_with(&p.root))
            .map(|p| p.root.clone())
            .or_else(|| path.parent().map(Path::to_path_buf));
        let Some(root) = root else {
            return;
        };
        if project_safe_mode || self.project.is_none() && !self.settings.is_project_trusted(&root) {
            self.set_status(
                "Trust this project or its folder before serving executable browser previews",
            );
            return;
        }
        let wants_native = embedded
            || profile.is_none() && self.settings.web_browsers.default == DefaultBrowser::BuiltIn;
        if wants_native && !Host::available() {
            self.web_preview.message =
                "This build does not include the native-web-preview engine feature".into();
            self.bottom = Some(BottomTool::WebPreview);
            return;
        }
        if let Err(error) = self.preview_server(&root) {
            self.set_status(error);
            return;
        }
        self.web_preview.owner = self.project.as_ref().map(|p| p.root.clone());
        self.web_preview.file = Some(path.clone());
        let markdown = markdown_file;
        let source = self
            .tabs
            .iter()
            .find(|tab| tab.state.path.as_deref() == Some(path.as_path()))
            .map(|tab| tab.state.doc.text());
        if markdown {
            let source = source
                .or_else(|| std::fs::read_to_string(&path).ok())
                .unwrap_or_default();
            // HTTP-relative links/images resolve through the captured host;
            // file:// bases would be blocked by browser origin rules.
            if source.len() > 1024 * 1024 {
                self.set_status("Markdown preview exceeds 1 MiB");
                return;
            }
            self.web_preview
                .server
                .as_ref()
                .unwrap()
                .override_markdown(&path, source);
        } else if let Some(source) = source.filter(|_| {
            matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("html" | "htm")
            )
        }) {
            self.web_preview
                .server
                .as_ref()
                .unwrap()
                .override_html(&path, source);
        }
        match self.web_preview.server.as_ref().unwrap().url(&path) {
            Ok(url) => self.open_web_url(&url, profile, embedded),
            Err(error) => self.set_status(error),
        }
    }
    pub(super) fn open_web_url(&mut self, url: &str, profile: Option<String>, embedded: bool) {
        let project_safe_mode = self.project_safe_mode();
        let url = match browser::validated_url(url) {
            Ok(url) => url,
            Err(error) => {
                self.set_status(error);
                return;
            }
        };
        if url.starts_with("file:") {
            match url::Url::parse(&url)
                .ok()
                .and_then(|url| url.to_file_path().ok())
            {
                Some(path) => self.open_web_file(&path, profile, embedded),
                None => self.set_status("Invalid file URL"),
            };
            return;
        }
        if project_safe_mode {
            self.set_status("Trust the project before opening browser previews");
            return;
        }
        let selected = match profile {
            Some(id) => DefaultBrowser::Profile(id),
            None if embedded => DefaultBrowser::BuiltIn,
            None => self.settings.web_browsers.default.clone(),
        };
        if selected == DefaultBrowser::BuiltIn {
            self.web_preview.address = url.clone();
            if !Host::available() {
                self.web_preview.requested = None;
                self.web_preview.attempted = true;
                self.web_preview.message="This build does not include native-web-preview; no embedded engine was created".into();
                self.bottom = Some(BottomTool::WebPreview);
                return;
            }
            self.web_preview.requested = Some(url);
            self.web_preview.attempted = false;
            self.web_preview.message.clear();
            self.web_preview.owner = self.project.as_ref().map(|p| p.root.clone());
            self.bottom = Some(BottomTool::WebPreview);
            self.floating_bottom = false;
            return;
        }
        let profile = match selected {
            DefaultBrowser::Profile(id) => match self
                .settings
                .web_browsers
                .profiles
                .iter()
                .find(|profile| profile.id == id)
                .cloned()
            {
                Some(profile) => profile,
                None => {
                    self.set_status(format!("Browser profile '{id}' no longer exists"));
                    return;
                }
            },
            DefaultBrowser::System if !self.settings.browser.trim().is_empty() => Profile {
                id: "legacy".into(),
                name: "Legacy browser".into(),
                kind: Kind::Custom,
                program: self.settings.browser.clone(),
                ..Default::default()
            },
            _ => Profile::template(Kind::System),
        };
        match browser::launch(&profile, &url) {
            Ok(Launch::System(url)) => {
                if let Some(ctx) = &self.web_preview.context {
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                } else if self.web_preview.system_urls.len() < 16 {
                    self.web_preview.system_urls.push(url);
                }
            }
            Ok(Launch::Program { program, args }) => {
                // An IFEO Debugger (MSEdgeRedirect) starts instead of the
                // browser: respected, but said instead of "Opened".
                let redirect = browser::redirected_by(&program)
                    .map(|debugger| browser::redirect_message(&profile.name, &debugger));
                let root = self
                    .project
                    .as_ref()
                    .map(|p| p.root.clone())
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| Settings::config_dir())
                    });
                let cfg = RunConfig {
                    name: format!("Browser launcher: {}", profile.name),
                    kind: RunConfigKind::Program { program, args },
                    working_dir: Some(root.clone()),
                    temporary: true,
                    add_content_roots: false,
                    add_source_roots: false,
                    ..Default::default()
                };
                let id = self.next_session_id;
                self.next_session_id += 1;
                self.run_panel
                    .add_session(RunSession::spawn_launcher(id, &cfg, &root));
                self.set_status(
                    redirect.unwrap_or_else(|| format!("Opened URL with {}", profile.name)),
                );
            }
            Err(error) => self.set_status(error),
        }
    }
    pub(super) fn web_browser_bar(&mut self, ui: &mut egui::Ui, index: usize) {
        if !self.settings.web_browsers.editor_bar
            || !matches!(
                self.tabs[index].state.language,
                Language::Html | Language::Markdown
            )
        {
            return;
        }
        let path = self.tabs[index].state.path.clone();
        let profiles = self
            .settings
            .web_browsers
            .profiles
            .iter()
            .filter(|p| p.enabled)
            .cloned()
            .collect::<Vec<_>>();
        ui.horizontal_wrapped(|ui| {
            ui.label("Open in browser:");
            if ui.small_button("Default").clicked() {
                if let Some(path) = &path {
                    self.open_web_file(path, None, false);
                }
            }
            for profile in profiles {
                if ui.small_button(&profile.name).clicked() {
                    if let Some(path) = &path {
                        self.open_web_file(path, Some(profile.id), false);
                    }
                }
            }
            if ui.small_button("Built-in preview").clicked() {
                if let Some(path) = &path {
                    self.open_web_file(path, None, true);
                }
            }
            if ui.small_button("Open URL…").clicked() {
                self.dialog = Dialog::OpenWebUrl;
            }
        });
    }
    pub(super) fn web_preview_tool(&mut self, ui: &mut egui::Ui) {
        // Popup memory belongs to this content viewport. Finalization runs in
        // the root viewport, which cannot observe a detached tool's popup.
        self.web_preview.frame_popup_open = egui::Popup::is_any_open(ui.ctx());
        ui.horizontal_wrapped(|ui| {
            if ui.button("Back").clicked() {
                let _ = self.web_preview.host.back();
            }
            if ui.button("Forward").clicked() {
                let _ = self.web_preview.host.forward();
            }
            if ui.button("Reload").clicked() {
                if let Some(server) = &self.web_preview.server {
                    server.bump();
                }
                let _ = self.web_preview.host.reload();
            }
            if ui.button("Stop preview").clicked() {
                self.cancel_web_preview_project();
            }
            ui.checkbox(
                &mut self.settings.web_browsers.reload_on_save,
                "Reload on save",
            );
            if ui.button("Open current file").clicked() {
                self.open_current_web_preview(true);
            }
            if ui.button("Open URL…").clicked() {
                self.dialog = Dialog::OpenWebUrl;
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.web_preview.address)
                    .desired_width(ui.available_width() - 90.),
            );
            if ui.button("Go").clicked() {
                let url = self.web_preview.address.clone();
                self.open_web_url(&url, None, true);
            }
        });
        if !self.web_preview.title.is_empty() {
            ui.label(&self.web_preview.title);
        }
        if !self.web_preview.message.is_empty() {
            ui.colored_label(self.theme.warning, &self.web_preview.message);
            if ui.button("Retry native engine").clicked() {
                self.web_preview.attempted = false;
                self.web_preview.requested = Some(self.web_preview.address.clone());
            }
        }
        if !Host::available() {
            ui.label("Native engine is not included in this build. Enable native-web-preview; Windows needs WebView2, macOS uses WKWebView, Linux needs WebKitGTK on X11/XWayland. This tool window does not substitute a text preview.");
            return;
        }
        if self.web_preview.requested.is_none() && !self.web_preview.host.is_open() {
            ui.label("Open an HTML/Markdown file or URL to create a native browser view");
            return;
        }
        let size = ui.available_size().max(egui::vec2(1., 1.));
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        self.web_preview.context = Some(ui.ctx().clone());
        self.web_preview.rect = Some((ui.ctx().viewport_id(), rect));
    }
    pub(super) fn web_preview_begin_frame(&mut self, ctx: &egui::Context) {
        let project_safe_mode = self.project_safe_mode();
        self.web_preview.context = Some(ctx.clone());
        self.web_preview.rect = None;
        self.web_preview.frame_popup_open = false;
        // Ordinary frames must pump GTK while the child remains mapped.
        // Known modal/quit owners suppress it now: the quit guard returns
        // before finish_frame, and native children otherwise cover that guard.
        if project_safe_mode
            || !matches!(self.dialog, Dialog::None)
            || self.choice.is_some()
            || egui::Popup::is_any_open(ctx)
            || self.coordinated_quit_active()
            || self.coordinated_quit_committed
            || self.exit_requested
        {
            self.web_preview.host.hide();
        }
        if self
            .web_preview
            .parent
            .is_some_and(|parent| eframe::native_window::get(ctx, parent).is_none())
        {
            self.web_preview.host.reset();
            self.web_preview.parent = None;
            self.web_preview.attempted = false;
            self.web_preview.requested = Some(self.web_preview.address.clone());
        }
        self.web_preview.host.pump();
        for server in &mut self.web_preview.retiring {
            server.reap();
        }
        self.web_preview.retiring.retain(Server::running);
        if self.coordinated_quit_active() || self.coordinated_quit_committed || self.exit_requested
        {
            return;
        }
        for url in std::mem::take(&mut self.web_preview.system_urls) {
            ctx.open_url(egui::OpenUrl::new_tab(url));
        }
        if self.web_preview.javascript != Some(self.settings.web_browsers.javascript) {
            self.web_preview.javascript = Some(self.settings.web_browsers.javascript);
            if self.web_preview.host.is_open() {
                self.web_preview.host.reset();
                self.web_preview.requested = Some(self.web_preview.address.clone());
                self.web_preview.attempted = false;
            }
        }
        if let Some(server) = &mut self.web_preview.server {
            server.reap();
            server.set_reload(self.settings.web_browsers.reload_on_save);
            server.configure_markdown(&self.settings.markdown);
            let revision = server.revision();
            if self
                .web_preview
                .revision
                .is_some_and(|previous| previous != revision)
                && !self.settings.web_browsers.javascript
            {
                let _ = self.web_preview.host.reload();
            }
            self.web_preview.revision = Some(revision);
        }
        if project_safe_mode {
            self.cancel_web_preview_project();
        }
        #[cfg(feature = "native-web-preview")]
        while let Ok(event) = self.web_preview.events.1.try_recv() {
            match event {
                Event::Title(title) => self.web_preview.title = title,
                Event::Navigated(url) => {
                    if url != "about:blank" {
                        self.web_preview.address = url;
                    }
                }
                Event::External(url) => {
                    if browser::validated_url(&url).is_ok() {
                        self.web_preview.message = format!(
                            "External navigation blocked: {url}. Use Open URL or a configured browser to open it explicitly."
                        );
                    }
                }
            }
        }
    }
    pub(super) fn web_preview_frame(&mut self, _frame: &eframe::Frame, ctx: &egui::Context) {
        self.web_preview_finish_frame(ctx);
    }
    fn web_preview_frame_visible(&self, ctx: &egui::Context) -> bool {
        !self.project_safe_mode()
            && !self.web_preview.frame_popup_open
            && matches!(self.dialog, Dialog::None)
            && self.choice.is_none()
            && !egui::Popup::is_any_open(ctx)
            && !self.coordinated_quit_active()
            && !self.coordinated_quit_committed
            && !self.exit_requested
    }
    fn web_preview_finish_frame(&mut self, ctx: &egui::Context) {
        let visible = self.web_preview_frame_visible(ctx);
        if visible {
            if let Some((viewport, rect)) = self.web_preview.rect {
                let Some(window) = eframe::native_window::get(ctx, viewport) else {
                    self.web_preview.host.hide();
                    ctx.request_repaint_after(std::time::Duration::from_millis(30));
                    return;
                };
                if self.web_preview.parent != Some(viewport) {
                    self.web_preview.host.reset();
                    self.web_preview.parent = Some(viewport);
                    self.web_preview.requested = Some(self.web_preview.address.clone());
                    self.web_preview.attempted = false;
                }
                let scale = window.scale_factor() as f32 * ctx.zoom_factor();
                if !self.web_preview.attempted {
                    if let Some(url) = self.web_preview.requested.take() {
                        self.web_preview.attempted = true;
                        if let Err(error) = self.web_preview.host.navigate(
                            window.as_ref(),
                            &url,
                            rect,
                            scale,
                            self.settings.web_browsers.javascript,
                            self.web_preview.events.0.clone(),
                        ) {
                            self.web_preview.message = error;
                        }
                    }
                }
                if let Err(error) =
                    self.web_preview
                        .host
                        .show(rect, scale, self.settings.web_browsers.zoom)
                {
                    self.web_preview.message = error;
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
            } else {
                self.web_preview.host.hide();
            }
        } else {
            // Native children render above egui: dialogs, menus, safe mode and
            // quit must still suppress them before the current frame paints.
            self.web_preview.host.hide();
        }
        if self.web_preview_pending() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    pub(super) fn web_preview_saved(&mut self, path: &Path) {
        let Some(server) = &self.web_preview.server else {
            return;
        };
        if !path.starts_with(&server.root) {
            return;
        }
        server.remove_override(path);
        server.note_saved(path);
    }
    pub(super) fn release_web_preview_native_parent(&mut self) {
        if self
            .web_preview
            .parent
            .is_some_and(|parent| parent != egui::ViewportId::ROOT)
        {
            self.web_preview.host.reset();
            self.web_preview.parent = None;
            self.web_preview.attempted = false;
            self.web_preview.requested =
                (!self.web_preview.address.is_empty()).then(|| self.web_preview.address.clone());
        }
    }

    pub(super) fn cancel_web_preview_project(&mut self) {
        self.web_preview.parent = None;
        self.web_preview.host.reset();
        self.web_preview.requested = None;
        self.web_preview.rect = None;
        self.web_preview.owner = None;
        self.web_preview.file = None;
        self.web_preview.address.clear();
        self.web_preview.title.clear();
        self.web_preview.message.clear();
        self.web_preview.url = UrlForm::default();
        self.web_preview.attempted = false;
        self.web_preview.revision = None;
        self.web_preview.system_urls.clear();
        while self.web_preview.events.1.try_recv().is_ok() {}
        if matches!(self.dialog, Dialog::OpenWebUrl) {
            self.dialog = Dialog::None;
        }
        if let Some(server) = self.web_preview.server.take() {
            server.cancel();
            self.web_preview.retiring.push(server);
        }
    }
    pub(super) fn web_preview_pending(&self) -> bool {
        self.web_preview.host.is_open()
            || self.web_preview.requested.is_some()
            || !self.web_preview.system_urls.is_empty()
            || self
                .web_preview
                .server
                .as_ref()
                .is_some_and(Server::running)
            || self.web_preview.retiring.iter().any(Server::running)
    }
    pub(super) fn stop_web_preview(&mut self, stamps: &[String]) {
        if stamps == self.web_preview_stamps() {
            self.cancel_web_preview_project();
        } else {
            self.set_status("Web preview changed; select the current preview to stop it");
        }
    }
    pub(super) fn web_preview_labels(&self) -> Vec<String> {
        self.web_preview
            .server
            .iter()
            .chain(&self.web_preview.retiring)
            .filter(|s| s.running())
            .map(|s| format!("Web preview host {} ({})", s.address, s.root.display()))
            .chain(
                self.web_preview
                    .host
                    .is_open()
                    .then(|| "Embedded web preview".into()),
            )
            .chain(
                self.web_preview
                    .requested
                    .as_ref()
                    .map(|_| "Pending embedded web preview".into()),
            )
            .chain(
                self.web_preview
                    .system_urls
                    .iter()
                    .map(|_| "Pending browser launch".into()),
            )
            .collect()
    }
    pub(super) fn web_preview_stamps(&self) -> Vec<String> {
        self.web_preview
            .server
            .iter()
            .chain(&self.web_preview.retiring)
            .filter(|s| s.running())
            .map(Server::stamp)
            .chain(self.web_preview.host.id().map(|id| {
                format!(
                    "webview:{id}:{:?}:{:?}",
                    self.web_preview.owner, self.web_preview.file
                )
            }))
            .chain(
                self.web_preview
                    .requested
                    .as_ref()
                    .map(|url| format!("pending-webview:{:?}:{url}", self.web_preview.owner)),
            )
            .chain(
                self.web_preview
                    .system_urls
                    .iter()
                    .map(|url| format!("pending-browser:{url}")),
            )
            .collect()
    }
    pub(super) fn web_url_dialog(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut go = false;
        let mut cancel = false;
        let profiles = self
            .settings
            .web_browsers
            .profiles
            .iter()
            .filter(|p| p.enabled)
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect::<Vec<_>>();
        egui::Window::new("Open URL")
            .id(egui::Id::new("open_web_url"))
            .open(&mut open)
            .collapsible(false)
            .show_modal(ctx, |ui| {
                ui.label("URL");
                ui.add(
                    egui::TextEdit::singleline(&mut self.web_preview.url.value)
                        .desired_width(500.)
                        .hint_text("https://example.org or file:///…"),
                );
                ui.checkbox(&mut self.web_preview.url.embedded, "Built-in web preview");
                egui::ComboBox::from_id_salt("url_browser_profile")
                    .selected_text(if self.web_preview.url.profile.is_empty() {
                        "Default".into()
                    } else {
                        profiles
                            .iter()
                            .find(|(id, _)| id == &self.web_preview.url.profile)
                            .map(|(_, name)| name.clone())
                            .unwrap_or_else(|| "Missing profile".into())
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.web_preview.url.profile,
                            String::new(),
                            "Default",
                        );
                        for (id, name) in profiles {
                            ui.selectable_value(&mut self.web_preview.url.profile, id, name);
                        }
                    })
                    .combo_named(crate::i18n::tr_static("Browser profile"));
                if let Err(error) = browser::validated_url(&self.web_preview.url.value) {
                    ui.label(error);
                }
                ui.horizontal(|ui| {
                    go = ui
                        .add_enabled(
                            browser::validated_url(&self.web_preview.url.value).is_ok(),
                            egui::Button::new("Open"),
                        )
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if go {
            let form = std::mem::take(&mut self.web_preview.url);
            self.open_web_url(
                &form.value,
                (!form.profile.is_empty() && !form.embedded).then_some(form.profile),
                form.embedded,
            );
        } else if open && !cancel {
            self.dialog = Dialog::OpenWebUrl;
        }
    }
    pub(super) fn web_browsers_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Web Browsers and Preview");
        let settings = &mut self.settings.web_browsers;
        egui::ComboBox::from_id_salt("default_web_browser")
            .selected_text(format!("{:?}", settings.default))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.default,
                    DefaultBrowser::System,
                    "System default",
                );
                ui.selectable_value(
                    &mut settings.default,
                    DefaultBrowser::BuiltIn,
                    "Built-in engine",
                );
                for profile in &settings.profiles {
                    ui.selectable_value(
                        &mut settings.default,
                        DefaultBrowser::Profile(profile.id.clone()),
                        &profile.name,
                    );
                }
            })
            .combo_named(crate::i18n::tr_static("Default browser"));
        ui.checkbox(
            &mut settings.editor_bar,
            "Show browser bar in HTML and Markdown editors",
        );
        ui.checkbox(
            &mut settings.reload_on_save,
            "Reload page in browser on save",
        );
        ui.checkbox(
            &mut settings.javascript,
            "Enable JavaScript in built-in preview",
        );
        ui.horizontal(|ui| {
            ui.label("Native preview zoom");
            ui.add(egui::DragValue::new(&mut settings.zoom).range(0.25..=4.0));
        });
        let mut remove = None;
        for (index, profile) in settings.profiles.iter_mut().enumerate() {
            egui::CollapsingHeader::new(&profile.name)
                .id_salt(("browser_profile", index))
                .show(ui, |ui| {
                    ui.checkbox(&mut profile.enabled, "Enabled");
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut profile.name);
                    });
                    ui.label(format!("Profile ID: {}", profile.id));
                    if profile.kind != Kind::System {
                        ui.horizontal(|ui| {
                            ui.label("Executable");
                            ui.text_edit_singleline(&mut profile.program);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Arguments");
                            ui.text_edit_singleline(&mut profile.arguments);
                        });
                        if cfg!(any(windows, all(unix, not(target_os = "macos")))) {
                            let detected = browser::detect_cached(profile);
                            let qualified = if cfg!(windows) {
                                profile.program.contains(['/', '\\', ':'])
                            } else {
                                profile.program.contains('/')
                            };
                            ui.label(match (&detected.program, qualified) {
                                (Some(path), _) => format!("Detected: {}", path.display()),
                                (None, true) => "Executable path used as configured".into(),
                                (None, false) if cfg!(windows) => {
                                    "Not found in install folders, App Paths or PATH".into()
                                }
                                (None, false) => "Not found on PATH".into(),
                            });
                            if let Some(debugger) = &detected.redirected_by {
                                ui.colored_label(
                                    ui.visuals().warn_fg_color,
                                    browser::redirect_message(&profile.name, debugger),
                                );
                            }
                        }
                        ui.label("$URL$ expands into one captured argument. No shell is used.");
                    }
                    if ui.button("Remove profile").clicked() {
                        remove = Some(index);
                    }
                });
        }
        if let Some(index) = remove {
            settings.profiles.remove(index);
        }
        if ui.button("Add custom browser").clicked() {
            let mut index = settings.profiles.len();
            while settings
                .profiles
                .iter()
                .any(|p| p.id == format!("custom-{index}"))
            {
                index += 1;
            }
            settings.profiles.push(Profile {
                id: format!("custom-{index}"),
                ..Default::default()
            });
        }
        if !self.settings.browser.is_empty() {
            ui.label(format!("Legacy browser command: {}", self.settings.browser));
            if ui.button("Import legacy browser command").clicked() {
                let id = "legacy-import".to_string();
                self.settings.web_browsers.profiles.retain(|p| p.id != id);
                self.settings.web_browsers.profiles.push(Profile {
                    id: id.clone(),
                    name: "Imported browser".into(),
                    kind: Kind::Custom,
                    program: std::mem::take(&mut self.settings.browser),
                    ..Default::default()
                });
                self.settings.web_browsers.default = DefaultBrowser::Profile(id);
            }
        }
        ui.label("Embedded preview is a real native WebView. Enable native-web-preview when building. Windows: WebView2 runtime; macOS: WKWebView; Linux: GTK3/WebKitGTK 4.1 with X11/XWayland. No engine is silently replaced with rendered HTML text.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detached_content_popup_suppresses_native_child_during_root_finalization() {
        let mut app = IdeApp::new_welcome(Settings::default());
        app.safe_mode = false;
        let ctx = egui::Context::default();
        let child = egui::ViewportId::from_hash_of("detached-native-preview");
        let popup = egui::Id::new("preview-child-menu");
        let input = |viewport| egui::RawInput {
            viewport_id: viewport,
            viewports: [
                (egui::ViewportId::ROOT, egui::ViewportInfo::default()),
                (
                    child,
                    egui::ViewportInfo {
                        parent: Some(egui::ViewportId::ROOT),
                        ..Default::default()
                    },
                ),
            ]
            .into_iter()
            .collect(),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000., 720.),
            )),
            ..Default::default()
        };
        ctx.run_ui(input(egui::ViewportId::ROOT), |ui| {
            app.web_preview_begin_frame(ui.ctx());
        })
        .drop_without_applying_deltas();
        // Model the actual immediate child-content callback between root setup
        // and finalization. Use egui's real viewport-local popup memory, without
        // constructing a native browser or pretending these are pixel checks.
        ctx.run_ui(input(child), |ui| {
            assert_eq!(ui.ctx().viewport_id(), child);
            egui::Popup::open_id(ui.ctx(), popup);
            assert!(egui::Popup::is_any_open(ui.ctx()));
            app.web_preview_tool(ui);
        })
        .drop_without_applying_deltas();
        ctx.run_ui(input(egui::ViewportId::ROOT), |ui| {
            assert!(
                !egui::Popup::is_any_open(ui.ctx()),
                "root cannot see the detached popup"
            );
            assert!(
                !app.web_preview_frame_visible(ui.ctx()),
                "captured content popup must suppress the real native child"
            );
            let before = app.web_preview.host.hide_requests();
            app.web_preview_finish_frame(ui.ctx());
            assert_eq!(app.web_preview.host.hide_requests(), before + 1);
            assert!(
                app.web_preview.frame_popup_open,
                "root finalization must not discard the child observation"
            );
        })
        .drop_without_applying_deltas();
        ctx.run_ui(input(egui::ViewportId::ROOT), |ui| {
            app.web_preview_begin_frame(ui.ctx());
            assert!(
                !app.web_preview.frame_popup_open,
                "new root frame resets the prior observation"
            );
        })
        .drop_without_applying_deltas();
        ctx.run_ui(input(child), |ui| {
            egui::Popup::close_id(ui.ctx(), popup);
            app.web_preview_tool(ui);
        })
        .drop_without_applying_deltas();
        ctx.run_ui(input(egui::ViewportId::ROOT), |ui| {
            assert!(
                app.web_preview_frame_visible(ui.ctx()),
                "closed child popup must not suppress subsequent frames"
            );
            app.dialog = Dialog::About;
            assert!(
                !app.web_preview_frame_visible(ui.ctx()),
                "root dialog suppression remains authoritative"
            );
        })
        .drop_without_applying_deltas();
    }
    #[test]
    fn frame_setup_keeps_native_child_mapped_but_current_layout_still_suppresses_it() {
        let ctx = egui::Context::default();
        let mut app = IdeApp::new_welcome(Settings::default());
        app.safe_mode = false;
        // Trace actual Host hide requests without constructing a platform
        // webview in this headless fixture. Native paint remains a GUI oracle.
        for _ in 0..4 {
            let before = app.web_preview.host.hide_requests();
            app.web_preview_begin_frame(&ctx);
            assert_eq!(
                app.web_preview.host.hide_requests(),
                before,
                "GTK must dispatch before any current-frame suppression"
            );
        }
        app.dialog = Dialog::About;
        app.web_preview.rect = Some((
            egui::ViewportId::ROOT,
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400., 200.)),
        ));
        app.web_preview_finish_frame(&ctx);
        assert_eq!(
            app.web_preview.host.hide_requests(),
            1,
            "modal must cover the native child"
        );
        app.dialog = Dialog::None;
        app.web_preview.rect = None;
        app.web_preview_finish_frame(&ctx);
        assert_eq!(
            app.web_preview.host.hide_requests(),
            2,
            "closed/hidden tool must hide the child"
        );
        app.web_preview.rect = Some((egui::ViewportId::ROOT, egui::Rect::EVERYTHING));
        app.web_preview_finish_frame(&ctx);
        assert_eq!(
            app.web_preview.host.hide_requests(),
            3,
            "missing native viewport cannot leave an old child mapped"
        );
        app.safe_mode = true;
        app.web_preview_finish_frame(&ctx);
        assert_eq!(
            app.web_preview.host.hide_requests(),
            4,
            "safe mode still suppresses native content"
        );
    }
    #[test]
    fn quit_guard_early_return_suppresses_native_child_before_ordinary_frame_finalization() {
        let mut app = IdeApp::new_welcome(Settings::default());
        app.settings.confirm_exit = true;
        let transaction = "web-preview-native-quit-consent";
        assert!(matches!(
            app.prepare_coordinated_quit(transaction),
            super::super::window_lifecycle::QuitProgress::PendingProcesses { .. }
        ));
        let ctx = egui::Context::default();
        let mut guard_rendered = false;
        let mut ordinary_frame_finished = false;
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000., 720.),
                )),
                ..Default::default()
            },
            |ui| {
                app.web_preview_begin_frame(ui.ctx());
                // Match the production early return, using its actual guard UI.
                if app.coordinated_quit_active() || app.coordinated_quit_committed {
                    app.coordinated_quit_guard_ui(ui.ctx());
                    guard_rendered = true;
                    return;
                }
                ordinary_frame_finished = true;
                app.web_preview_finish_frame(ui.ctx());
            },
        )
        .drop_without_applying_deltas();
        assert!(guard_rendered && !ordinary_frame_finished);
        assert_eq!(
            app.web_preview.host.hide_requests(),
            1,
            "quit consent must suppress the native child despite its early return"
        );
        assert_eq!(
            app.abort_coordinated_quit(transaction),
            super::super::window_lifecycle::QuitProgress::Aborted
        );
        let before = app.web_preview.host.hide_requests();
        app.web_preview_begin_frame(&ctx);
        assert_eq!(
            app.web_preview.host.hide_requests(),
            before,
            "cancelled quit restores ordinary mapped GTK dispatch"
        );
    }
    #[test]
    fn captured_stop_preserves_a_replacement_preview_and_cancels_queued_browser_launches() {
        let mut app = IdeApp::new_welcome(Settings::default());
        app.web_preview.requested = Some("https://example.org/first".into());
        let original = app.web_preview_stamps();
        app.web_preview.requested = Some("https://example.org/replacement".into());
        app.web_preview
            .system_urls
            .push("https://example.org/external".into());
        app.stop_web_preview(&original);
        assert_eq!(
            app.web_preview.requested.as_deref(),
            Some("https://example.org/replacement")
        );
        assert!(app.web_preview_pending());
        let current = app.web_preview_stamps();
        assert_ne!(original, current);
        app.stop_web_preview(&current);
        assert!(!app.web_preview_pending());
        assert!(app.web_preview_stamps().is_empty());
        assert!(app.web_preview.system_urls.is_empty());
    }
    #[test]
    fn global_system_url_planning_is_captured_without_starting_a_process() {
        let mut app = IdeApp::new_welcome(Settings::default());
        app.safe_mode = false;
        app.open_web_url("https://example.org/?q=two%20words", None, false);
        assert_eq!(
            app.web_preview.system_urls,
            vec!["https://example.org/?q=two%20words"]
        );
        assert!(app.run_panel.sessions.is_empty());
        app.cancel_web_preview_project();
        assert!(app.web_preview.system_urls.is_empty());
    }
    #[test]
    fn safe_mode_refuses_browser_execution_and_project_cancel_discards_url_intents() {
        let mut app = IdeApp::new_welcome(Settings::default());
        app.safe_mode = true;
        app.open_web_url("https://example.org", None, true);
        assert!(app.web_preview.requested.is_none());
        assert!(app.run_panel.sessions.is_empty());
        app.safe_mode = false;
        app.open_web_url("https://example.org", None, true);
        assert_eq!(app.web_preview.requested.is_some(), Host::available());
        app.web_preview.requested = Some("https://example.org".into());
        app.cancel_web_preview_project();
        assert!(app.web_preview.requested.is_none());
        assert!(!app.web_preview.host.is_open());
    }
}
