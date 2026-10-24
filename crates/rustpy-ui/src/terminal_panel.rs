//! Terminal tool window: renders vt100 screens, forwards keyboard input,
//! manages multiple shell tabs (roadmap §6 "Terminal").

use crate::a11y::AccessibleCombo;
use std::path::Path;

use egui::{Align2, Color32, EventFilter, PointerButton, Pos2, Rect, Sense, Stroke, Vec2};
#[cfg(test)]
use rustpy_terminal::SCROLLBACK_LINES;
use rustpy_terminal::{vt100, BufferPos, Key, Mods, Terminal};

use crate::text_selection::{unit_span, SelectUnit, TextPos, TextSelection};
use crate::theme::UiTheme;

pub struct TerminalTab {
    last_bells: u64,
    input_line: String,
    pub group: u64,
    pub vertical: bool,
    pub profile: String,
    pub renamed: bool,
    pub terminal: Terminal,
    pub title: String,
    pub id: u64,
    last_generation: u64,
    /// Mouse selection in buffer coordinates (lines counted from the oldest
    /// scrollback row, see [`BufferPos`]), so it stays on its text while the
    /// view scrolls.
    selection: Option<TermSelection>,
}

/// A selection plus the buffer state its coordinates refer to.
#[derive(Clone, Copy, Debug)]
struct TermSelection {
    sel: TextSelection,
    alt_screen: bool,
    size: (u16, u16),
    /// Output generation last seen with the selection.
    generation: u64,
}

impl TermSelection {
    /// Switching screens or resizing moves the text under the selection; so
    /// does new output once the scrollback is full (its oldest rows are then
    /// dropped and every line number shifts).
    #[cfg(test)]
    fn still_valid(
        &self,
        alt_screen: bool,
        size: (u16, u16),
        history: usize,
        generation: u64,
    ) -> bool {
        self.alt_screen == alt_screen
            && self.size == size
            && (history < SCROLLBACK_LINES || self.generation == generation)
    }
}

/// A clipboard shortcut as egui reports it: on Linux and Windows winit turns
/// Ctrl+C / Ctrl+X (also with Shift) into these events instead of key presses;
/// on macOS they come from Cmd+C / Cmd+X.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardShortcut {
    Copy,
    Cut,
}

/// What the terminal does with a [`ClipboardShortcut`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutAction {
    /// Copy the selected text; `keep` leaves it selected.
    CopySelection {
        keep: bool,
    },
    /// Send Ctrl+`char` to the program (`^C` interrupts it, `^X` e.g. leaves nano).
    SendCtrl(char),
    Ignore,
}

/// Ctrl+C copies the selection if there is one (and clears it, so a second
/// Ctrl+C interrupts) and otherwise interrupts the running program; Ctrl+X
/// always goes to the program. Ctrl+Shift+C only copies. On macOS Cmd+C only
/// copies and Cmd+X does nothing (Ctrl+C arrives there as an ordinary key).
pub fn copy_or_interrupt(
    shortcut: ClipboardShortcut,
    has_selection: bool,
    shift: bool,
    macos: bool,
) -> ShortcutAction {
    match shortcut {
        ClipboardShortcut::Copy if has_selection => ShortcutAction::CopySelection {
            keep: shift || macos,
        },
        ClipboardShortcut::Copy if shift || macos => ShortcutAction::Ignore,
        ClipboardShortcut::Copy => ShortcutAction::SendCtrl('c'),
        ClipboardShortcut::Cut if shift || macos => ShortcutAction::Ignore,
        ClipboardShortcut::Cut => ShortcutAction::SendCtrl('x'),
    }
}

impl TerminalTab {
    fn has_selection(&self) -> bool {
        self.selection.is_some_and(|s| !s.sel.is_empty())
    }

    /// Text of the current selection (`None` when nothing is selected).
    fn selected_text(&self) -> Option<String> {
        let s = self.selection.filter(|s| !s.sel.is_empty())?.sel;
        let (a, b) = s.range();
        let pos = |p: TextPos| BufferPos::new(p.line, p.col.min(usize::from(u16::MAX)) as u16);
        Some(self.terminal.text_between(pos(a), pos(b)))
    }

    /// Selects the whole buffer: scrollback and screen.
    fn select_all(&mut self) {
        let generation = self.terminal.generation();
        let (cols, rows) = (self.terminal.cols, self.terminal.rows);
        let (lines, alt_screen) = self
            .terminal
            .with_buffer(|s, hist| (hist + usize::from(s.size().0), s.alternate_screen()));
        let span = (
            TextPos::new(0, 0),
            TextPos::new(lines.saturating_sub(1), usize::MAX),
        );
        self.selection = Some(TermSelection {
            sel: TextSelection::new(SelectUnit::Char, span),
            alt_screen,
            size: (cols, rows),
            generation,
        });
    }

    /// Applies a clipboard shortcut (see [`copy_or_interrupt`]).
    fn clipboard_shortcut(
        &mut self,
        ctx: &egui::Context,
        shortcut: ClipboardShortcut,
        shift: bool,
    ) {
        match copy_or_interrupt(
            shortcut,
            self.has_selection(),
            shift,
            crate::actions::shortcuts::mac_keymap(),
        ) {
            ShortcutAction::CopySelection { keep } => {
                if let Some(text) = self.selected_text() {
                    ctx.copy_text(text);
                }
                if !keep {
                    self.selection = None;
                }
            }
            ShortcutAction::SendCtrl(c) => {
                self.selection = None;
                self.terminal.send_key(
                    Key::Char(c),
                    Mods {
                        ctrl: true,
                        ..Default::default()
                    },
                );
                self.terminal.scroll_to_bottom();
            }
            ShortcutAction::Ignore => {}
        }
    }
}

pub struct TerminalPanel {
    pub preferences: rustpy_settings::terminal::Preferences,
    pub detached: bool,
    pub events: Vec<TerminalEvent>,
    search: String,
    searching: bool,
    search_index: usize,
    rename: Option<(u64, String)>,
    history: Vec<String>,
    command: String,
    history_popup: bool,
    drag_tab: Option<u64>,
    pub tabs: Vec<TerminalTab>,
    pub active: usize,
    next_id: u64,
    pub focused: bool,
    announcement_owner: bool,
    output_focus_id: Option<(egui::Id, u64)>,
    want_focus: bool,
    auto_shell: bool,
    /// Shown in the empty panel while `auto_shell` is off: why no shell was started.
    idle_note: Option<String>,
    /// Shell override from settings (None = system default).
    pub shell: Option<String>,
    /// Custom command for the "+" button (None = default shell).
    pub new_tab_command: Option<(String, String, Vec<String>)>,
    /// Multi-line paste waiting for confirmation.
    pending_paste: Option<String>,
    pending_paste_tab: Option<u64>,
    /// Modifiers held at the end of the previous frame (egui only reports the
    /// state after a frame's events; Ctrl+C vs Ctrl+Shift+C needs it per event).
    last_modifiers: egui::Modifiers,
}

impl Default for TerminalPanel {
    fn default() -> Self {
        Self {
            preferences: Default::default(),
            detached: false,
            events: Vec::new(),
            search: String::new(),
            searching: false,
            search_index: 0,
            rename: None,
            history: Vec::new(),
            command: String::new(),
            history_popup: false,
            drag_tab: None,
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            focused: false,
            announcement_owner: true,
            output_focus_id: None,
            want_focus: false,
            auto_shell: true,
            idle_note: None,
            shell: None,
            new_tab_command: None,
            pending_paste: None,
            pending_paste_tab: None,
            last_modifiers: egui::Modifiers::NONE,
        }
    }
}

#[derive(Clone, Debug)]
pub enum TerminalEvent {
    OpenFile(std::path::PathBuf, usize),
    OpenUrl(String),
    SmartCommand(String, std::path::PathBuf),
    PythonConsole,
}

const ANSI: [Color32; 16] = [
    Color32::from_rgb(0x00, 0x00, 0x00),
    Color32::from_rgb(0xCD, 0x31, 0x31),
    Color32::from_rgb(0x0D, 0xBC, 0x79),
    Color32::from_rgb(0xE5, 0xE5, 0x10),
    Color32::from_rgb(0x24, 0x72, 0xC8),
    Color32::from_rgb(0xBC, 0x3F, 0xBC),
    Color32::from_rgb(0x11, 0xA8, 0xCD),
    Color32::from_rgb(0xE5, 0xE5, 0xE5),
    Color32::from_rgb(0x66, 0x66, 0x66),
    Color32::from_rgb(0xF1, 0x4C, 0x4C),
    Color32::from_rgb(0x23, 0xD1, 0x8B),
    Color32::from_rgb(0xF5, 0xF5, 0x43),
    Color32::from_rgb(0x3B, 0x8E, 0xEA),
    Color32::from_rgb(0xD6, 0x70, 0xD6),
    Color32::from_rgb(0x29, 0xB8, 0xDB),
    Color32::from_rgb(0xFF, 0xFF, 0xFF),
];

fn color(c: vt100::Color, default: Color32) -> Color32 {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) if i < 16 => ANSI[i as usize],
        vt100::Color::Idx(i) if i < 232 => {
            let i = i - 16;
            let (r, g, b) = (i / 36, (i / 6) % 6, i % 6);
            let f = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(f(r), f(g), f(b))
        }
        vt100::Color::Idx(i) => Color32::from_gray(8 + (i - 232) * 10),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

impl TerminalPanel {
    /// The App supplies the actual modal owner before painting this panel.
    pub(crate) fn claims_context_menu(
        &self,
        ctx: &egui::Context,
        key: egui::Key,
        mods: egui::Modifiers,
    ) -> bool {
        key == egui::Key::F10
            && mods == egui::Modifiers::SHIFT
            && self.pending_paste.is_none()
            && self.active < self.tabs.len()
            && !egui::Popup::is_any_open(ctx)
            && self.output_focus_id.is_some_and(|(id, tab)| {
                self.tabs[self.active].id == tab && ctx.memory(|memory| memory.has_focus(id))
            })
    }

    pub(crate) fn set_announcement_owner(&mut self, available: bool) {
        self.announcement_owner = available;
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Opens a new shell tab in `cwd` with extra environment variables.
    pub fn open(&mut self, cwd: &Path, env: &[(String, String)]) -> anyhow::Result<()> {
        let profile = self
            .preferences
            .profiles
            .iter()
            .find(|p| p.name == self.preferences.selected_profile)
            .cloned()
            .unwrap_or_default();
        let directory = cwd.to_path_buf();
        let mut extra = env.to_vec();
        extra.extend(
            self.preferences
                .environment
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        let target = rustpy_terminal::session::Target {
            environment: self
                .preferences
                .environment
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            kind: profile.target_kind.clone(),
            name: profile.target.clone(),
            shell: if profile.shell.is_empty() {
                self.shell.clone().unwrap_or_default()
            } else {
                profile.shell.clone()
            },
            args: if profile.arguments.is_empty() {
                self.preferences.arguments.clone()
            } else {
                profile.arguments.clone()
            },
            directory: profile.directory.clone(),
            user: profile.ssh_user.clone(),
            port: profile.ssh_port,
            key: profile.ssh_key.clone(),
        };
        let mut plan = rustpy_terminal::session::plan(&target, &directory)?;
        let integrate = self.preferences.shell_integration && !plan.remote;
        let shell_name = Path::new(&plan.program)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let startup = if integrate {
            activation_command(&shell_name, &extra)
        } else {
            None
        };
        // Integration is injected through generated startup files where the shell
        // supports it, so nothing is echoed; typed input is the fallback only.
        let injected = if integrate {
            let dir = rustpy_settings::Settings::system_dir().join("terminal");
            match rustpy_terminal::integration::launch(
                &plan.program,
                &plan.args,
                &self.preferences.prompt_prefix,
                startup.as_deref(),
                &dir,
            ) {
                Ok(launch) => launch,
                Err(err) => {
                    log::warn!("terminal: cannot write shell integration files: {err}");
                    None
                }
            }
        } else {
            None
        };
        if let Some(launch) = &injected {
            plan.args = launch.args.clone();
            extra.extend(launch.env.iter().cloned());
        }
        let mut t = Terminal::spawn_configured(
            &plan.program,
            &plan.args,
            &plan.cwd,
            &extra,
            120,
            30,
            self.preferences.scrollback,
        )?;
        if integrate && injected.is_none() {
            if let Some(command) = &startup {
                t.write(format!("{command}\r").as_bytes());
            }
            if let Some(setup) = rustpy_terminal::integration::shell_setup(
                &plan.program,
                &self.preferences.prompt_prefix,
            ) {
                t.write(setup.as_bytes());
            }
        }
        let title = if self.preferences.tab_name.is_empty() {
            profile.name.clone()
        } else {
            self.preferences.tab_name.clone()
        };
        self.push_tab(t, title);
        Ok(())
    }

    /// Opens a tab running `program args` (used for the Python console).
    pub fn open_command(
        &mut self,
        title: &str,
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &[(String, String)],
    ) -> anyhow::Result<()> {
        let t = Terminal::spawn_command(program, args, cwd, env, 120, 30)?;
        self.push_tab(t, title.to_string());
        Ok(())
    }

    fn push_tab(&mut self, t: Terminal, title: String) {
        self.next_id += 1;
        self.tabs.push(TerminalTab {
            last_bells: 0,
            group: self.next_id,
            vertical: false,
            profile: self.preferences.selected_profile.clone(),
            renamed: false,
            terminal: t,
            title,
            id: self.next_id,
            last_generation: 0,
            selection: None,
            input_line: String::new(),
        });
        self.active = self.tabs.len() - 1;
        self.want_focus = true;
    }

    /// Whether the panel auto-opens a plain shell when empty.
    pub fn set_auto_shell(&mut self, auto: bool) {
        self.auto_shell = auto;
        if auto {
            self.idle_note = None;
        }
    }

    /// Keeps the empty panel from starting a shell by itself (until
    /// [`Self::set_auto_shell`] allows it again); `note` tells the user why and
    /// that + starts one.
    pub fn hold_shell(&mut self, note: impl Into<String>) {
        self.auto_shell = false;
        self.idle_note = Some(note.into());
    }

    /// Whether an empty panel starts a shell when it is shown.
    pub fn auto_shell(&self) -> bool {
        self.auto_shell
    }

    pub fn close(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            if self.pending_paste_tab == Some(self.tabs[idx].id) {
                self.pending_paste = None;
                self.pending_paste_tab = None;
            }
            self.tabs[idx].terminal.kill();
            self.tabs.remove(idx);
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
        }
    }

    /// Gives up the keyboard (Escape → editor); the shell keeps running.
    pub fn release_focus(&mut self, ctx: &egui::Context) {
        if let Some((id, _)) = self.output_focus_id {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        self.want_focus = false;
        self.focused = false;
    }

    pub fn request_focus(&mut self) {
        self.want_focus = true;
    }

    /// Whether any terminal produced output since the last frame.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for t in &mut self.tabs {
            let g = t.terminal.generation();
            if g != t.last_generation {
                t.last_generation = g;
                changed = true;
                if !t.renamed {
                    let title = t.terminal.metadata().title;
                    if !title.is_empty() {
                        t.title = title;
                    }
                }
            }
        }
        if self.preferences.close_on_exit {
            let dead = self
                .tabs
                .iter_mut()
                .enumerate()
                .filter_map(|(i, t)| (!t.terminal.is_alive()).then_some(i))
                .collect::<Vec<_>>();
            for i in dead.into_iter().rev() {
                self.close(i);
            }
            if self.tabs.is_empty() {
                self.hold_shell("Session ended. Press + to start another.");
            }
        }
        changed
    }

    pub fn saved_tabs(&self) -> Vec<rustpy_settings::terminal::SavedTab> {
        self.tabs
            .iter()
            .map(|t| rustpy_settings::terminal::SavedTab {
                title: t.title.clone(),
                cwd: t.terminal.current_directory(),
                local_directory: t.terminal.cwd.clone(),
                profile: t.profile.clone(),
                group: t.group,
                vertical: t.vertical,
            })
            .collect()
    }
    pub fn restore(
        &mut self,
        tabs: &[rustpy_settings::terminal::SavedTab],
        env: &[(String, String)],
    ) {
        for saved in tabs.iter().take(32) {
            let old = self.preferences.selected_profile.clone();
            let profiles = self.preferences.profiles.clone();
            self.preferences.selected_profile = saved.profile.clone();
            let remote = self
                .preferences
                .profiles
                .iter_mut()
                .find(|p| p.name == saved.profile)
                .is_some_and(|p| {
                    let remote = p.target_kind != "local";
                    if remote {
                        p.directory = saved.cwd.to_string_lossy().into_owned();
                    }
                    remote
                });
            let cwd = if remote && !saved.local_directory.as_os_str().is_empty() {
                &saved.local_directory
            } else {
                &saved.cwd
            };
            if self.open(cwd, env).is_ok() {
                let tab = self.tabs.last_mut().unwrap();
                tab.title = saved.title.clone();
                tab.renamed = true;
                tab.group = saved.group;
                tab.vertical = saved.vertical;
            }
            self.preferences.selected_profile = old;
            self.preferences.profiles = profiles;
        }
        self.next_id = self
            .next_id
            .max(tabs.iter().map(|tab| tab.group).max().unwrap_or(0));
    }
    pub fn send_selection(&mut self, text: &str) {
        self.simulate_paste(text);
        self.request_focus();
    }
    pub fn override_shortcut(&self, key: egui::Key, mods: egui::Modifiers) -> bool {
        let mut text = String::new();
        if mods.ctrl || mods.command {
            text.push_str("Ctrl+");
        }
        if mods.alt {
            text.push_str("Alt+");
        }
        if mods.shift {
            text.push_str("Shift+");
        }
        text.push_str(key.name());
        self.preferences
            .override_shortcuts
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(&text))
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        mono: egui::FontId,
        cwd: &Path,
        env: &[(String, String)],
    ) {
        // Tabs row.
        let mut close: Option<usize> = None;
        let mut reorder = None;
        let mut split = None;
        ui.horizontal(|ui| {
            for (i, t) in self.tabs.iter().enumerate() {
                let resp = ui.selectable_label(i == self.active, &t.title);
                if resp.clicked() {
                    self.active = i;
                    self.want_focus = true;
                }
                if resp.middle_clicked() {
                    close = Some(i);
                }
                let drag = ui.interact(resp.rect, resp.id.with("drag"), Sense::drag());
                if drag.drag_started() {
                    self.drag_tab = Some(t.id);
                }
                if resp.hovered() && ui.input(|input| input.pointer.any_released()) {
                    if let Some(id) = self.drag_tab.take() {
                        reorder = Some((id, t.id));
                    }
                }
                resp.context_menu(|ui| {
                    if ui.button("Rename").clicked() {
                        self.rename = Some((t.id, t.title.clone()));
                        ui.close();
                    }
                    if ui.button("Split right").clicked() {
                        split = Some((i, false));
                        ui.close();
                    }
                    if ui.button("Split down").clicked() {
                        split = Some((i, true));
                        ui.close();
                    }
                    if ui.button("Close").clicked() {
                        close = Some(i);
                        ui.close();
                    }
                });
            }
            if ui.button("+").on_hover_text("New session").clicked() {
                let r = match self.new_tab_command.clone() {
                    Some((title, prog, args)) => self.open_command(&title, &prog, &args, cwd, env),
                    None => self.open(cwd, env),
                };
                if let Err(e) = r {
                    log::error!("cannot open terminal: {e}");
                }
            }
            if !self.tabs.is_empty() && ui.button("×").on_hover_text("Close session").clicked() {
                close = Some(self.active);
            }
            if ui.small_button("Split →").clicked() {
                split = Some((self.active, false));
            }
            if ui.small_button("Split ↓").clicked() {
                split = Some((self.active, true));
            }
            if ui.small_button("Find").clicked() {
                self.searching = !self.searching;
            }
            if ui.small_button("Clear").clicked() {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.terminal.clear();
                    tab.selection = None;
                }
            }
            if ui.small_button("End").clicked() {
                if let Some(tab) = self.tabs.get(self.active) {
                    tab.terminal.scroll_to_bottom();
                }
            }
            if ui.small_button("Float").clicked() {
                self.detached = !self.detached;
            }
            if ui.small_button("Python Console").clicked() {
                self.events.push(TerminalEvent::PythonConsole);
            }
            egui::ComboBox::from_id_salt(ui.id().with("terminal-profile"))
                .selected_text(&self.preferences.selected_profile)
                .show_ui(ui, |ui| {
                    for profile in &self.preferences.profiles {
                        ui.selectable_value(
                            &mut self.preferences.selected_profile,
                            profile.name.clone(),
                            &profile.name,
                        );
                    }
                })
                .combo_named(crate::i18n::tr_static("Terminal profile"));
        });
        if let Some((from, to)) = reorder {
            if let (Some(from), Some(to)) = (
                self.tabs.iter().position(|t| t.id == from),
                self.tabs.iter().position(|t| t.id == to),
            ) {
                let tab = self.tabs.remove(from);
                self.tabs.insert(to, tab);
                self.active = to;
            }
        }
        if let Some((index, vertical)) = split {
            if let Some(tab) = self.tabs.get(index) {
                let group = tab.group;
                let directory = tab.terminal.current_directory();
                if self.open(&directory, env).is_ok() {
                    self.tabs[self.active].group = group;
                    for tab in self.tabs.iter_mut().filter(|t| t.group == group) {
                        tab.vertical = vertical;
                    }
                }
            }
        }
        if let Some((id, name)) = &mut self.rename {
            let mut finish = false;
            ui.horizontal(|ui| {
                ui.label("Tab name");
                let response = ui.text_edit_singleline(name);
                if ui.button("Save").clicked()
                    || (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    finish = true;
                }
            });
            if finish {
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == *id) {
                    tab.title = name.clone();
                    tab.renamed = true;
                }
                self.rename = None;
            }
        }
        if let Some(i) = close {
            self.close(i);
        }
        if self.tabs.is_empty() {
            let r = match self.new_tab_command.clone() {
                Some((title, prog, args)) => self.open_command(&title, &prog, &args, cwd, env),
                None if self.auto_shell => self.open(cwd, env),
                None => {
                    let note = self
                        .idle_note
                        .as_deref()
                        .unwrap_or("No session. Press + to start one.");
                    ui.label(
                        egui::RichText::new(note).color(if self.idle_note.is_some() {
                            theme.warning
                        } else {
                            theme.text_dim
                        }),
                    );
                    return;
                }
            };
            if let Err(e) = r {
                ui.label(
                    egui::RichText::new(format!("Cannot start session: {e}")).color(theme.error),
                );
                return;
            }
        }
        ui.separator();
        let idx = self.active.min(self.tabs.len() - 1);
        if self.searching {
            ui.horizontal(|ui| {
                ui.label("Find");
                if ui.text_edit_singleline(&mut self.search).changed() {
                    self.search_index = 0;
                }
                let positions = self.tabs[idx].terminal.find(&self.search);
                if ui.button("Previous").clicked() && !positions.is_empty() {
                    self.search_index = (self.search_index + positions.len() - 1) % positions.len();
                }
                if ui.button("Next").clicked() && !positions.is_empty() {
                    self.search_index = (self.search_index + 1) % positions.len();
                }
                ui.label(format!("{} matches", positions.len()));
                if let Some(position) = positions.get(self.search_index) {
                    self.tabs[idx].terminal.reveal(*position);
                }
                if ui.small_button("×").clicked() {
                    self.searching = false;
                }
            });
        }
        if self.preferences.shell_integration && self.preferences.engine == "new" {
            let metadata = self.tabs[idx].terminal.metadata();
            egui::CollapsingHeader::new("Command blocks")
                .id_salt(self.tabs[idx].id)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for block in metadata.blocks.iter().rev().take(30) {
                                if self.preferences.block_separators {
                                    ui.separator();
                                }
                                ui.horizontal(|ui| {
                                    ui.label(format!(
                                        "{} {}",
                                        self.preferences.prompt_prefix,
                                        if block.command.is_empty() {
                                            format!("Command {}", block.id)
                                        } else {
                                            block.command.clone()
                                        }
                                    ));
                                    ui.label(
                                        block
                                            .status
                                            .map(|s| format!("exit {s}"))
                                            .unwrap_or_else(|| "running".into()),
                                    );
                                    if ui.small_button("Copy output").clicked() {
                                        ui.ctx().copy_text(block.output.clone());
                                    }
                                });
                                ui.collapsing("Output", |ui| {
                                    ui.monospace(&block.output);
                                });
                            }
                        });
                });
            ui.horizontal(|ui| {
                let input = ui.text_edit_singleline(&mut self.command);
                let mut run = ui.button("Run in terminal").clicked()
                    || (input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if ui.button("History").clicked() {
                    self.history_popup = !self.history_popup;
                }
                if ui.button("Complete").clicked() {
                    let choices = terminal_completions(
                        &self.command,
                        &self.tabs[idx].terminal.current_directory(),
                    );
                    if let Some(value) = choices.first() {
                        self.command = value.clone();
                    }
                }
                if self.preferences.smart_commands && ui.button("Run with IDE features").clicked() {
                    self.events.push(TerminalEvent::SmartCommand(
                        self.command.clone(),
                        self.tabs[idx].terminal.current_directory(),
                    ));
                    run = false;
                }
                if run && !self.command.trim().is_empty() {
                    let command = std::mem::take(&mut self.command);
                    self.tabs[idx].terminal.note_command(&command);
                    self.tabs[idx].terminal.paste(&command);
                    self.tabs[idx].terminal.write(b"\r");
                    self.history.push(command);
                    if self.history.len() > 500 {
                        self.history.remove(0);
                    }
                }
            });
            if self.history_popup {
                egui::ScrollArea::vertical()
                    .max_height(100.0)
                    .show(ui, |ui| {
                        for command in self.history.iter().rev() {
                            if ui.selectable_label(false, command).clicked() {
                                self.command = command.clone();
                                self.history_popup = false;
                            }
                        }
                    });
            }
        }
        if let Some(text) = self.pending_paste.clone() {
            let paste_index = self
                .pending_paste_tab
                .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
                .unwrap_or(idx);
            let lines = text.trim_end_matches('\n').lines().count();
            let mut done = false;
            egui::Frame::new()
                .fill(theme.panel_bg_darker)
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "Paste {lines} lines? The shell may run each line as a command."
                            ))
                            .color(theme.warning),
                        );
                        if ui.button("Paste").clicked() {
                            self.tabs[paste_index].terminal.paste(&text);
                            done = true;
                        }
                        if ui.button("Paste as one line").clicked() {
                            let one = text
                                .lines()
                                .map(str::trim)
                                .filter(|l| !l.is_empty())
                                .collect::<Vec<_>>()
                                .join(" ");
                            self.tabs[paste_index].terminal.paste(&one);
                            done = true;
                        }
                        if ui.button("Cancel").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape))
                        {
                            done = true;
                        }
                    });
                });
            if done {
                self.pending_paste = None;
                self.pending_paste_tab = None;
                self.want_focus = true;
            }
        }
        self.focused = false;
        let group = self.tabs[idx].group;
        let vertical = self.tabs[idx].vertical;
        let panes = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| (t.group == group).then_some(i))
            .collect::<Vec<_>>();
        if panes.len() > 1 {
            let count = panes.len() as f32;
            let size = ui.available_size();
            let layout = if vertical {
                egui::Layout::top_down(egui::Align::Min)
            } else {
                egui::Layout::left_to_right(egui::Align::Min)
            };
            ui.with_layout(layout, |ui| {
                for pane in panes {
                    let pane_size = if vertical {
                        Vec2::new(size.x, size.y / count - 4.0)
                    } else {
                        Vec2::new(size.x / count - 4.0, size.y)
                    };
                    ui.allocate_ui_with_layout(
                        pane_size,
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| self.render_tab(ui, theme, mono.clone(), pane),
                    );
                }
            });
        } else {
            self.render_tab(ui, theme, mono, idx);
        }
    }

    fn render_tab(&mut self, ui: &mut egui::Ui, theme: &UiTheme, mono: egui::FontId, idx: usize) {
        let id = ui.id().with(("terminal", self.tabs[idx].id));
        self.output_focus_id = Some((id, self.tabs[idx].id));
        let avail = ui.available_rect_before_wrap();
        let (char_w, row_h) =
            ui.fonts_mut(|f| (f.glyph_width(&mono, 'M'), f.row_height(&mono) + 1.0));
        let cols = ((avail.width() - 8.0) / char_w).floor().max(10.0) as u16;
        let rows = ((avail.height() - 4.0) / row_h).floor().max(3.0) as u16;
        let tab = &mut self.tabs[idx];
        tab.terminal.resize(cols, rows);
        // Register the area under the focus id so egui keeps keyboard focus on it
        // (focus on an id that no widget uses is dropped after one frame).
        ui.allocate_rect(avail, Sense::hover());
        let resp = ui.interact(avail, id, Sense::click_and_drag());
        let accessible_title: String = tab.title.chars().take(160).collect();
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Other,
                true,
                format!("Terminal {accessible_title}"),
            )
        });
        resp.ctx
            .accesskit_node_builder(resp.id, |node| node.set_role(egui::accesskit::Role::Group));
        let painter = ui.painter().with_clip_rect(avail);
        painter.rect_filled(
            avail,
            0.0,
            if theme.high_contrast {
                theme.panel_bg
            } else if theme.dark {
                Color32::from_rgb(0x1E, 0x1F, 0x22)
            } else {
                Color32::from_rgb(0xFF, 0xFF, 0xFF)
            },
        );
        if resp.clicked() || resp.drag_started() || (self.want_focus && idx == self.active) {
            if resp.clicked() || resp.drag_started() {
                self.active = idx;
            }
            ui.memory_mut(|m| m.request_focus(id));
            self.want_focus = false;
        }
        let focused = ui.memory(|m| m.has_focus(id));
        self.focused |= focused;
        if focused
            && self.announcement_owner
            && self.pending_paste.is_none()
            && !egui::Popup::is_any_open(ui.ctx())
        {
            let status = tab.terminal.with_screen(|screen| {
                let (row, column) = screen.cursor_position();
                let mut line = String::new();
                let mut count = 0;
                for col in 0..screen.size().1 {
                    let Some(cell) = screen.cell(row, col) else { break };
                    if cell.is_wide_continuation() { continue; }
                    let contents = cell.contents();
                    if contents.is_empty() {
                        line.push(' ');
                        count += 1;
                    } else {
                        for ch in contents.chars().take(160 - count) { line.push(ch); count += 1; }
                    }
                    if count >= 160 { break; }
                }
                format!("Terminal {}. Row {}, column {}. {}{}. Ctrl Alt F6 returns to tool controls. {}",
                    tab.title, row + 1, column + 1,
                    if tab.has_selection() { "Selection active. " } else { "" },
                    if screen.scrollback() > 0 { "Viewing scrollback. " } else { "" },
                    line.trim_end())
            });
            crate::a11y::announce(ui.ctx(), resp.id, &status);
        }
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        let default_fg = if theme.dark {
            Color32::from_rgb(0xBB, 0xBB, 0xBB)
        } else {
            Color32::from_rgb(0x20, 0x20, 0x20)
        };
        let selection_bg = if theme.dark {
            theme.selection_bg
        } else {
            Color32::from_rgb(0xA6, 0xD2, 0xFF)
        };
        let origin = Pos2::new(avail.left() + 4.0, avail.top() + 2.0);
        // Selecting past the top or bottom edge scrolls through the scrollback.
        let pointer = resp.interact_pointer_pos().or_else(|| resp.hover_pos());
        let metadata = tab.terminal.metadata();
        if let Some(text) = tab.terminal.take_clipboard() {
            if self.preferences.osc52_clipboard {
                ui.ctx().copy_text(text);
            }
        }
        if metadata.bells != tab.last_bells {
            tab.last_bells = metadata.bells;
            if self.preferences.audible_bell {
                rustpy_terminal::bell::ring();
            }
            if self.preferences.visual_bell {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ));
                painter.rect_stroke(
                    avail,
                    0.0,
                    Stroke::new(3.0, theme.warning),
                    egui::StrokeKind::Inside,
                );
            }
        }
        let mouse_mode = tab
            .terminal
            .with_screen(|screen| screen.mouse_protocol_mode());
        let mouse_reporting = self.preferences.mouse_reporting
            && !ui.input(|i| i.modifiers.shift)
            && mouse_mode != vt100::MouseProtocolMode::None;
        if mouse_reporting && resp.hovered() {
            if let Some(p) = pointer {
                let col = ((p.x - origin.x) / char_w).max(0.0) as u16 + 1;
                let row = ((p.y - origin.y) / row_h).max(0.0) as u16 + 1;
                let events = ui.input(|i| i.events.clone());
                for event in events {
                    if let egui::Event::PointerButton {
                        button, pressed, ..
                    } = event
                    {
                        let code = match button {
                            PointerButton::Primary => 0,
                            PointerButton::Middle => 1,
                            PointerButton::Secondary => 2,
                            _ => continue,
                        };
                        tab.terminal.mouse(code, col, row, pressed, false);
                    }
                }
                if matches!(mouse_mode, vt100::MouseProtocolMode::AnyMotion)
                    || matches!(mouse_mode, vt100::MouseProtocolMode::ButtonMotion)
                        && ui.input(|i| i.pointer.any_down())
                {
                    tab.terminal.mouse(0, col, row, true, true);
                }
            }
        }
        if !mouse_reporting
            && self.preferences.paste_on_middle
            && resp.clicked_by(PointerButton::Middle)
        {
            if let Ok(text) = crate::system_clipboard::open().and_then(|mut c| c.get_text()) {
                if text.contains('\n') && !tab.terminal.bracketed_paste_enabled() {
                    self.pending_paste = Some(text);
                    self.pending_paste_tab = Some(tab.id);
                } else {
                    tab.terminal.paste(&text);
                }
            }
        }
        if self.preferences.copy_on_selection && resp.drag_stopped_by(PointerButton::Primary) {
            if let Some(text) = tab.selected_text() {
                ui.ctx().copy_text(text);
            }
        }
        if let Some(p) = pointer.filter(|_| resp.hovered()) {
            let row = ((p.y - origin.y) / row_h).floor().max(0.0) as u16;
            let col = ((p.x - origin.x) / char_w).floor().max(0.0) as usize;
            let line = tab
                .terminal
                .with_screen(|s| s.contents_between(row, 0, row, cols));
            if let Some((start, end, target)) = terminal_link_at(&line, col, &metadata.hyperlinks) {
                if self.preferences.highlight_links {
                    painter.line_segment(
                        [
                            Pos2::new(
                                origin.x + start as f32 * char_w,
                                origin.y + (row + 1) as f32 * row_h - 1.0,
                            ),
                            Pos2::new(
                                origin.x + end as f32 * char_w,
                                origin.y + (row + 1) as f32 * row_h - 1.0,
                            ),
                        ],
                        Stroke::new(1.0, theme.accent),
                    );
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() && ui.input(|i| i.modifiers.command || i.modifiers.ctrl) {
                    if target.starts_with("https://") || target.starts_with("http://") {
                        self.events.push(TerminalEvent::OpenUrl(target));
                    } else {
                        let target = target.strip_prefix("file://").unwrap_or(&target);
                        let (path, line) = file_link(target);
                        let path = std::path::PathBuf::from(path);
                        let mut path = if path.is_absolute() {
                            path
                        } else {
                            tab.terminal.current_directory().join(path)
                        };
                        if let Some(profile) = self.preferences.profiles.iter().find(|p| {
                            p.name == tab.profile
                                && p.target_kind != "local"
                                && !p.directory.is_empty()
                        }) {
                            if let Ok(relative) = path.strip_prefix(&profile.directory) {
                                path = tab.terminal.cwd.join(relative);
                            }
                        }
                        self.events.push(TerminalEvent::OpenFile(path, line));
                    }
                }
            }
        }
        if resp.dragged_by(PointerButton::Primary) {
            if let Some(p) = pointer {
                let lines = if p.y < avail.top() {
                    1 + ((avail.top() - p.y) / row_h) as i32
                } else if p.y > avail.bottom() {
                    -1 - ((p.y - avail.bottom()) / row_h) as i32
                } else {
                    0
                };
                if lines != 0 {
                    tab.terminal.scroll(lines);
                    ui.ctx().request_repaint();
                }
            }
        }
        let generation = tab.terminal.generation();
        let (press_origin, shift_held) =
            ui.input(|i| (i.pointer.press_origin(), i.modifiers.shift));
        // Render the screen.
        let capacity = tab.terminal.scrollback_limit;
        let (cursor_pos, hide_cursor) = tab.terminal.with_buffer(|screen, hist| {
            let alt_screen = screen.alternate_screen();
            // Buffer line shown on screen row 0.
            let top = hist - screen.scrollback();
            if tab.selection.is_some_and(|s| {
                s.alt_screen != alt_screen
                    || s.size != (cols, rows)
                    || (hist >= capacity && s.generation != generation)
            }) {
                tab.selection = None;
            }
            // Mouse selection: drag selects characters, double click a word,
            // triple click a line, Shift+click extends.
            let hit = |p: Pos2| {
                let row = ((p.y - origin.y) / row_h)
                    .floor()
                    .clamp(0.0, f32::from(rows - 1)) as u16;
                let x = (p.x - origin.x) / char_w;
                (
                    row,
                    x.floor().clamp(0.0, f32::from(cols - 1)) as usize,
                    x.round().clamp(0.0, f32::from(cols)) as usize,
                )
            };
            let row_chars = |r: u16| {
                let mut out: Vec<char> = Vec::with_capacity(usize::from(cols));
                for c in 0..cols {
                    let ch = match screen.cell(r, c) {
                        Some(cell) if cell.is_wide_continuation() => {
                            out.last().copied().unwrap_or(' ')
                        }
                        Some(cell) => cell.contents().chars().next().unwrap_or(' '),
                        None => ' ',
                    };
                    out.push(ch);
                }
                out
            };
            let span_at = |unit: SelectUnit, p: Pos2| {
                let (row, cell, caret) = hit(p);
                let col = if unit == SelectUnit::Char {
                    caret
                } else {
                    cell
                };
                unit_span(
                    unit,
                    TextPos::new(top + usize::from(row), col),
                    &row_chars(row),
                )
            };
            let start = |unit: SelectUnit, p: Pos2| TermSelection {
                sel: TextSelection::new(unit, span_at(unit, p)),
                alt_screen,
                size: (cols, rows),
                generation,
            };
            if mouse_reporting {
                tab.selection = None;
            } else if resp.triple_clicked() {
                tab.selection = pointer.map(|p| start(SelectUnit::Line, p));
            } else if resp.double_clicked() {
                tab.selection = pointer.map(|p| start(SelectUnit::Word, p));
            } else if resp.clicked() {
                match (tab.selection.as_mut(), pointer) {
                    (Some(s), Some(p)) if shift_held => s.sel.extend_to(span_at(s.sel.unit, p)),
                    _ => tab.selection = None,
                }
            } else if resp.drag_started_by(PointerButton::Primary) {
                tab.selection = press_origin.map(|p| start(SelectUnit::Char, p));
            }
            if resp.dragged_by(PointerButton::Primary) {
                if let (Some(s), Some(p)) = (tab.selection.as_mut(), pointer) {
                    s.sel.extend_to(span_at(s.sel.unit, p));
                }
            }
            if resp.drag_stopped_by(PointerButton::Primary)
                && tab.selection.is_some_and(|s| s.sel.is_empty())
            {
                tab.selection = None;
            }
            if let Some(s) = tab.selection.as_mut() {
                s.generation = generation;
            }
            let selection = tab.selection.map(|s| s.sel);
            for r in 0..rows {
                let y = origin.y + r as f32 * row_h;
                let sel_cols =
                    selection.and_then(|s| s.cols_on(top + usize::from(r), usize::from(cols)));
                let selected =
                    |c: u16| sel_cols.is_some_and(|(a, b)| (a..b).contains(&usize::from(c)));
                let mut c = 0u16;
                while c < cols {
                    let Some(cell) = screen.cell(r, c) else { break };
                    // Merge a run of cells with identical attributes.
                    let mut run = String::new();
                    let mut end = c;
                    let attrs = (
                        cell.fgcolor(),
                        cell.bgcolor(),
                        cell.bold(),
                        cell.italic(),
                        cell.underline(),
                        cell.inverse(),
                        selected(c),
                    );
                    let (fg, bg, bold, italic, underline, inverse, in_selection) = attrs;
                    while end < cols {
                        let Some(cc) = screen.cell(r, end) else { break };
                        if (
                            cc.fgcolor(),
                            cc.bgcolor(),
                            cc.bold(),
                            cc.italic(),
                            cc.underline(),
                            cc.inverse(),
                            selected(end),
                        ) != attrs
                        {
                            break;
                        }
                        let text = cc.contents();
                        if cc.is_wide_continuation() {
                            end += 1;
                            continue;
                        }
                        if text.is_empty() {
                            run.push(' ');
                        } else {
                            run.push_str(text);
                        }
                        if cc.is_wide() {
                            run.push(' ');
                            end += 1;
                        }
                        end += 1;
                    }
                    let width = (end - c) as f32 * char_w;
                    let x = origin.x + c as f32 * char_w;
                    let (mut fgc, mut bgc) =
                        (color(fg, default_fg), color(bg, Color32::TRANSPARENT));
                    if inverse {
                        std::mem::swap(&mut fgc, &mut bgc);
                        if bgc == Color32::TRANSPARENT {
                            bgc = default_fg;
                        }
                        if fgc == Color32::TRANSPARENT {
                            fgc = Color32::BLACK;
                        }
                    }
                    if bold && matches!(fg, vt100::Color::Idx(i) if i < 8) {
                        fgc = color(
                            vt100::Color::Idx(if let vt100::Color::Idx(i) = fg {
                                i + 8
                            } else {
                                7
                            }),
                            default_fg,
                        );
                    }
                    if theme.high_contrast {
                        fgc = if inverse { theme.panel_bg } else { theme.text };
                        bgc = if inverse {
                            theme.text
                        } else {
                            Color32::TRANSPARENT
                        };
                    }
                    if in_selection {
                        bgc = selection_bg;
                        if theme.high_contrast {
                            fgc = theme.text;
                        }
                    }
                    if bgc != Color32::TRANSPARENT {
                        painter.rect_filled(
                            Rect::from_min_size(Pos2::new(x, y), Vec2::new(width, row_h)),
                            0.0,
                            bgc,
                        );
                    }
                    if !run.trim().is_empty() {
                        let font =
                            egui::FontId::new(mono.size, crate::theme::mono_family(bold, italic));
                        let galley = ui
                            .fonts_mut(|f| f.layout_no_wrap(run.trim_end().to_string(), font, fgc));
                        painter.galley(Pos2::new(x, y), galley, fgc);
                    }
                    if underline {
                        painter.line_segment(
                            [
                                Pos2::new(x, y + row_h - 2.0),
                                Pos2::new(x + width, y + row_h - 2.0),
                            ],
                            Stroke::new(1.0, fgc),
                        );
                    }
                    c = end.max(c + 1);
                }
            }
            (screen.cursor_position(), screen.hide_cursor())
        });
        let shape = if metadata.cursor_shape > 0 {
            metadata.cursor_shape
        } else {
            match self.preferences.cursor.as_str() {
                "underline" => 3,
                "bar" => 5,
                _ => 1,
            }
        };
        let blink_on = crate::a11y::reduced_motion()
            || !self.preferences.cursor_blink
            || ui.input(|i| i.time) % 1.0 < 0.5;
        if self.preferences.cursor_blink && !crate::a11y::reduced_motion() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        if !hide_cursor && blink_on {
            let (cr, cc) = cursor_pos;
            let mut rect = Rect::from_min_size(
                Pos2::new(origin.x + cc as f32 * char_w, origin.y + cr as f32 * row_h),
                Vec2::new(char_w, row_h),
            );
            if matches!(shape, 3 | 4) {
                rect.min.y = rect.max.y - 2.0;
            } else if matches!(shape, 5 | 6) {
                rect.max.x = rect.min.x + 2.0;
            }
            if focused {
                painter.rect_filled(
                    rect,
                    0.0,
                    Color32::from_rgba_unmultiplied(0xBB, 0xBB, 0xBB, 200),
                );
            } else {
                painter.rect_stroke(
                    rect,
                    0.0,
                    Stroke::new(1.0, Color32::from_gray(150)),
                    egui::StrokeKind::Inside,
                );
            }
        }
        // Scrollback indicator.
        let sb = tab.terminal.with_screen(|s| s.scrollback());
        if sb > 0 {
            painter.text(
                Pos2::new(avail.right() - 8.0, avail.top() + 4.0),
                Align2::RIGHT_TOP,
                format!("⬆ {sb}"),
                egui::FontId::proportional(11.0),
                theme.text_dim,
            );
        }
        // Mouse wheel → scrollback.
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.5 {
                if mouse_reporting {
                    tab.terminal
                        .mouse(if scroll > 0.0 { 64 } else { 65 }, 1, 1, true, false);
                } else {
                    tab.terminal.scroll((scroll / row_h * 3.0) as i32);
                }
            }
        }
        // Context menu.
        let mut pending_paste: Option<String> = None;
        crate::a11y::keyboard_context_menu(
            &resp,
            self.announcement_owner && self.pending_paste.is_none(),
        )
        .show(|ui| {
            if ui
                .add_enabled(
                    tab.has_selection(),
                    egui::Button::new("Copy").shortcut_text("Ctrl+Shift+C"),
                )
                .clicked()
            {
                if let Some(text) = tab.selected_text() {
                    ui.ctx().copy_text(text);
                }
                ui.close();
            }
            if ui
                .add(egui::Button::new("Paste").shortcut_text("Ctrl+Shift+V"))
                .clicked()
            {
                match crate::system_clipboard::open().and_then(|mut c| c.get_text()) {
                    Ok(t)
                        if t.trim_end_matches('\n').contains('\n')
                            && !tab.terminal.bracketed_paste_enabled() =>
                    {
                        pending_paste = Some(t)
                    }
                    Ok(t) => tab.terminal.paste(&t),
                    Err(e) => log::warn!("cannot read the clipboard: {e}"),
                }
                ui.close();
            }
            if ui.button("Select All").clicked() {
                tab.select_all();
                ui.close();
            }
        });
        // Keyboard input.
        if focused && !egui::Popup::is_any_open(ui.ctx()) {
            let (events, frame_mods) = ui.input(|i| (i.events.clone(), i.modifiers));
            // Modifiers held at each event: replay this frame's changes from the
            // previous frame's state (egui keeps only the state after the frame).
            let mut mods_now = if events
                .iter()
                .any(|e| matches!(e, egui::Event::ModifiersChanged(_)))
            {
                self.last_modifiers
            } else {
                frame_mods
            };
            for ev in events {
                match ev {
                    egui::Event::ModifiersChanged(m) => mods_now = m,
                    egui::Event::Text(t) => {
                        if tab.input_line.len() < 8192 {
                            tab.input_line.push_str(&t);
                        }
                        tab.selection = None;
                        tab.terminal.write(t.as_bytes());
                        tab.terminal.scroll_to_bottom();
                    }
                    egui::Event::Paste(t) => {
                        if t.trim_end_matches('\n').contains('\n')
                            && !tab.terminal.bracketed_paste_enabled()
                        {
                            pending_paste = Some(t);
                        } else {
                            tab.terminal.paste(&t);
                        }
                    }
                    // winit delivers Ctrl+C / Ctrl+X as Copy / Cut, never as keys.
                    egui::Event::Copy => {
                        tab.clipboard_shortcut(ui.ctx(), ClipboardShortcut::Copy, mods_now.shift)
                    }
                    egui::Event::Cut => {
                        tab.clipboard_shortcut(ui.ctx(), ClipboardShortcut::Cut, mods_now.shift)
                    }
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if (modifiers.ctrl || modifiers.command) && key == egui::Key::F {
                            self.searching = true;
                            continue;
                        }
                        if (modifiers.ctrl || modifiers.command) && key == egui::Key::R {
                            self.history_popup = true;
                            continue;
                        }
                        if (modifiers.ctrl || modifiers.command) && key == egui::Key::L {
                            tab.terminal.clear();
                            tab.selection = None;
                            continue;
                        }
                        let mods = Mods {
                            ctrl: modifiers.ctrl || modifiers.command,
                            alt: modifiers.alt
                                && (!crate::actions::shortcuts::mac_keymap()
                                    || self.preferences.option_as_meta),
                            shift: modifiers.shift,
                        };
                        if key == egui::Key::Enter && !tab.input_line.trim().is_empty() {
                            let command = std::mem::take(&mut tab.input_line);
                            tab.terminal.note_command(&command);
                            self.history.push(command);
                            if self.history.len() > 500 {
                                self.history.remove(0);
                            }
                        }
                        if key == egui::Key::Backspace {
                            tab.input_line.pop();
                        }
                        // Ctrl+Shift+V / Ctrl+Shift+C are handled by egui as Paste/Copy events.
                        if mods.ctrl && mods.shift {
                            continue;
                        }
                        let k = match key {
                            egui::Key::Enter => Some(Key::Enter),
                            egui::Key::Backspace => Some(Key::Backspace),
                            egui::Key::Tab => Some(Key::Tab),
                            egui::Key::Escape => Some(Key::Escape),
                            egui::Key::ArrowUp => Some(Key::Up),
                            egui::Key::ArrowDown => Some(Key::Down),
                            egui::Key::ArrowLeft => Some(Key::Left),
                            egui::Key::ArrowRight => Some(Key::Right),
                            egui::Key::Home => Some(Key::Home),
                            egui::Key::End => Some(Key::End),
                            egui::Key::PageUp => Some(Key::PageUp),
                            egui::Key::PageDown => Some(Key::PageDown),
                            egui::Key::Delete => Some(Key::Delete),
                            egui::Key::Insert => Some(Key::Insert),
                            egui::Key::F1 => Some(Key::F(1)),
                            egui::Key::F2 => Some(Key::F(2)),
                            egui::Key::F3 => Some(Key::F(3)),
                            egui::Key::F4 => Some(Key::F(4)),
                            egui::Key::F5 => Some(Key::F(5)),
                            egui::Key::F6 => Some(Key::F(6)),
                            egui::Key::F7 => Some(Key::F(7)),
                            egui::Key::F8 => Some(Key::F(8)),
                            egui::Key::F9 => Some(Key::F(9)),
                            egui::Key::F10 => Some(Key::F(10)),
                            egui::Key::F11 => Some(Key::F(11)),
                            egui::Key::F12 => Some(Key::F(12)),
                            k if mods.ctrl || mods.alt => {
                                // Letters/digits with Ctrl or Alt are not delivered as Text events.
                                let name = k.name();
                                let ch = if name.len() == 1 {
                                    name.chars().next()
                                } else {
                                    None
                                };
                                ch.map(|c| {
                                    Key::Char(if mods.shift {
                                        c.to_ascii_uppercase()
                                    } else {
                                        c.to_ascii_lowercase()
                                    })
                                })
                            }
                            _ => None,
                        };
                        if let Some(k) = k {
                            tab.selection = None;
                            tab.terminal.send_key(k, mods);
                            tab.terminal.scroll_to_bottom();
                        }
                    }
                    _ => {}
                }
            }
        }
        self.last_modifiers = ui.input(|i| i.modifiers);
        if !tab.terminal.is_alive() {
            painter.text(
                avail.center(),
                Align2::CENTER_CENTER,
                "[Process exited — press + for a new session]",
                egui::FontId::proportional(13.0),
                theme.text_dim,
            );
        }
        if pending_paste.is_some() {
            self.pending_paste = pending_paste;
            self.pending_paste_tab = Some(tab.id);
        }
    }

    /// Smoke tests: simulate a paste into the active session.
    pub fn simulate_paste(&mut self, text: &str) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if text.trim_end_matches('\n').contains('\n') && !tab.terminal.bracketed_paste_enabled()
            {
                self.pending_paste = Some(text.to_string());
                self.pending_paste_tab = Some(tab.id);
            } else {
                tab.terminal.paste(text);
            }
        }
    }
}

/// Environment for new terminal tabs: the interpreter's virtualenv is
/// activated only while Settings → Tools → Terminal → "Activate virtualenv" is on.
pub fn terminal_env(
    activate_virtualenv: bool,
    interpreter: Option<&rustpy_project::Interpreter>,
) -> Vec<(String, String)> {
    rustpy_project::env::terminal_env(
        activate_virtualenv,
        interpreter,
        &std::env::var("PATH").unwrap_or_default(),
    )
}

/// Environment that activates the selected interpreter's virtualenv in new shells.
pub fn venv_env(interpreter: Option<&rustpy_project::Interpreter>) -> Vec<(String, String)> {
    rustpy_project::env::venv_env(interpreter, &std::env::var("PATH").unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use rustpy_terminal::key_to_bytes;

    #[test]
    fn ctrl_c_interrupts_unless_text_is_selected() {
        use ClipboardShortcut::{Copy, Cut};
        assert_eq!(
            copy_or_interrupt(Copy, false, false, false),
            ShortcutAction::SendCtrl('c')
        );
        assert_eq!(
            copy_or_interrupt(Copy, true, false, false),
            ShortcutAction::CopySelection { keep: false }
        );
        assert_eq!(
            copy_or_interrupt(Copy, true, true, false),
            ShortcutAction::CopySelection { keep: true },
            "Ctrl+Shift+C keeps the selection"
        );
        assert_eq!(
            copy_or_interrupt(Copy, false, true, false),
            ShortcutAction::Ignore,
            "Ctrl+Shift+C never interrupts"
        );
        assert_eq!(
            copy_or_interrupt(Cut, false, false, false),
            ShortcutAction::SendCtrl('x')
        );
        assert_eq!(
            copy_or_interrupt(Cut, true, false, false),
            ShortcutAction::SendCtrl('x'),
            "terminal text cannot be cut"
        );
        assert_eq!(
            copy_or_interrupt(Cut, false, true, false),
            ShortcutAction::Ignore
        );
        // macOS: Cmd+C / Cmd+X are clipboard shortcuts only.
        assert_eq!(
            copy_or_interrupt(Copy, false, false, true),
            ShortcutAction::Ignore
        );
        assert_eq!(
            copy_or_interrupt(Copy, true, false, true),
            ShortcutAction::CopySelection { keep: true }
        );
        assert_eq!(
            copy_or_interrupt(Cut, true, false, true),
            ShortcutAction::Ignore
        );
        // What `SendCtrl` puts on the wire.
        let ctrl = Mods {
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(key_to_bytes(Key::Char('c'), ctrl, false), Some(vec![0x03]));
        assert_eq!(key_to_bytes(Key::Char('x'), ctrl, false), Some(vec![0x18]));
    }

    #[test]
    fn selection_survives_new_output_but_not_screen_changes() {
        let sel = TermSelection {
            sel: TextSelection::new(SelectUnit::Char, (TextPos::new(3, 0), TextPos::new(4, 2))),
            alt_screen: false,
            size: (80, 24),
            generation: 7,
        };
        assert!(
            sel.still_valid(false, (80, 24), 100, 9),
            "output appended below keeps line numbers"
        );
        assert!(
            !sel.still_valid(false, (80, 24), SCROLLBACK_LINES, 9),
            "a full scrollback drops rows above the selection"
        );
        assert!(sel.still_valid(false, (80, 24), SCROLLBACK_LINES, 7));
        assert!(!sel.still_valid(true, (80, 24), 0, 7), "alternate screen");
        assert!(!sel.still_valid(false, (100, 24), 100, 7), "resize");
    }

    #[test]
    fn terminal_env_honours_activate_virtualenv() {
        let venv = rustpy_project::Interpreter {
            path: PathBuf::from("/p/.venv/bin/python"),
            version: "3.12.1".into(),
            prefix: PathBuf::from("/p/.venv"),
            sys_path: Vec::new(),
            is_venv: true,
            platform: "linux".into(),
            kind: "venv".into(),
        };
        let env = terminal_env(true, Some(&venv));
        assert!(
            env.contains(&("VIRTUAL_ENV".to_string(), "/p/.venv".to_string())),
            "{env:?}"
        );
        assert!(
            env.iter()
                .any(|(k, v)| k == "PATH" && v.starts_with("/p/.venv/bin")),
            "{env:?}"
        );
        assert!(terminal_env(false, Some(&venv)).is_empty());
        let system = rustpy_project::Interpreter {
            is_venv: false,
            ..venv
        };
        assert!(terminal_env(true, Some(&system)).is_empty());
        assert!(terminal_env(true, None).is_empty());
    }

    fn wait_for_screen(tab: &TerminalTab, needle: &str) {
        let start = std::time::Instant::now();
        while !tab.terminal.contents().contains(needle) && start.elapsed().as_secs() < 10 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            tab.terminal.contents().contains(needle),
            "waiting for {needle:?}, screen:\n{}",
            tab.terminal.contents()
        );
    }

    #[test]
    fn copy_event_copies_a_selection_then_interrupts() {
        if cfg!(windows) || !Path::new("/bin/sh").exists() {
            return;
        }
        let mut panel = TerminalPanel::default();
        let script =
            "trap 'echo GOT_SIGINT; exit 0' INT; echo READY now; while :; do sleep 0.05; done";
        panel
            .open_command(
                "t",
                "/bin/sh",
                &["-c".to_string(), script.to_string()],
                Path::new("/"),
                &[],
            )
            .expect("spawn");
        let tab = &mut panel.tabs[0];
        wait_for_screen(tab, "READY now");
        let span = (TextPos::new(0, 0), TextPos::new(0, 5));
        tab.selection = Some(TermSelection {
            sel: TextSelection::new(SelectUnit::Word, span),
            alt_screen: false,
            size: (tab.terminal.cols, tab.terminal.rows),
            generation: 0,
        });
        assert_eq!(tab.selected_text().as_deref(), Some("READY"));
        let ctx = egui::Context::default();
        // Ctrl+C with a selection copies it (and clears it) ...
        tab.clipboard_shortcut(&ctx, ClipboardShortcut::Copy, false);
        assert!(ctx.output(|o| o
            .commands
            .contains(&egui::OutputCommand::CopyText("READY".into()))));
        assert!(tab.selection.is_none());
        assert!(!tab.terminal.contents().contains("GOT_SIGINT"));
        // ... and without one it interrupts the program.
        tab.clipboard_shortcut(&ctx, ClipboardShortcut::Copy, false);
        wait_for_screen(tab, "GOT_SIGINT");
        tab.select_all();
        let all = tab.selected_text().unwrap_or_default();
        assert!(
            all.starts_with("READY now\n") && all.contains("GOT_SIGINT"),
            "{all:?}"
        );
    }
}

fn terminal_completions(command: &str, cwd: &Path) -> Vec<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
    let split = command
        .rfind(char::is_whitespace)
        .map_or(0, |index| index + 1);
    let prefix = &command[split..];
    let path = std::path::Path::new(prefix);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let base = if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        cwd.join(parent)
    };
    let needle = path.file_name().unwrap_or_default().to_string_lossy();
    let mut values = Vec::new();
    if let Ok(entries) = std::fs::read_dir(base) {
        for entry in entries.flatten().take(2000) {
            if std::time::Instant::now() >= deadline {
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(needle.as_ref()) {
                let candidate = parent
                    .join(name)
                    .to_string_lossy()
                    .trim_start_matches("./")
                    .to_string();
                let suffix = if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    "/"
                } else {
                    ""
                };
                values.push(format!("{}{}{}", &command[..split], candidate, suffix));
            }
        }
    }
    if split == 0 && !prefix.contains(std::path::MAIN_SEPARATOR) {
        if let Some(paths) = std::env::var_os("PATH") {
            for directory in std::env::split_paths(&paths).take(128) {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                if let Ok(entries) = std::fs::read_dir(directory) {
                    for entry in entries.flatten().take(5000) {
                        if std::time::Instant::now() >= deadline {
                            break;
                        }
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.starts_with(prefix) {
                            values.push(name);
                        }
                    }
                }
            }
        }
    }
    values.sort();
    values.dedup();
    values.truncate(200);
    values
}
fn terminal_link_at(
    line: &str,
    column: usize,
    osc: &std::collections::BTreeMap<String, String>,
) -> Option<(usize, usize, String)> {
    for (label, target) in osc {
        if let Some(byte) = line.find(label) {
            let start = line[..byte].chars().count();
            let end = start + label.chars().count();
            if (start..end).contains(&column) {
                return Some((start, end, target.clone()));
            }
        }
    }
    let chars = line.chars().collect::<Vec<_>>();
    if column >= chars.len() {
        return None;
    }
    let valid = |ch: char| {
        !ch.is_whitespace() && !matches!(ch, '\'' | '"' | '<' | '>' | '(' | ')' | '[' | ']')
    };
    let mut start = column;
    let mut end = column;
    while start > 0 && valid(chars[start - 1]) {
        start -= 1;
    }
    while end < chars.len() && valid(chars[end]) {
        end += 1;
    }
    let word = chars[start..end]
        .iter()
        .collect::<String>()
        .trim_end_matches([',', ';', '.'])
        .to_string();
    if word.starts_with("https://")
        || word.starts_with("http://")
        || word.starts_with("file://")
        || word
            .rsplit_once(':')
            .is_some_and(|(_, number)| number.parse::<usize>().is_ok())
    {
        Some((start, start + word.chars().count(), word))
    } else {
        None
    }
}
fn file_link(value: &str) -> (&str, usize) {
    let Some((prefix, line)) = value.rsplit_once(':') else {
        return (value, 1);
    };
    if let Ok(last) = line.parse::<usize>() {
        if let Some((file, maybe_line)) = prefix.rsplit_once(':') {
            if let Ok(line) = maybe_line.parse::<usize>() {
                return (file, line.max(1));
            }
        }
        (prefix, last.max(1))
    } else {
        (value, 1)
    }
}
#[cfg(test)]
mod workspace_fixtures {
    use super::*;
    #[test]
    fn activation_runs_from_startup_files_not_typed_input() {
        let root = tempfile::tempdir().unwrap();
        let venv = root.path().join("venv");
        std::fs::create_dir_all(venv.join("bin")).unwrap();
        std::fs::write(venv.join("bin/activate"), "").unwrap();
        let env = [(
            "VIRTUAL_ENV".to_string(),
            venv.to_string_lossy().into_owned(),
        )];
        let command = activation_command("bash", &env).unwrap();
        assert!(command.starts_with(". '") && command.ends_with("bin/activate'"));
        assert!(
            !command.ends_with('\r'),
            "passed via RUSTPY_STARTUP, not typed"
        );
        // A missing activate script or no environment means no startup command.
        assert_eq!(activation_command("fish", &env), None);
        assert_eq!(activation_command("bash", &[]), None);
        let conda = [("CONDA_PREFIX".to_string(), "/opt/conda/envs/x".to_string())];
        assert!(activation_command("zsh", &conda)
            .unwrap()
            .contains("conda activate '/opt/conda/envs/x'"));
        assert_eq!(activation_command("pwsh", &conda), None);
    }
    #[test]
    fn hyperlinks_use_character_columns_and_keep_source_line() {
        let links = std::collections::BTreeMap::new();
        let (_, _, target) = terminal_link_at("λ example.py:42:9", 5, &links).unwrap();
        assert_eq!(file_link(&target), ("example.py", 42));
        let links = [("click here".into(), "https://example.org/a".into())]
            .into_iter()
            .collect();
        assert_eq!(
            terminal_link_at("λ click here", 4, &links).unwrap().2,
            "https://example.org/a"
        );
    }
    #[test]
    fn terminal_shortcut_override_is_explicit() {
        let panel = TerminalPanel::default();
        assert!(panel.override_shortcut(egui::Key::F, egui::Modifiers::CTRL));
        assert!(!panel.override_shortcut(egui::Key::F, egui::Modifiers::ALT));
    }
}

/// The command activating the project's virtualenv or conda environment in
/// `shell`, run after the user's startup files.
fn activation_command(shell: &str, env: &[(String, String)]) -> Option<String> {
    if let Some((_, prefix)) = env.iter().find(|(key, _)| key == "VIRTUAL_ENV") {
        let script = std::path::PathBuf::from(prefix).join(match shell {
            "fish" => "bin/activate.fish",
            "pwsh" | "powershell" => "Scripts/Activate.ps1",
            "cmd" => "Scripts/activate.bat",
            _ => "bin/activate",
        });
        if !script.is_file() {
            return None;
        }
        let quoted = rustpy_terminal::integration::quote(&script.to_string_lossy());
        return Some(match shell {
            "cmd" => format!("call \"{}\"", script.display()),
            _ => format!(". {quoted}"),
        });
    }
    let (_, prefix) = env.iter().find(|(key, _)| key == "CONDA_PREFIX")?;
    let quoted = rustpy_terminal::integration::quote(prefix);
    match shell {
        "fish" => Some(format!("if type -q conda; conda activate {quoted}; end")),
        "bash" | "zsh" => Some(format!(
            "if command -v conda >/dev/null 2>&1; then conda activate {quoted}; fi"
        )),
        _ => None,
    }
}
