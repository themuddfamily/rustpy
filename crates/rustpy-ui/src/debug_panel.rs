//! Debug tool window: toolbar, threads and frames, variables, watches and the
//! debug console (roadmap §3.3); also the value trees of the Evaluate dialog
//! and of the editor's value popup.

use crate::a11y::AccessibleCombo;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use egui::{Color32, Pos2, Rect, Sense, Stroke, Vec2};
use rustpy_debug::model::apply_completion;
use rustpy_debug::{DebugSession, DebugState, ReplEntry, StepInTarget, Variable};
use rustpy_run::OutputKind;

use crate::actions::Action;
use crate::debug_console::ConsoleInput;
use crate::debug_editor::{
    cached_hover_variable, hover_expression, truncate, HoverTracker, InlineState, ValuePopup,
};
use crate::theme::UiTheme;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DebugTab {
    Debugger,
    Console,
}

pub enum DebugEvent {
    ForceRunToCursor,
    InstallDebugger(PathBuf),
    AttachProcess(u32, Option<PathBuf>),
    Navigate(PathBuf, usize, usize),
    Status(String),
    /// Run an IDE action (Restart, Run to Cursor, Mute Breakpoints, …).
    Action(Action),
    /// Open the Evaluate Expression dialog with this expression.
    Evaluate(String),
}

/// A row picked in the variables / watches trees (F2 edits it).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Selected {
    key: String,
    /// A variable that Set Value can change (not an evaluation result).
    settable: bool,
    /// Index of the watch when the row is a watch.
    watch: Option<usize>,
}

/// Set Value in progress.
#[derive(Clone, Debug)]
struct ValueEdit {
    key: String,
    container: i64,
    name: String,
    text: String,
    focus: bool,
    /// The suspension `container` belongs to.
    epoch: u64,
}

impl ValueEdit {
    fn new(key: &str, container: i64, v: &Variable, epoch: u64) -> ValueEdit {
        ValueEdit {
            key: key.to_string(),
            container,
            name: v.name.clone(),
            text: v.value.clone(),
            focus: true,
            epoch,
        }
    }
}

/// Where a tree row lives (for its context menu).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Variable,
    Watch(usize),
    Result,
}

type HoverFrame = (u64, u64, Option<i64>, Option<i64>);

fn hover_popup_key(frame: HoverFrame, expression: &str) -> String {
    format!(
        "h/{}/{}/{:?}/{:?}/{expression}",
        frame.0, frame.1, frame.2, frame.3
    )
}

pub struct DebugPanel {
    pub restart_requested: Option<std::time::Instant>,
    pub setup_task: Option<rustpy_debug::backend_tasks::BackendTask>,
    setup_interpreter: String,
    setup_processes: Vec<rustpy_debug::backend_tasks::PythonProcess>,
    setup_filter: String,
    setup_message: String,
    pub tab: DebugTab,
    /// Expanded tree rows by path (`s/Locals/items/0`): kept across steps.
    pub expanded: HashSet<String>,
    pub watch_input: String,
    pub repl_input: String,
    pub show_library_frames: bool,
    pub frames_width: f32,
    selected: Option<Selected>,
    tree_row_focus: Option<egui::Id>,
    editing: Option<ValueEdit>,
    /// F2 on this row: Set Value starts when the row is drawn next.
    edit_request: Option<String>,
    /// Watch being edited: (index, text, request focus).
    editing_watch: Option<(usize, String, bool)>,
    /// The panel got the last click: it claims F2 from the global keymap.
    pub focused: bool,
    console_focused: bool,
    completion_selected: usize,
    completion_dismissed: Option<u64>,
    /// The completion list was opened (Ctrl+Space or `.`) and follows typing.
    completing: bool,
    history: Vec<String>,
    history_pos: Option<usize>,
    /// Smart Step Into: waiting for / choosing among the calls of the line.
    pub smart_step: bool,
    smart_selected: usize,
    /// Calls on the current statement (filtered `step_in_targets`).
    smart_targets: Option<Vec<StepInTarget>>,
    pub inline: InlineState,
    pub value_popup: Option<ValuePopup>,
    pub hover: HoverTracker,
    hover_frame: Option<HoverFrame>,
    popup_left_since: Option<f64>,
    /// Files edited during the current session (debugpy's line moves do not apply there).
    pub edited_files: crate::breakpoints::EditedFiles,
    /// Breakpoint text typed in the breakpoint popup or dialog, applied once complete.
    pub breakpoint_edits: crate::breakpoints::PendingEdits,
}

impl Default for DebugPanel {
    fn default() -> Self {
        Self {
            restart_requested: None,
            setup_task: None,
            setup_interpreter: "python".into(),
            setup_processes: Vec::new(),
            setup_filter: String::new(),
            setup_message: String::new(),
            tab: DebugTab::Debugger,
            expanded: HashSet::new(),
            watch_input: String::new(),
            repl_input: String::new(),
            show_library_frames: false,
            frames_width: 340.0,
            selected: None,
            tree_row_focus: None,
            editing: None,
            edit_request: None,
            editing_watch: None,
            focused: false,
            console_focused: false,
            completion_selected: 0,
            completion_dismissed: None,
            completing: false,
            history: Vec::new(),
            history_pos: None,
            smart_step: false,
            smart_selected: 0,
            smart_targets: None,
            inline: InlineState::default(),
            value_popup: None,
            hover: HoverTracker::default(),
            hover_frame: None,
            popup_left_since: None,
            edited_files: Default::default(),
            breakpoint_edits: Default::default(),
        }
    }
}

/// Open the existing tree menu from its focused row without needing a pointer.
/// Pointer menus retain their normal pointer anchor; keyboard menus remain at
/// the captured row while focus moves through the menu's ordinary controls.
fn tree_context_menu(response: &egui::Response) -> egui::Popup<'_> {
    let id = egui::Popup::default_response_id(response);
    let anchor_id = id.with("keyboard-anchor");
    if !egui::Popup::is_id_open(&response.ctx, id) {
        response.ctx.data_mut(|data| data.remove::<Rect>(anchor_id));
    }
    let open = response.has_focus()
        && !egui::Popup::is_any_open(&response.ctx)
        && response
            .ctx
            .input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, egui::Key::F10));
    let popup = egui::Popup::context_menu(response);
    if open {
        response
            .ctx
            .data_mut(|data| data.insert_temp(anchor_id, response.rect));
        egui::Popup::open_id(&response.ctx, id);
    }
    match response.ctx.data(|data| data.get_temp::<Rect>(anchor_id)) {
        Some(anchor) => popup.anchor(anchor),
        None => popup,
    }
}

/// Paints a tree expander triangle (the fonts lack ▼).
fn paint_expander(painter: &egui::Painter, c: Pos2, expanded: bool, color: Color32) {
    let s = 3.6;
    let pts = if expanded {
        vec![
            c + Vec2::new(-s, -s * 0.55),
            c + Vec2::new(s, -s * 0.55),
            c + Vec2::new(0.0, s * 0.75),
        ]
    } else {
        vec![
            c + Vec2::new(-s * 0.55, -s),
            c + Vec2::new(s * 0.75, 0.0),
            c + Vec2::new(-s * 0.55, s),
        ]
    };
    painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// Calls of the current statement among `targets` (debugpy offers every call left
/// in the frame); all of them when none matches.
pub fn filter_step_targets(targets: &[StepInTarget], statement: &str) -> Vec<StepInTarget> {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let stmt = squash(statement);
    let on_line: Vec<StepInTarget> = targets
        .iter()
        .filter(|t| {
            let label = t
                .label
                .rsplit_once(" (call ")
                .map(|(l, _)| l)
                .unwrap_or(&t.label);
            let label = squash(label);
            !label.is_empty() && stmt.contains(&label)
        })
        .cloned()
        .collect();
    if on_line.is_empty() {
        targets.to_vec()
    } else {
        on_line
    }
}

/// The statement starting at 1-based `line` of `path` as on disk (continuation
/// lines while brackets are open).
fn statement_on_disk(path: &Path, line: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let mut out = String::new();
    let mut depth: i32 = 0;
    for l in text.lines().skip(line.saturating_sub(1)).take(20) {
        out.push_str(l);
        out.push('\n');
        for c in l.split('#').next().unwrap_or("").chars() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
        }
        if depth <= 0 && !l.trim_end().ends_with('\\') {
            break;
        }
    }
    out
}

impl DebugPanel {
    pub fn discover_python_processes(&mut self, interpreter: Option<&Path>) {
        if let Some(interpreter) = interpreter {
            self.setup_interpreter = interpreter.to_string_lossy().into_owned();
        }
        self.setup_task = Some(rustpy_debug::backend_tasks::BackendTask::processes());
    }

    /// Whether the panel handles the key that the keymap resolved to `action`
    /// itself: F2 (Set Value / Edit Watch) on a selected tree row and completion
    /// (Ctrl+Space) in the debug console, whatever the keymap binds them to.
    pub fn claims(&self, action: Action, key: egui::Key, mods: egui::Modifiers) -> bool {
        let on_row = self.focused
            && self.selected.is_some()
            && self.editing.is_none()
            && self.editing_watch.is_none();
        (on_row && key == egui::Key::F2 && mods.is_none())
            || (self.console_focused
                && (action == Action::CodeCompletion
                    || (key == egui::Key::Space && mods.command_only())))
    }

    /// Shift F10 retains the IDE Run binding everywhere except the actual
    /// focused variable row, where it opens that row's context actions.
    pub fn claims_tree_menu(
        &self,
        ctx: &egui::Context,
        key: egui::Key,
        mods: egui::Modifiers,
    ) -> bool {
        key == egui::Key::F10
            && mods == egui::Modifiers::SHIFT
            && self.editing.is_none()
            && self.editing_watch.is_none()
            && !egui::Popup::is_any_open(ctx)
            && self
                .tree_row_focus
                .is_some_and(|row| ctx.memory(|memory| memory.has_focus(row)))
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        session: Option<&mut DebugSession>,
        mono: egui::FontId,
    ) -> Vec<DebugEvent> {
        let mut events = Vec::new();
        if let Some(result) = self.setup_task.as_ref().and_then(|task| task.poll()) {
            self.setup_task = None;
            match result {
                Ok(rustpy_debug::backend_tasks::TaskResult::Installed(path)) => {
                    self.setup_message = format!("debugpy installed for {}", path.display())
                }
                Ok(rustpy_debug::backend_tasks::TaskResult::Processes(rows)) => {
                    self.setup_processes = rows
                }
                Err(error) => self.setup_message = error,
            }
        }
        ui.collapsing("Debugger setup and attach", |ui| {
            ui.horizontal(|ui| {
                ui.label("Interpreter");
                ui.text_edit_singleline(&mut self.setup_interpreter);
                if ui
                    .add_enabled(
                        self.setup_task.is_none() && !self.setup_interpreter.trim().is_empty(),
                        egui::Button::new("Install debugpy"),
                    )
                    .clicked()
                {
                    events.push(DebugEvent::InstallDebugger(
                        self.setup_interpreter.trim().into(),
                    ));
                }
                if ui
                    .add_enabled(
                        self.setup_task.is_none(),
                        egui::Button::new("Refresh Python processes"),
                    )
                    .clicked()
                {
                    self.setup_task = Some(rustpy_debug::backend_tasks::BackendTask::processes());
                }
                if self.setup_task.is_some() {
                    crate::a11y::spinner(ui);
                    if ui.button("Cancel").clicked() {
                        self.setup_task = None;
                    }
                }
            });
            ui.label(&self.setup_message);
            if !self.setup_processes.is_empty() {
                ui.horizontal(|ui| {
                    ui.label("Filter");
                    ui.text_edit_singleline(&mut self.setup_filter);
                });
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for process in &self.setup_processes {
                            let text = format!("{} {}", process.pid, process.command);
                            if !self.setup_filter.is_empty() && !text.contains(&self.setup_filter) {
                                continue;
                            }
                            if ui.button(text).clicked() {
                                events.push(DebugEvent::AttachProcess(
                                    process.pid,
                                    Some(self.setup_interpreter.trim().into()),
                                ));
                            }
                        }
                    });
            }
            if self.setup_task.is_some() {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
        });
        let Some(session) = session else {
            ui.label(
                egui::RichText::new(
                    "No debug session. Use Debug 🐞 (Shift+F9) with a breakpoint set (Ctrl+F8).",
                )
                .color(theme.text_dim),
            );
            return events;
        };
        let panel_rect = ui.max_rect();
        if ui.input(|i| i.pointer.any_pressed()) {
            self.focused = ui
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|p| panel_rect.contains(p));
        }
        // A Set Value edit belongs to the suspension it started in.
        if self
            .editing
            .as_ref()
            .is_some_and(|e| !session.is_stopped() || e.epoch != session.stop_epoch)
        {
            self.editing = None;
        }
        self.inline.update(session);
        self.toolbar(ui, theme, session, &mut events);
        if let Some(path) = crate::debug_protocol_workspace::show(ui, session) {
            events.push(DebugEvent::Navigate(path, 1, 1));
        }
        if ui
            .add_enabled(
                session.is_stopped(),
                egui::Button::new("Force Run to Cursor"),
            )
            .clicked()
        {
            events.push(DebugEvent::ForceRunToCursor);
        }
        if let Some((path, line)) = crate::debug_backend_workspace::show(ui, session) {
            events.push(DebugEvent::Navigate(path, line, 1));
        }
        ui.separator();
        match self.tab {
            DebugTab::Debugger => self.debugger_tab(ui, theme, session, &mut events),
            DebugTab::Console => self.console_tab(ui, theme, session, mono, &mut events),
        }
        self.smart_step_chooser(ui, theme, session);
        if session.is_active() && (self.editing.is_some() || self.smart_step) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        events
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        session: &mut DebugSession,
        events: &mut Vec<DebugEvent>,
    ) {
        ui.horizontal(|ui| {
            let stopped = session.is_stopped();
            let active = session.is_active();
            let icon = |ui: &mut egui::Ui, enabled: bool, text: egui::RichText, tip: &str| {
                ui.add_enabled(
                    enabled,
                    egui::Button::new(text).min_size(Vec2::new(26.0, 0.0)),
                )
                .on_hover_text(tip)
                .on_disabled_hover_text(tip)
                .clicked()
            };
            if icon(
                ui,
                stopped,
                egui::RichText::new("▶").color(theme.success),
                "Resume Program (F9)",
            ) {
                session.continue_();
            }
            if icon(
                ui,
                active && !stopped,
                egui::RichText::new("⏸"),
                "Pause Program",
            ) {
                session.pause();
            }
            if icon(
                ui,
                active,
                egui::RichText::new("■").color(theme.error),
                "Stop (Ctrl+F2)",
            ) {
                session.stop();
            }
            if icon(
                ui,
                true,
                egui::RichText::new("⟳").color(theme.success),
                "Restart (Ctrl+F5)",
            ) {
                events.push(DebugEvent::Action(Action::RestartDebug));
            }
            ui.separator();
            if icon(ui, stopped, egui::RichText::new("⤵"), "Step Over (F8)") {
                session.step_over();
            }
            if icon(ui, stopped, egui::RichText::new("⬇"), "Step Into (F7)") {
                session.step_into();
            }
            if icon(
                ui,
                stopped,
                egui::RichText::new("⏬"),
                "Smart Step Into (Shift+F7): choose the call to enter",
            ) {
                events.push(DebugEvent::Action(Action::SmartStepInto));
            }
            if icon(ui, stopped, egui::RichText::new("⬆"), "Step Out (Shift+F8)") {
                session.step_out();
            }
            if icon(
                ui,
                stopped,
                egui::RichText::new("⏩"),
                "Run to Cursor (Alt+F9)",
            ) {
                events.push(DebugEvent::Action(Action::RunToCursor));
            }
            ui.separator();
            if icon(
                ui,
                stopped,
                egui::RichText::new("◎"),
                "Show Execution Point (Alt+F10)",
            ) {
                events.push(DebugEvent::Action(Action::ShowExecutionPoint));
            }
            if icon(
                ui,
                stopped,
                egui::RichText::new("🖩"),
                "Evaluate Expression (Alt+F8)",
            ) {
                events.push(DebugEvent::Evaluate(String::new()));
            }
            if icon(
                ui,
                true,
                egui::RichText::new("⏺").color(Color32::from_rgb(0xDB, 0x5C, 0x5C)),
                "View Breakpoints (Ctrl+Shift+F8)",
            ) {
                events.push(DebugEvent::Action(Action::ViewBreakpoints));
            }
            if ui
                .add(egui::Button::selectable(session.muted, "🔇"))
                .on_hover_text(if session.muted {
                    "Breakpoints are muted: click to unmute"
                } else {
                    "Mute Breakpoints"
                })
                .clicked()
            {
                events.push(DebugEvent::Action(Action::MuteBreakpoints));
            }
            ui.separator();
            if ui
                .selectable_label(self.tab == DebugTab::Debugger, "Debugger")
                .clicked()
            {
                self.tab = DebugTab::Debugger;
            }
            if ui
                .selectable_label(self.tab == DebugTab::Console, "Console")
                .clicked()
            {
                self.tab = DebugTab::Console;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let status = match &session.state {
                    DebugState::Starting => "Connecting to debugger…".to_string(),
                    DebugState::Running => "Running".to_string(),
                    DebugState::Stopped {
                        reason,
                        description,
                        ..
                    } => {
                        if let Some((id, desc, _)) = &session.exception_info {
                            format!("Stopped: {id}: {desc}")
                        } else if description.is_empty() {
                            format!("Stopped ({reason})")
                        } else {
                            format!("Stopped ({reason}): {description}")
                        }
                    }
                    DebugState::Terminated(code) => format!(
                        "Finished (exit code {})",
                        code.map(|c| c.to_string()).unwrap_or_else(|| "?".into())
                    ),
                    DebugState::Failed(e) => format!("Failed: {e}"),
                };
                let color = match &session.state {
                    DebugState::Stopped { .. } => theme.warning,
                    DebugState::Failed(_) => theme.error,
                    _ => theme.text_dim,
                };
                ui.add(egui::Label::new(egui::RichText::new(status).color(color)).truncate());
            });
        });
    }

    fn debugger_tab(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        session: &mut DebugSession,
        events: &mut Vec<DebugEvent>,
    ) {
        egui::Panel::left("debug_frames")
            .resizable(true)
            .default_size(self.frames_width)
            .min_size(200.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Frames").strong());
                    let current = session.selected_thread;
                    let name_of = |id: Option<i64>| {
                        session
                            .threads
                            .iter()
                            .find(|t| Some(t.0) == id)
                            .map(|t| t.1.clone())
                            .unwrap_or_else(|| "—".into())
                    };
                    if session.is_stopped() && session.threads.len() > 1 {
                        let mut pick = current;
                        egui::ComboBox::from_id_salt("debug_thread")
                            .selected_text(name_of(current))
                            .width(150.0)
                            .show_ui(ui, |ui| {
                                for (id, name) in &session.threads {
                                    let label = if Some(*id) == session.stopped_thread() {
                                        format!("{name} (suspended here)")
                                    } else {
                                        name.clone()
                                    };
                                    ui.selectable_value(&mut pick, Some(*id), label);
                                }
                            })
                            .combo_named(crate::i18n::tr_static("Thread"));
                        if let Some(t) = pick.filter(|t| Some(*t) != current) {
                            session.select_thread(t);
                        }
                    } else {
                        ui.label(
                            egui::RichText::new(name_of(
                                current.or_else(|| session.stopped_thread()),
                            ))
                            .color(theme.text_dim),
                        );
                    }
                    ui.checkbox(&mut self.show_library_frames, "Library frames");
                });
                ui.separator();
                let mut select: Option<usize> = None;
                egui::ScrollArea::vertical()
                    .id_salt("debug_frames_list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, f) in session.frames.iter().enumerate() {
                            if f.is_library
                                && !self.show_library_frames
                                && i != session.selected_frame
                            {
                                continue;
                            }
                            let file = f
                                .path
                                .as_ref()
                                .and_then(|p| p.file_name())
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            let text = format!("{}, {}:{}", f.name, file, f.line);
                            let color = if f.is_library {
                                theme.text_dim
                            } else {
                                theme.text
                            };
                            let resp = ui.selectable_label(
                                i == session.selected_frame,
                                egui::RichText::new(text).color(color),
                            );
                            if resp.clicked() {
                                select = Some(i);
                            }
                        }
                        if session.frames.is_empty() {
                            ui.label(
                                egui::RichText::new(if session.is_stopped() {
                                    "Loading frames…"
                                } else {
                                    "Frames are available when the program is suspended"
                                })
                                .color(theme.text_dim),
                            );
                        }
                    });
                if let Some(i) = select {
                    session.select_frame(i);
                    if let Some(f) = session.frames.get(i) {
                        if let Some(p) = &f.path {
                            events.push(DebugEvent::Navigate(p.clone(), f.line, f.column));
                        } else {
                            let id = f.id;
                            session.request_frame_source(id);
                        }
                    }
                }
            });
        let changed = self.inline.changed_names(session);
        egui::ScrollArea::vertical()
            .id_salt("debug_vars")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Variables").strong());
                if session.scopes.is_empty() {
                    let text = if session.is_stopped() {
                        "Loading…"
                    } else if session.is_active() {
                        "No variables while the program is running"
                    } else {
                        "The program has finished"
                    };
                    ui.label(egui::RichText::new(text).color(theme.text_dim));
                }
                let scopes = session.scopes.clone();
                for (si, scope) in scopes.iter().enumerate() {
                    let header = format!(
                        "{}{}",
                        scope.name,
                        if scope.expensive { " (expensive)" } else { "" }
                    );
                    let open_default = scope.name.starts_with("Local");
                    let id = ui.make_persistent_id(("scope", scope.name.as_str()));
                    egui::collapsing_header::CollapsingState::load_with_default_open(
                        ui.ctx(),
                        id,
                        open_default,
                    )
                    .show_header(ui, |ui| {
                        ui.label(egui::RichText::new(header).color(theme.text));
                    })
                    .body(|ui| {
                        session.request_variables(scope.variables_reference);
                        let flagged = if si == 0 { &changed } else { &HashSet::new() };
                        self.variables_tree(
                            ui,
                            theme,
                            session,
                            scope.variables_reference,
                            &format!("s/{}", scope.name),
                            0,
                            flagged,
                            events,
                        );
                    });
                }
                ui.separator();
                self.watches(ui, theme, session, events);
            });
        self.tree_keys(ui, session);
    }

    fn watches(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        session: &mut DebugSession,
        events: &mut Vec<DebugEvent>,
    ) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Watches").strong());
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.watch_input)
                    .hint_text("Add watch expression…")
                    .desired_width(220.0),
            );
            if ((resp.lost_focus() || resp.has_focus())
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                || ui.small_button("+").on_hover_text("New Watch").clicked())
                && !self.watch_input.trim().is_empty()
            {
                let expr = self.watch_input.trim().to_string();
                session.add_watch(&expr);
                self.watch_input.clear();
            }
        });
        let mut remove: Option<usize> = None;
        let watches = session.watches.clone();
        let stopped = session.is_stopped();
        for (i, w) in watches.iter().enumerate() {
            if let Some((idx, text, focus)) = &mut self.editing_watch {
                if *idx == i {
                    let mut done: Option<bool> = None;
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        let r = ui.add(
                            egui::TextEdit::singleline(text)
                                .desired_width(ui.available_width() - 8.0),
                        );
                        if *focus {
                            r.request_focus();
                            *focus = false;
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            done = Some(false);
                        } else if r.lost_focus() {
                            done = Some(
                                ui.input(|i| i.key_pressed(egui::Key::Enter))
                                    || !ui.input(|i| i.key_pressed(egui::Key::Escape)),
                            );
                        }
                    });
                    match done {
                        Some(true) => {
                            let expr = text.trim().to_string();
                            if expr.is_empty() {
                                remove = Some(i);
                            } else {
                                session.set_watch_expression(i, &expr);
                            }
                            self.editing_watch = None;
                        }
                        Some(false) => self.editing_watch = None,
                        None => {}
                    }
                    continue;
                }
            }
            let key = format!("w/{i}:{}", w.expression);
            match (&w.result, &w.error) {
                (Some(v), _) => {
                    let shown = Variable {
                        name: w.expression.clone(),
                        ..v.clone()
                    };
                    self.var_row(
                        ui,
                        theme,
                        session,
                        &shown,
                        None,
                        &key,
                        0,
                        false,
                        RowKind::Watch(i),
                        stopped,
                        events,
                    );
                }
                (None, error) => {
                    let (text, color) = match error {
                        Some(e) => (
                            format!("{} = {}", w.expression, e.lines().last().unwrap_or(e)),
                            theme.error,
                        ),
                        None => (format!("{} = …", w.expression), theme.text_dim),
                    };
                    let shown = Variable {
                        name: w.expression.clone(),
                        value: String::new(),
                        type_name: String::new(),
                        variables_reference: 0,
                        evaluate_name: Some(w.expression.clone()),
                    };
                    let resp = ui.add(
                        egui::Label::new(egui::RichText::new(format!("      {text}")).color(color))
                            .sense(Sense::click()),
                    );
                    self.watch_menu(&resp, session, i, &shown, events);
                    if resp.clicked() {
                        self.selected = Some(Selected {
                            key: key.clone(),
                            settable: false,
                            watch: Some(i),
                        });
                    }
                    if resp.double_clicked() {
                        self.editing_watch = Some((i, w.expression.clone(), true));
                    }
                }
            }
        }
        if !watches.is_empty() {
            ui.label(
                egui::RichText::new("Double-click or F2 edits a watch; Delete removes it.")
                    .color(theme.text_dim)
                    .small(),
            );
        }
        if let Some(i) = remove {
            session.remove_watch(i);
            self.selected = None;
        }
    }

    /// F2 / Delete / Escape on the selected tree row.
    fn tree_keys(&mut self, ui: &egui::Ui, session: &mut DebugSession) {
        if !self.focused
            || self.editing.is_some()
            || self.editing_watch.is_some()
            || ui.memory(|m| m.focused().is_some())
        {
            return;
        }
        let Some(sel) = self.selected.clone() else {
            return;
        };
        if ui.input(|i| i.key_pressed(egui::Key::F2)) {
            match (sel.watch, sel.settable) {
                (Some(i), _) => {
                    self.editing_watch = session
                        .watches
                        .get(i)
                        .map(|w| (i, w.expression.clone(), true))
                }
                // The row starts the edit with its current container and value
                // (variable references change at every suspension).
                (None, true) if session.is_stopped() => self.edit_request = Some(sel.key.clone()),
                _ => {}
            }
        }
        if let Some(i) = sel.watch {
            if ui.input(|inp| inp.key_pressed(egui::Key::Delete)) {
                session.remove_watch(i);
                self.selected = None;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn variables_tree(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        session: &mut DebugSession,
        reference: i64,
        key: &str,
        depth: usize,
        changed: &HashSet<String>,
        events: &mut Vec<DebugEvent>,
    ) {
        let Some(mut vars) = session.variables.get(&reference).cloned() else {
            ui.horizontal(|ui| {
                ui.add_space(20.0 + depth as f32 * 14.0);
                ui.label(egui::RichText::new("…").color(theme.text_dim));
            });
            return;
        };
        if vars.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(20.0 + depth as f32 * 14.0);
                ui.label(egui::RichText::new("(empty)").color(theme.text_dim));
            });
        }
        vars.retain(|v| {
            (!v.name.starts_with("__") || session.options.backend.show_special)
                && (!v.name.starts_with('_')
                    || v.name.starts_with("__")
                    || session.options.backend.show_protected)
        });
        if session.options.backend.sort_variables {
            vars.sort_by(|a, b| a.name.cmp(&b.name));
        }
        let stopped = session.is_stopped();
        for v in vars {
            let row_key = format!("{key}/{}", v.name);
            let flag = depth == 0 && changed.contains(&v.name);
            self.var_row(
                ui,
                theme,
                session,
                &v,
                Some(reference),
                &row_key,
                depth,
                flag,
                RowKind::Variable,
                stopped,
                events,
            );
        }
        if session.backend.page_more.contains(&reference)
            && ui.button("Load more variables").clicked()
        {
            let start = session
                .backend
                .page_counts
                .get(&reference)
                .copied()
                .unwrap_or(0);
            session.request_variables_page(reference, start);
        }
    }

    /// One tree row: expander, name, value and type; children when expanded.
    #[allow(clippy::too_many_arguments)]
    fn var_row(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        session: &mut DebugSession,
        v: &Variable,
        container: Option<i64>,
        key: &str,
        depth: usize,
        changed: bool,
        kind: RowKind,
        live: bool,
        events: &mut Vec<DebugEvent>,
    ) {
        let expandable = v.variables_reference != 0 && depth < 12 && live;
        let expanded = expandable && self.expanded.contains(key);
        let indent = 6.0 + depth as f32 * 14.0;
        if self.edit_request.as_deref() == Some(key) {
            self.edit_request = None;
            if let (Some(c), true) = (container, live) {
                self.editing = Some(ValueEdit::new(key, c, v, session.stop_epoch));
            }
        }
        if let Some(edit) = self.editing.as_mut().filter(|e| e.key == key) {
            let mut done: Option<bool> = None;
            ui.horizontal(|ui| {
                ui.add_space(indent + 14.0);
                ui.label(egui::RichText::new(format!("{} =", v.name)).color(
                    if theme.high_contrast {
                        theme.text
                    } else {
                        Color32::from_rgb(0xC8, 0x8F, 0xD6)
                    },
                ));
                let r = ui.add(
                    egui::TextEdit::singleline(&mut edit.text)
                        .desired_width(ui.available_width() - 8.0)
                        .hint_text("Python expression"),
                );
                if edit.focus {
                    r.request_focus();
                    edit.focus = false;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                } else if r.lost_focus() {
                    done = Some(ui.input(|i| i.key_pressed(egui::Key::Enter)));
                }
            });
            match done {
                Some(true) => {
                    let (container, name, text) = (
                        edit.container,
                        edit.name.clone(),
                        edit.text.trim().to_string(),
                    );
                    self.editing = None;
                    if !text.is_empty() {
                        session.set_variable(container, &name, &text);
                    }
                }
                Some(false) => self.editing = None,
                None => {}
            }
            return;
        }
        // Stable only for the current session, suspension and selected frame.
        let row_id = ui.id().with((
            "debug_variable",
            session.id,
            session.stop_epoch,
            session.selected_thread,
            session.current_frame().map(|frame| frame.id),
            key,
        ));
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), Sense::hover());
        let resp = ui.interact(rect, row_id, Sense::click());
        if resp.has_focus() {
            self.tree_row_focus = Some(resp.id);
            self.focused = true;
            self.selected = Some(Selected {
                key: key.to_string(),
                settable: container.is_some(),
                watch: if let RowKind::Watch(i) = kind {
                    Some(i)
                } else {
                    None
                },
            });
        }
        let keyboard =
            resp.has_focus() && self.editing_watch.is_none() && !egui::Popup::is_any_open(ui.ctx());
        let (expand_key, collapse_key, toggle_key, edit_key, delete_key) = if keyboard {
            ui.input_mut(|input| {
                (
                    input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
                    input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                    input.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                    input.consume_key(egui::Modifiers::NONE, egui::Key::F2),
                    input.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
                )
            })
        } else {
            (false, false, false, false, false)
        };
        // egui queues cardinal focus traversal before widgets consume keys.
        // These arrows belong to this focused row's expansion, not its neighbor.
        if expand_key || collapse_key {
            ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        }
        if edit_key {
            match kind {
                RowKind::Watch(i) => {
                    self.editing_watch = session
                        .watches
                        .get(i)
                        .map(|watch| (i, watch.expression.clone(), true));
                }
                RowKind::Variable if live && session.is_stopped() => {
                    if let Some(container) = container {
                        self.editing = Some(ValueEdit::new(key, container, v, session.stop_epoch));
                    }
                }
                _ => {}
            }
        }
        if delete_key {
            if let RowKind::Watch(i) = kind {
                session.remove_watch(i);
                self.selected = None;
            }
        }
        let is_selected = self.selected.as_ref().is_some_and(|s| s.key == key);
        let label = format!(
            "{}: {}{}{}{}",
            truncate(&v.name, 160),
            truncate(&v.value.replace('\n', "↩"), 320),
            if v.type_name.is_empty() {
                String::new()
            } else {
                format!("; type {}", truncate(&v.type_name, 80))
            },
            if changed { "; changed" } else { "" },
            if live {
                ""
            } else {
                "; not available in the current suspension"
            },
        );
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, is_selected, &label)
        });
        ui.ctx().accesskit_node_builder(resp.id, |node| {
            node.clear_toggled();
            node.set_selected(is_selected);
            if expandable {
                node.set_expanded(expanded);
            }
        });
        if keyboard {
            crate::a11y::announce(
                ui.ctx(),
                resp.id,
                &format!(
                    "{label}. {}. Tab to next value; Shift Tab to previous value; Shift F10 opens actions{}",
                    if expandable {
                        if expanded {
                            "Expanded"
                        } else {
                            "Collapsed"
                        }
                    } else {
                        "Leaf value"
                    },
                    if expandable {
                        "; Right expands, Left collapses, Enter toggles"
                    } else {
                        ""
                    },
                ),
            );
        }
        if is_selected {
            ui.painter().rect_filled(rect, 2.0, theme.selection_bg);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 2.0, theme.hover_bg);
        }
        let x = rect.left() + indent;
        let y = rect.center().y;
        if expandable {
            paint_expander(
                ui.painter(),
                Pos2::new(x + 4.0, y),
                expanded,
                theme.text_dim,
            );
        }
        let name_color = if v.name.starts_with("__") {
            theme.text_dim
        } else if theme.high_contrast {
            theme.text
        } else {
            Color32::from_rgb(0xC8, 0x8F, 0xD6)
        };
        let name_galley = ui.fonts_mut(|f| {
            f.layout_no_wrap(v.name.clone(), egui::FontId::proportional(13.0), name_color)
        });
        ui.painter().galley(
            Pos2::new(x + 14.0, y - name_galley.size().y / 2.0),
            name_galley.clone(),
            name_color,
        );
        // Group nodes ("special variables") have neither value nor type.
        let group = v.value.is_empty() && v.type_name.is_empty();
        // "↩" marks line breaks: the proportional font has no "⏎".
        let value = if group {
            String::new()
        } else {
            format!(" = {}", truncate(&v.value.replace('\n', "↩"), 160))
        };
        let value_color = if changed { theme.accent } else { theme.text };
        let g = ui
            .fonts_mut(|f| f.layout_no_wrap(value, egui::FontId::proportional(13.0), value_color));
        let vx = x + 14.0 + name_galley.size().x;
        ui.painter().with_clip_rect(rect).galley(
            Pos2::new(vx, y - g.size().y / 2.0),
            g.clone(),
            value_color,
        );
        if !v.type_name.is_empty() {
            ui.painter().with_clip_rect(rect).text(
                Pos2::new(vx + g.size().x + 8.0, y),
                egui::Align2::LEFT_CENTER,
                format!("{{{}}}", v.type_name),
                egui::FontId::proportional(11.0),
                theme.text_dim,
            );
        }
        let on_arrow = resp.interact_pointer_pos().is_some_and(|p| p.x < x + 12.0);
        if resp.clicked() {
            self.selected = Some(Selected {
                key: key.to_string(),
                settable: container.is_some(),
                watch: if let RowKind::Watch(i) = kind {
                    Some(i)
                } else {
                    None
                },
            });
        }
        let toggle =
            expandable && ((resp.clicked() && on_arrow) || resp.double_clicked() || toggle_key);
        if expandable && (toggle || expand_key || collapse_key) {
            if collapse_key || (toggle && expanded) {
                self.expanded.remove(key);
            } else {
                self.expanded.insert(key.to_string());
                session.request_variables(v.variables_reference);
            }
        } else if resp.double_clicked() {
            match kind {
                RowKind::Watch(i) => {
                    self.editing_watch = Some((
                        i,
                        session
                            .watches
                            .get(i)
                            .map(|w| w.expression.clone())
                            .unwrap_or_default(),
                        true,
                    ))
                }
                RowKind::Variable if session.is_stopped() => {
                    if let Some(c) = container {
                        self.editing = Some(ValueEdit::new(key, c, v, session.stop_epoch));
                    }
                }
                _ => {}
            }
        }
        match kind {
            RowKind::Watch(i) => self.watch_menu(&resp, session, i, v, events),
            _ => self.variable_menu(&resp, session, v, container, key, events),
        }
        if !v.value.is_empty() {
            resp.on_hover_text(truncate(&v.value, 2000));
        }
        if expanded {
            if !session.variables.contains_key(&v.variables_reference) {
                session.request_variables(v.variables_reference);
            }
            self.variables_tree(
                ui,
                theme,
                session,
                v.variables_reference,
                key,
                depth + 1,
                &HashSet::new(),
                events,
            );
        }
    }

    fn variable_menu(
        &mut self,
        resp: &egui::Response,
        session: &mut DebugSession,
        v: &Variable,
        container: Option<i64>,
        key: &str,
        events: &mut Vec<DebugEvent>,
    ) {
        let expr = v.evaluate_name.clone().unwrap_or_else(|| v.name.clone());
        tree_context_menu(resp).show(|ui| {
            if ui
                .add_enabled(
                    container.is_some() && session.is_stopped(),
                    egui::Button::new("Set Value…").shortcut_text("F2"),
                )
                .clicked()
            {
                if let Some(c) = container {
                    self.editing = Some(ValueEdit::new(key, c, v, session.stop_epoch));
                }
                ui.close();
            }
            ui.separator();
            if ui.button("Copy Value").clicked() {
                ui.ctx().copy_text(v.value.clone());
                ui.close();
            }
            if ui.button("Copy Name").clicked() {
                ui.ctx().copy_text(v.name.clone());
                ui.close();
            }
            if ui.button("Copy as Expression").clicked() {
                ui.ctx().copy_text(expr.clone());
                ui.close();
            }
            ui.separator();
            if ui
                .add_enabled(!expr.contains(' '), egui::Button::new("Add to Watches"))
                .clicked()
            {
                session.add_watch(&expr);
                ui.close();
            }
            if ui
                .add_enabled(
                    !expr.contains(' '),
                    egui::Button::new("Evaluate Expression…").shortcut_text("Alt+F8"),
                )
                .clicked()
            {
                events.push(DebugEvent::Evaluate(expr.clone()));
                ui.close();
            }
            if ui.button("Add Inline Watch").clicked() {
                let location = session
                    .current_frame()
                    .and_then(|frame| frame.path.clone().map(|path| (path, frame.line)));
                if let Some(location) = location {
                    session
                        .backend
                        .inline_watches
                        .insert(expr.clone(), location);
                    if !session.watches.iter().any(|watch| watch.expression == expr) {
                        session.add_watch(&expr);
                    }
                }
                ui.close();
            }
            if ui.button("Evaluate in Console").clicked() {
                rustpy_console::debugger::evaluate_in_console(session, &expr);
                self.tab = DebugTab::Console;
                ui.close();
            }
            ui.menu_button("View value", |ui| {
                for (label, kind) in [
                    ("Text", "text"),
                    ("JSON", "json"),
                    ("Bytes", "bytes"),
                    ("HTML source", "html"),
                    ("Image", "image"),
                    ("Array / dataframe / tensor", "table"),
                    ("Referrers", "referrers"),
                    ("Object source", "source"),
                    ("Type source", "type_source"),
                ] {
                    if ui.button(crate::i18n::tr_static(label)).clicked() {
                        session.inspect_value(&expr, kind);
                        ui.close();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Object label");
                ui.text_edit_singleline(&mut session.backend.label_input);
                if ui.button("Mark").clicked() {
                    let label = session.backend.label_input.clone();
                    if !label.trim().is_empty() {
                        session.mark_object(&expr, &label);
                    }
                }
            });
        });
    }

    fn watch_menu(
        &mut self,
        resp: &egui::Response,
        session: &mut DebugSession,
        i: usize,
        v: &Variable,
        events: &mut Vec<DebugEvent>,
    ) {
        let count = session.watches.len();
        tree_context_menu(resp).show(|ui| {
            if ui
                .add(egui::Button::new("Edit Watch").shortcut_text("F2"))
                .clicked()
            {
                self.editing_watch = session
                    .watches
                    .get(i)
                    .map(|w| (i, w.expression.clone(), true));
                ui.close();
            }
            if ui
                .add(egui::Button::new("Remove Watch").shortcut_text("Delete"))
                .clicked()
            {
                session.remove_watch(i);
                self.selected = None;
                ui.close();
            }
            if ui
                .add_enabled(i > 0, egui::Button::new("Move Up"))
                .clicked()
            {
                session.move_watch(i, true);
                ui.close();
            }
            if ui
                .add_enabled(i + 1 < count, egui::Button::new("Move Down"))
                .clicked()
            {
                session.move_watch(i, false);
                ui.close();
            }
            ui.separator();
            if ui.button("Copy Value").clicked() {
                ui.ctx().copy_text(v.value.clone());
                ui.close();
            }
            if ui.button("Evaluate Expression…").clicked() {
                events.push(DebugEvent::Evaluate(v.name.clone()));
                ui.close();
            }
        });
    }

    /// A result tree (Evaluate dialog, console result, value popup).
    pub fn result_tree(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        session: &mut DebugSession,
        v: &Variable,
        key: &str,
        epoch: u64,
    ) -> Vec<DebugEvent> {
        let mut events = Vec::new();
        let live = session.is_stopped() && epoch == session.stop_epoch;
        self.var_row(
            ui,
            theme,
            session,
            v,
            None,
            key,
            0,
            false,
            RowKind::Result,
            live,
            &mut events,
        );
        events
    }

    fn console_tab(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        session: &mut DebugSession,
        mono: egui::FontId,
        events: &mut Vec<DebugEvent>,
    ) {
        let stopped = session.is_stopped();
        // The input sits in a strip at the bottom and the transcript fills exactly
        // the rest: content shorter or taller than the tool window resizes it on
        // every frame.
        egui::Panel::bottom("debug_console_input_row")
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 3)))
            .show(ui, |ui| {
                self.console_input(ui, theme, session, &mono, stopped)
            });
        egui::ScrollArea::vertical()
            .id_salt("debug_console")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let repl = session.repl.clone();
                let mut next = 0;
                let output_len = session.output.len();
                for k in 0..=output_len {
                    while next < repl.len() && repl[next].at <= k {
                        let item = &repl[next];
                        match &item.entry {
                            ReplEntry::Input(code) => {
                                for (n, line) in code.lines().enumerate() {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{}{line}",
                                            if n == 0 { ">>> " } else { "... " }
                                        ))
                                        .font(mono.clone())
                                        .color(theme.accent),
                                    );
                                }
                            }
                            ReplEntry::Value(v) => {
                                let name = if v.name.is_empty() || v.name.chars().count() > 60 {
                                    "result".to_string()
                                } else {
                                    v.name.clone()
                                };
                                let shown = Variable { name, ..v.clone() };
                                let live = stopped && item.epoch == session.stop_epoch;
                                self.var_row(
                                    ui,
                                    theme,
                                    session,
                                    &shown,
                                    None,
                                    &format!("c/{next}"),
                                    0,
                                    false,
                                    RowKind::Result,
                                    live,
                                    events,
                                );
                            }
                            ReplEntry::Error(e) => {
                                ui.label(
                                    egui::RichText::new(e.trim_end())
                                        .font(mono.clone())
                                        .color(theme.error),
                                );
                            }
                        }
                        next += 1;
                    }
                    let Some(chunk) = session.output.get(k) else {
                        break;
                    };
                    let color = match chunk.kind {
                        OutputKind::Stderr => theme.error,
                        OutputKind::System => theme.text_dim,
                        OutputKind::Stdout => theme.text,
                    };
                    for line in chunk.text.lines() {
                        ui.label(egui::RichText::new(line).font(mono.clone()).color(color));
                    }
                }
            });
    }

    /// The debug console's input row and its completion list, which keeps
    /// filtering while more of the name is typed.
    fn console_input(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        session: &mut DebugSession,
        mono: &egui::FontId,
        stopped: bool,
    ) {
        let input_id = ui.make_persistent_id("debug_console_input");
        let cursor_before = text_cursor(ui.ctx(), input_id, &self.repl_input);
        let completions = session
            .completions
            .as_ref()
            .filter(|c| Some(c.id) != self.completion_dismissed)
            .and_then(|c| Some((c.id, c.visible_for(&self.repl_input, cursor_before)?)));
        let mut accept: Option<String> = None;
        if let Some((_, items)) = &completions {
            self.completion_selected = self.completion_selected.min(items.len() - 1);
        }
        ui.horizontal(|ui| {
            // Program input while it runs (input()), an expression while it is suspended.
            let input_kind = ConsoleInput::for_state(&session.state);
            ui.label(egui::RichText::new(input_kind.prompt()).color(theme.accent).font(mono.clone()));
            // Enter runs (the completion list takes it when shown), Shift+Enter is a new line.
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
            let consumed_enter = self.console_focused && enter && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            let (up, down) = if self.console_focused && (completions.is_some() || !self.repl_input.contains('\n')) { ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp), i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))) } else { (false, false) };
            let tab = self.console_focused && completions.is_some() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
            let esc = self.console_focused && completions.is_some() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            let ctrl_space = self.console_focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Space) || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Space));
            let hint = if stopped { "Evaluate in the current frame: Enter runs, Shift+Enter adds a line, Ctrl+Space completes" } else { input_kind.hint() };
            let resp = ui.add_enabled(input_kind != ConsoleInput::Closed, egui::TextEdit::multiline(&mut self.repl_input).id(input_id).desired_rows(1).desired_width(f32::INFINITY).font(mono.clone()).lock_focus(true).hint_text(hint));
            self.console_focused = resp.has_focus();
            // The list floats above the input: in the layout it would push the
            // panel taller every frame (the transcript takes the rest of the height).
            if let Some((_, items)) = &completions {
                egui::Area::new(input_id.with("completion_popup")).order(egui::Order::Foreground).pivot(egui::Align2::LEFT_BOTTOM).fixed_pos(resp.rect.left_top() + Vec2::new(0.0, -2.0)).show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).fill(theme.panel_bg).show(ui, |ui| {
                        egui::ScrollArea::vertical().id_salt("debug_console_completion_scroll").max_height(160.0).min_scrolled_height(160.0).show(ui, |ui| {
                            for (i, item) in items.iter().enumerate().take(60) {
                                let r = ui.selectable_label(i == self.completion_selected, egui::RichText::new(format!("{}  {}", item.label, item.kind)).font(mono.clone()));
                                if i == self.completion_selected {
                                    r.scroll_to_me(None);
                                }
                                if r.clicked() {
                                    accept = Some(item.label.clone());
                                }
                            }
                        });
                    });
                });
            }
            if let Some((id, items)) = &completions {
                if down {
                    self.completion_selected = (self.completion_selected + 1).min(items.len() - 1);
                } else if up {
                    self.completion_selected = self.completion_selected.saturating_sub(1);
                } else if consumed_enter || tab {
                    accept = items.get(self.completion_selected).map(|i| i.label.clone());
                } else if esc {
                    self.completion_dismissed = Some(*id);
                    self.completing = false;
                }
            } else if consumed_enter && !stopped && input_kind == ConsoleInput::Stdin {
                // The running program reads the line (input()).
                let line = std::mem::take(&mut self.repl_input);
                session.write_stdin(&format!("{}\n", line.trim_end_matches('\n')));
                resp.request_focus();
            } else if consumed_enter && !self.repl_input.trim().is_empty() {
                let code = std::mem::take(&mut self.repl_input);
                let code = code.trim_end().to_string();
                if self.history.last() != Some(&code) {
                    self.history.push(code.clone());
                }
                self.history_pos = None;
                self.completing = false;
                rustpy_console::debugger::evaluate_in_console(session, &code);
                resp.request_focus();
            } else if up && !self.history.is_empty() {
                let i = self.history_pos.map(|i| i.saturating_sub(1)).unwrap_or(self.history.len() - 1);
                self.history_pos = Some(i);
                self.repl_input = self.history[i].clone();
            } else if down {
                match self.history_pos {
                    Some(i) if i + 1 < self.history.len() => {
                        self.history_pos = Some(i + 1);
                        self.repl_input = self.history[i + 1].clone();
                    }
                    _ => {
                        self.history_pos = None;
                        self.repl_input.clear();
                    }
                }
            }
            let cursor = text_cursor(ui.ctx(), input_id, &self.repl_input);
            let before = self.repl_input[..cursor].chars().next_back();
            let in_name = before.is_some_and(|c| c.is_alphanumeric() || c == '_');
            if let Some(label) = accept.take() {
                let (text, new_cursor) = apply_completion(&self.repl_input, cursor, &label);
                self.repl_input = text;
                session.completions = None;
                self.completing = false;
                set_cursor(ui.ctx(), input_id, &self.repl_input, new_cursor);
                resp.request_focus();
            } else if resp.changed() && self.completing && !in_name && before != Some('.') {
                self.completing = false;
            } else if stopped && (ctrl_space || (resp.changed() && (before == Some('.') || (self.completing && in_name && session.completions.as_ref().and_then(|c| c.visible_for(&self.repl_input, cursor)).is_none())))) {
                // Ctrl+Space, a `.`, or the list no longer covers what was typed (e.g. deleted back).
                self.completion_selected = 0;
                self.completion_dismissed = None;
                self.completing = true;
                session.request_completions(&self.repl_input, cursor);
            }
        });
    }

    /// Smart Step Into: once the targets arrive, enter the only call directly or
    /// let the user pick one.
    fn smart_step_chooser(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        session: &mut DebugSession,
    ) {
        if !self.smart_step {
            return;
        }
        if !session.is_stopped() {
            self.smart_step = false;
            self.smart_targets = None;
            return;
        }
        if self.smart_targets.is_none() {
            let Some(targets) = session.step_in_targets.clone() else {
                return;
            };
            let statement = session
                .frames
                .first()
                .and_then(|f| Some(statement_on_disk(f.path.as_deref()?, f.line)))
                .unwrap_or_default();
            let filtered = filter_step_targets(&targets, &statement);
            match filtered.len() {
                0 => {
                    self.smart_step = false;
                    session.step_into();
                    return;
                }
                1 => {
                    self.smart_step = false;
                    session.step_into_target(filtered[0].id);
                    return;
                }
                _ => {
                    self.smart_selected = 0;
                    self.smart_targets = Some(filtered);
                }
            }
        }
        let Some(targets) = self.smart_targets.clone() else {
            return;
        };
        let mut pick: Option<i64> = None;
        let mut cancel = false;
        let (up, down, enter, esc) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if down {
            self.smart_selected = (self.smart_selected + 1).min(targets.len() - 1);
        }
        if up {
            self.smart_selected = self.smart_selected.saturating_sub(1);
        }
        if enter {
            pick = targets.get(self.smart_selected).map(|t| t.id);
        }
        if esc {
            cancel = true;
        }
        let anchor = ui.max_rect().left_top() + Vec2::new(40.0, 30.0);
        egui::Area::new(ui.id().with("smart_step_into"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .fill(theme.panel_bg)
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new("Smart Step Into: choose the call to enter")
                                .strong()
                                .color(theme.text),
                        );
                        ui.separator();
                        for (i, t) in targets.iter().enumerate() {
                            if ui
                                .selectable_label(
                                    i == self.smart_selected,
                                    egui::RichText::new(&t.label).monospace(),
                                )
                                .clicked()
                            {
                                pick = Some(t.id);
                            }
                        }
                        ui.label(
                            egui::RichText::new("Up/Down select · Enter steps into · Esc cancels")
                                .color(theme.text_dim)
                                .small(),
                        );
                    });
            });
        if pick.is_some() || cancel {
            self.smart_step = false;
            self.smart_targets = None;
        }
        if let Some(id) = pick {
            session.step_into_target(id);
        }
    }

    fn set_hover_frame(&mut self, frame: Option<HoverFrame>) {
        if self.hover_frame != frame {
            self.hover_frame = frame;
            self.hover = HoverTracker::default();
            self.value_popup = None;
            self.popup_left_since = None;
        }
    }

    /// Editor hover while suspended: shows an already cached frame value.
    /// Evaluation is reserved for an explicit Quick Evaluate request. `hovered` is the hovered line's text
    /// and char column. Returns true while a value popup is shown (the
    /// documentation hover stays away then).
    pub fn editor_hover(
        &mut self,
        ui: &egui::Ui,
        theme: &UiTheme,
        session: &mut DebugSession,
        hovered: Option<(String, usize)>,
        now: f64,
    ) -> bool {
        if !session.is_stopped() {
            self.set_hover_frame(None);
            self.value_popup = None;
            self.hover = HoverTracker::default();
            return false;
        }
        let frame = (
            session.id,
            session.stop_epoch,
            session.selected_thread,
            session.current_frame().map(|f| f.id),
        );
        self.set_hover_frame(Some(frame));
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let expr = hovered.and_then(|(line, col)| hover_expression(&line, col));
        if expr != self.hover.expression {
            self.hover = HoverTracker {
                expression: expr.clone(),
                since: now,
            };
        }
        let pinned = self.value_popup.as_ref().is_some_and(|p| p.pinned);
        if let (Some(e), false) = (&self.hover.expression, pinned) {
            let shown = self
                .value_popup
                .as_ref()
                .is_some_and(|p| &p.expression == e);
            if !shown && now - self.hover.since > 0.4 {
                self.value_popup = Some(ValuePopup {
                    expression: e.clone(),
                    anchor: pointer.unwrap_or_default() + Vec2::new(4.0, 16.0),
                    pinned: false,
                    rect: None,
                });
                self.popup_left_since = None;
            }
        }
        if self.hover.expression.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        let Some(popup) = self.value_popup.clone() else {
            return false;
        };
        // Close a hover popup once the pointer has left both the word and the popup for a moment.
        let over_popup = popup
            .rect
            .zip(pointer)
            .is_some_and(|(r, p)| r.expand(6.0).contains(p));
        if !popup.pinned {
            if over_popup || self.hover.expression.as_deref() == Some(popup.expression.as_str()) {
                self.popup_left_since = None;
            } else {
                let since = *self.popup_left_since.get_or_insert(now);
                if now - since > 0.35 {
                    self.value_popup = None;
                    return false;
                }
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(60));
            }
        } else if ui.input(|i| i.key_pressed(egui::Key::Escape))
            || (ui.input(|i| i.pointer.any_pressed()) && !over_popup)
        {
            self.value_popup = None;
            return false;
        }
        let result = if popup.pinned {
            session
                .hover_result
                .clone()
                .filter(|r| r.expression == popup.expression)
        } else {
            cached_hover_variable(session, &popup.expression)
                .cloned()
                .map(|v| rustpy_debug::EvalResult {
                    expression: popup.expression.clone(),
                    result: Ok(v),
                    epoch: session.stop_epoch,
                })
        };
        let var = match result.as_ref().map(|r| &r.result) {
            Some(Ok(v)) => Some(v.clone()),
            Some(Err(e)) if popup.pinned => Some(Variable {
                name: popup.expression.clone(),
                value: e.clone(),
                type_name: "error".into(),
                variables_reference: 0,
                evaluate_name: None,
            }),
            _ => None,
        };
        if popup.pinned && var.is_none() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
            return true;
        }
        let mut evaluate = false;
        let mut rect = None;
        egui::Area::new(egui::Id::new("debug_value_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(popup.anchor)
            .show(ui.ctx(), |ui| {
                let r = egui::Frame::popup(ui.style())
                    .fill(theme.panel_bg)
                    .show(ui, |ui| {
                        ui.set_min_width(260.0);
                        ui.set_max_width(620.0);
                        // An area only offers its previous size: without the minimum the
                        // list would stay as small as before children were loaded.
                        egui::ScrollArea::vertical()
                            .id_salt("debug_value_popup_scroll")
                            .max_height(320.0)
                            .min_scrolled_height(320.0)
                            .show(ui, |ui| {
                                if let Some(var) = &var {
                                    // Fresh identity per stop/frame prevents an old expansion
                                    // from fetching children merely because the pointer returned.
                                    let key = hover_popup_key(frame, &popup.expression);
                                    let _ = self.result_tree(
                                        ui,
                                        theme,
                                        session,
                                        var,
                                        &key,
                                        result.as_ref().map_or(session.stop_epoch, |r| r.epoch),
                                    );
                                } else {
                                    ui.label("Value not loaded for this frame");
                                    evaluate = ui.button("Quick Evaluate (Ctrl+Alt+F8)").clicked();
                                }
                            });
                    });
                rect = Some(r.response.rect);
            });
        if let Some(p) = &mut self.value_popup {
            p.rect = rect;
        }
        if evaluate {
            self.quick_evaluate(session, &popup.expression, popup.anchor);
        }
        true
    }

    /// Quick Evaluate Expression (Ctrl+Alt+F8): a value popup that stays open.
    pub fn quick_evaluate(&mut self, session: &mut DebugSession, expression: &str, anchor: Pos2) {
        self.set_hover_frame(Some((
            session.id,
            session.stop_epoch,
            session.selected_thread,
            session.current_frame().map(|f| f.id),
        )));
        session.evaluate_hover(expression);
        self.value_popup = Some(ValuePopup {
            expression: expression.to_string(),
            anchor,
            pinned: true,
            rect: None,
        });
    }
}

/// Cursor (byte offset in `text`) of the text edit `id` as last stored.
fn text_cursor(ctx: &egui::Context, id: egui::Id, text: &str) -> usize {
    let state = egui::TextEdit::load_state(ctx, id);
    let chars = state
        .and_then(|s| s.cursor.char_range())
        .map(|r| r.primary.index.0)
        .unwrap_or_else(|| text.chars().count());
    text.char_indices()
        .nth(chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

/// Moves the cursor of the text edit `id` to byte offset `at`.
fn set_cursor(ctx: &egui::Context, id: egui::Id, text: &str, at: usize) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        let ch = text[..at.min(text.len())].chars().count();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(ch),
            )));
        state.store(ctx, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_state_and_expansion_belong_to_the_session_stop_thread_and_frame() {
        let frame = (1, 2, Some(3), Some(4));
        let mut panel = DebugPanel::default();
        let old = hover_popup_key(frame, "obj");
        panel.expanded.insert(old.clone());
        for next in [
            (2, 2, Some(3), Some(4)),
            (1, 3, Some(3), Some(4)),
            (1, 2, Some(5), Some(4)),
            (1, 2, Some(3), Some(6)),
        ] {
            panel.set_hover_frame(Some(frame));
            panel.hover = HoverTracker {
                expression: Some("obj".into()),
                since: 1.0,
            };
            panel.value_popup = Some(ValuePopup {
                expression: "obj".into(),
                anchor: Pos2::ZERO,
                pinned: true,
                rect: None,
            });
            panel.set_hover_frame(Some(next));
            assert!(panel.hover.expression.is_none() && panel.value_popup.is_none());
            assert!(
                !panel.expanded.contains(&hover_popup_key(next, "obj")),
                "old expansion must not fetch children"
            );
        }
    }

    #[test]
    fn step_targets_are_limited_to_the_statement() {
        let t = |id: i64, label: &str| StepInTarget {
            id,
            label: label.into(),
        };
        let targets = vec![
            t(0, "inc(items[0])"),
            t(1, "double(inc(items[0]))"),
            t(2, "print(\"total\", total)"),
            t(3, "range(3)"),
        ];
        let on_line = filter_step_targets(&targets, "total = double(inc( items[0] ))\n");
        assert_eq!(on_line.iter().map(|t| t.id).collect::<Vec<_>>(), vec![0, 1]);
        let second = vec![t(0, "f(x)"), t(1, "f(x) (call 2)"), t(2, "g()")];
        assert_eq!(filter_step_targets(&second, "y = f(x) + f(x)").len(), 2);
        assert_eq!(
            filter_step_targets(&targets, "pass").len(),
            4,
            "no match: offer all"
        );
    }

    #[test]
    fn statements_span_open_brackets() {
        let dir = std::env::temp_dir().join(format!("rustpy-debug-panel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.py");
        std::fs::write(&p, "x = f(1,\n      2)  # (\ny = 3\n").unwrap();
        assert_eq!(statement_on_disk(&p, 1), "x = f(1,\n      2)  # (\n");
        assert_eq!(statement_on_disk(&p, 3), "y = 3\n");
        assert_eq!(statement_on_disk(&dir.join("missing.py"), 1), "");
        std::fs::remove_dir_all(&dir).ok();
    }
}
