//! Run tool window: process consoles with ANSI colors, file hyperlinks,
//! stdin input and session tabs (roadmap §3.2).

use std::path::{Path, PathBuf};

use rustpy_project::{RunConfig, RunConfigStore};
use rustpy_run::{OutputKind, RunSession, SessionStatus};

use crate::a11y::AccessibleCombo;
use crate::console_view::{ConsoleAction, ConsoleView};
use crate::run_anything::CommandSpec;
use crate::theme::UiTheme;

/// Events the run panel asks the app to perform.
pub enum RunPanelEvent {
    MenuAction(u64, crate::actions::Action),
    Rerun(u64),
    Stop(u64),
    Close(u64),
    OpenFile(PathBuf, usize),
    OpenUrl(String),
    SelectInProject(PathBuf),
    Attach(u64),
    Services(u64),
    Hide,
    Print(u64),
    Export(u64),
    Gist(u64),
}

/// What Rerun of a Run-window session starts.
#[derive(Clone, Debug, PartialEq)]
pub enum RerunPlan {
    /// A run configuration.
    Config(Box<RunConfig>),
    /// A Run Anything command (session name and command).
    Command(String, CommandSpec),
}

/// State of the run tool window.
pub struct RunPanel {
    pub sessions: Vec<RunSession>,
    pub active: Option<u64>,
    pub scroll_to_end: bool,
    pub soft_wrap: bool,
    tabs: std::collections::HashMap<u64, TabState>,
    pub settings: rustpy_settings::run_console::Settings,
    pub attention: Option<u64>,
    secondary: Option<u64>,
    confirm: Option<Confirmation>,
    approved_rerun: Option<u64>,
    announcement_owner: bool,
    focus_requested: bool,
    keyboard_session: Option<u64>,
    /// Whether the console, its find field or the input line has keyboard
    /// focus (the app then leaves Ctrl+F / F3 to the console).
    pub focused: bool,
    /// Run Anything, the Run…/Debug… choosers and the Stop picker.
    pub launcher: crate::run_anything::Launcher,
}

#[derive(Default)]
struct TabState {
    title: String,
    color: Option<[u8; 3]>,
    console: ConsoleView,
    external_links: Option<crate::external_tool_workspace::ConsoleLinks>,
    input: String,
    pinned: bool,
    paused: Option<rustpy_run::console_buffer::ConsoleBuffer>,
    interrupted: bool,
    close_after_stop: bool,
    background: bool,
    raw_terminal: bool,
}
#[derive(Clone, Copy)]
enum Confirmation {
    Rerun(u64),
    Terminate(u64),
}
fn terminal_key(key: egui::Key, modifiers: egui::Modifiers) -> Option<&'static str> {
    if modifiers.command || modifiers.ctrl {
        return match key {
            egui::Key::C => Some("\x03"),
            egui::Key::D => Some("\x04"),
            _ => None,
        };
    }
    match key {
        egui::Key::Enter => Some("\r"),
        egui::Key::Backspace => Some("\x7f"),
        egui::Key::Tab => Some("\t"),
        egui::Key::Escape => Some("\x1b"),
        egui::Key::ArrowUp => Some("\x1b[A"),
        egui::Key::ArrowDown => Some("\x1b[B"),
        egui::Key::ArrowRight => Some("\x1b[C"),
        egui::Key::ArrowLeft => Some("\x1b[D"),
        egui::Key::Home => Some("\x1b[H"),
        egui::Key::End => Some("\x1b[F"),
        egui::Key::Delete => Some("\x1b[3~"),
        _ => None,
    }
}

impl Default for RunPanel {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            active: None,
            scroll_to_end: true,
            soft_wrap: true,
            tabs: Default::default(),
            settings: Default::default(),
            attention: None,
            secondary: None,
            confirm: None,
            approved_rerun: None,
            announcement_owner: true,
            focus_requested: false,
            keyboard_session: None,
            focused: false,
            launcher: Default::default(),
        }
    }
}

impl RunPanel {
    pub(crate) fn set_external_tool_links(
        &mut self,
        id: u64,
        links: crate::external_tool_workspace::ConsoleLinks,
    ) {
        self.tabs.entry(id).or_default().external_links = Some(links);
    }
    pub(crate) fn claims_context_menu(
        &self,
        ctx: &egui::Context,
        key: egui::Key,
        mods: egui::Modifiers,
    ) -> bool {
        let id = self.keyboard_session.or(self.active);
        id.and_then(|id| self.tabs.get(&id))
            .is_some_and(|tab| tab.console.claims_context_menu(ctx, key, mods, id))
    }

    pub(crate) fn set_announcement_owner(&mut self, available: bool) {
        self.announcement_owner = available;
        for tab in self.tabs.values_mut() {
            tab.console.set_announcement_owner(available);
        }
    }

    pub fn add_session(&mut self, mut s: RunSession) {
        let limit = s
            .config
            .as_ref()
            .and_then(|cfg| cfg.console_buffer_kib)
            .unwrap_or(self.settings.buffer_kib);
        s.set_capture_limit(limit.saturating_mul(1024));
        // A failed spawn must preserve previous output, including finished sessions.
        if !self.settings.open_new_tab && !matches!(s.status, SessionStatus::Failed(_)) {
            if let Some(i) = self.sessions.iter().position(|x| {
                x.name == s.name
                    && !x.owns_process()
                    && !self.tabs.get(&x.id).is_some_and(|tab| tab.pinned)
            }) {
                self.sessions.remove(i);
            }
        }
        self.active = Some(s.id);
        self.sessions.push(s);
    }

    /// Remembers the configuration session `id` (already added) was started from.
    pub fn record_launch(&mut self, id: u64, cfg: RunConfig) {
        let live: Vec<u64> = self.sessions.iter().map(|s| s.id).collect();
        self.launcher.launched.retain(|k, _| live.contains(k));
        self.launcher.launched.insert(id, cfg);
    }

    /// What Rerun of session `id` starts; the app resolves it before it
    /// stops anything. A configuration session reruns its configuration as
    /// stored now (edits apply), else as it was launched, as a temporary
    /// configuration (see [`RunConfigStore::config_for_rerun`]: a temporary
    /// configuration may have been evicted since, and its name taken by a
    /// configuration for another script); a Run Anything session reruns its
    /// command. `None`: nothing to start.
    pub fn rerun_plan(&self, id: u64, store: &RunConfigStore) -> Option<RerunPlan> {
        let name = &self.sessions.iter().find(|s| s.id == id)?.name;
        if let Some(launched) = self.launcher.launched.get(&id) {
            return store
                .config_for_rerun(name, Some(launched))
                .map(Box::new)
                .map(RerunPlan::Config);
        }
        if let Some(spec) = self.launcher.commands.get(name) {
            return Some(RerunPlan::Command(name.clone(), spec.clone()));
        }
        store
            .config_for_rerun(name, None)
            .map(Box::new)
            .map(RerunPlan::Config)
    }

    /// Stops session `id` and closes its tab if it is still running (a
    /// rerun replaces it; a finished one is replaced by [`Self::add_session`]).
    pub fn replace_running(&mut self, id: u64) {
        if let Some(s) = self
            .sessions
            .iter_mut()
            .find(|s| s.id == id && s.owns_process())
        {
            s.stop();
            if self.can_replace(id) {
                self.sessions.retain(|s| s.id != id);
            }
        }
    }

    pub fn active_session_mut(&mut self) -> Option<&mut RunSession> {
        let id = self.active?;
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// Polls all sessions; returns true if any output changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for session in &mut self.sessions {
            let bytes = session
                .config
                .as_ref()
                .and_then(|cfg| cfg.console_buffer_kib)
                .unwrap_or(self.settings.buffer_kib)
                .saturating_mul(1024)
                .clamp(1024, 256 * 1024 * 1024);
            if session.console.max_bytes() != bytes {
                session.set_capture_limit(bytes);
            }
            let before = session.output_activity();
            changed |= session.poll();
            let after = session.output_activity();
            let tab = self.tabs.entry(session.id).or_default();
            let mut stdout = session
                .config
                .as_ref()
                .is_some_and(|cfg| cfg.show_console_on_stdout);
            let stderr = session
                .config
                .as_ref()
                .is_none_or(|cfg| cfg.show_console_on_stderr);
            // A PTY necessarily merges stdout/stderr into one terminal stream.
            stdout |= session.is_pty() && stderr;
            if !tab.background
                && ((stdout && before.0 != after.0) || (stderr && before.1 != after.1))
            {
                self.attention = Some(session.id);
            }
        }
        let close = self
            .sessions
            .iter()
            .filter(|session| {
                !session.owns_process()
                    && (self
                        .tabs
                        .get(&session.id)
                        .is_some_and(|tab| tab.close_after_stop)
                        || (self.settings.close_tabs_on_stop
                            && !self.tabs.get(&session.id).is_some_and(|tab| tab.pinned)))
            })
            .map(|session| session.id)
            .collect::<Vec<_>>();
        self.sessions.retain(|s| !close.contains(&s.id));
        self.tabs
            .retain(|id, _| self.sessions.iter().any(|s| s.id == *id));
        if self
            .active
            .is_some_and(|id| !self.sessions.iter().any(|s| s.id == id))
        {
            self.active = self.sessions.last().map(|s| s.id);
        }
        changed
    }
    pub fn modal_active(&self) -> bool {
        self.confirm.is_some()
    }
    pub fn request_console_focus(&mut self) {
        self.focus_requested = true;
    }
    pub fn can_replace(&self, id: u64) -> bool {
        !self.settings.open_new_tab
            && !self.tabs.get(&id).is_some_and(|tab| tab.pinned)
            && !self
                .sessions
                .iter()
                .any(|session| session.id == id && session.has_pending_daemon_retirement())
    }
    pub fn authorize_rerun(&mut self, id: u64) -> bool {
        if self.approved_rerun.take() == Some(id) {
            return true;
        }
        if self.settings.confirm_rerun
            && self.sessions.iter().any(|s| s.id == id && s.owns_process())
        {
            self.confirm = Some(Confirmation::Rerun(id));
            false
        } else {
            true
        }
    }
    pub fn request_stop(&mut self, id: u64) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|s| s.id == id && s.owns_process())
        {
            let tab = self.tabs.entry(id).or_default();
            if session.has_pending_daemon_retirement() {
                session.stop();
            } else if tab.interrupted {
                self.confirm = Some(Confirmation::Terminate(id));
            } else {
                session.interrupt();
                tab.interrupted = true;
            }
        }
    }
    pub fn request_close(&mut self, id: u64) {
        if self.sessions.iter().any(|s| s.id == id && s.owns_process()) {
            self.tabs.entry(id).or_default().close_after_stop = true;
            self.request_stop(id);
        } else {
            self.sessions.retain(|s| s.id != id);
            self.tabs.remove(&id);
            if self.active == Some(id) {
                self.active = self.sessions.last().map(|s| s.id);
            }
        }
    }
    pub fn background(&mut self, id: u64) {
        self.tabs.entry(id).or_default().background = true;
    }
    pub fn retained_text(&self, id: u64) -> Option<String> {
        let session = self.sessions.iter().find(|s| s.id == id)?;
        Some(
            self.tabs
                .get(&id)
                .and_then(|tab| tab.paused.as_ref())
                .unwrap_or(&session.console)
                .text(),
        )
    }

    /// Error output of the most recent session that failed (for "Fix with AI").
    pub fn last_error_text(&self) -> Option<String> {
        let s = self.sessions.iter().rev().find(|s| {
            matches!(s.status, SessionStatus::Exited(c) if c != 0)
                || matches!(s.status, SessionStatus::Failed(_))
        })?;
        let err: String = s
            .console
            .lines_from(0)
            .filter(|(_, l)| l.kind == OutputKind::Stderr)
            .map(|(_, l)| format!("{}\n", l.text))
            .collect();
        let tail: String = err
            .chars()
            .rev()
            .take(6000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if tail.trim().is_empty() {
            None
        } else {
            Some(tail)
        }
    }

    pub fn process_stamps(&self) -> Vec<String> {
        self.sessions
            .iter()
            .filter(|session| session.owns_process())
            .map(|session| session.lifecycle_stamp())
            .collect()
    }
    pub fn has_running(&self) -> bool {
        self.sessions.iter().any(|s| s.owns_process())
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        mono: egui::FontId,
    ) -> Vec<RunPanelEvent> {
        let mut events = Vec::new();
        if self.sessions.is_empty() {
            ui.label("Nothing has been run yet. Use Run ▶ (Shift+F10) or the gutter icons.");
            return events;
        }
        ui.horizontal_wrapped(|ui| {
            for session in &self.sessions {
                let pinned = self.tabs.get(&session.id).is_some_and(|tab| tab.pinned);
                let label = format!(
                    "{}{}{}",
                    if session.is_running() { "▶ " } else { "" },
                    if pinned { "📌 " } else { "" },
                    self.tabs
                        .get(&session.id)
                        .filter(|tab| !tab.title.is_empty())
                        .map(|tab| tab.title.as_str())
                        .unwrap_or(&session.name)
                );
                let response = ui.selectable_label(
                    self.active == Some(session.id),
                    egui::RichText::new(label).color(
                        if let Some(color) = self.tabs.get(&session.id).and_then(|tab| tab.color) {
                            egui::Color32::from_rgb(color[0], color[1], color[2])
                        } else if session.is_running() {
                            theme.success
                        } else {
                            theme.text
                        },
                    ),
                );
                response.context_menu(|ui| {
                    if crate::custom_menus::render(ui, "run.popup", |ui, action| {
                        if crate::custom_menus::action_button(ui, action).clicked() {
                            events.push(RunPanelEvent::MenuAction(session.id, action));
                            ui.close();
                        }
                    }) {
                        return;
                    }
                    let tab = self.tabs.entry(session.id).or_default();
                    ui.checkbox(&mut tab.pinned, "Pin tab");
                    ui.label("Rename tab");
                    ui.text_edit_singleline(&mut tab.title);
                    let mut color = tab.color.unwrap_or([120, 160, 220]);
                    if ui.color_edit_button_srgb(&mut color).changed() {
                        tab.color = Some(color);
                    }
                    if ui.button("Reset color").clicked() {
                        tab.color = None;
                    }
                    if ui.button("Close Other Tabs (finished only)").clicked() {
                        events.extend(
                            self.sessions
                                .iter()
                                .filter(|other| {
                                    other.id != session.id
                                        && !other.owns_process()
                                        && !self.tabs.get(&other.id).is_some_and(|tab| tab.pinned)
                                })
                                .map(|other| RunPanelEvent::Close(other.id)),
                        );
                        ui.close();
                    }
                    if ui.button("Close").clicked() {
                        events.push(RunPanelEvent::Close(session.id));
                        ui.close();
                    }
                });
                if response.clicked() {
                    self.active = Some(session.id);
                    self.tabs.entry(session.id).or_default().background = false;
                }
                if response.middle_clicked() {
                    events.push(RunPanelEvent::Close(session.id));
                }
            }
        });
        let Some(active) = self.active else {
            return events;
        };
        ui.horizontal(|ui| {
            ui.menu_button("Console options", |ui| self.settings_ui(ui));
            egui::ComboBox::from_id_salt("run-side-by-side")
                .selected_text(if self.secondary.is_some() {
                    "Side by side"
                } else {
                    "Single console"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.secondary, None, "Single console");
                    for session in &self.sessions {
                        if session.id != active {
                            ui.selectable_value(
                                &mut self.secondary,
                                Some(session.id),
                                &session.name,
                            );
                        }
                    }
                })
                .combo_named(crate::i18n::tr_static("Console layout"));
            if ui.button("Reset to default layout").clicked() {
                self.secondary = None;
                self.scroll_to_end = true;
                self.soft_wrap = true;
                self.settings.show_command_line = true;
            }
            if ui.button("Processes in background").clicked() {
                self.background(active);
                events.push(RunPanelEvent::Hide);
            }
            if ui.button("Services").clicked() {
                events.push(RunPanelEvent::Services(active));
            }
        });
        let focused = if let Some(other) = self
            .secondary
            .filter(|id| *id != active && self.sessions.iter().any(|s| s.id == *id))
        {
            ui.columns(2, |columns| {
                let first =
                    self.session_ui(&mut columns[0], theme, mono.clone(), active, &mut events);
                let second = self.session_ui(&mut columns[1], theme, mono, other, &mut events);
                first || second
            })
        } else {
            self.session_ui(ui, theme, mono, active, &mut events)
        };
        self.focused = focused;
        if let Some(confirm) = self.confirm {
            let mut answer = None;
            egui::Modal::new(egui::Id::new("run-console-confirm")).show(ui.ctx(), |ui| {
                ui.heading(match confirm {
                    Confirmation::Rerun(_) => "Confirm rerun",
                    Confirmation::Terminate(_) => "Terminate?",
                });
                ui.label(match confirm {
                    Confirmation::Rerun(_) => {
                        "Stop this running process and rerun its configuration?"
                    }
                    Confirmation::Terminate(_) => {
                        "The process has already received an interrupt. Force termination?"
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .button(match confirm {
                            Confirmation::Rerun(_) => "Rerun",
                            Confirmation::Terminate(_) => "Terminate",
                        })
                        .clicked()
                    {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                });
            });
            if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
            if let Some(accepted) = answer {
                self.confirm = None;
                if accepted {
                    match confirm {
                        Confirmation::Rerun(id) => {
                            self.approved_rerun = Some(id);
                            events.push(RunPanelEvent::Rerun(id));
                        }
                        Confirmation::Terminate(id) => {
                            if let Some(session) = self.sessions.iter_mut().find(|s| s.id == id) {
                                session.stop();
                            }
                        }
                    }
                }
            }
        }
        self.settings.soft_wrap = self.soft_wrap;
        self.settings.scroll_to_end = self.scroll_to_end;
        events
    }
    fn session_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        mono: egui::FontId,
        id: u64,
        events: &mut Vec<RunPanelEvent>,
    ) -> bool {
        let Some(index) = self.sessions.iter().position(|s| s.id == id) else {
            return false;
        };
        let mut tab = self.tabs.remove(&id).unwrap_or_default();
        let session = &mut self.sessions[index];
        let running = session.owns_process();
        if session.has_pending_daemon_retirement() && !session.is_running() {
            ui.colored_label(theme.error, "Docker cleanup unresolved. This tab remains owned until daemon and local process retirement are confirmed.");
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("↻ Rerun").clicked() {
                events.push(RunPanelEvent::Rerun(id));
            }
            if ui
                .add_enabled(
                    running,
                    egui::Button::new(if tab.interrupted {
                        "■ Terminate…"
                    } else {
                        "■ Stop"
                    }),
                )
                .clicked()
            {
                events.push(RunPanelEvent::Stop(id));
            }
            if ui.button("Clear All").clicked() {
                session.clear_output();
                tab.paused = None;
            }
            ui.checkbox(&mut tab.pinned, "Pin tab");
            let mut paused = tab.paused.is_some();
            if ui.checkbox(&mut paused, "Pause output").changed() {
                tab.paused = paused.then(|| session.console.clone());
            }
            ui.checkbox(&mut self.scroll_to_end, "Scroll to the end");
            ui.checkbox(&mut self.soft_wrap, "Use soft wraps");
            if ui.button("Find…").clicked() {
                tab.console
                    .open_find(tab.paused.as_ref().unwrap_or(&session.console));
            }
            if ui.button("Print").clicked() {
                events.push(RunPanelEvent::Print(id));
            }
            if ui.button("Export to file…").clicked() {
                events.push(RunPanelEvent::Export(id));
            }
            if ui.button("Create Gist…").clicked() {
                events.push(RunPanelEvent::Gist(id));
            }
            if ui
                .add_enabled(
                    session.is_running() && session.process_id().is_some(),
                    egui::Button::new("Attach debugger"),
                )
                .clicked()
            {
                events.push(RunPanelEvent::Attach(id));
            }
            if ui.button("Close").clicked() {
                events.push(RunPanelEvent::Close(id));
            }
        });
        if self.settings.show_command_line {
            ui.label(
                egui::RichText::new(&session.command_line)
                    .monospace()
                    .color(theme.text_dim),
            );
        }
        let status = match &session.status {
            SessionStatus::Running => "running".into(),
            SessionStatus::Exited(code) => format!("Process finished with exit code {code}"),
            SessionStatus::Killed => "Process terminated".into(),
            SessionStatus::Failed(error) => error.clone(),
        };
        ui.label(
            egui::RichText::new(format!(
                "{} — {status} ({:.1}s)",
                session.name,
                session.elapsed().as_secs_f32()
            ))
            .color(theme.text_dim),
        );
        if session.is_pty() {
            ui.checkbox(&mut tab.raw_terminal, "Send terminal keys directly");
            let width = ui.fonts_mut(|fonts| fonts.glyph_width(&mono, 'M')).max(1.0);
            let height = ui.fonts_mut(|fonts| fonts.row_height(&mono)).max(1.0);
            session.resize_pty(
                (ui.available_width() / width).max(20.0) as u16,
                (ui.available_height() / height).max(4.0) as u16,
            );
        }
        let mut console_settings = self.settings.clone();
        if let Some(links) = &tab.external_links {
            let mut filters = links.filters.clone();
            filters.extend(console_settings.link_filters);
            console_settings.link_filters = filters;
        }
        tab.console.configure(&console_settings);
        tab.console.set_announcement_owner(self.announcement_owner);
        let buffer = tab.paused.as_ref().unwrap_or(&session.console);
        let keys = tab.console.focused || (self.secondary.is_none() && self.focused);
        let actions = tab.console.ui(
            ui,
            theme,
            mono,
            id,
            buffer,
            self.soft_wrap,
            self.scroll_to_end,
            30.0,
            keys,
        );
        for action in actions {
            match action {
                ConsoleAction::OpenFile(path, line) => {
                    if let Some(path) = tab
                        .external_links
                        .as_ref()
                        .map(|links| crate::external_tool_workspace::output_path(&path, links))
                        .unwrap_or(Some(path))
                    {
                        events.push(RunPanelEvent::OpenFile(path, line));
                    }
                }
                ConsoleAction::OpenUrl(url) => events.push(RunPanelEvent::OpenUrl(url)),
                ConsoleAction::SelectInProject(path) => {
                    if let Some(path) = tab
                        .external_links
                        .as_ref()
                        .map(|links| crate::external_tool_workspace::output_path(&path, links))
                        .unwrap_or(Some(path))
                    {
                        events.push(RunPanelEvent::SelectInProject(path));
                    }
                }
                ConsoleAction::ClearAll => {
                    session.clear_output();
                    tab.paused = None;
                }
            }
        }
        if tab.raw_terminal && session.is_pty() && tab.console.terminal_input_focus() {
            for event in ui.input(|input| input.events.clone()) {
                match event {
                    egui::Event::Text(text) | egui::Event::Paste(text) => {
                        session.write_stdin(&text)
                    }
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if let Some(bytes) = terminal_key(key, modifiers) {
                            session.write_stdin(bytes);
                        }
                    }
                    _ => {}
                }
            }
        }
        let request_focus = self.focus_requested && self.active == Some(id);
        let input = ui
            .horizontal(|ui| {
                ui.label("›");
                let response = ui.add_enabled(
                    running && session.accepts_stdin(),
                    egui::TextEdit::singleline(&mut tab.input)
                        .desired_width(f32::INFINITY)
                        .hint_text(if session.accepts_stdin() {
                            "Type input and press Enter"
                        } else {
                            "Standard input is redirected"
                        }),
                );
                if request_focus && running && session.accepts_stdin() {
                    response.request_focus();
                }
                if running
                    && (response.has_focus() || response.lost_focus())
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    session.write_stdin(&format!("{}\n", std::mem::take(&mut tab.input)));
                    response.request_focus();
                }
                response.has_focus()
            })
            .inner;
        if request_focus {
            if !running || !session.accepts_stdin() {
                ui.memory_mut(|memory| {
                    memory.request_focus(egui::Id::new(("run_console_area", id)))
                });
            }
            self.focus_requested = false;
        }
        let focused = tab.console.focused || input;
        if focused {
            self.keyboard_session = Some(id);
        }
        self.tabs.insert(id, tab);
        focused
    }
    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(
            &mut self.settings.show_command_line,
            "Show command line header",
        );
        ui.checkbox(
            &mut self.settings.confirm_rerun,
            "Confirm rerun of a running process",
        );
        ui.checkbox(
            &mut self.settings.open_new_tab,
            "Open every run in a new tab",
        );
        ui.checkbox(
            &mut self.settings.close_tabs_on_stop,
            "Close unpinned tabs on stop",
        );
        ui.horizontal(|ui| {
            ui.label("Override console cycle buffer size (KiB)");
            ui.add(egui::DragValue::new(&mut self.settings.buffer_kib).range(1..=262144));
        });
        ui.horizontal(|ui| {
            ui.label("Console font size");
            ui.add(egui::DragValue::new(&mut self.settings.font_size).range(8.0..=40.0));
        });
        ui.checkbox(&mut self.settings.fold_output, "Fold lines containing");
        let mut patterns = self.settings.fold_contains.join("\n");
        if ui.text_edit_multiline(&mut patterns).changed() {
            self.settings.fold_contains = patterns
                .lines()
                .filter(|s| !s.is_empty())
                .take(64)
                .map(str::to_string)
                .collect();
        }
        ui.label("Custom link filters: regex captures named path/line, or groups 1/2");
        let mut filters = self.settings.link_filters.join("\n");
        if ui.text_edit_multiline(&mut filters).changed() {
            self.settings.link_filters = filters
                .lines()
                .filter(|s| !s.is_empty())
                .take(32)
                .map(str::to_string)
                .collect();
        }
        for pattern in &self.settings.link_filters {
            if let Err(error) = regex::RegexBuilder::new(pattern)
                .size_limit(64 * 1024)
                .build()
            {
                ui.colored_label(ui.visuals().error_fg_color, error.to_string());
            }
        }
        ui.checkbox(
            &mut self.settings.custom_colors,
            "Custom console colors (otherwise editor color scheme)",
        );
        if self.settings.custom_colors {
            for (label, color) in [
                ("Text", &mut self.settings.foreground),
                ("Stderr", &mut self.settings.stderr),
                ("System", &mut self.settings.system),
                ("Background", &mut self.settings.background),
            ] {
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr_static(label));
                    ui.color_edit_button_srgb(color);
                });
            }
        }
    }
}

/// Resolves a path from console output relative to the project root.
pub fn resolve_output_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn long_running(id: u64, name: &str) -> RunSession {
        RunSession::spawn_command(
            id,
            name,
            Path::new("sleep"),
            &["30".into()],
            Path::new("/"),
            &Default::default(),
        )
    }

    /// Rerun of a running session whose temporary configuration was evicted
    /// by five newer context launches reruns the configuration it was
    /// launched with (it used to stop the process, close its tab and start
    /// nothing); a session with nothing to rerun keeps running.
    #[cfg(unix)]
    #[test]
    fn rerun_is_resolved_before_anything_stops() {
        let mut store = RunConfigStore::default();
        let server = RunConfig::for_script(Path::new("/proj/server.py"));
        store.upsert(server.clone());
        let mut panel = RunPanel::default();
        panel.add_session(long_running(1, &server.name));
        panel.record_launch(1, server.clone());
        for n in 0..5 {
            store.upsert(RunConfig::for_script(Path::new(&format!("/proj/a{n}.py"))));
        }
        assert!(
            !store.configs.iter().any(|c| c.name == server.name),
            "the temporary configuration was evicted"
        );
        assert_eq!(
            panel.rerun_plan(1, &store),
            Some(RerunPlan::Config(Box::new(server.clone())))
        );
        // The configuration as stored now wins (edits apply).
        let mut edited = server.clone();
        edited.parameters = "--port 81".into();
        store.upsert(edited.clone());
        assert_eq!(
            panel.rerun_plan(1, &store),
            Some(RerunPlan::Config(Box::new(edited)))
        );
        // A Run Anything session reruns its command.
        let spec = CommandSpec {
            program: PathBuf::from("pip"),
            args: vec!["list".into()],
            cwd: PathBuf::from("/proj"),
            env: Vec::new(),
        };
        panel
            .launcher
            .commands
            .insert("pip list".into(), spec.clone());
        panel.add_session(long_running(2, "pip list"));
        assert_eq!(
            panel.rerun_plan(2, &store),
            Some(RerunPlan::Command("pip list".into(), spec))
        );
        // Nothing to rerun: no plan, so the app leaves the running session alone.
        panel.add_session(long_running(3, "orphan"));
        assert_eq!(panel.rerun_plan(3, &store), None);
        assert_eq!(panel.rerun_plan(99, &store), None);
        // Replacing a running session stops it and closes its tab.
        panel.add_session(long_running(4, "closed"));
        panel.record_launch(4, server.clone());
        panel.replace_running(4);
        assert!(panel.sessions.iter().all(|s| s.id != 4));
        assert!(panel.sessions.iter().any(|s| s.id == 3 && s.is_running()));
        // Launch records are dropped with their sessions.
        panel.record_launch(3, server.clone());
        assert_eq!(
            panel
                .launcher
                .launched
                .keys()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            [1, 3].into()
        );
        for s in &mut panel.sessions {
            s.stop();
        }
    }

    /// Rerun of a session whose evicted temporary configuration's name now
    /// belongs to a context configuration for another script (a/main.py and
    /// b/main.py) reruns the session's own script; it used to stop the
    /// session and start the other script. A deleted permanent configuration
    /// reruns as a temporary one, so the rerun does not store it again.
    #[cfg(unix)]
    #[test]
    fn rerun_runs_the_sessions_own_configuration() {
        let root = Path::new("/proj");
        let mut store = RunConfigStore::default();
        let a = RunConfig::for_script(Path::new("/proj/a/main.py"));
        store.upsert(a.clone());
        let mut panel = RunPanel::default();
        panel.add_session(long_running(1, &a.name));
        panel.record_launch(1, a.clone());
        for n in 0..5 {
            store.upsert(RunConfig::for_script(Path::new(&format!("/proj/s{n}.py"))));
        }
        let b = store.resolve_temporary(
            RunConfig::for_script(Path::new("/proj/b/main.py")),
            true,
            root,
        );
        assert_eq!(b.name, a.name, "the evicted name is free again");
        store.upsert(b.clone());
        let plan_a = panel.rerun_plan(1, &store);
        let srv = RunConfig {
            name: "srv".into(),
            temporary: false,
            ..RunConfig::for_script(Path::new("/proj/srv.py"))
        };
        store.upsert(srv.clone());
        panel.add_session(long_running(2, "srv"));
        panel.record_launch(2, srv.clone());
        store.configs.retain(|c| c.name != "srv");
        let plan_srv = panel.rerun_plan(2, &store);
        // Edits of a permanent configuration apply, to its script too.
        let moved = RunConfig {
            kind: rustpy_project::RunConfigKind::Python {
                script: PathBuf::from("/proj/v2/srv.py"),
            },
            ..srv.clone()
        };
        store.upsert(moved.clone());
        let plan_moved = panel.rerun_plan(2, &store);
        for s in &mut panel.sessions {
            s.stop();
        }
        assert_eq!(
            plan_a,
            Some(RerunPlan::Config(Box::new(a))),
            "reruns the session's own script"
        );
        assert_eq!(
            plan_srv,
            Some(RerunPlan::Config(Box::new(RunConfig {
                temporary: true,
                ..srv
            }))),
            "a deleted configuration reruns as a temporary one"
        );
        assert_eq!(plan_moved, Some(RerunPlan::Config(Box::new(moved))));
    }
    #[test]
    fn terminal_keys_and_console_preferences_are_explicit() {
        assert_eq!(
            terminal_key(egui::Key::ArrowUp, egui::Modifiers::NONE),
            Some("\x1b[A")
        );
        assert_eq!(
            terminal_key(egui::Key::C, egui::Modifiers::CTRL),
            Some("\x03")
        );
        assert_eq!(terminal_key(egui::Key::F, egui::Modifiers::CTRL), None);
        let mut panel = RunPanel::default();
        panel.settings.open_new_tab = true;
        assert!(!panel.can_replace(1));
        panel.settings.open_new_tab = false;
        panel.tabs.entry(1).or_default().pinned = true;
        assert!(!panel.can_replace(1));
        panel.tabs.get_mut(&1).unwrap().pinned = false;
        assert!(panel.can_replace(1));
    }
}
