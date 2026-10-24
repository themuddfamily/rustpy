//! Django `manage.py` console (Tools › Run manage.py Task…, Ctrl+Alt+R): a
//! bottom tool window whose command line completes command names (built-in,
//! project `management/commands` and `manage.py help --commands`), options
//! (`manage.py help <command>`, parsed lazily) and app labels, keeps a
//! per-project history (Up/Down), and runs every command through the Run
//! infrastructure ([`IdeApp::framework_manage_task`]: project interpreter,
//! `DJANGO_SETTINGS_MODULE`). The selected run's output streams below the
//! run chips, with Stop and Rerun; several runs can be active at once.
use super::*;
use rustpy_project::manage_py::{
    self, Candidate, CandidateKind, CompletionSource, HelpCache, History,
};
use std::sync::mpsc::{Receiver, Sender};

const INPUT_ID: &str = "manage_py_console_input";

enum Reply {
    /// Static discovery: custom commands and app labels.
    Discovered(Vec<manage_py::CustomCommand>, Vec<String>),
    /// `manage.py help --commands` output (`None` when it failed).
    Commands(Option<String>),
    /// `manage.py help <command>` output.
    Help(String, Option<String>),
}

struct Run {
    task: String,
    /// The Run session, once the launch created it.
    session: Option<u64>,
    /// Sessions launched before this run have smaller ids.
    min_id: u64,
}

pub(super) struct State {
    root: Option<PathBuf>,
    input: String,
    history: History,
    cursor: Option<usize>,
    draft: String,
    source: CompletionSource,
    custom: Vec<manage_py::CustomCommand>,
    reported: Vec<String>,
    cache: HelpCache,
    popup: Option<(usize, Vec<Candidate>)>,
    selected: usize,
    /// The popup was invoked (Ctrl+Space) or navigated (Up/Down): only then
    /// does Enter accept its selection instead of running the typed command.
    popup_focused: bool,
    tx: Sender<Reply>,
    rx: Receiver<Reply>,
    pending: usize,
    requested: std::collections::BTreeSet<String>,
    runs: Vec<Run>,
    active_run: Option<usize>,
    focus: bool,
    message: String,
}

impl Default for State {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            root: None,
            input: String::new(),
            history: History::default(),
            cursor: None,
            draft: String::new(),
            source: CompletionSource::default(),
            custom: Vec::new(),
            reported: Vec::new(),
            cache: HelpCache::default(),
            popup: None,
            selected: 0,
            popup_focused: false,
            tx,
            rx,
            pending: 0,
            requested: Default::default(),
            runs: Vec::new(),
            active_run: None,
            focus: false,
            message: String::new(),
        }
    }
}

impl State {
    fn rebuild_commands(&mut self) {
        self.source.commands = manage_py::merged_commands(&self.custom, &self.reported);
        self.source.static_options = self
            .custom
            .iter()
            .map(|c| (c.name.clone(), c.options.clone()))
            .collect();
    }

    /// Recomputes the popup for the current input (`force`: Ctrl+Space shows
    /// it for an empty word too).
    fn update_popup(&mut self, force: bool) {
        let (start, items) = manage_py::complete(&self.input, &self.source);
        let word_empty = start == self.input.len();
        if items.is_empty() || (word_empty && !force && self.popup.is_none()) {
            self.popup = None;
            self.popup_focused = false;
        } else {
            self.selected = self.selected.min(items.len() - 1);
            self.popup = Some((start, items));
        }
    }

    fn accept(&mut self, ctx: &egui::Context, text: &str) {
        let start = self.popup.as_ref().map_or(self.input.len(), |(s, _)| *s);
        self.input.truncate(start.min(self.input.len()));
        self.input.push_str(text);
        self.input.push(' ');
        self.popup = None;
        self.selected = 0;
        self.popup_focused = false;
        set_cursor_end(ctx, &self.input);
    }
}

fn set_cursor_end(ctx: &egui::Context, text: &str) {
    let id = egui::Id::new(INPUT_ID);
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        let end = egui::text::CCursor::new(text.chars().count());
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(end)));
        state.store(ctx, id);
    }
}

fn kind_label(kind: CandidateKind) -> &'static str {
    match kind {
        CandidateKind::Command => "command",
        CandidateKind::Option => "option",
        CandidateKind::AppLabel => "app",
    }
}

impl IdeApp {
    /// Tools › Run manage.py Task…: shows the console for the current project.
    pub(super) fn open_manage_py_console(&mut self) {
        if self.project.is_none() {
            self.set_status("Open a Django project to run manage.py tasks");
            return;
        }
        self.load_manage_py_console(false);
        self.manage_py.focus = true;
        self.bottom = Some(BottomTool::ManagePy);
    }

    /// Code completion while the manage.py command line has the keyboard
    /// completes that line (never the editor behind it).
    pub(super) fn manage_py_claims(&mut self, ctx: &egui::Context, action: Action) -> bool {
        if self.bottom != Some(BottomTool::ManagePy)
            || !matches!(action, Action::CodeCompletion | Action::SmartTypeCompletion)
            || !ctx.memory(|m| m.has_focus(egui::Id::new(INPUT_ID)))
        {
            return false;
        }
        self.manage_py.update_popup(true);
        self.manage_py.popup_focused = self.manage_py.popup.is_some();
        true
    }

    /// The project interpreter when it is a local executable.
    fn manage_py_python(&self) -> Option<PathBuf> {
        self.project
            .as_ref()?
            .interpreter
            .as_ref()
            .map(|i| i.path.clone())
            .filter(|path| path.is_file())
    }

    /// (Re)loads history, the help cache and discovery for the current
    /// project; `refresh` also re-runs discovery and the interpreter queries.
    fn load_manage_py_console(&mut self, refresh: bool) {
        let Some(project) = &self.project else { return };
        let root = project.root.clone();
        let facet = project.tree.model().frameworks;
        if self.manage_py.root.as_ref() != Some(&root) {
            // Another project: its history, caches and runs replace these.
            self.manage_py = State {
                root: Some(root.clone()),
                history: History::load(&root),
                ..Default::default()
            };
        } else if !refresh {
            return;
        }
        let python = self.manage_py_python();
        let key = python
            .as_deref()
            .map(|python| HelpCache::key(python, &root, &facet))
            .unwrap_or_default();
        let state = &mut self.manage_py;
        state.cache = HelpCache::load(&root, &key);
        state.reported = state.cache.commands.clone();
        state.source.help = state.cache.help.clone();
        state.requested.clear();
        state.rebuild_commands();
        if !facet.manage(&root).is_file() {
            state.message = format!("manage.py not found: {}", facet.manage(&root).display());
        } else {
            state.message.clear();
        }
        let tx = state.tx.clone();
        state.pending += 1;
        let (discover_root, discover_facet) = (root.clone(), facet.clone());
        std::thread::spawn(move || {
            let custom = manage_py::discover_custom_commands(&discover_root);
            let labels = manage_py::discover_app_labels(&discover_root, &discover_facet);
            let _ = tx.send(Reply::Discovered(custom, labels));
        });
        // `manage.py help` imports the project: only for trusted projects.
        if let Some(python) = python.filter(|_| !self.project_safe_mode()) {
            if facet.manage(&root).is_file() {
                let state = &mut self.manage_py;
                let tx = state.tx.clone();
                state.pending += 1;
                std::thread::spawn(move || {
                    let out = manage_py::query_help(&python, &root, &facet, &["--commands"]);
                    let _ = tx.send(Reply::Commands(out));
                });
            }
        }
    }

    /// Queries `manage.py help <command>` once per console load.
    fn request_manage_py_help(&mut self, command: &str) {
        if self.manage_py.source.help.contains_key(command)
            || !self.manage_py.requested.insert(command.to_string())
            || self.project_safe_mode()
        {
            return;
        }
        let Some(python) = self.manage_py_python() else {
            return;
        };
        let Some(project) = &self.project else { return };
        let root = project.root.clone();
        let facet = project.tree.model().frameworks;
        if !facet.manage(&root).is_file() {
            return;
        }
        let tx = self.manage_py.tx.clone();
        self.manage_py.pending += 1;
        let command = command.to_string();
        std::thread::spawn(move || {
            let out = manage_py::query_help(&python, &root, &facet, &[command.as_str()]);
            let _ = tx.send(Reply::Help(command, out));
        });
    }

    fn poll_manage_py_console(&mut self) {
        let mut changed = false;
        let mut save = false;
        let state = &mut self.manage_py;
        while let Ok(reply) = state.rx.try_recv() {
            state.pending = state.pending.saturating_sub(1);
            changed = true;
            match reply {
                Reply::Discovered(custom, labels) => {
                    state.custom = custom;
                    state.source.app_labels = labels;
                    state.rebuild_commands();
                }
                Reply::Commands(Some(out)) => {
                    let reported = manage_py::parse_command_list(&out);
                    if !reported.is_empty() {
                        state.cache.commands = reported.clone();
                        state.reported = reported;
                        state.rebuild_commands();
                        save = true;
                    }
                }
                Reply::Commands(None) => {
                    state.message =
                        "manage.py help --commands failed; completing built-in and project commands"
                            .into();
                }
                Reply::Help(command, Some(out)) => {
                    let help = manage_py::parse_command_help(&out);
                    if !help.options.is_empty() {
                        state.cache.help.insert(command.clone(), help.clone());
                        state.source.help.insert(command, help);
                        save = true;
                    }
                }
                Reply::Help(_, None) => {}
            }
        }
        if save && !state.cache.key.is_empty() {
            if let Some(root) = &state.root {
                let _ = state.cache.save(root);
            }
        }
        if changed && state.popup.is_some() {
            state.update_popup(false);
        }
        // Adopt the Run sessions created by console launches.
        let adopted: Vec<u64> = state.runs.iter().filter_map(|r| r.session).collect();
        let mut taken = adopted;
        for run in state.runs.iter_mut().filter(|r| r.session.is_none()) {
            if let Some(session) = self.run_panel.sessions.iter().find(|s| {
                s.id >= run.min_id
                    && !taken.contains(&s.id)
                    && (s.config.as_ref().is_some_and(|c| c.parameters == run.task)
                        || s.name.ends_with(run.task.as_str()))
            }) {
                run.session = Some(session.id);
                taken.push(session.id);
            }
        }
    }

    /// Runs `task` (`migrate blog`, …) with the project interpreter.
    fn run_manage_py_task(&mut self, task: String, replace: Option<usize>) {
        let task = task.trim().to_string();
        if task.is_empty() {
            return;
        }
        let Some(project) = &self.project else { return };
        let manage = project.tree.model().frameworks.manage(&project.root);
        if !manage.is_file() {
            self.manage_py.message = format!("manage.py not found: {}", manage.display());
            return;
        }
        if self.project_safe_mode() {
            self.manage_py.message = "Trust the project to run manage.py tasks".into();
            return;
        }
        let min_id = self.next_session_id;
        self.framework_manage_task(&task);
        // The launch shows the Run tool window; output streams here instead.
        self.bottom = Some(BottomTool::ManagePy);
        let run = Run {
            task,
            session: None,
            min_id,
        };
        match replace.filter(|i| *i < self.manage_py.runs.len()) {
            Some(index) => {
                self.manage_py.runs[index] = run;
                self.manage_py.active_run = Some(index);
            }
            None => {
                self.manage_py.runs.push(run);
                self.manage_py.active_run = Some(self.manage_py.runs.len() - 1);
            }
        }
    }

    fn submit_manage_py_input(&mut self) {
        let task = std::mem::take(&mut self.manage_py.input);
        let task = task.trim().to_string();
        let task = task
            .strip_prefix("python ")
            .map(str::trim_start)
            .unwrap_or(&task);
        let task = task
            .strip_prefix("manage.py ")
            .or_else(|| task.strip_prefix("./manage.py "))
            .unwrap_or(task)
            .trim()
            .to_string();
        self.manage_py.cursor = None;
        self.manage_py.draft.clear();
        self.manage_py.popup = None;
        if task.is_empty() {
            return;
        }
        self.manage_py.history.push(&task);
        if let Some(root) = &self.manage_py.root {
            if let Err(error) = self.manage_py.history.save(root) {
                self.manage_py.message = format!("Cannot save manage.py history: {error}");
            }
        }
        self.run_manage_py_task(task, None);
    }

    fn manage_py_session(&self, run: usize) -> Option<&RunSession> {
        let id = self.manage_py.runs.get(run)?.session?;
        self.run_panel.sessions.iter().find(|s| s.id == id)
    }

    pub(super) fn manage_py_console_view(&mut self, ui: &mut egui::Ui) {
        if self.project.is_none() {
            ui.label("Open a Django project to run manage.py tasks.");
            return;
        }
        self.load_manage_py_console(false);
        self.poll_manage_py_console();
        if self.manage_py.pending > 0 {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(150));
        }
        let theme = self.theme.clone();
        let mono = egui::FontId::new(
            self.settings.editor.font_size.max(10.0),
            crate::theme::mono_family(false, false),
        );
        // Toolbar and run chips.
        let mut select = None;
        let mut close = None;
        let mut stop = false;
        let mut rerun = false;
        let mut refresh = false;
        let running = self
            .manage_py
            .active_run
            .and_then(|i| self.manage_py_session(i))
            .is_some_and(|s| s.is_running());
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.manage_py.active_run.is_some(),
                    egui::Button::new("⟳ Rerun"),
                )
                .on_hover_text("Run the selected command again")
                .clicked()
            {
                rerun = true;
            }
            if ui
                .add_enabled(running, egui::Button::new("■ Stop"))
                .on_hover_text("Stop the selected command")
                .clicked()
            {
                stop = true;
            }
            if ui
                .button("Refresh")
                .on_hover_text("Rediscover commands, options and app labels")
                .clicked()
            {
                refresh = true;
            }
            ui.separator();
            for (index, run) in self.manage_py.runs.iter().enumerate() {
                let session = run
                    .session
                    .and_then(|id| self.run_panel.sessions.iter().find(|s| s.id == id));
                let marker = match session.map(|s| &s.status) {
                    None => "…",
                    Some(rustpy_run::SessionStatus::Running) => "▶",
                    Some(rustpy_run::SessionStatus::Exited(0)) => "✔",
                    Some(_) => "✖",
                };
                let selected = self.manage_py.active_run == Some(index);
                if ui
                    .selectable_label(selected, format!("{marker} {}", run.task))
                    .clicked()
                {
                    select = Some(index);
                }
                if ui.small_button("×").on_hover_text("Close").clicked() {
                    close = Some(index);
                }
            }
        });
        if let Some(index) = select {
            self.manage_py.active_run = Some(index);
        }
        if stop {
            if let Some(id) = self
                .manage_py
                .active_run
                .and_then(|i| self.manage_py.runs.get(i))
                .and_then(|r| r.session)
            {
                if let Some(session) = self.run_panel.sessions.iter_mut().find(|s| s.id == id) {
                    session.stop();
                }
            }
        }
        if rerun {
            if let Some(index) = self.manage_py.active_run {
                if let Some(id) = self.manage_py.runs.get(index).and_then(|r| r.session) {
                    if let Some(session) = self.run_panel.sessions.iter_mut().find(|s| s.id == id) {
                        if session.is_running() {
                            session.stop();
                        }
                    }
                }
                if let Some(task) = self.manage_py.runs.get(index).map(|r| r.task.clone()) {
                    self.run_manage_py_task(task, Some(index));
                }
            }
        }
        if let Some(index) = close {
            self.manage_py.runs.remove(index);
            self.manage_py.active_run = match self.manage_py.active_run {
                _ if self.manage_py.runs.is_empty() => None,
                Some(active) if active > index => Some(active - 1),
                Some(active) => Some(active.min(self.manage_py.runs.len() - 1)),
                None => None,
            };
        }
        if refresh {
            self.load_manage_py_console(true);
        }
        if !self.manage_py.message.is_empty() {
            ui.label(egui::RichText::new(&self.manage_py.message).color(theme.warning));
        }
        // Output of the selected run; the input line keeps the bottom row.
        let input_height = ui.spacing().interact_size.y + 8.0;
        let output_height = (ui.available_height() - input_height).max(40.0);
        let row_height = ui.fonts_mut(|f| f.row_height(&mono));
        egui::Frame::new().fill(theme.panel_bg_darker).show(ui, |ui| {
            ui.set_min_height(output_height);
            ui.set_max_height(output_height);
            ui.set_width(ui.available_width());
            let Some(session) = self.manage_py.active_run.and_then(|i| self.manage_py_session(i)) else {
                let hint = if self.manage_py.runs.is_empty() {
                    "Type a manage.py command below (Ctrl+Space completes, Up/Down browse history)."
                } else {
                    "Starting…"
                };
                ui.label(egui::RichText::new(hint).color(theme.text_dim));
                return;
            };
            let console = &session.console;
            let first = console.first_line();
            let total = console.end_line().saturating_sub(first);
            egui::ScrollArea::both()
                .id_salt(("manage_py_output", session.id))
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show_rows(ui, row_height, total, |ui, rows| {
                    for n in rows {
                        let Some(line) = console.line(first + n) else { continue };
                        let color = match line.kind {
                            rustpy_run::OutputKind::Stderr => theme.error,
                            rustpy_run::OutputKind::System => theme.text_dim,
                            rustpy_run::OutputKind::Stdout => theme.text,
                        };
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&line.text).font(mono.clone()).color(color),
                            )
                            .extend(),
                        );
                    }
                });
        });
        self.manage_py_input(ui, &theme, &mono);
    }

    fn manage_py_input(&mut self, ui: &mut egui::Ui, theme: &UiTheme, mono: &egui::FontId) {
        let id = egui::Id::new(INPUT_ID);
        let focused = ui.memory(|m| m.has_focus(id));
        let popup = self.manage_py.popup.is_some();
        let (up, down, enter, tab, esc, ctrl_space) = if focused {
            ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                    popup && i.consume_key(egui::Modifiers::NONE, egui::Key::Tab),
                    popup && i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                    i.consume_key(egui::Modifiers::CTRL, egui::Key::Space)
                        || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Space),
                )
            })
        } else {
            Default::default()
        };
        let mut accept = None;
        let mut submit = false;
        if let Some((start, items)) = &self.manage_py.popup {
            let last = items.len().saturating_sub(1);
            if down {
                self.manage_py.selected = (self.manage_py.selected + 1).min(last);
                self.manage_py.popup_focused = true;
            } else if up {
                self.manage_py.selected = self.manage_py.selected.saturating_sub(1);
                self.manage_py.popup_focused = true;
            } else if tab {
                accept = items.get(self.manage_py.selected).map(|c| c.text.clone());
            } else if enter {
                // In the manage.py console, Enter runs the typed line
                // unless the user invoked or navigated the lookup; a selection
                // that is already typed also just runs.
                let word = self.manage_py.input.get(*start..).unwrap_or("");
                accept = items
                    .get(self.manage_py.selected)
                    .filter(|c| self.manage_py.popup_focused && c.text != word)
                    .map(|c| c.text.clone());
                submit = accept.is_none();
            } else if esc {
                self.manage_py.popup = None;
            }
        } else if up || down {
            let state = &mut self.manage_py;
            let next = if up {
                if state.cursor.is_none() {
                    state.draft = state.input.clone();
                }
                state.history.older(state.cursor)
            } else {
                state.history.newer(state.cursor)
            };
            match next {
                Some(index) => {
                    state.cursor = Some(index);
                    state.input = state.history.entries[index].clone();
                }
                None if down && state.cursor.is_some() => {
                    state.cursor = None;
                    state.input = std::mem::take(&mut state.draft);
                }
                None => {}
            }
            set_cursor_end(ui.ctx(), &state.input);
        } else if enter {
            submit = true;
        }
        if let Some(text) = accept {
            self.manage_py.accept(ui.ctx(), &text);
        }
        if submit {
            self.manage_py.popup = None;
            self.manage_py.popup_focused = false;
        }
        let mut changed = false;
        let response = ui
            .horizontal(|ui| {
                ui.label(
                    egui::RichText::new("manage.py ›")
                        .font(mono.clone())
                        .color(theme.accent),
                );
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.manage_py.input)
                        .id(id)
                        .font(mono.clone())
                        .desired_width(f32::INFINITY)
                        .lock_focus(true)
                        .hint_text("command [options] — e.g. makemigrations blog"),
                );
                changed = response.changed();
                response
            })
            .inner;
        if self.manage_py.focus {
            response.request_focus();
            self.manage_py.focus = false;
        }
        if changed {
            self.manage_py.cursor = None;
            // Typing a word shows the list, without
            // focusing it: Enter still runs what was typed.
            self.manage_py.popup_focused = false;
            self.manage_py.update_popup(false);
        }
        if ctrl_space {
            self.manage_py.update_popup(true);
            self.manage_py.popup_focused = self.manage_py.popup.is_some();
        }
        // Lazily parse `help <command>` when options are being completed.
        if let Some((start, items)) = &self.manage_py.popup {
            let word = &self.manage_py.input[*start..];
            let command = self
                .manage_py
                .input
                .split_whitespace()
                .next()
                .map(str::to_string);
            if word.starts_with('-') || items.iter().any(|c| c.kind == CandidateKind::Option) {
                if let Some(command) = command {
                    self.request_manage_py_help(&command);
                }
            }
        }
        if let Some((_, items)) = &self.manage_py.popup {
            let mut clicked = None;
            egui::Area::new(id.with("popup"))
                .order(egui::Order::Foreground)
                .pivot(egui::Align2::LEFT_BOTTOM)
                .fixed_pos(response.rect.left_top() + egui::vec2(0.0, -2.0))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style())
                        .fill(theme.panel_bg)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("manage_py_completion_scroll")
                                .max_height(200.0)
                                .show(ui, |ui| {
                                    for (i, item) in items.iter().enumerate().take(200) {
                                        let help = self
                                            .manage_py
                                            .input
                                            .split_whitespace()
                                            .next()
                                            .and_then(|c| self.manage_py.source.help.get(c))
                                            .and_then(|h| {
                                                h.options
                                                    .iter()
                                                    .find(|o| o.flags.contains(&item.text))
                                            })
                                            .map(|o| o.help.as_str())
                                            .unwrap_or("");
                                        let label = egui::RichText::new(format!(
                                            "{:<28} {}",
                                            item.text,
                                            kind_label(item.kind)
                                        ))
                                        .font(mono.clone());
                                        let r = ui
                                            .selectable_label(i == self.manage_py.selected, label);
                                        let r = if help.is_empty() {
                                            r
                                        } else {
                                            r.on_hover_text(help)
                                        };
                                        if i == self.manage_py.selected {
                                            r.scroll_to_me(None);
                                        }
                                        if r.clicked() {
                                            clicked = Some(item.text.clone());
                                        }
                                    }
                                });
                        });
                });
            if let Some(text) = clicked {
                self.manage_py.accept(ui.ctx(), &text);
                response.request_focus();
            }
        }
        if submit {
            self.submit_manage_py_input();
            response.request_focus();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_follows_typed_words_and_accept_replaces_the_word() {
        let mut state = State {
            source: CompletionSource {
                commands: manage_py::merged_commands(&[], &[]),
                app_labels: vec!["blog".into()],
                ..Default::default()
            },
            input: "makemig".into(),
            ..Default::default()
        };
        state.update_popup(true);
        let (start, items) = state.popup.clone().unwrap();
        assert_eq!(start, 0);
        assert_eq!(items[0].text, "makemigrations");
        let ctx = egui::Context::default();
        state.accept(&ctx, "makemigrations");
        assert_eq!(state.input, "makemigrations ");
        assert!(state.popup.is_none());
        // An empty word only lists on Ctrl+Space.
        state.update_popup(false);
        assert!(state.popup.is_none());
        state.update_popup(true);
        assert_eq!(state.popup.as_ref().unwrap().1[0].text, "blog");
        state.accept(&ctx, "blog");
        assert_eq!(state.input, "makemigrations blog ");
    }

    fn console_frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
        ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
                ..Default::default()
            },
            |ui| {
                app.global_shortcuts(ui);
                egui::Panel::bottom("manage_py_test")
                    .exact_size(300.0)
                    .show(ui, |ui| app.manage_py_console_view(ui));
                egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
            },
        )
        .drop_without_applying_deltas();
    }

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// A project with an open Python editor and the focused manage.py console.
    fn console() -> (tempfile::TempDir, IdeApp, egui::Context) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("views.py");
        std::fs::write(&path, "import os\nos.pa").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        app.open_file(&path);
        app.focus_editor = false;
        app.open_manage_py_console();
        app.manage_py.source.commands = manage_py::merged_commands(&[], &[]);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        console_frame(&mut app, &ctx, 0.0, vec![]);
        console_frame(&mut app, &ctx, 0.02, vec![]);
        assert!(ctx.memory(|m| m.has_focus(egui::Id::new(INPUT_ID))));
        (root, app, ctx)
    }

    #[test]
    fn ctrl_space_in_the_console_completes_the_console_not_the_editor() {
        let (_root, mut app, ctx) = console();
        app.manage_py.input = "makemig".into();
        console_frame(
            &mut app,
            &ctx,
            0.04,
            vec![key(egui::Key::Space, egui::Modifiers::COMMAND)],
        );
        console_frame(&mut app, &ctx, 0.08, vec![]);
        assert!(
            !app.completion.visible,
            "the editor behind the console must not run Basic Completion"
        );
        let (_, items) = app.manage_py.popup.as_ref().expect("console lookup");
        assert_eq!(items[0].text, "makemigrations");
        // Ctrl+Space focused the lookup: Enter accepts its selection.
        console_frame(
            &mut app,
            &ctx,
            0.12,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
        );
        assert_eq!(app.manage_py.input, "makemigrations ");
        assert!(app.manage_py.history.entries.is_empty());
    }

    #[test]
    fn enter_with_an_unnavigated_typing_popup_runs_and_tab_accepts() {
        let (_root, mut app, ctx) = console();
        console_frame(
            &mut app,
            &ctx,
            0.04,
            vec![egui::Event::Text("check".into())],
        );
        assert!(app.manage_py.popup.is_some(), "typing shows the lookup");
        console_frame(
            &mut app,
            &ctx,
            0.08,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
        );
        assert_eq!(app.manage_py.history.entries, vec!["check".to_string()]);
        assert!(app.manage_py.input.is_empty() && app.manage_py.popup.is_none());
        // Tab still accepts the unnavigated selection.
        console_frame(
            &mut app,
            &ctx,
            0.12,
            vec![egui::Event::Text("makemig".into())],
        );
        console_frame(
            &mut app,
            &ctx,
            0.16,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
        );
        assert_eq!(app.manage_py.input, "makemigrations ");
        // A navigated selection is accepted by Enter rather than run.
        app.manage_py.input.clear();
        console_frame(&mut app, &ctx, 0.20, vec![egui::Event::Text("mi".into())]);
        console_frame(
            &mut app,
            &ctx,
            0.24,
            vec![key(egui::Key::ArrowDown, egui::Modifiers::NONE)],
        );
        let (_, items) = app.manage_py.popup.as_ref().expect("command lookup");
        let chosen = items[app.manage_py.selected].text.clone();
        console_frame(
            &mut app,
            &ctx,
            0.28,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
        );
        assert_eq!(app.manage_py.input, format!("{chosen} "));
        assert_eq!(app.manage_py.history.entries, vec!["check".to_string()]);
    }

    #[test]
    fn custom_command_options_complete_before_help_is_known() {
        let mut state = State {
            custom: vec![manage_py::CustomCommand {
                name: "import_feeds".into(),
                path: PathBuf::from("blog/management/commands/import_feeds.py"),
                options: vec!["--since".into()],
            }],
            ..Default::default()
        };
        state.rebuild_commands();
        assert!(state.source.commands.contains(&"import_feeds".to_string()));
        state.input = "import_feeds --si".into();
        state.update_popup(true);
        assert_eq!(state.popup.unwrap().1[0].text, "--since");
    }
}
