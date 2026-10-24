//! Debugger wiring of the IDE (roadmap §3.3): line and exception breakpoints
//! (edit popup, Breakpoints dialog, mute), session setup from the debugger
//! settings, the stepping actions, and debugger values in the editor.

use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::breakpoints::{properties_ui, BreakpointPopup, BreakpointsDialog, LineBp, Selection};
use crate::debug_editor::quick_evaluate_expression;
use crate::editor::view::InlineValue;
use rustpy_debug::{DebugOptions, LaunchSetup};
use rustpy_settings::ExceptionClassState;

impl IdeApp {
    pub(super) fn debug_frame_location_changed(
        &self,
        path: &Path,
        line: usize,
        column: usize,
    ) -> bool {
        let mapped = self.injected_debug_location(path, line);
        let (path, line, column) = mapped
            .as_ref()
            .map(|(path, line)| (path.as_path(), *line, 1))
            .unwrap_or((path, line, column));
        self.exec_point
            .as_ref()
            .is_none_or(|(current_path, current_line)| {
                current_path != path || *current_line != line
            })
            || self.exec_point_column != Some(column)
    }

    /// Frame columns use the adapter's wire units; editor columns count scalars.
    pub(super) fn navigate_debug_frame(
        &mut self,
        path: &Path,
        line: usize,
        column: usize,
        encoding: rustpy_debug::ColumnEncoding,
    ) {
        if self.navigate_notebook_debug_frame(path, line, column, encoding) {
            return;
        }
        let mapped = self.injected_debug_location(path, line);
        let (path, line, column) = mapped
            .as_ref()
            .map(|(path, line)| (path.as_path(), *line, 1))
            .unwrap_or((path, line, column));
        self.exec_point = Some((path.to_path_buf(), line));
        self.exec_point_column = Some(column);
        if !path.is_file() {
            return;
        }
        self.open_file(path);
        let normalized = rustpy_core::vfs::normalize_path(path);
        if let Some(tab) = self
            .active
            .and_then(|i| self.tabs.get_mut(i))
            .filter(|tab| tab.state.path.as_deref() == Some(normalized.as_path()))
        {
            tab.view.cancel_session_restore();
            let scalar_column = (column > 0 && line > 0 && line <= tab.state.doc.line_count())
                .then(|| {
                    let text = tab.state.doc.line_text(line - 1);
                    let byte = encoding.byte_offset(&text, column);
                    text[..byte].chars().count() + 1
                });
            tab.state.go_to_line(line, scalar_column);
        }
    }

    /// Sends the breakpoints of `path` and the global breakpoint settings to the session.
    pub(super) fn sync_breakpoints(&mut self, path: &Path) {
        self.sync_injected_debug_breakpoints(path);
        if let Some(d) = self.debug.as_mut().filter(|d| d.is_active()) {
            self.breakpoints.sync(path, d);
        }
    }

    /// What a new debug session starts with: breakpoints and Settings › Python Debugger.
    pub(super) fn debug_launch_setup(&self) -> LaunchSetup {
        let s = &self.settings.debugger;
        let mut backend = s.workspace.clone();
        backend.collect_runtime_types |= self.settings.collect_runtime_types;
        backend.breakpoints = self
            .breakpoints
            .all()
            .into_iter()
            .map(
                |(path, line, bp)| rustpy_settings::debugger_workspace::AdvancedBreakpoint {
                    path,
                    line,
                    metadata: bp.metadata,
                },
            )
            .collect();
        if backend.breakpoints.iter().any(|point| {
            point.metadata.suspend_policy
                == rustpy_settings::debugger_workspace::SuspendPolicy::Thread
        }) {
            backend.suspend_all_threads = false;
        }
        LaunchSetup {
            breakpoints: self.breakpoints.debug_map(),
            exceptions: self.breakpoints.debug_exceptions(),
            muted: self.breakpoints.muted,
            options: DebugOptions {
                backend,
                just_my_code: s.just_my_code,
                show_return_value: s.show_return_values,
                gevent: s.gevent_compatible,
            },
        }
    }

    /// Per frame: breakpoint text typed in a popup or dialog that closed,
    /// temporary breakpoints the session removed, breakpoints the debugger moved
    /// to a line with code, its error messages, and the locals behind the inline
    /// values.
    pub(super) fn debug_tick(&mut self) {
        // Closed (Escape too) or another selection: the typed text is complete.
        let (line, exception) = match &self.dialog {
            Dialog::EditBreakpoint(p) => (Some((p.path.clone(), p.line)), None),
            Dialog::Breakpoints(s) => match &s.selected {
                Some(Selection::Line(p, l)) => (Some((p.clone(), *l)), None),
                Some(Selection::Exception(i)) => (None, Some(*i)),
                _ => (None, None),
            },
            _ => (None, None),
        };
        self.commit_breakpoint_edits(line.as_ref().map(|(p, l)| (p.as_path(), *l)), exception);
        let Some(d) = self.debug.as_mut() else { return };
        self.debug_panel.edited_files.observe(
            d.id,
            self.tabs.iter().filter_map(|t| {
                Some((
                    t.state.path.clone()?,
                    t.state.doc.version(),
                    t.state.doc.is_modified(),
                ))
            }),
        );
        let removed = d.take_removed_breakpoints();
        let moved = d.take_moved_breakpoints();
        let errors = d.take_errors();
        self.debug_panel.inline.update(d);
        for (path, line) in &removed {
            self.breakpoints.remove(path, *line);
        }
        // debugpy checks the lines against the file on disk: in a file edited in
        // the IDE they are not the editor's lines, so the breakpoints stay put.
        let edited = &self.debug_panel.edited_files;
        let adopted = self.breakpoints.adopt_moves(&moved, |p| edited.contains(p));
        for (path, from, to, merged) in &adopted {
            d.adopt_moved_breakpoint(path, *from, *to);
            if *merged {
                // The debugger has two breakpoints there: send the one that stays.
                self.breakpoints.sync(path, d);
            }
        }
        if !removed.is_empty() || !adopted.is_empty() {
            self.save_workspace();
        }
        if let Some(e) = errors.last() {
            self.set_status(e.clone());
        }
    }

    /// Applies breakpoint text typed in the breakpoint popup or the Breakpoints
    /// dialog unless that breakpoint (`line`) or exception class (`exception`) is
    /// still being edited.
    fn commit_breakpoint_edits(&mut self, line: Option<(&Path, usize)>, exception: Option<usize>) {
        let pending = &mut self.debug_panel.breakpoint_edits;
        let typed_line = pending
            .line
            .take_if(|(p, l, _)| line != Some((p.as_path(), *l)));
        let typed_exception = pending.exception.take_if(|(i, _)| exception != Some(*i));
        let mut changed = false;
        if let Some((path, l, bp)) = typed_line {
            if self.breakpoints.get(&path, l).is_some_and(|b| *b != bp) {
                self.breakpoints.set(&path, l, bp);
                self.sync_breakpoints(&path);
                changed = true;
            }
        }
        if let Some((i, name)) = typed_exception {
            if let Some(c) = self
                .breakpoints
                .exceptions
                .classes
                .get_mut(i)
                .filter(|c| c.name != name)
            {
                c.name = name;
                if let Some(d) = self.debug.as_mut().filter(|d| d.is_active()) {
                    self.breakpoints.sync_global(d);
                }
                changed = true;
            }
        }
        if changed {
            self.save_workspace();
        }
    }

    /// Path and 1-based caret line of the active editor.
    fn caret_location(&self) -> Option<(PathBuf, usize)> {
        let tab = self.active.and_then(|i| self.tabs.get(i))?;
        let path = tab.state.path.clone()?;
        Some((path, tab.state.doc.line_of(tab.state.primary().head()) + 1))
    }

    pub(super) fn debugger_force_run_to_cursor(&mut self) {
        if self.project_safe_mode() {
            return;
        }
        if let Some((path, line)) = self.caret_location() {
            if let Some(session) = &mut self.debug {
                session.force_run_to_cursor(&path, line);
            }
        }
    }

    /// The debugger actions (Run menu, toolbar and shortcuts).
    pub(super) fn debugger_action(&mut self, action: Action) {
        if self.project_safe_mode()
            && matches!(
                action,
                Action::RunToCursor
                    | Action::JumpToCursor
                    | Action::SmartStepInto
                    | Action::QuickEvaluateExpression
                    | Action::RestartDebug
            )
        {
            self.notify_safe_mode("Debugger execution and evaluations");
            return;
        }
        let stopped = self.debug.as_ref().is_some_and(|d| d.is_stopped());
        match action {
            Action::RunToCursor | Action::JumpToCursor => {
                let Some((path, line)) = self.caret_location() else {
                    return;
                };
                match self.debug.as_mut() {
                    Some(d) if stopped => {
                        if action == Action::RunToCursor {
                            d.run_to_cursor(&path, line);
                        } else {
                            d.jump_to_line(&path, line);
                        }
                    }
                    _ => self
                        .set_status(format!("{} needs a suspended debug session", action.text())),
                }
            }
            Action::SmartStepInto => match self.debug.as_mut() {
                Some(d) if stopped => {
                    d.request_step_in_targets();
                    self.debug_panel.smart_step = true;
                    self.bottom = Some(BottomTool::Debug);
                }
                _ => self.set_status("Smart Step Into needs a suspended debug session"),
            },
            Action::ShowExecutionPoint => {
                let Some(d) = self.debug.as_mut().filter(|d| d.is_stopped()) else {
                    self.set_status("The program is not suspended");
                    return;
                };
                if let Some(t) = d.stopped_thread().filter(|t| d.selected_thread != Some(*t)) {
                    // The execution point shows once the thread's frames are loaded.
                    d.select_thread(t);
                    self.exec_point = None;
                    return;
                }
                if d.selected_frame != 0 {
                    d.select_frame(0);
                }
                let encoding = d.column_encoding();
                let Some((path, line, column)) = d
                    .frames
                    .first()
                    .and_then(|f| Some((f.path.clone()?, f.line, f.column)))
                else {
                    return;
                };
                self.navigate_debug_frame(&path, line, column, encoding);
                self.focus_editor = true;
            }
            Action::QuickEvaluateExpression => {
                let Some(i) = self.active.filter(|i| *i < self.tabs.len()) else {
                    return;
                };
                let tab = &self.tabs[i];
                let caret = tab.state.primary();
                let expr = if caret.has_selection() {
                    tab.state.doc.slice(caret.range()).trim().to_string()
                } else {
                    let pos = tab.state.doc.offset_to_position(caret.head());
                    let text = tab.state.doc.line_text(pos.line as usize);
                    let col = pos.column as usize;
                    quick_evaluate_expression(&text, col)
                        .or_else(|| quick_evaluate_expression(&text, col.saturating_sub(1)))
                        .unwrap_or_default()
                };
                let anchor = tab
                    .view
                    .caret_screen_rect
                    .map(|r| r.left_bottom() + Vec2::new(0.0, 4.0))
                    .unwrap_or(Pos2::new(240.0, 240.0));
                match self.debug.as_mut() {
                    Some(d) if stopped && !expr.is_empty() => {
                        self.debug_panel.quick_evaluate(d, &expr, anchor)
                    }
                    Some(_) if stopped => self
                        .set_status("Select an expression or put the caret on a name to evaluate"),
                    _ => self.set_status("Quick Evaluate needs a suspended debug session"),
                }
            }
            Action::ToggleTemporaryLineBreakpoint => {
                let Some((path, line)) = self.caret_location() else {
                    return;
                };
                self.breakpoints.toggle_temporary(&path, line);
                self.sync_breakpoints(&path);
                self.save_workspace();
            }
            Action::MuteBreakpoints => {
                self.breakpoints.muted = !self.breakpoints.muted;
                if let Some(d) = self.debug.as_mut() {
                    self.breakpoints.sync_global(d);
                }
                self.save_workspace();
                self.set_status(if self.breakpoints.muted {
                    "Breakpoints muted"
                } else {
                    "Breakpoints unmuted"
                });
            }
            Action::RestartDebug => {
                // Ctrl+F5 reruns what the Run window shows, else restarts the debug session.
                let run_first = self.bottom == Some(BottomTool::Run) || self.debug.is_none();
                let run_name = if run_first {
                    self.run_panel.active_session_mut().map(|s| s.name.clone())
                } else {
                    None
                };
                let run_cfg = run_name.and_then(|n| {
                    self.project
                        .as_ref()
                        .and_then(|p| p.run_configs.configs.iter().find(|c| c.name == n).cloned())
                });
                if let Some(cfg) = run_cfg {
                    self.run_config(cfg);
                } else if self.debug.is_some() {
                    if let Some(session) = &mut self.debug {
                        session.stop();
                    }
                    self.debug_panel.restart_requested = Some(std::time::Instant::now());
                } else {
                    self.set_status("Nothing to rerun");
                }
            }
            Action::ViewBreakpoints => {
                // On a breakpoint line: its popup first; pressed again: the dialog.
                if let Dialog::EditBreakpoint(p) = &self.dialog {
                    let selected = Some(Selection::Line(p.path.clone(), p.line));
                    self.dialog = Dialog::Breakpoints(Box::new(BreakpointsDialog {
                        selected,
                        ..Default::default()
                    }));
                    return;
                }
                let at_caret = self
                    .caret_location()
                    .filter(|(p, l)| self.breakpoints.get(p, *l).is_some());
                match at_caret {
                    Some((path, line)) => {
                        let anchor = self
                            .active
                            .and_then(|i| self.tabs.get(i))
                            .and_then(|t| t.view.caret_screen_rect)
                            .map(|r| r.left_bottom() + Vec2::new(0.0, 6.0))
                            .unwrap_or(Pos2::new(200.0, 200.0));
                        self.dialog = Dialog::EditBreakpoint(Box::new(BreakpointPopup {
                            path,
                            line,
                            anchor,
                            focus_condition: true,
                        }));
                    }
                    None => self.dialog = Dialog::Breakpoints(Box::default()),
                }
            }
            _ => {}
        }
    }

    /// Right-click on a breakpoint glyph (`line0` is 0-based).
    pub(super) fn open_breakpoint_popup(
        &mut self,
        tab: usize,
        line0: usize,
        pointer: Option<Pos2>,
    ) {
        let Some(path) = self.tabs.get(tab).and_then(|t| t.state.path.clone()) else {
            return;
        };
        if self.breakpoints.get(&path, line0 + 1).is_none() {
            return;
        }
        let anchor = pointer
            .map(|p| p + Vec2::new(8.0, 8.0))
            .unwrap_or(Pos2::new(200.0, 200.0));
        self.dialog = Dialog::EditBreakpoint(Box::new(BreakpointPopup {
            path,
            line: line0 + 1,
            anchor,
            focus_condition: true,
        }));
    }

    /// The breakpoint popup: switches apply at once, typed text once complete
    /// (Enter, Done, leaving the field, closing the popup).
    pub(super) fn edit_breakpoint_dialog(&mut self, ctx: &egui::Context, popup: BreakpointPopup) {
        let completion_names = self.breakpoint_completion_names();
        // Text typed for another breakpoint is complete.
        self.commit_breakpoint_edits(Some((popup.path.as_path(), popup.line)), None);
        let Some(stored) = self.breakpoints.get(&popup.path, popup.line).cloned() else {
            return;
        };
        let mut bp = self
            .debug_panel
            .breakpoint_edits
            .shown(&popup.path, popup.line, &stored);
        let theme = self.theme.clone();
        let file = popup
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (mut open, mut remove, mut more, mut done, mut left_field) =
            (true, false, false, false, false);
        egui::Window::new(format!("{file}:{}", popup.line))
            .id(egui::Id::new("edit_breakpoint"))
            .collapsible(false)
            .resizable(false)
            .fixed_pos(popup.anchor)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                // A text field gives up its focus on Enter while it runs: look before.
                let editing = ui.memory(|m| m.focused().is_some());
                left_field = properties_ui(
                    ui,
                    &mut bp,
                    &theme,
                    popup.focus_condition,
                    &completion_names,
                );
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Remove").clicked() {
                        remove = true;
                    }
                    if ui
                        .button("More…")
                        .on_hover_text("All breakpoints and exception breakpoints (Ctrl+Shift+F8)")
                        .clicked()
                    {
                        more = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Done").clicked() {
                            done = true;
                        }
                    });
                });
                if editing && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = true;
                }
            });
        let mut changed = false;
        if remove {
            self.debug_panel.breakpoint_edits.line = None;
            self.breakpoints.remove(&popup.path, popup.line);
        } else {
            let complete = left_field || done || more || !open;
            let apply = self.debug_panel.breakpoint_edits.apply(
                &popup.path,
                popup.line,
                &stored,
                bp,
                complete,
            );
            if apply != stored {
                self.breakpoints.set(&popup.path, popup.line, apply);
                changed = true;
            }
        }
        if remove || changed {
            self.sync_breakpoints(&popup.path);
            self.save_workspace();
        }
        if more {
            let selected = (!remove).then(|| Selection::Line(popup.path.clone(), popup.line));
            self.dialog = Dialog::Breakpoints(Box::new(BreakpointsDialog {
                selected,
                ..Default::default()
            }));
        } else if open && !remove && !done {
            self.dialog = Dialog::EditBreakpoint(Box::new(BreakpointPopup {
                focus_condition: false,
                ..popup
            }));
        } else {
            self.focus_editor = true;
        }
    }

    fn breakpoint_completion_names(&self) -> Vec<String> {
        let mut names = [
            "True",
            "False",
            "None",
            "and",
            "or",
            "not",
            "is",
            "in",
            "len",
            "isinstance",
            "hasattr",
            "getattr",
            "any",
            "all",
            "str",
            "int",
            "float",
            "type",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
        if let Some(session) = &self.debug {
            for variable in session.variables.values().flatten().take(4096) {
                if variable
                    .name
                    .chars()
                    .all(|character| character.is_alphanumeric() || character == '_')
                {
                    names.insert(variable.name.clone());
                }
            }
        }
        names.into_iter().collect()
    }

    /// View Breakpoints: line breakpoints, exception breakpoints and mute.
    pub(super) fn breakpoints_dialog(
        &mut self,
        ctx: &egui::Context,
        mut state: Box<BreakpointsDialog>,
    ) {
        let completion_names = self.breakpoint_completion_names();
        // Text typed for what is no longer selected is complete.
        match &state.selected {
            Some(Selection::Line(p, l)) => {
                self.commit_breakpoint_edits(Some((p.as_path(), *l)), None)
            }
            Some(Selection::Exception(i)) => self.commit_breakpoint_edits(None, Some(*i)),
            _ => self.commit_breakpoint_edits(None, None),
        }
        // Typed text applies once its field is left (see `PendingEdits`).
        let mut pending = std::mem::take(&mut self.debug_panel.breakpoint_edits);
        let theme = self.theme.clone();
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        let before = self.breakpoints.clone();
        let mut store = self.breakpoints.clone();
        let mut open = true;
        let mut jump: Option<(PathBuf, usize)> = None;
        egui::Window::new("Breakpoints").collapsible(false).resizable(true).default_size([760.0, 440.0]).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut store.muted, "Mute breakpoints").on_hover_text("No breakpoint suspends or logs while muted");
                ui.separator();
                ui.label("Exception class:");
                let r = ui.add(egui::TextEdit::singleline(&mut state.new_exception).hint_text("e.g. ValueError").desired_width(160.0));
                let add = ui.button("+ Add").on_hover_text("Add a Python exception breakpoint").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                let name = state.new_exception.trim().to_string();
                if add && !name.is_empty() {
                    let index = match store.exceptions.classes.iter().position(|c| c.name == name) {
                        Some(i) => i,
                        None => {
                            store.exceptions.classes.push(ExceptionClassState { name, ..Default::default() });
                            store.exceptions.classes.len() - 1
                        }
                    };
                    state.selected = Some(Selection::Exception(index));
                    state.new_exception.clear();
                }
                let removable = matches!(state.selected, Some(Selection::Line(..)) | Some(Selection::Exception(_)));
                if ui.add_enabled(removable, egui::Button::new("Remove")).clicked() {
                    match state.selected.take() {
                        Some(Selection::Line(p, l)) => {
                            store.remove(&p, l);
                            pending.line = None;
                        }
                        Some(Selection::Exception(i)) if i < store.exceptions.classes.len() => {
                            store.exceptions.classes.remove(i);
                            pending.exception = None;
                        }
                        _ => {}
                    }
                }
            });
            let groups=store.all().into_iter().map(|(_,_,bp)|bp.metadata.group).filter(|group|!group.is_empty()).collect::<std::collections::BTreeSet<_>>();
            ui.horizontal_wrapped(|ui|{for group in groups {ui.menu_button(&group,|ui|{for(label,enabled)in [("Enable group",true),("Disable group",false)]{if ui.button(crate::i18n::tr_static(label)).clicked(){for(path,line,mut bp)in store.all(){if bp.metadata.group==group{bp.enabled=enabled;store.set(&path,line,bp);}}ui.close();}}if ui.button("Remove group breakpoints").clicked(){for(path,line,bp)in store.all(){if bp.metadata.group==group{store.remove(&path,line);}}ui.close();}});}});
            ui.separator();
            egui::Panel::left("breakpoints_list").resizable(true).default_size(340.0).min_size(220.0).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("breakpoints_list_scroll").auto_shrink([false, false]).show(ui, |ui| {
                    ui.label(egui::RichText::new("Python Line Breakpoints").strong());
                    let all = store.all();
                    if all.is_empty() {
                        ui.label(egui::RichText::new("None. Click the gutter or press Ctrl+F8.").color(theme.text_dim));
                    }
                    for (path, line, bp) in all {
                        ui.horizontal(|ui| {
                            let mut enabled = bp.enabled;
                            if ui.checkbox(&mut enabled, "").changed() {
                                store.set(&path, line, LineBp { enabled, ..bp.clone() });
                            }
                            let rel = path.strip_prefix(&root).unwrap_or(&path).display().to_string();
                            let summary = bp.summary();
                            let text = if summary.is_empty() { format!("{rel}:{line}") } else { format!("{rel}:{line}  ({summary})") };
                            let text=if bp.metadata.group.is_empty(){text}else{format!("[{}] {text}",bp.metadata.group)};
                            let sel = state.selected == Some(Selection::Line(path.clone(), line));
                            let r = ui.selectable_label(sel, egui::RichText::new(text).color(if bp.enabled { theme.text } else { theme.text_dim }));
                            if r.clicked() {
                                state.selected = Some(Selection::Line(path.clone(), line));
                            }
                            if r.double_clicked() {
                                jump = Some((path.clone(), line));
                            }
                        });
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Python Exception Breakpoints").strong());
                    if ui.selectable_label(state.selected == Some(Selection::AnyException), "Any exception").clicked() {
                        state.selected = Some(Selection::AnyException);
                    }
                    for i in 0..store.exceptions.classes.len() {
                        ui.horizontal(|ui| {
                            let c = &mut store.exceptions.classes[i];
                            ui.checkbox(&mut c.enabled, "");
                            if ui.selectable_label(state.selected == Some(Selection::Exception(i)), pending.shown_exception(i, &c.name)).clicked() {
                                state.selected = Some(Selection::Exception(i));
                            }
                        });
                    }
                });
            });
            match state.selected.clone() {
                Some(Selection::Line(path, line)) => match store.get(&path, line).cloned() {
                    Some(stored) => {
                        ui.label(egui::RichText::new(format!("{}:{line}", path.strip_prefix(&root).unwrap_or(&path).display())).strong());
                        let mut bp = pending.shown(&path, line, &stored);
                        let left_field = properties_ui(ui, &mut bp, &theme, false,&completion_names);
                        let apply = pending.apply(&path, line, &stored, bp, left_field);
                        if apply != stored {
                            store.set(&path, line, apply);
                        }
                        ui.add_space(6.0);
                        if ui.button("Go to Source").clicked() {
                            jump = Some((path, line));
                        }
                    }
                    None => {
                        state.selected = None;
                    }
                },
                Some(Selection::AnyException) => {
                    ui.label(egui::RichText::new("Any exception").strong());
                    let e = &mut store.exceptions;
                    ui.checkbox(&mut e.on_raise, "On raise").on_hover_text("Suspend whenever an exception is raised, even if it is handled");
                    ui.checkbox(&mut e.on_terminate, "On termination").on_hover_text("Suspend on exceptions that are not handled");
                    ui.checkbox(&mut e.user_unhandled, "On exceptions not handled by user code");
                    ui.label(egui::RichText::new("SystemExit, KeyboardInterrupt and GeneratorExit never suspend.").color(theme.text_dim).small());
                }
                Some(Selection::Exception(i)) if i < store.exceptions.classes.len() => {
                    let c = &mut store.exceptions.classes[i];
                    let mut name = pending.shown_exception(i, &c.name);
                    let mut left_field = false;
                    ui.horizontal(|ui| {
                        ui.label("Exception class:");
                        left_field = ui.add(egui::TextEdit::singleline(&mut name).desired_width(220.0)).lost_focus();
                    });
                    c.name = pending.apply_exception(i, &c.name, name, left_field);
                    ui.checkbox(&mut c.enabled, "Enabled");
                    ui.checkbox(&mut c.on_raise, "On raise");
                    ui.checkbox(&mut c.on_terminate, "On termination");
                    ui.label(egui::RichText::new("Subclasses match too. Use a module path for other classes, e.g. requests.HTTPError.").color(theme.text_dim).small());
                }
                _ => {
                    ui.label(egui::RichText::new("Select a breakpoint to edit its properties.").color(theme.text_dim));
                }
            }
        });
        if store != before {
            for path in before.paths_differing(&store) {
                if let Some(d) = self.debug.as_mut().filter(|d| d.is_active()) {
                    store.sync(&path, d);
                }
            }
            if let Some(d) = self.debug.as_mut().filter(|d| d.is_active()) {
                store.sync_global(d);
            }
            self.breakpoints = store;
            self.save_workspace();
        }
        self.debug_panel.breakpoint_edits = pending;
        if !open || jump.is_some() {
            self.commit_breakpoint_edits(None, None);
        }
        if let Some((p, l)) = jump {
            self.open_file(&p);
            if let Some(tab) = self
                .opened_editor_index(&p)
                .and_then(|i| self.tabs.get_mut(i))
            {
                tab.view.cancel_session_restore();
                tab.state.go_to_line(l, None);
            }
            self.focus_editor = true;
        } else if open {
            self.dialog = Dialog::Breakpoints(state);
        }
    }

    /// Debugger values painted at the end of the lines of tab `i`.
    pub(super) fn debug_inline_values(&self, i: usize) -> Vec<InlineValue> {
        if !self.settings.debugger.inline_values {
            return Vec::new();
        }
        let (Some(d), Some(tab)) = (self.debug.as_ref(), self.tabs.get(i)) else {
            return Vec::new();
        };
        let Some(path) = tab.state.path.as_deref() else {
            return Vec::new();
        };
        let doc = &tab.state.doc;
        let mut values = self.debug_panel.inline.values_for(
            d,
            path,
            |line| doc.line_text(line),
            doc.line_count(),
        );
        if d.is_stopped() {
            for watch in &d.watches {
                if let Some((watch_path, line)) = d.backend.inline_watches.get(&watch.expression) {
                    if watch_path == path {
                        if let Some(result) = &watch.result {
                            values.push(InlineValue {
                                line: line.saturating_sub(1),
                                text: format!(
                                    "{} = {}",
                                    watch.expression,
                                    result.value.chars().take(200).collect::<String>()
                                ),
                                changed: false,
                            });
                        }
                    }
                }
            }
        }
        values
    }

    /// The value popup under the mouse while suspended; true when it replaces
    /// the documentation hover.
    pub(super) fn debug_editor_hover(
        &mut self,
        ui: &egui::Ui,
        i: usize,
        hovered: Option<TextSize>,
        now: f64,
    ) -> bool {
        if self.project_safe_mode() {
            return false;
        }
        let Some(d) = self.debug.as_mut() else {
            return false;
        };
        if !d.is_stopped() && self.debug_panel.value_popup.is_none() {
            return false;
        }
        let Some(tab) = self
            .tabs
            .get(i)
            .filter(|t| t.state.language == Language::Python)
        else {
            return false;
        };
        let line = hovered.map(|off| {
            let p = tab.state.doc.offset_to_position(off);
            (tab.state.doc.line_text(p.line as usize), p.column as usize)
        });
        self.debug_panel.editor_hover(ui, &self.theme, d, line, now)
    }

    /// Settings › Build, Execution, Deployment › Python Debugger.
    pub(super) fn page_debugger(&mut self, ui: &mut egui::Ui) {
        let dim = self.theme.text_dim;
        let s = &mut self.settings.debugger;
        ui.heading("Stepping");
        ui.checkbox(&mut s.just_my_code, "Do not step into library scripts").on_hover_text("debugpy \"justMyCode\": Step Into skips the standard library and site-packages, and exceptions raised there do not suspend. Breakpoints set in library files before debugging starts still suspend.");
        ui.add_space(8.0);
        ui.heading("Data views");
        ui.checkbox(&mut s.show_return_values, "Show return values")
            .on_hover_text(
                "After stepping out of a function, its return value is listed with the variables",
            );
        ui.checkbox(&mut s.inline_values, "Show values inline").on_hover_text("Variable values at the end of executed lines in the editor while the program is suspended");
        ui.add_space(8.0);
        ui.heading("Compatibility");
        ui.checkbox(&mut s.gevent_compatible, "Gevent compatible")
            .on_hover_text("Sets GEVENT_SUPPORT=True for programs that use gevent monkey-patching");
        ui.heading("Python backend");
        let workspace = &mut s.workspace;
        ui.checkbox(
            &mut workspace.sub_process,
            "Debug subprocesses and multiprocessing",
        );
        ui.checkbox(
            &mut workspace.collect_runtime_types,
            "Collect runtime types for code insight",
        );
        ui.checkbox(
            &mut workspace.drop_failed_tests,
            "Drop into debugger on failed tests",
        );
        ui.checkbox(&mut workspace.django, "Django template debugging");
        ui.checkbox(&mut workspace.jinja, "Jinja template debugging");
        ui.horizontal(|ui| {
            let qt_label = ui.label("Qt support");
            egui::ComboBox::from_id_salt("debug_qt")
                .selected_text(&workspace.qt_support)
                .show_ui(ui, |ui| {
                    for value in ["none", "auto", "pyqt5", "pyqt6", "pyside2", "pyside6"] {
                        ui.selectable_value(
                            &mut workspace.qt_support,
                            value.into(),
                            crate::i18n::tr_static(value),
                        );
                    }
                })
                .combo_labelled(&qt_label);
        });
        ui.checkbox(
            &mut workspace.use_cython,
            "Allow Cython acceleration when available",
        );
        ui.checkbox(
            &mut workspace.use_frame_eval,
            "Allow frame evaluation when available",
        );
        ui.checkbox(
            &mut workspace.use_monitoring,
            "Allow Python monitoring when available",
        );
        ui.collapsing("Do not step into", |ui| {
            let mut remove = None;
            for (index, pattern) in workspace.step_filters.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(pattern);
                    if ui.button("Remove").clicked() {
                        remove = Some(index);
                    }
                });
            }
            if let Some(index) = remove {
                workspace.step_filters.remove(index);
            }
            if ui.button("Add path filter").clicked() {
                workspace.step_filters.push(String::new());
            }
        });
        ui.checkbox(
            &mut workspace.jump_to_source,
            "Jump to source on suspension",
        );
        ui.horizontal(|ui| {
            let loading_label = ui.label("Variable loading");
            use rustpy_settings::debugger_workspace::LoadingPolicy;
            egui::ComboBox::from_id_salt("debug_loading")
                .selected_text(format!("{:?}", workspace.loading))
                .show_ui(ui, |ui| {
                    for policy in [
                        LoadingPolicy::OnDemand,
                        LoadingPolicy::Asynchronous,
                        LoadingPolicy::Synchronous,
                    ] {
                        ui.selectable_value(&mut workspace.loading, policy, format!("{policy:?}"));
                    }
                })
                .combo_labelled(&loading_label);
        });
        ui.horizontal(|ui| {
            ui.label("Children per page");
            ui.add(egui::DragValue::new(&mut workspace.max_children).range(1..=1000));
        });
        ui.checkbox(
            &mut workspace.enable_repr,
            "Enable custom __repr__ for launched Python types",
        );
        ui.checkbox(&mut workspace.show_special, "Show special variables");
        ui.checkbox(&mut workspace.show_protected, "Show protected variables");
        ui.checkbox(&mut workspace.sort_variables, "Sort variables by name");
        ui.horizontal(|ui| {
            ui.label("Debug server host");
            ui.text_edit_singleline(&mut workspace.server_host);
            ui.label("Port");
            ui.add(egui::DragValue::new(&mut workspace.server_port).range(1..=65535));
        });
        let server = ui
            .button("Start Python debug server")
            .clicked()
            .then(|| (workspace.server_host.clone(), workspace.server_port));
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Stepping, return values and gevent apply to the next debug session; inline values update at once.").color(dim));
        if let Some((host, port)) = server {
            let mut spec = rustpy_project::configuration_types::ConfigurationSpec::new(
                rustpy_project::configuration_types::ConfigurationType::PythonDebugServer,
            );
            spec.host = host;
            spec.port = port;
            spec.listen = true;
            self.launch_config(
                RunConfig {
                    name: "Python debug server".into(),
                    type_options: Some(spec),
                    ..Default::default()
                },
                crate::run_context::RunMode::Debug,
            );
        }
    }

    /// The Evaluate dialog's result tree; returns an expression to evaluate next.
    pub(super) fn evaluate_dialog_result(&mut self, ui: &mut egui::Ui) -> Option<String> {
        if self.project_safe_mode() {
            return None;
        }
        let theme = self.theme.clone();
        let d = self.debug.as_mut()?;
        let result = d.dialog_result.clone()?;
        let mut next = None;
        match &result.result {
            Ok(v) => {
                let key = format!("d/{}", result.expression);
                for ev in self
                    .debug_panel
                    .result_tree(ui, &theme, d, v, &key, result.epoch)
                {
                    if let DebugEvent::Evaluate(e) = ev {
                        next = Some(e);
                    }
                }
            }
            Err(e) => {
                ui.label(
                    egui::RichText::new(e.trim_end())
                        .color(theme.error)
                        .monospace(),
                );
            }
        }
        next
    }
}

#[cfg(test)]
mod frame_navigation_tests {
    use super::*;
    use rustpy_debug::ColumnEncoding;

    fn fixture() -> (tempfile::TempDir, IdeApp, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.py");
        std::fs::write(&path, "(\"😀é\", 1)[1] / 0\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(dir.path());
        let app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        assert!(app.tabs.is_empty());
        (dir, app, path)
    }

    #[test]
    fn adapter_columns_navigate_to_the_same_unicode_operator() {
        let (_dir, mut app, path) = fixture();
        for (encoding, column) in [
            (ColumnEncoding::DebugpyScalar, 15),
            (ColumnEncoding::Utf16, 16),
        ] {
            app.navigate_debug_frame(&path, 1, column, encoding);
            let state = &app.tabs[app.active.unwrap()].state;
            let offset = usize::from(state.primary().head());
            assert_eq!(offset, 18);
            assert!(state.doc.text()[offset..].starts_with('/'));
        }
    }

    #[test]
    fn another_column_on_the_same_frame_line_moves_the_caret() {
        let (_dir, mut app, path) = fixture();
        app.navigate_debug_frame(&path, 1, 15, ColumnEncoding::DebugpyScalar);
        assert!(!app.debug_frame_location_changed(&path, 1, 15));
        assert!(app.debug_frame_location_changed(&path, 1, 17));
        app.navigate_debug_frame(&path, 1, 17, ColumnEncoding::DebugpyScalar);
        let state = &app.tabs[app.active.unwrap()].state;
        assert!(state.doc.text()[usize::from(state.primary().head())..].starts_with('0'));
        assert!(!app.debug_frame_location_changed(&path, 1, 17));
        app.navigate_debug_frame(&path, 1, 0, ColumnEncoding::DebugpyScalar);
        assert_eq!(
            app.tabs[app.active.unwrap()].state.primary().head(),
            TextSize::new(0)
        );
    }

    #[test]
    fn rejected_frame_file_preserves_previous_tab_and_caret() {
        let (dir, mut app, path) = fixture();
        app.navigate_debug_frame(&path, 1, 15, ColumnEncoding::DebugpyScalar);
        let active = app.active;
        let before = app.tabs[active.unwrap()].state.primary().head();
        let rejected = dir.path().join("broken.py");
        std::fs::write(&rejected, b"# coding: utf-8\nvalue = '\xff'\n").unwrap();
        app.navigate_debug_frame(&rejected, 1, 1, ColumnEncoding::DebugpyScalar);
        assert_eq!(app.active, active);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[active.unwrap()].state.primary().head(), before);
        assert_eq!(
            app.tabs[active.unwrap()].state.path.as_deref(),
            Some(path.as_path())
        );
        app.navigate_debug_frame(
            &dir.path().join("missing.py"),
            1,
            1,
            ColumnEncoding::DebugpyScalar,
        );
        assert_eq!(app.tabs[active.unwrap()].state.primary().head(), before);
    }
}
