//! Updates (roadmap §8): Help › Check for Updates…, the "IDE Update" notification of the
//! daily automatic check, Settings › Appearance & Behavior › System Settings › Updates, and
//! Help › What's New in RustPy, which also opens by itself once after an update.
//!
//! Checks run on a background thread, so the UI never waits for the network. Headless runs
//! (`--screenshot` or `--input-script`, as `scripts/smoke.sh` uses) never check
//! automatically and never start a browser; in them `RUSTPY_UPDATE_FEED` stands in for the
//! network: the path of a GitHub Releases API JSON file, or `offline`, `ratelimit` or
//! `http:<status>` to act out a failure. `RUSTPY_NO_UPDATE_CHECK` switches the automatic
//! check off everywhere (for managed installations). Microsoft Store (MSIX) installations
//! never check or download: the Store delivers their updates, and Check for Updates and the
//! Settings page say so.

use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_settings::UpdateSettings;
use rustpy_updates::{Channel, FetchError, Release, Version};

/// Whether a store owns this installation's updates (Microsoft Store / MSIX package),
/// detected once per process.
fn store_managed_updates() -> bool {
    static MANAGED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MANAGED.get_or_init(|| {
        rustpy_updates::Platform::current()
            .install
            .updates_managed_externally()
    })
}

/// CHANGELOG.md of this build: What's New shows the section of the running version.
const CHANGELOG: &str = include_str!("../../../../CHANGELOG.md");

/// State of update checks, their windows and What's New.
#[derive(Default)]
pub(super) struct UpdatesUi {
    /// The first-frame work (What's New after an update) is done.
    started: bool,
    scheduler: rustpy_updates::Scheduler,
    check: Option<RunningCheck>,
    /// Check for Updates: progress, then the result.
    window: Option<UpdateWindow>,
    /// The automatic check found an update: the notification in the corner.
    notification: Option<Box<Release>>,
    whats_new: Option<WhatsNew>,
    /// What the last check of this session found (shown on the Settings page).
    last_outcome: Option<String>,
    install: Option<(
        rustpy_updates::install::UpdateOwner,
        crossbeam_channel::Receiver<Result<rustpy_updates::install::PreparedUpdate, String>>,
    )>,
    prepared: Option<rustpy_updates::install::PreparedUpdate>,
    restart_requested: bool,
}

struct RunningCheck {
    rx: crossbeam_channel::Receiver<Result<Vec<Release>, FetchError>>,
    /// The canonical API URL (or invalid setting) this worker was started for.
    repository: Result<String, FetchError>,
    /// Help › Check for Updates or the Settings button: the result is shown in a window.
    manual: bool,
}
impl Drop for UpdatesUi {
    fn drop(&mut self) {
        if let Some((owner, _)) = &self.install {
            owner.cancel();
        }
    }
}

enum UpdateWindow {
    Checking,
    UpToDate {
        newest: Option<String>,
    },
    Available {
        release: Box<Release>,
        download: Download,
    },
    /// The check could not complete: a calm explanation.
    Unavailable(String),
    /// A Microsoft Store installation: the Store delivers updates.
    StoreManaged,
}

/// Where Download leads: the installer or package for this platform, else the release page.
struct Download {
    url: String,
    file: Option<String>,
}

struct WhatsNew {
    version: String,
    markdown: String,
}

enum UpdateAction {
    Prepare(Box<Release>),
    Close,
    Download(String),
    Open(String),
    Ignore(String),
    Configure,
    Retry,
    ShowUpdate,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "Last checked:" text: `Today 10:14`, `Yesterday 18:02`, `Oct 3, 2026 09:30`.
fn describe_last_check<Tz: chrono::TimeZone>(last_check: i64, now: &chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(t) = chrono::DateTime::from_timestamp(last_check, 0).filter(|_| last_check > 0) else {
        return "Never".into();
    };
    let t = t.with_timezone(&now.timezone());
    match (now.date_naive() - t.date_naive()).num_days() {
        0 => format!("Today {}", t.format("%H:%M")),
        1 => format!("Yesterday {}", t.format("%H:%M")),
        _ => t.format("%b %-d, %Y %H:%M").to_string(),
    }
}

/// The explanation shown when a check could not complete (never alarming: a private
/// repository, GitHub's rate limit or being offline are expected situations).
fn calm_message(e: &FetchError) -> String {
    let mut m = e.message();
    if let FetchError::Refused {
        rate_limited: true,
        reset_at: Some(reset),
        ..
    } = e
    {
        if let Some(t) = chrono::DateTime::from_timestamp(*reset, 0) {
            m.push_str(&format!(
                " You can try again after {}.",
                t.with_timezone(&chrono::Local).format("%H:%M")
            ));
        }
    }
    m
}

/// Release notes of the running version for What's New, from the CHANGELOG.md of this build.
fn whats_new_markdown(changelog: &str, version: &str, releases_page: &str) -> String {
    match rustpy_updates::changelog::notes_for(changelog, version) {
        Some((label, notes)) if label.eq_ignore_ascii_case("unreleased") => format!("*This is a development build. These are the changes since the last release.*\n\n{notes}"),
        Some((_, notes)) => notes,
        None => format!("No release notes are included in this build. All releases are listed at {releases_page}."),
    }
}

/// Whether the user-editable values of the Updates page differ (bookkeeping such as the
/// time of the last check does not count).
pub(super) fn page_modified(old: &Settings, new: &Settings) -> bool {
    let (a, b) = (&old.updates, &new.updates);
    (
        a.check_automatically,
        a.download_automatically,
        &a.channel,
        a.repository.trim(),
        a.show_whats_new,
        &a.ignored_versions,
    ) != (
        b.check_automatically,
        b.download_automatically,
        &b.channel,
        b.repository.trim(),
        b.show_whats_new,
        &b.ignored_versions,
    )
}

/// Starts `url` in the browser of Settings › Tools › Web Browsers, else the system default.
fn open_in_browser(browser: &str, url: &str) -> std::io::Result<()> {
    let mut cmd = if !browser.trim().is_empty() {
        std::process::Command::new(browser.trim())
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        // Not `cmd /C start`: cmd would interpret `&` in the URL.
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    let mut child = cmd
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

impl IdeApp {
    pub(super) fn finish_update_restart(&mut self) {
        if !(self.exit_requested || self.coordinated_quit_committed)
            || !self.pending_update_restart()
        {
            return;
        }
        if let Some(prepared) = self.take_pending_update_restart() {
            if let Err(error) = prepared.launch_after_exit() {
                log::error!("Cannot start staged update: {error}");
                if let Some(directory) = prepared.artifact.parent() {
                    let _ = std::fs::write(directory.join("launch-error.txt"), &error);
                }
                self.updates.prepared = Some(prepared);
            }
        }
    }
    pub(super) fn update_restart_stamp(&self) -> String {
        format!(
            "{:?}",
            (
                self.updates.restart_requested,
                self.updates.prepared.as_ref().map(|prepared| (
                    &prepared.version,
                    &prepared.artifact,
                    &prepared.executable,
                    &prepared.arguments,
                    &prepared.sha256,
                    prepared.install,
                    prepared.binary
                ))
            )
        )
    }
    pub(super) fn stop_update_transfer(&mut self) {
        if let Some((owner, _)) = &self.updates.install {
            owner.cancel();
        }
    }
    pub(super) fn poll_update_retirement(&mut self, ctx: &egui::Context) {
        if let Some((_, receiver)) = &self.updates.install {
            match receiver.try_recv() {
                Ok(_) | Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.updates.install = None
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50))
                }
            }
        }
    }
    pub(crate) fn cancel_pending_update_restart(&mut self) {
        self.updates.restart_requested = false;
    }
    pub(crate) fn pending_update_restart(&self) -> bool {
        self.updates.restart_requested && self.updates.prepared.is_some()
    }
    pub(crate) fn take_pending_update_restart(
        &mut self,
    ) -> Option<rustpy_updates::install::PreparedUpdate> {
        if !self.updates.restart_requested {
            return None;
        }
        self.updates.restart_requested = false;
        self.updates.prepared.take()
    }
    pub(crate) fn update_transfer_state(&self) -> Option<(u64, String, String)> {
        self.updates
            .install
            .as_ref()
            .map(|(owner, _)| (owner.id, "Downloading IDE update".into(), owner.stamp()))
    }
    pub(crate) fn cancel_update_transfer(&mut self, id: u64) -> bool {
        if let Some((owner, _)) = &self.updates.install {
            if owner.id == id {
                owner.cancel();
                return true;
            }
        }
        false
    }
    fn update_install_window(&mut self, ctx: &egui::Context) {
        if let Some((_, receiver)) = &self.updates.install {
            match receiver.try_recv() {
                Ok(result) => {
                    self.updates.install = None;
                    match result {
                        Ok(prepared) => self.updates.prepared = Some(prepared),
                        Err(error) => self.set_status(error),
                    }
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.updates.install = None;
                    self.set_status("The update download worker stopped");
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(150));
                }
            }
        }
        if let Some((owner, _)) = &self.updates.install {
            egui::Window::new("Downloading IDE update")
                .collapsible(false)
                .show_modeless(ctx, |ui| {
                    crate::a11y::spinner(ui);
                    ui.label(format!(
                        "{} MiB downloaded",
                        owner.downloaded() / 1024 / 1024
                    ));
                    if ui.button("Cancel download").clicked() {
                        owner.cancel();
                    }
                });
        }
        let mut restart = false;
        if let Some(prepared) = &self.updates.prepared {
            egui::Window::new("Update ready").collapsible(false).show_modeless(ctx, |ui| { ui.label(format!("RustPy {} has been verified and is ready to install.", prepared.version)); ui.label("Your usual save and running-process prompts appear before restarting. Installation errors are recorded beside the staged update."); if ui.button("Restart to update").clicked() { restart = true; } });
        }
        if restart {
            self.updates.restart_requested = true;
            if self.shell_managed_window {
                self.shell_global_quit_requested = true;
            } else {
                self.request_exit();
            }
        }
    }

    /// Smoke tests and other scripted runs: no automatic check, no browser.
    fn updates_headless(&self) -> bool {
        self.screenshot_path.is_some() || self.input_player.is_some()
    }

    /// Called every frame: What's New after an update (first frame), the daily automatic
    /// check, results of running checks, and the update windows.
    pub(super) fn updates_tick(&mut self, ctx: &egui::Context) {
        self.update_install_window(ctx);
        if !self.updates.started {
            self.updates.started = true;
            self.whats_new_after_update();
        }
        let now = unix_now();
        let automatic = self.settings.updates.check_automatically
            && !self.updates_headless()
            && !store_managed_updates()
            && std::env::var_os("RUSTPY_NO_UPDATE_CHECK").is_none();
        if self
            .updates
            .scheduler
            .should_start(automatic, self.settings.updates.last_check, now)
        {
            self.start_update_check(false);
        }
        self.poll_update_check(now);
        if self.updates.check.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        self.update_window(ctx);
        self.update_notification(ctx);
        self.whats_new_window(ctx);
    }

    /// Help › Check for Updates… (and the button on the Settings page).
    pub(super) fn check_for_updates(&mut self) {
        if store_managed_updates() {
            self.updates.notification = None;
            self.updates.window = Some(UpdateWindow::StoreManaged);
            return;
        }
        self.updates.window = Some(UpdateWindow::Checking);
        self.updates.notification = None;
        match &mut self.updates.check {
            // An automatic check is already running: show its result.
            Some(c) => c.manual = true,
            None => self.start_update_check(true),
        }
    }

    /// Help › What's New in RustPy.
    pub(super) fn show_whats_new(&mut self) {
        let page = rustpy_updates::releases_page_url(&self.settings.updates.repository);
        let version = rustpy_updates::CURRENT_VERSION.to_string();
        self.updates.whats_new = Some(WhatsNew {
            markdown: whats_new_markdown(CHANGELOG, &version, &page),
            version,
        });
    }

    /// First frame: What's New when this version is newer than the one that ran last.
    fn whats_new_after_update(&mut self) {
        let current = rustpy_updates::CURRENT_VERSION;
        let last = self.settings.updates.last_seen_version.trim().to_string();
        if last == current {
            return;
        }
        // An empty value is the first start: nothing to announce.
        let upgraded =
            matches!((Version::parse(&last), Version::parse(current)), (Some(l), Some(c)) if c > l);
        if upgraded && self.settings.updates.show_whats_new {
            log::info!("updated from {last} to {current}: showing What's New");
            self.show_whats_new();
        }
        self.record_update_state(|u| u.last_seen_version = current.to_string());
    }

    /// Changes update bookkeeping (time of the last check, ignored and last seen versions)
    /// in the settings and in the Settings dialog's snapshot, so Cancel there keeps it, and
    /// saves the settings.
    fn record_update_state(&mut self, change: impl Fn(&mut UpdateSettings)) {
        change(&mut self.settings.updates);
        if let Some(s) = &mut self.settings_ui.snapshot {
            change(&mut s.settings.updates);
        }
        if let Err(e) = self.settings_to_persist().save() {
            log::warn!("cannot save settings: {e}");
        }
    }

    fn start_update_check(&mut self, manual: bool) {
        let repository = rustpy_updates::releases_api_url(&self.settings.updates.repository);
        let worker_repository = repository.clone();
        let feed = if self.updates_headless() {
            std::env::var("RUSTPY_UPDATE_FEED")
                .ok()
                .filter(|f| !f.trim().is_empty())
        } else {
            None
        };
        let (tx, rx) = crossbeam_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                let result = match feed {
                    Some(spec) => rustpy_updates::fetch_from_test_feed(&spec),
                    None => worker_repository.and_then(|url| rustpy_updates::fetch_releases(&url)),
                };
                let _ = tx.send(result);
            });
        match spawned {
            Ok(_) => {
                self.updates.scheduler.started();
                self.updates.check = Some(RunningCheck {
                    rx,
                    repository,
                    manual,
                });
            }
            Err(e) => {
                log::warn!("cannot start the update check: {e}");
                if manual {
                    self.updates.window = Some(UpdateWindow::Unavailable(
                        "RustPy could not start the update check. Please try again.".into(),
                    ));
                }
            }
        }
    }

    fn poll_update_check(&mut self, now: i64) {
        let Some(check) = &self.updates.check else {
            return;
        };
        if check.repository != rustpy_updates::releases_api_url(&self.settings.updates.repository) {
            // The old worker may still be on the network. Dropping its receiver
            // prevents its result or timestamp from being attributed to the new
            // repository; a waiting manual window checks the new setting instead.
            let retry = check.manual && matches!(self.updates.window, Some(UpdateWindow::Checking));
            self.updates.check = None;
            self.updates.scheduler = rustpy_updates::Scheduler::default();
            self.updates.last_outcome = None;
            self.updates.notification = None;
            if retry {
                self.start_update_check(true);
            }
            return;
        }
        let result = match check.rx.try_recv() {
            Ok(r) => r,
            Err(crossbeam_channel::TryRecvError::Empty) => return,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                Err(FetchError::Offline("the update check stopped".into()))
            }
        };
        // Show the result only when it was asked for and its window is still open
        // (a cancelled manual check shows nothing at all).
        let manual = check.manual;
        let show = manual && matches!(self.updates.window, Some(UpdateWindow::Checking));
        self.updates.check = None;
        let reached = result
            .as_ref()
            .map_or_else(FetchError::reached_server, |_| true);
        self.updates.scheduler.finished(reached, now);
        if reached {
            self.record_update_state(|u| u.last_check = now);
        }
        let releases = match result {
            Ok(r) => r,
            Err(e) => {
                log::info!("update check: {e}");
                let message = calm_message(&e);
                self.updates.last_outcome = Some(message.clone());
                if show {
                    self.updates.window = Some(UpdateWindow::Unavailable(message));
                }
                return;
            }
        };
        let current = rustpy_updates::current_version();
        let channel = Channel::from_setting(&self.settings.updates.channel);
        // A check the user asked for also shows an ignored update.
        let ignored: &[String] = if show {
            &[]
        } else {
            &self.settings.updates.ignored_versions
        };
        match rustpy_updates::update_for(&current, &releases, channel, ignored).cloned() {
            Some(release) => {
                log::info!(
                    "update check: RustPy {} is available ({})",
                    release.version,
                    channel.label()
                );
                self.updates.last_outcome =
                    Some(format!("RustPy {} is available.", release.version));
                if !manual
                    && self.settings.updates.download_automatically
                    && self.updates.prepared.is_none()
                    && release.assets.iter().any(|a| a.name == "SHA256SUMS")
                    && rustpy_updates::asset_for(&release, &rustpy_updates::Platform::current())
                        .is_some()
                {
                    self.run_update_action(UpdateAction::Prepare(Box::new(release.clone())));
                }
                if show {
                    self.show_update(Box::new(release));
                } else if !manual && self.updates.window.is_none() {
                    self.updates.notification = Some(Box::new(release));
                }
            }
            None => {
                let newest =
                    rustpy_updates::newest(&releases, channel).map(|r| r.version.to_string());
                log::info!(
                    "update check: up to date ({current}; newest on {}: {newest:?})",
                    channel.label()
                );
                self.updates.last_outcome = Some("You already have the latest version.".into());
                if show {
                    self.updates.window = Some(UpdateWindow::UpToDate { newest });
                }
            }
        }
    }

    fn show_update(&mut self, release: Box<Release>) {
        let platform = rustpy_updates::Platform::current();
        let download = match rustpy_updates::asset_for(&release, &platform) {
            Some(a) => Download {
                url: a.url.clone(),
                file: Some(a.name.clone()),
            },
            None => Download {
                url: release.page_url.clone(),
                file: None,
            },
        };
        self.updates.notification = None;
        self.updates.window = Some(UpdateWindow::Available { release, download });
    }

    /// Opens an http(s) link from a release in the browser (never in headless runs).
    fn open_update_link(&mut self, url: &str) {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            log::warn!("not opening {url:?}: not a web address");
            return;
        }
        if self.updates_headless() {
            log::info!("updates: headless run, not opening {url}");
            self.set_status(format!("Would open {url}"));
            return;
        }
        match open_in_browser(&self.settings.browser, url) {
            Ok(()) => self.set_status(format!("Opened {url}")),
            Err(e) => self.set_status(format!("Cannot open the browser: {e}")),
        }
    }

    fn run_update_action(&mut self, action: UpdateAction) {
        match action {
            UpdateAction::Prepare(release) => {
                if self.updates_headless() || self.updates.install.is_some() {
                    return;
                }
                let owner = rustpy_updates::install::UpdateOwner::default();
                let worker = owner.clone();
                let directory = Settings::system_dir().join("updates");
                let (tx, rx) = crossbeam_channel::bounded(1);
                std::thread::spawn(move || {
                    let _ = tx.send(rustpy_updates::install::prepare(
                        &release,
                        &rustpy_updates::Platform::current(),
                        &directory,
                        &worker,
                    ));
                });
                self.updates.install = Some((owner, rx));
            }
            UpdateAction::Close => {}
            UpdateAction::Download(url) | UpdateAction::Open(url) => self.open_update_link(&url),
            UpdateAction::Ignore(version) => {
                self.record_update_state(|u| {
                    if !u.ignored_versions.contains(&version) {
                        u.ignored_versions.push(version.clone());
                    }
                });
                self.set_status(format!("RustPy {version} will not be offered again"));
            }
            UpdateAction::Configure => {
                self.settings_ui.page = settings_ui::SettingsPage::Updates;
                self.dialog = Dialog::Settings;
            }
            UpdateAction::Retry => self.check_for_updates(),
            UpdateAction::ShowUpdate => {}
        }
    }

    fn update_window(&mut self, ctx: &egui::Context) {
        let Some(window) = self.updates.window.take() else {
            return;
        };
        let theme = self.theme.clone();
        let current = rustpy_updates::CURRENT_VERSION;
        let channel = Channel::from_setting(&self.settings.updates.channel);
        let mut open = true;
        let mut action: Option<UpdateAction> = None;
        let title = match &window {
            UpdateWindow::Checking => "Check for Updates".to_string(),
            UpdateWindow::Available { release, .. } => {
                format!("RustPy {} Available", release.version)
            }
            _ => "Update Info".to_string(),
        };
        let resizable = matches!(window, UpdateWindow::Available { .. });
        let mut win = egui::Window::new(title)
            .id(egui::Id::new("rustpy_update_window"))
            .collapsible(false)
            .resizable(resizable)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open);
        if resizable {
            win = win.default_size([620.0, 480.0]);
        } else {
            win = win.default_width(440.0);
        }
        win.show_modal(ctx, |ui| match &window {
            UpdateWindow::Checking => {
                ui.horizontal(|ui| {
                    crate::a11y::spinner(ui);
                    ui.label("Checking for updates…");
                });
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        action = Some(UpdateAction::Close);
                    }
                });
            }
            UpdateWindow::UpToDate { newest } => {
                ui.label("You already have the latest version of RustPy installed.");
                let detail = match newest.as_deref().and_then(Version::parse) {
                    Some(n) if n < rustpy_updates::current_version() => format!("This build ({current}) is newer than the latest release on the {} channel ({n}).", channel.label()),
                    _ => format!("RustPy {current} · {} channel", channel.label()),
                };
                ui.label(egui::RichText::new(detail).color(theme.text_dim));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Configure Automatic Updates…").clicked() {
                        action = Some(UpdateAction::Configure);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("OK").clicked() {
                            action = Some(UpdateAction::Close);
                        }
                    });
                });
            }
            UpdateWindow::StoreManaged => {
                ui.label(rustpy_updates::STORE_UPDATES_MESSAGE);
                ui.label(egui::RichText::new(format!("RustPy {current} · installed from the Microsoft Store")).color(theme.text_dim));
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("OK").clicked() {
                        action = Some(UpdateAction::Close);
                    }
                });
            }
            UpdateWindow::Unavailable(message) => {
                ui.label("RustPy could not check for updates right now.");
                ui.label(egui::RichText::new(message).color(theme.text_dim));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Configure…").clicked() {
                        action = Some(UpdateAction::Configure);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("OK").clicked() {
                            action = Some(UpdateAction::Close);
                        }
                        if ui.button("Try Again").clicked() {
                            action = Some(UpdateAction::Retry);
                        }
                    });
                });
            }
            UpdateWindow::Available { release, download } => {
                let eap = if release.is_early_access() { " (Early Access Program)" } else { "" };
                ui.label(egui::RichText::new(format!("RustPy {}{eap} is available", release.version)).size(17.0).strong());
                let mut facts = format!("Your version: {current}   ·   New version: {}", release.version);
                if !release.date().is_empty() {
                    facts.push_str(&format!("   ·   Released {}", release.date()));
                }
                ui.label(egui::RichText::new(facts).color(theme.text_dim));
                ui.separator();
                egui::Panel::bottom("rustpy_update_buttons").show_separator_line(true).show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("Ignore This Update").on_hover_text("Do not offer this version again").clicked() {
                            action = Some(UpdateAction::Ignore(release.version.to_string()));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let hint = match &download.file {
                                Some(f) => format!("Downloads {f} in your browser"),
                                None => "Opens the release page in your browser".to_string(),
                            };
                            if ui.add(egui::Button::new(egui::RichText::new("Download").strong())).on_hover_text(hint).clicked() {
                                action = Some(UpdateAction::Download(download.url.clone()));
                            }
                            if download.file.is_some() && release.assets.iter().any(|a| a.name == "SHA256SUMS") && ui.button("Download and install…").clicked() {
                                action = Some(UpdateAction::Prepare(release.clone()));
                            }
                            if ui.button("Remind Me Later").clicked() {
                                action = Some(UpdateAction::Close);
                            }
                        });
                    });
                });
                egui::ScrollArea::vertical().id_salt("rustpy_update_notes").auto_shrink([false, false]).show(ui, |ui| {
                    if release.notes.trim().is_empty() {
                        ui.label(egui::RichText::new("This release has no release notes.").color(theme.text_dim));
                    } else {
                        crate::markdown_view::render_markdown(ui, &theme, &release.notes, 13.0);
                    }
                    if !release.page_url.is_empty() {
                        ui.add_space(6.0);
                        if ui.link("Release notes on GitHub").clicked() {
                            action = Some(UpdateAction::Open(release.page_url.clone()));
                        }
                    }
                });
            }
        });
        if !open {
            action = Some(UpdateAction::Close);
        }
        // Following a link keeps the window; every button closes it.
        if matches!(action, None | Some(UpdateAction::Open(_))) {
            self.updates.window = Some(window);
        }
        if let Some(a) = action {
            self.run_update_action(a);
        }
    }

    /// The automatic check found an update: a small notification in the bottom right
    /// corner (an "IDE Update" balloon) until it is opened or dismissed.
    fn update_notification(&mut self, ctx: &egui::Context) {
        let Some(release) = self.updates.notification.take() else {
            return;
        };
        let theme = self.theme.clone();
        let mut open = true;
        let mut action: Option<UpdateAction> = None;
        egui::Window::new("IDE Update")
            .id(egui::Id::new("rustpy_update_notification"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -40.0))
            .open(&mut open)
            .show_modeless(ctx, |ui| {
                ui.label(
                    egui::RichText::new(format!("RustPy {} available", release.version)).strong(),
                );
                if release.is_early_access() {
                    ui.label(
                        egui::RichText::new("Early Access Program")
                            .color(theme.text_dim)
                            .small(),
                    );
                }
                ui.horizontal(|ui| {
                    if ui.link("Update…").clicked() {
                        action = Some(UpdateAction::ShowUpdate);
                    }
                    if !release.page_url.is_empty() && ui.link("Release Notes").clicked() {
                        action = Some(UpdateAction::Open(release.page_url.clone()));
                    }
                    if ui.link("Ignore This Update").clicked() {
                        action = Some(UpdateAction::Ignore(release.version.to_string()));
                    }
                });
            });
        match action {
            Some(UpdateAction::ShowUpdate) => self.show_update(release),
            Some(a) => self.run_update_action(a),
            None if open => self.updates.notification = Some(release),
            None => {}
        }
    }

    fn whats_new_window(&mut self, ctx: &egui::Context) {
        let Some(wn) = self.updates.whats_new.take() else {
            return;
        };
        let theme = self.theme.clone();
        let mut open = true;
        let mut close = false;
        let mut releases = false;
        egui::Window::new(format!("What's New in RustPy {}", wn.version))
            .id(egui::Id::new("rustpy_whats_new"))
            .collapsible(false)
            .resizable(true)
            .default_size([720.0, 520.0])
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show_modeless(ctx, |ui| {
                egui::Panel::bottom("rustpy_whats_new_buttons")
                    .show_separator_line(true)
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.link("All releases").clicked() {
                                releases = true;
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Close").clicked() {
                                        close = true;
                                    }
                                },
                            );
                        });
                    });
                egui::ScrollArea::vertical()
                    .id_salt("rustpy_whats_new_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        crate::markdown_view::render_markdown(ui, &theme, &wn.markdown, 14.0);
                    });
            });
        if releases {
            let page = rustpy_updates::releases_page_url(&self.settings.updates.repository);
            self.open_update_link(&page);
        }
        if open && !close {
            self.updates.whats_new = Some(wn);
        }
    }

    /// Settings › Appearance & Behavior › System Settings › Updates.
    pub(super) fn page_updates(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        let checking = self.updates.check.is_some();
        let mut check = false;
        ui.heading("Updates");
        if store_managed_updates() {
            ui.label(rustpy_updates::STORE_UPDATES_MESSAGE);
            ui.label(format!(
                "Current version: RustPy {}",
                rustpy_updates::CURRENT_VERSION
            ));
            ui.add_space(6.0);
            ui.checkbox(
                &mut self.settings.updates.show_whats_new,
                "Show What's New after an IDE update",
            );
            return;
        }
        ui.horizontal(|ui| {
            let check_label = ui.checkbox(
                &mut self.settings.updates.check_automatically,
                "Check IDE updates for:",
            );
            let current = Channel::from_setting(&self.settings.updates.channel);
            ui.add_enabled_ui(self.settings.updates.check_automatically, |ui| {
                egui::ComboBox::from_id_salt("updates_channel")
                    .selected_text(current.label())
                    .show_ui(ui, |ui| {
                        for c in Channel::ALL {
                            if ui.selectable_label(c == current, c.label()).clicked() {
                                self.settings.updates.channel = c.setting().to_string();
                            }
                        }
                    })
                    .combo_labelled(&check_label);
            });
        });
        ui.label(egui::RichText::new("RustPy looks for a newer release on startup and then once a day. Early Access Program builds are previews of the next release.").color(theme.text_dim).small());
        ui.checkbox(
            &mut self.settings.updates.download_automatically,
            "Download verified updates automatically (ask before restarting)",
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!checking, egui::Button::new("Check for Updates"))
                .clicked()
            {
                check = true;
            }
            if checking {
                crate::a11y::spinner(ui);
            }
            ui.label(format!(
                "Last checked: {}",
                describe_last_check(self.settings.updates.last_check, &chrono::Local::now())
            ));
        });
        if let Some(outcome) = &self.updates.last_outcome {
            ui.label(egui::RichText::new(outcome).color(theme.text_dim));
        }
        ui.label(format!(
            "Current version: RustPy {}",
            rustpy_updates::CURRENT_VERSION
        ));
        ui.add_space(6.0);
        ui.checkbox(
            &mut self.settings.updates.show_whats_new,
            "Show What's New after an IDE update",
        );
        ui.add_space(10.0);
        ui.label(egui::RichText::new("Release repository").strong());
        ui.horizontal(|ui| {
            ui.label("GitHub repository:");
            ui.add(
                egui::TextEdit::singleline(&mut self.settings.updates.repository)
                    .hint_text(rustpy_updates::DEFAULT_REPOSITORY)
                    .desired_width(340.0),
            );
        });
        match rustpy_updates::releases_api_url(&self.settings.updates.repository) {
            Ok(url) => ui.label(egui::RichText::new(format!("owner/name, a repository URL or a GitHub Enterprise releases API URL. Releases are read from {url}")).color(theme.text_dim).small()),
            Err(e) => ui.label(egui::RichText::new(e.message()).color(theme.warning).small()),
        };
        if !self.settings.updates.ignored_versions.is_empty() {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(format!(
                    "Ignored updates: {}",
                    self.settings.updates.ignored_versions.join(", ")
                ));
                if ui
                    .small_button("Clear")
                    .on_hover_text("Offer these versions again")
                    .clicked()
                {
                    self.settings.updates.ignored_versions.clear();
                }
            });
        }
        if check {
            self.check_for_updates();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    #[test]
    fn unaccepted_restart_is_not_launched_and_cancel_keeps_the_staged_package() {
        let mut app = IdeApp::new_welcome(Settings::default());
        app.updates.prepared = Some(rustpy_updates::install::PreparedUpdate {
            version: "999.0.0".into(),
            artifact: "<staged-artifact>".into(),
            executable: "<running-executable>".into(),
            install: rustpy_updates::InstallKind::Archive,
            arguments: Vec::new(),
            binary: false,
            sha256: "a".repeat(64),
        });
        app.updates.restart_requested = true;
        app.finish_update_restart();
        assert!(app.pending_update_restart());
        app.cancel_pending_update_restart();
        assert!(!app.pending_update_restart());
        assert!(app.updates.prepared.is_some());
    }

    #[test]
    fn last_checked_reads_as_relative_day_and_time() {
        let tz = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        let now = tz.with_ymd_and_hms(2026, 10, 10, 15, 30, 0).unwrap();
        let at = |y, mo, d, h, mi| tz.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().timestamp();
        assert_eq!(describe_last_check(0, &now), "Never");
        assert_eq!(
            describe_last_check(at(2026, 10, 10, 9, 5), &now),
            "Today 09:05"
        );
        assert_eq!(
            describe_last_check(at(2026, 10, 9, 23, 59), &now),
            "Yesterday 23:59"
        );
        assert_eq!(
            describe_last_check(at(2026, 10, 3, 8, 0), &now),
            "Oct 3, 2026 08:00"
        );
    }

    #[test]
    fn whats_new_uses_the_changelog_section() {
        let log =
            "# Changelog\n\n## [Unreleased]\n\n- Next\n\n## [0.2.0] - 2026-10-01\n\n- Updates\n";
        assert_eq!(whats_new_markdown(log, "0.2.0", "https://r"), "- Updates");
        assert!(whats_new_markdown(log, "0.3.0", "https://r").ends_with("\n\n- Next"));
        assert!(whats_new_markdown("# Changelog\n", "0.3.0", "https://r").contains("https://r"));
        // The CHANGELOG.md embedded in this build has notes for this version.
        assert!(
            rustpy_updates::changelog::notes_for(CHANGELOG, rustpy_updates::CURRENT_VERSION)
                .is_some()
        );
    }

    #[test]
    fn calm_messages_mention_the_rate_limit_reset() {
        let m = calm_message(&FetchError::Refused {
            status: 403,
            rate_limited: true,
            reset_at: Some(1_791_623_831),
        });
        assert!(m.contains("try again after"), "{m}");
        assert!(!calm_message(&FetchError::NotFound)
            .to_lowercase()
            .contains("error"));
    }

    #[test]
    fn only_user_options_mark_the_updates_page_modified() {
        let old = Settings::default();
        let mut new = old.clone();
        new.updates.last_check = 42;
        new.updates.last_seen_version = "9.9.9".into();
        assert!(!page_modified(&old, &new));
        new.updates.channel = "eap".into();
        assert!(page_modified(&old, &new));
    }

    #[test]
    fn repository_changes_discard_old_results_without_recording_a_check() {
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Settings::default()
            },
            None,
        );
        app.settings.updates.repository = "new/repository".into();
        app.settings.updates.last_check = 42;
        let (tx, rx) = crossbeam_channel::bounded(1);
        let release = rustpy_updates::parse_releases(
            r#"[{"tag_name":"v999.0.0","html_url":"https://old.example/release"}]"#,
        )
        .unwrap();
        tx.send(Ok(release.clone())).unwrap();
        app.updates.check = Some(RunningCheck {
            rx,
            repository: rustpy_updates::releases_api_url("old/repository"),
            manual: false,
        });
        app.updates.scheduler.started();
        app.updates.last_outcome = Some("old repository result".into());
        app.updates.notification = Some(Box::new(release[0].clone()));
        app.poll_update_check(100);
        assert!(app.updates.check.is_none());
        assert!(!app.updates.scheduler.is_running());
        assert!(app.updates.last_outcome.is_none());
        assert!(app.updates.notification.is_none());
        assert_eq!(app.settings.updates.last_check, 42);

        // Even an empty old receiver must be dropped immediately. A waiting
        // manual check is restarted for the new setting, not promoted from the
        // stale automatic request. An invalid setting avoids network in tests.
        app.settings.updates.repository = "invalid".into();
        let (_tx, rx) = crossbeam_channel::bounded(1);
        app.updates.check = Some(RunningCheck {
            rx,
            repository: rustpy_updates::releases_api_url("old/repository"),
            manual: true,
        });
        app.updates.window = Some(UpdateWindow::Checking);
        app.poll_update_check(101);
        let current = app.updates.check.as_ref().unwrap();
        assert_eq!(
            current.repository,
            rustpy_updates::releases_api_url("invalid")
        );
        assert!(current.manual);
        assert!(matches!(app.updates.window, Some(UpdateWindow::Checking)));
        assert_eq!(app.settings.updates.last_check, 42);
    }
}
